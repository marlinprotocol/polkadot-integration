use futures::prelude::*;
use futures_timer::Delay;
use ip_network::IpNetwork;
use libp2p::core::{connection::{ConnectionId, ListenerId}, ConnectedPoint, Multiaddr, PeerId, PublicKey};
use libp2p::swarm::{NetworkBehaviour, NetworkBehaviourAction, PollParameters, ProtocolsHandler};
use libp2p::swarm::protocols_handler::multi::MultiHandler;
use libp2p::kad::{Kademlia, KademliaBucketInserts, KademliaConfig, KademliaEvent, QueryResult, Quorum, Record};
use libp2p::kad::GetClosestPeersError;
use libp2p::kad::handler::KademliaHandler;
use libp2p::kad::QueryId;
use libp2p::kad::record::{self, store::{MemoryStore, RecordStore}};
use libp2p::mdns::{Mdns, MdnsEvent};
use libp2p::multiaddr::Protocol;
use std::{cmp, collections::{HashMap, HashSet, VecDeque}, io, num::NonZeroUsize, time::Duration};
use std::task::{Context, Poll};
use core::{fmt, iter};

#[derive(Clone, PartialEq, Eq, Hash)]
pub struct ProtocolId(smallvec::SmallVec<[u8; 6]>);

impl<'a> From<&'a str> for ProtocolId {
    fn from(bytes: &'a str) -> ProtocolId {
        ProtocolId(bytes.as_bytes().into())
    }
}

impl AsRef<str> for ProtocolId {
    fn as_ref(&self) -> &str {
        str::from_utf8(&self.0[..])
            .expect("the only way to build a ProtocolId is through a UTF-8 String; qed")
    }
}

impl fmt::Debug for ProtocolId {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        fmt::Debug::fmt(self.as_ref(), f)
    }
}


const MAX_KNOWN_EXTERNAL_ADDRESSES: usize = 32;

pub struct DiscoveryConfig {
    local_peer_id: PeerId,
    user_defined: Vec<(PeerId, Multiaddr)>,
    allow_private_ipv4: bool,
    allow_non_globals_in_dht: bool,
    discovery_only_if_under_num: u64,
    enable_mdns: bool,
    kademlia_disjoint_query_paths: bool,
    protocol_ids: HashSet<ProtocolId>,
}

impl DiscoveryConfig {
    pub fn new(local_public_key: PublicKey) -> Self {
        DiscoveryConfig {
            local_peer_id: local_public_key.into_peer_id(),
            user_defined: Vec::new(),
            allow_private_ipv4: true,
            allow_non_globals_in_dht: false,
            discovery_only_if_under_num: std::u64::MAX,
            enable_mdns: false,
            kademlia_disjoint_query_paths: false,
            protocol_ids: HashSet::new()
        }
    }

    pub fn discovery_limit(&mut self, limit: u64) -> &mut Self {
        self.discovery_only_if_under_num = limit;
        self
    }

    pub fn with_user_defined<I>(&mut self, user_defined: I) -> &mut Self
        where
            I: IntoIterator<Item = (PeerId, Multiaddr)>
    {
        self
    }

    pub fn allow_private_ipv4(&mut self, value: bool) -> &mut Self {
        self.allow_private_ipv4 = value;
        self
    }

    pub fn allow_non_globals_in_dht(&mut self, value: bool) -> &mut Self {
        self.allow_non_globals_in_dht = value;
        self
    }

    pub fn with_mdns(&mut self, value: bool) -> &mut Self {
        if value && cfg!(target_os = "unknown") {
            log::warn!(target: "sub-libp2p", "mDNS is not available on this platform")
        }
        self.enable_mdns = value;
        self
    }

    pub fn add_protocol(&mut self, id: ProtocolId) -> &mut Self {
        if self.protocol_ids.contains(&id) {
            warn!(target: "sub-libp2p", "Discovery already registered for protocol {:?}", id);
            return self;
        }

        self.protocol_ids.insert(id);

        self
    }

