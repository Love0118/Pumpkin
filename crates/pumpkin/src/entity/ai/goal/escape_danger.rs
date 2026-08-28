use std::sync::atomic::Ordering::Relaxed;

use super::{Controls, Goal, GoalFuture};
use crate::entity::{ai::pathfinder::NavigatorGoal, mob::Mob};
use pumpkin_data::tag::{self, Taggable};
use pumpkin_util::math::position::BlockPos;
use pumpkin_util::math::vector3::Vector3;
use rand::RngExt;

const RANGE: i32 = 5;
const VERTICAL_RANGE: i32 = 4;
const LAST_DAMAGE_SOURCE_TICKS: i32 = 40;
const TARGET_ATTEMPTS: usize = 10;

pub struct EscapeDangerGoal {
    speed: f64,
    goal_control: Controls,
    target: Option<Vector3<f64>>,
    panic_causes: &'static tag::Tag,
}

impl EscapeDangerGoal {
    #[must_use]
    pub fn new(speed: f64) -> Box<Self> {
        Box::new(Self {
            speed,
            goal_control: Controls::MOVE,
            target: None,
            panic_causes: &tag::DamageType::MINECRAFT_PANIC_CAUSES,
        })
    }

    #[must_use]
    pub fn new_with_tag(speed: f64, panic_causes: &'static tag::Tag) -> Box<Self> {
        Box::new(Self {
            speed,
            goal_control: Controls::MOVE,
            target: None,
            panic_causes,
        })
    }

    fn is_in_danger(&self, mob: &dyn Mob) -> bool {
        let living = &mob.get_mob_entity().living_entity;
        let age = living.entity.age.load(Relaxed);
        let elapsed = age.saturating_sub(living.last_damage_time.load(Relaxed));
        should_panic_from_damage(living.last_damage_type.load(), elapsed, self.panic_causes)
    }

    fn look_for_water(mob: &dyn Mob) -> Option<Vector3<f64>> {
        let entity = &mob.get_mob_entity().living_entity.entity;
        let world = entity.world.load();
        let origin = entity.block_pos.load();
        if world.get_block_state(&origin).is_solid() {
            return None;
        }

        let mut closest = None;
        let mut closest_distance = i32::MAX;
        for y in -1..=1 {
            for x in -RANGE..=RANGE {
                for z in -RANGE..=RANGE {
                    let candidate = BlockPos::new(origin.0.x + x, origin.0.y + y, origin.0.z + z);
                    if world
                        .get_fluid(&candidate)
                        .has_tag(&tag::Fluid::MINECRAFT_WATER)
                    {
                        let distance = x * x + y * y + z * z;
                        if distance < closest_distance {
                            closest_distance = distance;
                            closest = Some(candidate.0.to_f64());
                        }
                    }
                }
            }
        }
        closest
    }

    fn find_escape_target(mob: &dyn Mob) -> Option<Vector3<f64>> {
        let entity = &mob.get_mob_entity().living_entity.entity;
        let pos = entity.pos.load();
        let world = entity.world.load();
        let mut rng = mob.get_random();

        for _ in 0..TARGET_ATTEMPTS {
            let dx = rng.random_range(-RANGE..=RANGE);
            let dy = rng.random_range(-VERTICAL_RANGE..=VERTICAL_RANGE);
            let dz = rng.random_range(-RANGE..=RANGE);
            if dx == 0 && dy == 0 && dz == 0 {
                continue;
            }
            let candidate = BlockPos::floored(
                pos.x + f64::from(dx),
                pos.y + f64::from(dy),
                pos.z + f64::from(dz),
            );
            let state = world.get_block_state(&candidate);
            let below = world.get_block_state(&candidate.down());
            let is_water = world
                .get_fluid(&candidate)
                .has_tag(&tag::Fluid::MINECRAFT_WATER);
            if mob
                .get_mob_entity()
                .is_in_position_target_range_pos(&candidate)
                && !state.is_solid()
                && (below.is_solid() || is_water)
            {
                return Some(candidate.0.to_f64());
            }
        }

        None
    }
}

impl Goal for EscapeDangerGoal {
    fn can_start<'a>(&'a mut self, mob: &'a dyn Mob) -> GoalFuture<'a, bool> {
        Box::pin(async move {
            if !self.is_in_danger(mob) {
                return false;
            }
            let entity = &mob.get_mob_entity().living_entity.entity;
            self.target = if entity.fire_ticks.load(Relaxed) > 0 {
                Self::look_for_water(mob).or_else(|| Self::find_escape_target(mob))
            } else {
                Self::find_escape_target(mob)
            };
            self.target.is_some()
        })
    }

    fn should_continue<'a>(&'a self, mob: &'a dyn Mob) -> GoalFuture<'a, bool> {
        Box::pin(async move {
            let navigator = mob
                .get_mob_entity()
                .navigator
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            !navigator.is_idle()
        })
    }

    fn start<'a>(&'a mut self, mob: &'a dyn Mob) -> GoalFuture<'a, ()> {
        Box::pin(async move {
            if let Some(target) = self.target {
                let pos = mob.get_mob_entity().living_entity.entity.pos.load();
                let mut navigator = mob
                    .get_mob_entity()
                    .navigator
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                navigator.set_progress(NavigatorGoal::new(pos, target, self.speed));
            }
        })
    }

    fn stop<'a>(&'a mut self, _mob: &'a dyn Mob) -> GoalFuture<'a, ()> {
        Box::pin(async move {
            self.target = None;
        })
    }

    fn controls(&self) -> Controls {
        self.goal_control
    }
}

fn should_panic_from_damage(
    damage_type: Option<pumpkin_data::damage::DamageType>,
    elapsed_ticks: i32,
    panic_causes: &'static tag::Tag,
) -> bool {
    (0..=LAST_DAMAGE_SOURCE_TICKS).contains(&elapsed_ticks)
        && damage_type.is_some_and(|damage_type| damage_type.has_tag(panic_causes))
}

#[cfg(test)]
mod tests {
    use pumpkin_data::damage::DamageType;
    use pumpkin_data::tag;

    use super::{LAST_DAMAGE_SOURCE_TICKS, should_panic_from_damage};

    #[test]
    fn pig_panic_speed_is_preserved_by_the_common_goal() {
        let goal = super::EscapeDangerGoal::new(1.25);
        assert_eq!(goal.speed, 1.25);
    }

    #[test]
    fn default_panic_requires_a_recent_tagged_damage_source() {
        let causes = &tag::DamageType::MINECRAFT_PANIC_CAUSES;
        assert!(should_panic_from_damage(
            Some(DamageType::PLAYER_ATTACK),
            0,
            causes
        ));
        assert!(should_panic_from_damage(
            Some(DamageType::PLAYER_ATTACK),
            LAST_DAMAGE_SOURCE_TICKS,
            causes
        ));
        assert!(!should_panic_from_damage(
            Some(DamageType::PLAYER_ATTACK),
            LAST_DAMAGE_SOURCE_TICKS + 1,
            causes
        ));
        assert!(!should_panic_from_damage(Some(DamageType::FALL), 0, causes));
        assert!(!should_panic_from_damage(None, 0, causes));
    }

    #[test]
    fn wolf_environmental_panic_excludes_player_attacks() {
        let causes = &tag::DamageType::MINECRAFT_PANIC_ENVIRONMENTAL_CAUSES;
        assert!(!should_panic_from_damage(
            Some(DamageType::PLAYER_ATTACK),
            0,
            causes
        ));
        assert!(should_panic_from_damage(
            Some(DamageType::ON_FIRE),
            0,
            causes
        ));
    }
}
