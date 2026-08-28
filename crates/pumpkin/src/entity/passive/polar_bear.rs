use std::sync::{
    Arc, Weak,
    atomic::{AtomicBool, AtomicI32, Ordering},
};

use pumpkin_data::entity::EntityType;
use pumpkin_data::item_stack::ItemStack;
use pumpkin_nbt::compound::NbtCompound;
use pumpkin_protocol::java::client::play::Metadata;

use crate::entity::{
    Entity, EntityBase, EntityBaseFuture, NbtFuture,
    ageable::{AgeableData, AgeableMob},
    ai::goal::{
        Controls, Goal, GoalFuture, active_target::ActiveTargetGoal,
        escape_danger::EscapeDangerGoal, follow_parent::FollowParentGoal,
        look_around::RandomLookAroundGoal, look_at_entity::LookAtEntityGoal,
        persistent_anger_target::PersistentAngerTargetGoal, revenge::RevengeGoal, swim::SwimGoal,
        wander_around::WanderAroundGoal,
    },
    ai::neutral::{NeutralMob, PersistentAngerState},
    mob::{Mob, MobEntity},
    passive::animal::Animal,
};

/// Represents a Polar Bear, a neutral mob found in cold biomes.
///
/// Wiki: <https://minecraft.wiki/w/Polar_Bear>
pub struct PolarBearEntity {
    pub mob_entity: MobEntity,
    pub ageable_data: AgeableData,
    pub persistent_anger: PersistentAngerState,
    pub standing: AtomicBool,
    pub warning_sound_ticks: AtomicI32,
}

impl PolarBearEntity {
    pub fn new(entity: Entity) -> Arc<Self> {
        let mob_entity = MobEntity::new(entity);
        let polar_bear = Self {
            mob_entity,
            ageable_data: AgeableData::default(),
            persistent_anger: PersistentAngerState::default(),
            standing: AtomicBool::new(false),
            warning_sound_ticks: AtomicI32::new(0),
        };
        let mob_arc = Arc::new(polar_bear);
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
            goal_selector.add_goal(1, EscapeDangerGoal::new(2.0));
            goal_selector.add_goal(1, Box::new(WanderAroundGoal::new(1.0)));
            goal_selector.add_goal(4, Box::new(FollowParentGoal::new(1.25)));
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
            target_selector.add_goal(1, Box::new(PolarBearRevengeGoal::default()));
            target_selector.add_goal(2, PersistentAngerTargetGoal::new());
            target_selector.add_goal(2, Box::new(PolarBearAttackPlayersGoal::default()));
            // Polar bears are neutral but aggressive towards foxes
            target_selector.add_goal(
                1,
                Box::new(PolarBearFoxTargetGoal {
                    inner: ActiveTargetGoal::with_default(
                        &mob_arc.mob_entity,
                        &EntityType::FOX,
                        true,
                    ),
                }),
            );
        };

        mob_arc
    }

    #[must_use]
    pub fn is_standing(&self) -> bool {
        self.standing.load(Ordering::Relaxed)
    }

    pub fn set_standing(&self, standing: bool) {
        if self.standing.swap(standing, Ordering::Relaxed) != standing {
            self.get_entity().send_meta_data(
                &[Metadata::new(
                    pumpkin_data::tracked_data::polar_bear::STANDING_ID,
                    standing,
                )],
                None,
            );
        }
    }

    fn play_warning_sound(&self) {
        if self.warning_sound_ticks.load(Ordering::Relaxed) <= 0 {
            self.get_entity()
                .play_sound(pumpkin_data::sound::Sound::EntityPolarBearWarning);
            self.warning_sound_ticks.store(40, Ordering::Relaxed);
        }
    }

    async fn alert_adult_bears(&self, attacker: Arc<dyn EntityBase>) {
        let entity = self.get_entity();
        let world = entity.world.load();
        let box_area = entity.bounding_box.load().expand(8.0, 4.0, 8.0);

        for candidate in world.get_entities_at_box(&box_area) {
            let candidate_entity = candidate.get_entity();
            if !polar_bear_should_alert_adult(
                candidate_entity.entity_type == &EntityType::POLAR_BEAR,
                candidate_entity.entity_id == entity.entity_id,
                candidate_entity.age.load(Ordering::Relaxed) < 0,
            ) {
                continue;
            }

            if let Some(candidate_mob) = candidate.get_mob() {
                candidate_mob.set_mob_target(Some(attacker.clone())).await;
            }
        }
    }
}

