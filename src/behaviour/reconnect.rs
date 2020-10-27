use fnv::FnvHashMap;
use futures::prelude::*;
use libp2p::Multiaddr;
use libp2p::core::connection::{ConnectionId, ListenerId};
use libp2p::core::{ConnectedPoint, either::EitherOutput, PeerId, PublicKey};
use libp2p::swarm::{IntoProtocolsHandler, IntoProtocolsHandlerSelect, ProtocolsHandler};
use libp2p::swarm::{NetworkBehaviour, NetworkBehaviourAction, PollParameters};
use libp2p::identify::{Identify, IdentifyEvent, IdentifyInfo};
use libp2p::ping::{Ping, PingConfig, PingEvent, PingSuccess};
use log::{debug, trace, error};
use smallvec::SmallVec;
use std::{error, io};
use std::collections::hash_map::Entry;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;
use wasm_timer::Instant;
use futures::{stream::unfold, FutureExt, Stream, StreamExt};
use futures_timer::Delay;

pub fn interval(duration: Duration) -> impl Stream<Item = ()> + Unpin {
    unfold((), move |_| Delay::new(duration).map(|_| Some(((), ())))).map(drop)
}

const CACHE_EXPIRE: Duration = Duration::from_secs(10 * 60);
const GARBAGE_COLLECT_INTERVAL: Duration = Duration::from_secs(2 * 60);

pub struct PingBehaviour {
    ping: Ping,
    identify: Identify,
    nodes_info: FnvHashMap<PeerId, PingInfo>,
    garbage_collect: Pin<Box<dyn Stream<Item = ()> + Send>>,
}

#[derive(Debug)]
struct PingInfo {
    info_expire: Option<Instant>,
    endpoints: SmallVec<[ConnectedPoint; crate::MAX_CONNECTIONS_PER_PEER]>,
    client_version: Option<String>,
    latest_ping: Option<Duration>,
}

impl PingInfo {
    fn new(endpoint: ConnectedPoint) -> Self {
        let mut endpoints = SmallVec::new();
        endpoints.push(endpoint);
        PingInfo {
            info_expire: None,
            endpoints,
            client_version: None,
            latest_ping: None,
        }
    }
}

impl PingBehaviour {
    pub fn new(
        user_agent: String,
        local_public_key: PublicKey,
    ) -> Self {
        let identify = {
            let proto_version = "/substrate/1.0".to_string();
            Identify::new(proto_version, user_agent, local_public_key)
        };

        PingBehaviour {
            ping: Ping::new(PingConfig::new()),
            identify,
            nodes_info: FnvHashMap::default(),
            garbage_collect: Box::pin(interval(GARBAGE_COLLECT_INTERVAL)),
        }
    }

    pub fn node(&self, peer_id: &PeerId) -> Option<Node> {
        self.nodes_info.get(peer_id).map(Node)
    }

    fn handle_ping_report(&mut self, peer_id: &PeerId, ping_time: Duration) {
        println!("Ping time with {:?}: {:?}", peer_id, ping_time);
        if let Some(entry) = self.nodes_info.get_mut(peer_id) {
            entry.latest_ping = Some(ping_time);
        } else {
            println!("Received ping from node we're not connected to {:?}", peer_id);
        }
    }

    fn handle_identify_report(&mut self, peer_id: &PeerId, info: &IdentifyInfo) {
        println!("Identified {:?} => {:?}", peer_id, info);
        if let Some(entry) = self.nodes_info.get_mut(peer_id) {
            entry.client_version = Some(info.agent_version.clone());
        } else {
            println!("Received pong from node we're not connected to {:?}", peer_id);
        }
    }
}

pub struct Node<'a>(&'a PingInfo);

impl<'a> Node<'a> {
    pub fn endpoint(&self) -> &'a ConnectedPoint {
        &self.0.endpoints[0]
    }

    pub fn client_version(&self) -> Option<&'a str> {
        self.0.client_version.as_ref().map(|s| &s[..])
    }

    pub fn latest_ping(&self) -> Option<Duration> {
        self.0.latest_ping
    }
}

