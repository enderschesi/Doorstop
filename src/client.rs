use crate::queue::{Queue, QueueStatus};
use anyhow::bail as yeet;
use azalea_auth::AuthResult;
use azalea_protocol::connect::Connection;
use azalea_protocol::packets::game::serverbound_accept_teleportation_packet::ServerboundAcceptTeleportationPacket;
use azalea_protocol::packets::game::serverbound_keep_alive_packet::ServerboundKeepAlivePacket;
use azalea_protocol::packets::game::serverbound_move_player_pos_rot_packet::ServerboundMovePlayerPosRotPacket;
use azalea_protocol::packets::game::{ClientboundGamePacket, ServerboundGamePacket};
use azalea_protocol::packets::handshake::client_intention_packet::ClientIntentionPacket;
use azalea_protocol::packets::login::serverbound_hello_packet::ServerboundHelloPacket;
use azalea_protocol::packets::login::serverbound_key_packet::{
    NonceOrSaltSignature, ServerboundKeyPacket,
};
use azalea_protocol::packets::login::ClientboundLoginPacket;
use azalea_protocol::packets::status::serverbound_status_request_packet::ServerboundStatusRequestPacket;
use azalea_protocol::packets::status::ClientboundStatusPacket;
use azalea_protocol::packets::ConnectionProtocol;
use regex::Regex;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::sync::RwLock;

pub struct Client {
    auth: AuthResult,
    packets: Arc<RwLock<Option<Vec<ClientboundGamePacket>>>>,

    pub c2s: tokio::sync::broadcast::Sender<ServerboundGamePacket>,
    pub s2c: tokio::sync::broadcast::Sender<ClientboundGamePacket>,

    pub is_connected: Arc<AtomicBool>,
    pub is_standalone: Arc<AtomicBool>,

    pub queue: Arc<RwLock<Queue>>,
}

impl Client {
    pub async fn new(queue: Arc<RwLock<Queue>>) -> Self {
        let mut cache_path = PathBuf::new();
        cache_path.push("accounts.cache");
        let auth = azalea_auth::auth(
            std::env::var("email").unwrap().as_str(),
            azalea_auth::AuthOpts {
                cache_file: Some(cache_path),
                ..Default::default()
            },
        )
        .await
        .expect("Couldn't authenticate");

        let (s2c_sender, _) = tokio::sync::broadcast::channel(256);
        let (c2s_sender, _) = tokio::sync::broadcast::channel(256);

        Self {
            auth,
            packets: Arc::new(RwLock::new(None)),

            s2c: s2c_sender,
            c2s: c2s_sender,

            is_connected: Arc::new(AtomicBool::new(true)),
            is_standalone: Arc::new(AtomicBool::new(true)),

            queue,
        }
    }

    pub fn is_connected(&self) -> bool {
        self.is_connected.load(Ordering::Relaxed)
    }

    pub fn is_standalone(&self) -> bool {
        self.is_standalone.load(Ordering::Relaxed)
    }

    pub fn set_standalone(&self, value: bool) -> bool {
        let value_before = self.is_standalone.load(Ordering::Relaxed);
        self.is_standalone.store(value, Ordering::Relaxed);
        value_before
    }

    pub fn get_packets(&self) -> Arc<RwLock<Option<Vec<ClientboundGamePacket>>>> {
        self.packets.clone()
    }

