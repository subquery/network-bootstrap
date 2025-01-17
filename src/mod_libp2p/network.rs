use crate::mod_libp2p::behavior::{AgentBehavior, AgentEvent};
use base64::{engine::general_purpose::STANDARD, Engine};
use either::Either;
use futures_util::StreamExt;
use libp2p::{
    core::transport::upgrade::Version,
    dns,
    identify::{Behaviour as IdentifyBehavior, Config as IdentifyConfig, Event as IdentifyEvent},
    identity::{self, Keypair},
    kad::{
        self, store::MemoryStore as KadInMemory, Behaviour as KadBehavior, Config as KadConfig,
        Event as KademliaEvent,
    },
    noise,
    ping::{self, Event as PingEvent},
    pnet::{PnetConfig, PreSharedKey},
    swarm::SwarmEvent,
    tcp, yamux, PeerId, StreamProtocol, Swarm, Transport,
};
use std::{error::Error, time::Duration};
use tracing::info;

pub(crate) struct EventLoop {
    swarm: Swarm<AgentBehavior>,
}

impl EventLoop {
    pub async fn new() -> Result<Self, Box<dyn Error>> {
        match Self::start_swarm().await {
            Ok(swarm) => Ok(Self { swarm }),
            Err(error) => Err(error),
        }
    }

    pub(crate) async fn run(&mut self) {
        loop {
            tokio::select! {
                event = self.swarm.select_next_some() => self.handle_event(event).await,
            }
        }
    }

    pub async fn handle_event(&mut self, event: SwarmEvent<AgentEvent>) {
        match event {
            SwarmEvent::ConnectionEstablished { .. } => {}
            SwarmEvent::ConnectionClosed { peer_id, .. } => {
                self.swarm.behaviour_mut().kad.remove_peer(&peer_id);
            }
            SwarmEvent::Behaviour(AgentEvent::Identify(sub_event)) => {
                self.handle_identify_event(sub_event).await
            }
            SwarmEvent::Behaviour(AgentEvent::Kad(sub_event)) => {
                self.handle_kad_event(sub_event).await
            }
            SwarmEvent::Behaviour(AgentEvent::Ping(sub_event)) => {
                self.handle_ping_event(sub_event).await
            }
            _ => {}
        }
    }

    async fn handle_identify_event(&mut self, event: IdentifyEvent) {
        match event {
            IdentifyEvent::Received { peer_id, info, .. } => {
                for addr in info.clone().listen_addrs {
                    self.swarm.behaviour_mut().kad.add_address(&peer_id, addr);
                }
            }
            _ => {}
        }
    }

    async fn handle_kad_event(&mut self, _event: KademliaEvent) {}

    async fn handle_ping_event(&mut self, _event: PingEvent) {}

    pub async fn start_swarm() -> Result<Swarm<AgentBehavior>, Box<dyn Error>> {
        let sk = std::env::var("ACCOUNT_SK").expect("ACCOUNT_SK missing in .env");
        let private_key_bytes = hex::decode(sk)?;
        let secret_key = identity::secp256k1::SecretKey::try_from_bytes(private_key_bytes)?;
        let libp2p_keypair: Keypair = identity::secp256k1::Keypair::from(secret_key).into();

        let psk = Self::get_psk();

        if let Ok(psk) = psk {
            info!("using swarm key with fingerprint: {}", psk.fingerprint());
        }

        let mut swarm = libp2p::SwarmBuilder::with_existing_identity(libp2p_keypair.clone())
            .with_tokio()
            .with_other_transport(|key| {
                let noise_config = noise::Config::new(key).unwrap();
                let mut yamux_config = yamux::Config::default();
                yamux_config.set_max_num_streams(1024 * 1024);
                let base_transport =
                    tcp::tokio::Transport::new(tcp::Config::default().nodelay(true));
                let base_transport = dns::tokio::Transport::system(base_transport)
                    .expect("DNS")
                    .boxed();
                let maybe_encrypted = match psk {
                    Ok(psk) => Either::Left(
                        base_transport
                            .and_then(move |socket, _| PnetConfig::new(psk).handshake(socket)),
                    ),
                    Err(_) => Either::Right(base_transport),
                };
                maybe_encrypted
                    .upgrade(Version::V1Lazy)
                    .authenticate(noise_config)
                    .multiplex(yamux_config)
            })?
            .with_dns()?
            .with_behaviour(|key| {
                let local_peer_id = PeerId::from(key.clone().public());

                let mut kad_config = KadConfig::new(StreamProtocol::new("/agent/connection/1.0.0"));
                kad_config.set_periodic_bootstrap_interval(Some(Duration::from_secs(120)));
                kad_config.set_publication_interval(Some(Duration::from_secs(120)));
                kad_config.set_replication_interval(Some(Duration::from_secs(120)));
                let kad_memory = KadInMemory::new(local_peer_id);
                let kad = KadBehavior::with_config(local_peer_id, kad_memory, kad_config);

                let identify_config = IdentifyConfig::new(
                    "/agent/connection/1.0.0".to_string(),
                    key.clone().public(),
                )
                .with_push_listen_addr_updates(true)
                .with_interval(Duration::from_secs(120));
                let identify = IdentifyBehavior::new(identify_config);

                let ping = ping::Behaviour::new(
                    ping::Config::new().with_interval(Duration::from_secs(120)),
                );

                AgentBehavior::new(kad, identify, ping)
            })?
            .with_swarm_config(|c| c.with_idle_connection_timeout(Duration::from_secs(120)))
            .build();

        swarm.behaviour_mut().kad.set_mode(Some(kad::Mode::Server));

        let private_net_address =
            std::env::var("PRIVITE_NET_ADDRESS").unwrap_or("/ip4/0.0.0.0/tcp/8000".to_string());
        let private_net_address = private_net_address.parse()?;
        swarm.listen_on(private_net_address)?;
        Ok(swarm)
    }

    /// Read the pre shared key file from the given ipfs directory
    fn get_psk() -> Result<PreSharedKey, Box<dyn Error>> {
        let base64_key =
            std::env::var("PRIVITE_NET_KEY").map_err(|_| "PRIVITE_NET_KEY missing in .env")?;
        let bytes = STANDARD.decode(&base64_key)?;
        let key: [u8; 32] = bytes
            .try_into()
            .map_err(|_| "Decoded key must be 32 bytes long")?;
        Ok(PreSharedKey::new(key))
    }
}
