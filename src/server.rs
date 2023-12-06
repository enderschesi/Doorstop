use std::{
    sync::{
        atomic::{AtomicBool, AtomicUsize},
        Arc,
    },
    time::Duration,
};

use anyhow::{bail, Result};
use azalea_chat::{text_component::TextComponent, FormattedText};
use azalea_protocol::{
    connect::Connection,
    packets::{
        game::{ClientboundGamePacket, ServerboundGamePacket},
        handshaking::ServerboundHandshakePacket,
        login::{
            clientbound_game_profile_packet::ClientboundGameProfilePacket,
            clientbound_hello_packet::ClientboundHelloPacket,
            clientbound_login_compression_packet::ClientboundLoginCompressionPacket,
            clientbound_login_disconnect_packet::ClientboundLoginDisconnectPacket,
            ClientboundLoginPacket,
            ServerboundLoginPacket,
        },
        status::{
            clientbound_pong_response_packet::ClientboundPongResponsePacket,
            clientbound_status_response_packet::{
                ClientboundStatusResponsePacket,
                Players,
                Version,
            },
            ClientboundStatusPacket,
            ServerboundStatusPacket,
        },
        ConnectionProtocol,
    },
    read::ReadPacketError,
};
use rand::{rngs::OsRng, Rng};
use rsa::{traits::PublicKeyParts, Pkcs1v15Encrypt, RsaPrivateKey, RsaPublicKey};
use rsa_der::public_key_to_der;
use tokio::{
    net::{TcpListener, TcpStream},
    sync::RwLock,
};

use crate::queue::{Queue, Status};

lazy_static::lazy_static! {
    static ref PRIVATE_KEY: RsaPrivateKey = RsaPrivateKey::new(&mut OsRng, 1024).expect("Failed to generate private key");
    static ref PUBLIC_KEY: RsaPublicKey = PRIVATE_KEY.to_public_key();
}

pub struct Server {
    #[allow(dead_code)]
    connected_count: Arc<AtomicUsize>,

    queue: Arc<RwLock<Queue>>,

    packets: Arc<RwLock<Option<Vec<ClientboundGamePacket>>>>,
    c2s:     tokio::sync::broadcast::Sender<ServerboundGamePacket>,
    s2c:     tokio::sync::broadcast::Sender<ClientboundGamePacket>,
}

impl Server {
    pub fn new(
        queue: Arc<RwLock<Queue>>,
        packets: Arc<RwLock<Option<Vec<ClientboundGamePacket>>>>,
        c2s: tokio::sync::broadcast::Sender<ServerboundGamePacket>,
        s2c: tokio::sync::broadcast::Sender<ClientboundGamePacket>,
    ) -> Self {
        Self {
            connected_count: Arc::new(AtomicUsize::new(0)),
            queue,
            packets,
            c2s,
            s2c,
        }
    }

    pub async fn listen(&mut self, _is_standalone: Arc<AtomicBool>) -> Result<()> {
        // TODO: Move addr to a variable
        let listener = TcpListener::bind("0.0.0.0:25565").await?;

        loop {
            let (stream, _) = listener.accept().await?;

            _ = self.handle_connection(stream).await;
        }
    }

    async fn format_description(&self) -> String {
        let queue = self.queue.read().await;
        match queue.status() {
            Status::Idling => {
                String::from("                       &c&lIdling...                       ")
            }
            Status::Queueing => {
                if queue.current_place().is_some() {
                    format!(
                        " &e&lQueueing...&r | Place: &l{}&r{} | Time: &l{}&r ",
                        queue.current_place().unwrap(),
                        queue
                            .length()
                            .map_or(String::new(), |amount| format!("/&l{amount}&r")),
                        queue.estimate_time_till_end().map_or(
                            "Calculating".to_string(),
                            |duration| {
                                let days = duration.as_secs() / 86400;
                                let hours = (duration.as_secs() / 3600) % 24;
                                let minutes = (duration.as_secs() / 60) % 60;

                                let mut formatted_duration = String::new();
                                if days > 0 {
                                    formatted_duration.push_str(&format!("{days}D"));
                                }
                                if hours > 0 {
                                    formatted_duration.push_str(&format!("{hours}H"));
                                }
                                if minutes > 0 {
                                    formatted_duration.push_str(&format!("{minutes}M"));
                                }

                                formatted_duration
                            }
                        )
                    )
                } else {
                    String::from(" &e&lQueueing...&r | &l Waiting for data... ")
                }
            }
            Status::Waiting => String::new(),
        }
    }

    async fn handle_connection(&self, stream: TcpStream) -> Result<()> {
        stream.set_nodelay(true)?;

        let mut connection = Connection::wrap(stream);

        let intention = match connection.read().await {
            Ok(packet) => match packet {
                ServerboundHandshakePacket::ClientIntention(packet) => packet,
            },
            Err(e) => return Err(e.into()),
        };

        match intention.intention {
            ConnectionProtocol::Status => self.handle_status(connection.status()).await,
            ConnectionProtocol::Login => self.handle_login(connection.login()).await,
            _ => bail!("Unexpected intention!"),
        }?;

        Ok(())
    }