impl AgeableMob for PolarBearEntity {
    fn get_ageable_data(&self) -> &AgeableData {
        &self.ageable_data
    }
}

impl Animal for PolarBearEntity {
    fn is_food(&self, _item_stack: &ItemStack) -> bool {
        false
    }
}

impl NeutralMob for PolarBearEntity {
    fn persistent_anger_state(&self) -> &PersistentAngerState {
        &self.persistent_anger
    }
}

impl Mob for PolarBearEntity {
    fn as_ageable(&self) -> Option<&dyn AgeableMob> {
        Some(self)
    }

    fn as_animal(&self) -> Option<&dyn Animal> {
        Some(self)
    }

    fn as_neutral(&self) -> Option<&dyn NeutralMob> {
        Some(self)
    }

    fn on_damage<'a>(
        &'a self,
        _damage_type: pumpkin_data::damage::DamageType,
        source: Option<&'a dyn EntityBase>,
    ) -> EntityBaseFuture<'a, ()> {
        Box::pin(async move {
            if !self.is_baby() {
                return;
            }

            let Some(source) = source else {
                return;
            };
            if source.get_living_entity().is_none() {
                return;
            }

            let world = self.get_entity().world.load();
            let Some(attacker) = world.get_entity_by_id(source.get_entity().entity_id) else {
                return;
            };
            self.alert_adult_bears(attacker).await;
            self.set_mob_target(None).await;
        })
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

    fn mob_tick<'a>(&'a self, _caller: &'a Arc<dyn EntityBase>) -> EntityBaseFuture<'a, ()> {
        Box::pin(async move {
            self.ageable_ai_step();
            self.update_persistent_anger(true).await;

            let target = self.get_mob_entity().get_target().await;
            let distance_sq = target.as_ref().map_or(f64::INFINITY, |target| {
                self.get_entity()
                    .pos
                    .load()
                    .squared_distance_to_vec(&target.get_entity().pos.load())
            });
            let should_stand = polar_bear_should_stand(
                self.is_baby(),
                target
                    .as_ref()
                    .is_some_and(|target| target.get_entity().is_alive()),
                distance_sq,
            );
            self.set_standing(should_stand);

            let warning_ticks = self.warning_sound_ticks.load(Ordering::Relaxed);
            if warning_ticks > 0 {
                self.warning_sound_ticks.fetch_sub(1, Ordering::Relaxed);
            } else if should_stand {
                self.play_warning_sound();
            }
        })
    }

    fn mob_init_data_tracker(&self) -> EntityBaseFuture<'_, ()> {
        Box::pin(async move {
            let entity = self.get_entity();
            if self.is_baby() {
                entity.send_meta_data(
                    &[Metadata::new(
                        pumpkin_data::tracked_data::polar_bear::BABY_ID,
                        true,
                    )],
                    None,
                );
            }
            entity.send_meta_data(
                &[Metadata::new(
                    pumpkin_data::tracked_data::polar_bear::STANDING_ID,
                    self.is_standing(),
                )],
                None,
            );
        })
    }

    fn get_mob_entity(&self) -> &MobEntity {
        &self.mob_entity
    }
}

struct PolarBearRevengeGoal {
    inner: RevengeGoal,
}

struct PolarBearFoxTargetGoal {
    inner: Box<ActiveTargetGoal>,
}

impl Goal for PolarBearFoxTargetGoal {
    fn can_start<'a>(&'a mut self, mob: &'a dyn Mob) -> GoalFuture<'a, bool> {
        Box::pin(async move {
            if mob.as_ageable().is_some_and(AgeableMob::is_baby) {
                return false;
            }
            self.inner.can_start(mob).await
        })
    }

    fn should_continue<'a>(&'a self, mob: &'a dyn Mob) -> GoalFuture<'a, bool> {
        self.inner.should_continue(mob)
    }

    fn start<'a>(&'a mut self, mob: &'a dyn Mob) -> GoalFuture<'a, ()> {
        self.inner.start(mob)
    }

    fn stop<'a>(&'a mut self, mob: &'a dyn Mob) -> GoalFuture<'a, ()> {
        self.inner.stop(mob)
    }

    fn controls(&self) -> Controls {
        self.inner.controls()
    }
}

impl Default for PolarBearRevengeGoal {
    fn default() -> Self {
        Self {
            inner: RevengeGoal::new(true),
        }
    }
}

