use crate::mod_libp2p::behavior::{AgentBehavior, AgentEvent};
use alloy::primitives::{keccak256, Address};
use base64::{engine::general_purpose::STANDARD, Engine};
use cached::{stores::SizedCache, Cached};
use either::Either;
use futures_util::StreamExt;
use libp2p::{
    core::transport::upgrade::Version,
    dns,
    identify::{
        Behaviour as IdentifyBehavior, Config as IdentifyConfig, Event as IdentifyEvent,
        Info as IdentifyInfo,
    },
    identity::{self, Keypair, PublicKey as Libp2pPublicKey},
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
use once_cell::sync::Lazy;
use serde_json::{json, Value};
use std::{error::Error, time::Duration};
use tokio::sync::Mutex;
use tracing::{error, info};

pub(crate) struct EventLoop {
    swarm: Swarm<AgentBehavior>,
}

const METRICS_ADDRESS: &str = "0x41526be3cde4b0ff39a4a2908af3527a703e9fda";

const QUERY_INDEXER_URL: &str = match option_env!("USE_TESTNET_QUERY") {
    Some(_value) => "https://api.subquery.network/sq/subquery/base-testnet",
    None => "https://api.subquery.network/sq/subquery/subquery-mainnet",
};

static GLOBAL_INDEXER_CACHE: Lazy<Mutex<SizedCache<PeerId, ()>>> =
    Lazy::new(|| Mutex::new(SizedCache::with_size(2000)));

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
        if let IdentifyEvent::Received {
            connection_id,
            peer_id,
            info:
                IdentifyInfo {
                    public_key,
                    listen_addrs,
                    ..
                },
        } = event
        {
            let mut indexer_cache = GLOBAL_INDEXER_CACHE.lock().await;
            if indexer_cache.cache_get(&peer_id).is_none() {
                if let Ok(controller_address) =
                    Self::libp2p_publickey_to_eth_address(&public_key).await
                {
                    if controller_address == METRICS_ADDRESS
                        || Self::is_controller_valid(&controller_address).await.is_ok()
                    {
                        indexer_cache.cache_set(peer_id, ());
                        drop(indexer_cache);
                        for addr in listen_addrs {
                            self.swarm.behaviour_mut().kad.add_address(&peer_id, addr);
                        }
                    } else {
                        error!(
                            "peer_id {:?} is not valid, ethereum address: {} is not registered",
                            peer_id, controller_address
                        );
                        self.swarm.close_connection(connection_id);
                    }
                } else {
                    error!(
                        "peer_id {:?} is not valid, cannot convert into ethereum address",
                        peer_id,
                    );
                    self.swarm.close_connection(connection_id);
                }
            }
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

    async fn libp2p_publickey_to_eth_address(
        pub_key: &Libp2pPublicKey,
    ) -> Result<String, Box<dyn Error>> {
        if let Ok(secp256k1_key) = pub_key.clone().try_into_secp256k1() {
            let pub_key_bytes = secp256k1_key.to_bytes_uncompressed();

            let hash = keccak256(&pub_key_bytes[1..]);
            let address = Address::from_slice(&hash[12..]);

            Ok(address.to_checksum(None).to_lowercase())
        } else {
            Err("libp2p key error, cannot convert into secp256k1 key".into())
        }
    }

    async fn is_controller_valid(controller: &str) -> Result<(), Box<dyn Error>> {
        let client = reqwest::Client::new();

        let query = json!({
            "query": format!("{{\n  indexers(filter: {{controller: {{equalToInsensitive: \"{}\"}}}}) {{\n    nodes {{\n      id\n    }}\n  }}\n}}", controller)
        });

        let response = client.post(QUERY_INDEXER_URL).json(&query).send().await?;

        let body = response.text().await?;

        let v: Value = serde_json::from_str(&body)?;
        if let Some(_id) = v
            .get("data")
            .and_then(|data| data.get("indexers"))
            .and_then(|indexers| indexers.get("nodes"))
            .and_then(|nodes| nodes.get(0))
            .and_then(|node| node.get("id"))
            .and_then(|id| id.as_str())
        {
            Ok(())
        } else {
            Err("controller is not valid".into())
        }
    }
}
