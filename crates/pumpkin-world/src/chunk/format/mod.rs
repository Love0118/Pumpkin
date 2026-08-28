use std::{
    path::PathBuf,
    pin::Pin,
    str::FromStr,
    sync::{
        Arc, LazyLock, RwLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Instant,
};

use bytes::Bytes;
use pumpkin_data::{Block, BlockId, BlockStateId, biome::Biome, chunk::ChunkStatus, fluid::Fluid};
use pumpkin_nbt::{
    COMPOUND_ID, END_ID,
    compound::NbtCompound,
    serializer::{NbtWriteHelper, NbtWriteHelperJava},
};
use pumpkin_util::resource_location::{FromResourceLocation, ResourceLocation, ToResourceLocation};
use rustc_hash::FxHashMap;

use crate::{
    chunk::{
        ChunkEntityData, ChunkReadingError, ChunkSerializingError,
        format::anvil::{SingleChunkDataSerializer, WORLD_DATA_VERSION},
        io::{Dirtiable, file_manager::PathFromLevelFolder},
    },
    generation::section_coords,
    level::LevelFolder,
    serialization_metrics::{self, SerializationStage},
    tick::{ScheduledTick, TickPriority, scheduler::ChunkTickScheduler},
};
use pumpkin_util::math::position::BlockPos;
use pumpkin_util::math::vector2::Vector2;

use super::{
    ChunkData, ChunkHeightmaps, ChunkLight, ChunkParsingError, ChunkSections,
    palette::{BiomePalette, BlockPalette},
};
pub mod anvil;
pub mod linear;
pub mod pump;

const MAX_CHUNK_RESIDUAL_NBT_ENTRIES: usize = 512;
const MAX_CHUNK_RESIDUAL_NBT_BYTES: usize = 2 * 1024 * 1024;
const CHUNK_AUTHORITATIVE_ROOT_KEYS: &[&str] = &[
    "DataVersion",
    "xPos",
    "zPos",
    "yPos",
    "Status",
    "Heightmaps",
    "sections",
    "block_ticks",
    "fluid_ticks",
    "block_entities",
    "isLightOn",
    "InhabitedTime",
    "PumpkinCustomData",
    "BukkitValues",
];
const MAX_ENTITY_CHUNK_RESIDUAL_NBT_ENTRIES: usize = 256;
const MAX_ENTITY_CHUNK_RESIDUAL_NBT_BYTES: usize = 1024 * 1024;
const ENTITY_CHUNK_AUTHORITATIVE_ROOT_KEYS: &[&str] = &[
    "DataVersion",
    "Position",
    "Position-X",
    "Position-Z",
    "Entities",
];

type SharedBlockProperty = (Arc<str>, Arc<str>);
type SharedBlockProperties = Box<[SharedBlockProperty]>;

struct SharedPaletteStrings {
    block_names: Box<[Arc<str>]>,
    block_properties: Box<[SharedBlockProperties]>,
    biome_names: Box<[Arc<str>]>,
}

static SHARED_PALETTE_STRINGS: LazyLock<SharedPaletteStrings> = LazyLock::new(|| {
    fn namespaced(name: &'static str) -> Arc<str> {
        if name.starts_with("minecraft:") {
            Arc::from(name)
        } else {
            Arc::from(format!("minecraft:{name}"))
        }
    }

    fn intern(strings: &mut FxHashMap<&'static str, Arc<str>>, value: &'static str) -> Arc<str> {
        strings
            .entry(value)
            .or_insert_with(|| Arc::from(value))
            .clone()
    }

    let block_names = (0..BlockId::COUNT)
        .map(|raw_id| {
            let id = BlockId::new_or_air(raw_id);
            namespaced(Block::from_id(id).name)
        })
        .collect();

    let mut property_strings = FxHashMap::default();
    let block_properties = (0..BlockStateId::COUNT)
        .map(|raw_id| {
            let id = BlockStateId::new_or_air(raw_id);
            Block::from_state_id(id)
                .properties(id)
                .map(|properties| {
                    properties
                        .to_props()
                        .into_iter()
                        .map(|(name, value)| {
                            (
                                intern(&mut property_strings, name),
                                intern(&mut property_strings, value),
                            )
                        })
                        .collect()
                })
                .unwrap_or_default()
        })
        .collect();

    let plains = namespaced(Biome::PLAINS.registry_id);
    let mut biome_names = vec![plains; usize::from(u8::MAX) + 1];
    for biome in Biome::ALL {
        biome_names[usize::from(biome.id)] = namespaced(biome.registry_id);
    }

    SharedPaletteStrings {
        block_names,
        block_properties,
        biome_names: biome_names.into_boxed_slice(),
    }
});

#[inline]
fn shared_block_name(id: BlockStateId) -> Arc<str> {
    SHARED_PALETTE_STRINGS.block_names[usize::from(id.to_block_id().as_u16())].clone()
}

#[inline]
fn shared_block_properties(id: BlockStateId) -> &'static [(Arc<str>, Arc<str>)] {
    &SHARED_PALETTE_STRINGS.block_properties[usize::from(id.as_u16())]
}

#[inline]
fn shared_biome_name(id: u8) -> Arc<str> {
    SHARED_PALETTE_STRINGS.biome_names[usize::from(id)].clone()
}

impl SingleChunkDataSerializer for ChunkData {
    #[inline]
    fn from_bytes(bytes: &Bytes, pos: Vector2<i32>) -> Result<Self, ChunkReadingError> {
        Self::internal_from_bytes(bytes, pos).map_err(ChunkReadingError::ParsingError)
    }

    #[inline]
    fn to_bytes(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<Bytes, ChunkSerializingError>> + Send + '_>> {
        Box::pin(async move { self.internal_to_bytes() })
    }

    #[inline]
    fn position(&self) -> (i32, i32) {
        (self.x, self.z)
    }
}

