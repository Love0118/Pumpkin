use std::sync::{
    Arc, Weak,
    atomic::{AtomicBool, AtomicI32, Ordering},
};

use pumpkin_data::{
    attributes::Attributes,
    entity::EntityType,
    sound::{Sound, SoundCategory},
};
use pumpkin_nbt::compound::NbtCompound;
use rand::RngExt;

use crate::entity::{
    Entity, EntityBase, EntityBaseFuture, NbtFuture,
    ai::goal::{
        look_around::RandomLookAroundGoal, look_at_entity::LookAtEntityGoal,
        melee_attack::MeleeAttackGoal, persistent_anger_target::PersistentAngerTargetGoal,
        revenge::RevengeGoal, swim::SwimGoal, wander_around::WanderAroundGoal,
    },
    ai::neutral::{NeutralMob, PersistentAngerState},
    mob::{Mob, MobEntity},
};

const SPEED_BOOST: f64 = 0.05;
const ZOMBIFIED_PIGLIN_SPEED_BOOST_ID: &str = "minecraft:attacking";
const FIRST_ANGER_SOUND_MAX_DELAY: i32 = 20;
const ALERT_MIN_DELAY: i32 = 80;
const ALERT_MAX_DELAY: i32 = 120;
const ALERT_RANGE_Y: f64 = 10.0;

pub struct ZombifiedPiglinEntity {
    pub mob_entity: MobEntity,
    pub persistent_anger: PersistentAngerState,
    speed_boosted: AtomicBool,
    last_target_id: AtomicI32,
    first_anger_sound_in: AtomicI32,
    ticks_until_next_alert: AtomicI32,
}

impl ZombifiedPiglinEntity {
    pub fn new(entity: Entity) -> Arc<Self> {
        let mob_entity = MobEntity::new(entity);
        let piglin = Self {
            mob_entity,
            persistent_anger: PersistentAngerState::default(),
            speed_boosted: AtomicBool::new(false),
            last_target_id: AtomicI32::new(0),
            first_anger_sound_in: AtomicI32::new(-1),
            ticks_until_next_alert: AtomicI32::new(0),
        };
        let mob_arc = Arc::new(piglin);
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

            goal_selector.add_goal(0, Box::new(SwimGoal::default()));
            goal_selector.add_goal(2, Box::new(MeleeAttackGoal::new(1.0, true)));
            goal_selector.add_goal(5, Box::new(WanderAroundGoal::new(1.0)));
            goal_selector.add_goal(
                6,
                LookAtEntityGoal::with_default(mob_weak.clone(), &EntityType::PLAYER, 8.0),
            );
            goal_selector.add_goal(7, Box::new(RandomLookAroundGoal::default()));

            let mut target_selector = mob_arc
                .mob_entity
                .target_selector
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            target_selector.add_goal(1, Box::new(RevengeGoal::new(true).set_alert_others()));
            target_selector.add_goal(2, PersistentAngerTargetGoal::new());
        };

        mob_arc
    }
}

impl NeutralMob for ZombifiedPiglinEntity {
    fn persistent_anger_state(&self) -> &PersistentAngerState {
        &self.persistent_anger
    }
}

impl Mob for ZombifiedPiglinEntity {
    fn as_neutral(&self) -> Option<&dyn NeutralMob> {
        Some(self)
    }

    fn mob_write_nbt<'a>(&'a self, nbt: &'a mut NbtCompound) -> NbtFuture<'a, ()> {
        Box::pin(async move {
            self.write_persistent_anger_nbt(nbt);
        })
    }

    fn mob_read_nbt<'a>(&'a self, nbt: &'a NbtCompound) -> NbtFuture<'a, ()> {
        Box::pin(async move {
            self.read_persistent_anger_nbt(nbt).await;
        })
    }

