use std::sync::atomic::Ordering;

use crate::entity::{
    DamageContext, Entity, EntityBase, EntityBaseFuture, NbtFuture, living::LivingEntity,
};
use pumpkin_nbt::compound::NbtCompound;

pub struct PaintingEntity {
    entity: Entity,
}

impl PaintingEntity {
    pub const fn new(entity: Entity) -> Self {
        Self { entity }
    }
}

impl EntityBase for PaintingEntity {
    fn write_custom_nbt(&self, nbt: &mut NbtCompound) {
        nbt.put_byte("facing", self.entity.data.load(Ordering::Relaxed) as i8);
    }

    fn read_custom_nbt<'a>(&'a self, nbt: &'a NbtCompound) -> NbtFuture<'a, ()> {
        Box::pin(async {
            let facing = nbt.get_byte("facing").unwrap_or(3);
            self.entity.data.store(facing as i32, Ordering::Relaxed);
        })
    }

    fn get_entity(&self) -> &Entity {
        &self.entity
    }

    fn get_living_entity(&self) -> Option<&LivingEntity> {
        None
    }

    fn damage_with_context<'a>(
        &'a self,
        _target: &'a dyn EntityBase,
        _context: DamageContext<'a>,
    ) -> EntityBaseFuture<'a, bool> {
        Box::pin(async {
            // TODO
            self.entity.remove().await;
            true
        })
    }

    fn cast_any(&self) -> &dyn std::any::Any {
        self
    }
}
