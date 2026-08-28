use super::{Entity, EntityBase, NbtFuture, ai::pathfinder::Navigator, living::LivingEntity};
use crate::entity::ai::control::MoveControlTrait;
use crate::entity::ai::control::body_rotation_control::BodyRotationControl;
use crate::entity::ai::control::jump_control::JumpControl;
use crate::entity::ai::control::look_control::LookControl;
use crate::entity::ai::control::move_control::MoveControl;
use crate::entity::ai::goal::goal_selector::GoalSelector;
use crate::entity::ai::sensing::Sensing;
use crate::entity::player::Player;
use crate::entity::{DamageContext, EntityBaseFuture};
use crate::server::Server;
use crate::world::World;
use crossbeam::atomic::AtomicCell;
use pumpkin_data::attributes::Attributes;
use pumpkin_data::damage::DamageType;
use pumpkin_data::data_component_impl::EquipmentSlot;
use pumpkin_data::entity::MobCategory;
use pumpkin_data::item_stack::ItemStack;
use pumpkin_data::tag::{self, Taggable};
use pumpkin_data::tracked_data;
use pumpkin_nbt::compound::NbtCompound;
use pumpkin_protocol::java::client::play::{CHeadRot, CUpdateEntityRot, Metadata};
use pumpkin_util::Difficulty;
use pumpkin_util::math::boundingbox::BoundingBox;
use pumpkin_util::math::position::BlockPos;
use pumpkin_util::math::vector2::Vector2;
use pumpkin_util::math::vector3::Vector3;
use pumpkin_util::random::xoroshiro128::Xoroshiro;
use pumpkin_util::random::{RandomGenerator, get_seed};
use pumpkin_util::version::JavaMinecraftVersion;
use rand::RngExt;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::Ordering::Relaxed;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU8, Ordering};
use uuid::Uuid;

pub mod bat;
pub mod blaze;
pub mod breeze;
pub mod cave_spider;
pub mod creaking;
pub mod creeper;
pub mod elder_guardian;
pub mod enderman;
pub mod endermite;
pub mod equipment;
pub mod evoker;
pub mod ghast;
pub mod giant;
pub mod guardian;
pub mod hoglin;
pub mod illusioner;
pub mod magma_cube;
pub mod patrol;
pub mod phantom;
pub mod piglin;
pub mod piglin_brute;
pub mod pillager;
pub mod raider;
pub mod ravager;
pub mod shulker;
pub mod silverfish;
pub mod skeleton;
pub mod slime;
pub mod spider;
pub mod vex;
pub mod vindicator;
pub mod warden;
pub mod witch;
pub mod zoglin;
pub mod zombie;
pub mod zombified_piglin;

pub struct MobEntity {
    pub living_entity: LivingEntity,
    pub goals_selector: std::sync::Mutex<GoalSelector>,
    pub target_selector: std::sync::Mutex<GoalSelector>,
    pub navigator: std::sync::Mutex<Navigator>,
    pub target: tokio::sync::Mutex<Option<Arc<dyn EntityBase>>>,
    pub look_control: std::sync::Mutex<LookControl>,
    pub move_control: std::sync::Mutex<Box<dyn MoveControlTrait>>,
    pub jump_control: std::sync::Mutex<JumpControl>,
    pub body_rotation_control: tokio::sync::Mutex<BodyRotationControl>,
    pub sensing: Sensing,
    pub position_target: AtomicCell<BlockPos>,
    pub position_target_range: AtomicI32,
    pub love_ticks: AtomicI32,
    pub breeding_cooldown: AtomicI32,
    pub breeder: AtomicCell<Option<Uuid>>,
    pub persistence_required: AtomicBool,
    pub no_action_time: AtomicI32,
    mob_flags: AtomicU8,
    last_sent_yaw: AtomicU8,
    last_sent_pitch: AtomicU8,
    last_sent_head_yaw: AtomicU8,
}

const fn should_tick_mob_ai(no_ai: bool) -> bool {
    !no_ai
}

