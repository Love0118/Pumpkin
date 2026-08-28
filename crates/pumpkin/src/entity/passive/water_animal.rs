use std::sync::Arc;
use std::sync::atomic::Ordering;

use pumpkin_data::damage::DamageType;
use rand::RngExt;

use crate::entity::ai::pathfinder::node::PathType;
use crate::entity::breath::MAX_AIR;
use crate::entity::mob::Mob;
use crate::entity::{EntityBase, EntityBaseFuture};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct WaterAirTransition {
    air: i32,
    drown: bool,
}

const fn water_air_transition(
    alive: bool,
    in_water: bool,
    previous_air: i32,
) -> WaterAirTransition {
    if alive && !in_water {
        let air = previous_air.saturating_sub(1);
        if air <= -20 {
            WaterAirTransition {
                air: 0,
                drown: true,
            }
        } else {
            WaterAirTransition { air, drown: false }
        }
    } else {
        WaterAirTransition {
            air: MAX_AIR,
            drown: false,
        }
    }
}

/// Shared Vanilla policy for `WaterAnimal` and `AgeableWaterCreature` descendants.
pub trait WaterAnimal: Mob {
    /// Applies the family-wide water malus during entity initialization.
    fn initialize_water_animal(&self) {
        self.get_mob_entity()
            .navigator
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .set_pathfinding_malus(PathType::Water, 0.0);
    }

    /// Runs the common air-supply transition after the entity-specific AI step.
    fn water_animal_tick<'a>(
        &'a self,
        caller: &'a Arc<dyn EntityBase>,
    ) -> EntityBaseFuture<'a, ()> {
        Box::pin(async move {
            let living = &self.get_mob_entity().living_entity;
            let entity = &living.entity;
            let previous_air = living.breath_manager.air_supply.load(Ordering::Relaxed);
            let transition = water_air_transition(
                entity.is_alive(),
                entity.touching_water.load(Ordering::Relaxed),
                previous_air,
            );

            living.breath_manager.set_air_supply(entity, transition.air);
            if transition.drown {
                living.damage(caller.as_ref(), 2.0, DamageType::DROWN).await;
            }
        })
    }

    fn water_animal_experience_reward(&self) -> u32 {
        rand::rng().random_range(1..=3)
    }

    fn water_animal_is_pushed_by_fluids(&self) -> bool {
        false
    }

    fn water_animal_can_be_leashed(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::{WaterAirTransition, water_air_transition};
    use crate::entity::breath::MAX_AIR;

    #[test]
    fn dry_water_animal_loses_one_air_per_tick() {
        assert_eq!(
            water_air_transition(true, false, MAX_AIR),
            WaterAirTransition {
                air: MAX_AIR - 1,
                drown: false,
            }
        );
    }

    #[test]
    fn water_animal_drowns_at_vanilla_negative_twenty_threshold() {
        assert_eq!(
            water_air_transition(true, false, -19),
            WaterAirTransition {
                air: 0,
                drown: true,
            }
        );
    }

    #[test]
    fn water_or_inactive_state_restores_full_air() {
        for (alive, in_water) in [(true, true), (false, false), (false, true)] {
            assert_eq!(
                water_air_transition(alive, in_water, 17),
                WaterAirTransition {
                    air: MAX_AIR,
                    drown: false,
                }
            );
        }
    }
}
