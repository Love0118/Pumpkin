use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use crossbeam::atomic::AtomicCell;
use pumpkin_data::entity::EntityType;
use pumpkin_nbt::compound::NbtCompound;
use pumpkin_util::{Difficulty, GameMode};
use rand::RngExt;
use uuid::Uuid;

use crate::entity::mob::Mob;
use crate::entity::{EntityBase, EntityBaseFuture};
use crate::world::World;

const NO_ANGER_END_TIME: i64 = -1;
const DEFAULT_MIN_ANGER_TICKS: i64 = 20 * 20;
const DEFAULT_MAX_ANGER_TICKS: i64 = 39 * 20;

pub struct PersistentAngerState {
    end_time: AtomicI64,
    target_uuid: AtomicCell<Option<Uuid>>,
}

impl Default for PersistentAngerState {
    fn default() -> Self {
        Self {
            end_time: AtomicI64::new(NO_ANGER_END_TIME),
            target_uuid: AtomicCell::new(None),
        }
    }
}

impl PersistentAngerState {
    #[must_use]
    pub fn end_time(&self) -> i64 {
        self.end_time.load(Ordering::Relaxed)
    }

    pub fn set_end_time(&self, end_time: i64) {
        self.end_time.store(end_time, Ordering::Relaxed);
    }

    #[must_use]
    pub fn target_uuid(&self) -> Option<Uuid> {
        self.target_uuid.load()
    }

    pub fn set_target_uuid(&self, target_uuid: Option<Uuid>) {
        self.target_uuid.store(target_uuid);
    }

    #[must_use]
    pub fn is_angry(&self, game_time: i64) -> bool {
        self.end_time() > game_time
    }

    pub fn stop(&self) {
        self.set_target_uuid(None);
        self.set_end_time(NO_ANGER_END_TIME);
    }

    pub fn write_nbt(&self, nbt: &mut NbtCompound) {
        nbt.put_long("anger_end_time", self.end_time());
        if let Some(target_uuid) = self.target_uuid() {
            nbt.put_uuid("angry_at", target_uuid);
        }
    }

    pub fn read_nbt(&self, nbt: &NbtCompound, game_time: i64) {
        let end_time = nbt.get_long("anger_end_time").or_else(|| {
            nbt.get_int("AngerTime")
                .map(|remaining| game_time + i64::from(remaining.max(0)))
        });
        self.set_end_time(end_time.unwrap_or(NO_ANGER_END_TIME));
        self.set_target_uuid(nbt.get_uuid("angry_at").or_else(|| nbt.get_uuid("AngryAt")));
    }
}

fn is_valid_player_target(world: &World, target: &Arc<dyn EntityBase>) -> bool {
    if target.get_entity().entity_type != &EntityType::PLAYER
        || world.level_info.load().difficulty == Difficulty::Peaceful
    {
        return false;
    }
    world
        .get_player_by_uuid(target.get_entity().entity_uuid)
        .is_some_and(|player| {
            !matches!(
                player.gamemode.load(),
                GameMode::Creative | GameMode::Spectator
            )
        })
}

pub trait NeutralMob: Mob {
    fn persistent_anger_state(&self) -> &PersistentAngerState;

    fn persistent_anger_time_range(&self) -> (i64, i64) {
        (DEFAULT_MIN_ANGER_TICKS, DEFAULT_MAX_ANGER_TICKS)
    }

    fn on_persistent_anger_end_time_changed(&self, _end_time: i64) {}

    fn set_persistent_anger_end_time(&self, end_time: i64) {
        self.persistent_anger_state().set_end_time(end_time);
        self.on_persistent_anger_end_time_changed(end_time);
    }

    fn start_persistent_anger_timer(&self, game_time: i64) {
        let (min, max) = self.persistent_anger_time_range();
        let remaining = self.get_random().random_range(min..=max);
        self.set_persistent_anger_end_time(game_time + remaining);
    }

    fn write_persistent_anger_nbt(&self, nbt: &mut NbtCompound) {
        self.persistent_anger_state().write_nbt(nbt);
    }

    fn read_persistent_anger_nbt<'a>(&'a self, nbt: &'a NbtCompound) -> EntityBaseFuture<'a, ()> {
        Box::pin(async move {
            let world = self.get_entity().world.load_full();
            let game_time = world.get_world_age().await;
            self.persistent_anger_state().read_nbt(nbt, game_time);
            self.on_persistent_anger_end_time_changed(self.persistent_anger_state().end_time());

            if let Some(target_uuid) = self.persistent_anger_state().target_uuid()
                && let Some(target) = world.get_entity_by_uuid(target_uuid)
                && target.get_living_entity().is_some()
            {
                self.set_mob_target(Some(target)).await;
            }
        })
    }