    pub fn use_kademlia_disjoint_query_paths(&mut self, value: bool) -> &mut Self {
        self.kademlia_disjoint_query_paths = value;
        self
    }

    pub fn finish(self) -> DiscoveryBehaviour {
        let DiscoveryConfig {
            local_peer_id,
            user_defined,
            allow_private_ipv4,
            allow_non_globals_in_dht,
            discovery_only_if_under_num,
            enable_mdns,
            kademlia_disjoint_query_paths,
            protocol_ids,
        } = self;

        let kademlias = protocol_ids.into_iter()
            .map(|protocol_id| {
                let proto_name = protocol_name_from_protocol_id(&protocol_id);

                let mut config = KademliaConfig::default();
                config.set_protocol_name(proto_name);
                config.set_kbucket_inserts(KademliaBucketInserts::Manual);
                config.disjoint_query_paths(kademlia_disjoint_query_paths);

                let store = MemoryStore::new(local_peer_id.clone());
                let mut kad = Kademlia::with_config(local_peer_id.clone(), store, config);

                for (peer_id, addr) in &user_defined {
                    kad.add_address(peer_id, addr.clone());
                }

                (protocol_id, kad)
            })
            .collect();

        DiscoveryBehaviour {
            kademlias,
            next_kad_random_query: Delay::new(Duration::new(0, 0)),
            duration_to_next_kad: Duration::from_secs(1),
            pending_events: VecDeque::new(),
            local_peer_id,
            num_connections: 0,
            allow_private_ipv4,
            discovery_only_if_under_num,
            #[cfg(not(target_os = "unknown"))]
            mdns: if enable_mdns {
                match Mdns::new() {
                    Ok(mdns) => Some(mdns).into(),
                    Err(err) => {
                        warn!(target: "sub-libp2p", "Failed to initialize mDNS: {:?}", err);
                        None.into()
                    }
                }
            } else {
                None.into()
            },
            allow_non_globals_in_dht,
            known_external_addresses: LruHashSet::new(
                NonZeroUsize::new(MAX_KNOWN_EXTERNAL_ADDRESSES)
                    .expect("value is a constant; constant is non-zero; qed.")
            ),
        }
    }
}

pub struct DiscoveryBehaviour {
    user_defined: Vec<(PeerId, Multiaddr)>,
    kademlias: HashMap<ProtocolId, Kademlia<MemoryStore>>,
    next_kad_random_query: Delay,
    duration_to_next_kad: Duration,
    pending_events: VecDeque<DiscoveryOut>,
    local_peer_id: PeerId,
    num_connections: u64,
    allow_private_ipv4: bool,
    discovery_only_if_under_num: u64,
    allow_non_globals_in_dht: bool,
    known_external_addresses: LruHashSet<Multiaddr>,
}

impl DiscoveryBehaviour {
    pub fn known_peers(&mut self) -> HashSet<PeerId> {
        let mut peers = HashSet::new();
        for k in self.kademlias.values_mut() {
            for b in k.kbuckets() {
                for e in b.iter() {
                    if !peers.contains(e.node.key.preimage()) {
                    }
                }
            }
        }
        peers
    }

    pub fn add_known_address(&mut self, peer_id: PeerId, addr: Multiaddr) {
    }

    pub fn add_self_reported_address(
        &mut self,
        peer_id: &PeerId,
        supported_protocols: impl Iterator<Item = impl AsRef<[u8]>>,
        addr: Multiaddr
    ) {
        if !self.allow_non_globals_in_dht && !self.can_add_to_dht(&addr) {
            println!("Ignoring self-reported non-global address {} from {}.", addr, peer_id);
            return
        }

        let mut added = false;
        for protocol in supported_protocols {
            for kademlia in self.kademlias.values_mut() {
                if protocol.as_ref() == kademlia.protocol_name() {
                    println!(
                        "Adding self-reported address {} from {} to Kademlia DHT {}.",
                        addr, peer_id, String::from_utf8_lossy(kademlia.protocol_name()),
                    );
                    added = true;
                }
            }
        }

        if !added {
            println!(
                "Ignoring self-reported address {} from {} as remote node is not part of any \
                 Kademlia DHTs supported by the local node.", addr, peer_id,
            );
        }
    }

