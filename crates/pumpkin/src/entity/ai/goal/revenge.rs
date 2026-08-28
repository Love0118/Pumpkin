use std::sync::Arc;
use std::sync::atomic::Ordering::Relaxed;

use super::{Controls, Goal};
use crate::entity::EntityBase;
use crate::entity::ai::goal::GoalFuture;
use crate::entity::ai::goal::track_target::TrackTargetGoal;
use crate::entity::ai::target_predicate::TargetPredicate;
use crate::entity::mob::Mob;
use pumpkin_data::attributes::Attributes;
use pumpkin_data::entity::EntityType;
use pumpkin_data::tag::{self, Taggable};

#[derive(Clone, Copy)]
pub enum EntityTypeFilter {
    Exact(&'static EntityType),
    Tag(&'static tag::Tag),
}

impl EntityTypeFilter {
    fn matches(self, entity_type: &'static EntityType) -> bool {
        match self {
            Self::Exact(expected) => entity_type == expected,
            Self::Tag(tag) => entity_type.has_tag(tag),
        }
    }
}

pub struct RevengeGoal {
    track_target_goal: TrackTargetGoal,
    target: Option<Arc<dyn EntityBase>>,
    last_attacked_time: i32,
    target_predicate: TargetPredicate,
    alert_same_type: bool,
    ignored_damage_types: &'static [EntityTypeFilter],
    ignored_alert_types: &'static [EntityTypeFilter],
}

impl RevengeGoal {
    #[must_use]
    pub fn new(check_visibility: bool) -> Self {
        let target_predicate = TargetPredicate::create_attackable()
            .ignore_visibility()
            .ignore_distance_scaling_factor();
        Self {
            track_target_goal: TrackTargetGoal::with_default(check_visibility),
            target: None,
            last_attacked_time: 0,
            target_predicate,
            alert_same_type: false,
            ignored_damage_types: &[],
            ignored_alert_types: &[],
        }
    }

    #[must_use]
    pub const fn ignore_damage_from(
        mut self,
        ignored_damage_types: &'static [EntityTypeFilter],
    ) -> Self {
        self.ignored_damage_types = ignored_damage_types;
        self
    }

    #[must_use]
    pub const fn set_alert_others(mut self) -> Self {
        self.alert_same_type = true;
        self
    }

    #[must_use]
    pub const fn set_alert_others_except(
        mut self,
        ignored_alert_types: &'static [EntityTypeFilter],
    ) -> Self {
        self.alert_same_type = true;
        self.ignored_alert_types = ignored_alert_types;
        self
    }

    async fn alert_others(&self, mob: &dyn Mob, attacker: &Arc<dyn EntityBase>) {
        let mob_entity = mob.get_mob_entity();
        let entity = &mob_entity.living_entity.entity;
        let world = entity.world.load_full();
        let position = entity.pos.load();
        let follow_distance = mob_entity
            .living_entity
            .get_attribute_value(&Attributes::FOLLOW_RANGE);
        let owner = mob
            .as_tamable()
            .and_then(crate::entity::passive::tamable::TamableAnimal::get_owner);

        for candidate in world.entities.load().iter() {
            let candidate_entity = candidate.get_entity();
            let Some(candidate_mob) = candidate.get_mob() else {
                continue;
            };
            let candidate_pos = candidate_entity.pos.load();
            let owner_matches = owner.is_none_or(|owner| {
                candidate_mob
                    .as_tamable()
                    .and_then(crate::entity::passive::tamable::TamableAnimal::get_owner)
                    == Some(owner)
            });
            let ignored = self
                .ignored_alert_types
                .iter()
                .any(|filter| filter.matches(candidate_entity.entity_type));
            let within = (candidate_pos.y - position.y).abs() <= 10.0
                && candidate_pos.x - position.x <= follow_distance
                && candidate_pos.x - position.x >= -follow_distance
                && candidate_pos.z - position.z <= follow_distance
                && candidate_pos.z - position.z >= -follow_distance;
            let has_target = candidate_mob.get_mob_entity().get_target().await.is_some();
            let allied_to_attacker = world
                .entities_are_allied(candidate_entity, attacker.get_entity())
                .await;

            if should_alert_candidate(
                candidate_entity.entity_type == entity.entity_type
                    && candidate_entity.entity_id != entity.entity_id,
                !has_target && owner_matches && !ignored && !allied_to_attacker,
                within,
            ) {
                candidate_mob.set_mob_target(Some(attacker.clone())).await;
            }
        }
    }
}

const fn should_alert_candidate(valid_entity: bool, valid_target: bool, within: bool) -> bool {
    valid_entity && valid_target && within
}

impl Goal for RevengeGoal {
    fn can_start<'a>(&'a mut self, mob: &'a dyn Mob) -> GoalFuture<'a, bool> {
        Box::pin(async move {
            let mob_entity = mob.get_mob_entity();
            let living = &mob_entity.living_entity;

            let attacked_time = living.last_attacked_time.load(Relaxed);
            if attacked_time == self.last_attacked_time {
                return false;
            }

            let attacker_id = living.last_attacker_id.load(Relaxed);
            if attacker_id == 0 {
                return false;
            }

            let world = living.entity.world.load();
            let Some(attacker) = world.get_entity_by_id(attacker_id) else {
                return false;
            };

            if self
                .ignored_damage_types
                .iter()
                .any(|filter| filter.matches(attacker.get_entity().entity_type))
            {
                return false;
            }

            let Some(attacker_living) = attacker.get_living_entity() else {
                return false;
            };

            if !self
                .target_predicate
                .test(&world, Some(mob), attacker_living, &mob_entity.sensing)
                .await
            {
                return false;
            }

            self.target = Some(attacker);
            true
        })
    }

    fn should_continue<'a>(&'a self, mob: &'a dyn Mob) -> GoalFuture<'a, bool> {
        Box::pin(async { self.track_target_goal.should_continue(mob).await })
    }

    fn start<'a>(&'a mut self, mob: &'a dyn Mob) -> GoalFuture<'a, ()> {
        Box::pin(async {
            mob.set_mob_target(self.target.clone()).await;

            let mob_entity = mob.get_mob_entity();
            self.last_attacked_time = mob_entity.living_entity.last_attacked_time.load(Relaxed);
            self.track_target_goal.max_time_without_visibility = 300;

            self.track_target_goal.start(mob).await;
            if self.alert_same_type
                && let Some(attacker) = self.target.as_ref()
            {
                self.alert_others(mob, attacker).await;
            }
        })
    }

    fn stop<'a>(&'a mut self, mob: &'a dyn Mob) -> GoalFuture<'a, ()> {
        Box::pin(async {
            self.target = None;
            self.track_target_goal.stop(mob).await;
        })
    }

    fn controls(&self) -> Controls {
        self.track_target_goal.controls()
    }
}

#[cfg(test)]
mod tests {
    use super::should_alert_candidate;

    #[test]
    fn group_alert_requires_same_idle_owned_nonignored_mob_in_range() {
        assert!(should_alert_candidate(true, true, true));
        assert!(!should_alert_candidate(false, true, true));
        assert!(!should_alert_candidate(false, true, true));
        assert!(!should_alert_candidate(true, false, true));
        assert!(!should_alert_candidate(true, false, true));
        assert!(!should_alert_candidate(true, false, true));
        assert!(!should_alert_candidate(true, false, true));
        assert!(!should_alert_candidate(true, true, false));
    }

    #[test]
    fn entity_type_filters_support_exact_and_tag_families() {
        use pumpkin_data::entity::EntityType;
        use pumpkin_data::tag;

        assert!(
            super::EntityTypeFilter::Exact(&EntityType::GUARDIAN).matches(&EntityType::GUARDIAN)
        );
        assert!(
            !super::EntityTypeFilter::Exact(&EntityType::GUARDIAN)
                .matches(&EntityType::ELDER_GUARDIAN)
        );
        assert!(
            super::EntityTypeFilter::Tag(&tag::EntityType::MINECRAFT_RAIDERS)
                .matches(&EntityType::PILLAGER)
        );
        assert!(
            !super::EntityTypeFilter::Tag(&tag::EntityType::MINECRAFT_RAIDERS)
                .matches(&EntityType::ZOMBIE)
        );
    }
}
