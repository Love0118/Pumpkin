use std::collections::HashMap;
use std::io::{ErrorKind, Read, SeekFrom};
use std::marker::PhantomData;
use std::path::PathBuf;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use crate::chunk::format::anvil::{AnvilChunkFile, SingleChunkDataSerializer};
use crate::chunk::io::{ChunkSerializer, LoadedData, decompression_output_limit};
use crate::chunk::{ChunkReadingError, ChunkWritingError};
use bytes::{Buf, BufMut, Bytes};
use pumpkin_util::math::vector2::Vector2;
use ruzstd::decoding::StreamingDecoder;
use ruzstd::encoding::{CompressionLevel, compress_to_vec};
use tokio::io::{AsyncSeekExt, AsyncWriteExt, BufWriter};
use tracing::{error, warn};
use xxhash_rust::xxh64::xxh64;

use crate::{
    persistence::AsyncAtomicFile,
    serialization_metrics::{self, SerializationStage},
};

use super::anvil::CHUNK_COUNT;

/// The signature used as both header and footer.
/// <https://gist.github.com/Aaron2550/5701519671253d4c6190bde6706f9f98>
const SIGNATURE: [u8; 8] = u64::to_be_bytes(0xc3ff13183cca9d9a);
const MAX_DECOMPRESSED_BUCKET_BYTES: usize = 256 * 1024 * 1024;

/// Allowed grid sizes for v2 bucket subdivision.
const VALID_GRID_SIZES: &[u8] = &[1, 2, 4, 8, 16, 32];

/// Default grid size.  2×2 = 4 buckets gives a reasonable write amplification
/// tradeoff for most worlds.
const DEFAULT_GRID_SIZE: u8 = 2;

// ---------------------------------------------------------------------------
// Superblock  (26 bytes)
// ---------------------------------------------------------------------------
//
//  0.. 8   uint64  Signature  (0xc3ff13183cca9d9a)
//  8.. 9   uint8   Version    (2)
//  9..17   uint64  Newest timestamp
// 17..18   int8    Grid size
// 18..22   int32   Region X
// 22..26   int32   Region Z

struct LinearV2Superblock {
    newest_timestamp: u64,
    /// One of {1, 2, 4, 8, 16, 32}.
    grid_size: u8,
    region_x: i32,
    region_z: i32,
}

impl LinearV2Superblock {
    const SIZE: usize = 26;

    fn from_bytes(buf: &[u8]) -> Result<Self, ChunkReadingError> {
        if buf.len() < Self::SIZE {
            return Err(ChunkReadingError::IoError(std::io::Error::from(
                ErrorKind::UnexpectedEof,
            )));
        }
        let mut b = buf;
        let mut sig = [0u8; 8];
        sig.copy_from_slice(&b[..8]);
        b = &b[8..];

        if sig != SIGNATURE {
            error!("Linear v2: invalid superblock signature");
            return Err(ChunkReadingError::InvalidHeader);
        }

        let version = b.get_u8();
        if version != 0x02 {
            error!("Linear v2: unexpected version byte {version:#x} in superblock");
            return Err(ChunkReadingError::InvalidHeader);
        }

        let newest_timestamp = b.get_u64();
        let grid_size = b.get_u8();
        let region_x = b.get_i32();
        let region_z = b.get_i32();

        if !VALID_GRID_SIZES.contains(&grid_size) {
            error!("Linear v2: invalid grid size {grid_size}");
            return Err(ChunkReadingError::InvalidHeader);
        }

        Ok(Self {
            newest_timestamp,
            grid_size,
            region_x,
            region_z,
        })
    }

    fn to_bytes(&self) -> [u8; Self::SIZE] {
        let mut out = [0u8; Self::SIZE];
        let mut b = out.as_mut_slice();
        b.put_slice(&SIGNATURE);
        b.put_u8(0x02);
        b.put_u64(self.newest_timestamp);
        b.put_u8(self.grid_size);
        b.put_i32(self.region_x);
        b.put_i32(self.region_z);
        out
    }
}