const fn goal_control_policy(has_controlling_mob: bool, is_in_boat: bool) -> (bool, bool, bool) {
    let enable_move_and_look = !has_controlling_mob;
    (
        enable_move_and_look,
        enable_move_and_look && !is_in_boat,
        enable_move_and_look,
    )
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MobDespawnDecision {
    Keep,
    ResetNoActionTime,
    Discard,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SunProtectionOutcome {
    Unprotected,
    Protected,
    Damaged,
    Broken,
}

fn apply_sun_protection_damage(
    stack: &mut ItemStack,
    durability_roll: i32,
) -> SunProtectionOutcome {
    if stack.is_empty() {
        return SunProtectionOutcome::Unprotected;
    }
    if !stack.is_damageable() || stack.is_unbreakable() || durability_roll <= 0 {
        return SunProtectionOutcome::Protected;
    }

    let Some(max_damage) = stack.get_max_damage() else {
        return SunProtectionOutcome::Protected;
    };
    let new_damage = stack.get_damage().saturating_add(durability_roll);
    if new_damage >= max_damage {
        *stack = ItemStack::EMPTY.clone();
        SunProtectionOutcome::Broken
    } else {
        stack.set_damage(new_damage);
        SunProtectionOutcome::Damaged
    }
}

fn is_within_home(center: BlockPos, radius: i32, position: &BlockPos) -> bool {
    radius == -1 || center.squared_distance(position) < radius * radius
}

#[derive(Clone, Copy)]
struct MobDespawnContext {
    peaceful: bool,
    allowed_in_peaceful: bool,
    persistence_required: bool,
    custom_persistence: bool,
    nearest_player_distance_squared: Option<f64>,
    instant_despawn_distance: i32,
    no_action_time: i32,
    random_despawn_hit: bool,
    remove_when_far_away: bool,
}

fn mob_despawn_decision(context: MobDespawnContext) -> MobDespawnDecision {
    if context.peaceful && !context.allowed_in_peaceful {
        return MobDespawnDecision::Discard;
    }
    if context.persistence_required || context.custom_persistence {
        return MobDespawnDecision::ResetNoActionTime;
    }

    let Some(distance_squared) = context.nearest_player_distance_squared else {
        return MobDespawnDecision::Keep;
    };
    let instant_distance_squared =
        f64::from(context.instant_despawn_distance * context.instant_despawn_distance);
    if distance_squared > instant_distance_squared && context.remove_when_far_away {
        return MobDespawnDecision::Discard;
    }

    let no_despawn_distance_squared =
        f64::from(MobCategory::NO_DESPAWN_DISTANCE * MobCategory::NO_DESPAWN_DISTANCE);
    if context.no_action_time > 600
        && context.random_despawn_hit
        && distance_squared > no_despawn_distance_squared
        && context.remove_when_far_away
    {
        MobDespawnDecision::Discard
    } else if distance_squared < no_despawn_distance_squared {
        MobDespawnDecision::ResetNoActionTime
    } else {
        MobDespawnDecision::Keep
    }
}

/// Tick boundaries (both inclusive) when monsters do not burn in sunlight (26.1).
///
/// Sourced from `data/minecraft/timeline/day.json` — `monsters_burn` keyframes:
/// `value=false` at tick 12542 (dusk), `value=true` at tick 23460 (dawn).
///
/// TODO: Replace with `EnvironmentAttributes::MONSTERS_BURN` lookup once the
/// `EnvironmentAttributeSystem` is implemented in `pumpkin-data`.
pub(crate) const NIGHT_START: i64 = 12542;
pub(crate) const NIGHT_END: i64 = 23459;

impl MobEntity {
    const AI_DISABLED_FLAG: u8 = 1;
    const LEFT_HANDED_FLAG: u8 = 2;
    const ATTACKING_FLAG: u8 = 4;
    const CAN_PICK_UP_LOOT_FLAG: u8 = 8;

    #[must_use]
    pub fn new(entity: Entity) -> Self {
        Self {
            living_entity: LivingEntity::new(entity),
            goals_selector: std::sync::Mutex::new(GoalSelector::default()),
            target_selector: std::sync::Mutex::new(GoalSelector::default()),
            navigator: std::sync::Mutex::new(Navigator::default()),
            target: tokio::sync::Mutex::new(None),
            look_control: std::sync::Mutex::new(LookControl::default()),
            move_control: std::sync::Mutex::new(Box::new(MoveControl::default())),
            jump_control: std::sync::Mutex::new(JumpControl::default()),
            body_rotation_control: tokio::sync::Mutex::new(BodyRotationControl::default()),
            sensing: Sensing::default(),
            position_target: AtomicCell::new(BlockPos::ZERO),
            position_target_range: AtomicI32::new(-1),
            love_ticks: AtomicI32::new(0),
            breeding_cooldown: AtomicI32::new(0),
            breeder: AtomicCell::new(None),
            persistence_required: AtomicBool::new(false),
            no_action_time: AtomicI32::new(0),
            mob_flags: AtomicU8::new(0),
            last_sent_yaw: AtomicU8::new(0),
            last_sent_pitch: AtomicU8::new(0),
            last_sent_head_yaw: AtomicU8::new(0),
        }
    }

    fn restore_goal_selectors(&self, target_selector: GoalSelector, goals_selector: GoalSelector) {
        let mut target_slot = self
            .target_selector
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *target_slot = target_selector;
        drop(target_slot);
        let mut goals_slot = self
            .goals_selector
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *goals_slot = goals_selector;
        drop(goals_slot);
    }

    fn restore_navigator(&self, navigator: Navigator) {
        let mut navigator_slot = self
            .navigator
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *navigator_slot = navigator;
        drop(navigator_slot);
    }

    fn tick_controls(&self, mob: &dyn Mob) {
        let mut move_control = self
            .move_control
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        move_control.tick(mob);
        drop(move_control);
        let mut look_control = self
            .look_control
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        look_control.tick(mob);
        drop(look_control);
        let mut jump_control = self
            .jump_control
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        jump_control.tick(mob);
        drop(jump_control);
    }

    pub fn has_position_target(&self) -> bool {
        self.position_target_range.load(Relaxed) != -1
    }

    pub fn is_in_position_target_range(&self) -> bool {
        self.is_in_position_target_range_pos(&self.living_entity.entity.block_pos.load())
    }

    pub fn is_in_position_target_range_pos(&self, block_pos: &BlockPos) -> bool {
        let position_target_range = self.position_target_range.load(Relaxed);
        is_within_home(
            self.position_target.load(),
            position_target_range,
            block_pos,
        )
    }

    pub fn set_position_target(&self, center: BlockPos, radius: i32) {
        self.position_target.store(center);
        self.position_target_range.store(radius, Relaxed);
    }

    pub fn clear_position_target(&self) {
        self.position_target_range.store(-1, Relaxed);
    }

    pub fn position_target(&self) -> BlockPos {
        self.position_target.load()
    }

    pub fn position_target_range(&self) -> i32 {
        self.position_target_range.load(Relaxed)
    }

    pub fn set_attacking(&self, attacking: bool) {
        self.set_mob_flag(Self::ATTACKING_FLAG, attacking);
    }

    pub fn is_attacking(&self) -> bool {
        (self.mob_flags.load(Relaxed) & Self::ATTACKING_FLAG) != 0
    }

    pub fn set_left_handed(&self, left_handed: bool) {
        self.set_mob_flag(Self::LEFT_HANDED_FLAG, left_handed);
    }

    pub fn can_pick_up_loot(&self) -> bool {
        (self.mob_flags.load(Relaxed) & Self::CAN_PICK_UP_LOOT_FLAG) != 0
    }

    pub fn set_can_pick_up_loot(&self, value: bool) {
        self.set_mob_flag(Self::CAN_PICK_UP_LOOT_FLAG, value);
    }

    pub fn is_left_handed(&self) -> bool {
        (self.mob_flags.load(Relaxed) & Self::LEFT_HANDED_FLAG) != 0
    }

    pub fn set_no_ai(&self, no_ai: bool) {
        self.set_mob_flag(Self::AI_DISABLED_FLAG, no_ai);
    }

    pub fn is_no_ai(&self) -> bool {
        (self.mob_flags.load(Relaxed) & Self::AI_DISABLED_FLAG) != 0
    }

    pub fn set_persistence_required(&self) {
        self.persistence_required.store(true, Relaxed);
    }

    #[must_use]
    pub fn is_persistence_required(&self) -> bool {
        self.persistence_required.load(Relaxed)
    }

    pub async fn clear_ai_goals(&self, mob: &dyn Mob) {
        let running_goals = self
            .goals_selector
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
        for mut goal in running_goals {
            goal.goal.stop(mob).await;
        }

        let running_target_goals = self
            .target_selector
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
        for mut goal in running_target_goals {
            goal.goal.stop(mob).await;
        }
    }

    async fn update_goal_control_flags(&self) {
        let has_controlling_mob = self
            .living_entity
            .entity
            .passengers
            .lock()
            .await
            .first()
            .and_then(|passenger| passenger.get_mob())
            .is_some_and(|passenger| {
                !passenger.get_mob_entity().is_no_ai()
                    && !passenger
                        .get_entity()
                        .entity_type
                        .has_tag(&tag::EntityType::MINECRAFT_NON_CONTROLLING_RIDER)
            });
        let is_in_boat = self
            .living_entity
            .entity
            .vehicle
            .lock()
            .await
            .as_ref()
            .is_some_and(|vehicle| {
                vehicle
                    .get_entity()
                    .entity_type
                    .has_tag(&tag::EntityType::MINECRAFT_BOAT)
            });
        let (move_enabled, jump_enabled, look_enabled) =
            goal_control_policy(has_controlling_mob, is_in_boat);
        let mut goals = self
            .goals_selector
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        goals.set_control_enabled(crate::entity::ai::goal::Controls::MOVE, move_enabled);
        goals.set_control_enabled(crate::entity::ai::goal::Controls::JUMP, jump_enabled);
        goals.set_control_enabled(crate::entity::ai::goal::Controls::LOOK, look_enabled);
    }

    pub fn write_mob_nbt(&self, nbt: &mut NbtCompound) {
        nbt.put_bool("CanPickUpLoot", self.can_pick_up_loot());
        nbt.put_bool("PersistenceRequired", self.is_persistence_required());
        nbt.put_bool("LeftHanded", self.is_left_handed());
        if self.is_no_ai() {
            nbt.put_bool("NoAI", true);
        }
        if self.has_position_target() {
            nbt.put_int("home_radius", self.position_target_range());
            let home = self.position_target();
            let mut home_nbt = NbtCompound::new();
            home_nbt.put_int("x", home.0.x);
            home_nbt.put_int("y", home.0.y);
            home_nbt.put_int("z", home.0.z);
            nbt.put_compound("home_pos", home_nbt);
        }
    }

    pub fn read_mob_nbt(&self, nbt: &NbtCompound) {
        if let Some(no_ai) = nbt.get_bool("NoAI") {
            self.set_no_ai(no_ai);
        }
        if let Some(left_handed) = nbt.get_bool("LeftHanded") {
            self.set_left_handed(left_handed);
        }
        if let Some(can_pick_up_loot) = nbt.get_bool("CanPickUpLoot") {
            self.set_can_pick_up_loot(can_pick_up_loot);
        }
        self.persistence_required.store(
            nbt.get_bool("PersistenceRequired").unwrap_or(false),
            Relaxed,
        );
        let home_radius = nbt.get_int("home_radius").unwrap_or(-1);
        self.position_target_range.store(home_radius, Relaxed);
        if home_radius >= 0 {
            let home = nbt
                .get_compound("home_pos")
                .and_then(|home| {
                    Some(BlockPos::new(
                        home.get_int("x")?,
                        home.get_int("y")?,
                        home.get_int("z")?,
                    ))
                })
                .unwrap_or(BlockPos::ZERO);
            self.position_target.store(home);
        }
    }

    pub fn add_goal<G: crate::entity::ai::goal::Goal + 'static>(&self, priority: u8, goal: G) {
        self.goals_selector
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .add_goal(priority, Box::new(goal));
    }

    pub fn add_target_goal<G: crate::entity::ai::goal::Goal + 'static>(
        &self,
        priority: u8,
        goal: G,
    ) {
        self.target_selector
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .add_goal(priority, Box::new(goal));
    }

    pub async fn set_target(&self, target: Option<Arc<dyn EntityBase>>) {
        let mut t = self.target.lock().await;
        *t = target;
    }

    pub async fn get_target(&self) -> Option<Arc<dyn EntityBase>> {
        self.target.lock().await.clone()
    }

    fn set_mob_flag(&self, flag: u8, value: bool) {
        let old_b = self.mob_flags.load(Ordering::Relaxed);

        let new_b = if value { old_b | flag } else { old_b & !flag };

        if new_b != old_b {
            self.mob_flags.store(new_b, Ordering::Relaxed);

            self.living_entity.entity.send_meta_data(
                &[Metadata::new(tracked_data::mob::DATA_MOB_FLAGS_ID, new_b)],
                None,
            );
        }
    }

    pub fn is_in_love(&self) -> bool {
        self.love_ticks.load(Relaxed) > 0
    }

    pub fn set_love_ticks(&self, ticks: i32, breeder: Option<Uuid>) {
        self.love_ticks.store(ticks, Relaxed);
        self.breeder.store(breeder);
    }

    pub fn reset_love_ticks(&self) {
        self.love_ticks.store(0, Relaxed);
    }

    pub fn is_breeding_ready(&self) -> bool {
        self.living_entity.entity.age.load(Relaxed) >= 0
            && self.breeding_cooldown.load(Relaxed) <= 0
    }

    pub async fn is_in_attack_range(&self, target: &dyn EntityBase) -> bool {
        const DEFAULT_ATTACK_RANGE: f64 = 0.828_427_12; // sqrt(2.04) - 0.6

        // TODO: Implement DataComponent lookup for ATTACK_RANGE when components are ready
        let max_range = DEFAULT_ATTACK_RANGE;
        let min_range = 0.0;

        let target_hitbox = target.get_entity().bounding_box.load();

        if !self
            .get_attack_box(max_range)
            .await
            .intersects(&target_hitbox)
        {
            return false;
        }

        min_range <= 0.0
            || !self
                .get_attack_box(min_range)
                .await
                .intersects(&target_hitbox)
    }

    pub fn is_dark_enough_to_spawn(world: &World, pos: &BlockPos, is_thundering: bool) -> bool {
        let sky_light = world.get_sky_light_level(pos);
        if sky_light > rand::random_range(0..32) {
            return false;
        }

        let dimension = &world.dimension;
        let block_light_limit = dimension.monster_spawn_block_light_limit;

        let block_light = world.get_block_light_level(pos).unwrap_or(0);
        if block_light_limit < 15 && block_light > block_light_limit {
            return false;
        }

        let current_brightness = if is_thundering {
            (sky_light - 10).max(block_light)
        } else {
            sky_light.max(block_light)
        };

        // TODO
        let mut random = RandomGenerator::Xoroshiro(Xoroshiro::from_seed(get_seed()));
        current_brightness <= dimension.monster_spawn_light_level.get(&mut random) as u8
    }

    pub fn check_monster_spawn_rules(world: &World, pos: &BlockPos, is_thundering: bool) -> bool {
        if world.level_info.load().difficulty == Difficulty::Peaceful {
            return false;
        }

        if !Self::is_dark_enough_to_spawn(world, pos, is_thundering) {
            return false;
        }

        //TODO:check_mob_spawn_rules(entity_type, world, spawn_reason, pos).await
        true
    }

    pub async fn try_attack(&self, caller: &dyn EntityBase, target: &dyn EntityBase) {
        if self.living_entity.dead.load(Relaxed) {
            return;
        }

        let attack_damage: f32 =
            self.living_entity
                .get_attribute_value(&Attributes::ATTACK_DAMAGE) as f32;

        let damaged = target
            .damage_with_context(
                target,
                DamageContext::new(attack_damage, DamageType::MOB_ATTACK)
                    .with_direct_entity(caller)
                    .with_causing_entity(caller),
            )
            .await;

        if damaged {
            self.living_entity
                .last_attacking_id
                .store(target.get_entity().entity_id, Relaxed);
            self.living_entity
                .last_attack_time
                .store(self.living_entity.entity.age.load(Relaxed), Relaxed);
        }
    }

    async fn get_attack_box(&self, attack_range: f64) -> BoundingBox {
        let vehicle_lock = self.living_entity.entity.vehicle.lock().await;

        let base_box = vehicle_lock.as_ref().map_or_else(
            || self.living_entity.entity.bounding_box.load(),
            |vehicle| {
                let vehicle_box = vehicle.get_entity().bounding_box.load();
                let my_box = self.living_entity.entity.bounding_box.load();

                BoundingBox {
                    min: Vector3::new(
                        my_box.min.x.min(vehicle_box.min.x),
                        my_box.min.y,
                        my_box.min.z.min(vehicle_box.min.z),
                    ),
                    max: Vector3::new(
                        my_box.max.x.max(vehicle_box.max.x),
                        my_box.max.y,
                        my_box.max.z.max(vehicle_box.max.z),
                    ),
                }
            },
        );

        base_box.expand(attack_range, 0.0, attack_range)
    }

    pub async fn tick_sun_burn(&self, protection_slot: EquipmentSlot) {
        if self.living_entity.dead.load(Relaxed) || self.living_entity.health.load() <= 0.0 {
            return;
        }
        if !self
            .living_entity
            .entity
            .entity_type
            .has_tag(&tag::EntityType::MINECRAFT_BURN_IN_DAYLIGHT)
        {
            return;
        }
        if !self.is_sun_burn_tick().await {
            return;
        }
        self.apply_sun_burn(protection_slot).await;
    }

    async fn is_sun_burn_tick(&self) -> bool {
        let entity = &self.living_entity.entity;

        let world_arc = entity.world.load();
        let world = world_arc.as_ref();

        // Night boundary from data/minecraft/timeline/day.json — monsters_burn keyframes:
        // value=false at tick 12542 (dusk), value=true at tick 23460 (dawn).
        // TODO: read directly from EnvironmentAttributes::MONSTERS_BURN once implemented.

        let day_time = world.get_time_of_day().await % 24000;
        if (NIGHT_START..=NIGHT_END).contains(&day_time) {
            return false;
        }

        // Vanilla: getLightLevelDependentMagicValue() — sky light at eye pos, scaled 0–1.
        let eye_block_pos = entity.get_eye_pos();
        let brightness = world
            .level
            .light_engine
            .get_sky_light_level(&world.level, &eye_block_pos.to_block_pos())
            as f32
            / 15.0;

        if brightness <= 0.5 {
            return false;
        }

        let is_in_non_burnable = entity.touching_water.load(Relaxed)
            || world.weather.lock().await.raining
            || entity.is_in_powder_snow()
            || entity.was_in_powder_snow.load(Relaxed);

        if is_in_non_burnable {
            return false;
        }

        let pos = entity.pos.load();
        let top_y = world.get_top_block(Vector2::new(pos.x as i32, pos.z as i32));
        if (entity.get_eye_y() as i32) < top_y {
            return false;
        }

        let mut rng = rand::rng();
        rng.random::<f32>() * 30.0 < (brightness - 0.4) * 2.0
    }

    async fn apply_sun_burn(&self, protection_slot: EquipmentSlot) {
        let entity = &self.living_entity.entity;
        let durability_roll = rand::rng().random_range(0..2);
        let (outcome, updated_stack) = {
            let mut equipment = self.living_entity.entity_equipment.lock().await;
            equipment.equipment.get_mut(&protection_slot).map_or_else(
                || (SunProtectionOutcome::Unprotected, ItemStack::EMPTY.clone()),
                |stack| {
                    let outcome = apply_sun_protection_damage(stack, durability_roll);
                    (outcome, stack.clone())
                },
            )
        };

        match outcome {
            SunProtectionOutcome::Unprotected => entity.set_on_fire_for(8.0),
            SunProtectionOutcome::Protected => {}
            SunProtectionOutcome::Damaged => self
                .living_entity
                .send_equipment_changes(&[(protection_slot, updated_stack)]),
            SunProtectionOutcome::Broken => {
                entity.world.load().send_entity_status(
                    entity,
                    super::equipment_break_status(&protection_slot),
                    None,
                );
                self.living_entity
                    .send_equipment_changes(&[(protection_slot, updated_stack)]);
            }
        }
    }

    pub async fn mob_interact(&self, player: &Arc<Player>, item_stack: &mut ItemStack) -> bool {
        let entity = &self.living_entity.entity;

        // If already leashed to player, right-clicking unleashes the mob
        let currently_leashed = {
            let guard = entity.leashed_to.lock().await;
            guard.is_some()
        };

        if currently_leashed {
            entity.unleash().await;
            let lead_item =
                pumpkin_data::item_stack::ItemStack::new(1, &pumpkin_data::item::Item::LEAD);
            entity
                .world
                .load()
                .drop_stack(&entity.block_pos.load(), lead_item)
                .await;
            return true;
        }

        // If holding a lead, leash the mob to the player
        if item_stack.item.registry_key == "lead"
            || item_stack.item.registry_key == "minecraft:lead"
        {
            let diff = entity.pos.load() - player.get_entity().pos.load();
            let dist_sq = diff.length_squared();
            if dist_sq <= Entity::LEASH_SNAP_DISTANCE * Entity::LEASH_SNAP_DISTANCE {
                entity.leash_to(player.clone() as Arc<dyn EntityBase>).await;
                if player.gamemode.load() != pumpkin_util::GameMode::Creative {
                    item_stack.decrement(1);
                }
                return true;
            }
        }

        false
    }
}

