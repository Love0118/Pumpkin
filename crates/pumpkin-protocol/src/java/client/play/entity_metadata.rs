use std::io::{Cursor, Write};

use pumpkin_data::{
    block_state_remap::remap_block_state_for_version,
    item_id_remap::remap_item_id_for_version,
    meta_data_type::MetaDataType,
    packet::clientbound::play::SET_ENTITY_DATA,
    tracked_data::{TrackedData, TrackedId},
};
use pumpkin_macros::java_packet;
use pumpkin_util::version::JavaMinecraftVersion;

use crate::{
    ClientPacket, VarInt,
    ser::{NetworkWriteExt, WritingError},
};

use super::particle::particle_id_for_version;

pub trait MetadataSerializer {
    fn write_metadata(
        &self,
        writer: &mut impl std::io::Write,
        version: &JavaMinecraftVersion,
    ) -> Result<(), WritingError>;

    fn write_metadata_for_type(
        &self,
        writer: &mut impl std::io::Write,
        version: &JavaMinecraftVersion,
        metadata_type: MetaDataType,
    ) -> Result<(), WritingError> {
        if matches!(
            metadata_type,
            MetaDataType::BLOCK_STATE | MetaDataType::ITEM_STACK | MetaDataType::PARTICLE
        ) {
            let mut serialized = Vec::new();
            self.write_metadata(&mut serialized, version)?;
            let mut cursor = Cursor::new(serialized);

            if metadata_type == MetaDataType::BLOCK_STATE {
                let state = VarInt::decode(&mut cursor).map_err(|error| {
                    WritingError::Message(format!("Failed to decode block state metadata: {error}"))
                })?;
                let state = u16::try_from(state.0).map_or(state, |state_id| {
                    VarInt(i32::from(remap_block_state_for_version(state_id, *version)))
                });
                return writer.write_var_int(&state);
            }

            if metadata_type == MetaDataType::ITEM_STACK {
                let count = VarInt::decode(&mut cursor).map_err(|error| {
                    WritingError::Message(format!("Failed to decode item stack count: {error}"))
                })?;
                writer.write_var_int(&count)?;
                if count.0 > 0 {
                    let item_id = VarInt::decode(&mut cursor).map_err(|error| {
                        WritingError::Message(format!("Failed to decode item id: {error}"))
                    })?;
                    let remapped_id = u16::try_from(item_id.0)
                        .map_or(0, |id| remap_item_id_for_version(id, *version));
                    writer.write_var_int(&VarInt(i32::from(remapped_id)))?;
                    let remainder_start = cursor.position() as usize;
                    writer.write_slice(&cursor.into_inner()[remainder_start..])?;
                }
                return Ok(());
            }

            let particle_id = VarInt::decode(&mut cursor).map_err(|error| {
                WritingError::Message(format!("Failed to decode particle metadata: {error}"))
            })?;
            writer.write_var_int(&particle_id_for_version(particle_id, *version))?;
            let remainder_start = cursor.position() as usize;
            writer.write_slice(&cursor.into_inner()[remainder_start..])?;
            return Ok(());
        }
        self.write_metadata(writer, version)
    }
}

impl<T: MetadataSerializer + ?Sized> MetadataSerializer for &T {
    fn write_metadata(
        &self,
        writer: &mut impl std::io::Write,
        version: &JavaMinecraftVersion,
    ) -> Result<(), WritingError> {
        (*self).write_metadata(writer, version)
    }

    fn write_metadata_for_type(
        &self,
        writer: &mut impl std::io::Write,
        version: &JavaMinecraftVersion,
        metadata_type: MetaDataType,
    ) -> Result<(), WritingError> {
        (*self).write_metadata_for_type(writer, version, metadata_type)
    }
}

/// Updates the "Data Tracker" values for an entity.
///
/// Entity Metadata (or `DataWatchers`) controls persistent visual states that
/// don't require a full packet to update, such as whether an entity is on fire,
/// crouching, glowing, or the custom name displayed above its head.
#[java_packet(SET_ENTITY_DATA)]
pub struct CSetEntityMetadata {
    /// The Entity ID of the entity whose metadata is being updated.
    pub entity_id: VarInt,
    /// A serialized collection of metadata entries.
    /// Ends with a terminal byte (0xFF).
    pub metadata: Box<[u8]>,
}

impl CSetEntityMetadata {
    #[must_use]
    pub const fn new(entity_id: VarInt, metadata: Box<[u8]>) -> Self {
        Self {
            entity_id,
            metadata,
        }
    }
}

