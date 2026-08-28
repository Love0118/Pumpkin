#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use bytes::Bytes;
use criterion::{Criterion, criterion_group, criterion_main};
use pumpkin_config::chunk::AnvilChunkConfig;
use pumpkin_data::{BlockStateId, dimension::Dimension};
use pumpkin_nbt::{Nbt, compound::NbtCompound, tag::NbtTag};
use pumpkin_util::{math::vector2::Vector2, world_seed::Seed};
use pumpkin_world::{
    chunk::{
        ChunkData, ChunkEntityData,
        format::{
            anvil::{AnvilChunkFile, SingleChunkDataSerializer},
            linear::LinearV2File,
            pump::PumpFile,
        },
        io::{ChunkSerializer, LoadedData},
    },
    chunk_system::{Chunk, StagedChunkEnum, generate_single_chunk},
    generation::get_world_gen,
    serialization_metrics::serialization_metrics_snapshot,
    world::WorldPortalExt,
};
use std::{
    collections::BTreeMap,
    fs,
    hint::black_box,
    path::PathBuf,
    sync::{
        Arc, Barrier,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Instant,
};
use tempfile::tempdir;
use xxhash_rust::xxh64::xxh64;

struct BlockRegistry;

impl WorldPortalExt for BlockRegistry {
    fn can_place_at(
        &self,
        _block: &pumpkin_data::Block,
        _state: &pumpkin_data::BlockState,
        _block_accessor: &dyn pumpkin_world::world::BlockAccessor,
        _block_pos: &pumpkin_util::math::position::BlockPos,
    ) -> bool {
        true
    }

    fn mirror(
        &self,
        block: &pumpkin_data::Block,
        state_id: BlockStateId,
        mirror: pumpkin_data::Mirror,
    ) -> &'static pumpkin_data::BlockState {
        block.mirror(state_id, mirror)
    }

    fn rotate(
        &self,
        block: &pumpkin_data::Block,
        state_id: BlockStateId,
        rotation: pumpkin_data::Rotation,
    ) -> &'static pumpkin_data::BlockState {
        block.rotate(state_id, rotation)
    }

    fn spawn_mobs_for_chunk_generation(
        &self,
        _cache: &mut dyn pumpkin_world::generation::proto_chunk::GenerationCache,
        _biome: &'static pumpkin_data::chunk::Biome,
        _chunk_x: i32,
        _chunk_z: i32,
    ) {
    }
}

fn percentile(samples: &mut [u128], percentile: usize) -> u128 {
    samples.sort_unstable();
    samples[(samples.len() - 1) * percentile / 100]
}

