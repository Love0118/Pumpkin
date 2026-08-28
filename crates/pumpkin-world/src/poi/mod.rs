use std::collections::{BTreeMap, HashMap};
use std::io::{Cursor, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;
use tracing::{info, warn};

use flate2::Compression;
use flate2::read::ZlibDecoder;
use flate2::write::ZlibEncoder;
use pumpkin_util::math::position::BlockPos;
use pumpkin_util::math::vector3::Vector3;
use serde::{Deserialize, Serialize};

/// POI type identifier for nether portals
pub const POI_TYPE_NETHER_PORTAL: &str = "minecraft:nether_portal";

/// MCA format constants
const SECTOR_SIZE: usize = 4096;
const REGION_SIZE: usize = 32;
const CHUNK_COUNT: usize = REGION_SIZE * REGION_SIZE;
const HEADER_SIZE: usize = SECTOR_SIZE * 2; // Location table + timestamp table
const MAX_DECOMPRESSED_POI_CHUNK_BYTES: usize = 64 * 1024 * 1024;
const MAX_POI_RESIDUAL_ENTRIES: usize = 256;
const MAX_POI_RESIDUAL_BYTES: usize = 1024 * 1024;

/// Compression type for MCA format
const COMPRESSION_ZLIB: u8 = 2;

// Legacy Pumpkin POI files used 3955. The POI schema is identity-migrated on
// read and every successful save writes the current world data version.
const DATA_VERSION: i32 = crate::world_info::MAXIMUM_SUPPORTED_WORLD_DATA_VERSION;

/// A single Point of Interest entry (serializable)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PoiEntry {
    pub x: i32,
    pub y: i32,
    pub z: i32,
    #[serde(rename = "type")]
    pub poi_type: String,
    pub free_tickets: i32,
}

impl PoiEntry {
    #[must_use]
    pub fn new_portal(pos: BlockPos) -> Self {
        Self {
            x: pos.0.x,
            y: pos.0.y,
            z: pos.0.z,
            poi_type: POI_TYPE_NETHER_PORTAL.to_string(),
            free_tickets: 0,
        }
    }

    #[must_use]
    pub const fn pos(&self) -> BlockPos {
        BlockPos(Vector3::new(self.x, self.y, self.z))
    }
}

/// POI section data (serializable) - vanilla format
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct PoiSectionData {
    #[serde(default)]
    pub valid: i8,
    #[serde(default)]
    pub records: Vec<PoiEntry>,
}

/// POI chunk data (serializable) - vanilla format
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct PoiChunkData {
    pub data_version: i32,
    /// Sections keyed by Y section coordinate (e.g., "-1", "0", "1", "4")
    pub sections: HashMap<String, PoiSectionData>,
    #[serde(skip)]
    residual_nbt: pumpkin_nbt::compound::NbtCompound,
}

/// POI data for a single region (32x32 chunks) using MCA format
#[derive(Debug, Default, Clone)]
pub struct PoiRegion {
    /// Entries indexed by position
    entries: HashMap<(i32, i32, i32), PoiEntry>,
    /// Track which chunks are dirty
    dirty_chunks: std::collections::HashSet<(i32, i32)>,
    /// Unknown top-level chunk tags retained across load/save cycles.
    residual_roots: HashMap<usize, pumpkin_nbt::compound::NbtCompound>,
    mutation_generation: u64,
    persisted_generation: u64,
}

impl PoiRegion {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    const fn pos_key(pos: &BlockPos) -> (i32, i32, i32) {
        (pos.0.x, pos.0.y, pos.0.z)
    }

    /// Get chunk index in MCA file (0-1023)
    const fn chunk_index(chunk_x: i32, chunk_z: i32) -> usize {
        let local_x = chunk_x & 31;
        let local_z = chunk_z & 31;
        ((local_z << 5) | local_x) as usize
    }

    /// Returns section key as just the Y section coordinate (like vanilla)
    fn section_key(pos: &BlockPos) -> String {
        let section_y = pos.0.y >> 4;
        section_y.to_string()
    }

    pub fn add(&mut self, entry: PoiEntry) {
        let chunk_x = entry.x >> 4;
        let chunk_z = entry.z >> 4;
        self.dirty_chunks.insert((chunk_x, chunk_z));
        let key = (entry.x, entry.y, entry.z);
        self.entries.insert(key, entry);
        self.mutation_generation = self.mutation_generation.saturating_add(1);
    }