impl PathFromLevelFolder for ChunkData {
    #[inline]
    fn file_path(folder: &LevelFolder, file_name: &str) -> PathBuf {
        folder.region_folder.join(file_name)
    }
}

impl Dirtiable for ChunkData {
    #[inline]
    fn mark_dirty(&self, flag: bool) {
        self.dirty.store(flag, Ordering::Relaxed);
    }

    #[inline]
    fn is_dirty(&self) -> bool {
        self.dirty.load(Ordering::Relaxed)
    }

    fn dirty_generation(&self) -> u64 {
        self.dirty.generation()
    }

    fn mark_persisted(&self, generation: u64) {
        self.dirty.mark_persisted(generation);
    }
}

fn extract_u16_array(tag: &pumpkin_nbt::tag::NbtTag) -> Option<Box<[BlockStateId]>> {
    match tag {
        pumpkin_nbt::tag::NbtTag::IntArray(arr) => Some(
            arr.iter()
                .map(|&x| BlockStateId::new_or_air(x as u16))
                .collect(),
        ),
        pumpkin_nbt::tag::NbtTag::ByteArray(arr) => Some(
            arr.iter()
                .map(|&x| BlockStateId::new_or_air(x as u16))
                .collect(),
        ),
        pumpkin_nbt::tag::NbtTag::LongArray(arr) => Some(
            arr.iter()
                .map(|&x| BlockStateId::new_or_air(x as u16))
                .collect(),
        ),
        pumpkin_nbt::tag::NbtTag::List(list) => {
            let ids: Box<[BlockStateId]> = list
                .iter()
                .map(|t| match t {
                    pumpkin_nbt::tag::NbtTag::Int(x) => BlockStateId::new_or_air(*x as u16),
                    pumpkin_nbt::tag::NbtTag::Short(x) => BlockStateId::new_or_air(*x as u16),
                    pumpkin_nbt::tag::NbtTag::Byte(x) => BlockStateId::new_or_air(*x as u16),
                    pumpkin_nbt::tag::NbtTag::Long(x) => BlockStateId::new_or_air(*x as u16),
                    pumpkin_nbt::tag::NbtTag::Compound(compound) => {
                        if let Ok(entry) =
                            crate::generation::structure::template::PaletteEntry::from_nbt_compound(
                                compound,
                            )
                            && let Some(state) =
                                crate::generation::structure::template::BlockStateResolver::resolve_simple(
                                    &entry,
                                )
                        {
                            return state.id;
                        }
                        BlockStateId::AIR
                    }
                    _ => BlockStateId::AIR,
                })
                .collect();
            Some(ids)
        }
        _ => None,
    }
}

fn extract_u8_array(tag: &pumpkin_nbt::tag::NbtTag) -> Option<Box<[u8]>> {
    match tag {
        pumpkin_nbt::tag::NbtTag::ByteArray(arr) => Some(arr.iter().map(|&x| x as u8).collect()),
        pumpkin_nbt::tag::NbtTag::IntArray(arr) => Some(arr.iter().map(|&x| x as u8).collect()),
        pumpkin_nbt::tag::NbtTag::List(list) => {
            let bytes: Box<[u8]> = list
                .iter()
                .map(|t| match t {
                    pumpkin_nbt::tag::NbtTag::Byte(x) => *x as u8,
                    pumpkin_nbt::tag::NbtTag::Int(x) => *x as u8,
                    pumpkin_nbt::tag::NbtTag::Short(x) => *x as u8,
                    pumpkin_nbt::tag::NbtTag::String(s) => {
                        let name = s.strip_prefix("minecraft:").unwrap_or(s);
                        pumpkin_data::biome::Biome::from_name(name).map_or(0, |b| b.id)
                    }
                    _ => 0,
                })
                .collect();
            Some(bytes)
        }
        _ => None,
    }
}

fn parse_scheduled_tick<T>(nbt: &pumpkin_nbt::compound::NbtCompound) -> Option<ScheduledTick<T>>
where
    T: FromResourceLocation,
{
    let x = nbt.get_int("x")?;
    let y = nbt.get_int("y")?;
    let z = nbt.get_int("z")?;
    let delay = nbt.get_int("t")? as u8;
    let priority = TickPriority::try_from(nbt.get_int("p")?).ok()?;
    let res_loc_str = nbt.get_string("i")?;
    let res_loc = ResourceLocation::from_str(res_loc_str).ok()?;
    let value = T::from_resource_location(&res_loc)?;
    Some(ScheduledTick {
        delay,
        priority,
        position: BlockPos::new(x, y, z),
        value,
    })
}

