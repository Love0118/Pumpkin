use std::sync::Arc;

use pumpkin_data::entity::EntityType;
use pumpkin_util::math::position::BlockPos;
use pumpkin_util::math::vector3::Vector3;
use uuid::Uuid;

use crate::entity::boss::ender_dragon::EnderDragonEntity;
use crate::entity::boss::wither::WitherEntity;
use crate::entity::decoration::leash_knot::LeashKnotEntity;
use crate::entity::decoration::{
    armor_stand::ArmorStandEntity,
    display::{BlockDisplayEntity, ItemDisplayEntity, TextDisplayEntity},
    end_crystal::EndCrystalEntity,
    item_frame::ItemFrameEntity,
    painting::PaintingEntity,
};
use crate::entity::experience_orb::ExperienceOrbEntity;
use crate::entity::falling::FallingEntity;
use crate::entity::interaction::InteractionEntity;
use crate::entity::item::ItemEntity;
use crate::entity::lightning::LightningBoltEntity;
use crate::entity::living::LivingEntity;
use crate::entity::marker::MarkerEntity;
use crate::entity::mob::bat::{self, BatEntity};
use crate::entity::mob::blaze::BlazeEntity;
use crate::entity::mob::breeze::BreezeEntity;
use crate::entity::mob::cave_spider::CaveSpiderEntity;
use crate::entity::mob::creaking::CreakingEntity;
use crate::entity::mob::creeper::CreeperEntity;
use crate::entity::mob::elder_guardian::ElderGuardianEntity;
use crate::entity::mob::enderman::EndermanEntity;
use crate::entity::mob::endermite::EndermiteEntity;
use crate::entity::mob::evoker::EvokerEntity;
use crate::entity::mob::ghast::GhastEntity;
use crate::entity::mob::giant::GiantEntity;
use crate::entity::mob::guardian::GuardianEntity;
use crate::entity::mob::hoglin::HoglinEntity;
use crate::entity::mob::illusioner::IllusionerEntity;
use crate::entity::mob::magma_cube::MagmaCubeEntity;
use crate::entity::mob::phantom::PhantomEntity;
use crate::entity::mob::piglin::PiglinEntity;
use crate::entity::mob::piglin_brute::PiglinBruteEntity;
use crate::entity::mob::pillager::PillagerEntity;
use crate::entity::mob::ravager::RavagerEntity;
use crate::entity::mob::shulker::ShulkerEntity;
use crate::entity::mob::silverfish::SilverfishEntity;
use crate::entity::mob::skeleton::{
    bogged::BoggedSkeletonEntity, parched::ParchedSkeletonEntity, skeleton::SkeletonEntity,
    stray::StraySkeletonEntity, wither::WitherSkeletonEntity,
};
use crate::entity::mob::slime::SlimeEntity;
use crate::entity::mob::spider::SpiderEntity;
use crate::entity::mob::vex::VexEntity;
use crate::entity::mob::vindicator::VindicatorEntity;
use crate::entity::mob::warden::WardenEntity;
use crate::entity::mob::witch::WitchEntity;
use crate::entity::mob::zoglin::ZoglinEntity;
use crate::entity::mob::zombie::zombie_villager::ZombieVillagerEntity;
use crate::entity::mob::zombie::{drowned::DrownedEntity, husk::HuskEntity, zombie::ZombieEntity};
use crate::entity::mob::zombified_piglin::ZombifiedPiglinEntity;
use crate::entity::passive::allay::AllayEntity;
use crate::entity::passive::armadillo::ArmadilloEntity;
use crate::entity::passive::axolotl::AxolotlEntity;
use crate::entity::passive::bee::BeeEntity;
use crate::entity::passive::camel::CamelEntity;
use crate::entity::passive::cat::CatEntity;
use crate::entity::passive::chicken::ChickenEntity;
use crate::entity::passive::cod::CodEntity;
use crate::entity::passive::copper_golem::CopperGolemEntity;
use crate::entity::passive::cow::CowEntity;
use crate::entity::passive::dolphin::DolphinEntity;
use crate::entity::passive::donkey::DonkeyEntity;
use crate::entity::passive::fox::FoxEntity;
use crate::entity::passive::frog::FrogEntity;
use crate::entity::passive::glow_squid::GlowSquidEntity;
use crate::entity::passive::goat::GoatEntity;
use crate::entity::passive::happy_ghast::HappyGhastEntity;
use crate::entity::passive::horse::HorseEntity;
use crate::entity::passive::iron_golem::IronGolemEntity;
use crate::entity::passive::llama::LlamaEntity;
use crate::entity::passive::mooshroom::MooshroomEntity;
use crate::entity::passive::mule::MuleEntity;
use crate::entity::passive::nautilus::NautilusEntity;
use crate::entity::passive::ocelot::OcelotEntity;
use crate::entity::passive::panda::PandaEntity;
use crate::entity::passive::parrot::ParrotEntity;
use crate::entity::passive::pig::PigEntity;
use crate::entity::passive::polar_bear::PolarBearEntity;
use crate::entity::passive::pufferfish::PufferfishEntity;
use crate::entity::passive::rabbit::RabbitEntity;
use crate::entity::passive::salmon::SalmonEntity;
use crate::entity::passive::sheep::SheepEntity;
use crate::entity::passive::skeleton_horse::SkeletonHorseEntity;
use crate::entity::passive::sniffer::SnifferEntity;
use crate::entity::passive::snow_golem::SnowGolemEntity;
use crate::entity::passive::squid::SquidEntity;
use crate::entity::passive::strider::StriderEntity;
use crate::entity::passive::tadpole::TadpoleEntity;
use crate::entity::passive::trader_llama::TraderLlamaEntity;
use crate::entity::passive::tropical_fish::TropicalFishEntity;
use crate::entity::passive::turtle::TurtleEntity;
use crate::entity::passive::villager::VillagerEntity;
use crate::entity::passive::wandering_trader::WanderingTraderEntity;
use crate::entity::passive::wolf::WolfEntity;
use crate::entity::passive::zombie_horse::ZombieHorseEntity;
use crate::entity::projectile::ThrownItemEntity;
use crate::entity::projectile::arrow::ArrowEntity;
use crate::entity::projectile::egg::EggEntity;
use crate::entity::projectile::ender_pearl::EnderPearlEntity;
use crate::entity::projectile::eye_of_ender::EyeOfEnder;
use crate::entity::projectile::fireball::FireballEntity;
use crate::entity::projectile::firework_rocket::FireworkRocketEntity;
use crate::entity::projectile::fishing_bobber::FishingBobberEntity;
use crate::entity::projectile::lingering_potion::LingeringPotionEntity;
use crate::entity::projectile::llama_spit::LlamaSpitEntity;
use crate::entity::projectile::shulker_bullet::ShulkerBulletEntity;
use crate::entity::projectile::small_fireball::SmallFireballEntity;
use crate::entity::projectile::snowball::SnowballEntity;
use crate::entity::projectile::splash_potion::SplashPotionEntity;
use crate::entity::projectile::trident::TridentEntity;
use crate::entity::projectile::wind_charge::{WIND_CHARGE_GRAVITY, WindChargeEntity};
use crate::entity::projectile::wither_skull::WitherSkullEntity;
use crate::entity::tnt::TNTEntity;
use crate::entity::vehicle::boat::BoatEntity;
use crate::entity::vehicle::minecart::MinecartEntity;
use crate::entity::{Entity, EntityBase, mob};
use crate::world::World;
use pumpkin_data::Block;
use std::sync::atomic::AtomicBool;
use tracing::error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FactoryPolicy {
    Explicit,
    GenericEntity,
    GenericLiving,
    ExternalPlayer,
}

