use pumpkin_data::{
    Block, BlockDirection, BlockState, BlockStateId,
    block_properties::{BlockProperties, GlowLichenLikeProperties},
};
use pumpkin_util::{
    math::position::BlockPos,
    random::{RandomGenerator, RandomImpl},
};

use crate::generation::proto_chunk::GenerationCache;

/// Configuration emitted from Minecraft's `multiface_growth` configured features.
pub struct MultifaceGrowthFeature {
    pub block: &'static Block,
    pub can_be_placed_on: Vec<u16>,
    pub can_place_on_floor: bool,
    pub can_place_on_ceiling: bool,
    pub can_place_on_wall: bool,
    /// Retained from the vanilla configuration for the future worldgen spreader hook.
    pub chance_of_spreading: f32,
    pub search_range: i32,
}

impl MultifaceGrowthFeature {
    pub fn generate<T: GenerationCache>(
        &self,
        chunk: &mut T,
        _min_y: i8,
        _height: u16,
        _feature: pumpkin_data::placed_feature::PlacedFeature,
        random: &mut RandomGenerator,
        pos: BlockPos,
    ) -> bool {
        let origin_state = GenerationCache::get_block_state(chunk, &pos.0);
        if !is_air_or_water(origin_state)
            || !GlowLichenLikeProperties::handles_block_id(self.block.id)
        {
            return false;
        }

        let directions = self.shuffled_directions(random, None);
        if self.place_growth_if_possible(chunk, pos, origin_state, &directions) {
            return true;
        }

        for search_direction in directions {
            let placement_directions =
                self.shuffled_directions(random, Some(search_direction.opposite()));
            let mut search_pos = pos;

            // Vanilla walks a clear ray and stops at the first unrelated solid block.
            for _ in 0..self.search_range {
                search_pos = search_pos.offset(search_direction.to_offset());
                let old_state = GenerationCache::get_block_state(chunk, &search_pos.0);
                if !is_air_or_water(old_state) && old_state.to_block_id() != self.block.id {
                    break;
                }

                if self.place_growth_if_possible(
                    chunk,
                    search_pos,
                    old_state,
                    &placement_directions,
                ) {
                    return true;
                }
            }
        }

        false
    }

    fn place_growth_if_possible<T: GenerationCache>(
        &self,
        chunk: &mut T,
        pos: BlockPos,
        old_state: BlockStateId,
        placement_directions: &[BlockDirection],
    ) -> bool {
        for placement_direction in placement_directions {
            let neighbor_pos = pos.offset(placement_direction.to_offset());
            let neighbor_state = GenerationCache::get_block_state(chunk, &neighbor_pos.0);
            if !is_supported_block(
                self.can_be_placed_on.as_slice(),
                neighbor_state,
                *placement_direction,
            ) {
                continue;
            }

            let Some(new_state) =
                multiface_state_with_face(self.block, old_state, *placement_direction)
            else {
                return false;
            };

            chunk.set_block_state(&pos.0, new_state);

            // Minecraft next marks the position for post-processing and may invoke its
            // multiface spreader. Pumpkin has no matching generation-cache API yet, so
            // this bounded fix intentionally completes the placement state only.
            return true;
        }

        false
    }

    fn shuffled_directions(
        &self,
        random: &mut RandomGenerator,
        excluded: Option<BlockDirection>,
    ) -> Vec<BlockDirection> {
        let mut directions = Vec::with_capacity(6);
        if self.can_place_on_floor && excluded != Some(BlockDirection::Down) {
            directions.push(BlockDirection::Down);
        }
        if self.can_place_on_ceiling && excluded != Some(BlockDirection::Up) {
            directions.push(BlockDirection::Up);
        }
        if self.can_place_on_wall {
            for direction in [
                BlockDirection::North,
                BlockDirection::South,
                BlockDirection::West,
                BlockDirection::East,
            ] {
                if excluded != Some(direction) {
                    directions.push(direction);
                }
            }
        }

        // Match vanilla's shuffled direction order so no direction is always preferred.
        for index in (1..directions.len()).rev() {
            let swap_index = random.next_bounded_i32((index + 1) as i32) as usize;
            directions.swap(index, swap_index);
        }
        directions
    }
}

fn is_air_or_water(state: BlockStateId) -> bool {
    state.to_state().is_air() || state.to_block_id() == Block::WATER.id
}

fn is_supported_block(
    can_be_placed_on: &[u16],
    state: BlockStateId,
    placement_direction: BlockDirection,
) -> bool {
    // The configuration narrows eligible block types, then vanilla's multiface block
    // verifies that the neighbouring face can actually support the attached layer.
    can_be_placed_on.contains(&state.to_block_id().as_u16())
        && state
            .to_state()
            .is_side_solid(placement_direction.opposite())
}

