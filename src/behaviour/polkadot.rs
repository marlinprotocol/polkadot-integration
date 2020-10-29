use std::collections::{VecDeque, HashMap};
use std::marker::PhantomData;
use bytes::Bytes;
use futures::{task::{Context,Poll},future::BoxFuture,stream::FuturesUnordered};
use libp2p::{PeerId};
use libp2p::multiaddr::Multiaddr;
use libp2p::core::ConnectedPoint;
use libp2p::core::connection::ConnectionId;
use libp2p::swarm::{NetworkBehaviour, NetworkBehaviourAction, PollParameters, OneShotHandler, NegotiatedSubstream, OneShotHandlerConfig, SubstreamProtocol, NotifyHandler};
use sp_runtime::{traits::Block,generic::BlockId};
use std::{io,time::Duration};
use codec::{Encode, Decode, Input, Output};

use futures::io::{AsyncRead, AsyncWrite};
use std::time::Instant;
use futures_timer::Delay;

use super::super::protocol::net_protocol::{PolkadotProtocolEvent, PolkadotOutProtocol, BlockRequest, BlockResponse, BlockData, BlockAttributes};
use crate::protocol::net_protocol::{PolkadotInProtocol, build_protobuf_block_request};
use libp2p::core::upgrade::write_one;
use std::pin::Pin;
use crate::schema;
use prost::Message;
use prost;
use futures::FutureExt;
use futures::Future;
use futures::StreamExt;

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
    request: BlockRequest<B>,
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

pub type Error = Box<dyn std::error::Error + 'static>;

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

    fn on_block_request
    ( &mut self
      , peer: &PeerId
      , request: &schema::v1::BlockRequest
    ) -> Result<schema::v1::BlockResponse, Error>
    {
        println!("Block request from peer {}", peer);

        let from_block_id =
            match request.from_block {
                Some(schema::v1::block_request::FromBlock::Hash(ref h)) => {
                    let h = Decode::decode(&mut h.as_ref())?;
                    BlockId::<B>::Hash(h)
                }
                Some(schema::v1::block_request::FromBlock::Number(ref n)) => {
                    let n = Decode::decode(&mut n.as_ref())?;
                    BlockId::<B>::Number(n)
                }
                None => {
                    let msg = "missing `BlockRequest::from_block` field";
                    return Err(io::Error::new(io::ErrorKind::Other, msg).into())
                }
            };
        let mut blocks = Vec::new();
        Ok(schema::v1::BlockResponse { blocks })
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
        OneShotHandler::new(SubstreamProtocol::new(inProtocol,()),cfg)
    }


    fn addresses_of_peer(&mut self, _: &PeerId) -> Vec<Multiaddr> {
        Vec::new()
    }

    fn inject_connected(&mut self, peer_id: &PeerId) {
        println!("Connected to peer: {:?}", peer_id);
    }

    fn inject_disconnected(&mut self, peer_id: &PeerId) {
        println!("Disconnected from peer: {:?}", peer_id);
    }

    fn inject_connection_established(&mut self, peer_id: &PeerId, id: &ConnectionId, _: &ConnectedPoint){
        self.peer.entry(peer_id.clone()).
            or_default()
            .push(Connection{
                id:*id,
                ongoing_request: None,
            });
    }

    fn inject_event(
        &mut self,
        peer: PeerId,
        connection_id: ConnectionId,
        node_event: PolkadotProtocolEvent<B,NegotiatedSubstream>
    ) {
        match node_event {
            PolkadotProtocolEvent::Request(request, mut stream, handling_start) => {
                match self.on_block_request(&peer, &request) {
                    Ok(res) => {
                        println!("Enqueueing block response for peer {} with {} blocks", peer, res.blocks.len());
                        let mut data = Vec::with_capacity(res.encoded_len());
                        if let Err(e) = res.encode(&mut data) {
                            println!(
                                "Error encoding block response for peer {}: {}",
                                peer, e
                            )
                        } else {
                            self.outgoing.push((async move {
                                if let Err(e) = write_one(&mut stream, data).await {
                                    println!("Error writing block response: {}",
                                             e
                                    );
                                }
                                (peer, handling_start.elapsed())
                            }).boxed());
                        }
                    }
                    Err(e) => println!(
                        "Error handling block request from peer {}: {}", peer, e
                    )
                }
            }
            PolkadotProtocolEvent::Response(original_request,response) => {
                println!("Received block response from peer {} with {} blocks", peer, response.blocks.len());

                let blocks = response.blocks.into_iter().map(|block_data| {
                    Ok(BlockData::<B> {
                        hash: Decode::decode(&mut block_data.hash.as_ref())?,
                        header: if !block_data.header.is_empty() {
                            Some(Decode::decode(&mut block_data.header.as_ref())?)
                        } else {
                            None
                        },
                        body: if original_request.fields.contains(BlockAttributes::BODY) {
                            Some(block_data.body.iter().map(|body| {
                                Decode::decode(&mut body.as_ref())
                            }).collect::<Result<Vec<_>, _>>()?)
                        } else {
                            None
                        },
                        receipt: if !block_data.message_queue.is_empty() {
                            Some(block_data.receipt)
                        } else {
                            None
                        },
                        message_queue: if !block_data.message_queue.is_empty() {
                            Some(block_data.message_queue)
                        } else {
                            None
                        },
                        justification: if !block_data.justification.is_empty() {
                            Some(block_data.justification)
                        } else if block_data.is_empty_justification {
                            Some(Vec::new())
                        } else {
                            None
                        },
                    })
                }).collect::<Result<Vec<_>, codec::Error>>();

                match blocks {
                    Ok(blocks) => {
                        let id = original_request.id;
                        let e = ioEvent::Response {
                            peer: peer,
                            original_request,
                            response: BlockResponse::<B> { id, blocks },
                        };
                        self.network_actions.push_back(NetworkBehaviourAction::GenerateEvent(e));
                    }
                    Err(err) => {
                        println!("Failed to decode block response from peer {}: {}", peer, err)
                    }
                }
            }
        }
    }

    fn poll(&mut self, cx : &mut Context, _: &mut impl PollParameters) -> Poll<NetworkBehaviourAction<PolkadotOutProtocol<B>, ioEvent<B>>> {
        if let Some(ev) = self.network_actions.pop_front() {
            return Poll::Ready(ev);
        }

        for (peer, connections) in &mut self.peer {
            for connection in connections {
                let ongoing_request = match &mut connection.ongoing_request {
                    Some(rq) => rq,
                    None => continue,
                };

                if let Poll::Ready(_) = Pin::new(&mut ongoing_request.timeout).poll(cx) {
                    let original_request = ongoing_request.request.clone();
                    let request_duration = ongoing_request.emitted.elapsed();
                    connection.ongoing_request = None;
                    println!(
                        "Request timeout for {}: {:?}",
                        peer, original_request
                    );
                    let ev = ioEvent::RequestTimeout {
                        peer: peer.clone(),
                        original_request,
                        request_duration,
                    };
                    return Poll::Ready(NetworkBehaviourAction::GenerateEvent(ev));
                }
            }
        }

        if let Poll::Ready(Some((peer, total_handling_time))) = self.outgoing.poll_next_unpin(cx) {
            let ev = ioEvent::AnsweredRequest {
                peer,
                total_handling_time,
            };
            return Poll::Ready(NetworkBehaviourAction::GenerateEvent(ev));
        }

        Poll::Pending
    }

}
