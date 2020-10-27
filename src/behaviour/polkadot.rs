use std::collections::VecDeque;
use std::marker::PhantomData;

use futures::task::{Context,Poll};
use libp2p::{PeerId};
use libp2p::multiaddr::Multiaddr;
use libp2p::core::ConnectedPoint;
use libp2p::core::connection::ConnectionId;
use libp2p::swarm::{NetworkBehaviour, NetworkBehaviourAction, PollParameters};

use futures::io::{AsyncRead, AsyncWrite};

use super::super::protocol::net_protocol::{PolkadotProtocolEvent, PolkadotProtocolsHandler, PolkadotProtocolMessage, BlockRequest};

// Network behaviour that sends a single welcome message to every node which connects
pub struct PolkadotBehaviour {
    welcome_message: String,
    network_actions: VecDeque<NetworkBehaviourAction<PolkadotProtocolMessage, String>>,
    //_marker: PhantomData<TSubstream>
}

impl PolkadotBehaviour {
    pub fn new(welcome_message: String) -> Self {
        PolkadotBehaviour {
            welcome_message,
            network_actions: VecDeque::new(),
            //_marker: PhantomData
        }
    }
}

impl NetworkBehaviour for PolkadotBehaviour
//where
//    TSubstream: AsyncRead + AsyncWrite + Send + 'static
{
    type ProtocolsHandler = PolkadotProtocolsHandler;
    type OutEvent = String;

    fn new_handler(&mut self) -> Self::ProtocolsHandler {
        println!("new_handler");
        Default::default()
        //Self::ProtocolsHandler::default()
    }

    fn addresses_of_peer(&mut self, _: &PeerId) -> Vec<Multiaddr> {
        Vec::new()
    }

    fn inject_connected(&mut self, peer_id: &PeerId) {
        println!("Connected to peer: {:?}", peer_id);

        self.network_actions.push_back(NetworkBehaviourAction::GenerateEvent (
            event: PolkadotProtocolMessage::new(self.welcome_message.clone())
        ))

    }

    fn inject_disconnected(&mut self, peer_id: &PeerId) {
        println!("Disconnected from peer: {:?}", peer_id);
    }


    fn inject_event(&mut self, _: PeerId, connection: ConnectionId, event: PolkadotProtocolEvent) {
        match event {
            PolkadotProtocolEvent::Received(message) => {
                println!("inject_event");
                self.network_actions.push_back(NetworkBehaviourAction::GenerateEvent(message))
            }
            PolkadotProtocolEvent::Sent => {}
        }
    }

    fn poll(&mut self, _: &mut Context, _: &mut impl PollParameters) -> Poll<NetworkBehaviourAction<PolkadotProtocolMessage, String>> {
        match self.network_actions.pop_front() {
            Some(action) => {
                println!("swarm poll");
                Poll::Ready(action)
            }
            None => Poll::Pending
        }
    }
}
