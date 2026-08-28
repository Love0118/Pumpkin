mod blob_cache;
pub mod nethernet;
pub mod play;
pub mod status;
use crossbeam::atomic::AtomicCell;
use std::{
    collections::HashMap,
    io::{Cursor, Error, Write},
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
    },
};

use tracing::{debug, error, warn};

use bytes::Bytes;
use futures::{StreamExt, stream};
use pumpkin_config::networking::compression::CompressionInfo;
use pumpkin_protocol::{
    BClientPacket, PacketDecodeError, RawPacket,
    bedrock::{
        BEDROCK_GAME_PACKET, SubClient,
        client::{
            client_cache_miss_response::{CClientCacheMissResponse, CacheBlob},
            disconnect_player::CDisconnectPlayer,
            level_chunk::CLevelChunk,
        },
        packet_decoder::BedrockBatchDecoder,
        packet_encoder::BedrockBatchEncoder,
        server::{
            actor_event::SActorEvent, animate::SAnimate, block_pick_request::SBlockPickRequest,
            client_cache_blob_status::SClientCacheBlobStatus,
            client_cache_status::SClientCacheStatus, command_request::SCommandRequest,
            container_close::SContainerClose, emote::SEmote, emote_list::SEmoteList,
            interaction::SInteraction, inventory_transaction::SInventoryTransaction,
            loading_screen::SLoadingScreen, login::SLogin, mob_equipment::SMobEquipment,
            packet_violation_warning::SPacketViolationWarning, player_action::SPlayerAction,
            player_auth_input::SPlayerAuthInput, request_ability::SRequestAbility,
            request_chunk_radius::SRequestChunkRadius,
            request_network_settings::SRequestNetworkSettings,
            resource_pack_response::SResourcePackResponse, respawn::SRespawn,
            set_local_player_as_initialized::SSetLocalPlayerAsInitialized,
            set_player_inventory_options::SSetPlayerInventoryOptions, text::SText,
        },
    },
    packet::Packet,
    serial::{PacketRead, PacketReadSlice},
};
use tokio::{
    sync::mpsc::{Receiver, Sender},
    sync::{Mutex, RwLock, oneshot},
    task::JoinHandle,
};

use tokio_util::{sync::CancellationToken, task::TaskTracker};

pub mod login;
use self::nethernet::NetherNetSession;
use crate::{
    entity::player::Player,
    net::{
        DisconnectReason, PacketHandlerResult, PacketRateLimiter,
        outbound::{
            ByteBudgetError, OutboundByteBudget, OutboundBytePermit, PriorityBurst,
            StatePacketKind, StateRecoveryKey, StateRecoveryQueue, record_outbound_drop,
            record_outbound_resnapshot,
        },
    },
    plugin::api::events::world::chunk_send::ChunkSend,
    server::Server,
};
use arc_swap::ArcSwap;
use blob_cache::BlobCache;
use pumpkin_protocol::bedrock::server::login::ClientData;
use pumpkin_util::version::BedrockMinecraftVersion;
use pumpkin_world::level::SyncChunk;

struct OutgoingPacket {
    data: Bytes,
    completion: Option<oneshot::Sender<()>>,
    _budget: OutboundBytePermit,
}

impl OutgoingPacket {
    const fn normal(data: Bytes, budget: OutboundBytePermit) -> Self {
        Self {
            data,
            completion: None,
            _budget: budget,
        }
    }

    const fn priority(
        data: Bytes,
        completion: oneshot::Sender<()>,
        budget: OutboundBytePermit,
    ) -> Self {
        Self {
            data,
            completion: Some(completion),
            _budget: budget,
        }
    }
}

pub struct BedrockClient {
    session: Arc<NetherNetSession>,
    /// The client's IP address.
    pub address: SocketAddr,
    pub player: ArcSwap<Option<Arc<Player>>>,
    pub version: AtomicCell<BedrockMinecraftVersion>,
    pub client_data: ArcSwap<Option<Arc<ClientData>>>,
    /// All Bedrock clients
    /// This list is used to remove the client if the connection gets closed
    pub be_clients: Arc<Mutex<HashMap<SocketAddr, Arc<Self>>>>,

