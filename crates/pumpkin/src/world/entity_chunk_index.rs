use crate::entity::EntityBase;
use pumpkin_util::math::vector2::Vector2;
use rustc_hash::{FxHashMap, FxHashSet};
use std::sync::{Arc, Weak};

#[derive(Default)]
pub(super) struct EntityChunkIndex {
    chunks: FxHashMap<Vector2<i32>, FxHashSet<i32>>,
    positions: FxHashMap<i32, Vector2<i32>>,
    entities_by_id: FxHashMap<i32, Weak<dyn EntityBase>>,
}

impl EntityChunkIndex {
    pub fn insert(&mut self, entity: &Arc<dyn EntityBase>, chunk: Vector2<i32>) {
        let base_entity = entity.get_entity();
        let entity_id = base_entity.entity_id;
        let weak_entity = Arc::downgrade(entity);
        self.entities_by_id.insert(entity_id, weak_entity);
        self.insert_membership(entity_id, chunk);
    }

    fn insert_membership(&mut self, entity_id: i32, chunk: Vector2<i32>) {
        if let Some(previous_chunk) = self.positions.insert(entity_id, chunk) {
            if previous_chunk == chunk {
                return;
            }
            self.remove_from_chunk(previous_chunk, entity_id);
        }

        self.chunks.entry(chunk).or_default().insert(entity_id);
    }

    pub fn update_if_present(&mut self, entity_id: i32, chunk: Vector2<i32>) {
        let Some(previous_chunk) = self.positions.get(&entity_id).copied() else {
            return;
        };
        if previous_chunk == chunk {
            return;
        }

        self.positions.insert(entity_id, chunk);
        self.remove_from_chunk(previous_chunk, entity_id);
        self.chunks.entry(chunk).or_default().insert(entity_id);
    }

    pub fn remove(&mut self, entity_id: i32) {
        self.entities_by_id.remove(&entity_id);
        if let Some(chunk) = self.positions.remove(&entity_id) {
            self.remove_from_chunk(chunk, entity_id);
        }
    }

    pub fn ids_in_chunks(&self, chunks: &[Vector2<i32>]) -> Vec<i32> {
        chunks
            .iter()
            .filter_map(|chunk| self.chunks.get(chunk))
            .flat_map(|entity_ids| entity_ids.iter().copied())
            .collect()
    }

    pub fn entities_in_chunk(&self, chunk: Vector2<i32>) -> Vec<Arc<dyn EntityBase>> {
        self.chunks.get(&chunk).map_or_else(Vec::new, |entity_ids| {
            entity_ids
                .iter()
                .filter_map(|entity_id| self.entities_by_id.get(entity_id)?.upgrade())
                .collect()
        })
    }

    pub fn entity_by_id(&self, entity_id: i32) -> Option<Arc<dyn EntityBase>> {
        self.entities_by_id.get(&entity_id)?.upgrade()
    }

    fn remove_from_chunk(&mut self, chunk: Vector2<i32>, entity_id: i32) {
        let remove_chunk = self.chunks.get_mut(&chunk).is_some_and(|entity_ids| {
            entity_ids.remove(&entity_id);
            entity_ids.is_empty()
        });
        if remove_chunk {
            self.chunks.remove(&chunk);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tracks_insert_move_and_remove_by_chunk() {
        let first_chunk = Vector2::new(2, 3);
        let second_chunk = Vector2::new(3, 3);
        let mut index = EntityChunkIndex::default();

        index.insert_membership(10, first_chunk);
        index.insert_membership(11, first_chunk);
        index.update_if_present(10, second_chunk);
        index.update_if_present(99, second_chunk);

        assert_eq!(index.ids_in_chunks(&[first_chunk]), [11]);
        assert_eq!(index.ids_in_chunks(&[second_chunk]), [10]);

        index.remove(10);

        assert!(index.ids_in_chunks(&[second_chunk]).is_empty());
    }
}
