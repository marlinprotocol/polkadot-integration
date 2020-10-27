use std::collections::VecDeque;
use std::marker::PhantomData;

use futures::task::{Context,Poll};

use libp2p::PeerId;
use libp2p::multiaddr::Multiaddr;
use libp2p::core::ConnectedPoint;
use libp2p::core::connection::ConnectionId;
use libp2p::swarm::protocols_handler::DummyProtocolsHandler;
use libp2p::swarm::{NetworkBehaviour, NetworkBehaviourAction, PollParameters};

use tokio_io::{AsyncRead, AsyncWrite};

use void::Void;

// Network behaviour that re-connects to dialed nodes if the connection is dropped
pub struct ReconnectBehaviour {
    network_actions: VecDeque<NetworkBehaviourAction<Void, Void>>,
    //_marker: PhantomData<TSubstream>
}

impl ReconnectBehaviour {
    pub fn new() -> ReconnectBehaviour {
        ReconnectBehaviour {
            network_actions: VecDeque::new(),
            //_marker: PhantomData
        }
    }
}

impl NetworkBehaviour for ReconnectBehaviour
//where TSubstream : AsyncRead + AsyncWrite + Send + 'static
{
    type ProtocolsHandler = DummyProtocolsHandler;
    type OutEvent = Void;

    fn new_handler(&mut self) -> Self::ProtocolsHandler {
        DummyProtocolsHandler::default()
    }

    fn addresses_of_peer(&mut self, _: &PeerId) -> Vec<Multiaddr> {
        Vec::new()
    }

    fn inject_connected(&mut self, peer_id : &PeerId) {
        log::trace!("inject_disconnected {}", peer_id.to_base58());
    }

    fn inject_disconnected(&mut self, peer_id: &PeerId) {
        log::trace!("inject_disconnected {}", peer_id.to_base58());
        //self.network_actions.push_back(NetworkBehaviourAction::DialAddress { address })
    }

    fn inject_event(
        &mut self,
        _ : PeerId,
        _ : ConnectionId,
        _ : Void,
    ) {
        
    }

    fn poll(&mut self, _: &mut Context, _: &mut impl PollParameters) -> Poll<NetworkBehaviourAction<Void, Void>> {
        match self.network_actions.pop_front() {
            Some(action) => Poll::Ready(action),
            None => Poll::Pending
        }
    }
}