impl ChunkData {
    #[allow(clippy::too_many_lines)]
    pub fn internal_from_bytes(
        chunk_data: &[u8],
        position: Vector2<i32>,
    ) -> Result<Self, ChunkParsingError> {
        let is_named = chunk_data.len() >= 3
            && chunk_data[0] == 0x0a
            && chunk_data[1] == 0x00
            && chunk_data[2] == 0x00;

        let mut cursor = std::io::Cursor::new(chunk_data);
        let mut reader = pumpkin_nbt::deserializer::NbtReadHelperJava::new(&mut cursor);
        let nbt = if is_named {
            pumpkin_nbt::Nbt::read(&mut reader)
        } else {
            pumpkin_nbt::Nbt::read_unnamed(&mut reader)
        }
        .map_err(|e| ChunkParsingError::ErrorDeserializingChunk(e.to_string()))?;

        let mut root_tag = nbt.root_tag;
        crate::world_info::schema::migrate_persistent_root(
            crate::world_info::schema::PersistentRootSchema::TerrainChunk,
            &mut root_tag,
        )
        .map_err(|error| ChunkParsingError::ErrorDeserializingChunk(error.to_string()))?;

        let x_pos = root_tag.get_int("xPos").ok_or_else(|| {
            ChunkParsingError::ErrorDeserializingChunk("Missing xPos".to_string())
        })?;
        let z_pos = root_tag.get_int("zPos").ok_or_else(|| {
            ChunkParsingError::ErrorDeserializingChunk("Missing zPos".to_string())
        })?;

        if x_pos != position.x || z_pos != position.y {
            return Err(ChunkParsingError::ErrorDeserializingChunk(format!(
                "Expected data for chunk {},{} but got it for {},{}!",
                position.x, position.y, x_pos, z_pos,
            )));
        }

        let min_y_section = root_tag.get_int("yPos").ok_or_else(|| {
            ChunkParsingError::ErrorDeserializingChunk("Missing yPos".to_string())
        })?;

        let mut max_y_section = min_y_section as i8;
        if let Some(sections_list) = root_tag.get_list("sections") {
            for section_tag in sections_list {
                if let pumpkin_nbt::tag::NbtTag::Compound(section_compound) = section_tag {
                    let y = section_compound.get_byte("Y").unwrap_or(0);
                    if y > max_y_section {
                        max_y_section = y;
                    }
                }
            }
        }

        let section_count = (max_y_section as i32 - min_y_section + 1).max(0) as usize;
        let mut block_lights = vec![LightContainer::Empty(0); section_count];
        let mut sky_lights = vec![LightContainer::Empty(0); section_count];
        let mut block_palettes = vec![BlockPalette::default(); section_count];
        let mut biome_palettes = vec![BiomePalette::default(); section_count];

        if let Some(sections_list) = root_tag.get_list("sections") {
            for section_tag in sections_list {
                if let pumpkin_nbt::tag::NbtTag::Compound(section_compound) = section_tag {
                    let y = section_compound.get_byte("Y").unwrap_or(0);
                    let index = (y as i32 - min_y_section) as usize;
                    if index >= section_count {
                        continue;
                    }

                    let block_light = section_compound
                        .get("BlockLight")
                        .and_then(|tag| tag.extract_byte_array())
                        .map(|arr| {
                            // SAFETY: `arr` is an `i8` slice (`&[i8]`). `u8` and `i8` have identical memory layout, alignment (1 byte), and lifetime.
                            unsafe {
                                Box::from(std::slice::from_raw_parts(
                                    arr.as_ptr().cast::<u8>(),
                                    arr.len(),
                                ))
                            }
                        });

                    let sky_light = section_compound
                        .get("SkyLight")
                        .and_then(|tag| tag.extract_byte_array())
                        .map(|arr| {
                            // SAFETY: `arr` is an `i8` slice (`&[i8]`). `u8` and `i8` have identical memory layout, alignment (1 byte), and lifetime.
                            unsafe {
                                Box::from(std::slice::from_raw_parts(
                                    arr.as_ptr().cast::<u8>(),
                                    arr.len(),
                                ))
                            }
                        });

                    block_lights[index] =
                        block_light.map_or(LightContainer::Empty(0), LightContainer::Full);
                    sky_lights[index] =
                        sky_light.map_or(LightContainer::Empty(0), LightContainer::Full);

                    if let Some(bs_compound) = section_compound.get_compound("block_states") {
                        let data = bs_compound
                            .get_long_array("data")
                            .map(|arr| arr.to_vec().into_boxed_slice());
                        let palette = bs_compound
                            .get("palette")
                            .and_then(extract_u16_array)
                            .unwrap_or_else(|| vec![BlockStateId::AIR].into_boxed_slice());

                        block_palettes[index] =
                            BlockPalette::from_disk_nbt(ChunkSectionBlockStates { data, palette });
                    } else {
                        block_palettes[index] = BlockPalette::default();
                    }

                    if let Some(b_compound) = section_compound.get_compound("biomes") {
                        let data = b_compound
                            .get_long_array("data")
                            .map(|arr| arr.to_vec().into_boxed_slice());
                        let palette = b_compound
                            .get("palette")
                            .and_then(extract_u8_array)
                            .unwrap_or_else(|| vec![0].into_boxed_slice());

                        biome_palettes[index] =
                            BiomePalette::from_disk_nbt(ChunkSectionBiomes { data, palette });
                    } else {
                        biome_palettes[index] = BiomePalette::default();
                    }
                }
            }
        }

        // Assemble the LightEngine
        let light_engine = ChunkLight {
            block_light: block_lights.into_boxed_slice(),
            sky_light: sky_lights.into_boxed_slice(),
        };

        // Assemble the ChunkSections
        let min_y = section_coords::section_to_block(min_y_section);
        let (random_tick_sections, randomly_ticking_mask) =
            ChunkSections::build_random_tick_sections_cache(&block_palettes);
        let section = ChunkSections {
            count: block_palettes.len(),
            block_sections: RwLock::new(block_palettes.into_boxed_slice()),
            random_tick_sections: RwLock::new(random_tick_sections),
            randomly_ticking_mask: std::sync::atomic::AtomicU32::new(randomly_ticking_mask),
            biome_sections: RwLock::new(biome_palettes.into_boxed_slice()),
            min_y,
        };

        let heightmaps = root_tag.get_compound("Heightmaps").map_or(
            ChunkHeightmaps {
                world_surface: None,
                motion_blocking: None,
                motion_blocking_no_leaves: None,
            },
            |h_compound| ChunkHeightmaps {
                world_surface: h_compound
                    .get_long_array("WORLD_SURFACE")
                    .map(|a| a.to_vec().into_boxed_slice()),
                motion_blocking: h_compound
                    .get_long_array("MOTION_BLOCKING")
                    .map(|a| a.to_vec().into_boxed_slice()),
                motion_blocking_no_leaves: h_compound
                    .get_long_array("MOTION_BLOCKING_NO_LEAVES")
                    .map(|a| a.to_vec().into_boxed_slice()),
            },
        );
        let mut block_ticks = Vec::new();
        if let Some(list) = root_tag.get_list("block_ticks") {
            for tag in list {
                if let pumpkin_nbt::tag::NbtTag::Compound(compound) = tag
                    && let Some(tick) = parse_scheduled_tick::<&'static Block>(compound)
                {
                    block_ticks.push(tick);
                }
            }
        }

        let mut fluid_ticks = Vec::new();
        if let Some(list) = root_tag.get_list("fluid_ticks") {
            for tag in list {
                if let pumpkin_nbt::tag::NbtTag::Compound(compound) = tag
                    && let Some(tick) = parse_scheduled_tick::<&'static Fluid>(compound)
                {
                    fluid_ticks.push(tick);
                }
            }
        }

        let mut block_entities = FxHashMap::default();
        if let Some(list) = root_tag.get_list("block_entities") {
            for tag in list {
                if let pumpkin_nbt::tag::NbtTag::Compound(nbt) = tag
                    && let Some(x) = nbt.get_int("x")
                    && let Some(y) = nbt.get_int("y")
                    && let Some(z) = nbt.get_int("z")
                {
                    block_entities.insert(BlockPos::new(x, y, z), nbt.clone());
                }
            }
        }

        let light_correct = root_tag.get_bool("isLightOn").unwrap_or(false);

        let status_str = root_tag.get_string("Status").unwrap_or("minecraft:empty");
        let status = match status_str {
            "minecraft:structure_starts" => ChunkStatus::StructureStarts,
            "minecraft:structure_references" => ChunkStatus::StructureReferences,
            "minecraft:biomes" => ChunkStatus::Biomes,
            "minecraft:noise" => ChunkStatus::Noise,
            "minecraft:surface" => ChunkStatus::Surface,
            "minecraft:carvers" => ChunkStatus::Carvers,
            "minecraft:features" => ChunkStatus::Features,
            "minecraft:initialize_light" => ChunkStatus::InitializeLight,
            "minecraft:light" => ChunkStatus::Light,
            "minecraft:spawn" => ChunkStatus::Spawn,
            "minecraft:full" => ChunkStatus::Full,
            _ => ChunkStatus::Empty,
        };

        let custom_data = root_tag
            .get_compound("PumpkinCustomData")
            .or_else(|| root_tag.get_compound("BukkitValues"))
            .cloned()
            .unwrap_or_default();
        let residual_nbt = crate::persistence::bounded_residual_nbt(
            &root_tag,
            CHUNK_AUTHORITATIVE_ROOT_KEYS.iter().copied(),
            MAX_CHUNK_RESIDUAL_NBT_ENTRIES,
            MAX_CHUNK_RESIDUAL_NBT_BYTES,
        )
        .unwrap_or_else(|error| {
            tracing::warn!(
                "discarding residual NBT for chunk {},{}: {error}",
                position.x,
                position.y
            );
            NbtCompound::new()
        });

        Ok(Self {
            section,
            heightmap: std::sync::Mutex::new(heightmaps),
            x: position.x,
            z: position.y,
            // This chunk is read from disk, so it has not been modified
            dirty: crate::chunk::DirtyState::new(false),
            block_ticks: ChunkTickScheduler::from_iter(block_ticks),
            fluid_ticks: ChunkTickScheduler::from_iter(fluid_ticks),
            pending_block_entities: std::sync::Mutex::new(block_entities),
            light_engine: std::sync::Mutex::new(light_engine),
            light_populated: AtomicBool::new(light_correct),
            status,
            blending_data: None,
            inhabited_time: AtomicU64::new(root_tag.get_long("InhabitedTime").unwrap_or(0) as u64),
            custom_data: std::sync::Mutex::new(custom_data),
            residual_nbt: std::sync::Mutex::new(residual_nbt),
        })
    }

