use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::Instant,
};

use futures::{StreamExt, future::join_all, stream};
use pumpkin_util::math::vector2::Vector2;
use tokio::{
    join,
    sync::{OnceCell, OwnedSemaphorePermit, RwLock, Semaphore, mpsc},
};
use tracing::{debug, error, trace};

use crate::{
    chunk::{
        ChunkReadingError, ChunkWritingError,
        io::{BoxFuture, Dirtiable},
    },
    level::LevelFolder,
    serialization_metrics::{self, SerializationStage},
};

use super::{ChunkSerializer, FileIO, LoadedData};

const DEFAULT_REGION_IO_BYTE_BUDGET: usize = 256 * 1024 * 1024;
const REGION_IO_BUDGET_UNIT_BYTES: usize = 64 * 1024;

#[derive(Clone)]
struct RegionMemoryBudget {
    semaphore: Arc<Semaphore>,
    unit_bytes: usize,
    total_units: u32,
}

impl RegionMemoryBudget {
    fn new(limit_bytes: usize, unit_bytes: usize) -> Self {
        let unit_bytes = unit_bytes.max(1);
        let total_units = limit_bytes
            .max(1)
            .div_ceil(unit_bytes)
            .min(Semaphore::MAX_PERMITS)
            .min(u32::MAX as usize) as u32;
        Self {
            semaphore: Arc::new(Semaphore::new(total_units as usize)),
            unit_bytes,
            total_units,
        }
    }

    async fn reserve(
        &self,
        bytes: usize,
    ) -> Result<OwnedSemaphorePermit, tokio::sync::AcquireError> {
        let requested_units = bytes.max(1).div_ceil(self.unit_bytes);
        let units = requested_units.min(self.total_units as usize) as u32;
        self.semaphore.clone().acquire_many_owned(units).await
    }

    #[cfg(test)]
    fn available_bytes(&self) -> usize {
        self.semaphore
            .available_permits()
            .saturating_mul(self.unit_bytes)
    }
}

/// A simple implementation of the `ChunkSerializer` trait that loads and saves data
/// to disk using parallelism and a lazy-loading cache keyed by file path.
///
/// ### Concurrency model
///
/// * `file_locks` — one `Arc<RwLock<S>>` per on-disk file, created lazily.
///   All readers/writers for the same region file share this lock, so there
///   are never two concurrent writers for the same file.
/// * `watchers` — a ref-count per path.  While a path has active watchers the
///   serializer is **not** evicted from the cache and the file is **not**
///   flushed to disk (the caller owns the flush lifecycle).
///
/// ### Lock ordering (must never be violated to avoid deadlocks)
///
/// 1. `file_locks`  (outer)
/// 2. individual `RwLock<S>` inside each loader  (inner)
/// 3. `watchers`  (independent — never held at the same time as either above)
///
/// `watchers` is always acquired in its own critical section, after all
/// serializer locks are released, which keeps it strictly independent.
pub struct ChunkFileManager<S: ChunkSerializer<WriteBackend = PathBuf>> {
    file_locks: RwLock<BTreeMap<PathBuf, Arc<ChunkSerializerLazyLoader<S>>>>,
    watchers: RwLock<BTreeMap<PathBuf, usize>>,
    operation_gate: RwLock<()>,
    chunk_config: S::ChunkConfig,
    max_region_concurrency: usize,
    memory_budget: RegionMemoryBudget,
}

pub(crate) trait PathFromLevelFolder {
    fn file_path(folder: &LevelFolder, file_name: &str) -> PathBuf;
}

struct ChunkSerializerLazyLoader<S: ChunkSerializer<WriteBackend = PathBuf>> {
    path: PathBuf,
    /// Initialised at most once; subsequent calls reuse the same Arc.
    internal: OnceCell<Arc<RwLock<S>>>,
    memory_budget: RegionMemoryBudget,
}

fn validate_region_file_size(path: &Path, size: u64, limit: u64) -> Result<(), ChunkReadingError> {
    if size > limit {
        return Err(ChunkReadingError::IoError(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "region file {} is {size} bytes, exceeding the {limit} byte limit",
                path.display()
            ),
        )));
    }
    Ok(())
}

