use std::sync::{
    Arc, Weak,
    atomic::{AtomicBool, AtomicU8, Ordering},
};

use pumpkin_data::damage::DamageType;
use pumpkin_data::entity::EntityType;
use pumpkin_data::item::Item;
use pumpkin_data::item_stack::ItemStack;
use pumpkin_data::sound::Sound;
use pumpkin_data::tag::{self, Taggable};
use pumpkin_nbt::compound::NbtCompound;
use pumpkin_protocol::bedrock::client::set_actor_data::{
    EntityMetadata, MetadataValue, entity_data_flag, entity_data_key,
};
use pumpkin_protocol::codec::var_int::VarInt;
use pumpkin_protocol::java::client::play::Metadata;
use pumpkin_util::version::JavaMinecraftVersion;

use crate::entity::{
    Entity, EntityBase, EntityBaseFuture, NbtFuture,
    ageable::AgeableMob,
    ai::goal::{
        active_target::ActiveTargetGoal, avoid_entity::AvoidEntityGoal, beg::BegGoal,
        breed::BreedGoal, escape_danger::EscapeDangerGoal, follow_owner::FollowOwnerGoal,
        follow_parent::FollowParentGoal, look_around::RandomLookAroundGoal,
        look_at_entity::LookAtEntityGoal, melee_attack::MeleeAttackGoal,
        owner_hurt_by_target::OwnerHurtByTargetGoal, owner_hurt_target::OwnerHurtTargetGoal,
        persistent_anger_target::PersistentAngerTargetGoal, revenge::RevengeGoal, swim::SwimGoal,
        wander_around::WanderAroundGoal,
    },
    ai::neutral::{NeutralMob, PersistentAngerState},
    mob::{Mob, MobEntity},
    passive::{
        animal::Animal,
        tamable::{TamableAnimal, TamableData},
    },
};

pub struct WolfEntity {
    pub mob_entity: MobEntity,
    pub variant: AtomicU8,
    pub collar_color: AtomicU8,
    pub tamable_data: TamableData,
    pub ageable_data: crate::entity::ageable::AgeableData,
    pub persistent_anger: PersistentAngerState,
    pub interested: AtomicBool,
}

impl WolfEntity {
    pub fn new(entity: Entity) -> Arc<Self> {
        let mob_entity = MobEntity::new(entity);
        let wolf = Self {
            mob_entity,
            variant: AtomicU8::new(3),       // Default to pale
            collar_color: AtomicU8::new(14), // Default to red
            tamable_data: TamableData::default(),
            ageable_data: crate::entity::ageable::AgeableData::default(),
            persistent_anger: PersistentAngerState::default(),
            interested: AtomicBool::new(false),
        };
        let mob_arc = Arc::new(wolf);
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

            // Goal selector (matching Vanilla registerGoals):
            // 1: SwimGoal (FloatGoal)
            goal_selector.add_goal(1, Box::new(SwimGoal::default()));
            // 1: EscapeDangerGoal (TamableAnimalPanicGoal)
            goal_selector.add_goal(
                1,
                EscapeDangerGoal::new_with_tag(
                    1.5,
                    &tag::DamageType::MINECRAFT_PANIC_ENVIRONMENTAL_CAUSES,
                ),
            );
            // 3: Avoid Llama
            goal_selector.add_goal(
                3,
                Box::new(AvoidEntityGoal::new(&EntityType::LLAMA, 24.0, 1.5, 1.5)),
            );
            // 5: MeleeAttackGoal
            goal_selector.add_goal(5, Box::new(MeleeAttackGoal::new(1.0, true)));
            // 6: FollowOwnerGoal
            goal_selector.add_goal(6, FollowOwnerGoal::new(1.0, 10.0, 2.0));
            // 7: BreedGoal
            goal_selector.add_goal(7, BreedGoal::new(1.0));
            // 8: FollowParentGoal & WanderAroundGoal
            goal_selector.add_goal(8, Box::new(FollowParentGoal::new(1.1)));
            goal_selector.add_goal(8, Box::new(WanderAroundGoal::new(1.0)));
            // 9: BegGoal
            goal_selector.add_goal(9, BegGoal::new(8.0));
            // 10: LookAtPlayer & RandomLookAround
            goal_selector.add_goal(
                10,
                LookAtEntityGoal::with_default(mob_weak, &EntityType::PLAYER, 8.0),
            );
            goal_selector.add_goal(10, Box::new(RandomLookAroundGoal::default()));
        };