#[must_use]
pub const fn factory_policy(entity_type: &EntityType) -> FactoryPolicy {
    let id = entity_type.id;
    if id == EntityType::CAMEL_HUSK.id
        || id == EntityType::SULFUR_CUBE.id
        || id == EntityType::ZOMBIE_NAUTILUS.id
    {
        FactoryPolicy::GenericLiving
    } else if id == EntityType::PLAYER.id {
        FactoryPolicy::ExternalPlayer
    } else if id == EntityType::DRAGON_FIREBALL.id
        || id == EntityType::EXPERIENCE_BOTTLE.id
        || id == EntityType::MANNEQUIN.id
        || id == EntityType::OMINOUS_ITEM_SPAWNER.id
    {
        FactoryPolicy::GenericEntity
    } else {
        FactoryPolicy::Explicit
    }
}

#[expect(clippy::too_many_lines)]
pub fn from_type(
    entity_type: &'static EntityType,
    position: Vector3<f64>,
    world: &Arc<World>,
    uuid: Uuid,
) -> Arc<dyn EntityBase> {
    let entity = Entity::from_uuid(uuid, world.clone(), position, entity_type);

    let mob: Arc<dyn EntityBase> = match entity_type.id {
        // Zombie
        id if id == EntityType::ZOMBIE.id => ZombieEntity::new(entity),
        id if id == EntityType::DROWNED.id => DrownedEntity::new(entity),
        id if id == EntityType::HUSK.id => HuskEntity::new(entity),
        id if id == EntityType::ZOMBIE_VILLAGER.id => ZombieVillagerEntity::new(entity),
        id if id == EntityType::ZOMBIFIED_PIGLIN.id => ZombifiedPiglinEntity::new(entity),

        // Skeleton
        id if id == EntityType::SKELETON.id => SkeletonEntity::new(entity),
        id if id == EntityType::BOGGED.id => BoggedSkeletonEntity::new(entity),
        id if id == EntityType::PARCHED.id => ParchedSkeletonEntity::new(entity),
        id if id == EntityType::WITHER_SKELETON.id => WitherSkeletonEntity::new(entity),
        id if id == EntityType::STRAY.id => StraySkeletonEntity::new(entity),

        id if id == EntityType::BAT.id => BatEntity::new(entity),
        id if id == EntityType::CREEPER.id => CreeperEntity::new(entity),
        id if id == EntityType::ENDERMAN.id => EndermanEntity::new(entity),

        id if id == EntityType::BLAZE.id => BlazeEntity::new(entity),
        id if id == EntityType::SPIDER.id => SpiderEntity::new(entity),
        id if id == EntityType::CAVE_SPIDER.id => CaveSpiderEntity::new(entity),
        id if id == EntityType::GHAST.id => GhastEntity::new(entity),
        id if id == EntityType::MAGMA_CUBE.id => MagmaCubeEntity::new(entity),
        id if id == EntityType::PHANTOM.id => PhantomEntity::new(entity),
        id if id == EntityType::WITCH.id => WitchEntity::new(entity),
        id if id == EntityType::PIGLIN.id => PiglinEntity::new(entity),
        id if id == EntityType::PIGLIN_BRUTE.id => PiglinBruteEntity::new(entity),
        id if id == EntityType::PILLAGER.id => PillagerEntity::new(entity),
        id if id == EntityType::VINDICATOR.id => VindicatorEntity::new(entity),
        id if id == EntityType::EVOKER.id => EvokerEntity::new(entity),
        id if id == EntityType::RAVAGER.id => RavagerEntity::new(entity),
        id if id == EntityType::GUARDIAN.id => GuardianEntity::new(entity),
        id if id == EntityType::ELDER_GUARDIAN.id => ElderGuardianEntity::new(entity),
        id if id == EntityType::WARDEN.id => WardenEntity::new(entity),
        id if id == EntityType::HOGLIN.id => HoglinEntity::new(entity),
        id if id == EntityType::ZOGLIN.id => ZoglinEntity::new(entity),
        id if id == EntityType::BREEZE.id => BreezeEntity::new(entity),
        id if id == EntityType::CREAKING.id => CreakingEntity::new(entity),
        id if id == EntityType::ILLUSIONER.id => IllusionerEntity::new(entity),
        id if id == EntityType::VEX.id => VexEntity::new(entity),
        id if id == EntityType::ENDERMITE.id => EndermiteEntity::new(entity),
        id if id == EntityType::GIANT.id => GiantEntity::new(entity),

        id if id == EntityType::CAT.id => CatEntity::new(entity),
        id if id == EntityType::CHICKEN.id => ChickenEntity::new(entity),
        id if id == EntityType::COW.id => CowEntity::new(entity),
        id if id == EntityType::PIG.id => PigEntity::new(entity),
        id if id == EntityType::SHEEP.id => SheepEntity::new(entity),
        id if id == EntityType::WOLF.id => WolfEntity::new(entity),
        id if id == EntityType::FOX.id => FoxEntity::new(entity),
        id if id == EntityType::RABBIT.id => RabbitEntity::new(entity),
        id if id == EntityType::TURTLE.id => TurtleEntity::new(entity),
        id if id == EntityType::VILLAGER.id => VillagerEntity::new(entity),
        id if id == EntityType::SQUID.id => SquidEntity::new(entity),
        id if id == EntityType::HORSE.id => HorseEntity::new(entity),
        id if id == EntityType::DONKEY.id => DonkeyEntity::new(entity),
        id if id == EntityType::MULE.id => MuleEntity::new(entity),
        id if id == EntityType::ZOMBIE_HORSE.id => ZombieHorseEntity::new(entity),
        id if id == EntityType::SKELETON_HORSE.id => SkeletonHorseEntity::new(entity),
        id if id == EntityType::LLAMA.id => LlamaEntity::new(entity),
        id if id == EntityType::TRADER_LLAMA.id => TraderLlamaEntity::new(entity),
        id if id == EntityType::WANDERING_TRADER.id => WanderingTraderEntity::new(entity),
        id if id == EntityType::ALLAY.id => AllayEntity::new(entity),
        id if id == EntityType::ARMADILLO.id => ArmadilloEntity::new(entity),
        id if id == EntityType::AXOLOTL.id => AxolotlEntity::new(entity),
        id if id == EntityType::BEE.id => BeeEntity::new(entity),
        id if id == EntityType::CAMEL.id => CamelEntity::new(entity),
        id if id == EntityType::COPPER_GOLEM.id => CopperGolemEntity::new(entity),
        id if id == EntityType::FROG.id => FrogEntity::new(entity),
        id if id == EntityType::GOAT.id => GoatEntity::new(entity),
        id if id == EntityType::HAPPY_GHAST.id => HappyGhastEntity::new(entity),
        id if id == EntityType::MOOSHROOM.id => MooshroomEntity::new(entity),
        id if id == EntityType::OCELOT.id => OcelotEntity::new(entity),
        id if id == EntityType::PANDA.id => PandaEntity::new(entity),
        id if id == EntityType::PARROT.id => ParrotEntity::new(entity),
        id if id == EntityType::POLAR_BEAR.id => PolarBearEntity::new(entity),
        id if id == EntityType::SNIFFER.id => SnifferEntity::new(entity),
        id if id == EntityType::STRIDER.id => StriderEntity::new(entity),
        id if id == EntityType::GLOW_SQUID.id => GlowSquidEntity::new(entity),
        id if id == EntityType::COD.id => CodEntity::new(entity),
        id if id == EntityType::SALMON.id => SalmonEntity::new(entity),
        id if id == EntityType::PUFFERFISH.id => PufferfishEntity::new(entity),
        id if id == EntityType::TROPICAL_FISH.id => TropicalFishEntity::new(entity),
        id if id == EntityType::TADPOLE.id => TadpoleEntity::new(entity),
        id if id == EntityType::DOLPHIN.id => DolphinEntity::new(entity),
        id if id == EntityType::NAUTILUS.id => NautilusEntity::new(entity),

        id if id == EntityType::SNOW_GOLEM.id => SnowGolemEntity::new(entity),
        id if id == EntityType::IRON_GOLEM.id => IronGolemEntity::new(entity),

        id if id == EntityType::WITHER.id => WitherEntity::new(entity),
        id if id == EntityType::ENDER_DRAGON.id => EnderDragonEntity::new(entity),

        id if id == EntityType::AREA_EFFECT_CLOUD.id => {
            crate::entity::area_effect_cloud::AreaEffectCloudEntity::new(entity)
        }
        id if id == EntityType::ARMOR_STAND.id => Arc::new(ArmorStandEntity::new(entity)),
        id if id == EntityType::BLOCK_DISPLAY.id => BlockDisplayEntity::new(entity),
        id if id == EntityType::ITEM_DISPLAY.id => ItemDisplayEntity::new(entity),
        id if id == EntityType::TEXT_DISPLAY.id => TextDisplayEntity::new(entity),
        id if id == EntityType::PAINTING.id => Arc::new(PaintingEntity::new(entity)),
        id if id == EntityType::ITEM_FRAME.id || id == EntityType::GLOW_ITEM_FRAME.id => {
            Arc::new(ItemFrameEntity::new(entity))
        }
        id if id == EntityType::EVOKER_FANGS.id => Arc::new(
            crate::entity::projectile::evoker_fangs::EvokerFangsEntity::new(entity, 0, 0.0, None),
        ),
        id if id == EntityType::END_CRYSTAL.id => Arc::new(EndCrystalEntity::new(entity)),
        id if id == EntityType::ENDER_PEARL.id => Arc::new(EnderPearlEntity::new(entity)),
        id if id == EntityType::SNOWBALL.id => Arc::new(SnowballEntity::new(entity)),
        id if id == EntityType::EGG.id => Arc::new(EggEntity::new(entity)),
        id if id == EntityType::SILVERFISH.id => SilverfishEntity::new(entity),
        id if id == EntityType::SLIME.id => SlimeEntity::new(entity),
        id if id == EntityType::SHULKER.id => ShulkerEntity::new(entity),
        id if id == EntityType::SHULKER_BULLET.id => {
            // Shulker bullets are normally spawned by ShulkerEntity directly;
            // when loaded from the world we create a no-target bullet at the given position.
            Arc::new(ShulkerBulletEntity::orphan(entity))
        }
        id if id == EntityType::FALLING_BLOCK.id => {
            Arc::new(FallingEntity::new(entity, Block::SAND.default_state.id))
        }
        id if id == EntityType::EXPERIENCE_ORB.id => Arc::new(ExperienceOrbEntity::new(entity, 1)),
        id if id == EntityType::TNT.id => Arc::new(TNTEntity::new(entity, 4.0, 80)),
        id if id == EntityType::ITEM.id => Arc::new(ItemEntity::new_for_restore(entity)),
        id if id == EntityType::ARROW.id => Arc::new(ArrowEntity::new(entity, None)),
        id if id == EntityType::SPECTRAL_ARROW.id => Arc::new(ArrowEntity::new(entity, None)),
        id if id == EntityType::TRIDENT.id => Arc::new(TridentEntity::new(entity, None)),
        id if id == EntityType::MINECART.id
            || id == EntityType::CHEST_MINECART.id
            || id == EntityType::FURNACE_MINECART.id
            || id == EntityType::TNT_MINECART.id
            || id == EntityType::HOPPER_MINECART.id
            || id == EntityType::COMMAND_BLOCK_MINECART.id
            || id == EntityType::SPAWNER_MINECART.id =>
        {
            Arc::new(MinecartEntity::new(entity))
        }
        id if id == EntityType::FIREBALL.id => Arc::new(FireballEntity::new(entity)),
        id if id == EntityType::SMALL_FIREBALL.id => Arc::new(SmallFireballEntity::new(entity)),
        id if id == EntityType::WITHER_SKULL.id => Arc::new(WitherSkullEntity::new(entity)),
        id if id == EntityType::WIND_CHARGE.id => {
            let thrown = ThrownItemEntity {
                entity,
                owner_id: None,
                collides_with_projectiles: false,
                has_hit: AtomicBool::new(false),
                gravity: WIND_CHARGE_GRAVITY,
            };
            Arc::new(WindChargeEntity::new_normal(thrown))
        }
        id if id == EntityType::BREEZE_WIND_CHARGE.id => {
            let thrown = ThrownItemEntity {
                entity,
                owner_id: None,
                collides_with_projectiles: false,
                has_hit: AtomicBool::new(false),
                gravity: WIND_CHARGE_GRAVITY,
            };
            Arc::new(WindChargeEntity::new_breeze(thrown))
        }
        id if id == EntityType::LIGHTNING_BOLT.id => Arc::new(LightningBoltEntity::new(entity)),
        id if id == EntityType::MARKER.id => MarkerEntity::new(entity),
        id if id == EntityType::INTERACTION.id => InteractionEntity::new(entity),
        id if id == EntityType::FIREWORK_ROCKET.id => Arc::new(FireworkRocketEntity::new(entity)),
        id if id == EntityType::SPLASH_POTION.id => Arc::new(SplashPotionEntity::new(entity)),
        id if id == EntityType::LINGERING_POTION.id => Arc::new(LingeringPotionEntity::new(entity)),
        id if id == EntityType::LLAMA_SPIT.id => Arc::new(LlamaSpitEntity::new(entity)),
        id if id == EntityType::EYE_OF_ENDER.id => Arc::new(EyeOfEnder::new(entity)),
        id if id == EntityType::FISHING_BOBBER.id => Arc::new(FishingBobberEntity::orphan(entity)),
        id if id == EntityType::LEASH_KNOT.id => {
            Arc::new(LeashKnotEntity::new(entity, BlockPos::floored_v(position)))
        }
        id if id == EntityType::ACACIA_BOAT.id
            || id == EntityType::ACACIA_CHEST_BOAT.id
            || id == EntityType::BIRCH_BOAT.id
            || id == EntityType::BIRCH_CHEST_BOAT.id
            || id == EntityType::DARK_OAK_BOAT.id
            || id == EntityType::DARK_OAK_CHEST_BOAT.id
            || id == EntityType::JUNGLE_BOAT.id
            || id == EntityType::JUNGLE_CHEST_BOAT.id
            || id == EntityType::MANGROVE_BOAT.id
            || id == EntityType::MANGROVE_CHEST_BOAT.id
            || id == EntityType::OAK_BOAT.id
            || id == EntityType::OAK_CHEST_BOAT.id
            || id == EntityType::PALE_OAK_BOAT.id
            || id == EntityType::PALE_OAK_CHEST_BOAT.id
            || id == EntityType::SPRUCE_BOAT.id
            || id == EntityType::SPRUCE_CHEST_BOAT.id
            || id == EntityType::BAMBOO_RAFT.id
            || id == EntityType::BAMBOO_CHEST_RAFT.id
            || id == EntityType::CHERRY_BOAT.id
            || id == EntityType::CHERRY_CHEST_BOAT.id =>
        {
            Arc::new(BoatEntity::new(entity))
        }
        _ => match factory_policy(entity_type) {
            FactoryPolicy::GenericLiving => Arc::new(LivingEntity::new(entity)),
            FactoryPolicy::GenericEntity | FactoryPolicy::ExternalPlayer => Arc::new(entity),
            FactoryPolicy::Explicit => {
                error!(
                    "Entity type {} is marked explicit but has no runtime factory branch",
                    entity_type.resource_name
                );
                Arc::new(entity)
            }
        },
    };

    mob
}

