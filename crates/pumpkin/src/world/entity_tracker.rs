use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
};

use pumpkin_protocol::{
    bedrock::client::remove_actor::CRemoveActor,
    codec::{var_int::VarInt, var_long::VarLong},
    java::client::play::CRemoveEntities,
};
use pumpkin_util::math::{get_section_cord, vector2::Vector2};
use rustc_hash::{FxHashMap, FxHashSet};
use uuid::Uuid;

use crate::{
    entity::{EntityBase, player::Player},
    net::ClientPlatform,
};

use super::World;

#[derive(Clone, Copy, PartialEq, Eq)]
enum PairingState {
    Pending,
    Paired,
}

pub(super) struct EntityTracker {
    entity: Arc<dyn EntityBase>,
    tracked_chunk: AtomicU64,
    viewers: Mutex<FxHashMap<Uuid, PairingState>>,
}

impl EntityTracker {
    fn new(entity: Arc<dyn EntityBase>) -> Self {
        let chunk = entity_chunk(entity.as_ref());
        Self {
            entity,
            tracked_chunk: AtomicU64::new(chunk_key(chunk)),
            viewers: Mutex::new(FxHashMap::default()),
        }
    }
}

impl World {
    pub fn register_entity_tracking(&self, entity: Arc<dyn EntityBase>) {
        let entity_id = entity.get_entity().entity_id;
        self.entity_trackers
            .insert(entity_id, EntityTracker::new(entity));
        self.update_entity_tracking_for_entity_id(entity_id, true);
    }

    pub fn update_entity_tracking_for_player(&self, player: &Arc<Player>) {
        if self.get_player_by_uuid(player.gameprofile.id).is_none() {
            return;
        }
        for tracker in &self.entity_trackers {
            self.update_pairing(tracker.value(), player);
        }
    }

    pub fn update_entity_tracking_for_entity(&self, entity: &Arc<dyn EntityBase>) {
        self.update_entity_tracking_for_entity_id(entity.get_entity().entity_id, false);
    }

    pub(crate) fn update_entity_tracking_for_entity_id(&self, entity_id: i32, force: bool) {
        let Some(tracker) = self.entity_trackers.get(&entity_id) else {
            return;
        };

        let chunk = entity_chunk(tracker.entity.as_ref());
        tracker.entity.get_entity().chunk_pos.store(chunk);
        let previous_chunk = tracker
            .tracked_chunk
            .swap(chunk_key(chunk), Ordering::Relaxed);
        if !force && previous_chunk == chunk_key(chunk) {
            return;
        }

        for player in self.players.load().iter() {
            self.update_pairing(tracker.value(), player);
        }
    }

    fn update_pairing(&self, tracker: &EntityTracker, player: &Arc<Player>) {
        let entity = tracker.entity.get_entity();
        let player_id = player.gameprofile.id;
        let should_track = !entity.is_removed()
            && player.world().uuid == self.uuid
            && player
                .delivered_chunks
                .contains(&entity_chunk(tracker.entity.as_ref()));

        let mut viewers = tracker
            .viewers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        if should_track {
            if viewers.contains_key(&player_id) {
                return;
            }
            viewers.insert(player_id, PairingState::Pending);
            drop(viewers);

            let entity_id = entity.entity_id;
            let entity_uuid = entity.entity_uuid;
            let world = entity.world.load_full();
            let Some(server) = world.server.upgrade() else {
                tracker
                    .viewers
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .remove(&player_id);
                return;
            };
            server.spawn_task(async move {
                world
                    .finish_entity_pairing(entity_id, entity_uuid, player_id)
                    .await;
            });
        } else if matches!(viewers.remove(&player_id), Some(PairingState::Paired)) {
            drop(viewers);
            send_remove_entity(player, entity.entity_id);
        }
    }

    async fn finish_entity_pairing(&self, entity_id: i32, entity_uuid: Uuid, player_id: Uuid) {
        let Some(player) = self.get_player_by_uuid(player_id) else {
            return;
        };
        let entity = {
            let Some(tracker) = self.entity_trackers.get(&entity_id) else {
                return;
            };
            if tracker.entity.get_entity().entity_uuid != entity_uuid {
                return;
            }
            if !matches!(
                tracker
                    .viewers
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .get(&player_id),
                Some(PairingState::Pending)
            ) {
                return;
            }
            tracker.entity.clone()
        };

        player.client.enqueue_spawn_packet(&entity).await;

        let should_remain_paired = !entity.get_entity().is_removed()
            && player.world().uuid == self.uuid
            && player
                .delivered_chunks
                .contains(&entity_chunk(entity.as_ref()));
        let paired = self.entity_trackers.get(&entity_id).is_some_and(|tracker| {
            if tracker.entity.get_entity().entity_uuid != entity_uuid {
                return false;
            }
            let mut viewers = tracker
                .viewers
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if should_remain_paired
                && matches!(viewers.get(&player_id), Some(PairingState::Pending))
            {
                viewers.insert(player_id, PairingState::Paired);
                true
            } else {
                viewers.remove(&player_id);
                false
            }
        });

        if paired {
            player.try_restore_vehicle(&entity).await;
        } else {
            send_remove_entity(&player, entity_id);
        }
    }

    pub fn remove_entity_tracking(&self, entity_id: i32) {
        let Some((_, tracker)) = self.entity_trackers.remove(&entity_id) else {
            return;
        };

        let viewers = tracker
            .viewers
            .into_inner()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for player_id in viewers
            .into_iter()
            .filter_map(|(id, state)| (state == PairingState::Paired).then_some(id))
        {
            if let Some(player) = self.get_player_by_uuid(player_id) {
                send_remove_entity(&player, entity_id);
            }
        }
    }

    pub fn remove_player_from_entity_tracking(&self, player: &Player) {
        let player_id = player.gameprofile.id;
        for tracker in &self.entity_trackers {
            tracker
                .viewers
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .remove(&player_id);
        }
    }

    pub(crate) fn paired_entity_viewers(&self, entity_id: i32) -> Option<FxHashSet<Uuid>> {
        let tracker = self.entity_trackers.get(&entity_id)?;
        let viewers = tracker
            .viewers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .filter_map(|(id, state)| receives_entity_packets(state).then_some(*id))
            .collect();
        Some(viewers)
    }
}

fn entity_chunk(entity: &dyn EntityBase) -> Vector2<i32> {
    let position = entity.get_entity().pos.load();
    Vector2::new(
        get_section_cord(position.x.floor() as i32),
        get_section_cord(position.z.floor() as i32),
    )
}

fn chunk_key(chunk: Vector2<i32>) -> u64 {
    (u64::from(chunk.x as u32) << 32) | u64::from(chunk.y as u32)
}

fn receives_entity_packets(state: &PairingState) -> bool {
    *state == PairingState::Paired
}

fn send_remove_entity(player: &Player, entity_id: i32) {
    match player.client.as_ref() {
        ClientPlatform::Java(java) => {
            java.try_enqueue_packet(&CRemoveEntities::new(&[VarInt(entity_id)]));
        }
        ClientPlatform::Bedrock(bedrock) => {
            bedrock.try_enqueue_packet(&CRemoveActor::new(VarLong(i64::from(entity_id))));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{PairingState, receives_entity_packets};

    #[test]
    fn pending_pairing_is_not_an_entity_packet_recipient() {
        assert!(!receives_entity_packets(&PairingState::Pending));
        assert!(receives_entity_packets(&PairingState::Paired));
    }
}
