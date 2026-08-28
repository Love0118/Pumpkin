use std::collections::BTreeMap;
use std::sync::RwLock;

use pumpkin_data::meta_data_type::MetaDataType;
use pumpkin_data::tracked_data::TrackedId;
use pumpkin_protocol::bedrock::client::set_actor_data::EntityMetadata;
use pumpkin_protocol::codec::var_int::VarInt;
use pumpkin_protocol::java::client::play::{Metadata, MetadataSerializer};
use pumpkin_protocol::ser::{NetworkWriteExt, WritingError};
use pumpkin_util::version::JavaMinecraftVersion;

trait ErasedMetadataValue: Send + Sync {
    fn write(
        &self,
        writer: &mut Vec<u8>,
        version: JavaMinecraftVersion,
        metadata_type: MetaDataType,
    ) -> Result<(), WritingError>;
}

impl<T> ErasedMetadataValue for T
where
    T: MetadataSerializer + Send + Sync,
{
    fn write(
        &self,
        writer: &mut Vec<u8>,
        version: JavaMinecraftVersion,
        metadata_type: MetaDataType,
    ) -> Result<(), WritingError> {
        self.write_metadata_for_type(writer, &version, metadata_type)
    }
}

struct StoredJavaMetadata {
    index: TrackedId,
    metadata_type: MetaDataType,
    value: Box<dyn ErasedMetadataValue>,
}

impl StoredJavaMetadata {
    fn write(
        &self,
        writer: &mut Vec<u8>,
        version: JavaMinecraftVersion,
    ) -> Result<(), WritingError> {
        let index = self.index.get(&version);
        if index == 255 {
            return Ok(());
        }
        let metadata_type = self.metadata_type.id(version);
        if metadata_type < 0 {
            return Ok(());
        }

        writer.write_u8(index)?;
        writer.write_var_int(&VarInt(metadata_type))?;
        self.value.write(writer, version, self.metadata_type)
    }
}

#[derive(Default)]
pub struct TrackedMetadataState {
    java: RwLock<BTreeMap<u8, StoredJavaMetadata>>,
    bedrock: RwLock<EntityMetadata>,
}

impl TrackedMetadataState {
    pub fn apply_java<T>(&self, metadata: &[Metadata<T>])
    where
        T: MetadataSerializer + Clone + Send + Sync + 'static,
    {
        let mut java = self
            .java
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for value in metadata {
            java.insert(
                value.index.v26_2,
                StoredJavaMetadata {
                    index: value.index,
                    metadata_type: value.r#type,
                    value: Box::new(value.value.clone()),
                },
            );
        }
    }

    pub fn encode_java_delta<T>(
        metadata: &[Metadata<T>],
        version: JavaMinecraftVersion,
    ) -> Result<Box<[u8]>, WritingError>
    where
        T: MetadataSerializer,
    {
        let mut delta = Vec::new();
        for value in metadata {
            value.write(&mut delta, &version)?;
        }
        if delta.is_empty() {
            return Ok(Box::default());
        }
        delta.push(255);
        Ok(delta.into_boxed_slice())
    }

    pub fn apply_bedrock(&self, metadata: &EntityMetadata) {
        self.bedrock
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .0
            .extend(metadata.0.iter().map(|(key, value)| (*key, value.clone())));
    }

    pub fn java_snapshot(&self, version: JavaMinecraftVersion) -> Option<Box<[u8]>> {
        let java = self
            .java
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if java.is_empty() {
            return None;
        }

        let mut snapshot = Vec::new();
        for value in java.values() {
            if value.write(&mut snapshot, version).is_err() {
                return None;
            }
        }
        if snapshot.is_empty() {
            return None;
        }
        snapshot.push(255);
        Some(snapshot.into_boxed_slice())
    }

    pub fn bedrock_snapshot(&self) -> EntityMetadata {
        EntityMetadata(
            self.bedrock
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .0
                .iter()
                .map(|(key, value)| (*key, value.clone()))
                .collect(),
        )
    }
}

#[cfg(test)]
mod tests {
    use pumpkin_data::tracked_data;
    use pumpkin_protocol::bedrock::client::set_actor_data::{EntityMetadata, MetadataValue};
    use pumpkin_protocol::codec::var_int::VarInt;
    use pumpkin_protocol::java::client::play::Metadata;
    use pumpkin_util::version::JavaMinecraftVersion;

    use super::TrackedMetadataState;

    #[test]
    fn java_snapshot_replaces_an_accessor_instead_of_appending_duplicates() {
        let state = TrackedMetadataState::default();
        state.apply_java(&[Metadata::new(
            tracked_data::entity::DATA_AIR_SUPPLY_ID,
            VarInt(100),
        )]);
        state.apply_java(&[Metadata::new(
            tracked_data::entity::DATA_AIR_SUPPLY_ID,
            VarInt(200),
        )]);

        let snapshot = state
            .java_snapshot(JavaMinecraftVersion::V_26_2)
            .expect("the accessor was stored");
        let delta = TrackedMetadataState::encode_java_delta(
            &[Metadata::new(
                tracked_data::entity::DATA_AIR_SUPPLY_ID,
                VarInt(200),
            )],
            JavaMinecraftVersion::V_26_2,
        )
        .expect("the delta encodes");
        assert_eq!(snapshot, delta);
    }

    #[test]
    fn bedrock_snapshot_replaces_values_by_metadata_key() {
        let state = TrackedMetadataState::default();
        let mut first = EntityMetadata::new();
        first.set(7, MetadataValue::Int(10));
        state.apply_bedrock(&first);
        let mut second = EntityMetadata::new();
        second.set(7, MetadataValue::Int(20));
        state.apply_bedrock(&second);

        assert!(matches!(
            state.bedrock_snapshot().0.get(&7),
            Some(MetadataValue::Int(20))
        ));
    }
}
