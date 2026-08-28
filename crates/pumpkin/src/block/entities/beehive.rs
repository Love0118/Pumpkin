use super::BlockEntity;
use pumpkin_nbt::compound::NbtCompound;
use pumpkin_nbt::tag::NbtTag;
use pumpkin_util::math::position::BlockPos;
use std::pin::Pin;
use tokio::sync::Mutex;

pub struct BeehiveBlockEntity {
    pub position: BlockPos,
    state: Mutex<BeehiveState>,
}

#[derive(Clone, Default)]
struct BeehiveState {
    bees: Option<Vec<NbtTag>>,
    flower_pos: Option<BlockPos>,
}

impl BeehiveState {
    fn write_nbt(self, nbt: &mut NbtCompound) {
        if let Some(bees) = self.bees {
            nbt.put_list("Bees", bees);
        }
        if let Some(flower_pos) = self.flower_pos {
            let mut flower_nbt = NbtCompound::new();
            flower_nbt.put_int("X", flower_pos.0.x);
            flower_nbt.put_int("Y", flower_pos.0.y);
            flower_nbt.put_int("Z", flower_pos.0.z);
            nbt.put_compound("FlowerPos", flower_nbt);
        }
    }
}

impl BlockEntity for BeehiveBlockEntity {
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
        let bees = nbt.get_list("Bees").map(<[_]>::to_vec);
        let flower_pos = nbt.get_compound("FlowerPos").map(|c| {
            BlockPos::new(
                c.get_int("X").unwrap_or(0),
                c.get_int("Y").unwrap_or(0),
                c.get_int("Z").unwrap_or(0),
            )
        });
        Self {
            position,
            state: Mutex::new(BeehiveState { bees, flower_pos }),
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
        let mut nbt = NbtCompound::new();
        self.state.try_lock().ok()?.clone().write_nbt(&mut nbt);
        Some(nbt)
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

impl BeehiveBlockEntity {
    pub const ID: &'static str = "minecraft:beehive";
    #[must_use]
    pub const fn new(position: BlockPos) -> Self {
        Self {
            position,
            state: Mutex::const_new(BeehiveState {
                bees: None,
                flower_pos: None,
            }),
        }
    }

    pub fn try_bee_count(&self) -> Option<usize> {
        self.state
            .try_lock()
            .ok()
            .map(|state| state.bees.as_ref().map_or(0, Vec::len))
    }
}