    #[expect(clippy::too_many_lines)]
    fn mob_tick<'a>(&'a self, _caller: &'a Arc<dyn EntityBase>) -> EntityBaseFuture<'a, ()> {
        Box::pin(async move {
            self.update_persistent_anger(true).await;

            let entity = &self.mob_entity.living_entity.entity;
            let world = entity.world.load();
            let game_time = world.get_world_age().await;
            let angry = self.persistent_anger.is_angry(game_time);
            let is_baby = entity.age.load(Ordering::Relaxed) < 0;
            let living = &self.mob_entity.living_entity;

            if angry && !is_baby {
                if !self.speed_boosted.swap(true, Ordering::Relaxed) {
                    living.update_attribute(&Attributes::MOVEMENT_SPEED, |inst| {
                        inst.add_or_replace_modifier(crate::entity::attributes::Modifier {
                            id: ZOMBIFIED_PIGLIN_SPEED_BOOST_ID.to_string(),
                            amount: SPEED_BOOST,
                            operation: crate::entity::attributes::ModifierOperation::Add,
                        });
                    });
                    crate::entity::attributes::send_attribute_updates_for_living(
                        living,
                        vec![Attributes::MOVEMENT_SPEED],
                    )
                    .await;
                }
            } else if self.speed_boosted.swap(false, Ordering::Relaxed) {
                living.update_attribute(&Attributes::MOVEMENT_SPEED, |inst| {
                    inst.remove_modifier(ZOMBIFIED_PIGLIN_SPEED_BOOST_ID);
                });
                crate::entity::attributes::send_attribute_updates_for_living(
                    living,
                    vec![Attributes::MOVEMENT_SPEED],
                )
                .await;
            }

            let target = self.mob_entity.get_target().await;
            let target_id = target
                .as_ref()
                .map_or(0, |target| target.get_entity().entity_id);
            let previous_target_id = self.last_target_id.swap(target_id, Ordering::Relaxed);
            if target_id == 0 {
                self.first_anger_sound_in.store(-1, Ordering::Relaxed);
            } else if previous_target_id == 0 {
                self.first_anger_sound_in.store(
                    self.get_random()
                        .random_range(0..=FIRST_ANGER_SOUND_MAX_DELAY),
                    Ordering::Relaxed,
                );
                self.ticks_until_next_alert.store(
                    self.get_random()
                        .random_range(ALERT_MIN_DELAY..=ALERT_MAX_DELAY),
                    Ordering::Relaxed,
                );
            }

            let anger_sound_delay = self.first_anger_sound_in.load(Ordering::Relaxed);
            if anger_sound_delay > 0 {
                self.first_anger_sound_in.fetch_sub(1, Ordering::Relaxed);
            } else if anger_sound_delay == 0 {
                world.play_sound_fine(
                    Sound::EntityZombifiedPiglinAngry,
                    SoundCategory::Hostile,
                    &entity.pos.load(),
                    2.0,
                    1.8,
                );
                self.first_anger_sound_in.store(-1, Ordering::Relaxed);
            }

            let Some(target) = target else {
                return;
            };
            let alert_delay = self.ticks_until_next_alert.load(Ordering::Relaxed);
            if alert_delay > 0 {
                self.ticks_until_next_alert.fetch_sub(1, Ordering::Relaxed);
                return;
            }
            if !self
                .mob_entity
                .sensing
                .has_line_of_sight(entity, target.get_entity())
                .await
            {
                self.ticks_until_next_alert.store(
                    self.get_random()
                        .random_range(ALERT_MIN_DELAY..=ALERT_MAX_DELAY),
                    Ordering::Relaxed,
                );
                return;
            }

            let position = entity.pos.load();
            let follow_range = living.get_attribute_value(&Attributes::FOLLOW_RANGE);
            for candidate in world.entities.load().iter() {
                let candidate_entity = candidate.get_entity();
                if candidate_entity.entity_id == entity.entity_id
                    || candidate_entity.entity_type != &EntityType::ZOMBIFIED_PIGLIN
                {
                    continue;
                }
                let Some(candidate_mob) = candidate.get_mob() else {
                    continue;
                };
                if candidate_mob.get_mob_entity().get_target().await.is_some() {
                    continue;
                }
                let candidate_pos = candidate_entity.pos.load();
                if (candidate_pos.y - position.y).abs() > ALERT_RANGE_Y
                    || (candidate_pos.x - position.x).abs() > follow_range
                    || (candidate_pos.z - position.z).abs() > follow_range
                    || world
                        .entities_are_allied(candidate_entity, target.get_entity())
                        .await
                {
                    continue;
                }
                candidate_mob.set_mob_target(Some(target.clone())).await;
            }
            self.ticks_until_next_alert.store(
                self.get_random()
                    .random_range(ALERT_MIN_DELAY..=ALERT_MAX_DELAY),
                Ordering::Relaxed,
            );
        })
    }

    fn get_mob_entity(&self) -> &MobEntity {
        &self.mob_entity
    }
}
