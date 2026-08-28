#![no_main]

use libfuzzer_sys::fuzz_target;
use pumpkin_nbt::{Nbt, deserializer::NbtReadHelperJava};
use std::io::Cursor;

fuzz_target!(|data: &[u8]| {
    let mut reader = NbtReadHelperJava::new(Cursor::new(data));
    let Ok(decoded) = Nbt::read(&mut reader) else {
        return;
    };
    let encoded = decoded.write().expect("decoded NBT must remain serializable");
    let mut roundtrip_reader = NbtReadHelperJava::new(Cursor::new(encoded.as_ref()));
    let roundtrip = Nbt::read(&mut roundtrip_reader).expect("encoded NBT must decode");
    assert_eq!(roundtrip, decoded);
});