#[derive(Debug)]
pub enum ioPingEvent {
    Identified {
        peer_id: PeerId,
        info: IdentifyInfo,
    },
}

impl NetworkBehaviour for PingBehaviour {
    type ProtocolsHandler = IntoProtocolsHandlerSelect<
        <Ping as NetworkBehaviour>::ProtocolsHandler,
        <Identify as NetworkBehaviour>::ProtocolsHandler
    >;
    type OutEvent = ioPingEvent;

    fn new_handler(&mut self) -> Self::ProtocolsHandler {
        IntoProtocolsHandler::select(self.ping.new_handler(), self.identify.new_handler())
    }

    fn addresses_of_peer(&mut self, peer_id: &PeerId) -> Vec<Multiaddr> {
        let mut list = self.ping.addresses_of_peer(peer_id);
        list.extend_from_slice(&self.identify.addresses_of_peer(peer_id));
        list
    }

    fn inject_connected(&mut self, peer_id: &PeerId) {
        self.ping.inject_connected(peer_id);
        self.identify.inject_connected(peer_id);
    }

    fn inject_connection_established(&mut self, peer_id: &PeerId, conn: &ConnectionId, endpoint: &ConnectedPoint) {
        self.ping.inject_connection_established(peer_id, conn, endpoint);
        self.identify.inject_connection_established(peer_id, conn, endpoint);
        match self.nodes_info.entry(peer_id.clone()) {
            Entry::Vacant(e) => {
                e.insert(PingInfo::new(endpoint.clone()));
            }
            Entry::Occupied(e) => {
                let e = e.into_mut();
                if e.info_expire.as_ref().map(|exp| *exp < Instant::now()).unwrap_or(false) {
                    e.client_version = None;
                    e.latest_ping = None;
                }
                e.info_expire = None;
                e.endpoints.push(endpoint.clone());
            }
        }
    }

    fn inject_connection_closed(&mut self, peer_id: &PeerId, conn: &ConnectionId, endpoint: &ConnectedPoint) {
        self.ping.inject_connection_closed(peer_id, conn, endpoint);
        self.identify.inject_connection_closed(peer_id, conn, endpoint);

        if let Some(entry) = self.nodes_info.get_mut(peer_id) {
            entry.endpoints.retain(|ep| ep != endpoint)
        } else {
            println!("Unknown connection to {:?} closed: {:?}", peer_id, endpoint);
        }
    }

    fn inject_disconnected(&mut self, peer_id: &PeerId) {
        self.ping.inject_disconnected(peer_id);
        self.identify.inject_disconnected(peer_id);

        if let Some(entry) = self.nodes_info.get_mut(peer_id) {
            entry.info_expire = Some(Instant::now() + CACHE_EXPIRE);
        } else {
            println!("Disconnected from node we were not connected to {:?}", peer_id);
        }
    }

    fn inject_event(
        &mut self,
        peer_id: PeerId,
        connection: ConnectionId,
        event: <<Self::ProtocolsHandler as IntoProtocolsHandler>::Handler as ProtocolsHandler>::OutEvent
    ) {
        match event {
            EitherOutput::First(event) => self.ping.inject_event(peer_id, connection, event),
            EitherOutput::Second(event) => self.identify.inject_event(peer_id, connection, event),
        }
    }

    fn inject_addr_reach_failure(&mut self, peer_id: Option<&PeerId>, addr: &Multiaddr, error: &dyn std::error::Error) {
        self.ping.inject_addr_reach_failure(peer_id, addr, error);
        self.identify.inject_addr_reach_failure(peer_id, addr, error);
    }

    fn inject_dial_failure(&mut self, peer_id: &PeerId) {
        self.ping.inject_dial_failure(peer_id);
        self.identify.inject_dial_failure(peer_id);
    }

    fn inject_new_listen_addr(&mut self, addr: &Multiaddr) {
        self.ping.inject_new_listen_addr(addr);
        self.identify.inject_new_listen_addr(addr);
    }

