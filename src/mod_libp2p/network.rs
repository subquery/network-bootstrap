use crate::mod_libp2p::behavior::{AgentBehavior, AgentEvent};
use alloy::primitives::{keccak256, Address};
use cached::{stores::SizedCache, Cached};
use futures_util::StreamExt;
use libp2p::{
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
    swarm::SwarmEvent,
    tls, yamux, PeerId, StreamProtocol, Swarm,
};
use once_cell::sync::Lazy;
use serde_json::{json, Value};
use std::{collections::HashSet, error::Error, time::Duration};
use tokio::sync::Mutex;
use tracing::{error, info};

pub const TEST_BOOSTNODE_PEER_ID_LIST: [&str; 3] = [
    "16Uiu2HAm5SPUotukayoKUZG5jQQ9zAGgjAXXz4Tg62kzZMbikLdQ",
    "16Uiu2HAm3iA5E2xfMsVBtKnh4DEbCDmGEsQJMGWVsVWPMESsfnso",
    "16Uiu2HAmUFSkx4esqLoos3TvA55WKNDJURoqCrjQLGGuKeZWiw8e",
];
pub const PRODUCTION_BOOSTNODE_PEER_ID_LIST: [&str; 3] = [
    "16Uiu2HAm9dyPd6p9oU1bL1Sc7sJUg5WrB7gL4tJomesK27q3meHm",
    "16Uiu2HAmSN16v7Pq4EXam94c1Q8k3pTyCdumiRJfof7cYzPGpwQN",
    "16Uiu2HAm14nXNnB1GxnocpmHckaXfqtshp5ZD8QJvXeXr6kvV4KM",
];

pub const METRICS_PEER_ID: &str = "16Uiu2HAmNa64mzMD6Uq4EhUTdHKoZE7MLiEh7hCK3ACN5F5MgJoL";
pub const TEST_METRICS_PEER_ID: &str = "16Uiu2HAmUGCzsEwPGyuE9HcTzKGY4LUPFpLP3vKpT7buJUAdsKX5";

pub static QUERY_INDEXER_URL: Lazy<&str> = Lazy::new(|| {
    if std::env::var("NETWORK").as_deref() == Ok("testnet") {
        "https://api.subquery.network/sq/subquery/base-testnet"
    } else {
        "https://api.subquery.network/sq/subquery/subquery-mainnet"
    }
});

static LAZY_BOOTNODE_METRICS_LIST: Lazy<Vec<&str>> = Lazy::new(|| {
    if std::env::var("NETWORK").as_deref() == Ok("testnet") {
        let mut temp_list = TEST_BOOSTNODE_PEER_ID_LIST.to_vec();
        temp_list.push(TEST_METRICS_PEER_ID);
        temp_list
    } else {
        let mut temp_list = PRODUCTION_BOOSTNODE_PEER_ID_LIST.to_vec();
        temp_list.push(METRICS_PEER_ID);
        temp_list
    }
});

static GLOBAL_INDEXER_CACHE: Lazy<Mutex<SizedCache<PeerId, ()>>> =
    Lazy::new(|| Mutex::new(SizedCache::with_size(2000)));

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
        let mut interval1 = tokio::time::interval(tokio::time::Duration::from_secs(60));
        let mut interval2 = tokio::time::interval(tokio::time::Duration::from_secs(60));
        loop {
            tokio::select! {
                event = self.swarm.select_next_some() => self.handle_event(event).await,
                _ = interval1.tick() => {
                    let peer_list = self.swarm.behaviour_mut().kad_known_peers();
                    info!("\n------ 60 secs, local_peer_id is {:?}, kad peer list is {:?}\n\n", self.swarm.local_peer_id(), peer_list);
                }

                _ = interval2.tick() => {
                    let mut peer_list = HashSet::new();
                    for peer in self.swarm.connected_peers() {
                        peer_list.insert(peer);
                    }
                    info!("\n------ 60 secs, local_peer_id is {:?}, swarm peer list is {:?}\n\n", self.swarm.local_peer_id(), peer_list);
                }
            }
        }
    }

    pub async fn handle_event(&mut self, event: SwarmEvent<AgentEvent>) {
        match event {
            SwarmEvent::ConnectionEstablished { .. } => {}
            SwarmEvent::ConnectionClosed {
                peer_id,
                num_established,
                ..
            } => {
                if num_established == 0 {
                    let mut indexer_cache = GLOBAL_INDEXER_CACHE.lock().await;
                    indexer_cache.cache_remove(&peer_id);
                    drop(indexer_cache);
                    self.swarm.behaviour_mut().kad.remove_peer(&peer_id);
                    _ = self.swarm.disconnect_peer_id(peer_id);
                }
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
                if LAZY_BOOTNODE_METRICS_LIST.contains(&peer_id.to_base58().as_str()) {
                    indexer_cache.cache_set(peer_id, ());
                    drop(indexer_cache);
                    for addr in listen_addrs {
                        self.swarm.behaviour_mut().kad.add_address(&peer_id, addr);
                    }
                } else if let Ok(controller_address) =
                    Self::libp2p_publickey_to_eth_address(&public_key).await
                {
                    match Self::is_controller_valid(&controller_address).await {
                        Ok(_) => {
                            for addr in listen_addrs {
                                self.swarm.behaviour_mut().kad.add_address(&peer_id, addr);
                            }
                        }
                        Err(_) => {
                            error!(
                                "peer_id {:?} is not valid, ethereum address: {} is not registered",
                                peer_id, controller_address
                            );
                            self.swarm.close_connection(connection_id);
                        }
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

        let mut swarm = libp2p::SwarmBuilder::with_existing_identity(libp2p_keypair.clone())
            .with_tokio()
            .with_tcp(
                Default::default(),
                (tls::Config::new, noise::Config::new),
                yamux::Config::default,
            )?
            .with_quic()
            .with_dns_config(dns::ResolverConfig::default(), dns::ResolverOpts::default())
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
            .with_swarm_config(|c| c.with_idle_connection_timeout(Duration::from_secs(u64::MAX)))
            .build();

        swarm.behaviour_mut().kad.set_mode(Some(kad::Mode::Server));

        let libp2p_tcp_listen_address = std::env::var("LIBP2P_TCP_LISTEN_ADDRESS")
            .unwrap_or("/ip4/0.0.0.0/tcp/8000".to_string());
        let libp2p_tcp_listen_address = libp2p_tcp_listen_address.parse()?;
        swarm.listen_on(libp2p_tcp_listen_address)?;

        let libp2p_quic_listen_address = std::env::var("LIBP2P_QUIC_LISTEN_ADDRESS")
            .unwrap_or("/ip4/0.0.0.0/udp/8001/quic-v1".to_string());
        let libp2p_quic_listen_address = libp2p_quic_listen_address.parse()?;
        swarm.listen_on(libp2p_quic_listen_address)?;
        Ok(swarm)
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

        let response = client.post(*QUERY_INDEXER_URL).json(&query).send().await?;

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
