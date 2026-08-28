use std::{collections::HashSet, sync::Arc, time::Instant};

use criterion::{Criterion, criterion_group, criterion_main};
use futures::{StreamExt, stream};
use pumpkin::block::entities::{
    BlockEntity, campfire::CampfireBlockEntity, structure_block::StructureBlockBlockEntity,
};
use pumpkin_protocol::bedrock::client::level_chunk::CLevelChunk;
use pumpkin_util::math::position::BlockPos;
use pumpkin_world::chunk::ChunkData;

fn fixtures(count: usize) -> Vec<Arc<dyn BlockEntity>> {
    (0..count)
        .map(|index| {
            let position = BlockPos::new(index as i32, 64, -(index as i32));
            if index % 2 == 0 {
                Arc::new(CampfireBlockEntity::new(position)) as Arc<dyn BlockEntity>
            } else {
                Arc::new(StructureBlockBlockEntity::new(position)) as Arc<dyn BlockEntity>
            }
        })
        .collect()
}

async fn capture_sequential(block_entities: &[Arc<dyn BlockEntity>]) {
    for block_entity in block_entities {
        let snapshot = block_entity.capture_save_snapshot().await;
        std::hint::black_box(snapshot.into_nbt(None));
    }
}

async fn capture_bounded(block_entities: &[Arc<dyn BlockEntity>]) {
    stream::iter(block_entities)
        .map(|block_entity| async move {
            let snapshot = block_entity.capture_save_snapshot().await;
            std::hint::black_box(snapshot.into_nbt(None));
        })
        .buffer_unordered(4)
        .collect::<Vec<_>>()
        .await;
}

#[cfg(target_os = "linux")]
fn peak_rss_bytes() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let kib = status
        .lines()
        .find_map(|line| line.strip_prefix("VmHWM:"))?
        .split_whitespace()
        .next()?
        .parse::<u64>()
        .ok()?;
    Some(kib.saturating_mul(1024))
}

#[cfg(not(target_os = "linux"))]
const fn peak_rss_bytes() -> Option<u64> {
    None
}

fn stable_hash(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x1000_0000_01b3)
    })
}

#[expect(
    clippy::expect_used,
    reason = "a benchmark fixture must fail immediately when protocol encoding is invalid"
)]
fn encode_bedrock_chunks(chunks: &[Arc<ChunkData>], cache_enabled: bool) -> (usize, usize, u64) {
    let mut cached_hashes = HashSet::new();
    let mut retained_cache_bytes = 0usize;
    let mut packet_bytes = 0usize;
    let mut output_hash = 0u64;

    for chunk in chunks {
        let (packet, blobs) = CLevelChunk::encode_chunk(chunk, 0, cache_enabled, &[])
            .expect("Bedrock chunk fixture should encode");
        packet_bytes = packet_bytes.saturating_add(packet.len());
        output_hash ^= stable_hash(&packet).rotate_left((chunk.x & 63) as u32);
        for (hash, blob) in blobs {
            if cached_hashes.insert(hash) {
                retained_cache_bytes = retained_cache_bytes.saturating_add(blob.len());
            }
        }
    }

    (packet_bytes, retained_cache_bytes, output_hash)
}

#[expect(
    clippy::expect_used,
    reason = "failure to construct the benchmark runtime makes the benchmark unusable"
)]
fn bench_block_entity_snapshot(c: &mut Criterion) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("benchmark runtime should build");
    let mut group = c.benchmark_group("block_entity_snapshot");
    group.sample_size(20);

    for count in [32, 256] {
        let block_entities = fixtures(count);
        group.bench_function(format!("sequential_{count}"), |b| {
            b.iter(|| runtime.block_on(capture_sequential(&block_entities)));
        });
        group.bench_function(format!("bounded4_{count}"), |b| {
            b.iter(|| runtime.block_on(capture_bounded(&block_entities)));
        });
    }
    group.finish();
}

#[expect(
    clippy::print_stderr,
    reason = "CI captures this replayable acceptance matrix as a benchmark artifact"
)]
fn bench_bedrock_chunk_matrix(c: &mut Criterion) {
    let mut group = c.benchmark_group("bedrock_chunk_matrix");
    group.sample_size(10);

    for count in [32usize, 128, 512] {
        let chunks = (0..count)
            .map(|index| ChunkData::empty_sync(index as i32, -(index as i32)))
            .collect::<Vec<_>>();
        for cache_enabled in [false, true] {
            let started = Instant::now();
            let first = encode_bedrock_chunks(&chunks, cache_enabled);
            let elapsed = started.elapsed();
            assert_eq!(first, encode_bedrock_chunks(&chunks, cache_enabled));
            eprintln!(
                "SER-033 bedrock-chunk: chunks={count} cache_enabled={cache_enabled} packet_bytes={} retained_cache_bytes={} hash={:016x} elapsed_ms={:.3} peak_rss_bytes={}",
                first.0,
                first.1,
                first.2,
                elapsed.as_secs_f64() * 1_000.0,
                peak_rss_bytes().map_or_else(|| "unsupported".to_string(), |rss| rss.to_string()),
            );

            group.bench_function(format!("chunks_{count}_cache_{cache_enabled}"), |bencher| {
                bencher
                    .iter(|| std::hint::black_box(encode_bedrock_chunks(&chunks, cache_enabled)));
            });
        }
    }
    group.finish();
}

criterion_group!(
    benches,
    bench_block_entity_snapshot,
    bench_bedrock_chunk_matrix
);
criterion_main!(benches);
