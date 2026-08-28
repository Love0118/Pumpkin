use std::{error, pin::Pin};

use bytes::Bytes;
use pumpkin_util::math::vector2::Vector2;

use super::{ChunkReadingError, ChunkWritingError};
use crate::level::LevelFolder;

pub mod file_manager;

const DECOMPRESSION_RATIO_GRACE_BYTES: usize = 64 * 1024;
const MAX_DECOMPRESSION_RATIO: usize = 512;

const fn maximum_compressed_bytes(max_decompressed_bytes: usize) -> usize {
    max_decompressed_bytes
        .saturating_add(max_decompressed_bytes / 16)
        .saturating_add(DECOMPRESSION_RATIO_GRACE_BYTES)
}

/// Returns the largest decompressed payload a decoder may produce for this
/// compressed input. Small payloads receive a fixed grace window; larger inputs
/// are bounded by both the absolute format cap and a compression-ratio cap.
pub(crate) fn decompression_output_limit(
    compressed_bytes: usize,
    max_decompressed_bytes: usize,
) -> std::io::Result<usize> {
    let max_compressed_bytes = maximum_compressed_bytes(max_decompressed_bytes);
    if compressed_bytes > max_compressed_bytes {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "compressed payload has {compressed_bytes} bytes, exceeding the {max_compressed_bytes} byte format limit"
            ),
        ));
    }

    Ok(max_decompressed_bytes.min(
        DECOMPRESSION_RATIO_GRACE_BYTES
            .max(compressed_bytes.saturating_mul(MAX_DECOMPRESSION_RATIO)),
    ))
}

/// The result of loading a chunk data.
///
/// It can be the data loaded successfully, the data not found or an error
/// with the chunk coordinates and the error that occurred.
pub enum LoadedData<D: Send, Err: error::Error> {
    /// The chunk data was loaded successfully
    Loaded(D),

    /// The chunk data was not found
    Missing(Vector2<i32>),

    /// An error occurred while loading the chunk data
    Error((Vector2<i32>, Err)),
}

impl<D: Send, E: error::Error> LoadedData<D, E> {
    pub fn map_loaded<D2: Send>(self, map: impl FnOnce(D) -> D2) -> LoadedData<D2, E> {
        match self {
            Self::Loaded(data) => LoadedData::Loaded(map(data)),
            Self::Missing(pos) => LoadedData::Missing(pos),
            Self::Error(err) => LoadedData::Error(err),
        }
    }
}

pub trait Dirtiable {
    fn is_dirty(&self) -> bool;
    fn mark_dirty(&self, flag: bool);

    /// Returns the mutation generation represented by the next save snapshot.
    fn dirty_generation(&self) -> u64 {
        u64::from(self.is_dirty())
    }

    /// Marks `generation` durable without clearing mutations that happened after
    /// that snapshot was captured.
    fn mark_persisted(&self, generation: u64) {
        if self.dirty_generation() == generation {
            self.mark_dirty(false);
        }
    }
}

type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Trait to handle the IO of chunks
/// for loading and saving chunks data
/// can be implemented for different types of IO
/// or with different optimizations
///
/// The `R` type is the type of the data that will be loaded/saved
/// like `ChunkData` or `EntityData`
pub trait FileIO
where
    Self: Send + Sync,
{
    type Data: Send + Sync + Sized;

    /// Load the chunks data
    fn fetch_chunks<'a>(
        &'a self,
        folder: &'a LevelFolder,
        chunk_coords: &'a [Vector2<i32>],
        stream: tokio::sync::mpsc::Sender<LoadedData<Self::Data, ChunkReadingError>>,
    ) -> BoxFuture<'a, ()>; // Returns BoxFuture<()>

    /// Persist the chunks data
    fn save_chunks<'a>(
        &'a self,
        folder: &'a LevelFolder,
        chunks_data: Vec<(Vector2<i32>, Self::Data)>,
        force_flush: bool,
    ) -> BoxFuture<'a, Result<(), ChunkWritingError>>; // Returns BoxFuture<Result>

    /// Tells the `ChunkIO` that these chunks are currently loaded in memory
    fn watch_chunks<'a>(
        &'a self,
        folder: &'a LevelFolder,
        chunks: &'a [Vector2<i32>],
    ) -> BoxFuture<'a, ()>;

    /// Tells the `ChunkIO` that these chunks are no longer loaded in memory
    fn unwatch_chunks<'a>(
        &'a self,
        folder: &'a LevelFolder,
        chunks: &'a [Vector2<i32>],
    ) -> BoxFuture<'a, ()>;

    /// Tells the `ChunkIO` that no more chunks are loaded in memory
    fn clear_watched_chunks(&self) -> BoxFuture<'_, ()>;

    /// Ensure that all ongoing operations are finished
    fn block_and_await_ongoing_tasks(&self) -> BoxFuture<'_, ()>;
}

