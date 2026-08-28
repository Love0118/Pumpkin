use std::sync::RwLock;

use pumpkin_util::math::{get_section_cord, position::BlockPos};
use rustc_hash::{FxHashMap, FxHashSet};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct EntitySectionPos {
    x: i32,
    y: i32,
    z: i32,
}

impl EntitySectionPos {
    const fn from_block_pos(position: BlockPos) -> Self {
        Self {
            x: get_section_cord(position.0.x),
            y: get_section_cord(position.0.y),
            z: get_section_cord(position.0.z),
        }
    }
}

#[derive(Default)]
struct EntitySectionMaps {
    by_entity: FxHashMap<Uuid, EntitySectionPos>,
    by_section: FxHashMap<EntitySectionPos, FxHashSet<Uuid>>,
}

/// Owns coherent 3D section membership for every live world entity.
///
/// Both views change under one lock so movement, removal, and section pruning cannot expose a
/// half-applied membership transition to future tracking or ticking consumers.
#[derive(Default)]
pub(super) struct EntitySectionIndex {
    maps: RwLock<EntitySectionMaps>,
}

impl EntitySectionIndex {
    pub(super) fn insert(&self, uuid: Uuid, position: BlockPos) -> bool {
        let mut maps = self
            .maps
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if maps.by_entity.contains_key(&uuid) {
            return false;
        }

        let section = EntitySectionPos::from_block_pos(position);
        maps.by_entity.insert(uuid, section);
        maps.by_section.entry(section).or_default().insert(uuid);
        true
    }

    pub(super) fn move_entity(&self, uuid: Uuid, position: BlockPos) -> bool {
        let mut maps = self
            .maps
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(previous) = maps.by_entity.get(&uuid).copied() else {
            return false;
        };
        let next = EntitySectionPos::from_block_pos(position);
        if previous == next {
            return false;
        }

        let remove_previous_section = maps.by_section.get_mut(&previous).is_some_and(|entities| {
            entities.remove(&uuid);
            entities.is_empty()
        });
        if remove_previous_section {
            maps.by_section.remove(&previous);
        }
        maps.by_section.entry(next).or_default().insert(uuid);
        maps.by_entity.insert(uuid, next);
        true
    }

    pub(super) fn remove(&self, uuid: Uuid) -> bool {
        let mut maps = self
            .maps
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(section) = maps.by_entity.remove(&uuid) else {
            return false;
        };

        let remove_section = maps.by_section.get_mut(&section).is_some_and(|entities| {
            entities.remove(&uuid);
            entities.is_empty()
        });
        if remove_section {
            maps.by_section.remove(&section);
        }
        true
    }

    #[cfg(test)]
    fn section_of(&self, uuid: Uuid) -> Option<EntitySectionPos> {
        self.maps
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .by_entity
            .get(&uuid)
            .copied()
    }

    #[cfg(test)]
    fn section_count(&self) -> usize {
        self.maps
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .by_section
            .len()
    }
}

#[cfg(test)]
mod tests {
    use pumpkin_util::math::{position::BlockPos, vector3::Vector3};
    use uuid::Uuid;

    use super::{EntitySectionIndex, EntitySectionPos};

    #[test]
    fn insert_publishes_one_coherent_membership() {
        let index = EntitySectionIndex::default();
        let uuid = Uuid::new_v4();

        assert!(index.insert(uuid, BlockPos(Vector3::new(0, 15, -1))));
        assert!(!index.insert(uuid, BlockPos(Vector3::new(32, 32, 32))));
        assert_eq!(
            index.section_of(uuid),
            Some(EntitySectionPos { x: 0, y: 0, z: -1 })
        );
        assert_eq!(index.section_count(), 1);
    }

    #[test]
    fn crossing_a_section_moves_and_prunes_atomically() {
        let index = EntitySectionIndex::default();
        let uuid = Uuid::new_v4();
        assert!(index.insert(uuid, BlockPos(Vector3::new(15, 15, 15))));

        assert!(!index.move_entity(uuid, BlockPos(Vector3::new(15, 0, 15))));
        assert!(index.move_entity(uuid, BlockPos(Vector3::new(16, -1, 16))));
        assert_eq!(
            index.section_of(uuid),
            Some(EntitySectionPos { x: 1, y: -1, z: 1 })
        );
        assert_eq!(index.section_count(), 1);
    }

    #[test]
    fn removal_clears_both_views_and_empty_section() {
        let index = EntitySectionIndex::default();
        let uuid = Uuid::new_v4();
        assert!(index.insert(uuid, BlockPos(Vector3::new(1, 2, 3))));

        assert!(index.remove(uuid));
        assert!(!index.remove(uuid));
        assert_eq!(index.section_of(uuid), None);
        assert_eq!(index.section_count(), 0);
    }
}