impl Goal for PolarBearRevengeGoal {
    fn can_start<'a>(&'a mut self, mob: &'a dyn Mob) -> GoalFuture<'a, bool> {
        Box::pin(async move {
            if mob.as_ageable().is_some_and(AgeableMob::is_baby) {
                return false;
            }
            self.inner.can_start(mob).await
        })
    }

    fn should_continue<'a>(&'a self, mob: &'a dyn Mob) -> GoalFuture<'a, bool> {
        self.inner.should_continue(mob)
    }

    fn start<'a>(&'a mut self, mob: &'a dyn Mob) -> GoalFuture<'a, ()> {
        self.inner.start(mob)
    }

    fn stop<'a>(&'a mut self, mob: &'a dyn Mob) -> GoalFuture<'a, ()> {
        self.inner.stop(mob)
    }

    fn controls(&self) -> Controls {
        self.inner.controls()
    }
}

#[derive(Default)]
struct PolarBearAttackPlayersGoal {
    target: Option<Arc<dyn EntityBase>>,
    cooldown: i32,
}

impl Goal for PolarBearAttackPlayersGoal {
    fn can_start<'a>(&'a mut self, mob: &'a dyn Mob) -> GoalFuture<'a, bool> {
        Box::pin(async move {
            if mob.as_ageable().is_some_and(AgeableMob::is_baby) || self.cooldown > 0 {
                self.cooldown = (self.cooldown - 1).max(0);
                return false;
            }
            self.cooldown = 10;

            let entity = mob.get_entity();
            let world = entity.world.load();
            let box_area = entity.bounding_box.load().expand(8.0, 4.0, 8.0);
            let has_baby = world
                .get_entities_at_box(&box_area)
                .into_iter()
                .any(|candidate| {
                    candidate.get_entity().entity_type == &EntityType::POLAR_BEAR
                        && candidate.get_entity().age.load(Ordering::Relaxed) < 0
                });
            if !has_baby {
                return false;
            }

            let player = world.get_closest_player(entity.pos.load(), 10.0);
            self.target = player.and_then(|player| {
                (!matches!(
                    player.gamemode.load(),
                    pumpkin_util::GameMode::Creative | pumpkin_util::GameMode::Spectator
                ))
                .then_some(player as Arc<dyn EntityBase>)
            });
            self.target.is_some()
        })
    }

    fn should_continue<'a>(&'a self, _mob: &'a dyn Mob) -> GoalFuture<'a, bool> {
        Box::pin(async move {
            self.target.as_ref().is_some_and(|target| {
                target.get_entity().is_alive()
                    && target.get_player().is_none_or(|player| {
                        !matches!(
                            player.gamemode.load(),
                            pumpkin_util::GameMode::Creative | pumpkin_util::GameMode::Spectator
                        )
                    })
            })
        })
    }

    fn start<'a>(&'a mut self, mob: &'a dyn Mob) -> GoalFuture<'a, ()> {
        Box::pin(async move {
            mob.set_mob_target(self.target.clone()).await;
        })
    }

    fn stop<'a>(&'a mut self, mob: &'a dyn Mob) -> GoalFuture<'a, ()> {
        Box::pin(async move {
            mob.set_mob_target(None).await;
            self.target = None;
        })
    }

    fn controls(&self) -> Controls {
        Controls::TARGET
    }
}

const fn polar_bear_should_stand(is_baby: bool, target_alive: bool, distance_sq: f64) -> bool {
    !is_baby && target_alive && distance_sq <= 16.0
}

const fn polar_bear_should_alert_adult(
    same_type: bool,
    is_self: bool,
    candidate_is_baby: bool,
) -> bool {
    same_type && !is_self && !candidate_is_baby
}

#[cfg(test)]
mod tests {
    use super::{polar_bear_should_alert_adult, polar_bear_should_stand};

    #[test]
    fn standing_requires_adult_live_close_target() {
        assert!(!polar_bear_should_stand(true, true, 4.0));
        assert!(!polar_bear_should_stand(false, false, 4.0));
        assert!(!polar_bear_should_stand(false, true, 17.0));
        assert!(polar_bear_should_stand(false, true, 16.0));
    }

    #[test]
    fn baby_alert_only_selects_other_adult_bears() {
        assert!(polar_bear_should_alert_adult(true, false, false));
        assert!(!polar_bear_should_alert_adult(false, false, false));
        assert!(!polar_bear_should_alert_adult(true, true, false));
        assert!(!polar_bear_should_alert_adult(true, false, true));
    }
}
