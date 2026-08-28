#[allow(clippy::wildcard_imports)]
use super::*;

impl JavaClient {
    pub async fn handle_set_jigsaw_block(&self, player: &Arc<Player>, jigsaw: SSetJigsawBlock<'_>) {
        if !player.is_creative() {
            return;
        }
        if player.permission_lvl.load() < PermissionLvl::Two {
            return;
        }
        let pos = jigsaw.pos;
        let block_entity = player.world().get_block_entity(&pos);
        if let Some(block_entity) = block_entity {
            if block_entity.resource_location() != JigsawBlockEntity::ID {
                warn!("Client tried to change Jigsaw block but not Jigsaw block entity found");
                return;
            }

            let Some(jigsaw_block) = block_entity.as_any().downcast_ref::<JigsawBlockEntity>()
            else {
                return;
            };

            jigsaw_block
                .update_configuration(
                    jigsaw.name.to_string(),
                    jigsaw.target.to_string(),
                    jigsaw.pool.to_string(),
                    jigsaw.final_state.to_string(),
                    JigsawJointType::from_str(jigsaw.joint),
                    jigsaw.selection_priority.0,
                    jigsaw.placement_priority.0,
                )
                .await;

            player.world().update_block_entity(&block_entity);
        }
    }
}
