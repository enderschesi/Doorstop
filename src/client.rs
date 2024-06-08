use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};

use anyhow::{bail, Result};
use azalea_auth::AuthResult;
use azalea_client::ClientInformation;
use azalea_protocol::{
    connect::Connection,
    packets::{
        configuration::{
            serverbound_client_information_packet::ServerboundClientInformationPacket,
            serverbound_finish_configuration_packet::ServerboundFinishConfigurationPacket,
            ClientboundConfigurationPacket,
        },
        game::{
            serverbound_accept_teleportation_packet::ServerboundAcceptTeleportationPacket,
            serverbound_keep_alive_packet::ServerboundKeepAlivePacket,
            serverbound_move_player_pos_rot_packet::ServerboundMovePlayerPosRotPacket,
            ClientboundGamePacket,
            ServerboundGamePacket,
        },
        handshaking::client_intention_packet::ClientIntentionPacket,
        login::{
            serverbound_hello_packet::ServerboundHelloPacket,
            serverbound_key_packet::ServerboundKeyPacket,
            serverbound_login_acknowledged_packet::ServerboundLoginAcknowledgedPacket,
            ClientboundLoginPacket,
        },
        status::{
            serverbound_status_request_packet::ServerboundStatusRequestPacket,
            ClientboundStatusPacket,
        },
        ConnectionProtocol,
    },
};
use regex::Regex;
use tokio::{net::TcpStream, sync::RwLock};

use crate::queue::{Queue, Status};

pub struct Client {
    auth:    AuthResult,
    packets: Arc<RwLock<Option<Vec<ClientboundGamePacket>>>>,

    pub c2s: tokio::sync::broadcast::Sender<ServerboundGamePacket>,
    pub s2c: tokio::sync::broadcast::Sender<ClientboundGamePacket>,

    pub is_connected:  Arc<AtomicBool>,
    pub is_standalone: Arc<AtomicBool>,

    pub queue: Arc<RwLock<Queue>>,
}

impl Client {
    pub async fn new(queue: Arc<RwLock<Queue>>) -> Self {
        let Some(minecraft_dir) = minecraft_folder_path::minecraft_dir() else {
            panic!(
                "No {} environment variable found",
                minecraft_folder_path::home_env_var()
            )
        };

        let Ok(email) = std::env::var("email") else {
            panic!("No email environment variable found")
        };

        let auth = azalea_auth::auth(
            email.as_str(),
            azalea_auth::AuthOpts {
                cache_file: Some(minecraft_dir.join("azalea-auth.json")),
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

    #[allow(dead_code)]
    pub fn is_connected(&self) -> bool {
        self.is_connected.load(Ordering::Relaxed)
    }

    #[allow(dead_code)]
    pub fn is_standalone(&self) -> bool {
        self.is_standalone.load(Ordering::Relaxed)
    }

    #[allow(dead_code)]
    pub fn set_standalone(&self, value: bool) -> bool {
        let value_before = self.is_standalone.load(Ordering::Relaxed);
        self.is_standalone.store(value, Ordering::Relaxed);
        value_before
    }

    pub fn get_packets(&self) -> Arc<RwLock<Option<Vec<ClientboundGamePacket>>>> {
        self.packets.clone()
    }

    #[allow(clippy::too_many_lines)]
    pub async fn connect(&self) -> Result<()> {
        // TODO: Add ServerAddress parameter and resolve addr
        let stream = TcpStream::connect("31.25.11.130:25565").await?;
        stream.set_nodelay(true)?;

        let mut connection = Connection::wrap(stream);

        connection
            .write(
                ClientIntentionPacket {
                    protocol_version: 764,
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
                    name:       self.auth.profile.name.clone(),
                    profile_id: self.auth.profile.id,
                }
                .get(),
            )
            .await?;

        let _game_profile = loop {
            match connection.read().await? {
                ClientboundLoginPacket::Hello(packet) => {
                    let e = azalea_crypto::encrypt(&packet.public_key, &packet.challenge).unwrap();
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
                                key_bytes:           e.encrypted_public_key,
                                encrypted_challenge: e.encrypted_challenge,
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
                ClientboundLoginPacket::LoginDisconnect(packet) => bail!(
                    "Disconnected during login! Reason: {}",
                    packet.reason.to_string()
                ),
                _ => {}
            }
        };

        connection
            .write(ServerboundLoginAcknowledgedPacket {}.get())
            .await?;

        let mut connection = connection.configuration();

        connection
            .write(
                ServerboundClientInformationPacket {
                    information: ClientInformation::default(),
                }
                .get(),
            )
            .await?;

        loop {
            match connection.read().await? {
                ClientboundConfigurationPacket::FinishConfiguration(_) => {
                    connection
                        .write(ServerboundFinishConfigurationPacket {}.get())
                        .await?;
                    break;
                }
                ClientboundConfigurationPacket::Disconnect(packet) => bail!(
                    "Disconnected during configuration! Reason: {}",
                    packet.reason.to_string()
                ),
                _ => {}
            }
        }

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
                                x:         packet.x,
                                y:         packet.y,
                                z:         packet.z,
                                y_rot:     packet.y_rot,
                                x_rot:     packet.x_rot,
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
                Err(error) => {
                    println!("An error occurred! {}", *error);
                    continue;
                }
            }
        }

        *self.packets.write().await = Some(packets);

        let (mut reader, mut writer) = connection.into_split();

        {
            let mut queue = self.queue.write().await;
            queue.update_status(Status::Queueing);
        }

        // TODO: Replace these unwraps with something better

        let mut s2c_receiver = self.s2c.subscribe();
        let queue = self.queue.clone();
        let info_updater = tokio::spawn(async move {
            let re_place_in_queue = Regex::new(r"Position in queue: (\d+)").unwrap();
            loop {
                let packet = s2c_receiver.recv().await.unwrap();

                if let ClientboundGamePacket::SystemChat(packet) = packet {
                    if packet
                        .content
                        .to_string()
                        .contains("Connected to the server.")
                    {
                        let mut queue = queue.write().await;
                        queue.update_status(Status::Waiting);
                    }

                    if queue.read().await.status() == Status::Queueing {
                        if let Some(captures) =
                            re_place_in_queue.captures(&packet.content.to_string())
                        {
                            let number: u32 = captures.get(1).unwrap().as_str().parse().unwrap();
                            let mut queue = queue.write().await;
                            queue.update_place(number);
                        }
                    }
                }
            }
        });

        let queue = self.queue.clone();
        let pinger_updater = tokio::spawn(async move {
            loop {
                // TODO: Add ServerAddress parameter and resolve addr
                let stream = TcpStream::connect("51.81.4.128:25565").await.unwrap();
                stream.set_nodelay(true).unwrap();

                let mut connection = Connection::wrap(stream);

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
                        ClientboundStatusPacket::PongResponse(_) => continue,
                    }
                };

                let re_in_game = Regex::new(r"In-game: (\d+)").unwrap();
                let re_queued = Regex::new(r"Queue: (\d+)").unwrap();
                let re_priority = Regex::new(r"Priority queue: (\d+)").unwrap();

                let _in_game: u32 = re_in_game
                    .captures(status.players.sample.first().unwrap().name.as_str())
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

                queue.write().await.update_length(queued);

                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        });

        let mut c2s_receiver = self.c2s.subscribe();
        let c2s = tokio::spawn(async move {
            loop {
                let packet = c2s_receiver.recv().await.unwrap();

                if let ServerboundGamePacket::AcceptTeleportation(packet) = packet.clone()
                    && packet.id == 1
                {
                    continue;
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