    pub fn remove(&mut self, pos: &BlockPos) -> bool {
        let key = Self::pos_key(pos);
        if self.entries.remove(&key).is_some() {
            let chunk_x = pos.0.x >> 4;
            let chunk_z = pos.0.z >> 4;
            self.dirty_chunks.insert((chunk_x, chunk_z));
            self.mutation_generation = self.mutation_generation.saturating_add(1);
            return true;
        }
        false
    }

    #[must_use]
    pub fn get_all(&self) -> Vec<&PoiEntry> {
        self.entries.values().collect()
    }

    #[must_use]
    pub const fn is_dirty(&self) -> bool {
        self.mutation_generation != self.persisted_generation
    }

    #[must_use]
    pub const fn dirty_generation(&self) -> u64 {
        self.mutation_generation
    }

    pub fn mark_persisted(&mut self, generation: u64) {
        self.persisted_generation = self.persisted_generation.max(generation);
        if self.persisted_generation == self.mutation_generation {
            self.dirty_chunks.clear();
        }
    }

    /// Group entries by chunk, then create chunk NBT data
    fn get_chunk_data(&self, chunk_x: i32, chunk_z: i32) -> Option<PoiChunkData> {
        let mut sections: HashMap<String, PoiSectionData> = HashMap::new();

        for entry in self.entries.values() {
            let entry_chunk_x = entry.x >> 4;
            let entry_chunk_z = entry.z >> 4;

            if entry_chunk_x != chunk_x || entry_chunk_z != chunk_z {
                continue;
            }

            let section_key = Self::section_key(&entry.pos());
            let section = sections
                .entry(section_key)
                .or_insert_with(|| PoiSectionData {
                    valid: 1,
                    records: Vec::new(),
                });
            section.records.push(entry.clone());
        }

        if sections.is_empty() {
            None
        } else {
            Some(PoiChunkData {
                data_version: DATA_VERSION,
                sections,
                residual_nbt: self
                    .residual_roots
                    .get(&Self::chunk_index(chunk_x, chunk_z))
                    .cloned()
                    .unwrap_or_default(),
            })
        }
    }

    /// Compress chunk data to bytes
    fn compress_chunk_data(chunk_data: &PoiChunkData) -> std::io::Result<Vec<u8>> {
        let snapshot_started = Instant::now();
        let mut root = chunk_data.residual_nbt.clone();
        root.put_int("DataVersion", chunk_data.data_version);

        let mut sections_comp = pumpkin_nbt::compound::NbtCompound::new();
        for (sec_key, sec_data) in &chunk_data.sections {
            let mut sec_comp = pumpkin_nbt::compound::NbtCompound::new();
            sec_comp.put_byte("Valid", sec_data.valid);
            let mut rec_list = Vec::new();
            for rec in &sec_data.records {
                let mut rec_comp = pumpkin_nbt::compound::NbtCompound::new();
                rec_comp.put_int("x", rec.x);
                rec_comp.put_int("y", rec.y);
                rec_comp.put_int("z", rec.z);
                rec_comp.put_string("type", rec.poi_type.clone());
                rec_comp.put_int("free_tickets", rec.free_tickets);
                rec_list.push(pumpkin_nbt::tag::NbtTag::Compound(rec_comp));
            }
            sec_comp.put_list("Records", rec_list);
            sections_comp.put_compound(sec_key, sec_comp);
        }
        root.put_compound("Sections", sections_comp);
        crate::serialization_metrics::record_duration(
            crate::serialization_metrics::SerializationStage::Snapshot,
            snapshot_started.elapsed(),
        );

        let compress_started = Instant::now();
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        pumpkin_nbt::Nbt::from(root)
            .write_to_writer(&mut encoder)
            .map_err(std::io::Error::other)?;
        let compressed = encoder.finish()?;
        crate::serialization_metrics::record_duration(
            crate::serialization_metrics::SerializationStage::Compress,
            compress_started.elapsed(),
        );
        crate::serialization_metrics::record_compressed_bytes(compressed.len());
        Ok(compressed)
    }

    /// Decompress chunk data from bytes
    fn decompress_chunk_data(compressed: &[u8]) -> std::io::Result<PoiChunkData> {
        Self::decompress_chunk_data_with_limit(compressed, MAX_DECOMPRESSED_POI_CHUNK_BYTES)
    }