// ---------------------------------------------------------------------------
// Chunk existence bitmap  (128 bytes = 1024 bits)
// ---------------------------------------------------------------------------

struct ChunkBitmap([u8; 128]);

impl ChunkBitmap {
    const SIZE: usize = 128;

    const fn new() -> Self {
        Self([0u8; 128])
    }

    const fn set(&mut self, index: usize, exists: bool) {
        let byte = index / 8;
        let bit = index % 8;
        if exists {
            self.0[byte] |= 1 << bit;
        } else {
            self.0[byte] &= !(1 << bit);
        }
    }

    #[allow(dead_code)]
    const fn get(&self, index: usize) -> bool {
        let byte = index / 8;
        let bit = index % 8;
        (self.0[byte] >> bit) & 1 == 1
    }
}

// ---------------------------------------------------------------------------
// NBT features dictionary
// ---------------------------------------------------------------------------
//
// Repeated: u8 key_len, key bytes, u32 value
// Terminated by a single 0x00 byte.

struct NbtFeatures(HashMap<String, u32>);

impl NbtFeatures {
    fn empty() -> Self {
        Self(HashMap::new())
    }

    fn from_bytes(buf: &mut impl Buf) -> Result<Self, ChunkReadingError> {
        let mut map = HashMap::new();
        loop {
            if !buf.has_remaining() {
                return Err(ChunkReadingError::IoError(std::io::Error::from(
                    ErrorKind::UnexpectedEof,
                )));
            }
            let key_len = buf.get_u8();
            if key_len == 0 {
                break;
            }
            if buf.remaining() < key_len as usize + 4 {
                return Err(ChunkReadingError::IoError(std::io::Error::from(
                    ErrorKind::UnexpectedEof,
                )));
            }
            let key_bytes: Vec<u8> = (0..key_len).map(|_| buf.get_u8()).collect();
            let key = String::from_utf8(key_bytes).map_err(|_| ChunkReadingError::InvalidHeader)?;
            let value = buf.get_u32();
            map.insert(key, value);
        }
        Ok(Self(map))
    }

    fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        for (key, value) in &self.0 {
            let key_bytes = key.as_bytes();
            out.push(key_bytes.len() as u8);
            out.extend_from_slice(key_bytes);
            out.extend_from_slice(&value.to_be_bytes());
        }
        out.push(0); // end marker
        out
    }
}

// ---------------------------------------------------------------------------
// Bucket size entry  (13 bytes per bucket)
// ---------------------------------------------------------------------------
//
//  0..4    uint32   Bucket size (compressed)
//  4..5    int8     Compression level
//  5..13   uint64   xxhash64 of compressed bucket data

struct BucketSizeEntry {
    size: u32,
    compression_level: i8,
    xxhash: u64,
}

impl BucketSizeEntry {
    const SIZE: usize = 13;

    fn from_bytes(buf: &mut impl Buf) -> Result<Self, ChunkReadingError> {
        if buf.remaining() < Self::SIZE {
            return Err(ChunkReadingError::IoError(std::io::Error::from(
                ErrorKind::UnexpectedEof,
            )));
        }
        Ok(Self {
            size: buf.get_u32(),
            compression_level: buf.get_i8(),
            xxhash: buf.get_u64(),
        })
    }

    fn to_bytes(&self) -> [u8; Self::SIZE] {
        let mut out = [0u8; Self::SIZE];
        let mut b = out.as_mut_slice();
        b.put_u32(self.size);
        b.put_i8(self.compression_level);
        b.put_u64(self.xxhash);
        out
    }
}

// ---------------------------------------------------------------------------
// Per-chunk record inside a decompressed bucket
// ---------------------------------------------------------------------------
//
//  0.. 4   uint32   Chunk size (0 = absent)
//  4..12   uint64   Chunk timestamp
// 12..     bytes    Chunk NBT data  (only when size > 0)

struct BucketChunkEntry {
    timestamp: u64,
    data: Option<Bytes>,
}

impl BucketChunkEntry {
    /// Number of bytes for the fixed part (size + timestamp).
    const FIXED_SIZE: usize = 12;