pub fn check_spawn_rules(
    entity_type: &'static EntityType,
    world: &World,
    pos: &BlockPos,
    is_thundering: bool,
) -> bool {
    let id = entity_type.id;

    if id == EntityType::BOGGED.id
        || id == EntityType::CAVE_SPIDER.id
        || id == EntityType::CREEPER.id
        || id == EntityType::ENDERMAN.id
        || id == EntityType::GIANT.id
        || id == EntityType::RAVAGER.id
        || id == EntityType::SKELETON.id
        || id == EntityType::SPIDER.id
        || id == EntityType::WITCH.id
        || id == EntityType::WITHER.id
        || id == EntityType::WITHER_SKELETON.id
        || id == EntityType::ZOMBIE.id
        || id == EntityType::ZOMBIE_HORSE.id
        || id == EntityType::ZOMBIE_VILLAGER.id
        || id == EntityType::CREAKING.id
        || id == EntityType::EVOKER.id
        || id == EntityType::ILLUSIONER.id
        || id == EntityType::VEX.id
        || id == EntityType::VINDICATOR.id
        || id == EntityType::WARDEN.id
    {
        return mob::MobEntity::check_monster_spawn_rules(world, pos, is_thundering);
    }
    if id == EntityType::BAT.id {
        return bat::BatEntity::check_bat_spawn_rules(world, pos);
    }
    if id == EntityType::SLIME.id {
        return SlimeEntity::check_slime_spawn_rules(world, pos);
    }

    // TODO
    true
}

