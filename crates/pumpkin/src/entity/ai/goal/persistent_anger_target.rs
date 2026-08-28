use std::sync::Arc;

use crate::entity::EntityBase;
use crate::entity::ai::goal::track_target::TrackTargetGoal;
use crate::entity::ai::goal::{Controls, Goal, GoalFuture};
use crate::entity::mob::Mob;

/// Re-resolves a neutral mob's persisted UUID target after unload or after an
/// ordinary target goal exhausts its unseen-memory window.
pub struct PersistentAngerTargetGoal {
    track_target_goal: TrackTargetGoal,
    target: Option<Arc<dyn EntityBase>>,
}

impl PersistentAngerTargetGoal {
    #[must_use]
    pub fn new() -> Box<Self> {
        Box::new(Self {
            track_target_goal: TrackTargetGoal::with_default(true),
            target: None,
        })
    }
}

impl Goal for PersistentAngerTargetGoal {
    fn can_start<'a>(&'a mut self, mob: &'a dyn Mob) -> GoalFuture<'a, bool> {
        Box::pin(async move {
            let Some(neutral) = mob.as_neutral() else {
                return false;
            };
            let Some(target_uuid) = neutral.persistent_anger_state().target_uuid() else {
                return false;
            };
            let world = mob.get_entity().world.load_full();
            let Some(target) = world.get_entity_by_uuid(target_uuid) else {
                return false;
            };
            if !neutral.is_angry_at(&target).await {
                return false;
            }
            self.target = Some(target);
            true
        })
    }

    fn should_continue<'a>(&'a self, mob: &'a dyn Mob) -> GoalFuture<'a, bool> {
        Box::pin(async move {
            let Some(neutral) = mob.as_neutral() else {
                return false;
            };
            let Some(target) = mob.get_mob_entity().get_target().await else {
                return false;
            };
            neutral.is_angry_at(&target).await && self.track_target_goal.should_continue(mob).await
        })
    }

    fn start<'a>(&'a mut self, mob: &'a dyn Mob) -> GoalFuture<'a, ()> {
        Box::pin(async move {
            mob.set_mob_target(self.target.clone()).await;
            self.track_target_goal.start(mob).await;
        })
    }

    fn stop<'a>(&'a mut self, mob: &'a dyn Mob) -> GoalFuture<'a, ()> {
        Box::pin(async move {
            self.target = None;
            self.track_target_goal.stop(mob).await;
        })
    }

    fn controls(&self) -> Controls {
        Controls::TARGET
    }
}