        {
            let mut target_selector = mob_arc
                .mob_entity
                .target_selector
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);

            // Target selector (matching Vanilla registerGoals):
            // 1: OwnerHurtByTargetGoal
            target_selector.add_goal(1, OwnerHurtByTargetGoal::new());
            // 2: OwnerHurtTargetGoal
            target_selector.add_goal(2, OwnerHurtTargetGoal::new());
            // 3: HurtByTargetGoal (RevengeGoal)
            target_selector.add_goal(3, Box::new(RevengeGoal::new(true).set_alert_others()));
            target_selector.add_goal(4, PersistentAngerTargetGoal::new());
            // 5: NonTameRandomTarget (Sheep, Rabbit, Fox)
            target_selector.add_goal(
                5,
                ActiveTargetGoal::with_default(&mob_arc.mob_entity, &EntityType::SHEEP, false),
            );
            target_selector.add_goal(
                5,
                ActiveTargetGoal::with_default(&mob_arc.mob_entity, &EntityType::RABBIT, false),
            );
            target_selector.add_goal(
                5,
                ActiveTargetGoal::with_default(&mob_arc.mob_entity, &EntityType::FOX, false),
            );
            // 7: NearestAttackableTarget (Skeleton)
            target_selector.add_goal(
                7,
                ActiveTargetGoal::with_default(&mob_arc.mob_entity, &EntityType::SKELETON, false),
            );
        };

        mob_arc
    }

    pub fn get_tame_flags(&self) -> u8 {
        tame_flags(self.is_in_sitting_pose(), self.is_tame())
    }

    pub fn set_interested(&self, interested: bool) {
        self.interested.store(interested, Ordering::Relaxed);
        self.get_entity().send_meta_data(
            &[Metadata::new(
                pumpkin_data::tracked_data::wolf::INTERESTED,
                interested,
            )],
            None,
        );
    }

    #[must_use]
    pub fn is_interested(&self) -> bool {
        self.interested.load(Ordering::Relaxed)
    }

    pub fn set_variant(&self, variant: u8) {
        self.variant.store(variant, Ordering::Relaxed);
        self.get_entity().send_meta_data(
            &[Metadata::new(
                pumpkin_data::tracked_data::wolf::WOLF_VARIANT_ID,
                VarInt(variant as i32),
            )],
            None,
        );
    }

    pub fn set_collar_color(&self, color: u8) {
        self.collar_color.store(color, Ordering::Relaxed);
        self.get_entity().send_meta_data(
            &[Metadata::new(
                pumpkin_data::tracked_data::wolf::COLLAR_COLOR,
                VarInt(color as i32),
            )],
            None,
        );
    }

    fn bedrock_state_metadata(&self, angry: bool) -> EntityMetadata {
        let mut metadata = EntityMetadata::new();
        metadata.set_flag(
            entity_data_key::FLAGS,
            entity_data_flag::SITTING as u8,
            self.is_in_sitting_pose(),
        );
        metadata.set_flag(
            entity_data_key::FLAGS,
            entity_data_flag::TAMED as u8,
            self.is_tame(),
        );
        metadata.set_flag(entity_data_key::FLAGS, entity_data_flag::ANGRY as u8, angry);
        metadata.set_flag(
            entity_data_key::FLAGS,
            entity_data_flag::INTERESTED as u8,
            self.is_interested(),
        );
        metadata
    }

    #[must_use]
    pub fn get_tail_angle(&self) -> f32 {
        let entity_age = self.get_entity().age.load(Ordering::Relaxed) as i64;
        let angry = self.persistent_anger.end_time() > entity_age;
        tail_angle(
            angry,
            self.is_tame(),
            self.mob_entity.living_entity.health.load(),
            self.mob_entity.living_entity.get_max_health(),
        )
    }

    #[must_use]
    pub const fn hurt_sound() -> Sound {
        Sound::EntityWolfHurt
    }

    #[must_use]
    pub const fn death_sound() -> Sound {
        Sound::EntityWolfDeath
    }

    #[must_use]
    pub fn ambient_sound(&self, angry: bool) -> Sound {
        if angry {
            Sound::EntityWolfGrowl
        } else if self.is_tame() && self.mob_entity.living_entity.health.load() < 20.0 {
            Sound::EntityWolfWhine
        } else {
            Sound::EntityWolfAmbient
        }
    }

    #[must_use]
    pub const fn sound_volume() -> f32 {
        0.4
    }

    fn send_tameable_metadata(&self) {
        let mut bedrock = self.bedrock_state_metadata(false);
        bedrock.set_flag(
            entity_data_key::FLAGS,
            entity_data_flag::ANGRY as u8,
            self.persistent_anger.end_time() > self.get_entity().age.load(Ordering::Relaxed) as i64,
        );
        self.get_entity().send_meta_data(
            &[Metadata::new(
                pumpkin_data::tracked_data::wolf::TAMEABLE_FLAGS,
                self.get_tame_flags(),
            )],
            Some(&bedrock),
        );
    }
}