impl<S: ChunkSerializer<WriteBackend = PathBuf> + 'static> ChunkSerializerLazyLoader<S> {
    fn new(path: PathBuf, memory_budget: RegionMemoryBudget) -> Self {
        Self {
            path,
            internal: OnceCell::new(),
            memory_budget,
        }
    }

    /// Returns `true` only when no outside caller still holds a clone of this
    /// loader *or* the inner serializer.
    ///
    /// # Safety requirement
    /// **Must be called while the write-lock on the parent `file_locks` map is
    /// held.**  That guarantees no new `Arc` clones can be issued while we
    /// inspect the strong counts.
    fn can_remove(loader: &Arc<Self>) -> bool {
        // The map itself holds 1 strong count; anything above that means an
        // active caller still has a handle.
        if Arc::strong_count(loader) > 1 {
            return false;
        }
        loader
            .internal
            .get()
            .is_none_or(|arc| Arc::strong_count(arc) == 1)
    }

    /// Returns the serializer, initialising it from disk on the first call.
    async fn get(&self) -> Result<Arc<RwLock<S>>, ChunkReadingError> {
        self.internal
            .get_or_try_init(|| async {
                let serializer = self.read_from_disk().await?;
                Ok(Arc::new(RwLock::new(serializer)))
            })
            .await
            .cloned()
    }

    async fn read_from_disk(&self) -> Result<S, ChunkReadingError> {
        trace!("Opening file from disk: {}", self.path.display());

        let file_size = match tokio::fs::metadata(&self.path).await {
            Ok(metadata) => {
                validate_region_file_size(&self.path, metadata.len(), S::MAX_FILE_BYTES)?;
                metadata.len()
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                trace!("File not found, using default for: {}", self.path.display());
                return Ok(S::default());
            }
            Err(error) => return Err(ChunkReadingError::IoError(error)),
        };

        let file_size = usize::try_from(file_size).unwrap_or(usize::MAX);
        let estimated_bytes = file_size
            .saturating_mul(S::READ_FILE_MEMORY_MULTIPLIER)
            .saturating_add(S::PEAK_REGION_WORKING_BYTES);
        let _memory_permit = self
            .memory_budget
            .reserve(estimated_bytes)
            .await
            .map_err(|error| ChunkReadingError::IoError(std::io::Error::other(error)))?;

        match tokio::fs::read(&self.path).await {
            Ok(bytes) => {
                validate_region_file_size(&self.path, bytes.len() as u64, S::MAX_FILE_BYTES)?;
                if bytes.is_empty() {
                    trace!(
                        "File is empty (0 bytes), using default for: {}",
                        self.path.display()
                    );
                    return Ok(S::default());
                }
                let value = tokio::task::spawn_blocking(move || S::read(bytes.into()))
                    .await
                    .map_err(|e| ChunkReadingError::IoError(std::io::Error::other(e)))??;
                trace!("Successfully read file from disk: {}", self.path.display());
                Ok(value)
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(S::default()),
            Err(err) => Err(ChunkReadingError::IoError(err)),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        path::{Path, PathBuf},
        sync::{
            Arc,
            atomic::{AtomicBool, AtomicU64, Ordering},
        },
    };

    use bytes::Bytes;
    use pumpkin_util::math::vector2::Vector2;
    use tempfile::tempdir;
    use tokio::sync::{Notify, mpsc};

    use super::{
        ChunkFileManager, PathFromLevelFolder, RegionMemoryBudget, validate_region_file_size,
    };
    use crate::{
        chunk::{
            ChunkReadingError, ChunkWritingError,
            io::{ChunkSerializer, Dirtiable, FileIO, LoadedData},
        },
        level::LevelFolder,
        serialization_metrics::serialization_metrics_snapshot,
    };

    struct TestChunk {
        position: Vector2<i32>,
        generation: AtomicU64,
        persisted_generation: AtomicU64,
    }

    impl TestChunk {
        fn new(position: Vector2<i32>) -> Self {
            Self {
                position,
                generation: AtomicU64::new(1),
                persisted_generation: AtomicU64::new(0),
            }
        }
    }

    impl Dirtiable for TestChunk {
        fn is_dirty(&self) -> bool {
            self.dirty_generation() != self.persisted_generation.load(Ordering::Acquire)
        }

        fn mark_dirty(&self, flag: bool) {
            if flag {
                self.generation.fetch_add(1, Ordering::AcqRel);
            } else {
                self.persisted_generation
                    .store(self.dirty_generation(), Ordering::Release);
            }
        }

        fn dirty_generation(&self) -> u64 {
            self.generation.load(Ordering::Acquire)
        }

        fn mark_persisted(&self, generation: u64) {
            if self.dirty_generation() == generation {
                self.persisted_generation
                    .store(generation, Ordering::Release);
            }
        }
    }

    impl PathFromLevelFolder for TestChunk {
        fn file_path(folder: &LevelFolder, file_name: &str) -> PathBuf {
            folder.region_folder.join(file_name)
        }
    }

    #[derive(Default)]
    struct TestSaveControl {
        write_started: Notify,
        continue_write: Notify,
        fail_next_write: AtomicBool,
    }

    #[derive(Default)]
    struct TestSerializer {
        control: Option<Arc<TestSaveControl>>,
    }

    impl ChunkSerializer for TestSerializer {
        type Data = TestChunk;
        type WriteBackend = PathBuf;
        type ChunkConfig = Arc<TestSaveControl>;

        fn get_chunk_key(_chunk: &Vector2<i32>) -> String {
            "r.0.0.test".to_string()
        }

        fn should_write(&self, _is_watched: bool) -> bool {
            true
        }

        async fn write(&self, _backend: &PathBuf) -> Result<(), std::io::Error> {
            if let Some(control) = &self.control {
                control.write_started.notify_one();
                control.continue_write.notified().await;
                if control.fail_next_write.swap(false, Ordering::AcqRel) {
                    return Err(std::io::Error::other("injected region write failure"));
                }
            }
            Ok(())
        }

        fn read(_raw: Bytes) -> Result<Self, ChunkReadingError> {
            Ok(Self::default())
        }

        async fn update_chunk(
            &mut self,
            _chunk_data: &TestChunk,
            chunk_config: &Arc<TestSaveControl>,
        ) -> Result<(), ChunkWritingError> {
            self.control = Some(chunk_config.clone());
            Ok(())
        }

        async fn get_chunks(
            &self,
            chunks: Vec<Vector2<i32>>,
            stream: mpsc::Sender<LoadedData<TestChunk, ChunkReadingError>>,
        ) {
            for position in chunks {
                if stream.send(LoadedData::Missing(position)).await.is_err() {
                    return;
                }
            }
        }
    }

    fn test_level_folder(root: &Path) -> LevelFolder {
        LevelFolder {
            root_folder: root.to_path_buf(),
            dim_folder: root.to_path_buf(),
            region_folder: root.join("region"),
            entities_folder: root.join("entities"),
            poi_folder: root.join("poi"),
        }
    }

    #[test]
    fn region_file_size_limit_is_inclusive_and_fail_closed() {
        let path = Path::new("r.0.0.mca");
        assert!(validate_region_file_size(path, 8, 8).is_ok());
        let error = validate_region_file_size(path, 9, 8).unwrap_err();
        assert!(error.to_string().contains("exceeding the 8 byte limit"));
    }

    #[tokio::test]
    async fn region_memory_budget_caps_weighted_operations() {
        let budget = RegionMemoryBudget::new(8, 1);
        let first = budget.reserve(6).await.unwrap();
        assert_eq!(budget.available_bytes(), 2);

        let blocked =
            tokio::time::timeout(std::time::Duration::from_millis(20), budget.reserve(3)).await;
        assert!(blocked.is_err());

        drop(first);
        let second = budget.reserve(8).await.unwrap();
        assert_eq!(budget.available_bytes(), 0);
        drop(second);
        assert_eq!(budget.available_bytes(), 8);
    }

    #[tokio::test]
    async fn oversized_region_operation_runs_alone() {
        let budget = RegionMemoryBudget::new(8, 1);
        let permit = budget.reserve(64).await.unwrap();
        assert_eq!(budget.available_bytes(), 0);
        drop(permit);
        assert_eq!(budget.available_bytes(), 8);
    }

    #[tokio::test]
    async fn failed_write_preserves_concurrent_dirty_generation_until_retry() {
        let metrics_before = serialization_metrics_snapshot();
        let directory = tempdir().unwrap();
        let folder = Arc::new(test_level_folder(directory.path()));
        let control = Arc::new(TestSaveControl::default());
        control.fail_next_write.store(true, Ordering::Release);
        let manager = Arc::new(ChunkFileManager::<TestSerializer>::new(control.clone()));
        let position = Vector2::new(0, 0);
        let chunk = Arc::new(TestChunk::new(position));

        let failed_save = {
            let manager = manager.clone();
            let folder = folder.clone();
            let chunk = chunk.clone();
            tokio::spawn(async move {
                manager
                    .save_chunks(&folder, vec![(position, chunk)], true)
                    .await
            })
        };

        control.write_started.notified().await;
        chunk.mark_dirty(true);
        assert_eq!(chunk.dirty_generation(), 2);
        control.continue_write.notify_one();
        assert!(failed_save.await.unwrap().is_err());
        assert!(chunk.is_dirty());
        assert_eq!(chunk.persisted_generation.load(Ordering::Acquire), 0);

        let retry = {
            let manager = manager.clone();
            let folder = folder.clone();
            let chunk = chunk.clone();
            tokio::spawn(async move {
                manager
                    .save_chunks(&folder, vec![(position, chunk)], true)
                    .await
            })
        };
        control.write_started.notified().await;
        control.continue_write.notify_one();
        retry.await.unwrap().unwrap();

        assert!(!chunk.is_dirty());
        assert_eq!(chunk.persisted_generation.load(Ordering::Acquire), 2);
        assert_eq!(chunk.position, position);
        let metrics_after = serialization_metrics_snapshot();
        assert!(metrics_after.retries >= metrics_before.retries + 1);
        assert!(metrics_after.latest_dirty_generation >= 2);
        assert!(metrics_after.latest_durable_generation >= 2);
        assert!(metrics_after.lock_hold_nanos > metrics_before.lock_hold_nanos);
        assert!(metrics_after.write_nanos > metrics_before.write_nanos);
    }

    #[tokio::test]
    async fn barrier_waits_for_operations_that_entered_before_it() {
        let directory = tempdir().unwrap();
        let folder = Arc::new(test_level_folder(directory.path()));
        let control = Arc::new(TestSaveControl::default());
        let manager = Arc::new(ChunkFileManager::<TestSerializer>::new(control.clone()));
        let position = Vector2::new(0, 0);
        let chunk = Arc::new(TestChunk::new(position));

        let save = {
            let manager = manager.clone();
            let folder = folder.clone();
            tokio::spawn(async move {
                manager
                    .save_chunks(&folder, vec![(position, chunk)], true)
                    .await
            })
        };
        control.write_started.notified().await;

        let mut barrier = {
            let manager = manager.clone();
            tokio::spawn(async move { manager.block_and_await_ongoing_tasks().await })
        };
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(20), &mut barrier)
                .await
                .is_err(),
            "barrier returned before the pre-existing write completed"
        );

        control.continue_write.notify_one();
        save.await.unwrap().unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(1), barrier)
            .await
            .unwrap()
            .unwrap();
    }
}