    pub fn get_value(&mut self, key: &record::Key) {
        for k in self.kademlias.values_mut() {
            k.get_record(key, Quorum::One);
        }
    }

    pub fn put_value(&mut self, key: record::Key, value: Vec<u8>) {
        for k in self.kademlias.values_mut() {
            if let Err(e) = k.put_record(Record::new(key.clone(), value.clone()), Quorum::All) {
                println!("Libp2p => Failed to put record: {:?}", e);
                self.pending_events.push_back(DiscoveryOut::ValuePutFailed(key.clone(), Duration::from_secs(0)));
            }
        }
    }

    pub fn num_entries_per_kbucket(&mut self) -> impl ExactSizeIterator<Item = (&ProtocolId, Vec<(u32, usize)>)> {
        self.kademlias.iter_mut()
            .map(|(id, kad)| {
                let buckets = kad.kbuckets()
                    .map(|bucket| (bucket.range().0.ilog2().unwrap_or(0), bucket.iter().count()))
                    .collect();
                (id, buckets)
            })
    }

    pub fn num_kademlia_records(&mut self) -> impl ExactSizeIterator<Item = (&ProtocolId, usize)> {
        self.kademlias.iter_mut().map(|(id, kad)| {
            let num = kad.store_mut().records().count();
            (id, num)
        })
    }

    pub fn kademlia_records_total_size(&mut self) -> impl ExactSizeIterator<Item = (&ProtocolId, usize)> {
        self.kademlias.iter_mut().map(|(id, kad)| {
            let size = kad.store_mut().records().fold(0, |tot, rec| tot + rec.value.len());
            (id, size)
        })
    }

    pub fn can_add_to_dht(&self, addr: &Multiaddr) -> bool {
        let ip = match addr.iter().next() {
            Some(Protocol::Ip4(ip)) => IpNetwork::from(ip),
            Some(Protocol::Ip6(ip)) => IpNetwork::from(ip),
            Some(Protocol::Dns(_)) | Some(Protocol::Dns4(_)) | Some(Protocol::Dns6(_))
            => return true,
            _ => return false
        };
        ip.is_global()
    }
}

#[derive(Debug)]
pub enum DiscoveryOut {
    Discovered(PeerId),
    UnroutablePeer(PeerId),
    ValueFound(Vec<(record::Key, Vec<u8>)>, Duration),
    ValueNotFound(record::Key, Duration),
    ValuePut(record::Key, Duration),
    ValuePutFailed(record::Key, Duration),
    RandomKademliaStarted(Vec<ProtocolId>),
}

impl NetworkBehaviour for DiscoveryBehaviour {
    type ProtocolsHandler = MultiHandler<ProtocolId, KademliaHandler<QueryId>>;
    type OutEvent = DiscoveryOut;

    fn new_handler(&mut self) -> Self::ProtocolsHandler {
        let iter = self.kademlias.iter_mut()
            .map(|(p, k)| (p.clone(), NetworkBehaviour::new_handler(k)));

        MultiHandler::try_from_iter(iter)
            .expect("There can be at most one handler per `ProtocolId` and \
                protocol names contain the `ProtocolId` so no two protocol \
                names in `self.kademlias` can be equal which is the only error \
                `try_from_iter` can return, therefore this call is guaranteed \
                to succeed; qed")
    }