    fn write_into(&self, out: &mut Vec<u8>) {
        match &self.data {
            None => {
                out.put_u32(0);
                out.put_u64(self.timestamp);
            }
            Some(bytes) => {
                out.put_u32(bytes.len() as u32);
                out.put_u64(self.timestamp);
                out.extend_from_slice(bytes);
            }
        }
    }

    fn read_from(buf: &mut Bytes) -> Result<Self, ChunkReadingError> {
        if buf.remaining() < Self::FIXED_SIZE {
            return Err(ChunkReadingError::IoError(std::io::Error::from(
                ErrorKind::UnexpectedEof,
            )));
        }
        let size = buf.get_u32() as usize;
        let timestamp = buf.get_u64();
        if size == 0 {
            return Ok(Self {
                timestamp,
                data: None,
            });
        }
        if buf.remaining() < size {
            warn!(
                "Linear v2: not enough bytes for chunk (need {size}, have {})",
                buf.remaining()
            );
            return Err(ChunkReadingError::IoError(std::io::Error::from(
                ErrorKind::UnexpectedEof,
            )));
        }
        let data = buf.split_to(size);
        Ok(Self {
            timestamp,
            data: Some(data),
        })
    }
}

// ---------------------------------------------------------------------------
// Main LinearV2File struct
// ---------------------------------------------------------------------------

pub struct LinearV2File<S: SingleChunkDataSerializer> {
    /// Region coordinates, needed to rebuild the superblock on write.
    region_x: i32,
    region_z: i32,

    /// Grid size (1, 2, 4, 8, 16, or 32).
    grid_size: u8,

    /// Per-chunk timestamps.
    timestamps: [u64; CHUNK_COUNT],

    /// Per-chunk compressed NBT data (None = chunk absent).
    chunks_data: [Option<Bytes>; CHUNK_COUNT],

    _dummy: PhantomData<S>,
}

#[expect(clippy::large_stack_arrays)]
impl<S: SingleChunkDataSerializer> Default for LinearV2File<S> {
    fn default() -> Self {
        Self {
            region_x: 0,
            region_z: 0,
            grid_size: DEFAULT_GRID_SIZE,
            timestamps: [0u64; CHUNK_COUNT],
            chunks_data: [const { None }; CHUNK_COUNT],
            _dummy: PhantomData,
        }
    }
}

impl<S: SingleChunkDataSerializer> LinearV2File<S> {
    const fn get_chunk_index(x: i32, z: i32) -> usize {
        AnvilChunkFile::<S>::get_chunk_index(x, z)
    }

    /// Number of buckets in the grid (`grid_size²`).
    const fn bucket_count(grid_size: u8) -> usize {
        (grid_size as usize) * (grid_size as usize)
    }

    /// Number of chunks in each bucket.
    const fn chunks_per_bucket(grid_size: u8) -> usize {
        CHUNK_COUNT / Self::bucket_count(grid_size)
    }

    /// Build the decompressed byte stream for bucket `bucket_idx`.
    fn serialise_bucket(
        chunks_data: &[Option<Bytes>; CHUNK_COUNT],
        timestamps: &[u64; CHUNK_COUNT],
        bucket_idx: usize,
        grid_size: u8,
    ) -> Vec<u8> {
        let cpb = Self::chunks_per_bucket(grid_size);
        let mut buf = Vec::new();

        for local in 0..cpb {
            // Recover the global chunk index from bucket + local.
            let chunk_index = Self::global_chunk_index(bucket_idx, local, grid_size);
            let entry = BucketChunkEntry {
                timestamp: timestamps[chunk_index],
                data: chunks_data[chunk_index].clone(),
            };
            entry.write_into(&mut buf);
        }
        buf
    }

    /// Inverse of `chunk_bucket_index` + `chunk_local_index`.
    const fn global_chunk_index(bucket_idx: usize, local: usize, grid_size: u8) -> usize {
        let stride = 32 / grid_size as usize;
        let bucket_row = bucket_idx / grid_size as usize;
        let bucket_col = bucket_idx % grid_size as usize;
        let local_row = local / stride;
        let local_col = local % stride;
        let cz = bucket_row * stride + local_row;
        let cx = bucket_col * stride + local_col;
        cz * 32 + cx
    }