impl<S: ChunkSerializer<WriteBackend = PathBuf>> ChunkFileManager<S> {
    pub fn new(chunk_config: S::ChunkConfig) -> Self {
        let max_region_concurrency = std::thread::available_parallelism()
            .map_or(1, std::num::NonZero::get)
            .min(4);
        Self {
            file_locks: RwLock::new(BTreeMap::new()),
            watchers: RwLock::new(BTreeMap::new()),
            operation_gate: RwLock::new(()),
            chunk_config,
            max_region_concurrency,
            memory_budget: RegionMemoryBudget::new(
                DEFAULT_REGION_IO_BYTE_BUDGET,
                REGION_IO_BUDGET_UNIT_BYTES,
            ),
        }
    }
}

impl<S: ChunkSerializer<WriteBackend = PathBuf>> ChunkFileManager<S> {
    /// Returns the serializer for `path`, inserting a lazy-loader if absent.
    ///
    /// Uses an optimistic read-first pattern: in the common case (cache hit)
    /// we never need a write-lock on the map.
    async fn get_serializer(&self, path: &Path) -> Result<Arc<RwLock<S>>, ChunkReadingError> {
        {
            let locks = self.file_locks.read().await;
            if let Some(loader) = locks.get(path) {
                // Clone the Arc *before* releasing the lock so it stays alive.
                let loader = loader.clone();
                drop(locks);
                return loader.get().await;
            }
        }

        let loader = {
            let mut locks = self.file_locks.write().await;
            locks
                .entry(path.into())
                .or_insert_with(|| {
                    Arc::new(ChunkSerializerLazyLoader::new(
                        path.into(),
                        self.memory_budget.clone(),
                    ))
                })
                .clone()
            // Write-lock dropped here — `loader.get()` may block on I/O and
            // must not hold the map lock.
        };

        loader.get().await
    }

