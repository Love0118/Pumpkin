//! Reading, writing, and manipulating Minecraft's Named Binary Tag (NBT) data.
//!
//! The crate supports the standard Java Edition representation, unnamed network
//! NBT, Bedrock network NBT, and gzip-compressed NBT. Data is handled directly
//! via [`Nbt`], [`NbtCompound`], and [`NbtTag`].

#![deny(clippy::unwrap_used)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

use std::{
    io::{self, Write},
    ops::Deref,
};

use bytes::Bytes;
use deserializer::NbtReadHelper;
use serializer::{NbtWriteHelper, NbtWriteHelperBedrock, NbtWriteHelperJava};
use thiserror::Error;

/// Compound-tag storage and construction helpers.
pub mod compound;
/// Low-level NBT deserialization support.
pub mod deserializer;
/// Reading and writing gzip-compressed NBT.
pub mod nbt_compress;
/// Integration with Pumpkin's dynamic codec operations.
pub mod nbt_ops;
/// Low-level NBT serialization support.
pub mod serializer;
/// The individual NBT tag types.
pub mod tag;

pub use compound::NbtCompound;

// This NBT crate is inspired from CrabNBT

/// Numeric identifier for an end tag.
pub const END_ID: u8 = 0x00;
/// Numeric identifier for a byte tag.
pub const BYTE_ID: u8 = 0x01;
/// Numeric identifier for a short tag.
pub const SHORT_ID: u8 = 0x02;
/// Numeric identifier for an integer tag.
pub const INT_ID: u8 = 0x03;
/// Numeric identifier for a long tag.
pub const LONG_ID: u8 = 0x04;
/// Numeric identifier for a float tag.
pub const FLOAT_ID: u8 = 0x05;
/// Numeric identifier for a double tag.
pub const DOUBLE_ID: u8 = 0x06;
/// Numeric identifier for a byte-array tag.
pub const BYTE_ARRAY_ID: u8 = 0x07;
/// Numeric identifier for a string tag.
pub const STRING_ID: u8 = 0x08;
/// Numeric identifier for a list tag.
pub const LIST_ID: u8 = 0x09;
/// Numeric identifier for a compound tag.
pub const COMPOUND_ID: u8 = 0x0A;
/// Numeric identifier for an integer-array tag.
pub const INT_ARRAY_ID: u8 = 0x0B;
/// Numeric identifier for a long-array tag.
pub const LONG_ARRAY_ID: u8 = 0x0C;

/// Maximum number of elements accepted when decoding a list or array.
pub const MAX_ARRAY_LENGTH: usize = 512_000;
/// Maximum encoded byte length accepted for one NBT string.
pub const MAX_STRING_LENGTH: usize = 512_000;
/// Maximum nesting depth allowed when decoding NBT compound or list tags.
pub const MAX_NBT_DEPTH: usize = 512;

/// Errors produced while reading, writing, or converting NBT data.
#[derive(Error, Debug)]
pub enum Error {
    /// The root tag was not a compound tag and contains the reported tag ID.
    #[error("The root tag of the NBT file is not a compound tag. Received tag id: {0}")]
    NoRootCompound(u8),
    /// A tag ID not defined by the NBT format was encountered.
    #[error("Encountered an unknown NBT tag id: {0}.")]
    UnknownTagId(u8),
    /// A Java CESU-8 string could not be decoded.
    #[error("Failed to Cesu 8 Decode")]
    Cesu8DecodingError,
    /// A string could not be decoded as UTF-8.
    #[error("Failed to UTF-8 Decode")]
    Utf8DecodingError,
    /// Serde reported an invalid value or serializer state.
    #[error("Serde error: {0}")]
    SerdeError(String),
    /// The requested Rust type has no NBT representation.
    #[error("NBT doesn't support this type: {0}")]
    UnsupportedType(String),
    /// The underlying reader or writer returned an I/O error.
    #[error("NBT reading was cut short: {0}")]
    Incomplete(io::Error),
    /// A list or array declared a negative element count.
    #[error("Negative list length: {0}")]
    NegativeLength(i32),
    /// A string, list, or array exceeded the supported length.
    #[error("Length too large: {0}")]
    LargeLength(usize),
    /// A Bedrock variable-length integer exceeded its maximum encoded size.
    #[error("Failed to decode varint - value too large")]
    VarIntTooLarge,
    /// A Bedrock variable-length long exceeded its maximum encoded size.
    #[error("Failed to decode varlong - value too large")]
    VarLongTooLarge,
    /// NBT nesting depth exceeded the maximum allowed limit.
    #[error("NBT depth exceeded maximum allowed limit")]
    MaxDepthExceeded,
    /// A list tag specified an invalid element tag type.
    #[error("Invalid element tag type for list: {0}")]
    InvalidListTag(u8),
}

