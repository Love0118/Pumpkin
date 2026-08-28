//! Shared state transitions for blocks that occupy one or more attached faces.
//!
//! Minecraft's `MultifaceBlock` owns the common support rule. Blocks such as
//! glow lichen and sculk vein share that rule, while vines retain their own
//! hanging and spreading behaviour.

use crate::entity::{EntityBase, player::Player};
use crate::world::World;
use pumpkin_data::{
    Block, BlockDirection, BlockState, BlockStateId, FacingExt,
    block_properties::{BlockProperties, GlowLichenLikeProperties},
    fluid::Fluid,
};
use pumpkin_util::math::position::BlockPos;
use pumpkin_world::{tick::TickPriority, world::BlockAccessor};

/// Returns whether the neighbour exposes a solid face toward an attached block.
///
/// `direction_to_support` points from the multiface block to its neighbour, so
/// the neighbour must be solid on the opposite side. This matches vanilla
/// `MultifaceBlock.canAttachTo`.
pub(crate) const fn can_attach_to_state(
    support_state: &BlockState,
    direction_to_support: BlockDirection,
) -> bool {
    support_state.is_side_solid(direction_to_support.opposite())
}

/// Looks up whether a block at `position` can support the specified face.
pub(crate) fn can_attach_to(
    block_accessor: &dyn BlockAccessor,
    position: &BlockPos,
    direction_to_support: BlockDirection,
) -> bool {
    let support_position = position.offset(direction_to_support.to_offset());
    can_attach_to_state(
        block_accessor.get_block_state(&support_position),
        direction_to_support,
    )
}

pub(crate) const fn has_face(
    properties: GlowLichenLikeProperties,
    direction: BlockDirection,
) -> bool {
    match direction {
        BlockDirection::Down => properties.down,
        BlockDirection::Up => properties.up,
        BlockDirection::North => properties.north,
        BlockDirection::South => properties.south,
        BlockDirection::West => properties.west,
        BlockDirection::East => properties.east,
    }
}

pub(crate) const fn set_face(
    properties: &mut GlowLichenLikeProperties,
    direction: BlockDirection,
    value: bool,
) {
    match direction {
        BlockDirection::Down => properties.down = value,
        BlockDirection::Up => properties.up = value,
        BlockDirection::North => properties.north = value,
        BlockDirection::South => properties.south = value,
        BlockDirection::West => properties.west = value,
        BlockDirection::East => properties.east = value,
    }
}

pub(crate) const fn has_any_face(properties: GlowLichenLikeProperties) -> bool {
    properties.down
        || properties.up
        || properties.north
        || properties.south
        || properties.west
        || properties.east
}

/// Returns the state after one neighbour changed. Only the face touching that
/// neighbour is removed; valid faces and the waterlogged property are retained.
pub(crate) fn state_after_neighbor_update(
    mut properties: GlowLichenLikeProperties,
    block: &Block,
    changed_direction: BlockDirection,
    neighbour_state: &BlockState,
) -> BlockStateId {
    if !has_any_face(properties) {
        return Block::AIR.default_state.id;
    }

    if has_face(properties, changed_direction)
        && !can_attach_to_state(neighbour_state, changed_direction)
    {
        set_face(&mut properties, changed_direction, false);
    }

    if has_any_face(properties) {
        properties.to_state_id(block)
    } else {
        Block::AIR.default_state.id
    }
}

/// Adds one supported face, preserving an existing multiface state when one is
/// already present. A newly placed multiface block becomes waterlogged only when
/// it replaces a water source.
pub(crate) fn state_with_added_face(
    block: &Block,
    existing_state_id: Option<BlockStateId>,
    direction: BlockDirection,
    replaces_water_source: bool,
) -> BlockStateId {
    let mut properties = existing_state_id.map_or_else(
        || GlowLichenLikeProperties::default(block),
        |state_id| GlowLichenLikeProperties::from_state_id(state_id, block),
    );

    if existing_state_id.is_none() {
        properties.waterlogged = replaces_water_source;
    }
    set_face(&mut properties, direction, true);
    properties.to_state_id(block)
}