    fn stop_being_angry(&self) -> EntityBaseFuture<'_, ()> {
        Box::pin(async move {
            self.set_mob_target(None).await;
            if self.get_mob_entity().get_target().await.is_none() {
                self.get_mob_entity()
                    .living_entity
                    .last_attacker_id
                    .store(0, Ordering::Relaxed);
                self.persistent_anger_state().stop();
                self.on_persistent_anger_end_time_changed(NO_ANGER_END_TIME);
            }
        })
    }

    fn update_persistent_anger(
        &self,
        stay_angry_if_target_present: bool,
    ) -> EntityBaseFuture<'_, ()> {
        Box::pin(async move {
            let mob = self.get_mob_entity();
            let world = mob.living_entity.entity.world.load_full();
            let game_time = world.get_world_age().await;
            let current_target = mob.get_target().await;
            let target_uuid = self.persistent_anger_state().target_uuid();

            if let Some(target) = current_target.as_ref()
                && !target.get_entity().is_alive()
                && target_uuid == Some(target.get_entity().entity_uuid)
                && target.get_mob().is_some()
            {
                self.stop_being_angry().await;
                return;
            }

            if let Some(target) = current_target
                .as_ref()
                .filter(|target| target.get_entity().is_alive())
            {
                let uuid = target.get_entity().entity_uuid;
                let new_target = target_uuid != Some(uuid);
                if new_target {
                    self.persistent_anger_state().set_target_uuid(Some(uuid));
                }
                if new_target || stay_angry_if_target_present {
                    self.start_persistent_anger_timer(game_time);
                }
            }

            let current_target = mob.get_target().await;
            if self.persistent_anger_state().target_uuid().is_some()
                && !self.persistent_anger_state().is_angry(game_time)
                && (current_target.is_none()
                    || !current_target
                        .as_ref()
                        .is_some_and(|target| is_valid_player_target(&world, target))
                    || !stay_angry_if_target_present)
            {
                self.stop_being_angry().await;
                return;
            }

            if let Some(target_uuid) = self.persistent_anger_state().target_uuid()
                && let Some(target) = world.get_entity_by_uuid(target_uuid)
                && target.get_entity().entity_type == &EntityType::PLAYER
                && !is_valid_player_target(&world, &target)
            {
                self.stop_being_angry().await;
            }
        })
    }

    fn is_angry_at<'a>(&'a self, target: &'a Arc<dyn EntityBase>) -> EntityBaseFuture<'a, bool> {
        Box::pin(async move {
            if target.get_living_entity().is_none() || !target.get_entity().is_alive() {
                return false;
            }
            let world = self.get_entity().world.load_full();
            let game_time = world.get_world_age().await;
            if is_valid_player_target(&world, target)
                && world.level_info.load().game_rules.universal_anger
                && self.persistent_anger_state().is_angry(game_time)
                && self.persistent_anger_state().target_uuid().is_none()
            {
                return true;
            }
            self.persistent_anger_state().target_uuid() == Some(target.get_entity().entity_uuid)
        })
    }

    fn player_died(&self, player_uuid: Uuid) -> EntityBaseFuture<'_, ()> {
        Box::pin(async move {
            if self
                .get_entity()
                .world
                .load()
                .level_info
                .load()
                .game_rules
                .forgive_dead_players
                && self.persistent_anger_state().target_uuid() == Some(player_uuid)
            {
                self.stop_being_angry().await;
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use pumpkin_nbt::compound::NbtCompound;
    use uuid::Uuid;

    use super::{NO_ANGER_END_TIME, PersistentAngerState};

    #[test]
    fn anger_uses_absolute_world_time_and_expires_at_the_boundary() {
        let state = PersistentAngerState::default();
        state.set_end_time(140);
        assert!(state.is_angry(139));
        assert!(!state.is_angry(140));
        state.stop();
        assert_eq!(state.end_time(), NO_ANGER_END_TIME);
        assert_eq!(state.target_uuid(), None);
    }

    #[test]
    fn current_and_legacy_nbt_round_trip_to_the_same_state() {
        let target = Uuid::new_v4();
        let state = PersistentAngerState::default();
        state.set_end_time(999);
        state.set_target_uuid(Some(target));
        let mut current = NbtCompound::new();
        state.write_nbt(&mut current);

        let loaded = PersistentAngerState::default();
        loaded.read_nbt(&current, 100);
        assert_eq!(loaded.end_time(), 999);
        assert_eq!(loaded.target_uuid(), Some(target));

        let mut legacy = NbtCompound::new();
        legacy.put_int("AngerTime", 20);
        legacy.put_uuid("AngryAt", target);
        loaded.read_nbt(&legacy, 100);
        assert_eq!(loaded.end_time(), 120);
        assert_eq!(loaded.target_uuid(), Some(target));
    }
}
