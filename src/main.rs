use std::{env, io};
use async_std::{task};
use futures::prelude::*;
use std::{task::{Context, Poll}, time::Duration};
use bytes::Bytes;
use futures::stream::Stream;
use sp_consensus::block_import::BlockOrigin;
use sp_consensus::import_queue::{IncomingBlock};
use sp_runtime::{ConsensusEngineId,traits::{Block}};
use libp2p::{NetworkBehaviour, InboundUpgradeExt, OutboundUpgradeExt};
use libp2p::{identity,PeerId};
use std::{collections::{VecDeque}};
// https://github.com/libp2p/go-libp2p-pubsub/blob/master/gossipsub.go
use libp2p::{gossipsub};

use libp2p::{
    tcp,
	Transport,
	core::{
		self, either::EitherOutput, muxing::StreamMuxerBox,
		transport::{boxed::Boxed, OptionalTransport}, upgrade,
        either::{EitherError}, ConnectedPoint,
	},
	mplex, bandwidth, noise,
};

use libp2p::swarm::{NetworkBehaviourEventProcess, Swarm, SwarmEvent};

use tokio;
use futures::io::{AsyncRead, AsyncWrite};

use void::Void;

use network_behav::behaviour::reconnect::ReconnectBehaviour;
use network_behav::behaviour::polkadot::PolkadotBehaviour;
use network_behav::node_keys::identity_key::node_key;

use std::{error::Error};
use libp2p::gossipsub::{Gossipsub, GossipsubMessage, MessageId, MessageAuthenticity};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use libp2p::noise::PublicKey;
use libp2p::identity::Keypair::Ed25519;
use libp2p::identity::Keypair;

#[derive(NetworkBehaviour)]
struct MyBehaviour {
    polkadot: PolkadotBehaviour,
    // events: VecDeque<BehaviourOut<B>>,
    //reconnect: ReconnectBehaviour
}

pub enum BehaviourOut<B: Block> {
    BlockImport(BlockOrigin, Vec<IncomingBlock<B>>),
    NotificationsReceived {
        /// Node we received the message from.
        remote: PeerId,
        /// Concerned protocol and associated message.
        messages: Vec<(ConsensusEngineId, Bytes)>,
    },
}

impl MyBehaviour
//where
//    TSubstream: AsyncRead + AsyncWrite + Send + 'static
{
    fn new (welcome_message: String) -> Self {
        MyBehaviour {
            polkadot: PolkadotBehaviour::new(welcome_message),
            //reconnect: ReconnectBehaviour::new()
        }
    }
}

impl NetworkBehaviourEventProcess<String> for MyBehaviour
//where
//    TSubstream: AsyncRead + AsyncWrite + Send + 'static
{
    fn inject_event(&mut self, event: String) {
        println!("Welcome message received: {}", event)
        // self.events.push_back(event)
    }
}

impl NetworkBehaviourEventProcess<Void> for MyBehaviour
//where
//    TSubstream: AsyncRead + AsyncWrite + Send + 'static
{
    fn inject_event(&mut self, _: Void) {}
}

fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<String> = env::args().collect();
    let key_path = PathBuf::from("intergraion.keys");
    let keys = node_key{ file: key_path};
    let id_keys_2 = keys.load_or_generate(false).unwrap();
    let id_keys = identity::Keypair::generate_ed25519();
    let peer_id = PeerId::from(id_keys.public());
    let peer_id_2 = identity::PublicKey::Ed25519(id_keys_2.public()).into_peer_id();

    println!("My ID: {:?}", peer_id.clone());
    println!("Generated ID {:?}", peer_id_2.clone());
    let transport = tcp::TcpConfig::new();
	let message_id_fn = |message: &GossipsubMessage|{
		let mut s = DefaultHasher::new();
		message.data.hash(&mut s);
		MessageId::from(s.finish().to_string())
	};
	// let gossipsub_config = gossipsub::GossipsubConfigBuilder::new().
	// 											  heartbeat_interval(Duration::from_secs(10))
	// 											  .message_id_fn(message_id_fn).build();
	// let mut gossipsub = gossipsub::Gossipsub::new(MessageAuthenticity::Signed(id_keys.clone()),gossipsub_config);
    let behaviour = MyBehaviour::new("Hello!".to_owned());
    let addr = format!("/ip4/127.0.0.1/tcp/8000").parse().unwrap();
    
    let mut listening = false;

    let mut stdin = async_std::io::BufReader::new(async_std::io::stdin()).lines();

	let mut noise_legacy = noise::LegacyConfig::default();
	noise_legacy.send_legacy_handshake = true;

	let authentication_config = {
		let noise_keypair_legacy = noise::Keypair::<noise::X25519>::new().into_authentic(&id_keys.clone())
			.expect("can only fail in case of a hardware bug; since this signing is performed only \
				once and at initialization, we're taking the bet that the inconvenience of a very \
				rare panic here is basically zero");

		let noise_keypair_spec = noise::Keypair::<noise::X25519Spec>::new().into_authentic(&id_keys.clone())
			.expect("can only fail in case of a hardware bug; since this signing is performed only \
				once and at initialization, we're taking the bet that the inconvenience of a very \
				rare panic here is basically zero");

		let mut noise_legacy = noise::LegacyConfig::default();
		noise_legacy.recv_legacy_handshake = true;

		let mut xx_config = noise::NoiseConfig::xx(noise_keypair_spec);
		xx_config.set_legacy_config(noise_legacy.clone());
		let mut ix_config = noise::NoiseConfig::ix(noise_keypair_legacy);
		ix_config.set_legacy_config(noise_legacy);

		let extract_peer_id = |result| match result {
			EitherOutput::First((peer_id, o)) => (peer_id, EitherOutput::First(o)),
			EitherOutput::Second((peer_id, o)) => (peer_id, EitherOutput::Second(o)),
		};

		core::upgrade::SelectUpgrade::new(xx_config.into_authenticated(), ix_config.into_authenticated())
			.map_inbound(extract_peer_id)
			.map_outbound(extract_peer_id)
	};

	let multiplexing_config = {
		let mut mplex_config = mplex::MplexConfig::new();
		mplex_config.max_buffer_len_behaviour(mplex::MaxBufferBehaviour::Block);
		mplex_config.max_buffer_len(usize::MAX);

		let mut yamux_config = libp2p::yamux::Config::default();

		// if use_yamux_flow_control {
		yamux_config.set_window_update_mode(libp2p::yamux::WindowUpdateMode::OnRead);
		// }

		core::upgrade::SelectUpgrade::new(yamux_config, mplex_config)
			.map_inbound(move |muxer| core::muxing::StreamMuxerBox::new(muxer))
			.map_outbound(move |muxer| core::muxing::StreamMuxerBox::new(muxer))
	};

	let transport = transport.upgrade(upgrade::Version::V1)
		.authenticate(authentication_config)
		.multiplex(multiplexing_config)
		.timeout(Duration::from_secs(20))
		.map_err(|err| io::Error::new(io::ErrorKind::Other, err))
		.boxed();

    let mut swarm = Swarm::new(transport, /*gossipsub*/behaviour, peer_id);
    Swarm::listen_on(&mut swarm, addr).unwrap();

/*
    async {
        loop {
            match swarm.next_event().await {
                SwarmEvent::Behaviour(BehaviourOut::BlockImport(origin, blocks)) => {
                    println!("block received");
                }
            }
        }
    };
*/
    task::block_on(future::poll_fn(move |cx: &mut Context<'_>| {

        loop {
            match stdin.try_poll_next_unpin(cx)? {
                Poll::Ready(Some(line)) => println!("Event Received"),
                Poll::Ready(None) => println!("Stdin closed"),
                Poll::Pending => break
            }
        }

        loop {

            match swarm.poll_next_unpin(cx) {

                // Poll::Ready(SwarmEvent::Behaviour(BehaviourOut::BlockImport(origin, blocks)) ) => {
                    // println!("Libp2p => Connected({:?})", peer_id);

                        // let direction = match endpoint {
                        //     ConnectedPoint::Dialer { .. } => "out",
                        //     ConnectedPoint::Listener { .. } => "in",
                        // };

                    // println!("done")
                // }
                Poll::Ready(Some(event)) => println!("Swarm Event Rcvd"),
                Poll::Ready(None) => return Poll::Ready(Ok(())),
                Poll::Pending => {
                    if !listening {
                        if let Some(a) = Swarm::listeners(&swarm).next() {
                            println!("Listening on {:?}", a);
                            listening = true;
                        }
                    }
                    break
                }
            }
        }
        Poll::Pending
    }))

}

    