pub trait Mob: EntityBase + Send + Sync {
    fn get_random(&self) -> rand::rngs::ThreadRng {
        rand::rng()
    }

    fn get_max_look_yaw_change(&self) -> f32 {
        10.0
    }

    fn get_max_look_pitch_change(&self) -> f32 {
        40.0
    }

    fn get_max_head_rotation(&self) -> f32 {
        75.0
    }

    fn get_mob_entity(&self) -> &MobEntity;

    fn sun_protection_slot(&self) -> EquipmentSlot {
        EquipmentSlot::HEAD
    }

    /// Entity-local override point for Vanilla's `removeWhenFarAway` subclasses.
    fn remove_when_far_away(&self, _distance_squared: f64) -> bool {
        !self
            .get_mob_entity()
            .living_entity
            .entity
            .entity_type
            .category
            .is_persistent
    }

    /// Common Vanilla persistence exception. Bucket, raid, trust and other family-specific
    /// persistence conditions extend this in their concrete mob implementation.
    fn requires_custom_persistence(&self) -> EntityBaseFuture<'_, bool> {
        Box::pin(async move {
            let entity = &self.get_mob_entity().living_entity.entity;
            if entity.vehicle.lock().await.is_some() {
                return true;
            }
            entity.leashed_to.lock().await.is_some()
        })
    }

    fn check_despawn(&self) -> EntityBaseFuture<'_, bool> {
        Box::pin(async move {
            let mob_entity = self.get_mob_entity();
            let entity = &mob_entity.living_entity.entity;
            let world = entity.world.load_full();
            let position = entity.pos.load();
            let nearest_player_distance_squared = world
                .players
                .load()
                .iter()
                .map(|player| {
                    player
                        .get_entity()
                        .pos
                        .load()
                        .squared_distance_to_vec(&position)
                })
                .min_by(f64::total_cmp);
            let no_action_time = mob_entity.no_action_time.load(Relaxed);
            let no_despawn_distance_squared =
                f64::from(MobCategory::NO_DESPAWN_DISTANCE * MobCategory::NO_DESPAWN_DISTANCE);
            let remove_when_far_away = nearest_player_distance_squared
                .is_some_and(|distance| self.remove_when_far_away(distance));
            let random_despawn_hit = no_action_time > 600
                && nearest_player_distance_squared
                    .is_some_and(|distance| distance > no_despawn_distance_squared)
                && remove_when_far_away
                && self.get_random().random_range(0..800) == 0;
            let decision = mob_despawn_decision(MobDespawnContext {
                peaceful: world.level_info.load().difficulty == Difficulty::Peaceful,
                allowed_in_peaceful: entity.entity_type.is_allowed_in_peaceful(),
                persistence_required: mob_entity.is_persistence_required(),
                custom_persistence: self.requires_custom_persistence().await,
                nearest_player_distance_squared,
                instant_despawn_distance: entity.entity_type.category.despawn_distance,
                no_action_time,
                random_despawn_hit,
                remove_when_far_away,
            });

            match decision {
                MobDespawnDecision::Keep => false,
                MobDespawnDecision::ResetNoActionTime => {
                    mob_entity.no_action_time.store(0, Relaxed);
                    false
                }
                MobDespawnDecision::Discard => {
                    entity.remove().await;
                    true
                }
            }
        })
    }

    fn mob_bedrock_identifier(&self) -> Option<&'static str> {
        None
    }

    /// Metadata which must accompany this mob whenever it is spawned for a Java client.
    fn mob_java_spawn_metadata(
        &self,
        _version: JavaMinecraftVersion,
    ) -> EntityBaseFuture<'_, Option<Box<[u8]>>> {
        Box::pin(async { None })
    }

    /// Metadata which must accompany this mob whenever it is spawned for a Bedrock client.
    fn mob_bedrock_spawn_metadata(
        &self,
    ) -> EntityBaseFuture<
        '_,
        Option<pumpkin_protocol::bedrock::client::set_actor_data::EntityMetadata>,
    > {
        Box::pin(async { None })
    }

    fn get_job_site(&self) -> Option<BlockPos> {
        None
    }

    fn is_job_site_pending(&self) -> EntityBaseFuture<'_, bool> {
        Box::pin(async { false })
    }

    fn release_pending_job_site(&self, _position: BlockPos) -> EntityBaseFuture<'_, ()> {
        Box::pin(async {})
    }

    fn get_trading_player(&self) -> Option<Arc<Player>> {
        None
    }

    fn get_home(&self) -> Option<BlockPos> {
        None
    }

    fn get_path_aware_entity(&self) -> Option<&dyn PathAwareEntity> {
        None
    }

    fn get_item_steerable(&self) -> Option<&dyn crate::entity::item_steerable::ItemSteerable> {
        None
    }

    fn is_saddled(&self) -> bool {
        false
    }

    fn can_be_saddled(&self) -> bool {
        false
    }

    fn set_saddled(&self, _saddled: bool) {}

    /// Per-mob tick hook called each tick before AI runs. Override for mob-specific logic.
    fn mob_tick<'a>(&'a self, _caller: &'a Arc<dyn EntityBase>) -> EntityBaseFuture<'a, ()> {
        Box::pin(async {})
    }

    fn post_tick(&self) -> EntityBaseFuture<'_, ()> {
        Box::pin(async {})
    }

    /// Called before damage is applied. Return `false` to cancel the damage entirely.
    /// Used by endermen to dodge projectiles via teleportation.
    fn pre_damage<'a>(
        &'a self,
        _damage_type: DamageType,
        _source: Option<&'a dyn EntityBase>,
    ) -> EntityBaseFuture<'a, bool> {
        Box::pin(async { true })
    }

    fn on_damage<'a>(
        &'a self,
        _damage_type: DamageType,
        _source: Option<&'a dyn EntityBase>,
    ) -> EntityBaseFuture<'a, ()> {
        Box::pin(async {})
    }

    fn on_eating_grass(&self) -> EntityBaseFuture<'_, ()> {
        Box::pin(async {})
    }

    fn modify_incoming_damage(&self, amount: f32, _damage_type: DamageType) -> f32 {
        amount
    }

    fn can_attack_with_owner(&self, _target: &dyn EntityBase, _owner: &dyn EntityBase) -> bool {
        true
    }

    fn get_mob_gravity(&self) -> f64 {
        self.get_mob_entity().living_entity.get_gravity()
    }

    fn get_mob_y_velocity_drag(&self) -> Option<f64> {
        None
    }

    fn as_ageable(&self) -> Option<&dyn crate::entity::ageable::AgeableMob> {
        None
    }

    fn as_animal(&self) -> Option<&dyn crate::entity::passive::animal::Animal> {
        None
    }

    fn as_tamable(&self) -> Option<&dyn crate::entity::passive::tamable::TamableAnimal> {
        None
    }

    fn as_patrolling_monster(&self) -> Option<&dyn patrol::PatrollingMonster> {
        None
    }

    fn as_raider(&self) -> Option<&dyn raider::Raider> {
        None
    }

    fn as_neutral(&self) -> Option<&dyn crate::entity::ai::neutral::NeutralMob> {
        None
    }

    fn as_iron_golem(&self) -> Option<&crate::entity::passive::iron_golem::IronGolemEntity> {
        None
    }

    fn mob_write_nbt<'a>(&'a self, _nbt: &'a mut NbtCompound) -> NbtFuture<'a, ()> {
        Box::pin(async {})
    }

    fn mob_read_nbt<'a>(&'a self, _nbt: &'a NbtCompound) -> NbtFuture<'a, ()> {
        Box::pin(async {})
    }

    /// Set or clear the mob's target. Override to add side effects when targeting changes.
    fn set_mob_target(&self, target: Option<Arc<dyn EntityBase>>) -> EntityBaseFuture<'_, ()> {
        Box::pin(async move {
            let target_id = target.as_ref().map(|t| t.get_entity().entity_id);
            let mob = self.get_mob_entity();
            let mut event =
                crate::plugin::api::events::entity::entity_target::EntityTargetEvent::new(
                    mob.living_entity.entity.entity_id,
                    target_id,
                );
            let server = mob.living_entity.entity.world.load().server.upgrade();
            if let Some(server) = server {
                server.plugin_manager.fire(&server, &mut event).await;
            }
            if event.cancelled {
                return;
            }
            let mut mob_target = mob.target.lock().await;
            *mob_target = target;
        })
    }

    fn mob_interact<'a>(
        &'a self,
        player: &'a Arc<Player>,
        item_stack: &'a mut ItemStack,
    ) -> EntityBaseFuture<'a, bool> {
        Box::pin(async move { self.get_mob_entity().mob_interact(player, item_stack).await })
    }

    fn tame<'a>(&'a self, player: &'a Arc<Player>) -> EntityBaseFuture<'a, ()> {
        Box::pin(async move {
            let mob = self.get_mob_entity();
            let mut event = crate::plugin::api::events::entity::entity_tame::EntityTameEvent::new(
                mob.living_entity.entity.entity_id,
                player.clone(),
            );
            let server = mob.living_entity.entity.world.load().server.upgrade();
            if let Some(server) = server {
                server.plugin_manager.fire(&server, &mut event).await;
            }
        })
    }

    fn breed(&self, father_id: i32, mother_id: i32, child_id: i32) -> EntityBaseFuture<'_, ()> {
        Box::pin(async move {
            let mob = self.get_mob_entity();
            let mut event = crate::plugin::api::events::entity::entity_breed::EntityBreedEvent::new(
                father_id, mother_id, child_id,
            );
            let server = mob.living_entity.entity.world.load().server.upgrade();
            if let Some(server) = server {
                server.plugin_manager.fire(&server, &mut event).await;
            }
        })
    }

    fn dye<'a>(
        &'a self,
        color: crate::plugin::api::events::entity::entity_dye::DyeColor,
        player: Option<&'a Arc<Player>>,
    ) -> EntityBaseFuture<'a, ()> {
        Box::pin(async move {
            let mob = self.get_mob_entity();
            let mut event = crate::plugin::api::events::entity::entity_dye::EntityDyeEvent::new(
                mob.living_entity.entity.entity_id,
                color,
                player.cloned(),
            );
            let server = mob.living_entity.entity.world.load().server.upgrade();
            if let Some(server) = server {
                server.plugin_manager.fire(&server, &mut event).await;
            }
        })
    }

    fn enter_love_mode(
        &self,
        human_entity_id: Option<i32>,
        ticks_in_love: i32,
    ) -> EntityBaseFuture<'_, ()> {
        Box::pin(async move {
            let mob = self.get_mob_entity();
            let mut event = crate::plugin::api::events::entity::entity_enter_love_mode::EntityEnterLoveModeEvent::new(
                mob.living_entity.entity.entity_id,
                human_entity_id,
                ticks_in_love,
            );
            let server = mob.living_entity.entity.world.load().server.upgrade();
            if let Some(server) = server {
                server.plugin_manager.fire(&server, &mut event).await;
            }
        })
    }

    fn transform(&self, new_entity_id: i32, transform_reason: String) -> EntityBaseFuture<'_, ()> {
        Box::pin(async move {
            let mob = self.get_mob_entity();
            let mut event =
                crate::plugin::api::events::entity::entity_transform::EntityTransformEvent::new(
                    mob.living_entity.entity.entity_id,
                    new_entity_id,
                    transform_reason,
                );
            let server = mob.living_entity.entity.world.load().server.upgrade();
            if let Some(server) = server {
                server.plugin_manager.fire(&server, &mut event).await;
            }
        })
    }

    fn break_door(&self, block_pos: BlockPos) -> EntityBaseFuture<'_, ()> {
        Box::pin(async move {
            let mob = self.get_mob_entity();
            let mut event =
                crate::plugin::api::events::entity::entity_break_door::EntityBreakDoorEvent::new(
                    mob.living_entity.entity.entity_id,
                    block_pos,
                );
            let server = mob.living_entity.entity.world.load().server.upgrade();
            if let Some(server) = server {
                server.plugin_manager.fire(&server, &mut event).await;
            }
        })
    }

    fn enter_block(&self, block_pos: BlockPos) -> EntityBaseFuture<'_, ()> {
        Box::pin(async move {
            let mob = self.get_mob_entity();
            let mut event =
                crate::plugin::api::events::entity::entity_enter_block::EntityEnterBlockEvent::new(
                    mob.living_entity.entity.entity_id,
                    block_pos,
                );
            let server = mob.living_entity.entity.world.load().server.upgrade();
            if let Some(server) = server {
                server.plugin_manager.fire(&server, &mut event).await;
            }
        })
    }

    fn interact(&self, block_pos: BlockPos) -> EntityBaseFuture<'_, ()> {
        Box::pin(async move {
            let mob = self.get_mob_entity();
            let mut event =
                crate::plugin::api::events::entity::entity_interact::EntityInteractEvent::new(
                    mob.living_entity.entity.entity_id,
                    block_pos,
                );
            let server = mob.living_entity.entity.world.load().server.upgrade();
            if let Some(server) = server {
                server.plugin_manager.fire(&server, &mut event).await;
            }
        })
    }

    fn place_block(&self, block_pos: BlockPos, block_name: String) -> EntityBaseFuture<'_, ()> {
        Box::pin(async move {
            let mob = self.get_mob_entity();
            let mut event = crate::plugin::api::events::entity::entity_place::EntityPlaceEvent::new(
                mob.living_entity.entity.entity_id,
                block_pos,
                block_name,
            );
            let server = mob.living_entity.entity.world.load().server.upgrade();
            if let Some(server) = server {
                server.plugin_manager.fire(&server, &mut event).await;
            }
        })
    }

    fn mob_player_collision<'a>(&'a self, _player: &'a Arc<Player>) -> EntityBaseFuture<'a, ()> {
        Box::pin(async {})
    }

    fn get_owner_uuid(&self) -> Option<Uuid> {
        self.as_tamable()
            .and_then(crate::entity::passive::tamable::TamableAnimal::get_owner)
    }

    fn is_sitting(&self) -> bool {
        self.as_tamable()
            .is_some_and(crate::entity::passive::tamable::TamableAnimal::is_in_sitting_pose)
    }

    fn is_tamed(&self) -> bool {
        self.as_tamable()
            .is_some_and(crate::entity::passive::tamable::TamableAnimal::is_tame)
    }

    fn get_base_experience_reward(&self) -> u32 {
        self.get_entity().entity_type.experience_reward
    }

    fn mob_init_data_tracker(&self) -> EntityBaseFuture<'_, ()> {
        Box::pin(async move {
            let entity = self.get_entity();
            let is_baby = entity.age.load(std::sync::atomic::Ordering::Relaxed) < 0;
            if is_baby {
                entity.send_meta_data(
                    &[Metadata::new(tracked_data::ageable_mob::DATA_BABY_ID, true)],
                    None,
                );
            }
        })
    }

    fn mob_set_variant_name(&self, _name: &str) {}

    fn get_sheep(&self) -> Option<&crate::entity::passive::sheep::SheepEntity> {
        None
    }

    fn mob_on_lightning_strike<'a>(
        &'a self,
        caller: &'a dyn EntityBase,
        lightning: &'a crate::entity::lightning::LightningBoltEntity,
    ) -> EntityBaseFuture<'a, ()> {
        Box::pin(async move {
            self.get_mob_entity()
                .living_entity
                .on_lightning_strike(caller, lightning)
                .await;
        })
    }
}
impl<T: Mob + Send + 'static> EntityBase for T {
    fn get_mob(&self) -> Option<&dyn Mob> {
        Some(self)
    }

    fn on_lightning_strike<'a>(
        &'a self,
        caller: &'a dyn EntityBase,
        lightning: &'a crate::entity::lightning::LightningBoltEntity,
    ) -> EntityBaseFuture<'a, ()> {
        Box::pin(async move {
            self.mob_on_lightning_strike(caller, lightning).await;
        })
    }

    fn get_item_steerable(&self) -> Option<&dyn crate::entity::item_steerable::ItemSteerable> {
        Mob::get_item_steerable(self)
    }

    fn init_data_tracker(&self) -> EntityBaseFuture<'_, ()> {
        Box::pin(async move {
            self.mob_init_data_tracker().await;
            let world = self.get_mob_entity().living_entity.entity.world.load();
            crate::entity::mob::equipment::equip_mob_on_spawn(self as &dyn EntityBase, &world)
                .await;

            let entity_name = self.get_entity().entity_type.resource_name;
            if let Some(def) = crate::entity::mob::equipment::EQUIPMENT_REGISTRY.get(entity_name)
                && def.can_pick_up_loot
            {
                let difficulty = crate::entity::mob::equipment::RegionalDifficulty::at(
                    &world,
                    self.get_entity().pos.load(),
                );
                let pickup_chance = 0.55 * difficulty.special_multiplier;
                self.get_mob_entity()
                    .set_can_pick_up_loot(rand::random::<f32>() < pickup_chance);
            }
        })
    }

    fn set_variant_name(&self, name: &str) {
        self.mob_set_variant_name(name);
    }

    #[allow(clippy::too_many_lines)]
    fn tick<'a>(
        &'a self,
        caller: &'a Arc<dyn EntityBase>,
        server: &'a Server,
    ) -> EntityBaseFuture<'a, ()> {
        Box::pin(async move {
            let mob_entity = self.get_mob_entity();
            if self.check_despawn().await {
                return;
            }
            mob_entity.living_entity.entity.tick_leash().await;
            mob_entity.tick_sun_burn(self.sun_protection_slot()).await;

            if mob_entity.breeding_cooldown.load(Relaxed) > 0 {
                mob_entity.breeding_cooldown.fetch_sub(1, Relaxed);
            }

            if mob_entity.love_ticks.load(Relaxed) > 0 {
                let ticks = mob_entity.love_ticks.fetch_sub(1, Relaxed);
                if ticks % 10 == 0 {
                    let entity = &mob_entity.living_entity.entity;
                    let pos = entity.pos.load();
                    let world = entity.world.load();
                    world.spawn_particle(
                        pos + Vector3::new(0.0, f64::from(entity.height()) + 0.5, 0.0),
                        Vector3::new(0.5, 0.5, 0.5),
                        1.0,
                        1,
                        pumpkin_data::particle::Particle::Heart,
                    );
                }
            }

            if should_tick_mob_ai(mob_entity.is_no_ai()) {
                mob_entity.no_action_time.fetch_add(1, Relaxed);
                mob_entity.sensing.tick();
                let age = mob_entity.living_entity.entity.age.load(Relaxed);
                let entity_id = mob_entity.living_entity.entity.entity_id;

                // Target selector precedes the ordinary goal selector.
                let mut target_selector = {
                    let mut guard = mob_entity
                        .target_selector
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    std::mem::take(&mut *guard)
                };
                let mut goals_selector = {
                    let mut guard = mob_entity
                        .goals_selector
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    std::mem::take(&mut *guard)
                };

                if (age + entity_id) % 2 != 0 && age > 1 {
                    target_selector.tick_goals(self, false).await;
                    goals_selector.tick_goals(self, false).await;
                } else {
                    target_selector.tick(self).await;
                    goals_selector.tick(self).await;
                }

                mob_entity.restore_goal_selectors(target_selector, goals_selector);

                let mut navigator = {
                    let mut guard = mob_entity
                        .navigator
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    std::mem::take(&mut *guard)
                };
                navigator.tick(self).await;
                mob_entity.restore_navigator(navigator);

                self.mob_tick(caller).await;

                mob_entity.tick_controls(self);
            } else {
                let mut navigator = mob_entity
                    .navigator
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                navigator.stop();
                mob_entity
                    .living_entity
                    .movement_input
                    .store(Vector3::default());
                mob_entity.living_entity.jumping.store(false, Relaxed);
                mob_entity
                    .jump_control
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .clear();
            }

            mob_entity.living_entity.tick(caller, server).await;
            mob_entity
                .body_rotation_control
                .lock()
                .await
                .tick(self)
                .await;
            self.post_tick().await;

            if mob_entity.living_entity.entity.age.load(Relaxed) % 5 == 0 {
                mob_entity.update_goal_control_flags().await;
            }

            // --- Packet logic remains the same ---
            let entity = &mob_entity.living_entity.entity;
            let yaw = (entity.yaw.load() * 256.0 / 360.0).rem_euclid(256.0) as u8;
            let pitch = (entity.pitch.load() * 256.0 / 360.0).rem_euclid(256.0) as u8;
            let head_yaw = (entity.head_yaw.load() * 256.0 / 360.0).rem_euclid(256.0) as u8;

            let last_yaw = mob_entity.last_sent_yaw.load(Relaxed);
            let last_pitch = mob_entity.last_sent_pitch.load(Relaxed);
            let last_head_yaw = mob_entity.last_sent_head_yaw.load(Relaxed);

            let chunk_pos = entity.chunk_pos.load();
            if yaw.abs_diff(last_yaw) >= 1 || pitch.abs_diff(last_pitch) >= 1 {
                let world = entity.world.load();
                world.broadcast_to_chunk(
                    chunk_pos,
                    &CUpdateEntityRot::new(
                        entity.entity_id.into(),
                        yaw,
                        pitch,
                        entity.on_ground.load(Relaxed),
                    ),
                );
                mob_entity.last_sent_yaw.store(yaw, Relaxed);
                mob_entity.last_sent_pitch.store(pitch, Relaxed);
            }

            if head_yaw.abs_diff(last_head_yaw) >= 1 {
                let world = entity.world.load();

                world.broadcast_to_chunk(
                    chunk_pos,
                    &CHeadRot::new(entity.entity_id.into(), head_yaw),
                );
                mob_entity.last_sent_head_yaw.store(head_yaw, Relaxed);
            }
        })
    }

    fn is_collidable(&self, _entity: Option<Box<dyn EntityBase>>) -> bool {
        true
    }

    fn can_hit(&self) -> bool {
        true
    }

    fn damage_with_context<'a>(
        &'a self,
        target: &'a dyn EntityBase,
        context: DamageContext<'a>,
    ) -> EntityBaseFuture<'a, bool> {
        Box::pin(async move {
            let damage_type = context.damage_type();
            let source = context.direct_entity();
            // pre_damage hook: allows mobs to dodge/cancel damage (e.g. enderman projectile dodge)
            if !self.pre_damage(damage_type, source).await {
                return false;
            }
            // Mob-specific damage modifier (e.g. shulker armor when closed).
            let amount = self.modify_incoming_damage(context.amount(), damage_type);
            let damaged = self
                .get_mob_entity()
                .living_entity
                .damage_with_context(target, context.with_amount(amount))
                .await;
            if damaged {
                self.on_damage(damage_type, source).await;
            }
            damaged
        })
    }

    fn interact<'a>(
        &'a self,
        player: &'a Arc<Player>,
        item_stack: &'a mut ItemStack,
    ) -> EntityBaseFuture<'a, bool> {
        Box::pin(async move { self.mob_interact(player, item_stack).await })
    }

    fn on_player_collision<'a>(&'a self, player: &'a Arc<Player>) -> EntityBaseFuture<'a, ()> {
        Box::pin(async move { self.mob_player_collision(player).await })
    }

    fn get_entity(&self) -> &Entity {
        &self.get_mob_entity().living_entity.entity
    }

    fn get_living_entity(&self) -> Option<&LivingEntity> {
        Some(&self.get_mob_entity().living_entity)
    }

    fn cast_any(&self) -> &dyn std::any::Any {
        self
    }

    fn is_in_love(&self) -> bool {
        self.get_mob_entity().is_in_love()
    }

    fn is_breeding_ready(&self) -> bool {
        self.get_mob_entity().is_breeding_ready()
    }

    fn reset_love(&self) {
        self.get_mob_entity().reset_love_ticks();
    }

    fn set_breeding_cooldown(&self, ticks: i32) {
        self.get_mob_entity()
            .breeding_cooldown
            .store(ticks, Relaxed);
    }

    fn is_panicking(&self) -> bool {
        self.get_path_aware_entity()
            .is_some_and(PathAwareEntity::is_panicking)
    }

    fn get_job_site_pos(&self) -> Option<pumpkin_util::math::position::BlockPos> {
        <T as Mob>::get_job_site(self)
    }

    fn get_home_pos(&self) -> Option<pumpkin_util::math::position::BlockPos> {
        <T as Mob>::get_home(self)
    }

    fn write_custom_nbt_async<'a>(&'a self, nbt: &'a mut NbtCompound) -> NbtFuture<'a, ()> {
        Box::pin(async move {
            self.get_mob_entity().write_mob_nbt(nbt);
            if let Some(ageable) = self.as_ageable() {
                ageable.write_ageable_nbt(nbt);
            }
            if let Some(animal) = self.as_animal() {
                animal.write_animal_nbt(nbt);
            }
            if let Some(tamable) = self.as_tamable() {
                tamable.write_tamable_nbt(nbt);
            }
            self.mob_write_nbt(nbt).await;
        })
    }

    fn read_custom_nbt<'a>(&'a self, nbt: &'a NbtCompound) -> NbtFuture<'a, ()> {
        Box::pin(async move {
            self.get_mob_entity().read_mob_nbt(nbt);
            if let Some(ageable) = self.as_ageable() {
                ageable.read_ageable_nbt(nbt);
            }
            if let Some(animal) = self.as_animal() {
                animal.read_animal_nbt(nbt);
            }
            if let Some(tamable) = self.as_tamable() {
                tamable.read_tamable_nbt(nbt);
            }
            self.mob_read_nbt(nbt).await;
        })
    }

    fn get_gravity(&self) -> f64 {
        self.get_mob_gravity()
    }

    fn get_y_velocity_drag(&self) -> Option<f64> {
        self.get_mob_y_velocity_drag()
    }

    fn get_experience_reward(&self, _killer: Option<&dyn EntityBase>) -> u32 {
        if self
            .get_entity()
            .age
            .load(std::sync::atomic::Ordering::Relaxed)
            < 0
        {
            return 0;
        }
        // TODO: apply enchantment processing like in vanilla
        Mob::get_base_experience_reward(self)
    }

    fn get_base_experience_reward(&self) -> u32 {
        Mob::get_base_experience_reward(self)
    }
}