    fn addresses_of_peer(&mut self, peer_id: &PeerId) -> Vec<Multiaddr> {
        let mut list = Vec::new();

        {
            let mut list_to_filter = Vec::new();
            for k in self.kademlias.values_mut() {
                list_to_filter.extend(k.addresses_of_peer(peer_id))
            }

            if !self.allow_private_ipv4 {
                list_to_filter.retain(|addr| {
                    if let Some(Protocol::Ip4(addr)) = addr.iter().next() {
                        if addr.is_private() {
                            return false;
                        }
                    }
                    true
                });
            }

            list.extend(list_to_filter);
        }

        println!("Addresses of {:?}: {:?}", peer_id, list);

        list
    }

    fn inject_connection_established(&mut self, peer_id: &PeerId, conn: &ConnectionId, endpoint: &ConnectedPoint) {
        self.num_connections += 1;
        for k in self.kademlias.values_mut() {
            NetworkBehaviour::inject_connection_established(k, peer_id, conn, endpoint)
        }
    }

    fn inject_connected(&mut self, peer_id: &PeerId) {
        for k in self.kademlias.values_mut() {
            NetworkBehaviour::inject_connected(k, peer_id)
        }
    }

    fn inject_connection_closed(&mut self, peer_id: &PeerId, conn: &ConnectionId, endpoint: &ConnectedPoint) {
        self.num_connections -= 1;
        for k in self.kademlias.values_mut() {
            NetworkBehaviour::inject_connection_closed(k, peer_id, conn, endpoint)
        }
    }

    fn inject_disconnected(&mut self, peer_id: &PeerId) {
        for k in self.kademlias.values_mut() {
            NetworkBehaviour::inject_disconnected(k, peer_id)
        }
    }

    fn inject_addr_reach_failure(
        &mut self,
        peer_id: Option<&PeerId>,
        addr: &Multiaddr,
        error: &dyn std::error::Error
    ) {
        for k in self.kademlias.values_mut() {
            NetworkBehaviour::inject_addr_reach_failure(k, peer_id, addr, error)
        }
    }

    fn inject_event(
        &mut self,
        peer_id: PeerId,
        connection: ConnectionId,
        (pid, event): <Self::ProtocolsHandler as ProtocolsHandler>::OutEvent,
    ) {
        if let Some(kad) = self.kademlias.get_mut(&pid) {
            return kad.inject_event(peer_id, connection, event)
        }
        println!(
            "inject_node_event: no kademlia instance registered for protocol {:?}",
            pid)
    }

    fn inject_new_external_addr(&mut self, addr: &Multiaddr) {
        let new_addr = addr.clone()
            .with(Protocol::P2p(self.local_peer_id.clone().into()));

        if self.known_external_addresses.insert(new_addr.clone()) {
            println!(
                  "🔍 Discovered new external address for our node: {}",
                  new_addr,
            );
        }

        for k in self.kademlias.values_mut() {
            NetworkBehaviour::inject_new_external_addr(k, addr)
        }
    }

    fn inject_expired_listen_addr(&mut self, addr: &Multiaddr) {
        for k in self.kademlias.values_mut() {
            NetworkBehaviour::inject_expired_listen_addr(k, addr)
        }
    }

    fn inject_dial_failure(&mut self, peer_id: &PeerId) {
        for k in self.kademlias.values_mut() {
            NetworkBehaviour::inject_dial_failure(k, peer_id)
        }
    }

    fn inject_new_listen_addr(&mut self, addr: &Multiaddr) {
        for k in self.kademlias.values_mut() {
            NetworkBehaviour::inject_new_listen_addr(k, addr)
        }
    }