#[cfg(target_os = "linux")]
fn peak_rss_bytes() -> Option<u64> {
    let status = fs::read_to_string("/proc/self/status").ok()?;
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
fn peak_rss_bytes() -> Option<u64> {
    None
}

fn mixed_entity_fixture(count: usize) -> Vec<NbtCompound> {
    (0..count)
        .map(|index| match index % 4 {
            0 => {
                let mut entity = NbtCompound::new();
                entity.put_string("id", "minecraft:pig");
                entity.put_int("Age", index as i32);
                entity
            }
            1 => {
                let mut helmet = NbtCompound::new();
                helmet.put_string("id", "minecraft:iron_helmet");
                let mut sword = NbtCompound::new();
                sword.put_string("id", "minecraft:iron_sword");
                let mut entity = NbtCompound::new();
                entity.put_string("id", "minecraft:zombie");
                entity.put_list("ArmorItems", vec![NbtTag::Compound(helmet)]);
                entity.put_list("HandItems", vec![NbtTag::Compound(sword)]);
                entity
            }
            2 => {
                let mut memories = NbtCompound::new();
                memories.put_long("minecraft:home", index as i64);
                let mut brain = NbtCompound::new();
                brain.put_compound("memories", memories);
                let mut entity = NbtCompound::new();
                entity.put_string("id", "minecraft:villager");
                entity.put_compound("Brain", brain);
                entity
            }
            _ => {
                let mut passenger = NbtCompound::new();
                passenger.put_string("id", "minecraft:chicken");
                let mut entity = NbtCompound::new();
                entity.put_string("id", "minecraft:boat");
                entity.put_list("Passengers", vec![NbtTag::Compound(passenger)]);
                entity
            }
        })
        .collect()
}

fn measure_mixed_entity_restart(runtime: &tokio::runtime::Runtime) {
    const ENTITY_COUNT: usize = 10_000;

    let position = Vector2::new(0, 0);
    let mut root = NbtCompound::new();
    root.put_int("DataVersion", 4903);
    root.put("Position", NbtTag::IntArray(vec![position.x, position.y]));
    root.put_list(
        "Entities",
        mixed_entity_fixture(ENTITY_COUNT)
            .into_iter()
            .map(NbtTag::Compound)
            .collect(),
    );
    let initial_bytes = Bytes::from(Nbt::from(root).write_unnamed().unwrap());
    let chunk = ChunkEntityData::from_bytes(&initial_bytes, position)
        .expect("mixed entity fixture should parse before timing");
    let save_started = Instant::now();
    let bytes = runtime
        .block_on(chunk.to_bytes())
        .expect("mixed entity fixture should serialize");
    let save_elapsed = save_started.elapsed();
    let hash = xxh64(&bytes, 0);

    let load_started = Instant::now();
    let restored = ChunkEntityData::from_bytes(&bytes, position)
        .expect("mixed entity fixture should restart from serialized bytes");
    let restored_entities = runtime.block_on(restored.take_entities());
    let load_elapsed = load_started.elapsed();
    assert_eq!(restored_entities.len(), ENTITY_COUNT);

    eprintln!(
        "SER-033 mixed-entity: count={ENTITY_COUNT} bytes={} hash={hash:016x} save_ms={:.3} load_ms={:.3} peak_rss_bytes={}",
        bytes.len(),
        save_elapsed.as_secs_f64() * 1_000.0,
        load_elapsed.as_secs_f64() * 1_000.0,
        peak_rss_bytes().map_or_else(|| "unsupported".to_string(), |rss| rss.to_string()),
    );
}

async fn measure_format_case<S>(
    label: &str,
    count: usize,
    multi_region: bool,
    config: &S::ChunkConfig,
) where
    S: ChunkSerializer<Data = ChunkData, WriteBackend = PathBuf> + Default,
{
    let directory = tempdir().expect("format benchmark directory should be created");
    let mut regions = BTreeMap::<String, Vec<(Vector2<i32>, Arc<ChunkData>)>>::new();
    for index in 0..count {
        let position = if multi_region {
            let region = index / 32;
            let local = index % 32;
            Vector2::new((region * 32 + local) as i32, 0)
        } else {
            Vector2::new((index % 32) as i32, (index / 32) as i32)
        };
        regions
            .entry(S::get_chunk_key(&position))
            .or_default()
            .push((position, ChunkData::empty_sync(position.x, position.y)));
    }

    let before = serialization_metrics_snapshot();
    let started = Instant::now();
    let mut update_samples = Vec::with_capacity(count);
    let mut combined_hash = 0_u64;
    let mut total_file_bytes = 0_u64;
    let mut first_saved = None;

    for (region_index, chunks) in regions.values().enumerate() {
        let mut serializer = S::default();
        for (position, chunk) in chunks {
            let update_started = Instant::now();
            serializer
                .update_chunk(chunk, config)
                .await
                .expect("format benchmark chunk should update");
            update_samples.push(update_started.elapsed().as_nanos());
            if first_saved.is_none() {
                first_saved = Some((region_index, *position));
            }
        }
        let path = directory
            .path()
            .join(format!("{label}-{region_index}.region"));
        serializer
            .write(&path)
            .await
            .expect("format benchmark region should write durably");
        let persisted = fs::read(&path).expect("format benchmark output should be readable");
        total_file_bytes = total_file_bytes.saturating_add(persisted.len() as u64);
        combined_hash ^= xxh64(&persisted, region_index as u64);
    }

    let (first_region, first_position) = first_saved.expect("format fixture should not be empty");
    let first_path = directory
        .path()
        .join(format!("{label}-{first_region}.region"));
    let restored = S::read(
        fs::read(first_path)
            .expect("format restart file should be readable")
            .into(),
    )
    .expect("format restart file should parse");
    let (sender, mut receiver) = tokio::sync::mpsc::channel(1);
    restored.get_chunks(vec![first_position], sender).await;
    assert!(matches!(receiver.recv().await, Some(LoadedData::Loaded(_))));

    update_samples.sort_unstable();
    let p50 = update_samples[(update_samples.len() - 1) / 2];
    let p95 = update_samples[(update_samples.len() - 1) * 95 / 100];
    let p99 = update_samples[(update_samples.len() - 1) * 99 / 100];
    let elapsed = started.elapsed();
    let after = serialization_metrics_snapshot();
    eprintln!(
        "SER-033 chunk-matrix: format={label} chunks={count} layout={} regions={} file_bytes={total_file_bytes} hash={combined_hash:016x} elapsed_ms={:.3} chunks_per_s={:.1} update_p50_ns={p50} update_p95_ns={p95} update_p99_ns={p99} snapshot_bytes={} compressed_bytes={} written_bytes={} lock_hold_ns={} peak_rss_bytes={}",
        if multi_region { "multi" } else { "single" },
        regions.len(),
        elapsed.as_secs_f64() * 1_000.0,
        count as f64 / elapsed.as_secs_f64(),
        after.snapshot_bytes.saturating_sub(before.snapshot_bytes),
        after
            .compressed_bytes
            .saturating_sub(before.compressed_bytes),
        after.written_bytes.saturating_sub(before.written_bytes),
        after.lock_hold_nanos.saturating_sub(before.lock_hold_nanos),
        peak_rss_bytes().map_or_else(|| "unsupported".to_string(), |rss| rss.to_string()),
    );
}

fn measure_chunk_format_matrix(runtime: &tokio::runtime::Runtime) {
    for count in [32, 256, 1024] {
        for multi_region in [false, true] {
            runtime.block_on(measure_format_case::<AnvilChunkFile<ChunkData>>(
                "anvil",
                count,
                multi_region,
                &AnvilChunkConfig::default(),
            ));
            runtime.block_on(measure_format_case::<LinearV2File<ChunkData>>(
                "linear",
                count,
                multi_region,
                &(),
            ));
            runtime.block_on(measure_format_case::<PumpFile<ChunkData>>(
                "pump",
                count,
                multi_region,
                &(),
            ));
        }
    }
}

fn measure_mutation_tail(chunk: &Arc<ChunkData>, concurrent_serialization: bool) {
    const SAMPLE_COUNT: usize = 20_000;

    let stop = Arc::new(AtomicBool::new(false));
    let serializer_count = Arc::new(AtomicUsize::new(0));
    let barrier = Arc::new(Barrier::new(if concurrent_serialization { 2 } else { 1 }));
    let serializer = concurrent_serialization.then(|| {
        let chunk = Arc::clone(chunk);
        let stop = Arc::clone(&stop);
        let serializer_count = Arc::clone(&serializer_count);
        let barrier = Arc::clone(&barrier);
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .build()
                .expect("failed to create benchmark runtime");
            barrier.wait();
            while !stop.load(Ordering::Acquire) {
                runtime
                    .block_on(chunk.to_bytes())
                    .expect("failed to serialize benchmark chunk");
                serializer_count.fetch_add(1, Ordering::Relaxed);
            }
        })
    });

    barrier.wait();
    let mut lock_wait_ns = Vec::with_capacity(SAMPLE_COUNT);
    let mut tick_ns = Vec::with_capacity(SAMPLE_COUNT);
    for index in 0..SAMPLE_COUNT {
        let lock_started = Instant::now();
        drop(
            chunk
                .section
                .block_sections
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        lock_wait_ns.push(lock_started.elapsed().as_nanos());

        let state = if index & 1 == 0 {
            pumpkin_data::Block::STONE.default_state.id
        } else {
            pumpkin_data::Block::DIRT.default_state.id
        };
        let tick_started = Instant::now();
        black_box(chunk.set_block_absolute_y(0, 0, 0, state));
        tick_ns.push(tick_started.elapsed().as_nanos());
    }

    stop.store(true, Ordering::Release);
    if let Some(serializer) = serializer {
        serializer.join().expect("serializer thread panicked");
    }

    let label = if concurrent_serialization {
        "concurrent"
    } else {
        "baseline"
    };
    let lock_p95 = percentile(&mut lock_wait_ns, 95);
    let lock_p99 = percentile(&mut lock_wait_ns, 99);
    let tick_p95 = percentile(&mut tick_ns, 95);
    let tick_p99 = percentile(&mut tick_ns, 99);
    eprintln!(
        "SER-010 {label}: samples={SAMPLE_COUNT} serializations={} lock_p95_ns={lock_p95} lock_p99_ns={lock_p99} tick_p95_ns={tick_p95} tick_p99_ns={tick_p99}",
        serializer_count.load(Ordering::Relaxed),
    );
}

