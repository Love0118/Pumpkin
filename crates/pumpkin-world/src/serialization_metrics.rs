use std::sync::LazyLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SerializationMetricsSnapshot {
    pub save_queue_depth: u64,
    pub peak_save_queue_depth: u64,
    pub save_requests_in_flight: u64,
    pub peak_save_requests_in_flight: u64,
    pub regions_in_flight: u64,
    pub peak_regions_in_flight: u64,
    pub snapshot_bytes: u64,
    pub compressed_bytes: u64,
    pub written_bytes: u64,
    pub snapshot_nanos: u64,
    pub encode_nanos: u64,
    pub compress_nanos: u64,
    pub write_nanos: u64,
    pub fsync_nanos: u64,
    pub lock_hold_nanos: u64,
    pub latest_dirty_generation: u64,
    pub latest_durable_generation: u64,
    pub retries: u64,
}

#[derive(Clone, Copy)]
pub(crate) enum SerializationStage {
    Snapshot,
    Encode,
    Compress,
    Write,
    Fsync,
    LockHold,
}

#[derive(Default)]
struct SerializationMetrics {
    save_queue_depth: AtomicU64,
    peak_save_queue_depth: AtomicU64,
    save_requests_in_flight: AtomicU64,
    peak_save_requests_in_flight: AtomicU64,
    regions_in_flight: AtomicU64,
    peak_regions_in_flight: AtomicU64,
    snapshot_bytes: AtomicU64,
    compressed_bytes: AtomicU64,
    written_bytes: AtomicU64,
    snapshot_nanos: AtomicU64,
    encode_nanos: AtomicU64,
    compress_nanos: AtomicU64,
    write_nanos: AtomicU64,
    fsync_nanos: AtomicU64,
    lock_hold_nanos: AtomicU64,
    latest_dirty_generation: AtomicU64,
    latest_durable_generation: AtomicU64,
    retries: AtomicU64,
}

static METRICS: LazyLock<SerializationMetrics> = LazyLock::new(SerializationMetrics::default);

pub fn serialization_metrics_snapshot() -> SerializationMetricsSnapshot {
    SerializationMetricsSnapshot {
        save_queue_depth: METRICS.save_queue_depth.load(Ordering::Relaxed),
        peak_save_queue_depth: METRICS.peak_save_queue_depth.load(Ordering::Relaxed),
        save_requests_in_flight: METRICS.save_requests_in_flight.load(Ordering::Relaxed),
        peak_save_requests_in_flight: METRICS.peak_save_requests_in_flight.load(Ordering::Relaxed),
        regions_in_flight: METRICS.regions_in_flight.load(Ordering::Relaxed),
        peak_regions_in_flight: METRICS.peak_regions_in_flight.load(Ordering::Relaxed),
        snapshot_bytes: METRICS.snapshot_bytes.load(Ordering::Relaxed),
        compressed_bytes: METRICS.compressed_bytes.load(Ordering::Relaxed),
        written_bytes: METRICS.written_bytes.load(Ordering::Relaxed),
        snapshot_nanos: METRICS.snapshot_nanos.load(Ordering::Relaxed),
        encode_nanos: METRICS.encode_nanos.load(Ordering::Relaxed),
        compress_nanos: METRICS.compress_nanos.load(Ordering::Relaxed),
        write_nanos: METRICS.write_nanos.load(Ordering::Relaxed),
        fsync_nanos: METRICS.fsync_nanos.load(Ordering::Relaxed),
        lock_hold_nanos: METRICS.lock_hold_nanos.load(Ordering::Relaxed),
        latest_dirty_generation: METRICS.latest_dirty_generation.load(Ordering::Relaxed),
        latest_durable_generation: METRICS.latest_durable_generation.load(Ordering::Relaxed),
        retries: METRICS.retries.load(Ordering::Relaxed),
    }
}

pub(crate) struct InFlightGuard {
    current: &'static AtomicU64,
}

pub(crate) struct SaveQueueGuard {
    _in_flight: InFlightGuard,
}

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        self.current.fetch_sub(1, Ordering::Relaxed);
    }
}

