use crate::block::{
    BlockBehaviour, BlockFuture, BlockIsReplacing, BlockMetadata, CanPlaceAtArgs, CanUpdateAtArgs,
    GetStateForNeighborUpdateArgs, OnPlaceArgs, UseWithItemArgs,
    blocks::multiface::{
        existing_multiface_state, find_attachment_direction, schedule_water_tick_if_needed,
        state_after_neighbor_update, state_with_added_face,
    },
    registry::BlockActionResult,
};
use pumpkin_data::{
    Block, BlockDirection, BlockId, BlockState, BlockStateId,
    block_properties::{BlockProperties, GlowLichenLikeProperties},
    item::Item,
};
use pumpkin_world::world::BlockFlags;

pub struct SculkVeinBlock;

impl BlockMetadata for SculkVeinBlock {
    fn ids() -> Box<[BlockId]> {
        [BlockId::SCULK_VEIN].into()
    }
}

impl BlockBehaviour for SculkVeinBlock {
    fn on_place<'a>(&'a self, args: OnPlaceArgs<'a>) -> BlockFuture<'a, BlockStateId> {
        Box::pin(async move {
            let existing_state_id = match &args.replacing {
                BlockIsReplacing::Itself(state_id) => Some(*state_id),
                _ => None,
            };
            let Some(direction) = find_attachment_direction(
                args.world,
                args.position,
                Some(args.player),
                args.direction,
                args.block,
                existing_state_id,
            ) else {
                return Block::AIR.default_state.id;
            };

            // Adding a face must preserve an existing waterlogged multiface state.
            state_with_added_face(
                args.block,
                existing_state_id,
                direction,
                args.replacing.water_source(),
            )
        })
    }

    fn can_place_at(&self, args: CanPlaceAtArgs<'_>) -> bool {
        find_attachment_direction(
            args.block_accessor,
            args.position,
            args.player,
            args.direction.unwrap_or(BlockDirection::Down),
            args.block,
            existing_multiface_state(args.block_accessor, args.position, args.block),
        )
        .is_some()
    }

    fn can_update_at(&self, args: CanUpdateAtArgs<'_>) -> bool {
        find_attachment_direction(
            args.world,
            args.position,
            Some(args.player),
            args.direction,
            args.block,
            Some(args.state_id),
        )
        .is_some()
    }

    fn get_state_for_neighbor_update<'a>(
        &'a self,
        args: GetStateForNeighborUpdateArgs<'a>,
    ) -> BlockFuture<'a, BlockStateId> {
        Box::pin(async move {
            let properties = GlowLichenLikeProperties::from_state_id(args.state_id, args.block);
            schedule_water_tick_if_needed(args.world, *args.position, properties);
            state_after_neighbor_update(
                properties,
                args.block,
                args.direction,
                BlockState::from_id(args.neighbor_state_id),
            )
        })
    }

    fn use_with_item<'a>(
        &'a self,
        args: UseWithItemArgs<'a>,
    ) -> BlockFuture<'a, BlockActionResult> {
        Box::pin(async move {
            if args.item_stack.item.id != Item::SCULK_VEIN.id {
                return BlockActionResult::Pass;
            }
            let state = args.world.get_block_state(args.position);
            let Some(direction) = find_attachment_direction(
                args.world.as_ref(),
                args.position,
                Some(args.player),
                *args.hit.face,
                args.block,
                Some(state.id),
            ) else {
                return BlockActionResult::Fail;
            };

            args.world
                .set_block_state(
                    args.position,
                    state_with_added_face(args.block, Some(state.id), direction, false),
                    BlockFlags::NOTIFY_ALL,
                )
                .await;
            BlockActionResult::Consume
        })
    }
}