    #[allow(clippy::too_many_lines)]
    fn internal_to_bytes(&self) -> Result<Bytes, ChunkSerializingError> {
        use pumpkin_nbt::tag::NbtTag;

        fn extract_light_ref(light: Option<&LightContainer>) -> Option<&[u8]> {
            match light {
                Some(LightContainer::Full(data)) => Some(data.as_ref()),
                _ => None,
            }
        }

        let snapshot_started = Instant::now();
        let is_light_correct = self
            .light_populated
            .load(std::sync::atomic::Ordering::Relaxed);

        let block_entities_nbt = {
            let entities_guard = self
                .pending_block_entities
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            entities_guard.values().cloned().collect::<Vec<_>>()
        };

        let light = self
            .light_engine
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let heightmaps = self
            .heightmap
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let block_sections = self
            .section
            .block_sections
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .map(BlockPalette::to_disk_nbt)
            .collect::<Vec<_>>();
        let biome_sections = self
            .section
            .biome_sections
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .map(BiomePalette::to_disk_nbt)
            .collect::<Vec<_>>();
        let custom_data = self
            .custom_data
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let residual_nbt = self
            .residual_nbt
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        serialization_metrics::record_duration(
            SerializationStage::Snapshot,
            snapshot_started.elapsed(),
        );

        let encode_started = Instant::now();
        let min_section_y = (self.section.min_y >> 4) as i8;

        let mut root_compound = residual_nbt;
        root_compound.put_int("DataVersion", WORLD_DATA_VERSION);
        root_compound.put_int("xPos", self.x);
        root_compound.put_int("zPos", self.z);
        root_compound.put_int("yPos", section_coords::block_to_section(self.section.min_y));

        let status_str = match self.status {
            ChunkStatus::Empty => "minecraft:empty",
            ChunkStatus::StructureStarts => "minecraft:structure_starts",
            ChunkStatus::StructureReferences => "minecraft:structure_references",
            ChunkStatus::Biomes => "minecraft:biomes",
            ChunkStatus::Noise => "minecraft:noise",
            ChunkStatus::Surface => "minecraft:surface",
            ChunkStatus::Carvers => "minecraft:carvers",
            ChunkStatus::Features => "minecraft:features",
            ChunkStatus::InitializeLight => "minecraft:initialize_light",
            ChunkStatus::Light => "minecraft:light",
            ChunkStatus::Spawn => "minecraft:spawn",
            ChunkStatus::Full => "minecraft:full",
        };
        root_compound.put_string("Status", status_str.to_string());

        let mut heightmaps_compound = NbtCompound::new();
        if let Some(ref arr) = heightmaps.world_surface {
            heightmaps_compound.put("WORLD_SURFACE", NbtTag::LongArray(arr.to_vec()));
        }
        if let Some(ref arr) = heightmaps.motion_blocking {
            heightmaps_compound.put("MOTION_BLOCKING", NbtTag::LongArray(arr.to_vec()));
        }
        if let Some(ref arr) = heightmaps.motion_blocking_no_leaves {
            heightmaps_compound.put("MOTION_BLOCKING_NO_LEAVES", NbtTag::LongArray(arr.to_vec()));
        }
        root_compound.put_compound("Heightmaps", heightmaps_compound);

        let mut sections_list = Vec::new();
        for i in 0..self.section.count {
            let mut section_comp = NbtCompound::new();
            let y_val = i as i8 + min_section_y;
            section_comp.put_byte("Y", y_val);

            // block_states
            let block_states_nbt = &block_sections[i];
            let mut bs_comp = NbtCompound::new();
            if let Some(ref data_arr) = block_states_nbt.data {
                bs_comp.put("data", NbtTag::LongArray(data_arr.to_vec()));
            }
            let palette_tags: Vec<NbtTag> = block_states_nbt
                .palette
                .iter()
                .map(|&id| {
                    let mut comp = NbtCompound::new();
                    comp.put_string("Name", shared_block_name(id));
                    let properties = shared_block_properties(id);
                    if !properties.is_empty() {
                        let mut props_comp = NbtCompound::new();
                        for (name, value) in properties {
                            props_comp.put_string(name, Arc::clone(value));
                        }
                        comp.put_compound("Properties", props_comp);
                    }
                    NbtTag::Compound(comp)
                })
                .collect();
            bs_comp.put_list("palette", palette_tags);
            section_comp.put_compound("block_states", bs_comp);

            // biomes
            let biomes_nbt = &biome_sections[i];
            let mut b_comp = NbtCompound::new();
            if let Some(ref data_arr) = biomes_nbt.data {
                b_comp.put("data", NbtTag::LongArray(data_arr.to_vec()));
            }
            let biome_palette_tags: Vec<NbtTag> = biomes_nbt
                .palette
                .iter()
                .map(|&val| NbtTag::String(shared_biome_name(val)))
                .collect();
            b_comp.put_list("palette", biome_palette_tags);
            section_comp.put_compound("biomes", b_comp);

            // block_light
            if let Some(light_data) = extract_light_ref(light.block_light.get(i)) {
                let bytes: Box<[i8]> = light_data.iter().map(|&x| x as i8).collect();
                section_comp.put("BlockLight", NbtTag::ByteArray(bytes));
            }

            // sky_light
            if let Some(light_data) = extract_light_ref(light.sky_light.get(i)) {
                let bytes: Box<[i8]> = light_data.iter().map(|&x| x as i8).collect();
                section_comp.put("SkyLight", NbtTag::ByteArray(bytes));
            }

            sections_list.push(NbtTag::Compound(section_comp));
        }
        root_compound.put_list("sections", sections_list);

        let mut block_ticks_list = Vec::new();
        for tick in self.block_ticks.to_vec() {
            let mut tick_comp = NbtCompound::new();
            tick_comp.put_int("x", tick.position.0.x);
            tick_comp.put_int("y", tick.position.0.y);
            tick_comp.put_int("z", tick.position.0.z);
            tick_comp.put_int("t", tick.delay as i32);
            tick_comp.put_int("p", tick.priority as i32);
            tick_comp.put_string("i", tick.value.to_resource_location());
            block_ticks_list.push(NbtTag::Compound(tick_comp));
        }
        root_compound.put_list("block_ticks", block_ticks_list);

        let mut fluid_ticks_list = Vec::new();
        for tick in self.fluid_ticks.to_vec() {
            let mut tick_comp = NbtCompound::new();
            tick_comp.put_int("x", tick.position.0.x);
            tick_comp.put_int("y", tick.position.0.y);
            tick_comp.put_int("z", tick.position.0.z);
            tick_comp.put_int("t", tick.delay as i32);
            tick_comp.put_int("p", tick.priority as i32);
            tick_comp.put_string("i", tick.value.to_resource_location());
            fluid_ticks_list.push(NbtTag::Compound(tick_comp));
        }
        root_compound.put_list("fluid_ticks", fluid_ticks_list);

        let mut block_entities_list = Vec::new();
        for entity_comp in block_entities_nbt {
            block_entities_list.push(NbtTag::Compound(entity_comp));
        }
        root_compound.put_list("block_entities", block_entities_list);

        root_compound.put_bool("isLightOn", is_light_correct);
        root_compound.put_long(
            "InhabitedTime",
            self.inhabited_time.load(Ordering::Relaxed) as i64,
        );

        if !custom_data.is_empty() {
            root_compound.put_compound("PumpkinCustomData", custom_data);
        }

        let nbt = pumpkin_nbt::Nbt::from(root_compound);
        let result = nbt.write().map_err(ChunkSerializingError::from);
        serialization_metrics::record_duration(
            SerializationStage::Encode,
            encode_started.elapsed(),
        );
        result
    }