impl ClientPacket for CSetEntityMetadata {
    fn write_packet_data(
        &self,
        mut write: impl Write,
        version: &JavaMinecraftVersion,
    ) -> Result<(), WritingError> {
        // 1. Entity ID
        if *version <= JavaMinecraftVersion::V_1_7_6 {
            write.write_i32_be(self.entity_id.0)?;
        } else {
            write.write_var_int(&self.entity_id)?;
        }

        write.write_slice(&self.metadata)
    }
}

impl<'a> crate::ServerPacket<'a> for CSetEntityMetadata {
    fn read(
        bytebuf: &mut &'a [u8],
        version: &JavaMinecraftVersion,
    ) -> Result<Self, crate::ser::ReadingError> {
        use crate::ser::{NetworkReadExt, NetworkReadSliceExt};
        let entity_id = if *version <= JavaMinecraftVersion::V_1_7_6 {
            VarInt(bytebuf.get_i32_be()?)
        } else {
            bytebuf.get_var_int()?
        };
        let metadata = bytebuf.read_remaining_slice_borrowed(usize::MAX)?;
        Ok(Self {
            entity_id,
            metadata: metadata.into(),
        })
    }
}

pub struct Metadata<T> {
    pub index: TrackedId,
    pub r#type: MetaDataType,
    pub value: T,
}

impl<T> Metadata<T> {
    pub const fn new(tracked: TrackedData, value: T) -> Self {
        Self {
            index: tracked.id,
            r#type: tracked.r#type,
            value,
        }
    }

    pub const fn new_raw(index: TrackedId, r#type: MetaDataType, value: T) -> Self {
        Self {
            index,
            r#type,
            value,
        }
    }

    pub fn write<W: std::io::Write>(
        &self,
        mut writer: W,
        version: &JavaMinecraftVersion,
    ) -> Result<(), WritingError>
    where
        T: MetadataSerializer,
    {
        let resolved_index = self.index.get(version);

        if resolved_index == 255 {
            return Ok(());
        }

        let remapped_type_id = self.r#type.id(*version);
        if remapped_type_id < 0 {
            // Metadata type does not exist in this protocol version.
            return Ok(());
        }

        writer.write_u8(resolved_index)?;
        writer.write_var_int(&VarInt(remapped_type_id))?;

        self.value
            .write_metadata_for_type(&mut writer, version, self.r#type)?;

        Ok(())
    }
}

impl MetadataSerializer for bool {
    fn write_metadata(
        &self,
        writer: &mut impl std::io::Write,
        _version: &JavaMinecraftVersion,
    ) -> Result<(), WritingError> {
        writer.write_bool(*self)
    }
}

impl MetadataSerializer for i8 {
    fn write_metadata(
        &self,
        writer: &mut impl std::io::Write,
        _version: &JavaMinecraftVersion,
    ) -> Result<(), WritingError> {
        writer.write_i8(*self)
    }
}

impl MetadataSerializer for u8 {
    fn write_metadata(
        &self,
        writer: &mut impl std::io::Write,
        _version: &JavaMinecraftVersion,
    ) -> Result<(), WritingError> {
        writer.write_u8(*self)
    }
}

impl MetadataSerializer for i16 {
    fn write_metadata(
        &self,
        writer: &mut impl std::io::Write,
        _version: &JavaMinecraftVersion,
    ) -> Result<(), WritingError> {
        writer.write_i16(*self)
    }
}

impl MetadataSerializer for u16 {
    fn write_metadata(
        &self,
        writer: &mut impl std::io::Write,
        _version: &JavaMinecraftVersion,
    ) -> Result<(), WritingError> {
        writer.write_u16(*self)
    }
}

impl MetadataSerializer for i32 {
    fn write_metadata(
        &self,
        writer: &mut impl std::io::Write,
        _version: &JavaMinecraftVersion,
    ) -> Result<(), WritingError> {
        writer.write_i32(*self)
    }
}

impl MetadataSerializer for i64 {
    fn write_metadata(
        &self,
        writer: &mut impl std::io::Write,
        _version: &JavaMinecraftVersion,
    ) -> Result<(), WritingError> {
        writer.write_i64(*self)
    }
}

impl MetadataSerializer for u32 {
    fn write_metadata(
        &self,
        writer: &mut impl std::io::Write,
        _version: &JavaMinecraftVersion,
    ) -> Result<(), WritingError> {
        writer.write_u32(*self)
    }
}

impl MetadataSerializer for f32 {
    fn write_metadata(
        &self,
        writer: &mut impl std::io::Write,
        _version: &JavaMinecraftVersion,
    ) -> Result<(), WritingError> {
        writer.write_f32(*self)
    }
}

impl MetadataSerializer for VarInt {
    fn write_metadata(
        &self,
        writer: &mut impl std::io::Write,
        _version: &JavaMinecraftVersion,
    ) -> Result<(), WritingError> {
        writer.write_var_int(self)
    }