#[expect(dead_code)]
const DEFAULT_PATHFINDING_FAVOR: f32 = 0.0;

pub trait PathAwareEntity: Mob + Send + Sync {
    fn get_pathfinding_favor(&self, _block_pos: BlockPos, _world: Arc<World>) -> f32 {
        0.0
    }

    // TODO: missing SpawnReason attribute
    fn can_spawn(&self, world: Arc<World>) -> bool {
        self.get_pathfinding_favor(
            self.get_mob_entity().living_entity.entity.block_pos.load(),
            world,
        ) >= 0.0
    }

    fn is_navigation<'a>(&'a self) -> Pin<Box<dyn Future<Output = bool> + Send + 'a>> {
        Box::pin(async {
            let navigator = self
                .get_mob_entity()
                .navigator
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            !navigator.is_idle()
        })
    }

    // TODO: implement
    fn is_panicking(&self) -> bool {
        false
    }

    fn should_follow_leash(&self) -> bool {
        true
    }

    fn on_short_leash_tick(&self) {
        // TODO: implement
    }

    fn before_leash_tick(&self) {
        // TODO: implement
    }

    fn get_follow_leash_speed(&self) -> f32 {
        1.0
    }
}

pub trait RangedAttackMob: Mob + Send + Sync {
    fn perform_ranged_attack<'a>(
        &'a self,
        target: &'a Arc<dyn EntityBase>,
        power: f32,
    ) -> EntityBaseFuture<'a, ()>;
}

