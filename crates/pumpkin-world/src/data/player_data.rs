use pumpkin_nbt::compound::NbtCompound;
use std::fs::{File, create_dir_all};
use std::io;
use std::path::PathBuf;
use tracing::{debug, error};
use uuid::Uuid;

use crate::persistence::atomic_write;

/// Manages the storage and retrieval of player data from disk and memory cache.
///
/// This struct provides functions to load and save player data to/from NBT files,
/// with a memory cache to handle player disconnections temporarily.
pub struct PlayerDataStorage {
    /// Path to the directory where player data is stored
    data_path: PathBuf,
    /// Whether player data saving is enabled
    save_enabled: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum PlayerDataError {
    #[error("IO error: {0}")]
    Io(#[from] io::Error),
    #[error("NBT error: {0}")]
    Nbt(String),
}

impl PlayerDataStorage {
    /// Creates a new `PlayerDataStorage` with the specified data path and cache expiration time.
    pub fn new(data_path: impl Into<PathBuf>, enabled: bool) -> Self {
        let path = data_path.into();
        if !path.exists()
            && let Err(e) = create_dir_all(&path)
        {
            error!(
                "Failed to create player data directory at {}: {e}",
                path.display()
            );
        }

        Self {
            data_path: path,
            save_enabled: enabled,
        }
    }

    #[must_use]
    pub const fn get_data_path(&self) -> &PathBuf {
        &self.data_path
    }

    #[must_use]
    pub const fn is_save_enabled(&self) -> bool {
        self.save_enabled
    }

    pub const fn set_save_enabled(&mut self, enabled: bool) {
        self.save_enabled = enabled;
    }

    /// Returns the path for a player's data file based on their UUID.
    #[must_use]
    pub fn get_player_data_path(&self, uuid: &Uuid) -> PathBuf {
        self.get_data_path().join(format!("{uuid}.dat"))
    }

    /// Loads player data from NBT file or cache.
    ///
    /// This function first checks if player data exists in the cache.
    /// If not, it attempts to load the data from a .dat file on disk.
    ///
    /// # Arguments
    ///
    /// * `uuid` - The UUID of the player to load data for.
    ///
    /// # Returns
    ///
    /// A Result containing either the player's NBT data or an error.
    pub fn load_player_data(&self, uuid: &Uuid) -> Result<(bool, NbtCompound), PlayerDataError> {
        // If player data saving is disabled, return empty data
        if !self.is_save_enabled() {
            return Ok((false, NbtCompound::new()));
        }

        // If not in cache, load from disk
        let path = self.get_player_data_path(uuid);
        if !path.exists() {
            debug!("No player data file found for {uuid}");
            return Ok((false, NbtCompound::new()));
        }

        let file = match File::open(&path) {
            Ok(file) => file,
            Err(e) => {
                error!("Failed to open player data file for {uuid}: {e}");
                return Err(PlayerDataError::Io(e));
            }
        };

        match pumpkin_nbt::nbt_compress::read_gzip_compound_tag(file) {
            Ok(mut nbt) => {
                crate::world_info::schema::migrate_persistent_root(
                    crate::world_info::schema::PersistentRootSchema::Player,
                    &mut nbt,
                )
                .map_err(|error| PlayerDataError::Nbt(error.to_string()))?;
                debug!("Loaded player data for {uuid} from disk");
                Ok((true, nbt))
            }
            Err(e) => {
                error!("Failed to read player data for {uuid}: {e}");
                Err(PlayerDataError::Nbt(e.to_string()))
            }
        }
    }

    /// Saves player data to NBT file and updates cache.
    ///
    /// This function saves the player's data to a .dat file on disk and also
    /// updates the in-memory cache with the latest data.
    ///
    /// # Arguments
    ///
    /// * `uuid` - The UUID of the player to save data for.
    /// * `data` - The NBT compound data to save.
    ///
    /// # Returns
    ///
    /// A Result indicating success or the error that occurred.
    pub fn save_player_data(
        &self,
        uuid: &Uuid,
        mut data: NbtCompound,
    ) -> Result<(), PlayerDataError> {
        // Skip saving if disabled in config
        if !self.is_save_enabled() {
            return Ok(());
        }

        let path = self.get_player_data_path(uuid);

        // Ensure parent directory exists
        if let Some(parent) = path.parent()
            && let Err(e) = create_dir_all(parent)
        {
            error!("Failed to create player data directory for {uuid}: {e}");
            return Err(PlayerDataError::Io(e));
        }

        data.put_int(
            "DataVersion",
            crate::world_info::MAXIMUM_SUPPORTED_WORLD_DATA_VERSION,
        );
        atomic_write(&path, |file| {
            pumpkin_nbt::nbt_compress::write_gzip_compound_tag(data, file)
                .map_err(|error| PlayerDataError::Nbt(error.to_string()))
        })?;
        debug!("Saved player data for {uuid} to disk");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world_info::{
        MAXIMUM_SUPPORTED_WORLD_DATA_VERSION, MINIMUM_SUPPORTED_WORLD_DATA_VERSION,
    };
    use tempfile::tempdir;

    #[test]
    fn versioned_player_disk_restart_migrates_known_and_preserves_unknown() {
        let directory = tempdir().unwrap();
        let storage = PlayerDataStorage::new(directory.path(), true);
        let uuid = Uuid::new_v4();
        let path = storage.get_player_data_path(&uuid);
        let mut legacy = NbtCompound::new();
        legacy.put_int("DataVersion", MINIMUM_SUPPORTED_WORLD_DATA_VERSION);
        legacy.put_byte("playerGameType", 1);
        legacy.put_int("SpawnX", 4);
        legacy.put_int("SpawnY", 70);
        legacy.put_int("SpawnZ", -8);
        legacy.put_string("plugin:marker", "kept");
        pumpkin_nbt::nbt_compress::write_gzip_compound_tag(legacy, File::create(&path).unwrap())
            .unwrap();

        let (existed, migrated) = storage.load_player_data(&uuid).unwrap();
        assert!(existed);
        assert_eq!(
            migrated.get_int("DataVersion"),
            Some(MAXIMUM_SUPPORTED_WORLD_DATA_VERSION)
        );
        assert_eq!(migrated.get_int("playerGameType"), Some(1));
        assert_eq!(migrated.get_string("plugin:marker"), Some("kept"));
        assert_eq!(
            migrated
                .get_compound("respawn")
                .and_then(|respawn| respawn.get_int_array("pos")),
            Some([4, 70, -8].as_slice())
        );

        storage.save_player_data(&uuid, migrated).unwrap();
        let (_, restarted) = storage.load_player_data(&uuid).unwrap();
        assert_eq!(restarted.get_string("plugin:marker"), Some("kept"));
        assert_eq!(
            restarted.get_int("DataVersion"),
            Some(MAXIMUM_SUPPORTED_WORLD_DATA_VERSION)
        );
    }

    #[test]
    fn player_disk_load_rejects_future_version() {
        let directory = tempdir().unwrap();
        let storage = PlayerDataStorage::new(directory.path(), true);
        let uuid = Uuid::new_v4();
        let mut future = NbtCompound::new();
        future.put_int("DataVersion", MAXIMUM_SUPPORTED_WORLD_DATA_VERSION + 1);
        pumpkin_nbt::nbt_compress::write_gzip_compound_tag(
            future,
            File::create(storage.get_player_data_path(&uuid)).unwrap(),
        )
        .unwrap();

        assert!(matches!(
            storage.load_player_data(&uuid),
            Err(PlayerDataError::Nbt(message)) if message.contains("Unsupported player DataVersion")
        ));
    }
}
