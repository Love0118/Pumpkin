use super::BlockEntity;
use pumpkin_nbt::compound::NbtCompound;
use pumpkin_util::math::position::BlockPos;
use std::pin::Pin;
use tokio::sync::Mutex;

pub struct SkullBlockEntity {
    pub position: BlockPos,
    state: Mutex<SkullState>,
}

#[derive(Clone, Default)]
struct SkullState {
    note_block_sound: Option<String>,
    profile: Option<NbtCompound>,
}

impl SkullState {
    fn write_nbt(self, nbt: &mut NbtCompound) {
        if let Some(sound) = self.note_block_sound {
            nbt.put_string("note_block_sound", sound);
        }
        if let Some(profile) = self.profile {
            nbt.put_compound("profile", profile);
        }
    }
}

impl BlockEntity for SkullBlockEntity {
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
        let note_block_sound = nbt.get_string("note_block_sound").map(ToString::to_string);
        let profile = nbt.get_compound("profile").cloned();
        Self {
            position,
            state: Mutex::new(SkullState {
                note_block_sound,
                profile,
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

impl SkullBlockEntity {
    pub const ID: &'static str = "minecraft:skull";
    #[must_use]
    pub const fn new(position: BlockPos) -> Self {
        Self {
            position,
            state: Mutex::const_new(SkullState {
                note_block_sound: None,
                profile: None,
            }),
        }
    }

    pub fn try_note_block_sound(&self) -> Option<String> {
        self.state
            .try_lock()
            .ok()
            .and_then(|state| state.note_block_sound.clone())
    }
}
