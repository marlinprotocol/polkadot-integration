use std::collections::{VecDeque, HashMap};
use std::marker::PhantomData;
use bytes::Bytes;
use futures::{task::{Context,Poll},future::BoxFuture,stream::FuturesUnordered};
use libp2p::{PeerId};
use libp2p::multiaddr::Multiaddr;
use libp2p::core::ConnectedPoint;
use libp2p::core::connection::ConnectionId;
use libp2p::swarm::{NetworkBehaviour, NetworkBehaviourAction, PollParameters, OneShotHandler, NegotiatedSubstream, OneShotHandlerConfig, SubstreamProtocol, NotifyHandler};
use sp_runtime::traits::Block;
use std::{io,time::Duration};

use futures::io::{AsyncRead, AsyncWrite};
use std::time::Instant;
use futures_timer::Delay;

use super::super::protocol::net_protocol::{PolkadotProtocolEvent, PolkadotProtocolsHandler, PolkadotOutProtocol, BlockRequest, BlockResponse};
use crate::protocol::net_protocol::{PolkadotInProtocol, build_protobuf_block_request};

#[derive(Debug)]
pub enum ioEvent<B: Block> {
    AnsweredRequest {
        peer: PeerId,
        total_handling_time: Duration,
    },

    Response {
        peer: PeerId,
        original_request: BlockRequest<B>,
        response: BlockResponse<B>,
        request_duration: Duration,
    },

    RequestTimeout {
        peer: PeerId,
        original_request: BlockRequest<B>,
        request_duration: Duration,
    }
}

#[derive(Debug)]
struct OngoingRequest<B: Block> {
    emitted: Instant,
    request: message::BlockRequest<B>,
    timeout: Delay,
}

#[derive(Debug)]
struct Connection<B: Block> {
    id: ConnectionId,
    ongoing_request: Option<OngoingRequest<B>>,
}

#[derive(Debug)]
#[must_use]
pub enum SendRequestOutcome<B: Block> {
    Ok,
    Replaced {
        previous: BlockRequest<B>,
        request_duration: Duration,
    },
    NotConnected,
    EncodeError(prost::EncodeError),
}

pub struct PolkadotBehaviour<B: Block> {
    welcome_message: String,
    network_actions: VecDeque<NetworkBehaviourAction<PolkadotOutProtocol<B>, ioEvent<B>>>,
    peer: HashMap<PeerId, Vec<Connection<B>>>,
    outgoing: FuturesUnordered<BoxFuture<'static, (PeerId, Duration)>>,
}

impl<B: Block> PolkadotBehaviour<B> {
    pub fn new(welcome_message: String) -> Self {
        PolkadotBehaviour {
            welcome_message,
            network_actions: VecDeque::new(),
            peer: HashMap::new(),
            outgoing: FuturesUnordered::new(),
        }
    }

    pub fn send_request(&mut self, target: &PeerId, req: BlockRequest<B>) -> SendRequestOutcome<B> {

        let connection = if let Some(peer) = self.peer.get_mut(target) {
            if let Some(entry) = peer.iter_mut().find(|c| c.ongoing_request.is_some()) {
                entry
            } else if let Some(entry) = peer.get_mut(0) {
                entry
            } else {
                println!(
                    "State inconsistency: empty list of peer connections"
                );
                return SendRequestOutcome::NotConnected;
            }
        } else {
            return SendRequestOutcome::NotConnected;
        };

        let protobuf_rq = build_protobuf_block_request(
            req.fields,
            req.from.clone(),
            req.to.clone(),
            req.direction,
            req.max,
        );

        let mut buf = Vec::with_capacity(protobuf_rq.encoded_len());
        if let Err(err) = protobuf_rq.encode(&mut buf) {
            println!(
                "Failed to encode block request {:?}: {:?}",
                protobuf_rq,
                err
            );
            return SendRequestOutcome::EncodeError(err);
        }

        let previous_request = connection.ongoing_request.take();
        connection.ongoing_request = Some(OngoingRequest {
            emitted: Instant::now(),
            request: req.clone(),
            timeout: Delay::new(Duration::from_secs(40)),
        });

        println!("Enqueueing block request to {:?}: {:?}", target, protobuf_rq);
        self.network_actions.push_back(NetworkBehaviourAction::NotifyHandler {
            peer_id: target.clone(),
            handler: NotifyHandler::One(connection.id),
            event: PolkadotOutProtocol {
                block_protobuf_request: buf,
                recv_request: req,
            },
        });

        if let Some(previous_request) = previous_request {
            println!(
                "Replacing existing block request on connection {:?}",
                connection.id
            );
            SendRequestOutcome::Replaced {
                previous: previous_request.request,
                request_duration: previous_request.emitted.elapsed(),
            }
        } else {
            SendRequestOutcome::Ok
        }
    }
}

impl<B: Block> NetworkBehaviour for PolkadotBehaviour<B>
{
    type ProtocolsHandler = OneShotHandler<PolkadotInProtocol<B>,PolkadotOutProtocol<B>,PolkadotProtocolEvent<B,NegotiatedSubstream>>;
    type OutEvent = ioEvent<B>;

    fn new_handler(&mut self) -> Self::ProtocolsHandler {
        println!("new_handler");
        let inProtocol = PolkadotInProtocol{
            marker: PhantomData,
        };
        let mut cfg = OneShotHandlerConfig::default();
        cfg.keep_alive_timeout = Duration::from_secs(15);
        cfg.outbound_substream_timeout = Duration::from_secs(40);
        OneShotHandler::new(SubstreamProtocol::new(inProtocol,()),cfg);
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
