use std::{num::NonZeroI32, sync::RwLock};

use pumpkin_util::math::vector3::Vector3;
use rustc_hash::{FxHashMap, FxHashSet};
use uuid::Uuid;

#[derive(Default)]
pub(super) struct EntityTracker {
    seen_by_entity: RwLock<FxHashMap<Uuid, FxHashSet<Uuid>>>,
}

impl EntityTracker {
    pub(super) fn transition(
        &self,
        entity: Uuid,
        player: Uuid,
        should_track: bool,
    ) -> Option<bool> {
        let mut seen = self
            .seen_by_entity
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if should_track {
            return seen
                .entry(entity)
                .or_default()
                .insert(player)
                .then_some(true);
        }

        let removed = seen
            .get_mut(&entity)
            .is_some_and(|players| players.remove(&player));
        if seen.get(&entity).is_some_and(FxHashSet::is_empty) {
            seen.remove(&entity);
        }
        removed.then_some(false)
    }

    pub(super) fn remove_entity(&self, entity: Uuid) -> Vec<Uuid> {
        self.seen_by_entity
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&entity)
            .map_or_else(Vec::new, |players| players.into_iter().collect())
    }

    pub(super) fn remove_player(&self, player: Uuid) {
        let mut seen = self
            .seen_by_entity
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        seen.retain(|_, players| {
            players.remove(&player);
            !players.is_empty()
        });
    }
}

pub(super) fn should_track(
    entity_position: Vector3<f64>,
    player_position: Vector3<f64>,
    entity_range_blocks: i32,
    player_view_distance_chunks: NonZeroI32,
) -> bool {
    let visible_range =
        f64::from(entity_range_blocks.min(player_view_distance_chunks.get().saturating_mul(16)));
    let dx = player_position.x - entity_position.x;
    let dz = player_position.z - entity_position.z;
    dx.mul_add(dx, dz * dz) <= visible_range * visible_range
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroI32;

    use pumpkin_util::math::vector3::Vector3;
    use uuid::Uuid;

    use super::{EntityTracker, should_track};

    #[test]
    fn transition_emits_only_on_membership_changes() {
        let tracker = EntityTracker::default();
        let entity = Uuid::new_v4();
        let player = Uuid::new_v4();

        assert_eq!(tracker.transition(entity, player, true), Some(true));
        assert_eq!(tracker.transition(entity, player, true), None);
        assert_eq!(tracker.transition(entity, player, false), Some(false));
        assert_eq!(tracker.transition(entity, player, false), None);
    }

    #[test]
    fn range_uses_horizontal_distance_and_player_view_cap() {
        let origin = Vector3::new(0.0, 200.0, 0.0);
        assert!(should_track(
            origin,
            Vector3::new(63.0, -200.0, 0.0),
            64,
            NonZeroI32::new(10).unwrap()
        ));
        assert!(!should_track(
            origin,
            Vector3::new(65.0, 200.0, 0.0),
            64,
            NonZeroI32::new(10).unwrap()
        ));
        assert!(!should_track(
            origin,
            Vector3::new(49.0, 200.0, 0.0),
            160,
            NonZeroI32::new(3).unwrap()
        ));
    }
}