/// A complete NBT document containing a named root compound.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Nbt {
    /// Name stored alongside the root compound.
    pub name: String,
    /// Root compound containing the document's tags.
    pub root_tag: NbtCompound,
}

impl Nbt {
    /// Creates a document from a root name and compound.
    #[must_use]
    pub const fn new(name: String, tag: NbtCompound) -> Self {
        Self {
            name,
            root_tag: tag,
        }
    }

    /// Reads a named NBT document from a format-specific reader.
    ///
    /// Returns [`Error::NoRootCompound`] when the first tag is not a compound.
    pub fn read<'a, R: NbtReadHelper<'a>>(reader: &mut R) -> Result<Self, Error> {
        let tag_type_id = reader.get_u8()?;

        if tag_type_id != COMPOUND_ID {
            return Err(Error::NoRootCompound(tag_type_id));
        }

        Ok(Self {
            name: reader.get_string()?.into_owned(),
            root_tag: NbtCompound::deserialize_content(reader)?,
        })
    }

    /// Reads an NBT document that omits the root compound's name.
    ///
    /// The returned document has an empty [`Self::name`].
    pub fn read_unnamed<'a, R: NbtReadHelper<'a>>(reader: &mut R) -> Result<Self, Error> {
        let tag_type_id = reader.get_u8()?;

        if tag_type_id != COMPOUND_ID {
            return Err(Error::NoRootCompound(tag_type_id));
        }

        Ok(Self {
            name: String::new(),
            root_tag: NbtCompound::deserialize_content(reader)?,
        })
    }

    /// Serializes this document using the Java Edition NBT representation.
    ///
    /// Serialization failures, including an overlong root name, are returned to
    /// the caller instead of producing a partial byte sequence.
    pub fn write(&self) -> Result<Bytes, Error> {
        let mut bytes = Vec::new();
        self.write_to_writer(&mut bytes)?;
        Ok(bytes.into())
    }

    /// Serializes this document using the Bedrock network NBT representation.
    ///
    /// Serialization failures are returned to the caller instead of producing
    /// a partial byte sequence.
    pub fn write_bedrock(&self) -> Result<Bytes, Error> {
        let mut bytes = Vec::new();
        self.write_to_writer_bedrock(&mut bytes)?;
        Ok(bytes.into())
    }

    /// Writes this document directly in the Java Edition representation.
    pub fn write_to_writer<W: Write>(&self, writer: W) -> Result<(), Error> {
        let mut writer = NbtWriteHelperJava::new(writer);
        writer.write_u8(COMPOUND_ID)?;
        writer.write_string(&self.name)?;
        self.root_tag.serialize_content(&mut writer)
    }

    /// Writes this document directly in the Bedrock network representation.
    pub fn write_to_writer_bedrock<W: Write>(&self, writer: W) -> Result<(), Error> {
        let mut writer = NbtWriteHelperBedrock::new(writer);
        writer.write_u8(COMPOUND_ID)?;
        writer.write_string(&self.name)?;
        self.root_tag.serialize_content(&mut writer)
    }

    /// Writes a borrowed compound as an unnamed Bedrock network NBT document.
    pub fn write_compound_to_writer_bedrock<W: Write>(
        compound: &NbtCompound,
        writer: W,
    ) -> Result<(), Error> {
        let mut writer = NbtWriteHelperBedrock::new(writer);
        writer.write_u8(COMPOUND_ID)?;
        writer.write_string("")?;
        compound.serialize_content(&mut writer)
    }

