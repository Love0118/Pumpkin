use std::sync::Arc;

use crate::entity::mob::zombie::ZombieEntityBase;
use crate::entity::{
    Entity, NbtFuture,
    ai::goal::revenge::{EntityTypeFilter, RevengeGoal},
    mob::{Mob, MobEntity},
};
use pumpkin_data::entity::EntityType;
use pumpkin_nbt::compound::NbtCompound;

pub struct DrownedEntity {
    entity: Arc<ZombieEntityBase>,
}

impl DrownedEntity {
    pub fn new(entity: Entity) -> Arc<Self> {
        let entity = ZombieEntityBase::new(entity);
        let zombie = Self { entity };
        let mob_arc = Arc::new(zombie);
        let mut target_selector = mob_arc
            .entity
            .mob_entity
            .target_selector
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        target_selector.remove_goal_sync::<RevengeGoal>();
        target_selector.add_goal(
            1,
            Box::new(
                RevengeGoal::new(true)
                    .ignore_damage_from(&[EntityTypeFilter::Exact(&EntityType::DROWNED)])
                    .set_alert_others_except(&[EntityTypeFilter::Exact(
                        &EntityType::ZOMBIFIED_PIGLIN,
                    )]),
            ),
        );
        drop(target_selector);

        mob_arc
    }

    #[must_use]
    pub fn with_can_break_doors(entity: Entity, can_break_doors: bool) -> Arc<Self> {
        let entity = ZombieEntityBase::with_can_break_doors(entity, can_break_doors);
        let zombie = Self { entity };
        let mob_arc = Arc::new(zombie);
        let mut target_selector = mob_arc
            .entity
            .mob_entity
            .target_selector
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        target_selector.remove_goal_sync::<RevengeGoal>();
        target_selector.add_goal(
            1,
            Box::new(
                RevengeGoal::new(true)
                    .ignore_damage_from(&[EntityTypeFilter::Exact(&EntityType::DROWNED)])
                    .set_alert_others_except(&[EntityTypeFilter::Exact(
                        &EntityType::ZOMBIFIED_PIGLIN,
                    )]),
            ),
        );
        drop(target_selector);
        mob_arc
    }
}

impl Mob for DrownedEntity {
    fn get_mob_entity(&self) -> &MobEntity {
        &self.entity.mob_entity
    }

    fn mob_write_nbt<'a>(&'a self, nbt: &'a mut NbtCompound) -> NbtFuture<'a, ()> {
        Box::pin(async move {
            self.entity.mob_write_nbt(nbt).await;
        })
    }

    fn mob_read_nbt<'a>(&'a self, nbt: &'a NbtCompound) -> NbtFuture<'a, ()> {
        Box::pin(async move {
            self.entity.mob_read_nbt(nbt).await;
        })
    }
}
