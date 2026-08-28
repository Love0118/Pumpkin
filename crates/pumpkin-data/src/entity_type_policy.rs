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

    /// Vanilla stores tracking range in chunks while player-distance checks use blocks.
    #[must_use]
    pub const fn client_tracking_range_blocks(&self) -> i32 {
        self.client_tracking_range.saturating_mul(16)
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

    #[test]
    fn tracking_policy_covers_the_complete_registry() {
        assert_eq!(EntityType::ALL.len(), 158);
        for entity_type in EntityType::ALL {
            assert!((0..=32).contains(&entity_type.client_tracking_range));
            assert!(entity_type.update_interval > 0);
        }
    }

    #[test]
    fn tracking_policy_matches_vanilla_special_cases() {
        assert_eq!(EntityType::COD.client_tracking_range, 4);
        assert_eq!(EntityType::ITEM.client_tracking_range, 6);
        assert_eq!(EntityType::ITEM.update_interval, 20);
        assert_eq!(EntityType::PLAYER.client_tracking_range, 32);
        assert_eq!(EntityType::PLAYER.update_interval, 2);
        assert!(!EntityType::PLAYER.track_deltas);
        assert!(!EntityType::WITHER.track_deltas);
        assert!(EntityType::COD.track_deltas);
        assert_eq!(EntityType::COD.client_tracking_range_blocks(), 64);
    }
}