    pub fn set_custom_data(&self, namespace: &str, key: &str, value: pumpkin_nbt::tag::NbtTag) {
        let mut custom_data = self
            .custom_data
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        let mut namespace_data = custom_data
            .child_tags
            .remove(namespace)
            .and_then(|tag| match tag {
                pumpkin_nbt::tag::NbtTag::Compound(compound) => Some(compound),
                _ => None,
            })
            .unwrap_or_default();

        namespace_data.child_tags.insert(key.into(), value);
        custom_data.child_tags.insert(
            namespace.into(),
            pumpkin_nbt::tag::NbtTag::Compound(namespace_data),
        );
        self.dirty.store(true, Ordering::Relaxed);
    }

    pub fn get_custom_data(&self, namespace: &str, key: &str) -> Option<pumpkin_nbt::tag::NbtTag> {
        let custom_data = self
            .custom_data
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        custom_data
            .get(namespace)?
            .extract_compound()?
            .get(key)
            .cloned()
    }

    pub fn remove_custom_data(&self, namespace: &str, key: &str) {
        let mut custom_data = self
            .custom_data
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        let Some(pumpkin_nbt::tag::NbtTag::Compound(mut namespace_data)) =
            custom_data.child_tags.remove(namespace)
        else {
            return;
        };

        namespace_data.child_tags.remove(key);
        if !namespace_data.is_empty() {
            custom_data.child_tags.insert(
                namespace.into(),
                pumpkin_nbt::tag::NbtTag::Compound(namespace_data),
            );
        }
        self.dirty.store(true, Ordering::Relaxed);
    }