    /// Serializes this document without the root compound's name.
    pub fn write_unnamed(&self) -> Result<Bytes, Error> {
        let mut bytes = Vec::new();
        self.write_unnamed_to_writer(&mut bytes)?;
        Ok(bytes.into())
    }

    /// Writes this document directly without the root compound's name.
    pub fn write_unnamed_to_writer<W: Write>(&self, writer: W) -> Result<(), Error> {
        let mut writer = NbtWriteHelperJava::new(writer);
        writer.write_u8(COMPOUND_ID)?;
        self.root_tag.serialize_content(&mut writer)
    }
}

impl Deref for Nbt {
    type Target = NbtCompound;

    fn deref(&self) -> &Self::Target {
        &self.root_tag
    }
}

impl From<NbtCompound> for Nbt {
    fn from(value: NbtCompound) -> Self {
        Self::new(String::new(), value)
    }
}

impl<T> AsRef<T> for Nbt
where
    T: ?Sized,
    <Self as Deref>::Target: AsRef<T>,
{
    fn as_ref(&self) -> &T {
        self.deref().as_ref()
    }
}

impl AsMut<NbtCompound> for Nbt {
    fn as_mut(&mut self) -> &mut NbtCompound {
        &mut self.root_tag
    }
}

#[cfg(test)]
mod tests {
    use std::{
        io::{self, Write},
        sync::Arc,
    };

    use super::{COMPOUND_ID, END_ID, Error, Nbt, NbtCompound};
    use crate::serializer::{NbtWriteHelper, NbtWriteHelperJava};
    use crate::tag::NbtTag;

    struct FailingWriter {
        remaining: usize,
    }

    impl Write for FailingWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if self.remaining == 0 {
                return Err(io::Error::other("injected write failure"));
            }

            let written = bytes.len().min(self.remaining);
            self.remaining -= written;
            Ok(written)
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn empty_documents_have_expected_encodings() {
        let java = Nbt::from(NbtCompound::new())
            .write()
            .expect("empty Java NBT should serialize");
        let unnamed = Nbt::from(NbtCompound::new())
            .write_unnamed()
            .expect("empty unnamed NBT should serialize");
        let bedrock = Nbt::from(NbtCompound::new())
            .write_bedrock()
            .expect("empty Bedrock NBT should serialize");

        assert_eq!(java.as_ref(), [0x0A, 0x00, 0x00, 0x00]);
        assert_eq!(unnamed.as_ref(), [0x0A, 0x00]);
        assert_eq!(bedrock.as_ref(), [0x0A, 0x00, 0x00]);
    }

    #[test]
    fn direct_writers_match_buffer_convenience_methods() {
        let mut compound = NbtCompound::new();
        compound.put_int("answer", 42);
        compound.put_string("name", "Pumpkin".to_owned());
        let nbt = Nbt::new("root".to_owned(), compound);

        let expected_java = nbt.clone().write().expect("Java NBT should serialize");
        let mut direct_java = Vec::new();
        nbt.clone()
            .write_to_writer(&mut direct_java)
            .expect("direct Java NBT should serialize");
        assert_eq!(expected_java.as_ref(), direct_java);

        let expected_bedrock = nbt
            .clone()
            .write_bedrock()
            .expect("Bedrock NBT should serialize");
        let mut direct_bedrock = Vec::new();
        nbt.clone()
            .write_to_writer_bedrock(&mut direct_bedrock)
            .expect("direct Bedrock NBT should serialize");
        assert_eq!(expected_bedrock.as_ref(), direct_bedrock);

        let expected_unnamed = nbt
            .clone()
            .write_unnamed()
            .expect("unnamed NBT should serialize");
        let mut direct_unnamed = Vec::new();
        nbt.write_unnamed_to_writer(&mut direct_unnamed)
            .expect("direct unnamed NBT should serialize");
        assert_eq!(expected_unnamed.as_ref(), direct_unnamed);
    }

