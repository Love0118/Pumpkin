use std::sync::atomic::{AtomicI32, AtomicU8, AtomicU32, Ordering};
use std::sync::{Arc, Weak};

use pumpkin_data::attributes::Attributes;
use pumpkin_data::damage::DamageType;
use pumpkin_data::effect::StatusEffect;
use pumpkin_data::entity::EntityType;
use pumpkin_data::potion::Effect;
use pumpkin_nbt::compound::NbtCompound;
use pumpkin_protocol::java::client::play::Metadata;
use pumpkin_util::Difficulty;
use rand::RngExt;

use crate::entity::{
    DamageContext, Entity, EntityBase, EntityBaseFuture, NbtFuture,
    ai::goal::{Controls, Goal, GoalFuture},
    ai::goal::{
        look_around::RandomLookAroundGoal, look_at_entity::LookAtEntityGoal,
        persistent_anger_target::PersistentAngerTargetGoal, revenge::RevengeGoal, swim::SwimGoal,
        wander_around::WanderAroundGoal,
    },
    ai::neutral::{NeutralMob, PersistentAngerState},
    ai::pathfinder::NavigatorGoal,
    mob::{Mob, MobEntity},
};

const FLAG_ROLLING: u8 = 0x02;
const FLAG_STUNG: u8 = 0x04;
const FLAG_NECTAR: u8 = 0x08;
const STING_DEATH_CHECK_INTERVAL: i32 = 5;
const STING_DEATH_START_TICKS: i32 = 1_200;
const TIRED_OF_NECTAR_TICKS: i32 = 3_600;

/// Represents a Bee, a neutral flying mob that can pollinate crops and sting attackers.
///
/// Wiki: <https://minecraft.wiki/w/Bee>
pub struct BeeEntity {
    pub mob_entity: MobEntity,
    pub persistent_anger: PersistentAngerState,
    flags: AtomicU8,
    time_since_sting: AtomicI32,
    under_water_ticks: AtomicI32,
    ticks_without_nectar: AtomicI32,
    stay_out_of_hive_ticks: AtomicI32,
    crops_grown_since_pollination: AtomicI32,
    roll_amount: AtomicU32,
    previous_roll_amount: AtomicU32,
}