    async fn handle_status(
        &self,
        mut connection: Connection<ServerboundStatusPacket, ClientboundStatusPacket>,
    ) -> Result<()> {
        loop {
            match connection.read().await {
                Ok(packet) => match packet {
                    ServerboundStatusPacket::StatusRequest(_) => {
                        let description_1 = String::from(
                            " &k--------&r &5Doorstop&r | &5Your 2b2t proxy!&r &k--------&r ",
                        );
                        let description_2 = self.format_description().await;

                        connection
                            .write(
                                ClientboundStatusResponsePacket {
                                    description: FormattedText::Text(TextComponent::new(
                                        format!("{description_1}\n{description_2}")
                                            .replace('&', "§"),
                                    )),
                                    favicon: Some("data:image/png;base64,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".to_string()),
                                    players: Players {
                                        max: 1,
                                        online: 0,
                                        sample: Vec::new(),
                                    },
                                    version: Version {
                                        name: String::from("Doorstop 1.20.2"),
                                        protocol: 764,
                                    },
                                    enforces_secure_chat: None,
                                }
                                .get(),
                            )
                            .await?;
                    }
                    ServerboundStatusPacket::PingRequest(p) => {
                        connection
                            .write(ClientboundPongResponsePacket { time: p.time }.get())
                            .await?;
                        break;
                    }
                },
                Err(e) => match *e {
                    ReadPacketError::ConnectionClosed => {
                        break;
                    }
                    e => {
                        return Err(e.into());
                    }
                },
            }
        }

        Ok(())
    }

    #[allow(clippy::too_many_lines)]
    async fn handle_login(
        &self,
        mut connection: Connection<ServerboundLoginPacket, ClientboundLoginPacket>,
    ) -> Result<()> {
        let nonce = Vec::from(OsRng.gen::<[u8; 16]>());
        let hello = loop {
            match connection.read().await? {
                ServerboundLoginPacket::Hello(packet) => break packet,
                ServerboundLoginPacket::CustomQueryAnswer(_) => continue,
                _ => bail!("Unexpected packet!"),
            }
        };

        connection
            .write(
                ClientboundHelloPacket {
                    server_id:  String::new(),
                    public_key: public_key_to_der(
                        &PUBLIC_KEY.n().to_bytes_be(),
                        &PUBLIC_KEY.e().to_bytes_be(),
                    ),
                    nonce:      nonce.clone(),
                }
                .get(),
            )
            .await?;

        let key = loop {
            match connection.read().await? {
                ServerboundLoginPacket::Key(packet) => break packet,
                ServerboundLoginPacket::CustomQueryAnswer(_) => continue,
                _ => bail!("Unexpected packet!"),
            }
        };

        let shared_secret = PRIVATE_KEY
            .decrypt(Pkcs1v15Encrypt, &key.key_bytes)?
            .try_into()
            .unwrap();

        /*
        // Fuck this shit i have no idea how it works
        let verify_nonce = match key.nonce_or_salt_signature {
            NonceOrSaltSignature::Nonce(nonce) => nonce,
            NonceOrSaltSignature::SaltSignature(sig) => sig.
        };

        let verify_token = PRIVATE_KEY.decrypt(Pkcs1v15Encrypt, &verify_nonce)?;

        if verify_token != nonce {
            conn.write(
                ClientboundLoginDisconnectPacket {
                    reason: FormattedText::Text(TextComponent::new(
                        "Invalid Nonce!".to_string(),
                    )),
                }
                    .get(),
            )
                .await?;
            return Ok(());
        }
         */

        let game_profile = azalea_auth::sessionserver::serverside_auth(
            hello.name.as_str(),
            &public_key_to_der(&PUBLIC_KEY.n().to_bytes_be(), &PUBLIC_KEY.e().to_bytes_be()),
            &shared_secret,
            None,
        )
        .await?;

        connection.set_encryption_key(shared_secret);

        if game_profile.name.to_lowercase() != hello.name.to_lowercase() {
            connection
                .write(
                    ClientboundLoginDisconnectPacket {
                        reason: FormattedText::Text(TextComponent::new(
                            "Invalid Username!".to_string(),
                        )),
                    }
                    .get(),
                )
                .await?;
            return Ok(());
        }

        connection
            .write(
                ClientboundLoginCompressionPacket {
                    compression_threshold: 256,
                }
                .get(),
            )
            .await?;

        connection.set_compression_threshold(256);

        connection
            .write(
                ClientboundGameProfilePacket {
                    game_profile: game_profile.clone(),
                }
                .get(),
            )
            .await?;

        let mut connection = connection.game();

        if let Some(ref packets) = *self.packets.clone().read().await {
            let last_index = packets.len() - 1;
            for (index, packet) in packets.iter().enumerate() {
                if index == last_index {
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
                connection.write(packet.clone()).await?;
            }
        }

        let (mut reader, mut writer) = connection.into_split();

        let c2s_sender = self.c2s.clone();
        let c2s = tokio::spawn(async move {
            loop {
                match reader.read().await {
                    Err(error) => eprintln!("{error}"),
                    Ok(packet) => {
                        // Don't forward keep alive packets as they will be doubled
                        if let ServerboundGamePacket::KeepAlive(_) = packet {
                            println!("Keepalive from client!");
                            continue;
                        }

                        c2s_sender.send(packet.clone()).unwrap();
                    }
                }
            }
        });

        let mut s2c_receiver = self.s2c.subscribe();
        let s2c = tokio::spawn(async move {
            loop {
                let packet = s2c_receiver.recv().await.unwrap();

                writer.write(packet).await.unwrap();
            }
        });

        tokio::try_join!(c2s, s2c)?;

        Ok(())
    }
}