#[cfg(test)]
mod factory_ledger_tests {
    use std::collections::HashSet;

    use pumpkin_data::entity::{EntityType, entity_from_egg};

    use super::{FactoryPolicy, factory_policy};

    #[test]
    fn entity_type_registry_has_26_2_shape_and_unique_keys() {
        assert_eq!(EntityType::ALL.len(), 158);
        let ids = EntityType::ALL
            .iter()
            .map(|entity_type| entity_type.id)
            .collect::<HashSet<_>>();
        let names = EntityType::ALL
            .iter()
            .map(|entity_type| entity_type.resource_name)
            .collect::<HashSet<_>>();
        assert_eq!(ids.len(), EntityType::ALL.len());
        assert_eq!(names.len(), EntityType::ALL.len());
    }

    #[test]
    fn spawn_egg_registry_has_88_unique_entity_targets() {
        let targets = (u16::MIN..=u16::MAX)
            .filter_map(entity_from_egg)
            .collect::<Vec<_>>();
        assert_eq!(targets.len(), 88);
        assert_eq!(
            targets
                .iter()
                .map(|entity_type| entity_type.id)
                .collect::<HashSet<_>>()
                .len(),
            targets.len()
        );
        assert!(targets.iter().all(|target| {
            EntityType::ALL
                .iter()
                .any(|candidate| candidate.id == target.id)
        }));
    }