    pub fn has_custom_data(&self, namespace: &str, key: &str) -> bool {
        self.get_custom_data(namespace, key).is_some()
    }
}

impl PathFromLevelFolder for ChunkEntityData {
    #[inline]
    fn file_path(folder: &LevelFolder, file_name: &str) -> PathBuf {
        folder.entities_folder.join(file_name)
    }
}

impl Dirtiable for ChunkEntityData {
    #[inline]
    fn mark_dirty(&self, flag: bool) {
        self.dirty.store(flag, Ordering::Relaxed);
    }

    #[inline]
    fn is_dirty(&self) -> bool {
        self.dirty.load(Ordering::Relaxed)
    }

    fn dirty_generation(&self) -> u64 {
        self.dirty.generation()
    }

    fn mark_persisted(&self, generation: u64) {
        self.dirty.mark_persisted(generation);
    }
}

impl SingleChunkDataSerializer for ChunkEntityData {
    #[inline]
    fn from_bytes(bytes: &Bytes, pos: Vector2<i32>) -> Result<Self, ChunkReadingError> {
        Self::internal_from_bytes(bytes, pos).map_err(ChunkReadingError::ParsingError)
    }

    #[inline]
    fn to_bytes(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<Bytes, ChunkSerializingError>> + Send + '_>> {
        Box::pin(async move { self.internal_to_bytes().await })
    }

    #[inline]
    fn position(&self) -> (i32, i32) {
        (self.x, self.z)
    }
}

impl ChunkEntityData {
    fn internal_from_bytes(
        chunk_data: &[u8],
        position: Vector2<i32>,
    ) -> Result<Self, ChunkParsingError> {
        let is_named = chunk_data.len() >= 3
            && chunk_data[0] == 0x0a
            && chunk_data[1] == 0x00
            && chunk_data[2] == 0x00;
        let mut cursor = std::io::Cursor::new(chunk_data);
        let mut reader = pumpkin_nbt::deserializer::NbtReadHelperJava::new(
            pumpkin_nbt::deserializer::NbtStreamReader(&mut cursor),
        );
        let nbt = if is_named {
            pumpkin_nbt::Nbt::read(&mut reader)
        } else {
            pumpkin_nbt::Nbt::read_unnamed(&mut reader)
        }
        .map_err(|e| ChunkParsingError::ErrorDeserializingChunk(e.to_string()))?;

        let mut root = nbt.root_tag;
        crate::world_info::schema::migrate_persistent_root(
            crate::world_info::schema::PersistentRootSchema::EntityChunk,
            &mut root,
        )
        .map_err(|error| ChunkParsingError::ErrorDeserializingChunk(error.to_string()))?;
        let pos_array = if let Some(pumpkin_nbt::tag::NbtTag::IntArray(pos)) = root.get("Position")
        {
            if pos.len() >= 2 {
                [pos[0], pos[1]]
            } else {
                [0, 0]
            }
        } else {
            [0, 0]
        };

        if pos_array[0] != position.x || pos_array[1] != position.y {
            return Err(ChunkParsingError::ErrorDeserializingChunk(format!(
                "Expected data for entity chunk {},{} but got it for {},{}!",
                position.x, position.y, pos_array[0], pos_array[1],
            )));
        }

        let residual_nbt = crate::persistence::bounded_residual_nbt(
            &root,
            ENTITY_CHUNK_AUTHORITATIVE_ROOT_KEYS.iter().copied(),
            MAX_ENTITY_CHUNK_RESIDUAL_NBT_ENTRIES,
            MAX_ENTITY_CHUNK_RESIDUAL_NBT_BYTES,
        )
        .unwrap_or_else(|error| {
            tracing::warn!(
                "discarding residual NBT for entity chunk {},{}: {error}",
                position.x,
                position.y
            );
            NbtCompound::new()
        });
        let entities = match root.child_tags.remove("Entities") {
            Some(pumpkin_nbt::tag::NbtTag::List(list)) => list
                .into_iter()
                .filter_map(|tag| match tag {
                    pumpkin_nbt::tag::NbtTag::Compound(compound) => Some(compound),
                    _ => None,
                })
                .collect(),
            _ => Vec::new(),
        };

        let chunk = Self::from_entities(position, entities);
        *chunk
            .residual_nbt
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = residual_nbt;
        Ok(chunk)
    }

    async fn internal_to_bytes(&self) -> Result<Bytes, ChunkSerializingError> {
        let snapshot_started = Instant::now();
        let mut root = self
            .residual_nbt
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        root.put_int("DataVersion", WORLD_DATA_VERSION);
        root.put(
            "Position",
            pumpkin_nbt::tag::NbtTag::IntArray(vec![self.x, self.z]),
        );
        let entities = self.entity_snapshot().await;
        serialization_metrics::record_duration(
            SerializationStage::Snapshot,
            snapshot_started.elapsed(),
        );
        let encode_started = Instant::now();
        let mut bytes = Vec::new();
        let mut writer = NbtWriteHelperJava::new(&mut bytes);
        writer.write_u8(COMPOUND_ID)?;
        writer.write_string("")?;
        root.serialize_entries(&mut writer)?;
        NbtCompound::serialize_compound_list_entry("Entities", entities.as_slice(), &mut writer)?;
        writer.write_u8(END_ID)?;
        serialization_metrics::record_duration(
            SerializationStage::Encode,
            encode_started.elapsed(),
        );
        Ok(bytes.into())
    }
}