    tasks: TaskTracker,
    rt_handle: tokio::runtime::Handle,
    outgoing_packet_queue_send: Sender<OutgoingPacket>,
    /// A queue of serialized packets to send to the network
    outgoing_packet_queue_recv: Mutex<Option<Receiver<OutgoingPacket>>>,

    outgoing_packet_priority_send: Sender<OutgoingPacket>,
    outgoing_packet_priority_recv: Mutex<Option<Receiver<OutgoingPacket>>>,
    outgoing_byte_budget: OutboundByteBudget,
    state_recovery: Arc<StateRecoveryQueue>,
    state_recovery_sequence: AtomicU64,

    /// The packet encoder for outgoing packets.
    network_writer: Arc<RwLock<BedrockBatchEncoder>>,
    /// The packet decoder for incoming packets.
    network_reader: Mutex<BedrockBatchDecoder>,

    /// The next form ID to use for custom forms.
    pub next_form_id: AtomicU32,
    pub inventory_opened: AtomicBool,
    pub client_cache_supported: AtomicBool,
    blob_cache: Mutex<BlobCache>,
    /// An notifier that is triggered when this client is closed.
    close_token: CancellationToken,
    last_seen: Arc<AtomicCell<std::time::Instant>>,
    incoming_game_packet_send: Sender<RawPacket>,
    incoming_game_packet_recv: Mutex<Option<Receiver<RawPacket>>>,
    /// Packet rate limiter for incoming client packets.
    pub packet_limiter: PacketRateLimiter,
}

impl BedrockClient {
    #[must_use]
    pub fn new(
        session: Arc<NetherNetSession>,
        address: SocketAddr,
        be_clients: Arc<Mutex<HashMap<SocketAddr, Arc<Self>>>>,
        packet_limiter: PacketRateLimiter,
    ) -> Self {
        let (send, recv) = tokio::sync::mpsc::channel(4096);
        let (priority_send, priority_recv) = tokio::sync::mpsc::channel(4096);
        let (incoming_send, incoming_recv) = tokio::sync::mpsc::channel(4096);
        let rt_handle = tokio::runtime::Handle::current();
        Self {
            session,
            player: ArcSwap::new(Arc::new(None)),
            address,
            version: AtomicCell::new(BedrockMinecraftVersion::Unknown),
            client_data: ArcSwap::new(Arc::new(None)),
            be_clients,
            network_writer: Arc::new(RwLock::new(BedrockBatchEncoder::new())),
            network_reader: Mutex::new(BedrockBatchDecoder::new()),
            tasks: TaskTracker::new(),
            rt_handle,
            outgoing_packet_queue_send: send,
            outgoing_packet_queue_recv: Mutex::new(Some(recv)),
            outgoing_packet_priority_send: priority_send,
            outgoing_packet_priority_recv: Mutex::new(Some(priority_recv)),
            outgoing_byte_budget: OutboundByteBudget::default(),
            state_recovery: Arc::new(StateRecoveryQueue::default()),
            state_recovery_sequence: AtomicU64::new(0),
            next_form_id: AtomicU32::new(0),
            inventory_opened: AtomicBool::new(false),
            client_cache_supported: AtomicBool::new(false),
            blob_cache: Mutex::new(BlobCache::default()),
            close_token: CancellationToken::new(),
            last_seen: Arc::new(AtomicCell::new(std::time::Instant::now())),
            incoming_game_packet_send: incoming_send,
            incoming_game_packet_recv: Mutex::new(Some(incoming_recv)),
            packet_limiter,
        }
    }

    pub async fn get_packet(&self) -> Option<RawPacket> {
        let mut guard = self.incoming_game_packet_recv.lock().await;
        let recv = guard.as_mut()?;
        tokio::select! {
            () = self.await_close_interrupt() => None,
            packet = recv.recv() => packet,
        }
    }