const fn tame_flags(sitting: bool, tame: bool) -> u8 {
    (if sitting { 0x01 } else { 0 }) | (if tame { 0x04 } else { 0 })
}

fn tail_angle(angry: bool, tame: bool, health: f32, max_health: f32) -> f32 {
    if angry {
        1.539_380_4
    } else if tame {
        let damage_ratio = if max_health > 0.0 {
            (max_health - health) / max_health
        } else {
            0.0
        };
        (0.55 - damage_ratio * 0.4) * std::f32::consts::PI
    } else {
        std::f32::consts::PI / 5.0
    }
}

impl NeutralMob for WolfEntity {
    fn persistent_anger_state(&self) -> &PersistentAngerState {
        &self.persistent_anger
    }

    fn on_persistent_anger_end_time_changed(&self, end_time: i64) {
        let bedrock = self.bedrock_state_metadata(
            end_time > self.get_entity().age.load(Ordering::Relaxed) as i64,
        );
        self.mob_entity.living_entity.entity.send_meta_data(
            &[Metadata::new(
                pumpkin_data::tracked_data::wolf::ANGER_END_TIME,
                end_time,
            )],
            Some(&bedrock),
        );
    }
}

impl AgeableMob for WolfEntity {
    fn get_ageable_data(&self) -> &crate::entity::ageable::AgeableData {
        &self.ageable_data
    }
}

impl Animal for WolfEntity {
    fn is_food(&self, item_stack: &ItemStack) -> bool {
        let item = item_stack.get_item();
        item.has_tag(&tag::Item::MINECRAFT_WOLF_FOOD) || item == &Item::BONE
    }
}

impl TamableAnimal for WolfEntity {
    fn get_tamable_data(&self) -> &TamableData {
        &self.tamable_data
    }

    fn set_tame(&self, tame: bool) {
        self.tamable_data.is_tame.store(tame, Ordering::Relaxed);
        self.send_tameable_metadata();
    }

    fn set_in_sitting_pose(&self, sitting: bool) {
        self.tamable_data
            .ordered_to_sit
            .store(sitting, Ordering::Relaxed);
        self.send_tameable_metadata();
    }
}

impl Mob for WolfEntity {
    fn as_ageable(&self) -> Option<&dyn AgeableMob> {
        Some(self)
    }

    fn as_animal(&self) -> Option<&dyn Animal> {
        Some(self)
    }

    fn as_tamable(&self) -> Option<&dyn TamableAnimal> {
        Some(self)
    }

    fn as_neutral(&self) -> Option<&dyn NeutralMob> {
        Some(self)
    }

    fn can_attack_with_owner(&self, target: &dyn EntityBase, owner: &dyn EntityBase) -> bool {
        let target_entity = target.get_entity();
        let target_type = target_entity.entity_type;
        if *target_type == EntityType::CREEPER
            || *target_type == EntityType::GHAST
            || *target_type == EntityType::ARMOR_STAND
        {
            return false;
        }

        if *target_type == EntityType::WOLF {
            if let Some(target_mob) = target.get_mob()
                && let Some(tamable) = target_mob.as_tamable()
                && tamable.is_tame()
                && let Some(target_owner) = tamable.get_owner()
                && let Some(owner_player) = owner.get_player()
                && target_owner == owner_player.gameprofile.id
            {
                return false;
            }
            return true;
        }

        if *target_type == EntityType::PLAYER {
            if let Some(owner_player) = owner.get_player()
                && let Some(target_player) = target.get_player()
            {
                if owner_player.gameprofile.id == target_player.gameprofile.id {
                    return false;
                }
                let world = target_player.world();
                if !world.level_info.load().game_rules.pvp {
                    return false;
                }
            }
            return true;
        }

        if let Some(target_mob) = target.get_mob()
            && let Some(tamable) = target_mob.as_tamable()
            && tamable.is_tame()
        {
            return false;
        }

        true
    }