fn bench_chunk_io(c: &mut Criterion) {
    let dimension = Dimension::OVERWORLD;
    let world_gen = get_world_gen(
        Seed(42),
        dimension.clone(),
        false,
        Vec::new(),
        String::new(),
    );
    let chunk = generate_single_chunk(
        &dimension,
        0,
        &world_gen,
        &BlockRegistry,
        0,
        0,
        StagedChunkEnum::Full,
    );
    let Chunk::Level(chunk) = chunk else {
        panic!("full generation must return a level chunk");
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("failed to create benchmark runtime");
    measure_mixed_entity_restart(&runtime);
    measure_chunk_format_matrix(&runtime);
    let bytes = runtime
        .block_on(chunk.to_bytes())
        .expect("failed to serialize benchmark chunk");
    let position = Vector2::new(chunk.x, chunk.z);

    measure_mutation_tail(&chunk, false);
    measure_mutation_tail(&chunk, true);

    c.bench_function("chunk_nbt_serialization", |b| {
        b.iter(|| {
            black_box(
                runtime
                    .block_on(chunk.to_bytes())
                    .expect("failed to serialize benchmark chunk"),
            );
        });
    });

    c.bench_function("chunk_nbt_deserialization", |b| {
        b.iter(|| {
            black_box(
                ChunkData::from_bytes(black_box(&bytes), position)
                    .expect("failed to deserialize benchmark chunk"),
            );
        });
    });
}

criterion_group!(benches, bench_chunk_io);
criterion_main!(benches);