#[cfg(test)]
mod tests {
    use super::{
        MobDespawnContext, MobDespawnDecision, SunProtectionOutcome, apply_sun_protection_damage,
        goal_control_policy, is_within_home, mob_despawn_decision, should_tick_mob_ai,
    };
    use pumpkin_data::item::Item;
    use pumpkin_data::item_stack::ItemStack;
    use pumpkin_util::math::position::BlockPos;

    #[test]
    fn only_no_ai_gates_the_complete_ai_phase() {
        assert!(should_tick_mob_ai(false));
        assert!(!should_tick_mob_ai(true));
    }

    #[test]
    fn passenger_and_boat_policy_only_disables_vanilla_goal_controls() {
        assert_eq!(goal_control_policy(false, false), (true, true, true));
        assert_eq!(goal_control_policy(true, false), (false, false, false));
        assert_eq!(goal_control_policy(false, true), (true, false, true));
        assert_eq!(goal_control_policy(true, true), (false, false, false));
    }

    #[test]
    fn despawn_policy_preserves_persistence_distance_and_inactivity_ordering() {
        let base = MobDespawnContext {
            peaceful: false,
            allowed_in_peaceful: true,
            persistence_required: false,
            custom_persistence: false,
            nearest_player_distance_squared: Some(33.0 * 33.0),
            instant_despawn_distance: 128,
            no_action_time: 601,
            random_despawn_hit: false,
            remove_when_far_away: true,
        };

        assert_eq!(mob_despawn_decision(base), MobDespawnDecision::Keep);
        assert_eq!(
            mob_despawn_decision(MobDespawnContext {
                random_despawn_hit: true,
                ..base
            }),
            MobDespawnDecision::Discard
        );
        assert_eq!(
            mob_despawn_decision(MobDespawnContext {
                nearest_player_distance_squared: Some(129.0 * 129.0),
                no_action_time: 0,
                ..base
            }),
            MobDespawnDecision::Discard
        );
        assert_eq!(
            mob_despawn_decision(MobDespawnContext {
                nearest_player_distance_squared: Some(31.0 * 31.0),
                no_action_time: 601,
                random_despawn_hit: true,
                ..base
            }),
            MobDespawnDecision::ResetNoActionTime
        );
        assert_eq!(
            mob_despawn_decision(MobDespawnContext {
                persistence_required: true,
                nearest_player_distance_squared: Some(256.0 * 256.0),
                ..base
            }),
            MobDespawnDecision::ResetNoActionTime
        );
        assert_eq!(
            mob_despawn_decision(MobDespawnContext {
                peaceful: true,
                allowed_in_peaceful: false,
                persistence_required: true,
                ..base
            }),
            MobDespawnDecision::Discard
        );
    }