    fn decompress_chunk_data_with_limit(
        compressed: &[u8],
        max_decompressed_bytes: usize,
    ) -> std::io::Result<PoiChunkData> {
        let output_limit =
            crate::chunk::io::decompression_output_limit(compressed.len(), max_decompressed_bytes)?;
        let decoder = ZlibDecoder::new(compressed);
        let mut limited = decoder.take(output_limit.saturating_add(1) as u64);
        let mut uncompressed = Vec::new();
        limited.read_to_end(&mut uncompressed)?;
        if uncompressed.len() > output_limit {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "POI chunk exceeds decompressed byte limit",
            ));
        }

        let mut cursor = Cursor::new(uncompressed);
        let mut reader = pumpkin_nbt::deserializer::NbtReadHelperJava::new(
            pumpkin_nbt::deserializer::NbtStreamReader(&mut cursor),
        );
        let nbt = pumpkin_nbt::Nbt::read(&mut reader)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;

        let mut root = nbt.root_tag;
        crate::world_info::schema::migrate_persistent_root(
            crate::world_info::schema::PersistentRootSchema::Poi,
            &mut root,
        )
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
        let residual_nbt = crate::persistence::bounded_residual_nbt(
            &root,
            ["DataVersion", "Sections"],
            MAX_POI_RESIDUAL_ENTRIES,
            MAX_POI_RESIDUAL_BYTES,
        )
        .map_err(|message| std::io::Error::new(std::io::ErrorKind::InvalidData, message))?;
        let data_version = root.get_int("DataVersion").unwrap_or(DATA_VERSION);
        let mut sections = HashMap::new();

        if let Some(sec_tag) = root.get_compound("Sections") {
            for (sec_key, tag) in &sec_tag.child_tags {
                if let pumpkin_nbt::tag::NbtTag::Compound(sec_comp) = tag {
                    let valid = sec_comp.get_byte("Valid").unwrap_or(1);
                    let mut records = Vec::new();
                    if let Some(pumpkin_nbt::tag::NbtTag::List(rec_list)) = sec_comp.get("Records")
                    {
                        for rec_t in rec_list {
                            if let pumpkin_nbt::tag::NbtTag::Compound(rc) = rec_t {
                                records.push(PoiEntry {
                                    x: rc.get_int("x").unwrap_or(0),
                                    y: rc.get_int("y").unwrap_or(0),
                                    z: rc.get_int("z").unwrap_or(0),
                                    poi_type: rc
                                        .get_string("type")
                                        .unwrap_or(POI_TYPE_NETHER_PORTAL)
                                        .to_string(),
                                    free_tickets: rc.get_int("free_tickets").unwrap_or(0),
                                });
                            }
                        }
                    }
                    sections.insert(sec_key.to_string(), PoiSectionData { valid, records });
                }
            }
        }

        Ok(PoiChunkData {
            data_version,
            sections,
            residual_nbt,
        })
    }

    pub fn save(&mut self, path: &Path) -> std::io::Result<()> {
        if !self.is_dirty() {
            return Ok(());
        }

        if self.entries.is_empty() {
            // Don't save empty regions, delete the file if it exists
            if path.exists() {
                std::fs::remove_file(path)?;
            }
            self.mark_persisted(self.mutation_generation);
            return Ok(());
        }

        // Collect a small, deterministic index of chunks. Compressed payloads
        // are produced and written one at a time inside the atomic temp file.
        let mut chunks_with_data = BTreeMap::<usize, (i32, i32)>::new();
        for entry in self.entries.values() {
            let chunk_x = entry.x >> 4;
            let chunk_z = entry.z >> 4;
            chunks_with_data.insert(Self::chunk_index(chunk_x, chunk_z), (chunk_x, chunk_z));
        }

        let mut location_table = [0u32; CHUNK_COUNT];
        let mut timestamp_table = [0u32; CHUNK_COUNT];
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs() as u32);

        crate::persistence::atomic_write(path, |file| {
            file.write_all(&[0; HEADER_SIZE])?;
            let mut current_sector: u32 = 2;

            for (&index, &(chunk_x, chunk_z)) in &chunks_with_data {
                let Some(chunk_data) = self.get_chunk_data(chunk_x, chunk_z) else {
                    continue;
                };
                let compressed = Self::compress_chunk_data(&chunk_data)?;
                let data_len = compressed.len() + 5;
                let sector_count = data_len.div_ceil(SECTOR_SIZE);
                if sector_count > u8::MAX as usize {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!("POI chunk {chunk_x},{chunk_z} exceeds MCA sector limit"),
                    ));
                }

                location_table[index] = (current_sector << 8) | sector_count as u32;
                timestamp_table[index] = timestamp;
                file.write_all(&((compressed.len() + 1) as u32).to_be_bytes())?;
                file.write_all(&[COMPRESSION_ZLIB])?;
                file.write_all(&compressed)?;
                let padding = sector_count * SECTOR_SIZE - data_len;
                for _ in 0..padding / SECTOR_SIZE {
                    file.write_all(&[0; SECTOR_SIZE])?;
                }
                file.write_all(&vec![0; padding % SECTOR_SIZE])?;
                current_sector += sector_count as u32;
            }

            file.seek(SeekFrom::Start(0))?;
            for loc in &location_table {
                file.write_all(&loc.to_be_bytes())?;
            }
            for ts in &timestamp_table {
                file.write_all(&ts.to_be_bytes())?;
            }
            Ok::<(), std::io::Error>(())
        })?;

        self.mark_persisted(self.mutation_generation);
        Ok(())
    }

    pub fn load(path: &Path) -> std::io::Result<Self> {
        if !path.exists() {
            return Ok(Self::new());
        }

        let file_data = std::fs::read(path)?;
        if file_data.len() < HEADER_SIZE {
            return Ok(Self::new());
        }

        let mut region = Self::new();

        // Parse location table
        for index in 0..CHUNK_COUNT {
            let offset = index * 4;
            let location = u32::from_be_bytes([
                file_data[offset],
                file_data[offset + 1],
                file_data[offset + 2],
                file_data[offset + 3],
            ]);

            let sector_offset = (location >> 8) as usize;
            let sector_count = (location & 0xFF) as usize;

            if sector_offset == 0 || sector_count == 0 {
                continue;
            }

            let byte_offset = sector_offset * SECTOR_SIZE;
            let byte_end = byte_offset + sector_count * SECTOR_SIZE;

            if byte_end > file_data.len() {
                continue;
            }

            // Read chunk data
            let chunk_bytes = &file_data[byte_offset..byte_end];
            if chunk_bytes.len() < 5 {
                continue;
            }

            let length = u32::from_be_bytes([
                chunk_bytes[0],
                chunk_bytes[1],
                chunk_bytes[2],
                chunk_bytes[3],
            ]) as usize;
            let compression = chunk_bytes[4];

            if compression != COMPRESSION_ZLIB || length < 1 || length > chunk_bytes.len() - 4 {
                continue;
            }

            let compressed = &chunk_bytes[5..5 + length - 1];

            match Self::decompress_chunk_data(compressed) {
                Ok(chunk_data) => {
                    if !chunk_data.residual_nbt.is_empty() {
                        region
                            .residual_roots
                            .insert(index, chunk_data.residual_nbt.clone());
                    }
                    for (_section_key, section) in chunk_data.sections {
                        for entry in section.records {
                            let key = (entry.x, entry.y, entry.z);
                            region.entries.insert(key, entry);
                        }
                    }
                }
                Err(e) => {
                    warn!("Failed to parse POI chunk at index {index}: {e}");
                }
            }
        }

        region.persisted_generation = region.mutation_generation;
        Ok(region)
    }
}

