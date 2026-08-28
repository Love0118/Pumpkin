#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

/*
TODO
1. add proto chunk dirty flag
2. better priority
5. add lifetime to loading ticket
6. solve entity not unload problem
*/

pub type HashMapType<K, V> = rustc_hash::FxHashMap<K, V>;
pub type HashSetType<K> = rustc_hash::FxHashSet<K>;
pub type ChunkPos = pumpkin_util::math::vector2::Vector2<i32>;
pub type ChunkLevel = HashMapType<ChunkPos, i8>;
pub type IOLock = std::sync::Arc<(
    std::sync::Mutex<HashMapType<ChunkPos, u8>>,
    tokio::sync::Notify,
)>;

pub(crate) fn release_io_locks(lock: &IOLock, positions: impl IntoIterator<Item = ChunkPos>) {
    let mut locks = lock
        .0
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut released = false;
    for position in positions {
        match locks.entry(position) {
            std::collections::hash_map::Entry::Occupied(mut entry) => {
                let references = entry.get_mut();
                if *references == 1 {
                    entry.remove();
                    released = true;
                } else {
                    *references -= 1;
                }
            }
            std::collections::hash_map::Entry::Vacant(_) => {
                tracing::warn!(
                    "attempted to release missing chunk I/O lock entry for {position:?}"
                );
            }
        }
    }
    drop(locks);
    if released {
        lock.1.notify_waiters();
    }
}

pub struct ChunkWriteRequest {
    pub chunks: Vec<(ChunkPos, Chunk)>,
    pub force_flush: bool,
    pub completions: Vec<tokio::sync::oneshot::Sender<Result<(), String>>>,
    pub(crate) queue_guard: crate::serialization_metrics::SaveQueueGuard,
}

pub mod channel;
pub mod chunk_holder;
pub mod chunk_listener;
pub mod chunk_loading;
pub mod chunk_state;
pub mod dag;
pub mod generation;
pub mod generation_cache;
pub mod schedule;
pub mod worker_logic;

#[cfg(test)]
mod tests;

pub use channel::LevelChannel;
pub use chunk_holder::ChunkHolder;
pub use chunk_listener::ChunkListener;
pub use chunk_loading::ChunkLoading;
pub use chunk_state::{Chunk, StagedChunkEnum};
pub use dag::DAG;
pub use generation::generate_single_chunk;
pub use generation_cache::Cache;
pub use schedule::GenerationSchedule;
