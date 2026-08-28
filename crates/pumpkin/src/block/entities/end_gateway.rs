use super::BlockEntity;
use pumpkin_nbt::compound::NbtCompound;
use pumpkin_util::math::position::BlockPos;
use std::pin::Pin;
use tokio::sync::Mutex;

pub struct EndGatewayBlockEntity {
    pub position: BlockPos,
    state: Mutex<EndGatewayState>,
}

#[derive(Clone, Copy, Default)]
struct EndGatewayState {
    age: i64,
    exact_teleport: bool,
    exit_portal: Option<BlockPos>,
}

impl EndGatewayState {
    fn write_nbt(self, nbt: &mut NbtCompound) {
        nbt.put_long("Age", self.age);
        nbt.put_bool("ExactTeleport", self.exact_teleport);
        if let Some(exit) = self.exit_portal {
            let mut exit_nbt = NbtCompound::new();
            exit_nbt.put_int("X", exit.0.x);
            exit_nbt.put_int("Y", exit.0.y);
            exit_nbt.put_int("Z", exit.0.z);
            nbt.put_compound("ExitPortal", exit_nbt);
        }
    }
}

impl BlockEntity for EndGatewayBlockEntity {
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
        let age = nbt.get_long("Age").unwrap_or(0);
        let exact_teleport = nbt.get_bool("ExactTeleport").unwrap_or(false);
        let exit_portal = nbt.get_compound("ExitPortal").map(|c| {
            BlockPos::new(
                c.get_int("X").unwrap_or(0),
                c.get_int("Y").unwrap_or(0),
                c.get_int("Z").unwrap_or(0),
            )
        });
        Self {
            position,
            state: Mutex::new(EndGatewayState {
                age,
                exact_teleport,
                exit_portal,
            }),
        }
    }

    fn write_nbt<'a>(
        &'a self,
        nbt: &'a mut NbtCompound,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
        Box::pin(async move {
            (*self.state.lock().await).write_nbt(nbt);
        })
    }

    fn chunk_data_nbt(&self) -> Option<NbtCompound> {
        let mut nbt = NbtCompound::new();
        (*self.state.try_lock().ok()?).write_nbt(&mut nbt);
        Some(nbt)
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

impl EndGatewayBlockEntity {
    pub const ID: &'static str = "minecraft:end_gateway";
    #[must_use]
    pub fn new(position: BlockPos) -> Self {
        Self {
            position,
            state: Mutex::new(EndGatewayState::default()),
        }
    }

    pub fn try_age(&self) -> Option<i64> {
        self.state.try_lock().ok().map(|state| state.age)
    }

    pub fn try_exact_teleport(&self) -> Option<bool> {
        self.state.try_lock().ok().map(|state| state.exact_teleport)
    }
}