/// Trait to serialize and deserialize the chunk data to and from bytes.
///
/// The `Data` type is the type of the data that will be updated or serialized/deserialized
/// like `ChunkData` or `EntityData`
pub trait ChunkSerializer: Send + Sync + Default + 'static {
    /// Hard read-before-parse limit for a single region file.
    const MAX_FILE_BYTES: u64 = 1024 * 1024 * 1024;
    /// Conservative transient memory used by one region codec operation.
    const PEAK_REGION_WORKING_BYTES: usize = 64 * 1024 * 1024;
    /// Retained compressed payload growth estimated for each updated chunk.
    const RETAINED_BYTES_PER_DIRTY_CHUNK: usize = 1024 * 1024;
    /// Peak multiplier while raw file bytes and parsed data coexist.
    const READ_FILE_MEMORY_MULTIPLIER: usize = 2;

    type Data: Send + Sync + Sized + Dirtiable;
    type WriteBackend;

    type ChunkConfig;

    /// Get the key for the chunk (like the file name)
    fn get_chunk_key(chunk: &Vector2<i32>) -> String;

    fn should_write(&self, is_watched: bool) -> bool;

    #[must_use]
    fn estimated_fetch_peak_bytes(_requested_chunks: usize) -> usize {
        Self::PEAK_REGION_WORKING_BYTES
    }

    #[must_use]
    fn estimated_write_peak_bytes(dirty_chunks: usize) -> usize {
        Self::PEAK_REGION_WORKING_BYTES
            .saturating_add(Self::RETAINED_BYTES_PER_DIRTY_CHUNK.saturating_mul(dirty_chunks))
    }

    /// Serialize the data to bytes.
    fn write(
        &self,
        backend: &Self::WriteBackend,
    ) -> impl Future<Output = Result<(), std::io::Error>> + Send;

    /// Create a new instance from bytes
    fn read(r: Bytes) -> Result<Self, ChunkReadingError>;

    /// Add the chunk data to the serializer
    fn update_chunk(
        &mut self,
        chunk_data: &Self::Data,
        chunk_config: &Self::ChunkConfig,
    ) -> impl Future<Output = Result<(), ChunkWritingError>> + Send;

    /// Get the chunks data from the serializer
    fn get_chunks(
        &self,
        chunks: Vec<Vector2<i32>>,
        stream: tokio::sync::mpsc::Sender<LoadedData<Self::Data, ChunkReadingError>>,
    ) -> impl Future<Output = ()> + Send;
}

#[cfg(test)]
mod tests {
    use super::{
        DECOMPRESSION_RATIO_GRACE_BYTES, MAX_DECOMPRESSION_RATIO, decompression_output_limit,
        maximum_compressed_bytes,
    };

    #[test]
    fn decompression_envelope_accepts_boundary_and_rejects_hostile_sizes() {
        let maximum = 64 * 1024 * 1024;
        let boundary_compressed = maximum / MAX_DECOMPRESSION_RATIO;
        assert_eq!(
            decompression_output_limit(boundary_compressed, maximum).unwrap(),
            maximum
        );
        assert_eq!(
            decompression_output_limit(1, maximum).unwrap(),
            DECOMPRESSION_RATIO_GRACE_BYTES
        );
        assert!(
            decompression_output_limit(maximum_compressed_bytes(maximum) + 1, maximum).is_err()
        );
        assert!(decompression_output_limit(boundary_compressed - 1, maximum).unwrap() < maximum);
    }
}