    fn build_bitmap(&self) -> ChunkBitmap {
        let mut bitmap = ChunkBitmap::new();
        for (i, chunk) in self.chunks_data.iter().enumerate() {
            if chunk.is_some() {
                bitmap.set(i, true);
            }
        }
        bitmap
    }

    fn decompress_bucket(
        compressed: &[u8],
        max_decompressed_bytes: usize,
    ) -> Result<Vec<u8>, ChunkReadingError> {
        let output_limit = decompression_output_limit(compressed.len(), max_decompressed_bytes)
            .map_err(ChunkReadingError::IoError)?;
        let decoder =
            StreamingDecoder::new(compressed).map_err(|_| ChunkReadingError::RegionIsInvalid)?;
        let mut decompressed = Vec::new();
        decoder
            .take(output_limit.saturating_add(1) as u64)
            .read_to_end(&mut decompressed)
            .map_err(ChunkReadingError::IoError)?;
        if decompressed.len() > output_limit {
            return Err(ChunkReadingError::RegionIsInvalid);
        }
        Ok(decompressed)
    }
}

impl<S: SingleChunkDataSerializer + 'static> ChunkSerializer for LinearV2File<S> {
    const PEAK_REGION_WORKING_BYTES: usize = 2 * MAX_DECOMPRESSED_BUCKET_BYTES;

    type Data = S;
    type WriteBackend = PathBuf;
    type ChunkConfig = ();

    fn should_write(&self, is_watched: bool) -> bool {
        !is_watched
    }

    fn get_chunk_key(chunk: &Vector2<i32>) -> String {
        let (region_x, region_z) = AnvilChunkFile::<S>::get_region_coords(chunk);
        format!("./r.{region_x}.{region_z}.linear")
    }

    async fn write(&self, path: &PathBuf) -> Result<(), std::io::Error> {
        let file = AsyncAtomicFile::create(path.clone()).await?;
        let mut writer = BufWriter::new(file);

        let grid_size = self.grid_size;
        let bucket_count = Self::bucket_count(grid_size);
        let newest_timestamp = self.timestamps.iter().copied().max().unwrap_or(0);

        let superblock = LinearV2Superblock {
            newest_timestamp,
            grid_size,
            region_x: self.region_x,
            region_z: self.region_z,
        };

        let bitmap = self.build_bitmap();

        let features = NbtFeatures::empty();

        writer.write_all(&superblock.to_bytes()).await?;
        writer.write_all(&bitmap.0).await?;
        writer.write_all(&features.to_bytes()).await?;

        let bucket_table_position = writer.stream_position().await?;
        writer
            .write_all(&vec![0; BucketSizeEntry::SIZE * bucket_count])
            .await?;

        let mut bucket_entries = Vec::with_capacity(bucket_count);
        for bucket_idx in 0..bucket_count {
            let raw =
                Self::serialise_bucket(&self.chunks_data, &self.timestamps, bucket_idx, grid_size);
            if raw.len() > MAX_DECOMPRESSED_BUCKET_BYTES {
                return Err(std::io::Error::new(
                    ErrorKind::InvalidData,
                    format!(
                        "Linear bucket {bucket_idx} exceeds {MAX_DECOMPRESSED_BUCKET_BYTES} decompressed bytes"
                    ),
                ));
            }
            let compress_started = Instant::now();
            let compressed = tokio::task::spawn_blocking(move || {
                compress_to_vec(raw.as_slice(), CompressionLevel::Fastest)
            })
            .await
            .map_err(std::io::Error::other)?;
            serialization_metrics::record_duration(
                SerializationStage::Compress,
                compress_started.elapsed(),
            );
            serialization_metrics::record_compressed_bytes(compressed.len());
            bucket_entries.push(BucketSizeEntry {
                size: compressed.len() as u32,
                compression_level: 1,
                xxhash: xxh64(&compressed, 0),
            });
            writer.write_all(&compressed).await?;
        }

        writer.write_all(&SIGNATURE).await?;
        writer.flush().await?;

        writer.seek(SeekFrom::Start(bucket_table_position)).await?;
        for entry in &bucket_entries {
            writer.write_all(&entry.to_bytes()).await?;
        }
        writer.flush().await?;
        writer.into_inner().commit().await
    }

    #[expect(clippy::large_stack_arrays)]
    fn read(raw_file: Bytes) -> Result<Self, ChunkReadingError> {
        let mut buf = raw_file;

        if buf.remaining() < LinearV2Superblock::SIZE {
            return Err(ChunkReadingError::IoError(std::io::Error::from(
                ErrorKind::UnexpectedEof,
            )));
        }
        let superblock_bytes = buf.split_to(LinearV2Superblock::SIZE);
        let superblock = LinearV2Superblock::from_bytes(&superblock_bytes)?;
        let grid_size = superblock.grid_size;
        let bucket_count = Self::bucket_count(grid_size);

        if buf.remaining() < ChunkBitmap::SIZE {
            return Err(ChunkReadingError::IoError(std::io::Error::from(
                ErrorKind::UnexpectedEof,
            )));
        }
        // We read the bitmap but per the spec it is not yet reliable, so we
        // do not use it to short-circuit — actual presence is determined by
        // the per-chunk size field inside each bucket.
        let mut bitmap_raw = [0u8; ChunkBitmap::SIZE];
        bitmap_raw.copy_from_slice(&buf.split_to(ChunkBitmap::SIZE));
        //let _bitmap = ChunkBitmap(bitmap_raw);

        let _features = NbtFeatures::from_bytes(&mut buf)?;

        let total_bucket_meta = bucket_count * BucketSizeEntry::SIZE;
        if buf.remaining() < total_bucket_meta {
            return Err(ChunkReadingError::IoError(std::io::Error::from(
                ErrorKind::UnexpectedEof,
            )));
        }
        let mut bucket_entries: Vec<BucketSizeEntry> = Vec::with_capacity(bucket_count);
        for _ in 0..bucket_count {
            bucket_entries.push(BucketSizeEntry::from_bytes(&mut buf)?);
        }

        // Work out where the footer starts so we can validate it without
        // consuming the bucket data bytes yet.
        let total_compressed: usize = bucket_entries.iter().map(|e| e.size as usize).sum();
        if buf.remaining() < total_compressed + SIGNATURE.len() {
            return Err(ChunkReadingError::IoError(std::io::Error::from(
                ErrorKind::UnexpectedEof,
            )));
        }
        let footer_offset = total_compressed;
        {
            let footer = &buf[footer_offset..footer_offset + SIGNATURE.len()];
            if footer != SIGNATURE {
                error!("Linear v2: invalid footer signature");
                return Err(ChunkReadingError::InvalidHeader);
            }
        }

        let mut timestamps = [0u64; CHUNK_COUNT];
        let mut chunks_data: [Option<Bytes>; CHUNK_COUNT] = [const { None }; CHUNK_COUNT];

        for (bucket_idx, entry) in bucket_entries.iter().enumerate() {
            let compressed_size = entry.size as usize;

            // xxhash64 integrity check.
            let actual_hash = xxh64(&buf[..compressed_size], 0);
            if actual_hash != entry.xxhash {
                error!(
                    "Linear v2: xxhash mismatch for bucket {bucket_idx} \
                     (expected {:#x}, got {:#x})",
                    entry.xxhash, actual_hash
                );
                // Skip the corrupted bucket rather than aborting the whole
                // file — the remaining buckets may be intact.
                buf.advance(compressed_size);
                continue;
            }

            let compressed_slice = &buf[..compressed_size];
            let decompressed =
                Self::decompress_bucket(compressed_slice, MAX_DECOMPRESSED_BUCKET_BYTES)?;
            buf.advance(compressed_size);

            let mut bucket_buf: Bytes = decompressed.into();
            let cpb = Self::chunks_per_bucket(grid_size);

            for local in 0..cpb {
                let chunk_index = Self::global_chunk_index(bucket_idx, local, grid_size);
                let record = BucketChunkEntry::read_from(&mut bucket_buf)?;
                timestamps[chunk_index] = record.timestamp;
                chunks_data[chunk_index] = record.data;
            }
        }

        Ok(Self {
            region_x: superblock.region_x,
            region_z: superblock.region_z,
            grid_size,
            timestamps,
            chunks_data,
            _dummy: PhantomData,
        })
    }

    async fn update_chunk(
        &mut self,
        chunk: &Self::Data,
        _chunk_config: &Self::ChunkConfig,
    ) -> Result<(), ChunkWritingError> {
        let index = Self::get_chunk_index(chunk.position().0, chunk.position().1);
        let chunk_raw: Bytes = chunk
            .to_bytes()
            .await
            .map_err(|err| ChunkWritingError::ChunkSerializingError(err.to_string()))?;
        serialization_metrics::record_snapshot_bytes(chunk_raw.len());

        self.timestamps[index] = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        self.chunks_data[index] = Some(chunk_raw);
        Ok(())
    }

    async fn get_chunks(
        &self,
        chunks: Vec<Vector2<i32>>,
        stream: tokio::sync::mpsc::Sender<LoadedData<Self::Data, ChunkReadingError>>,
    ) {
        for chunk in chunks {
            let index = Self::get_chunk_index(chunk.x, chunk.y);

            let is_ok = match &self.chunks_data[index] {
                None => stream.send(LoadedData::Missing(chunk)).await.is_ok(),
                Some(data) => {
                    let data = data.clone();
                    let result = match tokio::task::spawn_blocking(move || {
                        S::from_bytes(&data, chunk)
                    })
                    .await
                    {
                        Ok(Ok(c)) => LoadedData::Loaded(c),
                        Ok(Err(err)) => LoadedData::Error((chunk, err)),
                        Err(err) => LoadedData::Error((
                            chunk,
                            ChunkReadingError::IoError(std::io::Error::other(err)),
                        )),
                    };
                    stream.send(result).await.is_ok()
                }
            };

            if !is_ok {
                // Receiver dropped — stop early to avoid unnecessary work.
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::future::Future;

    use tempfile::tempdir;

    use super::*;
    use crate::chunk::ChunkSerializingError;
    use crate::chunk::io::Dirtiable;

    struct MockChunk;

    impl Dirtiable for MockChunk {
        fn is_dirty(&self) -> bool {
            false
        }

        fn mark_dirty(&self, _dirty: bool) {}
    }

    impl SingleChunkDataSerializer for MockChunk {
        fn to_bytes(
            &self,
        ) -> std::pin::Pin<Box<dyn Future<Output = Result<Bytes, ChunkSerializingError>> + Send + '_>>
        {
            Box::pin(async { unreachable!() })
        }

        fn from_bytes(_bytes: &Bytes, _pos: Vector2<i32>) -> Result<Self, ChunkReadingError> {
            Ok(Self)
        }

        fn position(&self) -> (i32, i32) {
            (0, 0)
        }
    }

    // Helper: build a tiny chunk payload.
    fn fake_nbt(seed: u8) -> Bytes {
        Bytes::from(vec![seed; 64])
    }

    #[test]
    fn bitmap_round_trip() {
        let mut bm = ChunkBitmap::new();
        bm.set(0, true);
        bm.set(7, true);
        bm.set(1023, true);
        assert!(bm.get(0));
        assert!(!bm.get(1));
        assert!(bm.get(7));
        assert!(!bm.get(8));
        assert!(bm.get(1023));
        assert!(!bm.get(1022));
    }

    #[test]
    fn nbt_features_empty_round_trip() {
        let f = NbtFeatures::empty();
        let encoded = f.to_bytes();
        assert_eq!(encoded, vec![0u8]); // just the end marker

        let mut buf: Bytes = encoded.into();
        let decoded = NbtFeatures::from_bytes(&mut buf).unwrap();
        assert!(decoded.0.is_empty());
    }

    #[test]
    fn nbt_features_round_trip() {
        let mut f = NbtFeatures::empty();
        f.0.insert("version".into(), 42);
        let encoded = f.to_bytes();
        let mut buf: Bytes = encoded.into();
        let decoded = NbtFeatures::from_bytes(&mut buf).unwrap();
        assert_eq!(decoded.0.get("version"), Some(&42u32));
    }

    fn chunk_bucket_index(i: usize, gs: u8) -> usize {
        let cx = i % 32;
        let cz = i / 32;
        let stride = 32 / gs as usize;
        (cz / stride) * gs as usize + (cx / stride)
    }

    fn chunk_local_index(i: usize, gs: u8) -> usize {
        let cx = i % 32;
        let cz = i / 32;
        let stride = 32 / gs as usize;
        (cz % stride) * stride + (cx % stride)
    }

    fn global_chunk_index(bucket: usize, local: usize, gs: u8) -> usize {
        let stride = 32 / gs as usize;
        let brow = bucket / gs as usize;
        let bcol = bucket % gs as usize;
        let lrow = local / stride;
        let lcol = local % stride;
        let cz = brow * stride + lrow;
        let cx = bcol * stride + lcol;
        cz * 32 + cx
    }

    #[test]
    fn index_round_trip_grid2() {
        for i in 0..CHUNK_COUNT {
            let b = chunk_bucket_index(i, 2);
            let l = chunk_local_index(i, 2);
            let recovered = global_chunk_index(b, l, 2);
            assert_eq!(i, recovered, "chunk index {i} round-trip failed");
        }
    }

    #[test]
    fn index_round_trip_grid4() {
        for i in 0..CHUNK_COUNT {
            let b = chunk_bucket_index(i, 4);
            let l = chunk_local_index(i, 4);
            let recovered = global_chunk_index(b, l, 4);
            assert_eq!(i, recovered, "chunk index {i} round-trip failed");
        }
    }

    #[test]
    fn index_round_trip_grid1() {
        for i in 0..CHUNK_COUNT {
            let b = chunk_bucket_index(i, 1);
            let l = chunk_local_index(i, 1);
            let recovered = global_chunk_index(b, l, 1);
            assert_eq!(i, recovered, "chunk index {i} round-trip failed");
        }
    }

    #[test]
    fn superblock_round_trip() {
        let sb = LinearV2Superblock {
            newest_timestamp: 0xDEADBEEFCAFEBABE,
            grid_size: 4,
            region_x: -7,
            region_z: 13,
        };
        let encoded = sb.to_bytes();
        let decoded = LinearV2Superblock::from_bytes(&encoded).unwrap();
        assert_eq!(decoded.newest_timestamp, sb.newest_timestamp);
        assert_eq!(decoded.grid_size, 4);
        assert_eq!(decoded.region_x, -7);
        assert_eq!(decoded.region_z, 13);
    }

    #[test]
    fn superblock_rejects_bad_signature() {
        let mut bytes = LinearV2Superblock {
            newest_timestamp: 0,
            grid_size: 2,
            region_x: 0,
            region_z: 0,
        }
        .to_bytes();
        bytes[0] ^= 0xFF; // corrupt the signature
        assert!(LinearV2Superblock::from_bytes(&bytes).is_err());
    }

    #[test]
    fn bucket_entry_present_round_trip() {
        let entry = BucketChunkEntry {
            timestamp: 12345,
            data: Some(fake_nbt(0xAB)),
        };
        let mut buf = Vec::new();
        entry.write_into(&mut buf);
        let mut b: Bytes = buf.into();
        let decoded = BucketChunkEntry::read_from(&mut b).unwrap();
        assert_eq!(decoded.timestamp, 12345);
        assert_eq!(decoded.data.unwrap().as_ref(), &[0xABu8; 64][..]);
    }

    #[test]
    fn bucket_entry_absent_round_trip() {
        let entry = BucketChunkEntry {
            timestamp: 0,
            data: None,
        };
        let mut buf = Vec::new();
        entry.write_into(&mut buf);
        let mut b: Bytes = buf.into();
        let decoded = BucketChunkEntry::read_from(&mut b).unwrap();
        assert!(decoded.data.is_none());
    }

    #[test]
    fn decompressed_bucket_limit_rejects_overflow() {
        let compressed = compress_to_vec(&[7u8; 129][..], CompressionLevel::Fastest);
        assert!(LinearV2File::<MockChunk>::decompress_bucket(&compressed, 128).is_err());
    }

    #[test]
    fn decompressed_bucket_rejects_hostile_compression_ratio() {
        let raw = vec![0; 1024 * 1024];
        let compressed = compress_to_vec(&raw[..], CompressionLevel::Fastest);
        assert!(LinearV2File::<MockChunk>::decompress_bucket(&compressed, raw.len()).is_err());
    }

    #[test]
    fn decompressed_bucket_accepts_legitimate_payload_at_test_limit() {
        let mut state = 0x1234_5678_u32;
        let raw = (0..1024 * 1024)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                state as u8
            })
            .collect::<Vec<_>>();
        let compressed = compress_to_vec(&raw[..], CompressionLevel::Fastest);
        assert_eq!(
            LinearV2File::<MockChunk>::decompress_bucket(&compressed, raw.len()).unwrap(),
            raw
        );
    }

    #[tokio::test]
    async fn streamed_bucket_write_round_trips_header_and_payload() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("r.0.0.linear");
        let mut region = LinearV2File::<MockChunk>::default();
        region.grid_size = 4;
        region.timestamps[0] = 42;
        region.chunks_data[0] = Some(fake_nbt(0xAB));

        region.write(&path).await.unwrap();

        let raw = tokio::fs::read(path).await.unwrap();
        let decoded = LinearV2File::<MockChunk>::read(Bytes::from(raw)).unwrap();
        assert_eq!(decoded.timestamps[0], 42);
        assert_eq!(decoded.chunks_data[0].as_deref(), Some(&[0xAB; 64][..]));
    }

    #[tokio::test]
    async fn repeated_atomic_replacement_keeps_latest_region_valid() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("r.0.0.linear");
        let mut region = LinearV2File::<MockChunk>::default();
        region.grid_size = 4;

        for generation in 1..=32u8 {
            region.timestamps[0] = u64::from(generation);
            region.chunks_data[0] = Some(fake_nbt(generation));
            region.write(&path).await.unwrap();

            let raw = tokio::fs::read(&path).await.unwrap();
            region = LinearV2File::<MockChunk>::read(Bytes::from(raw)).unwrap();
            assert_eq!(region.timestamps[0], u64::from(generation));
            assert_eq!(
                region.chunks_data[0].as_deref(),
                Some(vec![generation; 64].as_slice())
            );
            assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
        }
    }

    #[tokio::test]
    async fn corrupted_bucket_is_rejected_without_losing_other_buckets() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("r.0.0.linear");
        let mut region = LinearV2File::<MockChunk>::default();
        region.grid_size = 4;
        region.timestamps[0] = 11;
        region.chunks_data[0] = Some(fake_nbt(0x11));
        region.timestamps[16] = 22;
        region.chunks_data[16] = Some(fake_nbt(0x22));
        region.write(&path).await.unwrap();

        let mut raw = tokio::fs::read(&path).await.unwrap();
        let bucket_data_offset = LinearV2Superblock::SIZE
            + ChunkBitmap::SIZE
            + NbtFeatures::empty().to_bytes().len()
            + BucketSizeEntry::SIZE * LinearV2File::<MockChunk>::bucket_count(4);
        raw[bucket_data_offset] ^= 0xFF;

        let decoded = LinearV2File::<MockChunk>::read(Bytes::from(raw)).unwrap();
        assert!(decoded.chunks_data[0].is_none());
        assert_eq!(decoded.timestamps[16], 22);
        assert_eq!(decoded.chunks_data[16].as_deref(), Some(&[0x22; 64][..]));
    }
}
