use std::sync::{Arc, atomic::Ordering};
use tokio::sync::Mutex;

use crate::{
    entity::{
        DamageContext, Entity, EntityBase, EntityBaseFuture, NbtFuture, living::LivingEntity,
    },
    net::{bedrock::BedrockClient, java::JavaClient},
    server::Server,
};
use pumpkin_nbt::{compound::NbtCompound, tag::NbtTag};

pub struct MarkerEntity {
    pub entity: Entity,
    pub data: Mutex<NbtCompound>,
}

impl MarkerEntity {
    pub fn new(entity: Entity) -> Arc<Self> {
        entity.no_clip.store(true, Ordering::Relaxed);
        Arc::new(Self {
            entity,
            data: Mutex::new(NbtCompound::new()),
        })
    }
}

impl EntityBase for MarkerEntity {
    fn write_custom_nbt_async<'a>(&'a self, nbt: &'a mut NbtCompound) -> NbtFuture<'a, ()> {
        Box::pin(async move {
            let data = self.data.lock().await;
            if !data.is_empty() {
                nbt.put("data", NbtTag::Compound(data.clone()));
            }
        })
    }

    fn read_custom_nbt<'a>(&'a self, nbt: &'a NbtCompound) -> NbtFuture<'a, ()> {
        Box::pin(async move {
            if let Some(data) = nbt.get_compound("data") {
                *self.data.lock().await = data.clone();
            }
        })
    }

    fn tick<'a>(
        &'a self,
        _caller: &'a Arc<dyn EntityBase>,
        _server: &'a Server,
    ) -> EntityBaseFuture<'a, ()> {
        Box::pin(async move {})
    }

    fn init_data_tracker(&self) -> EntityBaseFuture<'_, ()> {
        Box::pin(async move {})
    }

    fn get_entity(&self) -> &Entity {
        &self.entity
    }

    fn get_living_entity(&self) -> Option<&LivingEntity> {
        None
    }

    fn cast_any(&self) -> &dyn std::any::Any {
        self
    }

    fn is_pushable(&self) -> bool {
        false
    }

    fn is_pushed_by_fluids(&self) -> bool {
        false
    }

    fn can_hit(&self) -> bool {
        false
    }

    fn is_immune_to_explosion(&self) -> bool {
        true
    }

    fn damage_with_context<'a>(
        &'a self,
        _target: &'a dyn EntityBase,
        _context: DamageContext<'a>,
    ) -> EntityBaseFuture<'a, bool> {
        Box::pin(async move { false })
    }

    fn send_java_spawn_packet<'a>(&'a self, _client: &'a JavaClient) -> EntityBaseFuture<'a, ()> {
        Box::pin(async move {})
    }

    fn send_bedrock_spawn_packet<'a>(
        &'a self,
        _client: &'a BedrockClient,
    ) -> EntityBaseFuture<'a, ()> {
        Box::pin(async move {})
    }
}