impl BeeEntity {
    pub fn new(entity: Entity) -> Arc<Self> {
        let mob_entity = MobEntity::new(entity);
        let bee = Self {
            mob_entity,
            persistent_anger: PersistentAngerState::default(),
            flags: AtomicU8::new(0),
            time_since_sting: AtomicI32::new(0),
            under_water_ticks: AtomicI32::new(0),
            ticks_without_nectar: AtomicI32::new(0),
            stay_out_of_hive_ticks: AtomicI32::new(0),
            crops_grown_since_pollination: AtomicI32::new(0),
            roll_amount: AtomicU32::new(0.0f32.to_bits()),
            previous_roll_amount: AtomicU32::new(0.0f32.to_bits()),
        };
        let mob_arc = Arc::new(bee);
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

            goal_selector.add_goal(0, Box::new(BeeAttackGoal::new(1.4)));
            goal_selector.add_goal(0, Box::new(SwimGoal::default()));
            goal_selector.add_goal(1, Box::new(WanderAroundGoal::new(1.0)));
            goal_selector.add_goal(
                2,
                LookAtEntityGoal::with_default(mob_weak, &EntityType::PLAYER, 6.0),
            );
            goal_selector.add_goal(3, Box::new(RandomLookAroundGoal::default()));

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

    #[must_use]
    pub fn has_stung(&self) -> bool {
        self.flags.load(Ordering::Relaxed) & FLAG_STUNG != 0
    }

    pub fn set_has_stung(&self, has_stung: bool) {
        self.set_flag(FLAG_STUNG, has_stung);
        if !has_stung {
            self.time_since_sting.store(0, Ordering::Relaxed);
        }
    }

    #[must_use]
    pub fn has_nectar(&self) -> bool {
        self.flags.load(Ordering::Relaxed) & FLAG_NECTAR != 0
    }

    pub fn set_has_nectar(&self, has_nectar: bool) {
        if has_nectar {
            self.ticks_without_nectar.store(0, Ordering::Relaxed);
        }
        self.set_flag(FLAG_NECTAR, has_nectar);
    }

    pub fn drop_off_nectar(&self) {
        self.set_has_nectar(false);
        self.crops_grown_since_pollination
            .store(0, Ordering::Relaxed);
    }

    #[must_use]
    pub fn is_rolling(&self) -> bool {
        self.flags.load(Ordering::Relaxed) & FLAG_ROLLING != 0
    }

    #[must_use]
    pub fn roll_amount(&self, interpolation: f32) -> f32 {
        let previous = f32::from_bits(self.previous_roll_amount.load(Ordering::Relaxed));
        let current = f32::from_bits(self.roll_amount.load(Ordering::Relaxed));
        previous + (current - previous) * interpolation.clamp(0.0, 1.0)
    }

    pub fn set_stay_out_of_hive_ticks(&self, ticks: i32) {
        self.stay_out_of_hive_ticks
            .store(ticks.max(0), Ordering::Relaxed);
    }

    /// The hive goal may use this guard once the shared hive/POI owner is available.
    /// Keeping it here ensures a stung or actively attacking bee cannot be re-homed.
    pub async fn can_enter_hive(&self) -> bool {
        if self.has_stung()
            || self.is_rolling()
            || self.stay_out_of_hive_ticks.load(Ordering::Relaxed) > 0
        {
            return false;
        }
        let has_reason_to_enter = self.has_nectar()
            || self.ticks_without_nectar.load(Ordering::Relaxed) > TIRED_OF_NECTAR_TICKS;
        if !has_reason_to_enter {
            return false;
        }
        self.mob_entity.get_target().await.is_none()
    }

    fn set_flag(&self, flag: u8, value: bool) {
        let old_flags = self.flags.load(Ordering::Relaxed);
        let new_flags = if value {
            old_flags | flag
        } else {
            old_flags & !flag
        };
        if old_flags != new_flags {
            self.flags.store(new_flags, Ordering::Relaxed);
            self.mob_entity.living_entity.entity.send_meta_data(
                &[Metadata::new(
                    pumpkin_data::tracked_data::bee::DATA_FLAGS_ID,
                    new_flags,
                )],
                None,
            );
        }
    }

    fn update_roll_amount(&self) {
        let current = f32::from_bits(self.roll_amount.load(Ordering::Relaxed));
        self.previous_roll_amount
            .store(current.to_bits(), Ordering::Relaxed);
        let next = if self.is_rolling() {
            (current + 0.2).min(1.0)
        } else {
            (current - 0.24).max(0.0)
        };
        self.roll_amount.store(next.to_bits(), Ordering::Relaxed);
    }

    async fn sting_target(&self, target: &dyn EntityBase) -> bool {
        let damage = self
            .mob_entity
            .living_entity
            .get_attribute_value(&Attributes::ATTACK_DAMAGE) as f32;
        let context = DamageContext::new(damage, DamageType::STING)
            .with_direct_entity(self)
            .with_causing_entity(self);
        if !target.damage_with_context(target, context).await {
            return false;
        }

        let difficulty = self
            .mob_entity
            .living_entity
            .entity
            .world
            .load()
            .level_info
            .load()
            .difficulty;
        let poison_ticks = sting_poison_duration_ticks(difficulty);
        if poison_ticks > 0
            && let Some(living) = target.get_living_entity()
        {
            living
                .add_effect(Effect {
                    effect_type: &StatusEffect::POISON,
                    duration: poison_ticks,
                    amplifier: 0,
                    ambient: false,
                    show_particles: true,
                    show_icon: true,
                    blend: false,
                })
                .await;
        }

        self.set_has_stung(true);
        self.stop_being_angry().await;
        self.mob_entity
            .living_entity
            .entity
            .play_sound(pumpkin_data::sound::Sound::EntityBeeSting);
        true
    }

    async fn tick_sting_lifecycle(&self) {
        let under_water_ticks = if self
            .mob_entity
            .living_entity
            .entity
            .touching_water
            .load(Ordering::Relaxed)
        {
            self.under_water_ticks.fetch_add(1, Ordering::Relaxed) + 1
        } else {
            self.under_water_ticks.store(0, Ordering::Relaxed);
            0
        };
        if under_water_ticks > 20 {
            self.mob_entity
                .living_entity
                .damage_with_context(self, DamageContext::new(1.0, DamageType::DROWN))
                .await;
        }

        if !self.has_stung() {
            return;
        }
        let ticks = self.time_since_sting.fetch_add(1, Ordering::Relaxed) + 1;
        if ticks % STING_DEATH_CHECK_INTERVAL != 0 {
            return;
        }
        let upper_bound = (STING_DEATH_START_TICKS - ticks).clamp(1, STING_DEATH_START_TICKS);
        if self.get_random().random_range(0..upper_bound) == 0 {
            let health = self.mob_entity.living_entity.health.load();
            self.mob_entity
                .living_entity
                .damage_with_context(self, DamageContext::new(health, DamageType::GENERIC))
                .await;
        }
    }
}

const fn sting_poison_duration_ticks(difficulty: Difficulty) -> i32 {
    match difficulty {
        Difficulty::Normal => 10 * 20,
        Difficulty::Hard => 18 * 20,
        Difficulty::Peaceful | Difficulty::Easy => 0,
    }
}

struct BeeAttackGoal {
    speed: f64,
    cooldown: i32,
}

impl BeeAttackGoal {
    const fn new(speed: f64) -> Self {
        Self { speed, cooldown: 0 }
    }
}

impl Goal for BeeAttackGoal {
    fn can_start<'a>(&'a mut self, mob: &'a dyn Mob) -> GoalFuture<'a, bool> {
        Box::pin(async move {
            let Some(bee) = mob.cast_any().downcast_ref::<BeeEntity>() else {
                return false;
            };
            if bee.has_stung() {
                return false;
            }
            mob.get_mob_entity()
                .get_target()
                .await
                .is_some_and(|target| target.get_entity().is_alive())
        })
    }

