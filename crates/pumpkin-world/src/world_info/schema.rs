use pumpkin_nbt::{compound::NbtCompound, tag::NbtTag};

use super::{MAXIMUM_SUPPORTED_WORLD_DATA_VERSION, MINIMUM_SUPPORTED_WORLD_DATA_VERSION};

pub const MINIMUM_SUPPORTED_POI_DATA_VERSION: i32 = 3955;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PersistentRootSchema {
    TerrainChunk,
    EntityChunk,
    Player,
    Poi,
}

impl PersistentRootSchema {
    const fn name(self) -> &'static str {
        match self {
            Self::TerrainChunk => "terrain chunk",
            Self::EntityChunk => "entity chunk",
            Self::Player => "player",
            Self::Poi => "POI",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MigrationOutcome {
    pub source_version: i32,
    pub target_version: i32,
    pub inferred_missing_version: bool,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SchemaMigrationError {
    #[error("Unsupported {schema} DataVersion {version}; supported range is {minimum}..={maximum}")]
    UnsupportedVersion {
        schema: &'static str,
        version: i32,
        minimum: i32,
        maximum: i32,
    },
    #[error("No migration is registered for {schema}")]
    MissingRegistryEntry { schema: &'static str },
}

struct RootMigration {
    schema: PersistentRootSchema,
    minimum_version: i32,
    transform: fn(&mut NbtCompound),
}

const ROOT_MIGRATIONS: &[RootMigration] = &[
    RootMigration {
        schema: PersistentRootSchema::TerrainChunk,
        minimum_version: MINIMUM_SUPPORTED_WORLD_DATA_VERSION,
        transform: identity_migration,
    },
    RootMigration {
        schema: PersistentRootSchema::EntityChunk,
        minimum_version: MINIMUM_SUPPORTED_WORLD_DATA_VERSION,
        transform: migrate_entity_chunk,
    },
    RootMigration {
        schema: PersistentRootSchema::Player,
        minimum_version: MINIMUM_SUPPORTED_WORLD_DATA_VERSION,
        transform: migrate_player,
    },
    RootMigration {
        schema: PersistentRootSchema::Poi,
        minimum_version: MINIMUM_SUPPORTED_POI_DATA_VERSION,
        transform: identity_migration,
    },
];

const fn identity_migration(_root: &mut NbtCompound) {}

fn migrate_entity_chunk(root: &mut NbtCompound) {
    if root.get("Position").is_none()
        && let (Some(x), Some(z)) = (root.get_int("Position-X"), root.get_int("Position-Z"))
    {
        root.put("Position", NbtTag::IntArray(vec![x, z]));
    }
    root.child_tags.remove("Position-X");
    root.child_tags.remove("Position-Z");
}

fn migrate_player(root: &mut NbtCompound) {
    for key in ["playerGameType", "previousPlayerGameType"] {
        if root.get_int(key).is_none()
            && let Some(value) = root.get_byte(key)
        {
            root.put_int(key, i32::from(value));
        }
    }

    if root.get_compound("respawn").is_none()
        && let (Some(x), Some(y), Some(z)) = (
            root.get_int("SpawnX"),
            root.get_int("SpawnY"),
            root.get_int("SpawnZ"),
        )
    {
        let mut respawn = NbtCompound::new();
        respawn.put_string(
            "dimension",
            root.get_string("SpawnDimension")
                .unwrap_or("minecraft:overworld"),
        );
        respawn.put("pos", NbtTag::IntArray(vec![x, y, z]));
        respawn.put_float("angle", 0.0);
        respawn.put_bool("forced", root.get_bool("SpawnForced").unwrap_or(false));
        root.put_compound("respawn", respawn);
    }
}

pub fn migrate_persistent_root(
    schema: PersistentRootSchema,
    root: &mut NbtCompound,
) -> Result<MigrationOutcome, SchemaMigrationError> {
    let migration = ROOT_MIGRATIONS
        .iter()
        .find(|migration| migration.schema == schema)
        .ok_or(SchemaMigrationError::MissingRegistryEntry {
            schema: schema.name(),
        })?;
    let stored_version = root.get_int("DataVersion");
    let source_version = stored_version.unwrap_or(migration.minimum_version);
    if !(migration.minimum_version..=MAXIMUM_SUPPORTED_WORLD_DATA_VERSION).contains(&source_version)
    {
        return Err(SchemaMigrationError::UnsupportedVersion {
            schema: schema.name(),
            version: source_version,
            minimum: migration.minimum_version,
            maximum: MAXIMUM_SUPPORTED_WORLD_DATA_VERSION,
        });
    }

    (migration.transform)(root);
    root.put_int("DataVersion", MAXIMUM_SUPPORTED_WORLD_DATA_VERSION);
    Ok(MigrationOutcome {
        source_version,
        target_version: MAXIMUM_SUPPORTED_WORLD_DATA_VERSION,
        inferred_missing_version: stored_version.is_none(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn player_migration_is_versioned_and_preserves_unknown_fields() {
        let mut root = NbtCompound::new();
        root.put_int("DataVersion", MINIMUM_SUPPORTED_WORLD_DATA_VERSION);
        root.put_byte("playerGameType", 1);
        root.put_int("SpawnX", 4);
        root.put_int("SpawnY", 70);
        root.put_int("SpawnZ", -8);
        root.put_string("plugin:marker", "kept");

        let outcome = migrate_persistent_root(PersistentRootSchema::Player, &mut root).unwrap();

        assert_eq!(outcome.source_version, MINIMUM_SUPPORTED_WORLD_DATA_VERSION);
        assert_eq!(
            root.get_int("DataVersion"),
            Some(MAXIMUM_SUPPORTED_WORLD_DATA_VERSION)
        );
        assert_eq!(root.get_int("playerGameType"), Some(1));
        assert_eq!(
            root.get_compound("respawn")
                .and_then(|respawn| respawn.get_int_array("pos")),
            Some([4, 70, -8].as_slice())
        );
        assert_eq!(root.get_string("plugin:marker"), Some("kept"));
    }

    #[test]
    fn schema_registry_infers_missing_and_rejects_out_of_range_versions() {
        let mut missing = NbtCompound::new();
        let outcome =
            migrate_persistent_root(PersistentRootSchema::EntityChunk, &mut missing).unwrap();
        assert!(outcome.inferred_missing_version);
        assert_eq!(outcome.source_version, MINIMUM_SUPPORTED_WORLD_DATA_VERSION);

        for version in [
            MINIMUM_SUPPORTED_WORLD_DATA_VERSION - 1,
            MAXIMUM_SUPPORTED_WORLD_DATA_VERSION + 1,
        ] {
            let mut root = NbtCompound::new();
            root.put_int("DataVersion", version);
            assert!(migrate_persistent_root(PersistentRootSchema::Player, &mut root).is_err());
        }
    }
}