    fn write_metadata_for_type(
        &self,
        writer: &mut impl std::io::Write,
        version: &JavaMinecraftVersion,
        metadata_type: MetaDataType,
    ) -> Result<(), WritingError> {
        let value = if metadata_type == MetaDataType::BLOCK_STATE {
            u16::try_from(self.0).map_or(*self, |state_id| {
                Self(i32::from(remap_block_state_for_version(state_id, *version)))
            })
        } else if metadata_type == MetaDataType::PARTICLE {
            particle_id_for_version(*self, *version)
        } else {
            *self
        };
        writer.write_var_int(&value)
    }
}

impl MetadataSerializer for String {
    fn write_metadata(
        &self,
        writer: &mut impl std::io::Write,
        _version: &JavaMinecraftVersion,
    ) -> Result<(), WritingError> {
        writer.write_string(self)
    }
}

impl MetadataSerializer for pumpkin_util::text::TextComponent {
    fn write_metadata(
        &self,
        writer: &mut impl std::io::Write,
        version: &JavaMinecraftVersion,
    ) -> Result<(), WritingError> {
        if *version < JavaMinecraftVersion::V_1_20_3 {
            let json = self.to_json_for_version(version);
            writer.write_string(&json)
        } else {
            writer.write_slice(&self.encode_for_version(version))
        }
    }
}

impl MetadataSerializer for Option<pumpkin_util::text::TextComponent> {
    fn write_metadata(
        &self,
        writer: &mut impl std::io::Write,
        version: &JavaMinecraftVersion,
    ) -> Result<(), WritingError> {
        if let Some(text) = self {
            writer.write_bool(true)?;
            text.write_metadata(writer, version)?;
        } else {
            writer.write_bool(false)?;
        }
        Ok(())
    }
}

impl MetadataSerializer for crate::codec::item_stack_seralizer::ItemStackSerializer<'_> {
    fn write_metadata(
        &self,
        writer: &mut impl std::io::Write,
        _version: &JavaMinecraftVersion,
    ) -> Result<(), WritingError> {
        self.write(writer)
    }

    fn write_metadata_for_type(
        &self,
        writer: &mut impl std::io::Write,
        version: &JavaMinecraftVersion,
        metadata_type: MetaDataType,
    ) -> Result<(), WritingError> {
        if metadata_type == MetaDataType::ITEM_STACK {
            self.write_with_version(writer, version)
        } else {
            self.write(writer)
        }
    }
}

impl MetadataSerializer for Option<String> {
    fn write_metadata(
        &self,
        writer: &mut impl std::io::Write,
        _version: &JavaMinecraftVersion,
    ) -> Result<(), WritingError> {
        if let Some(s) = self {
            writer.write_bool(true)?;
            writer.write_string(s)?;
        } else {
            writer.write_bool(false)?;
        }
        Ok(())
    }
}

impl MetadataSerializer for pumpkin_util::math::position::BlockPos {
    fn write_metadata(
        &self,
        writer: &mut impl std::io::Write,
        version: &JavaMinecraftVersion,
    ) -> Result<(), WritingError> {
        writer.write_block_pos(self, version)
    }
}

impl MetadataSerializer for Option<pumpkin_util::math::position::BlockPos> {
    fn write_metadata(
        &self,
        writer: &mut impl std::io::Write,
        version: &JavaMinecraftVersion,
    ) -> Result<(), WritingError> {
        if let Some(pos) = self {
            writer.write_bool(true)?;
            writer.write_block_pos(pos, version)?;
        } else {
            writer.write_bool(false)?;
        }
        Ok(())
    }
}

impl MetadataSerializer for crate::codec::optional_int::OptionalInt {
    fn write_metadata(
        &self,
        writer: &mut impl std::io::Write,
        _version: &JavaMinecraftVersion,
    ) -> Result<(), WritingError> {
        let val = self.0.map_or(0, |id| id + 1);
        writer.write_var_int(&VarInt(val))
    }
}

impl MetadataSerializer for uuid::Uuid {
    fn write_metadata(
        &self,
        writer: &mut impl std::io::Write,
        _version: &JavaMinecraftVersion,
    ) -> Result<(), WritingError> {
        writer.write_uuid(self)
    }
}