#[derive(Clone)]
pub struct ChunkSectionBiomes {
    pub(crate) data: Option<Box<[i64]>>,
    pub(crate) palette: Box<[u8]>,
}

#[derive(Clone)]
pub struct ChunkSectionBlockStates {
    pub(crate) data: Option<Box<[i64]>>,
    pub(crate) palette: Box<[BlockStateId]>,
}

#[derive(Debug, Clone)]
pub enum LightContainer {
    Empty(u8),
    Full(Box<[u8]>),
}

impl LightContainer {
    pub const DIM: usize = 16;
    pub const ARRAY_SIZE: usize = Self::DIM * Self::DIM * Self::DIM / 2;

    #[must_use]
    pub fn new_empty(default: u8) -> Self {
        assert!(default <= 15, "Default value must be between 0 and 15");
        Self::Empty(default)
    }

    #[must_use]
    pub fn new(data: Box<[u8]>) -> Self {
        assert!(
            data.len() == Self::ARRAY_SIZE,
            "Data length must be {}",
            Self::ARRAY_SIZE
        );
        Self::Full(data)
    }

    #[must_use]
    pub fn new_filled(default: u8) -> Self {
        assert!(default <= 15, "Default value must be between 0 and 15");
        let value = default << 4 | default;
        Self::Full([value; Self::ARRAY_SIZE].into())
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        matches!(self, Self::Empty(_))
    }

    const fn index(x: usize, y: usize, z: usize) -> usize {
        y * 16 * 16 + z * 16 + x
    }

    #[must_use]
    pub fn get(&self, x: usize, y: usize, z: usize) -> u8 {
        match self {
            Self::Full(data) => {
                let index = Self::index(x, y, z);
                data[index >> 1] >> (4 * (index & 1)) & 0x0F
            }
            Self::Empty(default) => *default,
        }
    }

    pub fn set(&mut self, x: usize, y: usize, z: usize, value: u8) {
        match self {
            Self::Full(data) => {
                let index = Self::index(x, y, z);
                let mask = 0x0F << (4 * (index & 1));
                data[index >> 1] &= !mask;
                data[index >> 1] |= value << (4 * (index & 1));
            }
            Self::Empty(default) => {
                if value != *default {
                    *self = Self::new_filled(*default);
                    self.set(x, y, z, value);
                }
            }
        }
    }

    pub fn fill(&mut self, value: u8) {
        *self = Self::new_filled(value);
    }
}

