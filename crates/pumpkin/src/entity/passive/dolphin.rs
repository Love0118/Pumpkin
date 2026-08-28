use std::sync::{Arc, Weak};

use pumpkin_data::entity::EntityType;

use crate::entity::{
    Entity,
    ai::goal::{
        look_around::RandomLookAroundGoal,
        look_at_entity::LookAtEntityGoal,
        revenge::{EntityTypeFilter, RevengeGoal},
        swim::SwimGoal,
        try_find_water::TryFindWaterGoal,
        wander_around::WanderAroundGoal,
    },
    mob::{Mob, MobEntity},
};

const DOLPHIN_IGNORED_DAMAGE_TYPES: &[EntityTypeFilter] = &[
    EntityTypeFilter::Exact(&EntityType::GUARDIAN),
    EntityTypeFilter::Exact(&EntityType::ELDER_GUARDIAN),
];

/// Represents a Dolphin, a neutral aquatic mob that can give players the Dolphin's Grace effect.
///
/// Wiki: <https://minecraft.wiki/w/Dolphin>
pub struct DolphinEntity {
    pub mob_entity: MobEntity,
}

impl DolphinEntity {
    pub fn new(entity: Entity) -> Arc<Self> {
        let mob_entity = MobEntity::new(entity);
        let dolphin = Self { mob_entity };
        let mob_arc = Arc::new(dolphin);
        let mob_weak: Weak<dyn Mob> = {
            let mob_arc: Arc<dyn Mob> = mob_arc.clone();
            Arc::downgrade(&mob_arc)
        };

        {
            let mut goal_selector = mob_arc
                .mob_entity
                .goals_selector
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);

            goal_selector.add_goal(0, Box::new(TryFindWaterGoal));
            goal_selector.add_goal(0, Box::new(SwimGoal::default()));
            goal_selector.add_goal(1, Box::new(WanderAroundGoal::new(1.0)));
            goal_selector.add_goal(
                2,
                LookAtEntityGoal::with_default(mob_weak, &EntityType::PLAYER, 6.0),
            );
            goal_selector.add_goal(3, Box::new(RandomLookAroundGoal::default()));
        };

        {
            let mut target_selector = mob_arc
                .mob_entity
                .target_selector
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            target_selector.add_goal(
                1,
                Box::new(
                    RevengeGoal::new(true)
                        .ignore_damage_from(DOLPHIN_IGNORED_DAMAGE_TYPES)
                        .set_alert_others(),
                ),
            );
        };

        mob_arc
    }
}

impl Mob for DolphinEntity {
    fn get_mob_entity(&self) -> &MobEntity {
        &self.mob_entity
    }
}
