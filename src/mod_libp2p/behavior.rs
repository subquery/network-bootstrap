use libp2p::{
    identify::{Behaviour as IdentifyBehavior, Event as IdentifyEvent},
    kad::{
        store::MemoryStore as KademliaInMemory, Behaviour as KademliaBehavior,
        Event as KademliaEvent,
    },
    ping::{self, Behaviour as PingBehaviour, Event as PingEvent},
    swarm::NetworkBehaviour,
};

#[derive(NetworkBehaviour)]
#[behaviour(to_swarm = "AgentEvent")]
pub(crate) struct AgentBehavior {
    pub identify: IdentifyBehavior,
    pub kad: KademliaBehavior<KademliaInMemory>,
    pub ping: ping::Behaviour,
}

impl AgentBehavior {
    pub fn new(
        kad: KademliaBehavior<KademliaInMemory>,
        identify: IdentifyBehavior,
        ping: PingBehaviour,
    ) -> Self {
        Self {
            kad,
            identify,
            ping,
        }
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