/// Finds the first vacant supported face in vanilla's placement order.
pub(crate) fn find_attachment_direction(
    block_accessor: &dyn BlockAccessor,
    position: &BlockPos,
    player: Option<&Player>,
    preferred_direction: BlockDirection,
    block: &Block,
    existing_state_id: Option<BlockStateId>,
) -> Option<BlockDirection> {
    let existing_properties =
        existing_state_id.map(|state_id| GlowLichenLikeProperties::from_state_id(state_id, block));
    let can_add_face = |direction| {
        !existing_properties.is_some_and(|properties| has_face(properties, direction))
            && can_attach_to(block_accessor, position, direction)
    };

    if can_add_face(preferred_direction) {
        return Some(preferred_direction);
    }

    let player = player?;
    player
        .get_entity()
        .get_entity_facing_order()
        .into_iter()
        .map(|facing| facing.to_block_direction())
        .find(|&direction| can_add_face(direction))
}

/// Returns the current multiface state only when this exact block already
/// occupies the target position.
pub(crate) fn existing_multiface_state(
    block_accessor: &dyn BlockAccessor,
    position: &BlockPos,
    block: &Block,
) -> Option<BlockStateId> {
    let (existing_block, existing_state) = block_accessor.get_block_and_state(position);
    (existing_block == block).then_some(existing_state.id)
}

/// Vanilla schedules a water-fluid update whenever a waterlogged multiface
/// block receives a neighbour shape update.
pub(crate) fn schedule_water_tick_if_needed(
    world: &World,
    position: BlockPos,
    properties: GlowLichenLikeProperties,
) {
    if properties.waterlogged {
        world.schedule_fluid_tick(
            &Fluid::WATER,
            position,
            Fluid::WATER.flow_speed as u8,
            TickPriority::Normal,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn neighbour_update_removes_only_the_lost_face_and_keeps_waterlogging() {
        let mut properties = GlowLichenLikeProperties::default(&Block::GLOW_LICHEN);
        set_face(&mut properties, BlockDirection::North, true);
        set_face(&mut properties, BlockDirection::East, true);
        properties.waterlogged = true;

        let updated = state_after_neighbor_update(
            properties,
            &Block::GLOW_LICHEN,
            BlockDirection::North,
            Block::AIR.default_state,
        );
        let updated = GlowLichenLikeProperties::from_state_id(updated, &Block::GLOW_LICHEN);

        assert!(!updated.north);
        assert!(updated.east);
        assert!(updated.waterlogged);
    }

    #[test]
    fn neighbour_update_replaces_the_last_unsupported_face_with_air() {
        let mut properties = GlowLichenLikeProperties::default(&Block::GLOW_LICHEN);
        set_face(&mut properties, BlockDirection::Up, true);

        assert_eq!(
            state_after_neighbor_update(
                properties,
                &Block::GLOW_LICHEN,
                BlockDirection::Up,
                Block::AIR.default_state,
            ),
            Block::AIR.default_state.id
        );
    }

    #[test]
    fn attachment_rejects_a_neighbour_without_a_solid_opposite_face() {
        assert!(can_attach_to_state(
            Block::STONE.default_state,
            BlockDirection::North
        ));
        assert!(!can_attach_to_state(
            Block::AIR.default_state,
            BlockDirection::North
        ));
    }

    #[test]
    fn active_glow_lichen_emits_vanillas_light_level() {
        let state = state_with_added_face(&Block::GLOW_LICHEN, None, BlockDirection::North, false);

        assert_eq!(BlockState::from_id(state).luminance, 7);
        assert_eq!(Block::GLOW_LICHEN.default_state.luminance, 0);
    }
}
