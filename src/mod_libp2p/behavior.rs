use std::collections::HashSet;

use libp2p::{
    allow_block_list::{AllowedPeers, Behaviour as AllowListBehaviour},
    identify::{Behaviour as IdentifyBehavior, Event as IdentifyEvent},
    kad::{
        store::MemoryStore as KademliaInMemory, Behaviour as KademliaBehavior,
        Event as KademliaEvent,
    },
    ping::{self, Behaviour as PingBehaviour, Event as PingEvent},
    swarm::NetworkBehaviour,
    PeerId,
};

#[derive(NetworkBehaviour)]
#[behaviour(to_swarm = "AgentEvent")]
pub(crate) struct AgentBehavior {
    pub identify: IdentifyBehavior,
    pub kad: KademliaBehavior<KademliaInMemory>,
    pub ping: ping::Behaviour,
    pub allowed_peers: AllowListBehaviour<AllowedPeers>,
}

impl AgentBehavior {
    pub fn new(
        kad: KademliaBehavior<KademliaInMemory>,
        identify: IdentifyBehavior,
        ping: PingBehaviour,
    ) -> Self {
        let allowed_peers = AllowListBehaviour::default();
        Self {
            kad,
            identify,
            ping,
            allowed_peers,
        }
    }

    pub fn kad_known_peers(&mut self) -> HashSet<PeerId> {
        let mut peers = HashSet::new();
        for b in self.kad.kbuckets() {
            for e in b.iter() {
                if !peers.contains(e.node.key.preimage()) {
                    peers.insert(*e.node.key.preimage());
                }
            }
        }

        peers
    }
}

#[derive(Debug)]
pub(crate) enum AgentEvent {
    Identify(IdentifyEvent),
    Kad(KademliaEvent),
    Ping(PingEvent),
}

impl From<IdentifyEvent> for AgentEvent {
    fn from(value: IdentifyEvent) -> Self {
        Self::Identify(value)
    }
}

impl From<KademliaEvent> for AgentEvent {
    fn from(value: KademliaEvent) -> Self {
        Self::Kad(value)
    }
}

impl From<PingEvent> for AgentEvent {
    fn from(value: PingEvent) -> Self {
        Self::Ping(value)
    }
}

impl From<std::convert::Infallible> for AgentEvent {
    fn from(_: std::convert::Infallible) -> Self {
        panic!("NodeBehaviour is not Infallible!")
    }
}
