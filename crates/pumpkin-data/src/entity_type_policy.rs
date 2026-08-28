use crate::entity_type::EntityType;

const NOT_ALLOWED_IN_PEACEFUL: [&EntityType; 38] = [
    &EntityType::BLAZE,
    &EntityType::BOGGED,
    &EntityType::BREEZE,
    &EntityType::CAVE_SPIDER,
    &EntityType::CREAKING,
    &EntityType::CREEPER,
    &EntityType::DROWNED,
    &EntityType::ELDER_GUARDIAN,
    &EntityType::ENDERMAN,
    &EntityType::ENDERMITE,
    &EntityType::EVOKER,
    &EntityType::GHAST,
    &EntityType::GIANT,
    &EntityType::GUARDIAN,
    &EntityType::HOGLIN,
    &EntityType::HUSK,
    &EntityType::ILLUSIONER,
    &EntityType::MAGMA_CUBE,
    &EntityType::PARCHED,
    &EntityType::PHANTOM,
    &EntityType::PIGLIN_BRUTE,
    &EntityType::PILLAGER,
    &EntityType::RAVAGER,
    &EntityType::SILVERFISH,
    &EntityType::SKELETON,
    &EntityType::SLIME,
    &EntityType::SPIDER,
    &EntityType::STRAY,
    &EntityType::VEX,
    &EntityType::VINDICATOR,
    &EntityType::WARDEN,
    &EntityType::WITCH,
    &EntityType::WITHER,
    &EntityType::WITHER_SKELETON,
    &EntityType::ZOGLIN,
    &EntityType::ZOMBIE,
    &EntityType::ZOMBIE_VILLAGER,
    &EntityType::ZOMBIFIED_PIGLIN,
];

impl EntityType {
    /// Whether this 26.2 entity type may exist while the world difficulty is Peaceful.
    #[must_use]
    pub fn is_allowed_in_peaceful(&self) -> bool {
        !NOT_ALLOWED_IN_PEACEFUL
            .iter()
            .any(|disallowed| *disallowed == self)
    }
}

#[cfg(test)]
mod tests {
    use super::EntityType;

    #[test]
    fn peaceful_policy_preserves_hostile_exceptions() {
        assert!(!EntityType::BLAZE.is_allowed_in_peaceful());
        assert!(!EntityType::ZOMBIFIED_PIGLIN.is_allowed_in_peaceful());
        assert!(EntityType::PIGLIN.is_allowed_in_peaceful());
        assert!(EntityType::ENDER_DRAGON.is_allowed_in_peaceful());
        assert!(EntityType::COW.is_allowed_in_peaceful());
    }
}
