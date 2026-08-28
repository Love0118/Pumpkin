use rand::Rng;
use std::{
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use futures::Future;
use pumpkin_data::block_properties::{BlockProperties, JigsawLikeProperties, Orientation};
use pumpkin_nbt::compound::NbtCompound;
use pumpkin_util::{
    math::position::BlockPos,
    random::{RandomGenerator, xoroshiro128::Xoroshiro},
};
use pumpkin_world::generation::structure::structures::{
    StructureGeneratorContext, StructurePosition,
    jigsaw::{JigsawJointType, PoolElementStructurePiece},
    jigsaw_placement::{
        DimensionPadding, JigsawPlacement, LiquidSettings, MaxDistance, PoolAliasLookup,
    },
};

use tokio::sync::Mutex;

use crate::block::blocks::jigsaw::JigsawBlock;
use crate::world::World;

use super::BlockEntity;

pub struct JigsawBlockEntity {
    pub position: BlockPos,
    state: Mutex<JigsawState>,
    pub dirty: AtomicBool,
}

#[derive(Clone)]
struct JigsawState {
    name: String,
    target: String,
    pool: String,
    final_state: String,
    joint: JigsawJointType,
    selection_priority: i32,
    placement_priority: i32,
}

impl JigsawState {
    fn write_nbt(&self, nbt: &mut NbtCompound) {
        nbt.put_string(JigsawBlockEntity::NAME, self.name.clone());
        nbt.put_string(JigsawBlockEntity::TARGET, self.target.clone());
        nbt.put_string(JigsawBlockEntity::POOL, self.pool.clone());
        nbt.put_string(JigsawBlockEntity::FINAL_STATE, self.final_state.clone());
        nbt.put_string(JigsawBlockEntity::JOINT, self.joint.as_str());
        nbt.put_int(
            JigsawBlockEntity::PLACEMENT_PRIORITY,
            self.placement_priority,
        );
        nbt.put_int(
            JigsawBlockEntity::SELECTION_PRIORITY,
            self.selection_priority,
        );
    }
}

impl JigsawBlockEntity {
    pub const ID: &'static str = "minecraft:jigsaw";
    pub const EMPTY_ID: &'static str = "minecraft:empty";
    pub const DEFAULT_FINAL_STATE: &'static str = "minecraft:air";
    pub const DEFAULT_PLACEMENT_PRIORITY: i32 = 0;
    pub const DEFAULT_SELECTION_PRIORITY: i32 = 0;
    pub const NAME: &'static str = "name";
    pub const TARGET: &'static str = "target";
    pub const POOL: &'static str = "pool";
    pub const FINAL_STATE: &'static str = "final_state";
    pub const JOINT: &'static str = "joint";
    pub const PLACEMENT_PRIORITY: &'static str = "placement_priority";
    pub const SELECTION_PRIORITY: &'static str = "selection_priority";

    #[must_use]
    pub fn new(position: BlockPos) -> Self {
        Self {
            position,
            state: Mutex::new(JigsawState {
                name: Self::EMPTY_ID.to_string(),
                target: Self::EMPTY_ID.to_string(),
                pool: Self::EMPTY_ID.to_string(),
                final_state: Self::DEFAULT_FINAL_STATE.to_string(),
                joint: JigsawJointType::Rollable,
                selection_priority: Self::DEFAULT_SELECTION_PRIORITY,
                placement_priority: Self::DEFAULT_PLACEMENT_PRIORITY,
            }),
            dirty: AtomicBool::new(false),
        }
    }

    #[must_use]
    pub const fn get_default_joint_type(orientation: Orientation) -> JigsawJointType {
        let front = JigsawBlock::get_front_facing(orientation);
        if front.is_horizontal() {
            JigsawJointType::Aligned
        } else {
            JigsawJointType::Rollable
        }
    }

    pub async fn generate(&self, world: &Arc<World>, levels: i32, keep_jigsaws: bool) {
        let state = self.state.lock().await.clone();

        let block_state = world.get_block_state(&self.position);
        let props =
            JigsawLikeProperties::from_state_id(block_state.id, &pumpkin_data::Block::JIGSAW);
        let front = JigsawBlock::get_front_facing(props.r#orientation);

        let position = self.position.offset(front.to_offset());

        let structure = {
            let mut context = StructureGeneratorContext {
                seed: world.level_info.load().world_gen_settings.seed,
                chunk_x: position.chunk_position().x,
                chunk_z: position.chunk_position().y,
                random: RandomGenerator::Xoroshiro(Xoroshiro::from_seed(rand::rng().next_u64())),
                sea_level: 63,
                min_y: -64,
                height_sampler: None,
                structure_key: None,
            };

            JigsawPlacement::add_pieces(
                &mut context,
                &state.pool,
                Some(&state.target),
                levels,
                position,
                false,
                false,
                &MaxDistance::new(128),
                DimensionPadding::ZERO,
                LiquidSettings::ApplyWaterlog,
                &PoolAliasLookup::default(),
            )
        };

        if let Some(structure) = structure {
            self.place_structure(world, structure, keep_jigsaws).await;
        }
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "the atomic vanilla jigsaw configuration contains these seven protocol fields"
    )]
    pub async fn update_configuration(
        &self,
        name: String,
        target: String,
        pool: String,
        final_state: String,
        joint: JigsawJointType,
        selection_priority: i32,
        placement_priority: i32,
    ) {
        *self.state.lock().await = JigsawState {
            name,
            target,
            pool,
            final_state,
            joint,
            selection_priority,
            placement_priority,
        };
        self.dirty.store(true, Ordering::Relaxed);
    }

    pub fn try_name(&self) -> Option<String> {
        self.state.try_lock().ok().map(|state| state.name.clone())
    }

    pub fn try_target(&self) -> Option<String> {
        self.state.try_lock().ok().map(|state| state.target.clone())
    }

    pub fn try_pool(&self) -> Option<String> {
        self.state.try_lock().ok().map(|state| state.pool.clone())
    }

    pub fn try_final_state(&self) -> Option<String> {
        self.state
            .try_lock()
            .ok()
            .map(|state| state.final_state.clone())
    }

    pub fn try_selection_priority(&self) -> Option<i32> {
        self.state
            .try_lock()
            .ok()
            .map(|state| state.selection_priority)
    }

    pub fn try_placement_priority(&self) -> Option<i32> {
        self.state
            .try_lock()
            .ok()
            .map(|state| state.placement_priority)
    }

    async fn place_structure(
        &self,
        world: &Arc<World>,
        structure: StructurePosition,
        keep_jigsaws: bool,
    ) {
        let mut pieces = std::mem::take(
            &mut structure
                .collector
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .pieces,
        );
        let mut placer = crate::world::block_placer::WorldBlockPlacer::new(world);
        for piece in &mut pieces {
            if let Some(pool_piece) = piece.as_any().downcast_ref::<PoolElementStructurePiece>() {
                pumpkin_world::generation::structure::structures::jigsaw::place_pool_element_templates(
                    pool_piece,
                    &mut placer,
                    None,
                    keep_jigsaws,
                );
            }
        }
        placer.finalize();
        world.queue_block_updates(&placer.changed_positions).await;
        world.flush_block_updates().await;
    }
}

impl BlockEntity for JigsawBlockEntity {
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
            state: Mutex::new(JigsawState {
                name: nbt
                    .get_string(Self::NAME)
                    .unwrap_or(Self::EMPTY_ID)
                    .to_string(),
                target: nbt
                    .get_string(Self::TARGET)
                    .unwrap_or(Self::EMPTY_ID)
                    .to_string(),
                pool: nbt
                    .get_string(Self::POOL)
                    .unwrap_or(Self::EMPTY_ID)
                    .to_string(),
                final_state: nbt
                    .get_string(Self::FINAL_STATE)
                    .unwrap_or(Self::DEFAULT_FINAL_STATE)
                    .to_string(),
                joint: nbt
                    .get_string(Self::JOINT)
                    .map_or(JigsawJointType::Rollable, JigsawJointType::from_str),
                selection_priority: nbt
                    .get_int(Self::SELECTION_PRIORITY)
                    .unwrap_or(Self::DEFAULT_SELECTION_PRIORITY),
                placement_priority: nbt
                    .get_int(Self::PLACEMENT_PRIORITY)
                    .unwrap_or(Self::DEFAULT_PLACEMENT_PRIORITY),
            }),
            dirty: AtomicBool::new(false),
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
        let state = self.state.try_lock().ok()?.clone();
        let mut nbt = NbtCompound::new();
        state.write_nbt(&mut nbt);
        Some(nbt)
    }

    fn is_dirty(&self) -> bool {
        self.dirty.load(Ordering::Relaxed)
    }

    fn clear_dirty(&self) {
        self.dirty.store(false, Ordering::Relaxed);
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}
