use super::BlockEntity;
use pumpkin_data::item_stack::ItemStack;
use pumpkin_nbt::compound::NbtCompound;
use pumpkin_nbt::tag::NbtTag;
use pumpkin_util::math::position::BlockPos;
use std::pin::Pin;
use tokio::sync::Mutex;

pub struct DecoratedPotBlockEntity {
    pub position: BlockPos,
    state: Mutex<DecoratedPotState>,
}

#[derive(Clone, Default)]
struct DecoratedPotState {
    sherds: Option<Vec<NbtTag>>,
    item: Option<ItemStack>,
}

impl DecoratedPotState {
    fn write_nbt(self, nbt: &mut NbtCompound) {
        if let Some(sherds) = self.sherds {
            nbt.put_list("sherds", sherds);
        }
        if let Some(item) = self.item {
            let mut item_nbt = NbtCompound::new();
            item.write_item_stack(&mut item_nbt);
            nbt.put_compound("item", item_nbt);
        }
    }
}

impl BlockEntity for DecoratedPotBlockEntity {
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
        let sherds = nbt.get_list("sherds").map(<[_]>::to_vec);
        let item = nbt
            .get_compound("item")
            .and_then(ItemStack::read_item_stack);
        Self {
            position,
            state: Mutex::new(DecoratedPotState { sherds, item }),
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

impl DecoratedPotBlockEntity {
    pub const ID: &'static str = "minecraft:decorated_pot";

    #[must_use]
    pub const fn new(position: BlockPos) -> Self {
        Self {
            position,
            state: Mutex::const_new(DecoratedPotState {
                sherds: None,
                item: None,
            }),
        }
    }

    pub async fn get_item(&self) -> Option<ItemStack> {
        self.state.lock().await.item.clone()
    }

    pub async fn take_item(&self) -> Option<ItemStack> {
        self.state.lock().await.item.take()
    }

    pub async fn try_insert_item(&self, stack: &mut ItemStack, count: u8) -> bool {
        let mut state = self.state.lock().await;
        if let Some(existing) = state.item.as_mut() {
            if existing.item.id == stack.item.id {
                let add = count.min(64 - existing.item_count);
                if add > 0 {
                    existing.item_count += add;
                    stack.item_count -= add;
                    return true;
                }
            }
            false
        } else {
            let insert_count = count.min(stack.item_count);
            let mut inserted = stack.clone();
            inserted.item_count = insert_count;
            state.item = Some(inserted);
            stack.item_count -= insert_count;
            true
        }
    }

    pub async fn get_comparator_output(&self) -> u8 {
        self.state.lock().await.item.as_ref().map_or(0, |item| {
            if item.item_count == 0 {
                0
            } else {
                let max_count = 64f32;
                1 + ((item.item_count as f32 / max_count) * 14.0).floor() as u8
            }
        })
    }
}