/// Region-based POI storage using MCA format
pub struct PoiStorage {
    /// Path to the poi folder
    folder: PathBuf,
    /// Loaded regions, keyed by (`region_x`, `region_z`)
    regions: HashMap<(i32, i32), PoiRegion>,
}

impl PoiStorage {
    #[must_use]
    pub fn new(poi_folder: PathBuf) -> Self {
        Self {
            folder: poi_folder,
            regions: HashMap::new(),
        }
    }

    const fn region_coords(pos: &BlockPos) -> (i32, i32) {
        let chunk_x = pos.0.x >> 4;
        let chunk_z = pos.0.z >> 4;
        (chunk_x >> 5, chunk_z >> 5)
    }

    fn region_path(&self, rx: i32, rz: i32) -> PathBuf {
        self.folder.join(format!("r.{rx}.{rz}.mca"))
    }

    fn get_or_load_region(&mut self, rx: i32, rz: i32) -> &mut PoiRegion {
        let path = self.region_path(rx, rz);
        self.regions.entry((rx, rz)).or_insert_with(|| {
            PoiRegion::load(&path).unwrap_or_else(|e| {
                if path.exists() {
                    warn!("Failed to load POI region {}: {}", path.display(), e);
                }
                PoiRegion::new()
            })
        })
    }

