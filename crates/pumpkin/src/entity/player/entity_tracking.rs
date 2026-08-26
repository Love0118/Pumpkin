use pumpkin_util::math::vector2::Vector2;
use rustc_hash::FxHashSet;
use std::sync::Mutex;

#[derive(Default)]
pub(super) struct EntityTrackingState {
    tracked_ids: Mutex<FxHashSet<i32>>,
    sent_chunks: Mutex<FxHashSet<Vector2<i32>>>,
}

impl EntityTrackingState {
    pub fn start_tracking(&self, entity_id: i32) -> bool {
        self.tracked_ids
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(entity_id)
    }

    pub fn stop_tracking(&self, entity_id: i32) -> bool {
        self.tracked_ids
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&entity_id)
    }

    pub fn stop_tracking_candidates(&self, entity_ids: impl IntoIterator<Item = i32>) -> Vec<i32> {
        let mut tracked_ids = self
            .tracked_ids
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        entity_ids
            .into_iter()
            .filter(|entity_id| tracked_ids.remove(entity_id))
            .collect()
    }

    pub fn clear(&self) {
        self.tracked_ids
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
        self.sent_chunks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
    }

    pub fn tracked_ids(&self) -> Vec<i32> {
        self.tracked_ids
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .copied()
            .collect()
    }

    pub fn mark_chunks_sent(&self, chunks: impl IntoIterator<Item = Vector2<i32>>) {
        self.sent_chunks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .extend(chunks);
    }

    pub fn forget_chunks(&self, chunks: impl IntoIterator<Item = Vector2<i32>>) {
        let mut sent_chunks = self
            .sent_chunks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for chunk in chunks {
            sent_chunks.remove(&chunk);
        }
    }

    pub fn is_chunk_sent(&self, chunk: Vector2<i32>) -> bool {
        self.sent_chunks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains(&chunk)
    }

    pub fn sent_chunks(&self) -> Vec<Vector2<i32>> {
        self.sent_chunks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .copied()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transitions_allow_repairing_without_duplicate_spawns() {
        let tracking = EntityTrackingState::default();

        assert!(tracking.start_tracking(10));
        assert!(!tracking.start_tracking(10));
        assert_eq!(tracking.stop_tracking_candidates([10, 11]), [10]);
        assert!(tracking.start_tracking(10));
        assert!(tracking.stop_tracking(10));
        assert!(!tracking.stop_tracking(10));

        let first_chunk = Vector2::new(2, 3);
        let second_chunk = Vector2::new(3, 3);
        tracking.mark_chunks_sent([first_chunk, second_chunk]);
        assert!(tracking.is_chunk_sent(first_chunk));
        tracking.forget_chunks([first_chunk]);
        assert!(!tracking.is_chunk_sent(first_chunk));
        assert_eq!(tracking.sent_chunks(), [second_chunk]);
    }
}
