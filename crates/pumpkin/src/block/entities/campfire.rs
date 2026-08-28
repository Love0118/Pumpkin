use super::BlockEntity;
use pumpkin_data::item_stack::ItemStack;
use pumpkin_nbt::compound::NbtCompound;
use pumpkin_nbt::tag::NbtTag;
use pumpkin_util::math::position::BlockPos;
use std::pin::Pin;
use tokio::sync::Mutex;

#[derive(Clone)]
struct CampfireState {
    items: [ItemStack; 4],
    cooking_times: [i32; 4],
    cooking_total_times: [i32; 4],
}

pub struct CampfireBlockEntity {
    pub position: BlockPos,
    state: Mutex<CampfireState>,
}

impl BlockEntity for CampfireBlockEntity {
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
        let mut items = std::array::from_fn(|_| ItemStack::EMPTY.clone());
        if let Some(list) = nbt.get_list("Items") {
            for tag in list {
                if let Some(compound) = tag.extract_compound() {
                    let slot = compound.get_byte("Slot").unwrap_or(0) as usize;
                    if slot < 4
                        && let Some(stack) = ItemStack::read_item_stack(compound)
                    {
                        items[slot] = stack;
                    }
                }
            }
        }
        let mut cooking_times = [0; 4];
        if let Some(arr) = nbt.get_int_array("CookingTimes") {
            for (i, &val) in arr.iter().enumerate().take(4) {
                cooking_times[i] = val;
            }
        }
        let mut cooking_total_times = [0; 4];
        if let Some(arr) = nbt.get_int_array("CookingTotalTimes") {
            for (i, &val) in arr.iter().enumerate().take(4) {
                cooking_total_times[i] = val;
            }
        }

        Self {
            position,
            state: Mutex::new(CampfireState {
                items,
                cooking_times,
                cooking_total_times,
            }),
        }
    }

    fn write_nbt<'a>(
        &'a self,
        nbt: &'a mut NbtCompound,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
        Box::pin(async move {
            let snapshot = self.state.lock().await.clone();
            let mut list = Vec::new();
            for (i, stack) in snapshot.items.iter().enumerate() {
                if !stack.is_empty() {
                    let mut item_nbt = NbtCompound::new();
                    item_nbt.put_byte("Slot", i as i8);
                    stack.write_item_stack(&mut item_nbt);
                    list.push(NbtTag::Compound(item_nbt));
                }
            }
            nbt.put_list("Items", list);

            nbt.put(
                "CookingTimes",
                NbtTag::IntArray(snapshot.cooking_times.into()),
            );
            nbt.put(
                "CookingTotalTimes",
                NbtTag::IntArray(snapshot.cooking_total_times.into()),
            );
        })
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

impl CampfireBlockEntity {
    pub const ID: &'static str = "minecraft:campfire";
    #[must_use]
    pub fn new(position: BlockPos) -> Self {
        Self {
            position,
            state: Mutex::new(CampfireState {
                items: std::array::from_fn(|_| ItemStack::EMPTY.clone()),
                cooking_times: [0; 4],
                cooking_total_times: [0; 4],
            }),
        }
    }
}