    pub fn add(&mut self, pos: BlockPos, poi_type: &str) {
        self.add_with_free_tickets(pos, poi_type, 0);
    }

    pub fn add_with_free_tickets(&mut self, pos: BlockPos, poi_type: &str, free_tickets: i32) {
        let (rx, rz) = Self::region_coords(&pos);
        let region = self.get_or_load_region(rx, rz);
        region.add(PoiEntry {
            x: pos.0.x,
            y: pos.0.y,
            z: pos.0.z,
            poi_type: poi_type.to_string(),
            free_tickets,
        });
    }

    pub fn add_portal(&mut self, pos: BlockPos) {
        self.add(pos, POI_TYPE_NETHER_PORTAL);
    }

    pub fn remove(&mut self, pos: &BlockPos) -> bool {
        let (rx, rz) = Self::region_coords(pos);
        let region = self.get_or_load_region(rx, rz);
        region.remove(pos)
    }

    /// Get all POI positions within a square radius (for portal search)
    #[expect(clippy::similar_names)]
    pub fn get_in_square(
        &mut self,
        center: BlockPos,
        radius: i32,
        poi_type: Option<&str>,
    ) -> Vec<BlockPos> {
        let min_x = center.0.x - radius;
        let max_x = center.0.x + radius;
        let min_z = center.0.z - radius;
        let max_z = center.0.z + radius;

        // Calculate which regions we need to check
        let min_rx = (min_x >> 4) >> 5;
        let max_rx = (max_x >> 4) >> 5;
        let min_rz = (min_z >> 4) >> 5;
        let max_rz = (max_z >> 4) >> 5;

        let mut results = Vec::new();

        for rx in min_rx..=max_rx {
            for rz in min_rz..=max_rz {
                let region = self.get_or_load_region(rx, rz);
                for entry in region.get_all() {
                    if let Some(filter_type) = poi_type
                        && entry.poi_type != filter_type
                    {
                        continue;
                    }

                    let dx = (entry.x - center.0.x).abs();
                    let dz = (entry.z - center.0.z).abs();
                    if dx <= radius && dz <= radius {
                        results.push(entry.pos());
                    }
                }
            }
        }

        results
    }

    /// Finds the closest POI whose type matches `matches`, considering
    /// entries within `radius` blocks of `center` on the x/z axes (like
    /// vanilla's `PoiManager.findClosestWithType`: a chebyshev square gather
    /// followed by picking the smallest 3D squared distance).
    ///
    /// Returns the entry's position together with its type.
    pub fn find_closest_matching(
        &mut self,
        center: BlockPos,
        radius: i32,
        matches: impl Fn(&str) -> bool,
    ) -> Option<(BlockPos, String)> {
        let min_rx = ((center.0.x - radius) >> 4) >> 5;
        let max_rx = ((center.0.x + radius) >> 4) >> 5;
        let min_rz = ((center.0.z - radius) >> 4) >> 5;
        let max_rz = ((center.0.z + radius) >> 4) >> 5;

        let mut best: Option<(BlockPos, String, i64)> = None;

        for rx in min_rx..=max_rx {
            for rz in min_rz..=max_rz {
                let region = self.get_or_load_region(rx, rz);
                for entry in region.get_all() {
                    if (entry.x - center.0.x).abs() > radius
                        || (entry.z - center.0.z).abs() > radius
                        || !matches(&entry.poi_type)
                    {
                        continue;
                    }

                    let dx = i64::from(entry.x - center.0.x);
                    let dy = i64::from(entry.y - center.0.y);
                    let dz = i64::from(entry.z - center.0.z);
                    let distance_sq = dx * dx + dy * dy + dz * dz;

                    if best.as_ref().is_none_or(|(_, _, d)| distance_sq < *d) {
                        best = Some((entry.pos(), entry.poi_type.clone(), distance_sq));
                    }
                }
            }
        }

        best.map(|(pos, poi_type, _)| (pos, poi_type))
    }