    /// Attempt to evict the cached serializer for `path`.
    ///
    /// The entry is only removed when *both* conditions hold:
    /// 1. No watcher still references the path.
    /// 2. No other `Arc` clone is live (ensured via `can_remove`).
    async fn maybe_evict(&self, path: &PathBuf) {
        // Check watchers independently of file_locks to honour lock ordering.
        let still_watched = {
            let watchers = self.watchers.read().await;
            watchers.get(path).is_some_and(|&c| c > 0)
        };

        if still_watched {
            return;
        }

        let mut locks = self.file_locks.write().await;
        let removable = locks
            .get(path)
            .is_some_and(ChunkSerializerLazyLoader::can_remove);

        if removable {
            locks.remove(path);
            trace!("Evicted serializer cache for {}", path.display());
        } else {
            trace!(
                "Skipping eviction for {} — references still live",
                path.display()
            );
        }
    }
}

impl<P, S> FileIO for ChunkFileManager<S>
where
    P: PathFromLevelFolder + Send + Sync + Sized + Dirtiable + 'static,
    S: ChunkSerializer<Data = P, WriteBackend = PathBuf>,
    S::ChunkConfig: Send + Sync,
{
    type Data = Arc<S::Data>;

    fn watch_chunks<'a>(
        &'a self,
        folder: &'a LevelFolder,
        chunks: &'a [Vector2<i32>],
    ) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            let paths: Vec<_> = chunks
                .iter()
                .map(|c| P::file_path(folder, &S::get_chunk_key(c)))
                .collect();

            let mut watchers = self.watchers.write().await;
            for path in paths {
                *watchers.entry(path).or_insert(0) += 1;
            }
        })
    }

    fn unwatch_chunks<'a>(
        &'a self,
        folder: &'a LevelFolder,
        chunks: &'a [Vector2<i32>],
    ) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            let paths: Vec<_> = chunks
                .iter()
                .map(|c| P::file_path(folder, &S::get_chunk_key(c)))
                .collect();

            let mut paths_to_evict = Vec::new();
            {
                let mut watchers = self.watchers.write().await;
                for path in paths {
                    if let std::collections::btree_map::Entry::Occupied(mut e) =
                        watchers.entry(path)
                    {
                        let count = e.get_mut();
                        *count = count.saturating_sub(1);
                        if *count == 0 {
                            let (path, _) = e.remove_entry();
                            paths_to_evict.push(path);
                        }
                    }
                }
            }

            for path in paths_to_evict {
                self.maybe_evict(&path).await;
            }
        })
    }

    fn clear_watched_chunks(&self) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            let paths: Vec<PathBuf> = {
                let mut watchers = self.watchers.write().await;
                let keys: Vec<_> = watchers.keys().cloned().collect();
                watchers.clear();
                keys
            };
            for path in paths {
                self.maybe_evict(&path).await;
            }
        })
    }

    fn fetch_chunks<'a>(
        &'a self,
        folder: &'a LevelFolder,
        chunk_coords: &'a [Vector2<i32>],
        stream: mpsc::Sender<LoadedData<Self::Data, ChunkReadingError>>,
    ) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            let _operation = self.operation_gate.read().await;
            // Group requested chunk coords by their region file.
            let mut regions_chunks: BTreeMap<String, Vec<Vector2<i32>>> = BTreeMap::new();
            for at in chunk_coords {
                regions_chunks
                    .entry(S::get_chunk_key(at))
                    .or_default()
                    .push(*at);
            }

            let region_tasks = regions_chunks.into_iter().map(|(file_name, chunks)| {
                let task_stream = stream.clone();
                async move {
                    let _region_in_flight = serialization_metrics::begin_region();
                    let path = P::file_path(folder, &file_name);

                    let chunk_serializer = match self.get_serializer(&path).await {
                        Ok(s) => s,
                        Err(ChunkReadingError::ChunkNotExist) => {
                            return;
                        }
                        Err(err) => {
                            // Best-effort: report the error for the first coord in the batch.
                            let _ = task_stream.send(LoadedData::Error((chunks[0], err))).await;
                            return;
                        }
                    };

                    let _memory_permit = match self
                        .memory_budget
                        .reserve(S::estimated_fetch_peak_bytes(chunks.len()))
                        .await
                    {
                        Ok(permit) => permit,
                        Err(error) => {
                            let _ = task_stream
                                .send(LoadedData::Error((
                                    chunks[0],
                                    ChunkReadingError::IoError(std::io::Error::other(error)),
                                )))
                                .await;
                            return;
                        }
                    };

                    // A bounded channel of 1 keeps backpressure between the
                    // serializer and the caller without unbounded buffering.
                    let (send, mut recv) =
                        mpsc::channel::<LoadedData<S::Data, ChunkReadingError>>(1);

                    // Forward received chunks, wrapping them in `Arc`.
                    // Captured move is intentional — `task_stream` is consumed here.
                    let forward = async move {
                        while let Some(data) = recv.recv().await {
                            let wrapped = data.map_loaded(Arc::new);
                            if task_stream.send(wrapped).await.is_err() {
                                // Receiver dropped; abort early to avoid wasted work.
                                return;
                            }
                        }
                    };

                    // Hold the read lock only for the duration of `get_chunks`.
                    let read = async move {
                        let serializer = chunk_serializer.read().await;
                        serializer.get_chunks(chunks, send).await;
                    };

                    join!(forward, read);

                    // Evict if not watched and references are dropped
                    self.maybe_evict(&path).await;
                }
            });

            stream::iter(region_tasks)
                .buffer_unordered(self.max_region_concurrency)
                .collect::<Vec<()>>()
                .await;
        })
    }

    #[expect(
        clippy::too_many_lines,
        reason = "the transactional region update, flush, generation ack, and eviction order is intentionally kept visible"
    )]
    fn save_chunks<'a>(
        &'a self,
        folder: &'a LevelFolder,
        chunks_data: Vec<(Vector2<i32>, Self::Data)>,
        force_flush: bool,
    ) -> BoxFuture<'a, Result<(), ChunkWritingError>> {
        Box::pin(async move {
            let _save_request_in_flight = serialization_metrics::begin_save_request();
            let _operation = self.operation_gate.read().await;
            // Group chunks by region file.
            let mut regions_chunks: BTreeMap<String, Vec<Self::Data>> = BTreeMap::new();
            for (at, chunk) in chunks_data {
                regions_chunks
                    .entry(S::get_chunk_key(&at))
                    .or_default()
                    .push(chunk);
            }

            let tasks = regions_chunks
                .into_iter()
                .map(|(file_name, chunk_locks)| async move {
                    let _region_in_flight = serialization_metrics::begin_region();
                    let path = P::file_path(folder, &file_name);
                    trace!("Saving chunks into {}", path.display());

                    let chunk_serializer = match self.get_serializer(&path).await {
                        Ok(s) => s,
                        Err(ChunkReadingError::ChunkNotExist) => {
                            return Err(ChunkWritingError::IoError(std::io::Error::other(
                                "get_serializer returned ChunkNotExist",
                            )));
                        }
                        Err(ChunkReadingError::IoError(err)) => {
                            error!("I/O error reading region before write: {err}");
                            return Err(ChunkWritingError::IoError(err));
                        }
                        Err(err) => {
                            return Err(ChunkWritingError::IoError(std::io::Error::other(
                                err.to_string(),
                            )));
                        }
                    };

                    let dirty_chunk_count =
                        chunk_locks.iter().filter(|chunk| chunk.is_dirty()).count();
                    let _memory_permit = self
                        .memory_budget
                        .reserve(S::estimated_write_peak_bytes(dirty_chunk_count))
                        .await
                        .map_err(|error| {
                            ChunkWritingError::IoError(std::io::Error::other(error))
                        })?;

                    let mut serialized_generations = Vec::new();
                    let update_result = {
                        let mut writer = chunk_serializer.write().await;
                        let lock_started = Instant::now();
                        let update_result: Result<(), ChunkWritingError> = async {
                            for chunk in &chunk_locks {
                                if chunk.is_dirty() {
                                    let generation = chunk.dirty_generation();
                                    serialization_metrics::record_dirty_generation(generation);
                                    writer.update_chunk(&**chunk, &self.chunk_config).await?;
                                    serialized_generations.push((chunk.clone(), generation));
                                }
                            }
                            Ok(())
                        }
                        .await;
                        drop(writer);
                        serialization_metrics::record_duration(
                            SerializationStage::LockHold,
                            lock_started.elapsed(),
                        );
                        update_result
                        // Write-lock released here — flush can proceed under a read-lock.
                    };
                    update_result?;

                    trace!("Chunk data updated for {}", path.display());

                    // We check watchers *after* releasing the write-lock to honour
                    // lock ordering (serializer lock → watchers, never the reverse).
                    let is_watched = {
                        let watchers = self.watchers.read().await;
                        watchers.get(&path).is_some_and(|&c| c > 0)
                    };

                    if force_flush || !is_watched {
                        // A read-lock suffices for `write()` since we have already
                        // applied all mutations above.
                        {
                            let serializer = chunk_serializer.read().await;
                            let lock_started = Instant::now();
                            debug!("Flushing {} to disk", path.display());
                            let write_started = Instant::now();
                            let write_result = serializer.write(&path).await;
                            serialization_metrics::record_duration(
                                SerializationStage::Write,
                                write_started.elapsed(),
                            );
                            drop(serializer);
                            serialization_metrics::record_duration(
                                SerializationStage::LockHold,
                                lock_started.elapsed(),
                            );
                            write_result.map_err(ChunkWritingError::IoError)?;
                            // Read-lock released here.
                        };

                        for (chunk, generation) in serialized_generations {
                            chunk.mark_persisted(generation);
                            serialization_metrics::record_durable_generation(generation);
                        }

                        // Drop our handle so `can_remove` may succeed.
                        drop(chunk_serializer);

                        // Evict the cache entry when no longer needed.
                        self.maybe_evict(&path).await;
                    }

                    Ok(())
                });

            // Collect all region results; surface the first error encountered.
            let results: Vec<Result<(), ChunkWritingError>> = stream::iter(tasks)
                .buffer_unordered(self.max_region_concurrency)
                .collect()
                .await;
            for _ in results.iter().filter(|result| result.is_err()) {
                // A failed dirty region remains eligible for the next scheduler
                // collection, so this records one retained retry opportunity.
                serialization_metrics::record_retry();
            }
            results.into_iter().find(Result::is_err).unwrap_or(Ok(()))
        })
    }

    /// Blocks until all in-flight serialiser operations have completed by
    /// acquiring (and immediately releasing) a write-lock on every cached
    /// serialiser.
    ///
    /// This is a linearisation point: after this future resolves no mutation
    /// started before the call is still running.
    fn block_and_await_ongoing_tasks(&self) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            // Tokio's fair RwLock queues later operations behind this writer.
            // Acquiring it therefore completes every fetch/save that entered the
            // gate before the barrier and prevents a newer operation from
            // overtaking the linearisation point.
            let _barrier = self.operation_gate.write().await;

            // Snapshot the current set of loaders under a read-lock so we do
            // not block new insertions longer than necessary.
            let loaders: Vec<Arc<ChunkSerializerLazyLoader<S>>> =
                { self.file_locks.read().await.values().cloned().collect() };

            // For each loader that has been initialised, acquire a write-lock
            // and release it immediately.  This guarantees that any concurrent
            // read or write operation that was in progress has finished.
            let drain_tasks = loaders.into_iter().map(|loader| async move {
                if let Some(serializer_arc) = loader.internal.get() {
                    // Acquiring + immediately dropping the write-lock acts as a
                    // barrier: it can only succeed once all current lock holders
                    // have released their guards.
                    let _guard = serializer_arc.write().await;
                }
            });

            join_all(drain_tasks).await;
        })
    }
}

