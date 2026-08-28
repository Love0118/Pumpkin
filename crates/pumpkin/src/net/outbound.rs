use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::{OwnedSemaphorePermit, Semaphore, TryAcquireError};

pub const DEFAULT_OUTBOUND_BYTE_LIMIT: usize = 32 * 1024 * 1024;
pub const MAX_PRIORITY_BURST: usize = 8;
const MAX_PENDING_STATE_ENTRIES: usize = 4096;
const MAX_PENDING_STATE_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ByteBudgetError {
    Closed,
    Full,
    Oversized { bytes: usize, limit: usize },
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct OutboundMetricsSnapshot {
    pub queued_bytes: u64,
    pub peak_queued_bytes: u64,
    pub dropped_packets: u64,
    pub resnapshots: u64,
    pub blob_cache_hits: u64,
    pub blob_cache_misses: u64,
    pub blob_cache_bytes: u64,
    pub peak_blob_cache_bytes: u64,
    pub blob_cache_evictions: u64,
}

static QUEUED_BYTES: AtomicU64 = AtomicU64::new(0);
static PEAK_QUEUED_BYTES: AtomicU64 = AtomicU64::new(0);
static DROPPED_PACKETS: AtomicU64 = AtomicU64::new(0);
static RESNAPSHOTS: AtomicU64 = AtomicU64::new(0);
static BLOB_CACHE_HITS: AtomicU64 = AtomicU64::new(0);
static BLOB_CACHE_MISSES: AtomicU64 = AtomicU64::new(0);
static BLOB_CACHE_BYTES: AtomicU64 = AtomicU64::new(0);
static PEAK_BLOB_CACHE_BYTES: AtomicU64 = AtomicU64::new(0);
static BLOB_CACHE_EVICTIONS: AtomicU64 = AtomicU64::new(0);

#[must_use]
pub fn outbound_metrics_snapshot() -> OutboundMetricsSnapshot {
    OutboundMetricsSnapshot {
        queued_bytes: QUEUED_BYTES.load(Ordering::Relaxed),
        peak_queued_bytes: PEAK_QUEUED_BYTES.load(Ordering::Relaxed),
        dropped_packets: DROPPED_PACKETS.load(Ordering::Relaxed),
        resnapshots: RESNAPSHOTS.load(Ordering::Relaxed),
        blob_cache_hits: BLOB_CACHE_HITS.load(Ordering::Relaxed),
        blob_cache_misses: BLOB_CACHE_MISSES.load(Ordering::Relaxed),
        blob_cache_bytes: BLOB_CACHE_BYTES.load(Ordering::Relaxed),
        peak_blob_cache_bytes: PEAK_BLOB_CACHE_BYTES.load(Ordering::Relaxed),
        blob_cache_evictions: BLOB_CACHE_EVICTIONS.load(Ordering::Relaxed),
    }
}

pub fn record_outbound_drop() {
    DROPPED_PACKETS.fetch_add(1, Ordering::Relaxed);
}

#[allow(dead_code)]
pub fn record_outbound_resnapshot() {
    RESNAPSHOTS.fetch_add(1, Ordering::Relaxed);
}

pub fn record_blob_cache_hit(hit: bool) {
    if hit {
        BLOB_CACHE_HITS.fetch_add(1, Ordering::Relaxed);
    } else {
        BLOB_CACHE_MISSES.fetch_add(1, Ordering::Relaxed);
    }
}

pub fn add_blob_cache_bytes(bytes: usize) {
    let bytes = bytes as u64;
    let current = BLOB_CACHE_BYTES
        .fetch_add(bytes, Ordering::Relaxed)
        .saturating_add(bytes);
    PEAK_BLOB_CACHE_BYTES.fetch_max(current, Ordering::Relaxed);
}

pub fn remove_blob_cache_bytes(bytes: usize) {
    BLOB_CACHE_BYTES.fetch_sub(bytes as u64, Ordering::Relaxed);
}

pub fn record_blob_cache_eviction() {
    BLOB_CACHE_EVICTIONS.fetch_add(1, Ordering::Relaxed);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct StateRecoveryKey(u64);

#[derive(Debug, Clone, Copy)]
pub enum StatePacketKind {
    Metadata(i32),
    Relationship(i32),
}

impl StateRecoveryKey {
    pub const fn for_packet(kind: StatePacketKind, metadata_sequence: u64) -> Self {
        match kind {
            // Metadata deltas may cover different fields, so replay every dropped
            // delta in order rather than incorrectly coalescing by entity.
            StatePacketKind::Metadata(_entity_id) => Self(metadata_sequence & ((1u64 << 62) - 1)),
            StatePacketKind::Relationship(entity_id) => {
                Self((1u64 << 62) | entity_id as u32 as u64)
            }
        }
    }
}

#[derive(Default)]
struct PendingState {
    entries: BTreeMap<StateRecoveryKey, PendingPacket>,
    retained_bytes: usize,
    recovering: bool,
}

struct PendingPacket {
    sequence: u64,
    payload: bytes::Bytes,
}

#[derive(Default)]
pub struct StateRecoveryQueue {
    pending: Mutex<PendingState>,
    notify: tokio::sync::Notify,
}

impl StateRecoveryQueue {
    pub fn route(
        &self,
        key: StateRecoveryKey,
        sequence: u64,
        payload: bytes::Bytes,
        try_enqueue: impl FnOnce(bytes::Bytes) -> bool,
    ) -> Result<(), ()> {
        let mut pending = self
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        // Keep the decision and recovery transition under the same lock. Otherwise
        // two concurrent state packets could let a later packet use the fast path
        // while an earlier failed packet is only just entering recovery.
        if !pending.recovering && try_enqueue(payload.clone()) {
            return Ok(());
        }

        let is_new = !pending.entries.contains_key(&key);
        let previous_bytes = pending
            .entries
            .get(&key)
            .map_or(0, |packet| packet.payload.len());
        let retained_bytes = pending
            .retained_bytes
            .saturating_sub(previous_bytes)
            .saturating_add(payload.len());
        if (is_new && pending.entries.len() >= MAX_PENDING_STATE_ENTRIES)
            || retained_bytes > MAX_PENDING_STATE_BYTES
        {
            return Err(());
        }
        pending
            .entries
            .insert(key, PendingPacket { sequence, payload });
        pending.retained_bytes = retained_bytes;
        pending.recovering = true;
        drop(pending);
        self.notify.notify_one();
        Ok(())
    }

    pub async fn next_batch(&self) -> Vec<bytes::Bytes> {
        loop {
            let batch = {
                let mut pending = self
                    .pending
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if pending.entries.is_empty() {
                    None
                } else {
                    pending.retained_bytes = 0;
                    let mut packets = std::mem::take(&mut pending.entries)
                        .into_values()
                        .collect::<Vec<_>>();
                    packets.sort_unstable_by_key(|packet| packet.sequence);
                    Some(packets.into_iter().map(|packet| packet.payload).collect())
                }
            };
            if let Some(batch) = batch {
                return batch;
            }
            self.notify.notified().await;
        }
    }

    pub fn finish_batch(&self) {
        let mut pending = self
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if pending.entries.is_empty() {
            pending.recovering = false;
        }
    }

    #[cfg(test)]
    fn pending_snapshot(&self) -> (usize, usize) {
        let pending = self
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        (pending.entries.len(), pending.retained_bytes)
    }
}

#[derive(Debug, Default)]
pub struct PriorityBurst {
    consecutive_priority: usize,
}

impl PriorityBurst {
    pub const fn prefer_normal(&self) -> bool {
        self.consecutive_priority >= MAX_PRIORITY_BURST
    }

    pub const fn record_priority(&mut self) {
        self.consecutive_priority = self.consecutive_priority.saturating_add(1);
    }

    pub const fn record_normal(&mut self) {
        self.consecutive_priority = 0;
    }
}

#[derive(Debug)]
pub struct OutboundBytePermit {
    _permit: OwnedSemaphorePermit,
    bytes: u64,
}

impl OutboundBytePermit {
    fn new(permit: OwnedSemaphorePermit, bytes: usize) -> Self {
        let bytes = bytes as u64;
        let current = QUEUED_BYTES
            .fetch_add(bytes, Ordering::Relaxed)
            .saturating_add(bytes);
        PEAK_QUEUED_BYTES.fetch_max(current, Ordering::Relaxed);
        Self {
            _permit: permit,
            bytes,
        }
    }
}

impl Drop for OutboundBytePermit {
    fn drop(&mut self) {
        QUEUED_BYTES.fetch_sub(self.bytes, Ordering::Relaxed);
    }
}

#[derive(Clone)]
pub struct OutboundByteBudget {
    semaphore: Arc<Semaphore>,
    limit: usize,
}

impl Default for OutboundByteBudget {
    fn default() -> Self {
        Self::new(DEFAULT_OUTBOUND_BYTE_LIMIT)
    }
}

impl OutboundByteBudget {
    pub fn new(limit: usize) -> Self {
        assert!(limit > 0 && u32::try_from(limit).is_ok());
        Self {
            semaphore: Arc::new(Semaphore::new(limit)),
            limit,
        }
    }

    pub async fn reserve(&self, bytes: usize) -> Result<OutboundBytePermit, ByteBudgetError> {
        let permits = self.permits_for(bytes)?;
        self.semaphore
            .clone()
            .acquire_many_owned(permits)
            .await
            .map_err(|_| ByteBudgetError::Closed)
            .map(|permit| OutboundBytePermit::new(permit, bytes))
    }

    pub fn try_reserve(&self, bytes: usize) -> Result<OutboundBytePermit, ByteBudgetError> {
        let permits = self.permits_for(bytes)?;
        self.semaphore
            .clone()
            .try_acquire_many_owned(permits)
            .map_err(|error| match error {
                TryAcquireError::Closed => ByteBudgetError::Closed,
                TryAcquireError::NoPermits => {
                    record_outbound_drop();
                    ByteBudgetError::Full
                }
            })
            .map(|permit| OutboundBytePermit::new(permit, bytes))
    }

    fn permits_for(&self, bytes: usize) -> Result<u32, ByteBudgetError> {
        let bytes = bytes.max(1);
        if bytes > self.limit {
            record_outbound_drop();
            return Err(ByteBudgetError::Oversized {
                bytes,
                limit: self.limit,
            });
        }
        Ok(bytes as u32)
    }

    #[cfg(test)]
    fn available_bytes(&self) -> usize {
        self.semaphore.available_permits()
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::{
        ByteBudgetError, OutboundByteBudget, PriorityBurst, StatePacketKind, StateRecoveryKey,
        StateRecoveryQueue, outbound_metrics_snapshot, record_outbound_resnapshot,
    };

    static QUEUE_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    #[tokio::test]
    async fn queued_bytes_never_exceed_the_limit() {
        let _test_guard = QUEUE_TEST_LOCK.lock().await;
        let before = outbound_metrics_snapshot();
        let budget = OutboundByteBudget::new(8);
        let first = budget.reserve(6).await.unwrap();

        assert_eq!(
            outbound_metrics_snapshot().queued_bytes,
            before.queued_bytes + 6
        );

        assert!(matches!(budget.try_reserve(3), Err(ByteBudgetError::Full)));
        assert_eq!(budget.available_bytes(), 2);

        drop(first);
        let second = budget.try_reserve(8).unwrap();
        assert_eq!(budget.available_bytes(), 0);
        drop(second);
        assert_eq!(budget.available_bytes(), 8);
        assert_eq!(
            outbound_metrics_snapshot().queued_bytes,
            before.queued_bytes
        );
        assert!(outbound_metrics_snapshot().peak_queued_bytes >= before.queued_bytes + 8);
    }

    #[test]
    fn an_oversized_packet_is_rejected_without_consuming_budget() {
        let before = outbound_metrics_snapshot();
        let budget = OutboundByteBudget::new(8);
        assert_eq!(
            budget.try_reserve(9).unwrap_err(),
            ByteBudgetError::Oversized { bytes: 9, limit: 8 }
        );
        assert_eq!(budget.available_bytes(), 8);
        assert!(outbound_metrics_snapshot().dropped_packets > before.dropped_packets);
    }

    #[tokio::test]
    async fn resnapshot_counter_is_observable() {
        let _test_guard = QUEUE_TEST_LOCK.lock().await;
        let before = outbound_metrics_snapshot();
        record_outbound_resnapshot();
        assert_eq!(
            outbound_metrics_snapshot().resnapshots,
            before.resnapshots + 1
        );
    }

    #[test]
    fn priority_burst_requires_a_normal_packet_after_eight_priority_packets() {
        let mut burst = PriorityBurst::default();
        for _ in 0..super::MAX_PRIORITY_BURST {
            assert!(!burst.prefer_normal());
            burst.record_priority();
        }
        assert!(burst.prefer_normal());
        burst.record_normal();
        assert!(!burst.prefer_normal());
    }

    #[tokio::test]
    #[expect(
        clippy::print_stderr,
        reason = "the release acceptance log records slow-client p95/p99 samples"
    )]
    async fn slow_client_byte_budget_matrix_is_bounded_for_one_twenty_and_one_hundred_clients() {
        const CLIENT_LIMIT: usize = 64 * 1024;
        const PACKET_BYTES: usize = 4 * 1024;
        let _test_guard = QUEUE_TEST_LOCK.lock().await;

        for client_count in [1usize, 20, 100] {
            let before = outbound_metrics_snapshot();
            let budgets = (0..client_count)
                .map(|_| OutboundByteBudget::new(CLIENT_LIMIT))
                .collect::<Vec<_>>();
            let recovery_queues = (0..client_count)
                .map(|_| StateRecoveryQueue::default())
                .collect::<Vec<_>>();
            let permits_per_client = CLIENT_LIMIT / PACKET_BYTES;
            let mut retained = (0..client_count)
                .map(|_| Vec::with_capacity(permits_per_client))
                .collect::<Vec<_>>();
            let mut reserve_samples = Vec::with_capacity(client_count * permits_per_client);

            for (client_index, budget) in budgets.iter().enumerate() {
                for _ in 0..permits_per_client {
                    let started = Instant::now();
                    retained[client_index].push(budget.try_reserve(PACKET_BYTES).unwrap());
                    reserve_samples.push(started.elapsed().as_nanos());
                }
                assert!(matches!(
                    budget.try_reserve(PACKET_BYTES),
                    Err(ByteBudgetError::Full)
                ));
                assert_eq!(budget.available_bytes(), 0);

                let packet = bytes::Bytes::from(vec![0; PACKET_BYTES]);
                recovery_queues[client_index]
                    .route(
                        StateRecoveryKey::for_packet(
                            StatePacketKind::Metadata(client_index as i32),
                            0,
                        ),
                        0,
                        packet,
                        |_| false,
                    )
                    .unwrap();
            }

            let expected_bytes = (client_count * CLIENT_LIMIT) as u64;
            assert_eq!(
                outbound_metrics_snapshot().queued_bytes,
                before.queued_bytes + expected_bytes
            );
            reserve_samples.sort_unstable();
            let p95 = reserve_samples[(reserve_samples.len() - 1) * 95 / 100];
            let p99 = reserve_samples[(reserve_samples.len() - 1) * 99 / 100];
            eprintln!(
                "SER-033 slow-client: clients={client_count} queued_bytes={expected_bytes} reserve_p95_ns={p95} reserve_p99_ns={p99}"
            );

            for client_index in 0..client_count {
                retained[client_index].pop();
                let batch = recovery_queues[client_index].next_batch().await;
                assert_eq!(batch.len(), 1);
                let recovered = batch.into_iter().next().unwrap();
                retained[client_index].push(
                    budgets[client_index]
                        .reserve(recovered.len())
                        .await
                        .unwrap(),
                );
                record_outbound_resnapshot();
                recovery_queues[client_index].finish_batch();
                assert_eq!(recovery_queues[client_index].pending_snapshot(), (0, 0));
            }
            assert!(
                outbound_metrics_snapshot().resnapshots >= before.resnapshots + client_count as u64
            );
            assert_eq!(
                outbound_metrics_snapshot().queued_bytes,
                before.queued_bytes + expected_bytes
            );

            drop(retained);
            assert_eq!(
                outbound_metrics_snapshot().queued_bytes,
                before.queued_bytes
            );
        }
    }

    #[tokio::test]
    async fn state_recovery_replays_metadata_coalesces_relationships_and_enforces_its_bound() {
        let queue = StateRecoveryQueue::default();
        let key = StateRecoveryKey::for_packet(StatePacketKind::Relationship(42), 0);
        queue
            .route(key, 0, bytes::Bytes::from_static(b"old"), |_| false)
            .unwrap();
        queue
            .route(key, 1, bytes::Bytes::from_static(b"latest"), |_| {
                panic!("recovery barrier must prevent the fast path")
            })
            .unwrap();
        assert_eq!(queue.pending_snapshot(), (1, 6));

        let batch = queue.next_batch().await;
        assert_eq!(batch, vec![bytes::Bytes::from_static(b"latest")]);
        assert_eq!(queue.pending_snapshot(), (0, 0));
        queue.finish_batch();

        queue
            .route(
                StateRecoveryKey::for_packet(StatePacketKind::Metadata(42), 0),
                2,
                bytes::Bytes::from_static(b"first-delta"),
                |_| false,
            )
            .unwrap();
        queue
            .route(
                StateRecoveryKey::for_packet(StatePacketKind::Metadata(42), 1),
                3,
                bytes::Bytes::from_static(b"second-delta"),
                |_| panic!("recovery barrier must preserve metadata order"),
            )
            .unwrap();
        assert_eq!(
            queue.next_batch().await,
            vec![
                bytes::Bytes::from_static(b"first-delta"),
                bytes::Bytes::from_static(b"second-delta")
            ]
        );
        queue.finish_batch();

        let mut used_fast_path = false;
        queue
            .route(
                StateRecoveryKey::for_packet(StatePacketKind::Relationship(42), 0),
                4,
                bytes::Bytes::from_static(b"fast"),
                |_| {
                    used_fast_path = true;
                    true
                },
            )
            .unwrap();
        assert!(used_fast_path);
        assert_eq!(queue.pending_snapshot(), (0, 0));

        assert!(
            queue
                .route(
                    StateRecoveryKey::for_packet(StatePacketKind::Relationship(7), 0),
                    5,
                    bytes::Bytes::from(vec![0; super::MAX_PENDING_STATE_BYTES + 1]),
                    |_| false,
                )
                .is_err()
        );
        assert_eq!(queue.pending_snapshot(), (0, 0));
    }
}