impl Default for LightContainer {
    fn default() -> Self {
        Self::new_empty(15)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pumpkin_data::{Block, biome::Biome};
    use pumpkin_nbt::compound::NbtCompound;
    use pumpkin_nbt::tag::NbtTag;

    #[test]
    fn palette_identifiers_and_properties_reuse_shared_strings() {
        let oak_log = Block::OAK_LOG.default_state.id;
        let birch_log = Block::BIRCH_LOG.default_state.id;

        let first_name = shared_block_name(oak_log);
        let second_name = shared_block_name(oak_log);
        assert_eq!(first_name.as_ref(), "minecraft:oak_log");
        assert!(Arc::ptr_eq(&first_name, &second_name));

        let oak_axis = shared_block_properties(oak_log)
            .iter()
            .find(|(name, _)| name.as_ref() == "axis")
            .unwrap();
        let birch_axis = shared_block_properties(birch_log)
            .iter()
            .find(|(name, _)| name.as_ref() == "axis")
            .unwrap();
        assert!(Arc::ptr_eq(&oak_axis.0, &birch_axis.0));
        assert!(Arc::ptr_eq(&oak_axis.1, &birch_axis.1));

        let first_biome = shared_biome_name(Biome::PLAINS.id);
        let second_biome = shared_biome_name(Biome::PLAINS.id);
        assert_eq!(first_biome.as_ref(), "minecraft:plains");
        assert!(Arc::ptr_eq(&first_biome, &second_biome));
    }

    #[test]
    fn extract_u16_array_from_vanilla_compound_palette() {
        let mut entry1 = NbtCompound::new();
        entry1.put_string("Name", "minecraft:stone".to_string());

        let mut entry2 = NbtCompound::new();
        entry2.put_string("Name", "minecraft:repeater".to_string());
        let mut props = NbtCompound::new();
        props.put_string("facing", "north".to_string());
        props.put_string("delay", "2".to_string());
        props.put_string("locked", "false".to_string());
        props.put_string("powered", "false".to_string());
        entry2.put_compound("Properties", props);

        let list_tag = NbtTag::List(vec![NbtTag::Compound(entry1), NbtTag::Compound(entry2)]);
        let result = extract_u16_array(&list_tag).expect("should extract palette");

        assert_eq!(result.len(), 2);
        assert_eq!(result[0], Block::STONE.default_state.id);

        let repeater_state = Block::REPEATER
            .from_properties(&[
                ("facing", "north"),
                ("delay", "2"),
                ("locked", "false"),
                ("powered", "false"),
            ])
            .to_state_id(&Block::REPEATER);
        assert_eq!(result[1], repeater_state);
    }

    #[test]
    fn extract_u8_array_from_vanilla_string_palette() {
        let list_tag = NbtTag::List(vec![
            NbtTag::String("minecraft:plains".to_string().into()),
            NbtTag::String("minecraft:the_void".to_string().into()),
        ]);
        let result = extract_u8_array(&list_tag).expect("should extract biome palette");

        assert_eq!(result.len(), 2);
        assert_eq!(
            result[0],
            pumpkin_data::biome::Biome::from_name("plains").unwrap().id
        );
        assert_eq!(
            result[1],
            pumpkin_data::biome::Biome::from_name("the_void")
                .unwrap()
                .id
        );
    }

    #[test]
    fn future_chunk_data_version_is_rejected_before_decode() {
        let mut root = NbtCompound::new();
        root.put_int(
            "DataVersion",
            crate::world_info::MAXIMUM_SUPPORTED_WORLD_DATA_VERSION + 1,
        );
        let bytes = pumpkin_nbt::Nbt::from(root).write_unnamed().unwrap();

        let error = match ChunkData::internal_from_bytes(&bytes, Vector2::new(0, 0)) {
            Ok(_) => panic!("future chunk DataVersion should be rejected"),
            Err(error) => error,
        };
        assert!(
            error
                .to_string()
                .contains("Unsupported terrain chunk DataVersion")
        );
    }

    #[tokio::test]
    async fn entity_chunk_snapshot_is_immutable_and_round_trips_replacement() {
        let position = Vector2::new(3, -7);
        let mut pig = NbtCompound::new();
        pig.put_string("id", "minecraft:pig".to_owned());
        let chunk = ChunkEntityData::from_entities(position, vec![pig]);
        let original_snapshot = chunk.entity_snapshot().await;
        let original_generation = chunk.dirty.generation();

        let mut wolf = NbtCompound::new();
        wolf.put_string("id", "minecraft:wolf".to_owned());
        chunk.replace_entities(vec![wolf]).await;

        assert_eq!(original_snapshot[0].get_string("id"), Some("minecraft:pig"));
        assert!(chunk.dirty.generation() > original_generation);

        let bytes = chunk.internal_to_bytes().await.unwrap();
        let decoded = ChunkEntityData::internal_from_bytes(&bytes, position).unwrap();
        let decoded_entities = decoded.take_entities().await;
        assert_eq!(decoded_entities.len(), 1);
        assert_eq!(decoded_entities[0].get_string("id"), Some("minecraft:wolf"));
    }

    #[tokio::test]
    async fn unknown_chunk_roots_survive_disk_restart_round_trip() {
        fn read_named(bytes: &[u8]) -> NbtCompound {
            let mut cursor = std::io::Cursor::new(bytes);
            pumpkin_nbt::Nbt::read(&mut pumpkin_nbt::deserializer::NbtReadHelperJava::new(
                &mut cursor,
            ))
            .unwrap()
            .root_tag
        }

        let position = Vector2::new(0, 0);
        let terrain = ChunkData::empty(0, 0);
        let mut terrain_root = read_named(&terrain.internal_to_bytes().unwrap());
        terrain_root.put_string("plugin:terrain_marker", "kept");
        let terrain_bytes = pumpkin_nbt::Nbt::from(terrain_root).write().unwrap();
        let terrain = ChunkData::internal_from_bytes(&terrain_bytes, position).unwrap();
        let terrain_after_restart = read_named(&terrain.internal_to_bytes().unwrap());
        assert_eq!(
            terrain_after_restart.get_string("plugin:terrain_marker"),
            Some("kept")
        );
        assert_eq!(terrain_after_restart.get_int("xPos"), Some(0));

        let mut pig = NbtCompound::new();
        pig.put_string("id", "minecraft:pig");
        let entities = ChunkEntityData::from_entities(position, vec![pig]);
        let mut entity_root = read_named(&entities.internal_to_bytes().await.unwrap());
        entity_root.put_string("plugin:entity_chunk_marker", "kept");
        entity_root.put_int("Position-X", 0);
        entity_root.put_int("Position-Z", 0);
        let entity_bytes = pumpkin_nbt::Nbt::from(entity_root).write().unwrap();
        let entities = ChunkEntityData::internal_from_bytes(&entity_bytes, position).unwrap();
        let entity_after_restart = read_named(&entities.internal_to_bytes().await.unwrap());
        assert_eq!(
            entity_after_restart.get_string("plugin:entity_chunk_marker"),
            Some("kept")
        );
        assert!(entity_after_restart.get("Position-X").is_none());
        assert!(entity_after_restart.get("Position-Z").is_none());
        assert_eq!(
            entity_after_restart.get("Position"),
            Some(&NbtTag::IntArray(vec![0, 0]))
        );
    }

    #[tokio::test]
    async fn mixed_entity_payload_round_trips_brain_equipment_and_passengers() {
        let mut simple = NbtCompound::new();
        simple.put_string("id", "minecraft:pig".to_owned());

        let mut memories = NbtCompound::new();
        memories.put_long("minecraft:home", 42);
        let mut brain = NbtCompound::new();
        brain.put_compound("memories", memories);
        let mut brain_entity = NbtCompound::new();
        brain_entity.put_string("id", "minecraft:villager".to_owned());
        brain_entity.put_compound("Brain", brain);

        let mut helmet = NbtCompound::new();
        helmet.put_string("id", "minecraft:iron_helmet".to_owned());
        let mut sword = NbtCompound::new();
        sword.put_string("id", "minecraft:iron_sword".to_owned());
        let mut equipped = NbtCompound::new();
        equipped.put_string("id", "minecraft:zombie".to_owned());
        equipped.put_list("ArmorItems", vec![NbtTag::Compound(helmet)]);
        equipped.put_list("HandItems", vec![NbtTag::Compound(sword)]);

        let mut rider = NbtCompound::new();
        rider.put_string("id", "minecraft:chicken".to_owned());
        let mut vehicle = NbtCompound::new();
        vehicle.put_string("id", "minecraft:boat".to_owned());
        vehicle.put_list("Passengers", vec![NbtTag::Compound(rider)]);

        let entities = vec![simple, brain_entity, equipped, vehicle];
        let position = Vector2::new(-11, 9);
        let chunk = ChunkEntityData::from_entities(position, entities.clone());

        let bytes = chunk.internal_to_bytes().await.unwrap();
        let decoded = ChunkEntityData::internal_from_bytes(&bytes, position).unwrap();

        assert_eq!(decoded.take_entities().await, entities);
    }
}
