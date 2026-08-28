use std::collections::BTreeMap;
use std::marker::PhantomData;
use std::path::PathBuf;
use std::time::Instant;

use crate::chunk::format::anvil::SingleChunkDataSerializer;
use crate::chunk::io::{ChunkSerializer, LoadedData, decompression_output_limit};
use crate::chunk::{ChunkReadingError, ChunkWritingError};
use bytes::Bytes;
use pumpkin_util::math::vector2::Vector2;
use ruzstd::decoding::StreamingDecoder;
use ruzstd::encoding::{CompressionLevel, compress_to_vec};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncWrite, AsyncWriteExt};

use crate::{
    persistence::AsyncAtomicFile,
    serialization_metrics::{self, SerializationStage},
};

const MAX_DECOMPRESSED_CHUNK_BYTES: usize = 64 * 1024 * 1024;

fn decompress_chunk_with_limit(
    compressed: &[u8],
    max_decompressed_bytes: usize,
) -> Result<Bytes, ChunkReadingError> {
    let output_limit = decompression_output_limit(compressed.len(), max_decompressed_bytes)
        .map_err(ChunkReadingError::IoError)?;
    let decoder = StreamingDecoder::new(compressed)
        .map_err(|error| ChunkReadingError::IoError(std::io::Error::other(error.to_string())))?;
    let mut limited = std::io::Read::take(decoder, output_limit.saturating_add(1) as u64);
    let mut decompressed = Vec::new();
    std::io::Read::read_to_end(&mut limited, &mut decompressed)
        .map_err(ChunkReadingError::IoError)?;
    if decompressed.len() > output_limit {
        return Err(ChunkReadingError::RegionIsInvalid);
    }
    Ok(Bytes::from(decompressed))
}

async fn write_nbt_tag_header(
    writer: &mut (impl AsyncWrite + Unpin),
    tag_id: u8,
    name: &str,
) -> Result<(), std::io::Error> {
    let name_len = u16::try_from(name.len())
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidData, "NBT name too long"))?;
    writer.write_u8(tag_id).await?;
    writer.write_u16(name_len).await?;
    writer.write_all(name.as_bytes()).await
}

pub struct PumpFile<D> {
    pub data: PumpData,
    _phantom: PhantomData<D>,
}

#[derive(Serialize, Deserialize, Default)]
pub struct PumpData {
    pub x: i32,
    pub z: i32,
    pub chunks: BTreeMap<String, Vec<u8>>,
}

impl<D> Default for PumpFile<D> {
    fn default() -> Self {
        Self {
            data: PumpData::default(),
            _phantom: PhantomData,
        }
    }
}

