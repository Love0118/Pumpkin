use pumpkin_util::math::{get_section_cord, vector2::Vector2};
use std::{num::NonZero, sync::Arc};

use pumpkin_protocol::{
    bedrock::client::network_chunk_publisher_update::CNetworkChunkPublisherUpdate,
    codec::var_int::VarInt,
    java::client::play::{CCenterChunk, CRemoveEntities, CUnloadChunk},
};
use pumpkin_world::cylindrical_chunk_iterator::Cylindrical;

use crate::{
    entity::{EntityBase, player::Player},
    net::ClientPlatform,
};

pub fn get_view_distance(player: &Player) -> NonZero<u8> {
    let fallback = NonZero::new(2).unwrap_or(NonZero::<u8>::MIN);
    let Some(server) = player.world().server.upgrade() else {
        return fallback;
    };
    let max_view_distance = match player.client.as_ref() {
        ClientPlatform::Java(_) => server.advanced_config.networking.java.view_distance,
        ClientPlatform::Bedrock(_) => server.advanced_config.networking.bedrock.view_distance,
    };
    player
        .config
        .load()
        .view_distance
        .clamp(fallback, max_view_distance)
}

// Checks if the target chunk is within the view distance
// of the center chunk. Uses Chebyshev distance.
#[must_use]
#[inline]
pub fn is_within_view_distance(
    center: Vector2<i32>,
    target: Vector2<i32>,
    view_distance: i32,
) -> bool {
    (target.x - center.x).abs().max((target.y - center.y).abs()) <= view_distance
}

fn collect_unloading_entity_ids(
    entities: impl Iterator<Item = (i32, Vector2<i32>)>,
    old_tracking: Cylindrical,
    new_tracking: Cylindrical,
) -> Vec<VarInt> {
    entities
        .filter_map(|(entity_id, chunk_pos)| {
            (old_tracking.is_within_distance(chunk_pos.x, chunk_pos.y)
                && !new_tracking.is_within_distance(chunk_pos.x, chunk_pos.y))
            .then_some(entity_id.into())
        })
        .collect()
}

pub async fn update_position(player: &Arc<Player>) {
    let entity = &player.get_entity();
    let new_chunk_center = entity.chunk_pos.load();
    let old_cylindrical = player.watched_section.load();

    // This does break when a new player spawns
    // if old_cylindrical.center == new_chunk_center {
    //     return;
    // }

    let view_distance = get_view_distance(player);
    let new_cylindrical = Cylindrical::new(new_chunk_center, view_distance);

    if old_cylindrical == new_cylindrical {
        return;
    }

    match player.client.as_ref() {
        ClientPlatform::Java(java_client) => {
            java_client
                .send_packet(&CCenterChunk {
                    chunk_x: new_chunk_center.x.into(),
                    chunk_z: new_chunk_center.y.into(),
                })
                .await;
        }
        ClientPlatform::Bedrock(bedrock_client) => {
            bedrock_client
                .send_packet(&CNetworkChunkPublisherUpdate::new(
                    player.get_entity().block_pos.load(),
                    u32::from(view_distance.get()) * 16,
                ))
                .await;
        }
    }
    let (loading_iter, unloading_iter) =
        Cylindrical::changed_chunks(old_cylindrical, new_cylindrical);
    let loading_chunks: Vec<_> = loading_iter.collect();
    let unloading_chunks: Vec<_> = unloading_iter.collect();

    // Use the chunk_manager's world reference, which is updated on dimension change.
    // This ensures we load chunks from the correct world after portal teleportation.
    let world = {
        let mut chunk_manager = player.chunk_manager.lock().await;
        let world = chunk_manager.world().clone();
        chunk_manager.update_center_and_view_distance(
            new_chunk_center,
            view_distance.into(),
            &world.level,
            &loading_chunks,
            &unloading_chunks,
        );
        world
    };
    player.watched_section.store(new_cylindrical);

    if let ClientPlatform::Java(client) = player.client.as_ref() {
        let unloading_entity_ids = {
            let entities = world.entities.load();
            collect_unloading_entity_ids(
                entities.iter().map(|entity| {
                    let entity = entity.get_entity();
                    let position = entity.pos.load();
                    (
                        entity.entity_id,
                        Vector2::new(
                            get_section_cord(position.x.floor() as i32),
                            get_section_cord(position.z.floor() as i32),
                        ),
                    )
                }),
                old_cylindrical,
                new_cylindrical,
            )
        };

        if !unloading_entity_ids.is_empty() {
            client
                .enqueue_client_packet(&CRemoveEntities::new(&unloading_entity_ids))
                .await;
        }

        for chunk in &unloading_chunks {
            client
                .enqueue_client_packet(&CUnloadChunk::new(chunk.x, chunk.y))
                .await;
        }
    }

    // Make sure the watched section and the chunk watcher updates are async atomic. We want to
    // ensure what we unload when the player disconnects is correct.
    world
        .level
        .mark_chunks_as_newly_watched(&loading_chunks)
        .await;
    let chunks_to_clean = world
        .level
        .mark_chunks_as_not_watched(&unloading_chunks)
        .await;

    if !chunks_to_clean.is_empty() {
        world.remove_entities_in_chunks(&chunks_to_clean).await;
        world.level.clean_entity_chunks(&chunks_to_clean);
    }

    if !loading_chunks.is_empty() {
        world.spawn_world_entity_chunks(player.clone(), loading_chunks, new_chunk_center);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unloading_entity_ids_only_include_entities_in_chunks_leaving_view() {
        let view_distance = NonZero::new(4).unwrap();
        let old_tracking = Cylindrical::new(Vector2::new(0, 0), view_distance);
        let new_tracking = Cylindrical::new(Vector2::new(1, 0), view_distance);
        let leaving = old_tracking
            .all_chunks_within()
            .find(|chunk| !new_tracking.is_within_distance(chunk.x, chunk.y))
            .unwrap();
        let staying = old_tracking
            .all_chunks_within()
            .find(|chunk| new_tracking.is_within_distance(chunk.x, chunk.y))
            .unwrap();
        let entering = new_tracking
            .all_chunks_within()
            .find(|chunk| !old_tracking.is_within_distance(chunk.x, chunk.y))
            .unwrap();
        let entities = [(10, leaving), (11, staying), (12, entering)];

        let ids = collect_unloading_entity_ids(entities.into_iter(), old_tracking, new_tracking);

        assert_eq!(ids.iter().map(|id| id.0).collect::<Vec<_>>(), [10]);
    }
}