    fn on_damage<'a>(
        &'a self,
        _damage_type: DamageType,
        _source: Option<&'a dyn EntityBase>,
    ) -> EntityBaseFuture<'a, ()> {
        Box::pin(async move {
            // Vanilla clears ordered sitting only after damage has passed all
            // invulnerability, cancellation, and cooldown checks.
            self.set_ordered_to_sit(false);
        })
    }

    fn mob_write_nbt<'a>(&'a self, nbt: &'a mut NbtCompound) -> NbtFuture<'a, ()> {
        Box::pin(async {
            let variant_str = match self.variant.load(Ordering::Relaxed) {
                0 => "minecraft:ashen",
                1 => "minecraft:black",
                2 => "minecraft:chestnut",
                4 => "minecraft:rusty",
                5 => "minecraft:snowy",
                6 => "minecraft:spotted",
                7 => "minecraft:striped",
                8 => "minecraft:woods",
                _ => "minecraft:pale",
            };
            nbt.put_string("variant", variant_str.to_string());
            nbt.put_byte(
                "CollarColor",
                self.collar_color.load(Ordering::Relaxed) as i8,
            );
            self.write_animal_nbt(nbt);
            self.write_tamable_nbt(nbt);
            self.write_persistent_anger_nbt(nbt);
        })
    }

    fn mob_read_nbt<'a>(&'a self, nbt: &'a NbtCompound) -> NbtFuture<'a, ()> {
        Box::pin(async {
            if let Some(variant_str) = nbt.get_string("variant") {
                let variant = match variant_str
                    .strip_prefix("minecraft:")
                    .unwrap_or(variant_str)
                {
                    "ashen" => 0,
                    "black" => 1,
                    "chestnut" => 2,
                    "rusty" => 4,
                    "snowy" => 5,
                    "spotted" => 6,
                    "striped" => 7,
                    "woods" => 8,
                    _ => 3,
                };
                self.set_variant(variant);
            }
            if let Some(collar) = nbt.get_byte("CollarColor") {
                self.set_collar_color(collar as u8);
            } else if let Some(collar_int) = nbt.get_int("CollarColor") {
                self.set_collar_color(collar_int as u8);
            }
            self.read_animal_nbt(nbt);
            self.read_tamable_nbt(nbt);
            self.read_persistent_anger_nbt(nbt).await;
        })
    }

    fn get_mob_entity(&self) -> &MobEntity {
        &self.mob_entity
    }

    fn mob_tick<'a>(&'a self, _caller: &'a Arc<dyn EntityBase>) -> EntityBaseFuture<'a, ()> {
        Box::pin(async move {
            self.update_persistent_anger(true).await;
        })
    }

    fn mob_set_variant_name(&self, name: &str) {
        let variant = match name.strip_prefix("minecraft:").unwrap_or(name) {
            "ashen" => 0,
            "black" => 1,
            "chestnut" => 2,
            "rusty" => 4,
            "snowy" => 5,
            "spotted" => 6,
            "striped" => 7,
            "woods" => 8,
            _ => 3,
        };
        self.set_variant(variant);
    }

    fn mob_interact<'a>(
        &'a self,
        player: &'a Arc<crate::entity::player::Player>,
        item_stack: &'a mut ItemStack,
    ) -> EntityBaseFuture<'a, bool> {
        Box::pin(async move {
            if (item_stack.item.registry_key == "lead"
                || item_stack.item.registry_key == "minecraft:lead")
                && self
                    .persistent_anger
                    .is_angry(self.get_entity().world.load().get_world_age().await)
            {
                return false;
            }
            self.get_mob_entity().mob_interact(player, item_stack).await
        })
    }

    fn mob_init_data_tracker(&self) -> EntityBaseFuture<'_, ()> {
        Box::pin(async move {
            let entity = self.get_entity();
            let is_baby = entity.age.load(Ordering::Relaxed) < 0;
            if is_baby {
                entity.send_meta_data(
                    &[Metadata::new(
                        pumpkin_data::tracked_data::wolf::BABY_ID,
                        true,
                    )],
                    None,
                );
            }
            entity.send_meta_data(
                &[Metadata::new(
                    pumpkin_data::tracked_data::wolf::TAMEABLE_FLAGS,
                    self.get_tame_flags(),
                )],
                Some(&self.bedrock_state_metadata(
                    self.persistent_anger.end_time() > entity.age.load(Ordering::Relaxed) as i64,
                )),
            );
            entity.send_meta_data(
                &[Metadata::new(
                    pumpkin_data::tracked_data::wolf::COLLAR_COLOR,
                    VarInt(self.collar_color.load(Ordering::Relaxed) as i32),
                )],
                None,
            );
            entity.send_meta_data(
                &[Metadata::new(
                    pumpkin_data::tracked_data::wolf::WOLF_VARIANT_ID,
                    VarInt(self.variant.load(Ordering::Relaxed) as i32),
                )],
                None,
            );
            entity.send_meta_data(
                &[Metadata::new(
                    pumpkin_data::tracked_data::wolf::OWNER_UUID,
                    self.get_owner(),
                )],
                None,
            );
            entity.send_meta_data(
                &[Metadata::new(
                    pumpkin_data::tracked_data::wolf::INTERESTED,
                    self.is_interested(),
                )],
                None,
            );
            self.on_persistent_anger_end_time_changed(self.persistent_anger.end_time());
        })
    }

    fn mob_java_spawn_metadata(
        &self,
        version: JavaMinecraftVersion,
    ) -> EntityBaseFuture<'_, Option<Box<[u8]>>> {
        Box::pin(async move {
            let mut metadata = Vec::new();
            Metadata::new(
                pumpkin_data::tracked_data::wolf::BABY_ID,
                self.get_entity().age.load(Ordering::Relaxed) < 0,
            )
            .write(&mut metadata, &version)
            .ok()?;
            Metadata::new(
                pumpkin_data::tracked_data::wolf::TAMEABLE_FLAGS,
                self.get_tame_flags(),
            )
            .write(&mut metadata, &version)
            .ok()?;
            Metadata::new(
                pumpkin_data::tracked_data::wolf::COLLAR_COLOR,
                VarInt(self.collar_color.load(Ordering::Relaxed) as i32),
            )
            .write(&mut metadata, &version)
            .ok()?;
            Metadata::new(
                pumpkin_data::tracked_data::wolf::OWNER_UUID,
                self.get_owner(),
            )
            .write(&mut metadata, &version)
            .ok()?;
            Metadata::new(
                pumpkin_data::tracked_data::wolf::INTERESTED,
                self.is_interested(),
            )
            .write(&mut metadata, &version)
            .ok()?;
            Metadata::new(
                pumpkin_data::tracked_data::wolf::ANGER_END_TIME,
                self.persistent_anger.end_time(),
            )
            .write(&mut metadata, &version)
            .ok()?;
            Metadata::new(
                pumpkin_data::tracked_data::wolf::WOLF_VARIANT_ID,
                VarInt(self.variant.load(Ordering::Relaxed) as i32),
            )
            .write(&mut metadata, &version)
            .ok()?;
            metadata.push(255);
            Some(metadata.into_boxed_slice())
        })
    }

    fn mob_bedrock_spawn_metadata(&self) -> EntityBaseFuture<'_, Option<EntityMetadata>> {
        Box::pin(async move {
            let world_age = self.get_entity().world.load().get_world_age().await;
            let mut metadata = self.get_entity().bedrock_metadata();
            let angry = self.persistent_anger.is_angry(world_age);
            let state = self.bedrock_state_metadata(angry);
            metadata.0.extend(state.0);
            metadata.set(
                entity_data_key::VARIANT,
                MetadataValue::Int(self.variant.load(Ordering::Relaxed) as i32),
            );
            metadata.set(
                entity_data_key::COLOR_INDEX,
                MetadataValue::Int(self.collar_color.load(Ordering::Relaxed) as i32),
            );
            Some(metadata)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{tail_angle, tame_flags};

    #[test]
    fn tame_flags_match_java_tamable_bits() {
        assert_eq!(tame_flags(false, false), 0);
        assert_eq!(tame_flags(true, false), 0x01);
        assert_eq!(tame_flags(false, true), 0x04);
        assert_eq!(tame_flags(true, true), 0x05);
    }

    #[test]
    fn tail_angle_matches_wolf_pose_contract() {
        assert!(
            (tail_angle(false, false, 8.0, 8.0) - std::f32::consts::PI / 5.0).abs() < f32::EPSILON
        );
        assert!((tail_angle(true, true, 4.0, 8.0) - 1.539_380_4).abs() < f32::EPSILON);

        let healthy = tail_angle(false, true, 8.0, 8.0);
        let hurt = tail_angle(false, true, 4.0, 8.0);
        assert!(hurt < healthy);
    }

    #[test]
    fn tail_angle_handles_zero_max_health() {
        assert!(
            (tail_angle(false, true, 0.0, 0.0) - 0.55 * std::f32::consts::PI).abs() < f32::EPSILON
        );
    }
}
