#![no_main]
use libfuzzer_sys::fuzz_target;
use pumpkin_nbt::nbt_compress::{
    read_gzip_compound_tag, read_gzip_compound_tag_with_limit,
    write_gzip_compound_tag_to_bytes,
};
use pumpkin_nbt::{Nbt, NbtCompound, tag::NbtTag};
use std::io::Cursor;

fuzz_target!(|data: &[u8]| {
    let cursor = Cursor::new(data);
    let _ = read_gzip_compound_tag(cursor);

    let Some((&mode, payload)) = data.split_first() else {
        return;
    };
    let payload = &payload[..payload.len().min(4096)];
    let mut compound = NbtCompound::new();
    compound.put(
        "payload",
        NbtTag::ByteArray(payload.iter().map(|byte| *byte as i8).collect()),
    );
    let Ok(encoded) = Nbt::from(compound.clone()).write() else {
        return;
    };
    let Ok(mut compressed) = write_gzip_compound_tag_to_bytes(compound) else {
        return;
    };
    match mode % 3 {
        0 => {
            assert!(read_gzip_compound_tag_with_limit(
                Cursor::new(compressed),
                encoded.len()
            )
            .is_ok());
        }
        1 => {
            let limit = encoded.len().saturating_sub(1);
            assert!(matches!(
                read_gzip_compound_tag_with_limit(Cursor::new(compressed), limit),
                Err(pumpkin_nbt::Error::LargeLength(length)) if length == limit + 1
            ));
        }
        _ => {
            compressed.truncate(compressed.len().saturating_sub(1 + payload.len() % 8));
            let _ = read_gzip_compound_tag(Cursor::new(compressed));
        }
    }
});
