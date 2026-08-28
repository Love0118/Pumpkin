use super::BlockEntity;
use pumpkin_nbt::compound::NbtCompound;
use pumpkin_util::math::position::BlockPos;
use std::pin::Pin;
use tokio::sync::Mutex;

pub struct TrialSpawnerBlockEntity {
    pub position: BlockPos,
    state: Mutex<TrialSpawnerState>,
}

#[derive(Clone, Default)]
struct TrialSpawnerState {
    normal_config: Option<NbtCompound>,
    ominous_config: Option<NbtCompound>,
    spawner_data: Option<NbtCompound>,
}

impl TrialSpawnerState {
    fn write_nbt(self, nbt: &mut NbtCompound) {
        if let Some(config) = self.normal_config {
            nbt.put_compound("normal_config", config);
        }
        if let Some(config) = self.ominous_config {
            nbt.put_compound("ominous_config", config);
        }
        if let Some(data) = self.spawner_data {
            nbt.put_compound("spawner_data", data);
        }
    }
}

impl BlockEntity for TrialSpawnerBlockEntity {
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
            state: Mutex::new(TrialSpawnerState {
                normal_config: nbt.get_compound("normal_config").cloned(),
                ominous_config: nbt.get_compound("ominous_config").cloned(),
                spawner_data: nbt.get_compound("spawner_data").cloned(),
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
        let mut nbt = NbtCompound::new();
        self.state.try_lock().ok()?.clone().write_nbt(&mut nbt);
        Some(nbt)
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

impl TrialSpawnerBlockEntity {
    pub const ID: &'static str = "minecraft:trial_spawner";
    #[must_use]
    pub const fn new(position: BlockPos) -> Self {
        Self {
            position,
            state: Mutex::const_new(TrialSpawnerState {
                normal_config: None,
                ominous_config: None,
                spawner_data: None,
            }),
        }
    }
}