impl<D> ChunkSerializer for PumpFile<D>
where
    D: SingleChunkDataSerializer + Send + Sync + Sized + 'static,
{
    const READ_FILE_MEMORY_MULTIPLIER: usize = 3;

    type Data = D;
    type WriteBackend = PathBuf;
    type ChunkConfig = ();

    fn get_chunk_key(chunk: &Vector2<i32>) -> String {
        let region_x = chunk.x >> 5;
        let region_z = chunk.y >> 5;
        format!("r.{region_x}.{region_z}.pump")
    }

    fn should_write(&self, _is_watched: bool) -> bool {
        true
    }

    async fn write(&self, backend: &Self::WriteBackend) -> Result<(), std::io::Error> {
        const TAG_END: u8 = 0;
        const TAG_INT: u8 = 3;
        const TAG_BYTE_ARRAY: u8 = 7;
        const TAG_COMPOUND: u8 = 10;

        let mut file = AsyncAtomicFile::create(backend.clone()).await?;
        file.write_u8(TAG_COMPOUND).await?;

        write_nbt_tag_header(&mut file, TAG_INT, "x").await?;
        file.write_i32(self.data.x).await?;
        write_nbt_tag_header(&mut file, TAG_INT, "z").await?;
        file.write_i32(self.data.z).await?;
        write_nbt_tag_header(&mut file, TAG_COMPOUND, "chunks").await?;
        for (key, payload) in &self.data.chunks {
            let payload_len = i32::try_from(payload.len()).map_err(|_| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "Pump chunk payload too large",
                )
            })?;
            write_nbt_tag_header(&mut file, TAG_BYTE_ARRAY, key).await?;
            file.write_i32(payload_len).await?;
            file.write_all(payload).await?;
        }
        file.write_u8(TAG_END).await?;
        file.write_u8(TAG_END).await?;
        file.commit().await
    }

    fn read(r: Bytes) -> Result<Self, ChunkReadingError> {
        let mut cursor = std::io::Cursor::new(r);
        let mut reader = pumpkin_nbt::deserializer::NbtReadHelperJava::new(
            pumpkin_nbt::deserializer::NbtStreamReader(&mut cursor),
        );
        let nbt = pumpkin_nbt::Nbt::read_unnamed(&mut reader).map_err(|e| {
            ChunkReadingError::ParsingError(
                crate::chunk::ChunkParsingError::ErrorDeserializingChunk(e.to_string()),
            )
        })?;

        let x = nbt.get_int("x").unwrap_or(0);
        let z = nbt.get_int("z").unwrap_or(0);
        let mut chunks = BTreeMap::new();
        if let Some(chunks_tag) = nbt.get_compound("chunks") {
            for (k, v) in &chunks_tag.child_tags {
                if let pumpkin_nbt::tag::NbtTag::ByteArray(arr) = v {
                    let u8_vec: Vec<u8> = arr.iter().map(|&b| b as u8).collect();
                    chunks.insert(k.to_string(), u8_vec);
                }
            }
        }

        Ok(Self {
            data: PumpData { x, z, chunks },
            _phantom: PhantomData,
        })
    }

    async fn update_chunk(
        &mut self,
        chunk_data: &Self::Data,
        _chunk_config: &Self::ChunkConfig,
    ) -> Result<(), ChunkWritingError> {
        let (x, z) = chunk_data.position();
        self.data.x = x >> 5;
        self.data.z = z >> 5;
        let rel_x = x.rem_euclid(32);
        let rel_z = z.rem_euclid(32);
        let index = (rel_x + rel_z * 32) as usize;

        let bytes = chunk_data
            .to_bytes()
            .await
            .map_err(|e| ChunkWritingError::ChunkSerializingError(e.to_string()))?;
        if bytes.len() > MAX_DECOMPRESSED_CHUNK_BYTES {
            return Err(ChunkWritingError::ChunkSerializingError(format!(
                "Pump chunk exceeds {MAX_DECOMPRESSED_CHUNK_BYTES} decompressed bytes"
            )));
        }
        serialization_metrics::record_snapshot_bytes(bytes.len());

        let compress_started = Instant::now();
        let compressed = tokio::task::spawn_blocking(move || {
            compress_to_vec(&bytes[..], CompressionLevel::Fastest)
        })
        .await
        .map_err(|error| ChunkWritingError::IoError(std::io::Error::other(error)))?;
        serialization_metrics::record_duration(
            SerializationStage::Compress,
            compress_started.elapsed(),
        );
        serialization_metrics::record_compressed_bytes(compressed.len());

        self.data.chunks.insert(index.to_string(), compressed);

        Ok(())
    }

    async fn get_chunks(
        &self,
        chunks: Vec<Vector2<i32>>,
        stream: tokio::sync::mpsc::Sender<LoadedData<Self::Data, ChunkReadingError>>,
    ) {
        for pos in chunks {
            let rel_x = pos.x.rem_euclid(32);
            let rel_z = pos.y.rem_euclid(32);
            let index = (rel_x + rel_z * 32) as usize;

            if let Some(chunk_bytes) = self.data.chunks.get(&index.to_string()) {
                let chunk_bytes = chunk_bytes.clone();
                let res = tokio::task::spawn_blocking(move || {
                    let bytes =
                        decompress_chunk_with_limit(&chunk_bytes, MAX_DECOMPRESSED_CHUNK_BYTES)?;
                    D::from_bytes(&bytes, pos)
                })
                .await;

                let data_res = match res {
                    Ok(Ok(data)) => LoadedData::Loaded(data),
                    Ok(Err(e)) => LoadedData::Error((pos, e)),
                    Err(e) => LoadedData::Error((
                        pos,
                        ChunkReadingError::IoError(std::io::Error::other(e)),
                    )),
                };

                if stream.send(data_res).await.is_err() {
                    return;
                }
            } else {
                let _ = stream.send(LoadedData::Missing(pos)).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chunk::ChunkReadingError;
    use crate::chunk::ChunkSerializingError;
    use crate::chunk::format::anvil::SingleChunkDataSerializer;
    use crate::chunk::io::Dirtiable;
    use crate::chunk::io::{ChunkSerializer, LoadedData};
    use bytes::Bytes;
    use pumpkin_util::math::vector2::Vector2;
    use serde::{Deserialize, Serialize};
    use std::future::Future;
    use std::pin::Pin;
    use tempfile::TempDir;

    #[derive(Debug, Serialize, Deserialize, Clone)]
    struct MockChunk {
        x: i32,
        z: i32,
        data: Vec<u8>,
    }

    impl Dirtiable for MockChunk {
        fn is_dirty(&self) -> bool {
            true
        }
        fn mark_dirty(&self, _: bool) {}
    }

    impl SingleChunkDataSerializer for MockChunk {
        fn to_bytes(
            &self,
        ) -> Pin<Box<dyn Future<Output = Result<Bytes, ChunkSerializingError>> + Send + '_>>
        {
            let mut root = pumpkin_nbt::compound::NbtCompound::new();
            root.put_int("x", self.x);
            root.put_int("z", self.z);
            let i8_vec: Vec<i8> = self.data.iter().map(|&b| b as i8).collect();
            root.put("data", pumpkin_nbt::tag::NbtTag::ByteArray(i8_vec.into()));
            let bytes = pumpkin_nbt::Nbt::from(root).write_unnamed();
            Box::pin(async move { bytes.map_err(ChunkSerializingError::from) })
        }
        fn from_bytes(bytes: &Bytes, pos: Vector2<i32>) -> Result<Self, ChunkReadingError> {
            let mut cursor = std::io::Cursor::new(bytes);
            let mut reader = pumpkin_nbt::deserializer::NbtReadHelperJava::new(
                pumpkin_nbt::deserializer::NbtStreamReader(&mut cursor),
            );
            let nbt = pumpkin_nbt::Nbt::read_unnamed(&mut reader).map_err(|e| {
                ChunkReadingError::ParsingError(
                    crate::chunk::ChunkParsingError::ErrorDeserializingChunk(e.to_string()),
                )
            })?;
            let data = match nbt.get("data") {
                Some(pumpkin_nbt::tag::NbtTag::ByteArray(arr)) => {
                    arr.iter().map(|&b| b as u8).collect()
                }
                _ => Vec::new(),
            };
            Ok(Self {
                x: pos.x,
                z: pos.y,
                data,
            })
        }
        fn position(&self) -> (i32, i32) {
            (self.x, self.z)
        }
    }

    #[tokio::test]
    async fn pump_file_roundtrip() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("r.0.0.pump");

        let mut pump_file: PumpFile<MockChunk> = PumpFile::default();
        let chunk = MockChunk {
            x: 0,
            z: 0,
            data: vec![1, 2, 3],
        };

        pump_file.update_chunk(&chunk, &()).await.unwrap();
        pump_file.write(&file_path).await.unwrap();

        let bytes = tokio::fs::read(&file_path).await.unwrap();
        let read_file = PumpFile::<MockChunk>::read(Bytes::from(bytes)).unwrap();

        assert_eq!(read_file.data.chunks.len(), 1);
        let (stream_tx, mut stream_rx) = tokio::sync::mpsc::channel(1);
        read_file
            .get_chunks(vec![Vector2::new(0, 0)], stream_tx)
            .await;

        let loaded = stream_rx.recv().await.unwrap();
        match loaded {
            LoadedData::Loaded(c) => {
                assert_eq!(c.data, vec![1, 2, 3]);
            }
            _ => panic!("Expected LoadedData::Loaded"),
        }
    }

    #[test]
    fn pump_chunk_decompression_limit_rejects_overflow() {
        let compressed = compress_to_vec(&[7u8; 129][..], CompressionLevel::Fastest);
        assert!(decompress_chunk_with_limit(&compressed, 128).is_err());
    }

    #[test]
    fn pump_chunk_rejects_hostile_compression_ratio() {
        let raw = vec![0; 1024 * 1024];
        let compressed = compress_to_vec(&raw[..], CompressionLevel::Fastest);
        assert!(decompress_chunk_with_limit(&compressed, raw.len()).is_err());
    }

    #[test]
    fn pump_chunk_accepts_legitimate_payload_at_test_limit() {
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
            decompress_chunk_with_limit(&compressed, raw.len()).unwrap(),
            raw
        );
    }
}
