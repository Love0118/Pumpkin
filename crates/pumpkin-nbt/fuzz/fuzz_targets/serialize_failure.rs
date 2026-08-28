#![no_main]

use libfuzzer_sys::fuzz_target;
use pumpkin_nbt::{Nbt, deserializer::NbtReadHelperJava};
use std::io::{self, Cursor, Write};

struct FailingWriter {
    remaining: usize,
}

impl Write for FailingWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.remaining == 0 {
            return Err(io::Error::other("injected fuzz writer failure"));
        }
        let written = bytes.len().min(self.remaining);
        self.remaining -= written;
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fuzz_target!(|data: &[u8]| {
    let Some((&budget, nbt_bytes)) = data.split_first() else {
        return;
    };
    let mut reader = NbtReadHelperJava::new(Cursor::new(nbt_bytes));
    let Ok(decoded) = Nbt::read(&mut reader) else {
        return;
    };
    let _ = decoded.write_to_writer(FailingWriter {
        remaining: usize::from(budget),
    });
});
