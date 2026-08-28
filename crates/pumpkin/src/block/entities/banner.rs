use super::BlockEntity;
use pumpkin_nbt::compound::NbtCompound;
use pumpkin_nbt::tag::NbtTag;
use pumpkin_util::math::position::BlockPos;
use std::pin::Pin;
use tokio::sync::Mutex;

pub struct BannerBlockEntity {
    pub position: BlockPos,
    state: Mutex<BannerState>,
}

#[derive(Clone, Default)]
struct BannerState {
    custom_name: Option<String>,
    patterns: Option<Vec<NbtTag>>,
}

impl BannerState {
    fn write_nbt(self, nbt: &mut NbtCompound) {
        if let Some(name) = self.custom_name {
            nbt.put_string("CustomName", name);
        }
        if let Some(patterns) = self.patterns {
            nbt.put_list("patterns", patterns);
        }
    }
}

impl BlockEntity for BannerBlockEntity {
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
        let custom_name = nbt.get_string("CustomName").map(ToString::to_string);
        let patterns = nbt.get_list("patterns").map(<[_]>::to_vec);
        Self {
            position,
            state: Mutex::new(BannerState {
                custom_name,
                patterns,
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

impl BannerBlockEntity {
    pub const ID: &'static str = "minecraft:banner";
    #[must_use]
    pub const fn new(position: BlockPos) -> Self {
        Self {
            position,
            state: Mutex::const_new(BannerState {
                custom_name: None,
                patterns: None,
            }),
        }
    }

    pub fn try_custom_name(&self) -> Option<String> {
        self.state
            .try_lock()
            .ok()
            .and_then(|state| state.custom_name.clone())
    }
}