impl MetadataSerializer for Option<uuid::Uuid> {
    fn write_metadata(
        &self,
        writer: &mut impl std::io::Write,
        _version: &JavaMinecraftVersion,
    ) -> Result<(), WritingError> {
        if let Some(uuid) = self {
            writer.write_bool(true)?;
            writer.write_uuid(uuid)?;
        } else {
            writer.write_bool(false)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Cursor, Read};

    use pumpkin_data::{
        block_state_remap::remap_block_state_for_version, item::Item, item_stack::ItemStack,
        meta_data_type::MetaDataType, particle::Particle,
    };
    use pumpkin_util::version::JavaMinecraftVersion;

    use crate::{VarInt, codec::item_stack_seralizer::ItemStackSerializer, ser::NetworkWriteExt};

    use super::{Metadata, MetadataSerializer, particle_id_for_version};

    #[test]
    fn long_metadata_uses_the_protocol_big_endian_encoding() {
        let mut encoded = Vec::new();
        0x0102_0304_0506_0708_i64
            .write_metadata(&mut encoded, &JavaMinecraftVersion::V_1_21_5)
            .unwrap();
        assert_eq!(encoded, 0x0102_0304_0506_0708_i64.to_be_bytes());
    }

    struct ParticleMetadata {
        particle_id: VarInt,
        data: [u8; 4],
    }

    impl MetadataSerializer for ParticleMetadata {
        fn write_metadata(
            &self,
            writer: &mut impl std::io::Write,
            _version: &JavaMinecraftVersion,
        ) -> Result<(), crate::WritingError> {
            writer.write_var_int(&self.particle_id)?;
            writer.write_slice(&self.data)
        }

        fn write_metadata_for_type(
            &self,
            writer: &mut impl std::io::Write,
            version: &JavaMinecraftVersion,
            metadata_type: MetaDataType,
        ) -> Result<(), crate::WritingError> {
            if metadata_type == MetaDataType::PARTICLE {
                writer.write_var_int(&particle_id_for_version(self.particle_id, *version))?;
                writer.write_slice(&self.data)
            } else {
                self.write_metadata(writer, version)
            }
        }
    }

    fn encoded_particle(version: JavaMinecraftVersion) -> (VarInt, Vec<u8>) {
        let particle_data = [0x12, 0x34, 0x56, 0x78];
        let metadata = Metadata::new(
            pumpkin_data::tracked_data::area_effect_cloud::DATA_PARTICLE,
            ParticleMetadata {
                particle_id: VarInt(Particle::ExplosionEmitter as i32),
                data: particle_data,
            },
        );
        let mut bytes = Vec::new();
        metadata.write(&mut bytes, &version).unwrap();

        assert_eq!(
            bytes[0],
            pumpkin_data::tracked_data::area_effect_cloud::DATA_PARTICLE.get(&version)
        );
        assert_eq!(bytes[1], MetaDataType::PARTICLE.id(version) as u8);

        let mut cursor = Cursor::new(&bytes[2..]);
        let particle_id = VarInt::decode(&mut cursor).unwrap();
        let mut remainder = Vec::new();
        cursor.read_to_end(&mut remainder).unwrap();
        (particle_id, remainder)
    }

    #[test]
    fn block_state_metadata_remaps_without_temporary_reencoding() {
        let version = JavaMinecraftVersion::V_1_20;
        let state = 1_000u16;
        let mut bytes = Vec::new();
        VarInt(i32::from(state))
            .write_metadata_for_type(&mut bytes, &version, MetaDataType::BLOCK_STATE)
            .unwrap();

        let mut cursor = Cursor::new(bytes);
        let remapped_state = VarInt::decode(&mut cursor).unwrap();
        assert_eq!(
            remapped_state,
            VarInt(i32::from(remap_block_state_for_version(state, version)))
        );
    }

    #[test]
    fn item_metadata_writes_the_versioned_serializer_directly() {
        let version = JavaMinecraftVersion::V_1_20;
        let serializer = ItemStackSerializer::from(ItemStack::new(1, &Item::MACE));
        let mut actual = Vec::new();
        serializer
            .write_metadata_for_type(&mut actual, &version, MetaDataType::ITEM_STACK)
            .unwrap();

        let mut expected = Vec::new();
        serializer
            .write_with_version(&mut expected, &version)
            .unwrap();
        assert_eq!(actual, expected);
    }

    #[test]
    fn particle_metadata_id_remaps_for_1_21_11() {
        let (particle_id, data) = encoded_particle(JavaMinecraftVersion::V_1_21_11);

        assert_eq!(particle_id, VarInt(22));
        assert_eq!(data, [0x12, 0x34, 0x56, 0x78]);
    }

    #[test]
    fn particle_metadata_id_stays_latest_for_26_2() {
        let (particle_id, data) = encoded_particle(JavaMinecraftVersion::V_26_2);

        assert_eq!(particle_id, VarInt(29));
        assert_eq!(data, [0x12, 0x34, 0x56, 0x78]);
    }
}