    #[test]
    fn sunlight_protection_blocks_fire_and_breaks_at_max_damage() {
        let mut empty = ItemStack::EMPTY.clone();
        assert_eq!(
            apply_sun_protection_damage(&mut empty, 1),
            SunProtectionOutcome::Unprotected
        );

        let mut pumpkin = ItemStack::new(1, &Item::CARVED_PUMPKIN);
        assert_eq!(
            apply_sun_protection_damage(&mut pumpkin, 1),
            SunProtectionOutcome::Protected
        );
        assert!(!pumpkin.is_empty());

        let mut helmet = ItemStack::new(1, &Item::IRON_HELMET);
        let max_damage = helmet.get_max_damage().expect("helmet has max damage");
        helmet.set_damage(max_damage - 1);
        assert_eq!(
            apply_sun_protection_damage(&mut helmet, 0),
            SunProtectionOutcome::Protected
        );
        assert_eq!(helmet.get_damage(), max_damage - 1);
        assert_eq!(
            apply_sun_protection_damage(&mut helmet, 1),
            SunProtectionOutcome::Broken
        );
        assert!(helmet.is_empty());
    }

    #[test]
    fn home_restriction_uses_negative_one_sentinel_and_strict_radius() {
        let center = BlockPos::new(10, 64, 10);
        assert!(is_within_home(center, -1, &BlockPos::new(1000, 64, 1000)));
        assert!(is_within_home(center, 5, &BlockPos::new(13, 64, 13)));
        assert!(!is_within_home(center, 5, &BlockPos::new(15, 64, 10)));
    }
}