fn begin_in_flight(current: &'static AtomicU64, peak: &'static AtomicU64) -> InFlightGuard {
    let value = current.fetch_add(1, Ordering::Relaxed).saturating_add(1);
    peak.fetch_max(value, Ordering::Relaxed);
    InFlightGuard { current }
}

pub(crate) fn begin_save_request() -> InFlightGuard {
    begin_in_flight(
        &METRICS.save_requests_in_flight,
        &METRICS.peak_save_requests_in_flight,
    )
}

pub(crate) fn enter_save_queue() -> SaveQueueGuard {
    SaveQueueGuard {
        _in_flight: begin_in_flight(&METRICS.save_queue_depth, &METRICS.peak_save_queue_depth),
    }
}

pub(crate) fn begin_region() -> InFlightGuard {
    begin_in_flight(&METRICS.regions_in_flight, &METRICS.peak_regions_in_flight)
}

pub(crate) fn record_snapshot_bytes(bytes: usize) {
    METRICS
        .snapshot_bytes
        .fetch_add(bytes as u64, Ordering::Relaxed);
}

pub(crate) fn record_compressed_bytes(bytes: usize) {
    METRICS
        .compressed_bytes
        .fetch_add(bytes as u64, Ordering::Relaxed);
}

pub(crate) fn record_written_bytes(bytes: u64) {
    METRICS.written_bytes.fetch_add(bytes, Ordering::Relaxed);
}

pub(crate) fn record_duration(stage: SerializationStage, duration: Duration) {
    let nanos = duration.as_nanos().min(u128::from(u64::MAX)) as u64;
    let metric = match stage {
        SerializationStage::Snapshot => &METRICS.snapshot_nanos,
        SerializationStage::Encode => &METRICS.encode_nanos,
        SerializationStage::Compress => &METRICS.compress_nanos,
        SerializationStage::Write => &METRICS.write_nanos,
        SerializationStage::Fsync => &METRICS.fsync_nanos,
        SerializationStage::LockHold => &METRICS.lock_hold_nanos,
    };
    metric.fetch_add(nanos, Ordering::Relaxed);
}

pub(crate) fn record_dirty_generation(generation: u64) {
    METRICS
        .latest_dirty_generation
        .fetch_max(generation, Ordering::Relaxed);
}

pub(crate) fn record_durable_generation(generation: u64) {
    METRICS
        .latest_durable_generation
        .fetch_max(generation, Ordering::Relaxed);
}

pub(crate) fn record_retry() {
    METRICS.retries.fetch_add(1, Ordering::Relaxed);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_exposes_gauges_bytes_duration_generation_and_retry() {
        let before = serialization_metrics_snapshot();
        {
            let _queued = enter_save_queue();
            let _save = begin_save_request();
            let _region = begin_region();
            assert_eq!(
                serialization_metrics_snapshot().save_queue_depth,
                before.save_queue_depth + 1
            );
            assert_eq!(
                serialization_metrics_snapshot().save_requests_in_flight,
                before.save_requests_in_flight + 1
            );
            record_snapshot_bytes(11);
            record_compressed_bytes(7);
            record_written_bytes(5);
            record_duration(SerializationStage::Encode, Duration::from_nanos(3));
            record_dirty_generation(9);
            record_durable_generation(8);
            record_retry();
        }
        let after = serialization_metrics_snapshot();
        assert_eq!(after.save_queue_depth, before.save_queue_depth);
        assert_eq!(
            after.save_requests_in_flight,
            before.save_requests_in_flight
        );
        assert_eq!(after.regions_in_flight, before.regions_in_flight);
        assert!(after.snapshot_bytes >= before.snapshot_bytes + 11);
        assert!(after.compressed_bytes >= before.compressed_bytes + 7);
        assert!(after.written_bytes >= before.written_bytes + 5);
        assert!(after.encode_nanos >= before.encode_nanos + 3);
        assert!(after.latest_dirty_generation >= 9);
        assert!(after.latest_durable_generation >= 8);
        assert!(after.retries >= before.retries + 1);
    }
}
