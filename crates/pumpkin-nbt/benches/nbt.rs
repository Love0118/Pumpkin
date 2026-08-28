#![allow(clippy::unwrap_used)]

use criterion::{Criterion, criterion_group, criterion_main};
use pumpkin_nbt::{
    Nbt, NbtCompound, deserializer,
    nbt_compress::{read_gzip_compound_tag, write_gzip_compound_tag},
    tag::NbtTag,
};
use std::{
    collections::{BTreeMap, HashMap},
    hint::black_box,
    io::{Cursor, Error, ErrorKind, Write},
};

struct FailAfter {
    remaining: usize,
}

impl Write for FailAfter {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        if self.remaining == 0 {
            return Err(Error::new(ErrorKind::WriteZero, "injected short writer"));
        }
        let written = buffer.len().min(self.remaining);
        self.remaining -= written;
        Ok(written)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn create_large_compound(depth: usize) -> NbtCompound {
    let mut compound = NbtCompound::new();
    compound.put_byte("byte", 123);
    compound.put_short("short", 1342);
    compound.put_int("int", 4313);
    compound.put_long("long", 34);
    compound.put_float("float", 1.00);
    compound.put_double("double", 69.42);
    compound.put_string("string", "Hello test benchmark data".to_string());
    compound.put(
        "byte_array",
        NbtTag::ByteArray(vec![0, 1, 2, 3, 4, 5, 6, 7, 8, 9].into()),
    );
    compound.put(
        "int_array",
        NbtTag::IntArray(vec![10, 11, 12, 13, 14, 15, 16, 17, 18, 19]),
    );
    compound.put(
        "long_array",
        NbtTag::LongArray(vec![20, 21, 22, 23, 24, 25, 26, 27, 28, 29]),
    );

    let list = vec![
        NbtTag::String("one".into()),
        NbtTag::String("two".into()),
        NbtTag::String("three".into()),
    ];
    compound.put_list("list_string", list);

    if depth > 0 {
        compound.put_compound("nested", create_large_compound(depth - 1));
    }
    compound
}

fn create_player_fixture() -> NbtCompound {
    let mut player = NbtCompound::new();
    player.put_int("DataVersion", 4903);
    player.put_list(
        "Pos",
        vec![
            NbtTag::Double(12.5),
            NbtTag::Double(64.0),
            NbtTag::Double(-8.5),
        ],
    );
    player.put_float("Health", 20.0);
    player.put_int("foodLevel", 20);
    player.put_int("playerGameType", 1);

    let inventory = (0..41)
        .map(|slot| {
            let mut item = NbtCompound::new();
            item.put_byte("Slot", slot as i8);
            item.put_string(
                "id",
                if slot % 2 == 0 {
                    "minecraft:stone"
                } else {
                    "minecraft:iron_sword"
                },
            );
            item.put_int("count", (slot % 64 + 1) as i32);
            NbtTag::Compound(item)
        })
        .collect();
    player.put_list("Inventory", inventory);

    let mut abilities = NbtCompound::new();
    abilities.put_bool("flying", false);
    abilities.put_bool("mayfly", true);
    abilities.put_float("flySpeed", 0.05);
    abilities.put_float("walkSpeed", 0.1);
    player.put_compound("abilities", abilities);
    player.put_string("plugin:marker", "round-trip");
    player
}

fn create_entity_fixture() -> NbtCompound {
    let mut entity = NbtCompound::new();
    entity.put_string("id", "minecraft:wolf");
    entity.put_list(
        "Pos",
        vec![
            NbtTag::Double(12.5),
            NbtTag::Double(64.0),
            NbtTag::Double(-8.5),
        ],
    );
    entity.put_list(
        "Motion",
        vec![
            NbtTag::Double(0.0),
            NbtTag::Double(0.0),
            NbtTag::Double(0.0),
        ],
    );
    entity.put_list("Rotation", vec![NbtTag::Float(90.0), NbtTag::Float(0.0)]);
    entity.put_float("Health", 20.0);
    entity.put_short("Air", 300);
    entity.put_bool("OnGround", true);
    entity.put_bool("Invulnerable", false);
    entity.put_int("AngerTime", 400);
    entity.put_list("Tags", vec![NbtTag::String("roadmap-fixture".into())]);
    let mut brain = NbtCompound::new();
    brain.put_compound("memories", NbtCompound::new());
    entity.put_compound("Brain", brain);
    entity
}

fn create_chunk_fixture() -> NbtCompound {
    let mut chunk = NbtCompound::new();
    chunk.put_int("DataVersion", 4903);
    chunk.put_int("xPos", 4);
    chunk.put_int("zPos", -3);
    chunk.put_int("yPos", -4);
    chunk.put_string("Status", "minecraft:full");
    chunk.put_bool("isLightOn", true);
    chunk.put_long("InhabitedTime", 12_345);
    let sections = (-4i8..20)
        .map(|y| {
            let mut section = NbtCompound::new();
            section.put_byte("Y", y);
            let mut block_states = NbtCompound::new();
            let mut palette_entry = NbtCompound::new();
            palette_entry.put_string("Name", "minecraft:air");
            block_states.put_list("palette", vec![NbtTag::Compound(palette_entry)]);
            section.put_compound("block_states", block_states);
            NbtTag::Compound(section)
        })
        .collect();
    chunk.put_list("sections", sections);
    chunk.put_list("block_entities", Vec::new());
    chunk.put_list("block_ticks", Vec::new());
    chunk.put_list("fluid_ticks", Vec::new());
    chunk
}

#[derive(Default)]
struct CompoundKeyStats {
    compounds: usize,
    keys: usize,
    key_bytes: usize,
    compounds_with_at_most_eight_keys: usize,
}

fn collect_compound_key_stats(compound: &NbtCompound, stats: &mut CompoundKeyStats) {
    stats.compounds += 1;
    stats.keys += compound.child_tags.len();
    stats.key_bytes += compound
        .child_tags
        .keys()
        .map(|key| key.len())
        .sum::<usize>();
    stats.compounds_with_at_most_eight_keys += usize::from(compound.child_tags.len() <= 8);
    for tag in compound.child_tags.values() {
        match tag {
            NbtTag::Compound(nested) => collect_compound_key_stats(nested, stats),
            NbtTag::List(entries) => {
                for entry in entries {
                    if let NbtTag::Compound(nested) = entry {
                        collect_compound_key_stats(nested, stats);
                    }
                }
            }
            _ => {}
        }
    }
}

fn fixture_keys(compound: &NbtCompound) -> Vec<Box<str>> {
    compound.child_tags.keys().cloned().collect()
}

fn bench_fixture_maps(c: &mut Criterion, name: &str, fixture: &NbtCompound) {
    let mut stats = CompoundKeyStats::default();
    collect_compound_key_stats(fixture, &mut stats);
    let serialized_size = Nbt::from(fixture.clone()).write().unwrap().len();
    let suffix = format!(
        "{name}/c{}-k{}-kb{}-small{}-wire{}",
        stats.compounds,
        stats.keys,
        stats.key_bytes,
        stats.compounds_with_at_most_eight_keys,
        serialized_size
    );
    let keys = fixture_keys(fixture);

    let benchmark_name = format!("nbt/map/hashmap/insert/{suffix}");
    c.bench_function(&benchmark_name, |b| {
        b.iter(|| {
            let map = keys
                .iter()
                .cloned()
                .enumerate()
                .map(|(index, key)| (key, index))
                .collect::<HashMap<_, _>>();
            black_box(map);
        });
    });
    let benchmark_name = format!("nbt/map/btreemap/insert/{suffix}");
    c.bench_function(&benchmark_name, |b| {
        b.iter(|| {
            let map = keys
                .iter()
                .cloned()
                .enumerate()
                .map(|(index, key)| (key, index))
                .collect::<BTreeMap<_, _>>();
            black_box(map);
        });
    });

    let hash_map = keys
        .iter()
        .cloned()
        .enumerate()
        .map(|(index, key)| (key, index))
        .collect::<HashMap<_, _>>();
    let tree_map = keys
        .iter()
        .cloned()
        .enumerate()
        .map(|(index, key)| (key, index))
        .collect::<BTreeMap<_, _>>();
    let benchmark_name = format!("nbt/map/hashmap/lookup/{suffix}");
    c.bench_function(&benchmark_name, |b| {
        b.iter(|| {
            for key in &keys {
                black_box(hash_map.get(key));
            }
        });
    });
    let benchmark_name = format!("nbt/map/btreemap/lookup/{suffix}");
    c.bench_function(&benchmark_name, |b| {
        b.iter(|| {
            for key in &keys {
                black_box(tree_map.get(key));
            }
        });
    });
}

pub fn bench_nbt(c: &mut Criterion) {
    let compound_data = create_large_compound(5);
    let nbt_wrapper = Nbt::new(String::new(), compound_data.clone());
    let wrapper_bytes_java = nbt_wrapper.clone().write().unwrap();
    let wrapper_bytes_bedrock = nbt_wrapper.write_bedrock().unwrap();
    let player_fixture = create_player_fixture();
    let entity_fixture = create_entity_fixture();
    let chunk_fixture = create_chunk_fixture();
    let mut player_gzip = Vec::new();
    write_gzip_compound_tag(player_fixture.clone(), &mut player_gzip).unwrap();

    bench_fixture_maps(c, "entity", &entity_fixture);
    bench_fixture_maps(c, "player", &player_fixture);
    bench_fixture_maps(c, "chunk", &chunk_fixture);

    c.bench_function("nbt/java/serialize/raw", |b| {
        b.iter(|| {
            let nbt = Nbt::new(String::new(), compound_data.clone());
            nbt.write().unwrap();
        });
    });

    c.bench_function("nbt/java/serialize/reused-writer", |b| {
        let mut bytes = Vec::new();
        b.iter(|| {
            bytes.clear();
            let nbt = Nbt::new(String::new(), compound_data.clone());
            nbt.write_to_writer(&mut bytes).unwrap();
        });
    });

    c.bench_function("nbt/java/serialize/error-writer", |b| {
        b.iter(|| {
            let mut writer = FailAfter { remaining: 32 };
            let nbt = Nbt::new(String::new(), compound_data.clone());
            assert!(nbt.write_to_writer(&mut writer).is_err());
        });
    });

    c.bench_function("nbt/player/gzip/write-stream", |b| {
        let mut bytes = Vec::new();
        b.iter(|| {
            bytes.clear();
            write_gzip_compound_tag(player_fixture.clone(), &mut bytes).unwrap();
        });
    });

    c.bench_function("nbt/player/gzip/read-stream", |b| {
        b.iter(|| {
            let restored = read_gzip_compound_tag(Cursor::new(&player_gzip)).unwrap();
            assert_eq!(restored.get_string("plugin:marker"), Some("round-trip"));
        });
    });

    c.bench_function("nbt/java/deserialize/raw", |b| {
        b.iter(|| {
            let mut cursor = Cursor::new(&wrapper_bytes_java[..]);
            let mut reader = deserializer::NbtReadHelperJava::new(&mut cursor);
            Nbt::read(&mut reader).unwrap();
        });
    });

    c.bench_function("nbt/bedrock/serialize/raw", |b| {
        b.iter(|| {
            let nbt = Nbt::new(String::new(), compound_data.clone());
            nbt.write_bedrock().unwrap();
        });
    });

    c.bench_function("nbt/bedrock/serialize/reused-writer", |b| {
        let mut bytes = Vec::new();
        b.iter(|| {
            bytes.clear();
            let nbt = Nbt::new(String::new(), compound_data.clone());
            nbt.write_to_writer_bedrock(&mut bytes).unwrap();
        });
    });

    c.bench_function("nbt/bedrock/deserialize/raw", |b| {
        b.iter(|| {
            let mut cursor = Cursor::new(&wrapper_bytes_bedrock[..]);
            let mut reader = deserializer::NbtReadHelperBedrock::new(&mut cursor);
            Nbt::read(&mut reader).unwrap();
        });
    });
}

criterion_group!(benches, bench_nbt);
criterion_main!(benches);