    pub fn start_outgoing_packet_task(self: &Arc<Self>) {
        enum Selection {
            Packet(OutgoingPacket, bool),
            Tick,
            Closed,
        }

        let recovery_client = self.clone();
        self.spawn_task(async move {
            'recovery: loop {
                let batch = tokio::select! {
                    () = recovery_client.close_token.cancelled() => break,
                    batch = recovery_client.state_recovery.next_batch() => batch,
                };
                for packet_data in batch {
                    let Some(permit) = recovery_client
                        .reserve_outgoing_bytes(packet_data.len())
                        .await
                    else {
                        break 'recovery;
                    };
                    if recovery_client
                        .outgoing_packet_queue_send
                        .send(OutgoingPacket::normal(packet_data, permit))
                        .await
                        .is_err()
                    {
                        break 'recovery;
                    }
                    record_outbound_resnapshot();
                }
                recovery_client.state_recovery.finish_batch();
            }
        });

        let client = self.clone();
        self.spawn_task(async move {
            let Some(mut packet_receiver) = client.outgoing_packet_queue_recv.lock().await.take()
            else {
                return;
            };
            let Some(mut priority_packet_receiver) =
                client.outgoing_packet_priority_recv.lock().await.take()
            else {
                return;
            };
            let mut interval = tokio::time::interval(std::time::Duration::from_millis(100));
            let mut priority_burst = PriorityBurst::default();

            while !client.close_token.is_cancelled() {
                let selection = if priority_burst.prefer_normal() {
                    tokio::select! {
                        biased;
                        () = client.close_token.cancelled() => Selection::Closed,
                        _ = interval.tick() => Selection::Tick,
                        res = packet_receiver.recv() => res.map_or(Selection::Closed, |packet| Selection::Packet(packet, false)),
                        res = priority_packet_receiver.recv() => res.map_or(Selection::Closed, |packet| Selection::Packet(packet, true)),
                    }
                } else {
                    tokio::select! {
                        biased;
                        () = client.close_token.cancelled() => Selection::Closed,
                        _ = interval.tick() => Selection::Tick,
                        res = priority_packet_receiver.recv() => res.map_or(Selection::Closed, |packet| Selection::Packet(packet, true)),
                        res = packet_receiver.recv() => res.map_or(Selection::Closed, |packet| Selection::Packet(packet, false)),
                    }
                };
                let packet = match selection {
                    Selection::Packet(packet, true) => {
                        priority_burst.record_priority();
                        packet
                    }
                    Selection::Packet(packet, false) => {
                        priority_burst.record_normal();
                        packet
                    }
                    Selection::Tick => {
                        if !client.tick_connection().await {
                            break;
                        }
                        continue;
                    }
                    Selection::Closed => break,
                };

                if packet.data.first() != Some(&BEDROCK_GAME_PACKET) {
                    warn!("Refusing to send a non-game packet over NetherNet");
                    continue;
                }
                let data = packet.data.slice(1..);
                if let Err(error) = client.session.send(data).await {
                    warn!(
                        "Failed to send NetherNet packet to {}: {error}",
                        client.address
                    );
                    client.close().await;
                }

                if let Some(completion) = packet.completion {
                    let _ = completion.send(());
                }
            }
        });
    }

    async fn tick_connection(&self) -> bool {
        if self.last_seen.load().elapsed() > std::time::Duration::from_secs(10) {
            debug!("Bedrock client {} timed out", self.address);
            self.close().await;
            return false;
        }
        true
    }

    pub async fn process_nethernet_packet(self: &Arc<Self>, server: &Arc<Server>, packet: Bytes) {
        self.last_seen.store(std::time::Instant::now());
        let mut batch = Vec::with_capacity(packet.len() + 1);
        batch.push(BEDROCK_GAME_PACKET);
        batch.extend_from_slice(&packet);
        if let Err(error) = self.process_batch(server, batch).await {
            error!(
                "Failed to handle NetherNet payload for {}: {error}",
                self.address
            );
            self.kick(DisconnectReason::BadPacket, error.to_string())
                .await;
        }
    }

    pub fn nethernet_public_key(&self) -> Option<&pumpkin_util::p384::PublicKey> {
        self.session.client_public_key()
    }

    pub async fn set_compression(&self, compression: CompressionInfo) {
        self.network_reader
            .lock()
            .await
            .set_compression(compression.threshold as usize);

        self.network_writer
            .write()
            .await
            .set_compression((compression.threshold as usize, compression.level));
    }

    pub async fn kick(&self, reason: DisconnectReason, message: String) {
        self.send_packet(&CDisconnectPlayer::new(reason as i32, message))
            .await;
        self.close().await;
    }

    pub async fn kick_explicit(
        &self,
        reason: DisconnectReason,
        message: String,
        skip_message: bool,
        filtered_message: String,
        send_packet: bool,
    ) {
        if send_packet {
            self.send_packet(&CDisconnectPlayer {
                reason: pumpkin_protocol::codec::var_int::VarInt(reason as i32),
                skip_message,
                message,
                filtered_message,
            })
            .await;
        }
        self.close().await;
    }

    pub async fn send_chunks(&self, chunks: &[SyncChunk]) {
        let player = self.player.load_full();
        let Some(player) = player.as_ref() else {
            debug!(
                "send_chunks: player not set yet, dropping {} chunks",
                chunks.len()
            );
            return;
        };
        let Some(server) = player.world().server.upgrade() else {
            return;
        };

        let mut valid_chunks = Vec::with_capacity(chunks.len());
        for chunk in chunks {
            let mut event = ChunkSend::new(player.world(), chunk.clone());
            server.plugin_manager.fire(&server, &mut event).await;
            if !event.cancelled {
                valid_chunks.push(chunk.clone());
            }
        }

        if valid_chunks.is_empty() {
            return;
        }

        let bedrock_dimension =
            if player.world().dimension == pumpkin_data::dimension::Dimension::THE_NETHER {
                1
            } else if player.world().dimension == pumpkin_data::dimension::Dimension::THE_END {
                2
            } else {
                0
            };

        let cache_enabled = server.advanced_config.networking.bedrock.chunk_caching
            && self.client_cache_supported.load(Ordering::Relaxed);

        let world = player.world();
        let encode_jobs = valid_chunks.into_iter().map(|chunk| {
            let block_actors = world.bedrock_chunk_block_actors(&chunk);
            tokio::task::spawn_blocking(move || {
                CLevelChunk::encode_chunk(&chunk, bedrock_dimension, cache_enabled, &block_actors)
            })
        });
        let mut encoded_chunks = stream::iter(encode_jobs).buffered(4);

        while let Some(result) = encoded_chunks.next().await {
            match result {
                Ok(Ok((payload, blobs))) => {
                    if !blobs.is_empty() {
                        let mut cache = self.blob_cache.lock().await;
                        for (hash, payload) in blobs {
                            if !cache.insert(hash, payload) {
                                warn!(
                                    "Skipping Bedrock chunk blob {hash:#x}: larger than cache budget"
                                );
                            }
                        }
                    }

                    let packet = {
                        let encoder = self.network_writer.read().await;
                        let mut packet_buf = Vec::new();
                        match encoder.write_game_packet(
                            CLevelChunk::PACKET_ID as u16,
                            SubClient::Main,
                            SubClient::Main,
                            &payload,
                            &mut packet_buf,
                        ) {
                            Ok(()) => Some(Bytes::from(packet_buf)),
                            Err(error) => {
                                error!("Failed to write game packet wrapper: {error}");
                                None
                            }
                        }
                    };
                    if let Some(packet) = packet {
                        self.enqueue_packet_data(packet).await;
                    }
                }
                Ok(Err(error)) => error!("Failed to serialize Bedrock chunk: {error:?}"),
                Err(error) => error!("Join error in Bedrock chunk serialization: {error:?}"),
            }
        }
    }

    pub fn set_player(&self, player: Arc<Player>) {
        self.player.store(Arc::new(Some(player)));
    }

    pub async fn enqueue_packet(&self, packet_data: Bytes) {
        self.enqueue_packet_data(packet_data).await;
    }

    pub fn try_enqueue_packet(&self, packet_data: Bytes) {
        self.try_enqueue_packet_data(packet_data);
    }

    /// Queues a clientbound packet to be sent to the connected client. Queued chunks are sent
    /// in-order to the client
    ///
    /// # Arguments
    ///
    /// * `packet_data`: A `Bytes` payload representing the encoded packet.
    pub async fn enqueue_packet_data(&self, packet_data: Bytes) {
        let Some(budget) = self.reserve_outgoing_bytes(packet_data.len()).await else {
            return;
        };
        if let Err(err) = self
            .outgoing_packet_queue_send
            .send(OutgoingPacket::normal(packet_data, budget))
            .await
        {
            // This is expected to fail if we are closed
            if !self.is_closed() {
                error!("Failed to add packet to the outgoing packet queue for client: {err}");
            }
        }
    }

    pub fn try_enqueue_packet_data(&self, packet_data: Bytes) {
        let _ = self.try_enqueue_packet_data_inner(packet_data);
    }

    fn try_enqueue_packet_data_inner(&self, packet_data: Bytes) -> bool {
        let Some(budget) = self.try_reserve_outgoing_bytes(packet_data.len()) else {
            return false;
        };
        match self
            .outgoing_packet_queue_send
            .try_send(OutgoingPacket::normal(packet_data, budget))
        {
            Ok(()) => true,
            Err(err) => {
                match err {
                    tokio::sync::mpsc::error::TrySendError::Full(_) => {
                        record_outbound_drop();
                        debug!(
                            "Failed to add packet to the outgoing packet queue for client: channel full"
                        );
                    }
                    tokio::sync::mpsc::error::TrySendError::Closed(_) => {
                        if !self.is_closed() {
                            error!(
                                "Failed to add packet to the outgoing packet queue for client: channel closed"
                            );
                        }
                    }
                }
                false
            }
        }
    }

    pub(crate) fn try_enqueue_state_packet(&self, kind: StatePacketKind, packet_data: Bytes) {
        let sequence = self.state_recovery_sequence.fetch_add(1, Ordering::Relaxed);
        let key = StateRecoveryKey::for_packet(kind, sequence);
        if self
            .state_recovery
            .route(key, sequence, packet_data, |packet_data| {
                self.try_enqueue_packet_data_inner(packet_data) || self.is_closed()
            })
            .is_err()
        {
            warn!(
                "Closing Bedrock client {} because the bounded state recovery queue is full",
                self.address
            );
            self.close_token.cancel();
        }
    }

    pub fn write_raw_packet<P: BClientPacket>(
        packet: &P,
        mut writer: impl Write,
    ) -> Result<(), Error> {
        writer.write_all(&[P::PACKET_ID as u8])?;
        packet.write_packet(writer)
    }

    pub async fn write_game_packet<P: BClientPacket>(
        &self,
        packet: &P,
        write: impl Write,
    ) -> Result<(), Error> {
        let mut packet_payload = Vec::new();
        packet.write_packet(&mut packet_payload)?;

        let encoder = self.network_writer.read().await;
        encoder.write_game_packet(
            P::PACKET_ID as u16,
            SubClient::Main,
            SubClient::Main,
            &packet_payload,
            write,
        )
    }

    pub fn serialize_packet<P: BClientPacket>(&self, packet: &P) -> Result<Bytes, Error> {
        self.network_writer
            .try_read()
            .map_err(|_| Error::other("Bedrock packet encoder is busy"))?
            .serialize_packet(packet)
    }

    pub async fn send_packet<P: BClientPacket>(&self, packet: &P) {
        let mut data = Vec::new();
        match self.write_game_packet(packet, &mut data).await {
            Ok(()) => self.send_game_packet(data.into()).await,
            Err(err) => error!("Failed to serialize Bedrock packet: {err}"),
        }
    }

    pub async fn enqueue_client_packet<P: BClientPacket>(&self, packet: &P) {
        let mut data = Vec::new();
        match self.write_game_packet(packet, &mut data).await {
            Ok(()) => self.enqueue_packet(data.into()).await,
            Err(err) => error!("Failed to serialize Bedrock packet: {err}"),
        }
    }

    pub async fn send_game_packet(&self, packet_data: Bytes) {
        let Some(budget) = self.reserve_outgoing_bytes(packet_data.len()).await else {
            return;
        };
        let (tx, rx) = oneshot::channel();
        if let Err(err) = self
            .outgoing_packet_priority_send
            .send(OutgoingPacket::priority(packet_data, tx, budget))
            .await
        {
            if !self.is_closed() {
                error!("Failed to add priority packet to the outgoing packet queue: {err}");
            }
        } else {
            let _ = rx.await;
        }
    }

    async fn reserve_outgoing_bytes(&self, bytes: usize) -> Option<OutboundBytePermit> {
        let result = tokio::select! {
            () = self.close_token.cancelled() => return None,
            result = self.outgoing_byte_budget.reserve(bytes) => result,
        };
        match result {
            Ok(permit) => Some(permit),
            Err(ByteBudgetError::Oversized { bytes, limit }) => {
                warn!(
                    "Refusing oversized Bedrock packet for {}: {bytes} bytes exceeds {limit}",
                    self.address
                );
                self.close().await;
                None
            }
            Err(ByteBudgetError::Closed | ByteBudgetError::Full) => None,
        }
    }

    fn try_reserve_outgoing_bytes(&self, bytes: usize) -> Option<OutboundBytePermit> {
        match self.outgoing_byte_budget.try_reserve(bytes) {
            Ok(permit) => Some(permit),
            Err(ByteBudgetError::Full) => {
                debug!(
                    "Dropping outgoing Bedrock packet for {}: queued byte budget is full",
                    self.address
                );
                None
            }
            Err(ByteBudgetError::Oversized { bytes, limit }) => {
                warn!(
                    "Refusing oversized Bedrock packet for {}: {bytes} bytes exceeds {limit}",
                    self.address
                );
                self.close_token.cancel();
                None
            }
            Err(ByteBudgetError::Closed) => None,
        }
    }

    pub async fn close(&self) {
        if self.close_token.is_cancelled() {
            return;
        }
        self.close_token.cancel();
        self.session.close().await;
        self.be_clients.lock().await.remove(&self.address);
    }

    pub async fn await_tasks(&self) {
        self.tasks.close();
        self.tasks.wait().await;
    }

    pub fn is_closed(&self) -> bool {
        self.close_token.is_cancelled() || self.session.is_closed()
    }

    pub fn enqueue_spawn_packet(self: &Arc<Self>, entity: Arc<dyn crate::entity::EntityBase>) {
        let client = self.clone();
        self.spawn_task(async move {
            entity.send_bedrock_spawn_packet(&client).await;
        });
    }

    async fn process_batch(
        self: &Arc<Self>,
        server: &Arc<Server>,
        payload: Vec<u8>,
    ) -> Result<(), Error> {
        let decompressed_payload = self
            .get_packet_payload(payload)
            .await
            .ok_or_else(|| Error::other("Failed to decompress game packet batch"))?;
        let mut cursor = Cursor::new(decompressed_payload);

        while (cursor.position() as usize) < cursor.get_ref().len() {
            let game_packet = self
                .network_reader
                .lock()
                .await
                .get_game_packet(&mut cursor)
                .map_err(|e| Error::other(e.to_string()))?;

            if !self.packet_limiter.check_packet() {
                warn!(
                    "Bedrock client {} exceeded packet rate limit (rate: {}/s)",
                    self.address,
                    self.packet_limiter.max_rate()
                );
                self.kick(
                    DisconnectReason::Kicked,
                    server
                        .advanced_config
                        .networking
                        .bedrock
                        .packet_limiter
                        .kick_message
                        .clone(),
                )
                .await;
                return Err(Error::other("Packet rate limit exceeded"));
            }

            self.handle_game_packet(server, game_packet).await?;
        }

        Ok(())
    }

    async fn handle_game_packet(
        &self,
        _server: &Arc<Server>,
        packet: RawPacket,
    ) -> Result<(), Error> {
        if let Err(err) = self.incoming_game_packet_send.send(packet).await {
            debug!("Failed to send game packet to session task: {err}");
        }
        Ok(())
    }

    pub async fn handle_login_sequence(
        self: &Arc<Self>,
        server: &Arc<Server>,
    ) -> PacketHandlerResult {
        while let Some(packet) = self.get_packet().await {
            let payload = &mut Cursor::new(&packet.payload);
            match packet.id {
                SRequestNetworkSettings::PACKET_ID => {
                    let packet = match SRequestNetworkSettings::read(payload) {
                        Ok(p) => p,
                        Err(err) => {
                            error!("Failed to read SRequestNetworkSettings: {err}");
                            continue;
                        }
                    };
                    self.handle_request_network_settings(packet, server).await;
                }
                SLogin::PACKET_ID => {
                    let packet = match SLogin::read(payload) {
                        Ok(p) => p,
                        Err(err) => {
                            error!("Failed to read SLogin: {err}");
                            self.kick(DisconnectReason::BadPacket, err.to_string())
                                .await;
                            return PacketHandlerResult::Stop;
                        }
                    };
                    match self.handle_login(packet, server).await {
                        Ok(result) => return result,
                        Err(err) => {
                            self.kick(DisconnectReason::Unknown, err.to_string()).await;
                            return PacketHandlerResult::Stop;
                        }
                    }
                }
                _ => {
                    debug!(
                        "Received unexpected game packet {} during login sequence",
                        packet.id
                    );
                }
            }
        }
        PacketHandlerResult::Stop
    }

    pub async fn progress_player_packets(
        self: &Arc<Self>,
        player: &Arc<Player>,
        server: &Arc<Server>,
    ) {
        while let Some(packet) = self.get_packet().await {
            let mut event = crate::plugin::server::packet::PacketReceivedEvent::new(
                player.clone(),
                packet.id,
                packet.payload.clone(),
            );
            server.plugin_manager.fire(server, &mut event).await;
            if event.cancelled {
                continue;
            }

            if let Err(err) = self.handle_play_packet(player, server, packet).await {
                error!("Failed to handle Bedrock play packet: {err}");
            }
        }
    }

    #[allow(clippy::too_many_lines)]
    pub async fn handle_play_packet(
        &self,
        player: &Arc<Player>,
        server: &Arc<Server>,
        packet: RawPacket,
    ) -> Result<(), Error> {
        let payload = &packet.payload[..];
        let reader = &mut &payload[..];
        match packet.id {
            SClientCacheStatus::PACKET_ID => {
                let packet = SClientCacheStatus::read(reader)?;
                self.client_cache_supported
                    .store(packet.cache_supported, Ordering::Relaxed);
            }
            SClientCacheBlobStatus::PACKET_ID => {
                self.handle_client_cache_blob_status(SClientCacheBlobStatus::read(reader)?)
                    .await;
            }
            SResourcePackResponse::PACKET_ID => {
                self.handle_resource_pack_response(SResourcePackResponse::read(reader)?, server)
                    .await;
            }
            SPlayerAuthInput::PACKET_ID => {
                self.handle_player_auth_input(player, SPlayerAuthInput::read(reader)?, server)
                    .await;
            }
            SRequestChunkRadius::PACKET_ID => {
                self.handle_request_chunk_radius(player, SRequestChunkRadius::read(reader)?)
                    .await;
            }
            SInventoryTransaction::PACKET_ID => {
                self.handle_inventory_action(player, SInventoryTransaction::read(reader)?).await;
            }
            pumpkin_protocol::bedrock::server::item_stack_request::SItemStackRequest::PACKET_ID => {
                self.handle_item_stack_request(player, pumpkin_protocol::bedrock::server::item_stack_request::SItemStackRequest::read(reader)?).await;
            }
            SInteraction::PACKET_ID => {
                self.handle_interaction(player, SInteraction::read(reader)?, server)
                    .await;
            }
            SContainerClose::PACKET_ID => {
                self.handle_container_close(player, SContainerClose::read(reader)?)
                    .await;
            }
            SText::PACKET_ID => {
                self.handle_chat_message(server, player, SText::read_slice(reader)?)
                    .await;
            }
            SCommandRequest::PACKET_ID => {
                self.handle_chat_command(player, server, SCommandRequest::read_slice(reader)?)
                    .await;
            }
            SSetLocalPlayerAsInitialized::PACKET_ID => {
                self.handle_set_local_player_as_initialized(
                    player,
                    &SSetLocalPlayerAsInitialized::read(reader)?,
                );
            }
            SSetPlayerInventoryOptions::PACKET_ID => {
                let _ = SSetPlayerInventoryOptions::read(reader)?;
                // Ignore for now
            }
            SPlayerAction::PACKET_ID => {
                self.handle_player_action(player, server, SPlayerAction::read(reader)?)
                    .await;
            }
            SRespawn::PACKET_ID => {
                self.handle_respawn(player, SRespawn::read(reader)?).await;
            }
            SAnimate::PACKET_ID => {
                self.handle_animate(player, server, &SAnimate::read(reader)?).await;
            }
            SActorEvent::PACKET_ID => {
                self.handle_actor_event(player, SActorEvent::read(reader)?).await;
            }
            SEmote::PACKET_ID => {
                self.handle_emote(player, server, SEmote::read_slice(reader)?).await;
            }
            SEmoteList::PACKET_ID => {
                self.handle_emote_list(player, server, &SEmoteList::read(reader)?);
            }
            pumpkin_protocol::bedrock::server::modal_form_response::SModalFormResponse::PACKET_ID => {
                self.handle_modal_form_response(
                    player,
                    server,
                    pumpkin_protocol::bedrock::server::modal_form_response::SModalFormResponse::read_slice(
                        reader,
                    )?,
                )
                .await;
            }
            SLoadingScreen::PACKET_ID => {
                // Ignore for now
            }
            SBlockPickRequest::PACKET_ID => {
                self.handle_block_pick_request(player, SBlockPickRequest::read(reader)?)
                    .await;
            }
            SRequestAbility::PACKET_ID => {
                self.handle_request_ability(player, SRequestAbility::read(reader)?)
                    .await;
            }
            SMobEquipment::PACKET_ID => {
                self.handle_mob_equipment(server, player, SMobEquipment::read(reader)?)
                    .await;
            }
            SPacketViolationWarning::PACKET_ID => {
                let warning = SPacketViolationWarning::read(reader)?;
                warn!(
                    violation_type = warning.violation_type.0,
                    severity = warning.severity.0,
                    packet_id = warning.packet_id.0,
                    context = %warning.context,
                    "Bedrock client rejected a server packet"
                );
            }
            _ => {
                warn!("Bedrock: Received Unknown Game packet: {}", packet.id);
            }
        }
        Ok(())
    }

    pub async fn handle_client_cache_blob_status(&self, packet: SClientCacheBlobStatus) {
        const MAX_RESPONSE_PAYLOAD_BYTES: usize = 8 * 1024 * 1024;

        if packet.miss_hashes.is_empty() {
            return;
        }
        let response_batches = {
            let mut cache = self.blob_cache.lock().await;
            let mut batches = Vec::new();
            let mut batch = Vec::new();
            let mut batch_bytes = 0;
            for hash in packet.miss_hashes {
                if let Some(payload) = cache.get(hash) {
                    if !batch.is_empty() && batch_bytes + payload.len() > MAX_RESPONSE_PAYLOAD_BYTES
                    {
                        batches.push(std::mem::take(&mut batch));
                        batch_bytes = 0;
                    }
                    batch_bytes += payload.len();
                    batch.push(CacheBlob { hash, payload });
                } else {
                    warn!("Client requested missing blob {hash:#x} not found in server cache");
                }
            }
            if !batch.is_empty() {
                batches.push(batch);
            }
            batches
        };
        for blobs in response_batches {
            self.send_packet(&CClientCacheMissResponse { blobs: &blobs })
                .await;
        }
    }

    pub async fn await_close_interrupt(&self) {
        self.close_token.cancelled().await;
    }

    pub async fn get_packet_payload(&self, packet: Vec<u8>) -> Option<Bytes> {
        let mut network_reader = self.network_reader.lock().await;
        tokio::select! {
            () = self.await_close_interrupt() => {
                debug!("Canceling player packet processing");
                None
            },
            packet_result = network_reader.get_packet_payload(packet) => {
                match packet_result {
                    Ok(packet) => Some(packet),
                    Err(err) => {
                        if !matches!(err, PacketDecodeError::ConnectionClosed) {
                            debug!("Failed to decode packet from client: {err}");
                            let text = format!("Error while reading incoming packet {err}");
                            self.kick(DisconnectReason::BadPacket, text).await;
                        }
                        None
                    }
                }
            }
        }
    }

    pub fn spawn_task<F>(&self, task: F) -> Option<JoinHandle<F::Output>>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        if self.close_token.is_cancelled() {
            None
        } else {
            let _guard = self.rt_handle.enter();
            Some(self.tasks.spawn(task))
        }
    }
}