    pub async fn connect(&mut self) -> anyhow::Result<()> {
        let connection = TcpStream::connect("51.81.4.128:25565").await?;
        connection.set_nodelay(true).unwrap();
        let mut connection = Connection::wrap(connection);

        connection
            .write(
                ClientIntentionPacket {
                    protocol_version: 760,
                    hostname: "2b2t.org".to_string(),
                    port: 25566,
                    intention: ConnectionProtocol::Login,
                }
                .get(),
            )
            .await?;

        let mut connection = connection.login();

        connection
            .write(
                ServerboundHelloPacket {
                    username: self.auth.profile.name.clone(),
                    public_key: None,
                    profile_id: Some(self.auth.profile.id),
                }
                .get(),
            )
            .await?;

        let _game_profile = loop {
            match connection.read().await? {
                ClientboundLoginPacket::Hello(packet) => {
                    let e = azalea_crypto::encrypt(&packet.public_key, &packet.nonce).unwrap();
                    connection
                        .authenticate(
                            self.auth.access_token.as_str(),
                            &self.auth.profile.id,
                            e.secret_key,
                            &packet,
                        )
                        .await?;
                    connection
                        .write(
                            ServerboundKeyPacket {
                                key_bytes: e.encrypted_public_key,
                                nonce_or_salt_signature: NonceOrSaltSignature::Nonce(
                                    e.encrypted_nonce,
                                ),
                            }
                            .get(),
                        )
                        .await?;
                    connection.set_encryption_key(e.secret_key);
                }
                ClientboundLoginPacket::GameProfile(packet) => break packet,
                ClientboundLoginPacket::LoginCompression(packet) => {
                    connection.set_compression_threshold(packet.compression_threshold);
                    continue;
                }
                _ => yeet!("Unexpected packet!"),
            }
        };

        let mut connection = connection.game();

        let mut packets = vec![];

        loop {
            match connection.read().await {
                Ok(ClientboundGamePacket::PlayerPosition(packet)) => {
                    packets.push(packet.clone().get());
                    connection
                        .write(ServerboundAcceptTeleportationPacket { id: packet.id }.get())
                        .await?;
                    connection
                        .write(
                            ServerboundMovePlayerPosRotPacket {
                                x: packet.x,
                                y: packet.y,
                                z: packet.z,
                                y_rot: packet.y_rot,
                                x_rot: packet.x_rot,
                                on_ground: false,
                            }
                            .get(),
                        )
                        .await?;
                }
                Ok(ClientboundGamePacket::ServerData(_)) => break,
                Ok(packet) => {
                    packets.push(packet.clone());
                    continue;
                }
                Err(e) => {
                    println!("An error occured! {}", *e);
                    continue;
                }
            }
        };

        *self.packets.write().await = Some(packets);


        let (mut reader, mut writer) = connection.into_split();

        {
            let mut queue = self.queue.write().await;
            queue.update_status(QueueStatus::Queueing);
        }

        // TODO: Replace these unwraps with something better

        let mut s2c_receiver = self.s2c.subscribe();
        let queue = self.queue.clone();
        let info_updater = tokio::spawn(async move {
            let re_place_in_queue = Regex::new(r"Position in queue: (\d+)").unwrap();
            loop {
                let packet = s2c_receiver.recv().await.unwrap();

                if let ClientboundGamePacket::SystemChat(packet) = packet {
                    if packet.content.to_string().contains("Connected to the server.") {
                        let mut queue = queue.write().await;
                        queue.update_status(QueueStatus::Waiting);
                    }

                    if queue.read().await.status() == QueueStatus::Queueing {
                        if let Some(captures) = re_place_in_queue.captures(&packet.content.to_string()) {
                            let number: u32 = captures.get(1).unwrap().as_str().parse().unwrap();
                            let mut queue = queue.write().await;
                            queue.update_place(number)
                        }
                    }
                }
            }
        });

        let queue = self.queue.clone();
        let pinger_updater = tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(2)).await;
                let connection = TcpStream::connect("51.81.4.128:25565").await.unwrap();
                connection.set_nodelay(true).unwrap();
                let mut connection = Connection::wrap(connection);

                connection
                    .write(
                        ClientIntentionPacket {
                            protocol_version: 760,
                            hostname: "2b2t.org".to_string(),
                            port: 25566,
                            intention: ConnectionProtocol::Status,
                        }
                        .get(),
                    )
                    .await
                    .unwrap();

                let mut connection = connection.status();

                connection
                    .write(ServerboundStatusRequestPacket {}.get())
                    .await
                    .unwrap();

                let status = loop {
                    match connection.read().await.unwrap() {
                        ClientboundStatusPacket::StatusResponse(status) => break status,
                        _ => continue,
                    }
                };

                let re_in_game = Regex::new(r"In-game: (\d+)").unwrap();
                let re_queued = Regex::new(r"Queue: (\d+)").unwrap();
                let re_priority = Regex::new(r"Priority queue: (\d+)").unwrap();

                let _in_game: u32 = re_in_game
                    .captures(status.players.sample.get(0).unwrap().name.as_str())
                    .unwrap()
                    .get(1)
                    .unwrap()
                    .as_str()
                    .parse()
                    .unwrap();
                let queued: u32 = re_queued
                    .captures(status.players.sample.get(1).unwrap().name.as_str())
                    .unwrap()
                    .get(1)
                    .unwrap()
                    .as_str()
                    .parse()
                    .unwrap();
                let _priority: u32 = re_priority
                    .captures(status.players.sample.get(2).unwrap().name.as_str())
                    .unwrap()
                    .get(1)
                    .unwrap()
                    .as_str()
                    .parse()
                    .unwrap();

                let mut queue = queue.write().await;

                queue.update_length(queued);
            }
        });

        let mut c2s_receiver = self.c2s.subscribe();
        let c2s = tokio::spawn(async move {
            loop {
                let packet = c2s_receiver.recv().await.unwrap();

                if let ServerboundGamePacket::AcceptTeleportation(packet) = packet.clone() && packet.id == 1 {
                    continue
                }

                writer.write(packet).await.unwrap();
            }
        });

        let s2c_sender = self.s2c.clone();
        let s2c = tokio::spawn(async move {
            loop {
                let packet = reader.read().await.unwrap();

                s2c_sender.send(packet.clone()).unwrap();
            }
        });

        let mut s2c_receiver = self.s2c.subscribe();
        let c2s_sender = self.c2s.clone();
        let keepalive = tokio::spawn(async move {
            loop {
                let packet = s2c_receiver.recv().await.unwrap();

                if let ClientboundGamePacket::KeepAlive(packet) = packet {
                    c2s_sender
                        .send(ServerboundKeepAlivePacket { id: packet.id }.get())
                        .unwrap();
                }
            }
        });

        tokio::try_join!(c2s, s2c, keepalive, info_updater, pinger_updater)?;

        Ok(())
    }
}
