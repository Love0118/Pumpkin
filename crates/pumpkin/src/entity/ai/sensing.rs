use std::future::Future;
use std::sync::{Arc, Mutex};

use crate::entity::Entity;

const MAX_LINE_OF_SIGHT_DISTANCE_SQUARED: f64 = 128.0 * 128.0;

/// Per-mob, per-AI-tick line-of-sight cache.
///
/// Vanilla keeps separate integer sets for visible and hidden targets. A compact
/// vector stores the same result here because a mob normally queries only a few
/// targets per tick and avoids allocating two hash tables for every mob.
#[derive(Default)]
pub struct Sensing {
    visibility: Mutex<Vec<(i32, bool)>>,
}

impl Sensing {
    /// Invalidates all visibility results at the start of an AI tick.
    pub fn tick(&self) {
        self.visibility
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
    }

    async fn cached_or_compute<F, Fut>(&self, target_id: i32, compute: F) -> bool
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = bool>,
    {
        let cached = {
            let visibility = self
                .visibility
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            visibility
                .iter()
                .find_map(|(id, visible)| (*id == target_id).then_some(*visible))
        };
        if let Some(visible) = cached {
            return visible;
        }

        let visible = compute().await;
        let mut visibility = self
            .visibility
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some((_, cached)) = visibility.iter().find(|(id, _)| *id == target_id) {
            *cached
        } else {
            visibility.push((target_id, visible));
            visible
        }
    }

    /// Returns whether `observer` can see `target`, caching the result by target
    /// entity id until the next [`Self::tick`].
    pub async fn has_line_of_sight(&self, observer: &Entity, target: &Entity) -> bool {
        self.cached_or_compute(target.entity_id, || async move {
            let observer_world = observer.world.load_full();
            let target_world = target.world.load_full();
            if !Arc::ptr_eq(&observer_world, &target_world) {
                return false;
            }

            let from = observer.get_eye_pos();
            let to = target.get_eye_pos();
            if from.squared_distance_to_vec(&to) > MAX_LINE_OF_SIGHT_DISTANCE_SQUARED {
                return false;
            }

            observer_world
                .raycast(from, to, async |block_pos, world| {
                    world.get_block_state(block_pos).is_solid()
                })
                .await
                .is_none()
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::Sensing;

    #[tokio::test]
    async fn caches_both_visibility_results_until_the_next_tick() {
        let sensing = Sensing::default();
        let computations = AtomicUsize::new(0);

        for expected in [true, false] {
            let target_id = if expected { 1 } else { 2 };
            assert_eq!(
                sensing
                    .cached_or_compute(target_id, || async {
                        computations.fetch_add(1, Ordering::Relaxed);
                        expected
                    })
                    .await,
                expected
            );
            assert_eq!(
                sensing
                    .cached_or_compute(target_id, || async {
                        computations.fetch_add(1, Ordering::Relaxed);
                        !expected
                    })
                    .await,
                expected
            );
        }
        assert_eq!(computations.load(Ordering::Relaxed), 2);

        sensing.tick();
        assert!(
            sensing
                .cached_or_compute(2, || async {
                    computations.fetch_add(1, Ordering::Relaxed);
                    true
                })
                .await
        );
        assert_eq!(computations.load(Ordering::Relaxed), 3);
    }
}