    pub async fn save_all(&mut self) -> std::io::Result<()> {
        let folder = self.folder.clone();
        let saves = self
            .regions
            .iter()
            .filter(|(_, region)| region.is_dirty())
            .map(|(&(rx, rz), region)| {
                (
                    (rx, rz),
                    region.dirty_generation(),
                    region.clone(),
                    self.folder.join(format!("r.{rx}.{rz}.mca")),
                )
            })
            .collect::<Vec<_>>();
        let saved = saves.len();

        let persisted = tokio::task::spawn_blocking(move || {
            std::fs::create_dir_all(folder)?;
            let mut persisted = Vec::with_capacity(saves.len());
            for (key, generation, mut region, path) in saves {
                region.save(&path)?;
                persisted.push((key, generation));
            }
            Ok::<_, std::io::Error>(persisted)
        })
        .await
        .map_err(std::io::Error::other)??;

        for (key, generation) in persisted {
            if let Some(region) = self.regions.get_mut(&key) {
                region.mark_persisted(generation);
            }
        }

        if saved > 0 {
            info!("Saved {saved} POI region(s)");
        }
        Ok(())
    }

    /// Get count of loaded regions
    #[must_use]
    pub fn loaded_region_count(&self) -> usize {
        self.regions.len()
    }

    /// Get total POI count across all loaded regions
    #[must_use]
    pub fn total_poi_count(&self) -> usize {
        self.regions.values().map(|r| r.get_all().len()).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn poi_entry() {
        let entry = PoiEntry::new_portal(BlockPos(Vector3::new(100, 64, 200)));
        assert_eq!(entry.x, 100);
        assert_eq!(entry.y, 64);
        assert_eq!(entry.z, 200);
        assert_eq!(entry.poi_type, POI_TYPE_NETHER_PORTAL);
    }

    #[test]
    fn poi_region() {
        let mut region = PoiRegion::new();
        region.add(PoiEntry::new_portal(BlockPos(Vector3::new(100, 64, 200))));
        region.add(PoiEntry::new_portal(BlockPos(Vector3::new(101, 64, 200))));

        assert_eq!(region.get_all().len(), 2);
        assert!(region.is_dirty());

        region.remove(&BlockPos(Vector3::new(100, 64, 200)));
        assert_eq!(region.get_all().len(), 1);
    }

    #[test]
    fn poi_find_closest_matching() {
        let mut storage = PoiStorage::new(std::env::temp_dir().join("pumpkin_poi_closest_test"));

        storage.add_portal(BlockPos(Vector3::new(100, 64, 100)));
        storage.add_portal(BlockPos(Vector3::new(120, 64, 100)));
        storage.add(BlockPos(Vector3::new(101, 64, 100)), "minecraft:home");

        let center = BlockPos(Vector3::new(105, 64, 100));
        let (pos, poi_type) = storage
            .find_closest_matching(center, 256, |t| t == POI_TYPE_NETHER_PORTAL)
            .unwrap();
        assert_eq!(pos, BlockPos(Vector3::new(100, 64, 100)));
        assert_eq!(poi_type, POI_TYPE_NETHER_PORTAL);

        // The overall closest one ignores the type filter mismatch above.
        let (pos, poi_type) = storage
            .find_closest_matching(center, 256, |_| true)
            .unwrap();
        assert_eq!(pos, BlockPos(Vector3::new(101, 64, 100)));
        assert_eq!(poi_type, "minecraft:home");

        assert!(
            storage
                .find_closest_matching(center, 256, |t| t == "minecraft:lodestone")
                .is_none()
        );
        // Out of horizontal range.
        assert!(
            storage
                .find_closest_matching(BlockPos(Vector3::new(1000, 64, 100)), 16, |_| true)
                .is_none()
        );
    }

    #[tokio::test]
    async fn poi_storage_mca() {
        let dir = std::env::temp_dir().join("pumpkin_poi_mca_test");
        let _ = std::fs::remove_dir_all(&dir);

        let mut storage = PoiStorage::new(dir.join("poi"));

        storage.add_portal(BlockPos(Vector3::new(100, 64, 100)));
        storage.add_portal(BlockPos(Vector3::new(110, 64, 100)));
        storage.add_portal(BlockPos(Vector3::new(1000, 64, 1000))); // Different region

        let results = storage.get_in_square(
            BlockPos(Vector3::new(105, 64, 100)),
            16,
            Some(POI_TYPE_NETHER_PORTAL),
        );
        assert_eq!(results.len(), 2);

        storage.save_all().await.unwrap();

        // Verify .mca file was created
        let mca_path = dir.join("poi").join("r.0.0.mca");
        assert!(mca_path.exists());

        // Reload and verify
        let mut storage2 = PoiStorage::new(dir.join("poi"));
        let results2 = storage2.get_in_square(
            BlockPos(Vector3::new(105, 64, 100)),
            16,
            Some(POI_TYPE_NETHER_PORTAL),
        );
        assert_eq!(results2.len(), 2);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn poi_decompression_limit_rejects_overflow() {
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(&[0; 129]).unwrap();
        let compressed = encoder.finish().unwrap();
        assert!(PoiRegion::decompress_chunk_data_with_limit(&compressed, 128).is_err());
    }

    #[test]
    fn poi_decompression_rejects_hostile_compression_ratio() {
        let raw = vec![0; 1024 * 1024];
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::best());
        encoder.write_all(&raw).unwrap();
        let compressed = encoder.finish().unwrap();
        assert!(PoiRegion::decompress_chunk_data_with_limit(&compressed, raw.len()).is_err());
    }

    #[test]
    fn poi_persisted_generation_does_not_clear_newer_mutation() {
        let mut region = PoiRegion::new();
        region.add(PoiEntry {
            x: 0,
            y: 64,
            z: 0,
            poi_type: POI_TYPE_NETHER_PORTAL.to_owned(),
            free_tickets: 0,
        });
        let snapshot_generation = region.dirty_generation();
        region.add(PoiEntry {
            x: 1,
            y: 64,
            z: 0,
            poi_type: POI_TYPE_NETHER_PORTAL.to_owned(),
            free_tickets: 0,
        });

        region.mark_persisted(snapshot_generation);

        assert!(region.is_dirty());
    }

    #[test]
    fn poi_data_version_identity_migration_writes_current_and_rejects_future() {
        let legacy = PoiChunkData {
            data_version: crate::world_info::schema::MINIMUM_SUPPORTED_POI_DATA_VERSION,
            sections: HashMap::new(),
            residual_nbt: pumpkin_nbt::compound::NbtCompound::new(),
        };
        let compressed = PoiRegion::compress_chunk_data(&legacy).unwrap();
        assert_eq!(
            PoiRegion::decompress_chunk_data(&compressed)
                .unwrap()
                .data_version,
            DATA_VERSION
        );

        let future = PoiChunkData {
            data_version: DATA_VERSION + 1,
            sections: HashMap::new(),
            residual_nbt: pumpkin_nbt::compound::NbtCompound::new(),
        };
        let compressed = PoiRegion::compress_chunk_data(&future).unwrap();
        assert!(PoiRegion::decompress_chunk_data(&compressed).is_err());
    }

    #[test]
    fn poi_unknown_root_survives_disk_restart_and_rewrite() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "pumpkin_poi_residual_{}_{}",
            std::process::id(),
            nonce
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("r.0.0.mca");

        let mut region = PoiRegion::new();
        region.add(PoiEntry::new_portal(BlockPos(Vector3::new(1, 64, 1))));
        let mut residual = pumpkin_nbt::compound::NbtCompound::new();
        residual.put_string("plugin:marker", "kept");
        residual.put_int("DataVersion", -1);
        residual.put_string("Sections", "stale");
        region.residual_roots.insert(0, residual);
        region.save(&path).unwrap();

        let mut reloaded = PoiRegion::load(&path).unwrap();
        reloaded.add(PoiEntry::new_portal(BlockPos(Vector3::new(2, 64, 2))));
        reloaded.save(&path).unwrap();

        let restarted = PoiRegion::load(&path).unwrap();
        let chunk = restarted.get_chunk_data(0, 0).unwrap();
        assert_eq!(chunk.residual_nbt.get_string("plugin:marker"), Some("kept"));
        assert!(chunk.residual_nbt.get("DataVersion").is_none());
        assert!(chunk.residual_nbt.get("Sections").is_none());
        assert_eq!(chunk.data_version, DATA_VERSION);
        assert_eq!(
            chunk
                .sections
                .values()
                .map(|section| section.records.len())
                .sum::<usize>(),
            2
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