    fn should_continue<'a>(&'a self, mob: &'a dyn Mob) -> GoalFuture<'a, bool> {
        Box::pin(async move {
            let Some(bee) = mob.cast_any().downcast_ref::<BeeEntity>() else {
                return false;
            };
            if bee.has_stung() {
                return false;
            }
            mob.get_mob_entity()
                .get_target()
                .await
                .is_some_and(|target| target.get_entity().is_alive())
        })
    }

    fn start<'a>(&'a mut self, mob: &'a dyn Mob) -> GoalFuture<'a, ()> {
        Box::pin(async move {
            self.cooldown = 0;
            if let Some(target) = mob.get_mob_entity().get_target().await {
                let target_pos = target.get_entity().pos.load();
                let mob_pos = mob.get_entity().pos.load();
                mob.get_mob_entity()
                    .navigator
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .set_progress(NavigatorGoal {
                        current_progress: mob_pos,
                        destination: target_pos,
                        speed: self.speed,
                    });
            }
        })
    }

    fn stop<'a>(&'a mut self, mob: &'a dyn Mob) -> GoalFuture<'a, ()> {
        Box::pin(async move {
            mob.get_mob_entity()
                .navigator
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .stop();
        })
    }

    fn tick<'a>(&'a mut self, mob: &'a dyn Mob) -> GoalFuture<'a, ()> {
        Box::pin(async move {
            let Some(target) = mob.get_mob_entity().get_target().await else {
                return;
            };
            mob.get_mob_entity()
                .look_control
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .look_at_entity_with_range(&target, 30.0, 30.0);

            if self.cooldown > 0 {
                self.cooldown -= 1;
            }

            let in_range = mob
                .get_mob_entity()
                .is_in_attack_range(target.as_ref())
                .await;
            let visible = mob
                .get_mob_entity()
                .sensing
                .has_line_of_sight(mob.get_entity(), target.get_entity())
                .await;
            if self.cooldown <= 0
                && in_range
                && visible
                && let Some(bee) = mob.cast_any().downcast_ref::<BeeEntity>()
            {
                bee.sting_target(target.as_ref()).await;
                self.cooldown = 20;
            }
        })
    }

    fn should_run_every_tick(&self) -> bool {
        true
    }

    fn controls(&self) -> Controls {
        Controls::MOVE | Controls::LOOK
    }
}

impl NeutralMob for BeeEntity {
    fn persistent_anger_state(&self) -> &PersistentAngerState {
        &self.persistent_anger
    }

    fn on_persistent_anger_end_time_changed(&self, end_time: i64) {
        self.mob_entity.living_entity.entity.send_meta_data(
            &[Metadata::new(
                pumpkin_data::tracked_data::bee::ANGER_END_TIME,
                end_time,
            )],
            None,
        );
    }
}

impl Mob for BeeEntity {
    fn as_neutral(&self) -> Option<&dyn NeutralMob> {
        Some(self)
    }