    fn inject_expired_listen_addr(&mut self, addr: &Multiaddr) {
        self.ping.inject_expired_listen_addr(addr);
        self.identify.inject_expired_listen_addr(addr);
    }

    fn inject_new_external_addr(&mut self, addr: &Multiaddr) {
        self.ping.inject_new_external_addr(addr);
        self.identify.inject_new_external_addr(addr);
    }

    fn inject_listener_error(&mut self, id: ListenerId, err: &(dyn error::Error + 'static)) {
        self.ping.inject_listener_error(id, err);
        self.identify.inject_listener_error(id, err);
    }

    fn inject_listener_closed(&mut self, id: ListenerId, reason: Result<(), &io::Error>) {
        self.ping.inject_listener_closed(id, reason);
        self.identify.inject_listener_closed(id, reason);
    }

    fn poll(
        &mut self,
        cx: &mut Context,
        params: &mut impl PollParameters
    ) -> Poll<
        NetworkBehaviourAction<
            <<Self::ProtocolsHandler as IntoProtocolsHandler>::Handler as ProtocolsHandler>::InEvent,
            Self::OutEvent
        >
    > {
        loop {
            match self.ping.poll(cx, params) {
                Poll::Pending => break,
                Poll::Ready(NetworkBehaviourAction::GenerateEvent(ev)) => {
                    if let ioPingEvent { peer, result: Ok(PingSuccess::Ping { rtt }) } = ev {
                        self.handle_ping_report(&peer, rtt)
                    }
                },
                Poll::Ready(NetworkBehaviourAction::DialAddress { address }) =>
                    return Poll::Ready(NetworkBehaviourAction::DialAddress { address }),
                Poll::Ready(NetworkBehaviourAction::DialPeer { peer_id, condition }) =>
                    return Poll::Ready(NetworkBehaviourAction::DialPeer { peer_id, condition }),
                Poll::Ready(NetworkBehaviourAction::NotifyHandler { peer_id, handler, event }) =>
                    return Poll::Ready(NetworkBehaviourAction::NotifyHandler {
                        peer_id,
                        handler,
                        event: EitherOutput::First(event)
                    }),
                Poll::Ready(NetworkBehaviourAction::ReportObservedAddr { address }) =>
                    return Poll::Ready(NetworkBehaviourAction::ReportObservedAddr { address }),
            }
        }

        loop {
            match self.identify.poll(cx, params) {
                Poll::Pending => break,
                Poll::Ready(NetworkBehaviourAction::GenerateEvent(event)) => {
                    match event {
                        IdentifyEvent::Received { peer_id, info, .. } => {
                            self.handle_identify_report(&peer_id, &info);
                            let event = PingEvent::Identified { peer_id, info };
                            return Poll::Ready(NetworkBehaviourAction::GenerateEvent(event));
                        }
                        IdentifyEvent::Error { peer_id, error } =>
                            println!("Identification with peer {:?} failed => {}", peer_id, error),
                        IdentifyEvent::Sent { .. } => {}
                    }
                },
                Poll::Ready(NetworkBehaviourAction::DialAddress { address }) =>
                    return Poll::Ready(NetworkBehaviourAction::DialAddress { address }),
                Poll::Ready(NetworkBehaviourAction::DialPeer { peer_id, condition }) =>
                    return Poll::Ready(NetworkBehaviourAction::DialPeer { peer_id, condition }),
                Poll::Ready(NetworkBehaviourAction::NotifyHandler { peer_id, handler, event }) =>
                    return Poll::Ready(NetworkBehaviourAction::NotifyHandler {
                        peer_id,
                        handler,
                        event: EitherOutput::Second(event)
                    }),
                Poll::Ready(NetworkBehaviourAction::ReportObservedAddr { address }) =>
                    return Poll::Ready(NetworkBehaviourAction::ReportObservedAddr { address }),
            }
        }

        while let Poll::Ready(Some(())) = self.garbage_collect.poll_next_unpin(cx) {
            self.nodes_info.retain(|_, node| {
                node.info_expire.as_ref().map(|exp| *exp >= Instant::now()).unwrap_or(true)
            });
        }

        Poll::Pending
    }
}