    #[test]
    fn borrowed_bedrock_compound_writer_matches_owned_document() {
        let mut compound = NbtCompound::new();
        compound.put_int("answer", 42);
        compound.put_string("name", "Pumpkin".to_owned());
        let expected = Nbt::from(compound.clone()).write_bedrock().unwrap();

        let mut actual = Vec::new();
        Nbt::write_compound_to_writer_bedrock(&compound, &mut actual).unwrap();

        assert_eq!(actual, expected.as_ref());
        assert_eq!(compound.get_int("answer"), Some(42));
    }

    #[test]
    fn direct_writer_propagates_io_failure() {
        let mut compound = NbtCompound::new();
        compound.put_string("value", "payload".to_owned());

        let error = Nbt::from(compound)
            .write_to_writer(FailingWriter { remaining: 3 })
            .expect_err("injected writer failure must be returned");

        assert!(matches!(error, Error::Incomplete(_)));
    }

    #[test]
    fn overlong_java_root_name_is_an_error() {
        let error = Nbt::new("x".repeat(u16::MAX as usize + 1), NbtCompound::new())
            .write()
            .expect_err("overlong Java root name must not yield partial bytes");

        assert!(matches!(error, Error::LargeLength(_)));
    }

    #[test]
    fn borrowed_compound_list_matches_owned_list_encoding() {
        let mut pig = NbtCompound::new();
        pig.put_string("id", "minecraft:pig".to_owned());
        let mut wolf = NbtCompound::new();
        wolf.put_string("id", "minecraft:wolf".to_owned());
        let entities = vec![pig, wolf];

        let mut owned_root = NbtCompound::new();
        owned_root.put_list(
            "Entities",
            entities.iter().cloned().map(NbtTag::Compound).collect(),
        );
        let expected = Nbt::from(owned_root).write().unwrap();

        let mut actual = Vec::new();
        let mut writer = NbtWriteHelperJava::new(&mut actual);
        writer.write_u8(COMPOUND_ID).unwrap();
        writer.write_string("").unwrap();
        NbtCompound::serialize_compound_list_entry("Entities", &entities, &mut writer).unwrap();
        writer.write_u8(END_ID).unwrap();

        assert_eq!(actual, expected.as_ref());
        assert_eq!(entities[0].get_string("id"), Some("minecraft:pig"));
    }

    #[test]
    fn cloned_string_tags_share_their_payload() {
        let original = NbtTag::String(Arc::from("minecraft:pig"));
        let cloned = original.clone();
        let (NbtTag::String(original), NbtTag::String(cloned)) = (original, cloned) else {
            panic!("both tags should remain strings");
        };

        assert!(Arc::ptr_eq(&original, &cloned));
    }

    #[test]
    fn compound_bytes_are_deterministic_across_insertion_order() {
        let mut first_nested = NbtCompound::new();
        first_nested.put_int("z", 3);
        first_nested.put_int("a", 1);
        let mut first = NbtCompound::new();
        first.put_string("name", "Pumpkin");
        first.put_compound("nested", first_nested);

        let mut second_nested = NbtCompound::new();
        second_nested.put_int("a", 1);
        second_nested.put_int("z", 3);
        let mut second = NbtCompound::new();
        second.put_compound("nested", second_nested);
        second.put_string("name", "Pumpkin");

        assert_eq!(
            Nbt::from(first).write().unwrap(),
            Nbt::from(second).write().unwrap()
        );
    }

    #[test]
    fn bedrock_root_name_respects_the_shared_string_budget() {
        let mut bytes = vec![COMPOUND_ID];
        let mut length = (super::MAX_STRING_LENGTH as u32) + 1;
        loop {
            let mut byte = (length & 0x7f) as u8;
            length >>= 7;
            if length != 0 {
                byte |= 0x80;
            }
            bytes.push(byte);
            if length == 0 {
                break;
            }
        }
        let mut reader =
            crate::deserializer::NbtReadHelperBedrock::new(std::io::Cursor::new(bytes));

        let error = Nbt::read(&mut reader).expect_err("oversized Bedrock string must fail closed");

        assert!(
            matches!(error, Error::LargeLength(length) if length == super::MAX_STRING_LENGTH + 1)
        );
    }
}
