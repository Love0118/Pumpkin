#[allow(clippy::wildcard_imports)]
use super::*;
use pumpkin_nbt::{Nbt, compound::NbtCompound};
use pumpkin_protocol::java::{
    client::play::CTagQueryResponse,
    server::play::{SBlockEntityTagQuery, SEntityTagQuery},
};
use tracing::error;

impl JavaClient {
    pub async fn handle_block_entity_tag_query(
        &self,
        player: &Player,
        packet: SBlockEntityTagQuery,
    ) {
        if player.permission_lvl.load() < PermissionLvl::Two {
            return;
        }

        let mut compound = NbtCompound::new();
        let block_entity = player.world().get_block_entity(&packet.location);
        if let Some(block_entity) = block_entity {
            block_entity.write_nbt(&mut compound).await;
        }

        let nbt_bytes = match Nbt::new(String::new(), compound).write_unnamed() {
            Ok(bytes) => bytes,
            Err(serialization_error) => {
                error!(
                    "Failed to serialize block entity tag query response: {serialization_error}"
                );
                return;
            }
        };
        self.send_packet(&CTagQueryResponse::new(packet.transaction_id, &nbt_bytes))
            .await;
    }

    pub async fn handle_entity_tag_query(&self, player: &Player, packet: SEntityTagQuery) {
        if player.permission_lvl.load() < PermissionLvl::Two {
            return;
        }

        let mut compound = NbtCompound::new();
        let entity = player.world().get_entity_by_id(packet.entity_id.0);
        if let Some(entity) = entity {
            entity.write_nbt(&mut compound).await;
        }

        let nbt_bytes = match Nbt::new(String::new(), compound).write_unnamed() {
            Ok(bytes) => bytes,
            Err(serialization_error) => {
                error!("Failed to serialize entity tag query response: {serialization_error}");
                return;
            }
        };
        self.send_packet(&CTagQueryResponse::new(packet.transaction_id, &nbt_bytes))
            .await;
    }
}
