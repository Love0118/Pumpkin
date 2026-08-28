#[allow(clippy::wildcard_imports)]
use super::*;

impl JavaClient {
    pub async fn handle_set_command_block(
        &self,
        player: &Arc<Player>,
        command: SSetCommandBlock<'_>,
    ) {
        if !player.is_creative() {
            return;
        }
        if player.permission_lvl.load() < PermissionLvl::Two {
            return;
        }
        let pos = command.pos;
        let block_entity = player.world().get_block_entity(&pos);
        if let Some(block_entity) = block_entity {
            if block_entity.resource_location() != CommandBlockEntity::ID {
                warn!("Client tried to change Command block but not Command block entity found");
                return;
            }

            let Ok(command_block_mode) = CommandBlockMode::try_from(command.mode) else {
                self.kick(TextComponent::text("Invalid Command block mode"))
                    .await;
                return;
            };

            let block = player.world().get_block(&pos);
            let old_state_id = player.world().get_block_state_id(&pos);
            let mut props = CommandBlockLikeProperties::from_state_id(old_state_id, block);

            let block_type = match command_block_mode {
                CommandBlockMode::Chain => Block::CHAIN_COMMAND_BLOCK,
                CommandBlockMode::Repeating => Block::REPEATING_COMMAND_BLOCK,
                CommandBlockMode::Impulse => Block::COMMAND_BLOCK,
            };

            let Some(old_command_block) =
                block_entity.as_any().downcast_ref::<CommandBlockEntity>()
            else {
                return;
            };

            props.conditional = command.is_conditional();

            let new_state_id = props.to_state_id(&block_type);
            player
                .world()
                .set_block_state(
                    &command.pos,
                    new_state_id,
                    BlockFlags::SKIP_BLOCK_ADDED_CALLBACK,
                )
                .await;

            let mut cmd = command.command;
            if cmd.starts_with('/') {
                cmd = &cmd[1..];
            }

            let command_block = CommandBlockEntity::new(
                pos,
                command.track_output(),
                block_type == Block::CHAIN_COMMAND_BLOCK,
            );
            command_block.powered.store(
                old_command_block.powered.load(Ordering::SeqCst),
                Ordering::SeqCst,
            );
            command_block.condition_met.store(
                old_command_block.condition_met.load(Ordering::SeqCst),
                Ordering::SeqCst,
            );
            command_block
                .auto
                .store(command.is_automatic(), Ordering::SeqCst);
            command_block.dirty.store(
                old_command_block.dirty.load(Ordering::SeqCst),
                Ordering::SeqCst,
            );
            command_block
                .set_command_and_last_output(cmd.to_string(), old_command_block.last_output().await)
                .await;
            player.world().add_block_entity(Arc::new(command_block));

            player
                .send_system_message(&TextComponent::text(format!(
                    "Command set: {}",
                    command.command
                )))
                .await;

            // The automatic flag means always active
            if command.is_automatic() && block_type != Block::CHAIN_COMMAND_BLOCK {
                player.world().schedule_block_tick(
                    &block_type,
                    pos,
                    1,
                    pumpkin_world::tick::TickPriority::Normal,
                );
            }
        }
    }
}