    fn mob_write_nbt<'a>(&'a self, nbt: &'a mut NbtCompound) -> NbtFuture<'a, ()> {
        Box::pin(async move {
            nbt.put_bool("HasNectar", self.has_nectar());
            nbt.put_bool("HasStung", self.has_stung());
            nbt.put_int(
                "TicksSincePollination",
                self.ticks_without_nectar.load(Ordering::Relaxed),
            );
            nbt.put_int(
                "CannotEnterHiveTicks",
                self.stay_out_of_hive_ticks.load(Ordering::Relaxed),
            );
            nbt.put_int(
                "CropsGrownSincePollination",
                self.crops_grown_since_pollination.load(Ordering::Relaxed),
            );
            self.write_persistent_anger_nbt(nbt);
        })
    }

    fn mob_read_nbt<'a>(&'a self, nbt: &'a NbtCompound) -> NbtFuture<'a, ()> {
        Box::pin(async move {
            self.set_has_nectar(nbt.get_bool("HasNectar").unwrap_or(false));
            self.set_has_stung(nbt.get_bool("HasStung").unwrap_or(false));
            self.ticks_without_nectar.store(
                nbt.get_int("TicksSincePollination").unwrap_or(0).max(0),
                Ordering::Relaxed,
            );
            self.stay_out_of_hive_ticks.store(
                nbt.get_int("CannotEnterHiveTicks").unwrap_or(0).max(0),
                Ordering::Relaxed,
            );
            self.crops_grown_since_pollination.store(
                nbt.get_int("CropsGrownSincePollination")
                    .unwrap_or(0)
                    .max(0),
                Ordering::Relaxed,
            );
            self.read_persistent_anger_nbt(nbt).await;
        })
    }

    fn mob_tick<'a>(&'a self, _caller: &'a Arc<dyn EntityBase>) -> EntityBaseFuture<'a, ()> {
        Box::pin(async move {
            self.tick_sting_lifecycle().await;

            let stay_out = self.stay_out_of_hive_ticks.load(Ordering::Relaxed);
            if stay_out > 0 {
                self.stay_out_of_hive_ticks
                    .store(stay_out - 1, Ordering::Relaxed);
            }
            if !self.has_nectar() {
                self.ticks_without_nectar.fetch_add(1, Ordering::Relaxed);
            }

            let should_roll = if self.has_stung() {
                false
            } else {
                let target = self.mob_entity.get_target().await;
                let position = self.mob_entity.living_entity.entity.pos.load();
                self.persistent_anger.is_angry(
                    self.mob_entity
                        .living_entity
                        .entity
                        .world
                        .load()
                        .get_world_age()
                        .await,
                ) && target.is_some_and(|target| {
                    target
                        .get_entity()
                        .pos
                        .load()
                        .squared_distance_to_vec(&position)
                        < 4.0
                })
            };
            self.set_flag(FLAG_ROLLING, should_roll);
            self.update_roll_amount();
            self.update_persistent_anger(false).await;
        })
    }

    fn mob_init_data_tracker(&self) -> EntityBaseFuture<'_, ()> {
        Box::pin(async move {
            self.mob_entity.living_entity.entity.send_meta_data(
                &[Metadata::new(
                    pumpkin_data::tracked_data::bee::DATA_FLAGS_ID,
                    self.flags.load(Ordering::Relaxed),
                )],
                None,
            );
            self.on_persistent_anger_end_time_changed(self.persistent_anger.end_time());
        })
    }

    fn on_damage<'a>(
        &'a self,
        _damage_type: DamageType,
        source: Option<&'a dyn EntityBase>,
    ) -> EntityBaseFuture<'a, ()> {
        Box::pin(async move {
            let Some(source) = source.filter(|source| source.get_living_entity().is_some()) else {
                return;
            };
            let world = self.mob_entity.living_entity.entity.world.load_full();
            let now = world.get_world_age().await;
            self.persistent_anger
                .set_target_uuid(Some(source.get_entity().entity_uuid));
            self.start_persistent_anger_timer(now);
        })
    }

    fn get_mob_entity(&self) -> &MobEntity {
        &self.mob_entity
    }
}

#[cfg(test)]
mod tests {
    use super::sting_poison_duration_ticks;
    use pumpkin_util::Difficulty;

    #[test]
    fn sting_poison_duration_matches_difficulty() {
        assert_eq!(sting_poison_duration_ticks(Difficulty::Peaceful), 0);
        assert_eq!(sting_poison_duration_ticks(Difficulty::Easy), 0);
        assert_eq!(sting_poison_duration_ticks(Difficulty::Normal), 200);
        assert_eq!(sting_poison_duration_ticks(Difficulty::Hard), 360);
    }
}
