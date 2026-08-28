use super::BlockEntity;
use pumpkin_nbt::compound::NbtCompound;
use pumpkin_util::math::position::BlockPos;
use std::pin::Pin;
use tokio::sync::Mutex;

#[derive(Clone)]
struct StructureBlockState {
    name: String,
    author: String,
    metadata: String,
    pos: [i32; 3],
    size: [i32; 3],
    rotation: String,
    mirror: String,
    mode: String,
    ignore_entities: bool,
    show_air: bool,
    show_bounding_box: bool,
    integrity: f32,
    seed: i64,
}

impl StructureBlockState {
    fn write_nbt(&self, nbt: &mut NbtCompound) {
        nbt.put_string("name", self.name.clone());
        nbt.put_string("author", self.author.clone());
        nbt.put_string("metadata", self.metadata.clone());
        nbt.put_int("posX", self.pos[0]);
        nbt.put_int("posY", self.pos[1]);
        nbt.put_int("posZ", self.pos[2]);
        nbt.put_int("sizeX", self.size[0]);
        nbt.put_int("sizeY", self.size[1]);
        nbt.put_int("sizeZ", self.size[2]);
        nbt.put_string("rotation", self.rotation.clone());
        nbt.put_string("mirror", self.mirror.clone());
        nbt.put_string("mode", self.mode.clone());
        nbt.put_bool("ignoreEntities", self.ignore_entities);
        nbt.put_bool("showAir", self.show_air);
        nbt.put_bool("showBoundingBox", self.show_bounding_box);
        nbt.put_float("integrity", self.integrity);
        nbt.put_long("seed", self.seed);
    }
}

pub struct StructureBlockBlockEntity {
    pub position: BlockPos,
    state: Mutex<StructureBlockState>,
}

impl BlockEntity for StructureBlockBlockEntity {
    fn resource_location(&self) -> &'static str {
        Self::ID
    }

    fn get_position(&self) -> BlockPos {
        self.position
    }

    fn from_nbt(nbt: &pumpkin_nbt::compound::NbtCompound, position: BlockPos) -> Self
    where
        Self: Sized,
    {
        Self {
            position,
            state: Mutex::new(StructureBlockState {
                name: nbt.get_string("name").unwrap_or("").to_string(),
                author: nbt.get_string("author").unwrap_or("").to_string(),
                metadata: nbt.get_string("metadata").unwrap_or("").to_string(),
                pos: [
                    nbt.get_int("posX").unwrap_or(0),
                    nbt.get_int("posY").unwrap_or(0),
                    nbt.get_int("posZ").unwrap_or(0),
                ],
                size: [
                    nbt.get_int("sizeX").unwrap_or(0),
                    nbt.get_int("sizeY").unwrap_or(0),
                    nbt.get_int("sizeZ").unwrap_or(0),
                ],
                rotation: nbt.get_string("rotation").unwrap_or("NONE").to_string(),
                mirror: nbt.get_string("mirror").unwrap_or("NONE").to_string(),
                mode: nbt.get_string("mode").unwrap_or("DATA").to_string(),
                ignore_entities: nbt.get_bool("ignoreEntities").unwrap_or(true),
                show_air: nbt.get_bool("showAir").unwrap_or(false),
                show_bounding_box: nbt.get_bool("showBoundingBox").unwrap_or(true),
                integrity: nbt.get_float("integrity").unwrap_or(1.0),
                seed: nbt.get_long("seed").unwrap_or(0),
            }),
        }
    }

    fn write_nbt<'a>(
        &'a self,
        nbt: &'a mut NbtCompound,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
        Box::pin(async move {
            self.state.lock().await.clone().write_nbt(nbt);
        })
    }

    fn chunk_data_nbt(&self) -> Option<NbtCompound> {
        let snapshot = self.state.try_lock().ok()?.clone();
        let mut nbt = NbtCompound::new();
        snapshot.write_nbt(&mut nbt);
        Some(nbt)
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

impl StructureBlockBlockEntity {
    pub const ID: &'static str = "minecraft:structure_block";
    #[must_use]
    pub fn new(position: BlockPos) -> Self {
        Self {
            position,
            state: Mutex::new(StructureBlockState {
                name: String::new(),
                author: String::new(),
                metadata: String::new(),
                pos: [0; 3],
                size: [0; 3],
                rotation: "NONE".to_string(),
                mirror: "NONE".to_string(),
                mode: "DATA".to_string(),
                ignore_entities: true,
                show_air: false,
                show_bounding_box: true,
                integrity: 1.0,
                seed: 0,
            }),
        }
    }

    pub fn try_name(&self) -> Option<String> {
        self.state.try_lock().ok().map(|state| state.name.clone())
    }

    pub fn try_author(&self) -> Option<String> {
        self.state.try_lock().ok().map(|state| state.author.clone())
    }

    pub fn try_mode(&self) -> Option<String> {
        self.state.try_lock().ok().map(|state| state.mode.clone())
    }

    pub fn try_integrity(&self) -> Option<f32> {
        self.state.try_lock().ok().map(|state| state.integrity)
    }

    pub fn try_seed(&self) -> Option<i64> {
        self.state.try_lock().ok().map(|state| state.seed)
    }
}