pub enum LevelFileIO<Linear, Anvil, Pump>
where
    Linear: ChunkSerializer<WriteBackend = PathBuf>,
    Anvil: ChunkSerializer<WriteBackend = PathBuf>,
    Pump: ChunkSerializer<WriteBackend = PathBuf>,
{
    Linear(ChunkFileManager<Linear>),
    Anvil(ChunkFileManager<Anvil>),
    Pump(ChunkFileManager<Pump>),
}

impl<P, Linear, Anvil, Pump> FileIO for LevelFileIO<Linear, Anvil, Pump>
where
    P: PathFromLevelFolder + Send + Sync + Sized + Dirtiable + 'static,
    Linear: ChunkSerializer<Data = P, WriteBackend = PathBuf>,
    Anvil: ChunkSerializer<Data = P, WriteBackend = PathBuf>,
    Pump: ChunkSerializer<Data = P, WriteBackend = PathBuf>,
    Linear::ChunkConfig: Send + Sync,
    Anvil::ChunkConfig: Send + Sync,
    Pump::ChunkConfig: Send + Sync,
{
    type Data = Arc<P>;

    fn fetch_chunks<'a>(
        &'a self,
        folder: &'a LevelFolder,
        chunk_coords: &'a [Vector2<i32>],
        stream: tokio::sync::mpsc::Sender<LoadedData<Self::Data, ChunkReadingError>>,
    ) -> BoxFuture<'a, ()> {
        match self {
            Self::Linear(io) => io.fetch_chunks(folder, chunk_coords, stream),
            Self::Anvil(io) => io.fetch_chunks(folder, chunk_coords, stream),
            Self::Pump(io) => io.fetch_chunks(folder, chunk_coords, stream),
        }
    }

    fn save_chunks<'a>(
        &'a self,
        folder: &'a LevelFolder,
        chunks_data: Vec<(Vector2<i32>, Self::Data)>,
        force_flush: bool,
    ) -> BoxFuture<'a, Result<(), ChunkWritingError>> {
        match self {
            Self::Linear(io) => io.save_chunks(folder, chunks_data, force_flush),
            Self::Anvil(io) => io.save_chunks(folder, chunks_data, force_flush),
            Self::Pump(io) => io.save_chunks(folder, chunks_data, force_flush),
        }
    }

    fn watch_chunks<'a>(
        &'a self,
        folder: &'a LevelFolder,
        chunks: &'a [Vector2<i32>],
    ) -> BoxFuture<'a, ()> {
        match self {
            Self::Linear(io) => io.watch_chunks(folder, chunks),
            Self::Anvil(io) => io.watch_chunks(folder, chunks),
            Self::Pump(io) => io.watch_chunks(folder, chunks),
        }
    }

    fn unwatch_chunks<'a>(
        &'a self,
        folder: &'a LevelFolder,
        chunks: &'a [Vector2<i32>],
    ) -> BoxFuture<'a, ()> {
        match self {
            Self::Linear(io) => io.unwatch_chunks(folder, chunks),
            Self::Anvil(io) => io.unwatch_chunks(folder, chunks),
            Self::Pump(io) => io.unwatch_chunks(folder, chunks),
        }
    }

    fn clear_watched_chunks(&self) -> BoxFuture<'_, ()> {
        match self {
            Self::Linear(io) => io.clear_watched_chunks(),
            Self::Anvil(io) => io.clear_watched_chunks(),
            Self::Pump(io) => io.clear_watched_chunks(),
        }
    }

    fn block_and_await_ongoing_tasks(&self) -> BoxFuture<'_, ()> {
        match self {
            Self::Linear(io) => io.block_and_await_ongoing_tasks(),
            Self::Anvil(io) => io.block_and_await_ongoing_tasks(),
            Self::Pump(io) => io.block_and_await_ongoing_tasks(),
        }
    }
}
