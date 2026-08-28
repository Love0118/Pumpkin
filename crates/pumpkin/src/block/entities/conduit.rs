use super::BlockEntity;
use pumpkin_nbt::compound::NbtCompound;
use pumpkin_nbt::tag::NbtTag;
use pumpkin_util::math::position::BlockPos;
use std::pin::Pin;
use tokio::sync::Mutex;

pub struct ConduitBlockEntity {
    pub position: BlockPos,
    state: Mutex<ConduitState>,
}

#[derive(Clone, Default)]
struct ConduitState {
    active: bool,
    target: Option<NbtTag>,
}

impl ConduitState {
    fn write_nbt(self, nbt: &mut NbtCompound) {
        nbt.put_bool("Active", self.active);
        if let Some(target) = self.target {
            nbt.put("Target", target);
        }
    }
}

impl BlockEntity for ConduitBlockEntity {
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
        let active = nbt.get_bool("Active").unwrap_or(false);
        let target = nbt.get("Target").cloned();
        Self {
            position,
            state: Mutex::new(ConduitState { active, target }),
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

impl ConduitBlockEntity {
    pub const ID: &'static str = "minecraft:conduit";
    #[must_use]
    pub fn new(position: BlockPos) -> Self {
        Self {
            position,
            state: Mutex::new(ConduitState::default()),
        }
    }
}
