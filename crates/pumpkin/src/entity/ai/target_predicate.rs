use pumpkin_util::Difficulty;

use crate::entity::ai::sensing::Sensing;
use crate::entity::living::LivingEntity;
use crate::entity::mob::Mob;
use crate::world::World;
use std::sync::Arc;

const MIN_DISTANCE: f64 = 2.0;

pub type PredicateFn = dyn Fn(&LivingEntity, &World) -> bool + Send + Sync;

pub struct TargetPredicate {
    pub attackable: bool,
    pub base_max_distance: f64,
    pub respects_visibility: bool,
    pub use_distance_scaling_factor: bool,
    pub predicate: Option<Arc<PredicateFn>>,
}

impl Default for TargetPredicate {
    fn default() -> Self {
        Self {
            attackable: true,
            base_max_distance: -1.0,
            respects_visibility: true,
            use_distance_scaling_factor: true,
            predicate: None,
        }
    }
}

impl TargetPredicate {
    fn new(attackable: bool) -> Self {
        Self {
            attackable,
            ..Default::default()
        }
    }

    #[must_use]
    pub fn create_attackable() -> Self {
        Self::new(true)
    }

    #[must_use]
    pub fn create_non_attackable() -> Self {
        Self::new(false)
    }

    #[must_use]
    pub fn copy(&self) -> Self {
        Self {
            attackable: self.attackable,
            base_max_distance: self.base_max_distance,
            respects_visibility: self.respects_visibility,
            use_distance_scaling_factor: self.use_distance_scaling_factor,
            predicate: self.predicate.clone(),
        }
    }

    #[must_use]
    pub const fn set_base_max_distance(mut self, base_max_distance: f64) -> Self {
        self.base_max_distance = base_max_distance;
        self
    }

    #[must_use]
    pub const fn ignore_visibility(mut self) -> Self {
        self.respects_visibility = false;
        self
    }

    #[must_use]
    pub const fn ignore_distance_scaling_factor(mut self) -> Self {
        self.use_distance_scaling_factor = false;
        self
    }

    pub fn set_predicate<F>(&mut self, predicate: F)
    where
        F: Fn(&LivingEntity, &World) -> bool + Send + Sync + 'static,
    {
        self.predicate = Some(Arc::new(predicate));
    }

    pub async fn test(
        &self,
        world: &World,
        tester: Option<&dyn Mob>,
        target: &LivingEntity,
        sensing: &Sensing,
    ) -> bool {
        let tester_living = tester.map(|mob| &mob.get_mob_entity().living_entity);
        if tester_living.is_some_and(|tester| tester.entity.entity_id == target.entity.entity_id) {
            return false;
        }

        if !target.is_part_of_game() {
            return false;
        }

        if self.attackable
            && (!target.can_take_damage()
                || world.level_info.load().difficulty == Difficulty::Peaceful)
        {
            return false;
        }

        if let Some(tester_ent) = tester_living
            && self.base_max_distance > 0.0
        {
            // TODO: use distance_scaling_factor from target
            let max_dist = self.base_max_distance.max(MIN_DISTANCE);
            let dist_sq = tester_ent
                .entity
                .pos
                .load()
                .squared_distance_to_vec(&target.entity.pos.load());

            if dist_sq > max_dist * max_dist {
                return false;
            }
        }

        if let Some(tester) = tester {
            let tester_entity = tester.get_entity();
            if world
                .entities_are_allied(tester_entity, &target.entity)
                .await
            {
                return false;
            }
            if let Some(owner_uuid) = tester
                .as_tamable()
                .and_then(super::super::passive::tamable::TamableAnimal::get_owner)
            {
                if target.entity.entity_uuid == owner_uuid {
                    return false;
                }
                if world
                    .get_entity_by_uuid(target.entity.entity_uuid)
                    .is_some_and(|target_entity| {
                        target_entity
                            .get_mob()
                            .and_then(|mob| mob.as_tamable())
                            .and_then(super::super::passive::tamable::TamableAnimal::get_owner)
                            == Some(owner_uuid)
                    })
                {
                    return false;
                }
            }
        }

        if self.respects_visibility
            && let Some(tester_ent) = tester_living
            && !sensing
                .has_line_of_sight(&tester_ent.entity, &target.entity)
                .await
        {
            return false;
        }

        if self
            .predicate
            .as_ref()
            .is_some_and(|predicate| !predicate(target, world))
        {
            return false;
        }

        true
    }
}
