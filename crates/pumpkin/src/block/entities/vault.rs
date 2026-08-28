use super::BlockEntity;
use pumpkin_nbt::compound::NbtCompound;
use pumpkin_util::math::position::BlockPos;
use std::collections::HashSet;
use std::pin::Pin;
use tokio::sync::Mutex;
use uuid::Uuid;

pub struct VaultBlockEntity {
    pub position: BlockPos,
    state: Mutex<VaultState>,
}

#[derive(Clone, Default)]
struct VaultState {
    config: Option<NbtCompound>,
    server_data: Option<NbtCompound>,
    rewarded_players: HashSet<Uuid>,
}

impl VaultState {
    fn write_nbt(self, nbt: &mut NbtCompound) {
        if let Some(config) = self.config {
            nbt.put_compound("config", config);
        }
        if let Some(server_data) = self.server_data {
            nbt.put_compound("server_data", server_data);
        }
    }
}

impl BlockEntity for VaultBlockEntity {
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
            state: Mutex::new(VaultState {
                config: nbt.get_compound("config").cloned(),
                server_data: nbt.get_compound("server_data").cloned(),
                rewarded_players: HashSet::new(),
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

impl VaultBlockEntity {
    pub const ID: &'static str = "minecraft:vault";

    #[must_use]
    pub fn new(position: BlockPos) -> Self {
        Self {
            position,
            state: Mutex::new(VaultState::default()),
        }
    }

    pub async fn has_rewarded(&self, player_id: &Uuid) -> bool {
        self.state.lock().await.rewarded_players.contains(player_id)
    }

    pub async fn mark_rewarded(&self, player_id: Uuid) {
        self.state.lock().await.rewarded_players.insert(player_id);
    }
}