    fn inject_listener_error(&mut self, id: ListenerId, err: &(dyn std::error::Error + 'static)) {
        for k in self.kademlias.values_mut() {
            NetworkBehaviour::inject_listener_error(k, id, err)
        }
    }

    fn inject_listener_closed(&mut self, id: ListenerId, reason: Result<(), &io::Error>) {
        for k in self.kademlias.values_mut() {
            NetworkBehaviour::inject_listener_closed(k, id, reason)
        }
    }

    fn poll(
        &mut self,
        cx: &mut Context,
        params: &mut impl PollParameters,
    ) -> Poll<
        NetworkBehaviourAction<
            <Self::ProtocolsHandler as ProtocolsHandler>::InEvent,
            Self::OutEvent,
        >,
    > {
        if let Some(ev) = self.pending_events.pop_front() {
            return Poll::Ready(NetworkBehaviourAction::GenerateEvent(ev));
        }

        while let Poll::Ready(_) = self.next_kad_random_query.poll_unpin(cx) {
            let actually_started = if self.num_connections < self.discovery_only_if_under_num {
                let random_peer_id = PeerId::random();
                debug!(target: "sub-libp2p",
                       "Libp2p <= Starting random Kademlia request for {:?}",
                       random_peer_id);
                for k in self.kademlias.values_mut() {
                    k.get_closest_peers(random_peer_id.clone());
                }
                true
            } else {
                debug!(
                    target: "sub-libp2p",
                    "Kademlia paused due to high number of connections ({})",
                    self.num_connections
                );
                false
            };

            self.next_kad_random_query = Delay::new(self.duration_to_next_kad);
            self.duration_to_next_kad = cmp::min(self.duration_to_next_kad * 2,
                                                 Duration::from_secs(60));

            if actually_started {
                let ev = DiscoveryOut::RandomKademliaStarted(self.kademlias.keys().cloned().collect());
                return Poll::Ready(NetworkBehaviourAction::GenerateEvent(ev));
            }
        }

        for (pid, kademlia) in &mut self.kademlias {
            while let Poll::Ready(ev) = kademlia.poll(cx, params) {
                match ev {
                    NetworkBehaviourAction::GenerateEvent(ev) => match ev {
                        KademliaEvent::RoutingUpdated { peer, .. } => {
                            let ev = DiscoveryOut::Discovered(peer);
                            return Poll::Ready(NetworkBehaviourAction::GenerateEvent(ev));
                        }
                        KademliaEvent::UnroutablePeer { peer, .. } => {
                            let ev = DiscoveryOut::UnroutablePeer(peer);
                            return Poll::Ready(NetworkBehaviourAction::GenerateEvent(ev));
                        }
                        KademliaEvent::RoutablePeer { peer, .. } => {
                            let ev = DiscoveryOut::Discovered(peer);
                            return Poll::Ready(NetworkBehaviourAction::GenerateEvent(ev));
                        }
                        KademliaEvent::PendingRoutablePeer { .. } => {
                        }
                        KademliaEvent::QueryResult { result: QueryResult::GetClosestPeers(res), .. } => {
                            match res {
                                Err(GetClosestPeersError::Timeout { key, peers }) => {
                                    debug!(target: "sub-libp2p",
                                           "Libp2p => Query for {:?} timed out with {} results",
                                           HexDisplay::from(&key), peers.len());
                                },
                                Ok(ok) => {
                                    trace!(target: "sub-libp2p",
                                           "Libp2p => Query for {:?} yielded {:?} results",
                                           HexDisplay::from(&ok.key), ok.peers.len());
                                    if ok.peers.is_empty() && self.num_connections != 0 {
                                        debug!(target: "sub-libp2p", "Libp2p => Random Kademlia query has yielded empty \
                                            results");
                                    }
                                }
                            }
                        }
                        KademliaEvent::QueryResult { result: QueryResult::GetRecord(res), stats, .. } => {
                            let ev = match res {
                                Ok(ok) => {
                                    let results = ok.records
                                        .into_iter()
                                        .map(|r| (r.record.key, r.record.value))
                                        .collect();

                                    DiscoveryOut::ValueFound(results, stats.duration().unwrap_or_else(Default::default))
                                }
                                Err(e @ libp2p::kad::GetRecordError::NotFound { .. }) => {
                                    trace!(target: "sub-libp2p",
                                           "Libp2p => Failed to get record: {:?}", e);
                                    DiscoveryOut::ValueNotFound(e.into_key(), stats.duration().unwrap_or_else(Default::default))
                                }
                                Err(e) => {
                                    warn!(target: "sub-libp2p",
                                          "Libp2p => Failed to get record: {:?}", e);
                                    DiscoveryOut::ValueNotFound(e.into_key(), stats.duration().unwrap_or_else(Default::default))
                                }
                            };
                            return Poll::Ready(NetworkBehaviourAction::GenerateEvent(ev));
                        }
                        KademliaEvent::QueryResult { result: QueryResult::PutRecord(res), stats, .. } => {
                            let ev = match res {
                                Ok(ok) => DiscoveryOut::ValuePut(ok.key, stats.duration().unwrap_or_else(Default::default)),
                                Err(e) => {
                                    warn!(target: "sub-libp2p",
                                          "Libp2p => Failed to put record: {:?}", e);
                                    DiscoveryOut::ValuePutFailed(e.into_key(), stats.duration().unwrap_or_else(Default::default))
                                }
                            };
                            return Poll::Ready(NetworkBehaviourAction::GenerateEvent(ev));
                        }
                        KademliaEvent::QueryResult { result: QueryResult::RepublishRecord(res), .. } => {
                            match res {
                                Ok(ok) => debug!(target: "sub-libp2p",
                                                 "Libp2p => Record republished: {:?}",
                                                 ok.key),
                                Err(e) => warn!(target: "sub-libp2p",
                                                "Libp2p => Republishing of record {:?} failed with: {:?}",
                                                e.key(), e)
                            }
                        }
                        e => {
                            warn!(target: "sub-libp2p", "Libp2p => Unhandled Kademlia event: {:?}", e)
                        }
                    }
                    NetworkBehaviourAction::DialAddress { address } =>
                        return Poll::Ready(NetworkBehaviourAction::DialAddress { address }),
                    NetworkBehaviourAction::DialPeer { peer_id, condition } =>
                        return Poll::Ready(NetworkBehaviourAction::DialPeer { peer_id, condition }),
                    NetworkBehaviourAction::NotifyHandler { peer_id, handler, event } =>
                        return Poll::Ready(NetworkBehaviourAction::NotifyHandler {
                            peer_id,
                            handler,
                            event: (pid.clone(), event)
                        }),
                    NetworkBehaviourAction::ReportObservedAddr { address } =>
                        return Poll::Ready(NetworkBehaviourAction::ReportObservedAddr { address }),
                }
            }
        }

        #[cfg(not(target_os = "unknown"))]
        while let Poll::Ready(ev) = self.mdns.poll(cx, params) {
            match ev {
                NetworkBehaviourAction::GenerateEvent(event) => {
                    match event {
                        MdnsEvent::Discovered(list) => {
                            if self.num_connections >= self.discovery_only_if_under_num {
                                continue;
                            }

                            self.pending_events.extend(list.map(|(peer_id, _)| DiscoveryOut::Discovered(peer_id)));
                            if let Some(ev) = self.pending_events.pop_front() {
                                return Poll::Ready(NetworkBehaviourAction::GenerateEvent(ev));
                            }
                        },
                        MdnsEvent::Expired(_) => {}
                    }
                },
                NetworkBehaviourAction::DialAddress { address } =>
                    return Poll::Ready(NetworkBehaviourAction::DialAddress { address }),
                NetworkBehaviourAction::DialPeer { peer_id, condition } =>
                    return Poll::Ready(NetworkBehaviourAction::DialPeer { peer_id, condition }),
                NetworkBehaviourAction::NotifyHandler { event, .. } =>
                    match event {},     // `event` is an enum with no variant
                NetworkBehaviourAction::ReportObservedAddr { address } =>
                    return Poll::Ready(NetworkBehaviourAction::ReportObservedAddr { address }),
            }
        }

        Poll::Pending
    }
}

fn protocol_name_from_protocol_id(id: &ProtocolId) -> Vec<u8> {
    let mut v = vec![b'/'];
    v.extend_from_slice(id.as_ref().as_bytes());
    v.extend_from_slice(b"/kad");
    v
}