/// Builds the precise multiface state for a successful placement. A fresh state starts
/// with every face disabled; an existing matching block retains its faces and gains one.
fn multiface_state_with_face(
    block: &'static Block,
    old_state: BlockStateId,
    face: BlockDirection,
) -> Option<&'static BlockState> {
    if !GlowLichenLikeProperties::handles_block_id(block.id) {
        return None;
    }

    let old_block_id = old_state.to_block_id();
    if old_block_id != block.id && !is_air_or_water(old_state) {
        return None;
    }

    let mut properties = if old_block_id == block.id {
        GlowLichenLikeProperties::from_state_id(old_state, block)
    } else {
        GlowLichenLikeProperties {
            r#down: false,
            r#east: false,
            r#north: false,
            r#south: false,
            r#up: false,
            r#waterlogged: old_block_id == Block::WATER.id,
            r#west: false,
        }
    };

    let selected_face = match face {
        BlockDirection::Down => &mut properties.r#down,
        BlockDirection::Up => &mut properties.r#up,
        BlockDirection::North => &mut properties.r#north,
        BlockDirection::South => &mut properties.r#south,
        BlockDirection::West => &mut properties.r#west,
        BlockDirection::East => &mut properties.r#east,
    };
    // Vanilla rejects a placement when the same face is already occupied instead of
    // treating it as a successful generation attempt.
    if *selected_face {
        return None;
    }
    *selected_face = true;

    Some(BlockState::from_id(properties.to_state_id(block)))
}

#[cfg(test)]
mod tests {
    use super::{is_supported_block, multiface_state_with_face};
    use pumpkin_data::{
        Block, BlockDirection,
        block_properties::{BlockProperties, GlowLichenLikeProperties},
    };

    fn glow_lichen_properties(
        state: &'static pumpkin_data::BlockState,
    ) -> GlowLichenLikeProperties {
        GlowLichenLikeProperties::from_state_id(state.id, &Block::GLOW_LICHEN)
    }

    #[test]
    fn new_growth_enables_only_the_selected_face() {
        let state = multiface_state_with_face(
            &Block::GLOW_LICHEN,
            Block::AIR.default_state.id,
            BlockDirection::North,
        )
        .expect("air can receive glow lichen");
        let properties = glow_lichen_properties(state);

        assert!(properties.r#north);
        assert!(!properties.r#down);
        assert!(!properties.r#east);
        assert!(!properties.r#south);
        assert!(!properties.r#up);
        assert!(!properties.r#west);
        assert!(!properties.r#waterlogged);
    }

    #[test]
    fn existing_growth_merges_faces_instead_of_replacing_them() {
        let north_state = multiface_state_with_face(
            &Block::GLOW_LICHEN,
            Block::AIR.default_state.id,
            BlockDirection::North,
        )
        .expect("air can receive glow lichen");
        let merged_state =
            multiface_state_with_face(&Block::GLOW_LICHEN, north_state.id, BlockDirection::East)
                .expect("an existing glow lichen can receive another face");
        let properties = glow_lichen_properties(merged_state);

        assert!(properties.r#north);
        assert!(properties.r#east);
        assert!(!properties.r#down);
        assert!(!properties.r#south);
        assert!(!properties.r#up);
        assert!(!properties.r#west);
        assert!(!properties.r#waterlogged);
    }

    #[test]
    fn an_existing_face_is_not_placed_twice() {
        let north_state = multiface_state_with_face(
            &Block::GLOW_LICHEN,
            Block::AIR.default_state.id,
            BlockDirection::North,
        )
        .expect("air can receive glow lichen");

        assert!(
            multiface_state_with_face(&Block::GLOW_LICHEN, north_state.id, BlockDirection::North,)
                .is_none()
        );
    }

    #[test]
    fn water_growth_is_waterlogged_with_only_the_selected_face() {
        let state = multiface_state_with_face(
            &Block::GLOW_LICHEN,
            Block::WATER.default_state.id,
            BlockDirection::Up,
        )
        .expect("water can receive glow lichen");
        let properties = glow_lichen_properties(state);

        assert!(properties.r#up);
        assert!(properties.r#waterlogged);
        assert!(!properties.r#down);
        assert!(!properties.r#east);
        assert!(!properties.r#north);
        assert!(!properties.r#south);
        assert!(!properties.r#west);
    }

    #[test]
    fn sculk_vein_growth_uses_a_face_state_instead_of_its_default_state() {
        let state = multiface_state_with_face(
            &Block::SCULK_VEIN,
            Block::AIR.default_state.id,
            BlockDirection::Down,
        )
        .expect("air can receive sculk vein");
        let properties = GlowLichenLikeProperties::from_state_id(state.id, &Block::SCULK_VEIN);

        assert_ne!(state.id, Block::SCULK_VEIN.default_state.id);
        assert!(properties.r#down);
        assert!(!properties.r#east);
        assert!(!properties.r#north);
        assert!(!properties.r#south);
        assert!(!properties.r#up);
        assert!(!properties.r#west);
        assert!(!properties.r#waterlogged);
    }

    #[test]
    fn unsupported_existing_blocks_are_rejected() {
        assert!(
            multiface_state_with_face(
                &Block::GLOW_LICHEN,
                Block::STONE.default_state.id,
                BlockDirection::North,
            )
            .is_none()
        );
    }

    #[test]
    fn configured_support_list_is_respected() {
        let supported_blocks = [Block::STONE.id.as_u16()];

        assert!(is_supported_block(
            &supported_blocks,
            Block::STONE.default_state.id,
            BlockDirection::North,
        ));
        assert!(!is_supported_block(
            &supported_blocks,
            Block::DIRT.default_state.id,
            BlockDirection::North,
        ));
    }

    #[test]
    fn partial_support_faces_are_rejected() {
        let supported_blocks = [Block::OAK_SLAB.id.as_u16()];

        assert!(!is_supported_block(
            &supported_blocks,
            Block::OAK_SLAB.default_state.id,
            BlockDirection::North,
        ));
    }
}
