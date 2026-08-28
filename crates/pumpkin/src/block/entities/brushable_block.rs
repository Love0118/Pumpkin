use super::BlockEntity;
use pumpkin_data::item_stack::ItemStack;
use pumpkin_nbt::compound::NbtCompound;
use pumpkin_util::math::position::BlockPos;
use std::pin::Pin;
use tokio::sync::Mutex;

pub struct BrushableBlockBlockEntity {
    pub position: BlockPos,
    state: Mutex<BrushableBlockState>,
}

#[derive(Clone, Default)]
struct BrushableBlockState {
    item: Option<ItemStack>,
    hits: i32,
    direction: u8,
}

impl BrushableBlockState {
    fn write_nbt(self, nbt: &mut NbtCompound) {
        if let Some(item) = self.item {
            let mut item_nbt = NbtCompound::new();
            item.write_item_stack(&mut item_nbt);
            nbt.put_compound("item", item_nbt);
        }
        nbt.put_int("hits", self.hits);
        nbt.put_byte("direction", self.direction as i8);
    }
}

impl BlockEntity for BrushableBlockBlockEntity {
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
        let item = nbt
            .get_compound("item")
            .and_then(ItemStack::read_item_stack);
        let hits = nbt.get_int("hits").unwrap_or(0);
        let direction = nbt.get_byte("direction").unwrap_or(0) as u8;
        Self {
            position,
            state: Mutex::new(BrushableBlockState {
                item,
                hits,
                direction,
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

impl BrushableBlockBlockEntity {
    pub const ID: &'static str = "minecraft:brushable_block";
    #[must_use]
    pub const fn new(position: BlockPos) -> Self {
        Self {
            position,
            state: Mutex::const_new(BrushableBlockState {
                item: None,
                hits: 0,
                direction: 0,
            }),
        }
    }

    pub async fn apply_brush_hit(&self) -> (i32, Option<ItemStack>) {
        let mut state = self.state.lock().await;
        state.hits += 1;
        let item = (state.hits >= 4).then(|| state.item.take()).flatten();
        (state.hits, item)
    }

    pub async fn take_item(&self) -> Option<ItemStack> {
        self.state.lock().await.item.take()
    }
}
