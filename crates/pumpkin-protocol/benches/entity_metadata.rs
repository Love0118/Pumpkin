use std::hint::black_box;
use std::io::Cursor;

use criterion::{Criterion, criterion_group, criterion_main};
use pumpkin_data::{
    block_state_remap::remap_block_state_for_version, meta_data_type::MetaDataType,
};
use pumpkin_protocol::{
    codec::var_int::VarInt,
    java::client::play::MetadataSerializer,
    ser::{NetworkWriteExt, WritingError},
};
use pumpkin_util::version::JavaMinecraftVersion;

fn legacy_block_state(
    state: VarInt,
    version: JavaMinecraftVersion,
    writer: &mut Vec<u8>,
) -> Result<(), WritingError> {
    let mut temporary = Vec::new();
    state.write_metadata(&mut temporary, &version)?;
    let mut cursor = Cursor::new(temporary);
    let decoded =
        VarInt::decode(&mut cursor).map_err(|error| WritingError::Message(error.to_string()))?;
    let remapped = u16::try_from(decoded.0).map_or(decoded, |state_id| {
        VarInt(i32::from(remap_block_state_for_version(state_id, version)))
    });
    writer.write_var_int(&remapped)
}

fn direct_block_state(state: VarInt, version: JavaMinecraftVersion) -> Vec<u8> {
    let mut output = Vec::with_capacity(8);
    state
        .write_metadata_for_type(&mut output, &version, MetaDataType::BLOCK_STATE)
        .unwrap();
    output
}

fn bench_entity_metadata(criterion: &mut Criterion) {
    let version = JavaMinecraftVersion::V_1_20;
    let state = VarInt(1_000);
    let mixed_versions = [
        JavaMinecraftVersion::V_1_20,
        JavaMinecraftVersion::V_1_21_4,
        JavaMinecraftVersion::V_26_2,
    ];
    let mixed_payloads = mixed_versions.map(|version| direct_block_state(state, version));
    for recipient in 0..100 {
        let version_index = recipient % mixed_versions.len();
        assert_eq!(
            direct_block_state(state, mixed_versions[version_index]),
            mixed_payloads[version_index]
        );
    }
    let mut group = criterion.benchmark_group("java_entity_metadata");

    group.bench_function("block_state_legacy_temporary", |bencher| {
        bencher.iter(|| {
            let mut output = Vec::with_capacity(8);
            legacy_block_state(black_box(state), version, &mut output).unwrap();
            black_box(output)
        });
    });

    group.bench_function("block_state_direct", |bencher| {
        bencher.iter(|| {
            let mut output = Vec::with_capacity(8);
            black_box(state)
                .write_metadata_for_type(&mut output, &version, MetaDataType::BLOCK_STATE)
                .unwrap();
            black_box(output)
        });
    });

    for recipients in [1usize, 20, 100] {
        group.bench_function(format!("legacy_per_recipient_{recipients}"), |bencher| {
            bencher.iter(|| {
                let mut payloads = Vec::with_capacity(recipients);
                for _ in 0..recipients {
                    let mut output = Vec::with_capacity(8);
                    legacy_block_state(state, version, &mut output).unwrap();
                    payloads.push(output);
                }
                black_box(payloads)
            });
        });

        group.bench_function(format!("shared_version_payload_{recipients}"), |bencher| {
            bencher.iter(|| {
                let mut output = Vec::with_capacity(8);
                state
                    .write_metadata_for_type(&mut output, &version, MetaDataType::BLOCK_STATE)
                    .unwrap();
                for _ in 0..recipients {
                    black_box(output.as_slice());
                }
                black_box(output)
            });
        });

        group.bench_function(
            format!("mixed_version_per_recipient_{recipients}"),
            |bencher| {
                bencher.iter(|| {
                    let payloads = (0..recipients)
                        .map(|recipient| {
                            direct_block_state(
                                state,
                                mixed_versions[recipient % mixed_versions.len()],
                            )
                        })
                        .collect::<Vec<_>>();
                    black_box(payloads)
                });
            },
        );

        group.bench_function(
            format!("mixed_version_shared_payload_{recipients}"),
            |bencher| {
                bencher.iter(|| {
                    for recipient in 0..recipients {
                        black_box(&mixed_payloads[recipient % mixed_payloads.len()]);
                    }
                    black_box(&mixed_payloads)
                });
            },
        );
    }

    group.finish();
}

criterion_group!(benches, bench_entity_metadata);
criterion_main!(benches);