    #[test]
    fn incomplete_factory_policies_are_explicit_and_stable() {
        let incomplete = EntityType::ALL
            .iter()
            .filter_map(|entity_type| {
                let policy = factory_policy(entity_type);
                (policy != FactoryPolicy::Explicit).then_some((entity_type.resource_name, policy))
            })
            .collect::<Vec<_>>();

        assert_eq!(
            incomplete,
            vec![
                ("camel_husk", FactoryPolicy::GenericLiving),
                ("dragon_fireball", FactoryPolicy::GenericEntity),
                ("experience_bottle", FactoryPolicy::GenericEntity),
                ("mannequin", FactoryPolicy::GenericEntity),
                ("ominous_item_spawner", FactoryPolicy::GenericEntity),
                ("player", FactoryPolicy::ExternalPlayer),
                ("sulfur_cube", FactoryPolicy::GenericLiving),
                ("zombie_nautilus", FactoryPolicy::GenericLiving),
            ]
        );
    }

    #[test]
    fn no_save_no_summon_special_types_are_declared() {
        for (entity_type, summonable) in [
            (&EntityType::PLAYER, false),
            (&EntityType::FISHING_BOBBER, false),
            (&EntityType::LEASH_KNOT, true),
            (&EntityType::LIGHTNING_BOLT, true),
        ] {
            assert!(
                !entity_type.saveable,
                "{} must not save",
                entity_type.resource_name
            );
            assert_eq!(entity_type.summonable, summonable);
        }
    }
}
