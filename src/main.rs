use tokio::io;
use tokio::task;
use std::error::Error;
use libp2p::OutboundUpgradeExt;
use libp2p::InboundUpgradeExt;
use std::time::Duration;
use libp2p::Transport;
use futures;
use std::sync::Arc;
use futures::io::{AsyncReadExt, AsyncWriteExt};
use libp2p::mplex;
use libp2p::tcp;
use libp2p::noise;
use libp2p::core;
use libp2p::core::muxing::{StreamMuxerBox, StreamMuxerEvent};
use libp2p::core::muxing::{event_from_ref_and_wrap, outbound_from_ref_and_wrap};
use libp2p::core::transport::ListenerEvent;
use libp2p::core::either::EitherOutput;
use libp2p::core::upgrade;
use libp2p::identity;
use futures::{StreamExt};
use gateway_dot::node_keys::identity_key::NodeKey;
use std::path::PathBuf;
use sp_runtime::generic::Header;
use sp_runtime::traits::BlakeTwo256;
use codec::{Encode, Decode, Input, Output};
use tokio::prelude::*;
use gateway_dot::api;
use prost::Message;
use libp2p::bytes::BufMut;


/// Block state in the chain.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum BlockState {
	/// Block is not part of the best chain.
	Normal,
	/// Latest best block.
	Best,
}

impl Encode for BlockState {
	fn encode_to<T: Output>(&self, dest: &mut T) {
		match self {
			BlockState::Normal => dest.push_byte(0x00),
			BlockState::Best => dest.push_byte(0x01),
		}
	}
}

impl Decode for BlockState {
	fn decode<I: Input>(input: &mut I) -> Result<Self, codec::Error> {
		match input.read_byte()? {
			0 => Ok(BlockState::Normal),
			1 => Ok(BlockState::Best),
			_ => Err(codec::Error::from("No variant")),
		}
	}
}

/// Announce a new complete relay chain block on the network.
#[derive(Debug, PartialEq, Eq, Clone)]
pub struct BlockAnnounce {
	/// New block header.
	pub header: Header<u32, BlakeTwo256>,
	/// Block state. TODO: Remove `Option` and custom encoding when v4 becomes common.
	pub state: Option<BlockState>,
	/// Data associated with this block announcement, e.g. a candidate message.
	pub data: Option<Vec<u8>>,
}

// Custom Encode/Decode impl to maintain backwards compatibility with v3.
// This assumes that the packet contains nothing but the announcement message.
// TODO: Get rid of it once protocol v4 is common.
impl Encode for BlockAnnounce {
	fn encode_to<T: Output>(&self, dest: &mut T) {
		self.header.encode_to(dest);
		if let Some(state) = &self.state {
			state.encode_to(dest);
		}
		if let Some(data) = &self.data {
			data.encode_to(dest)
		}
	}
}

impl Decode for BlockAnnounce {
	fn decode<I: Input>(input: &mut I) -> Result<Self, codec::Error> {
		let header = Header::decode(input)?;
		let state = BlockState::decode(input).ok();
		let data = Decode::decode(input).ok();
		Ok(BlockAnnounce {
			header,
			state,
			data,
		})
	}
}

fn spawn_bridge_task() -> tokio::sync::mpsc::Sender<Vec<u8>> {
	// MPSC queue for bridge connection
	let (tx, mut rx) = tokio::sync::mpsc::channel::<Vec<u8>>(100);

	// Bridge connection
	task::spawn(async move {
		struct Backoff {
			timeout: u64,
		};

		impl Backoff {
			fn new() -> Backoff {
				Backoff { timeout: 1 }
			}

			async fn run(&mut self) {
				tokio::time::delay_for(Duration::from_secs(self.timeout)).await;
				self.timeout *= 2;
				if self.timeout > 64 {
					self.timeout = 64;
				}
			}
		}

		let mut backoff = Backoff::new();

		loop {
			println!("Connecting to bridge");
			if let Ok(mut stream) = tokio::net::TcpStream::connect("127.0.0.1:15003").await {
				loop {
					let msg = rx.recv().await.unwrap();
					if let Err(_) = stream.write_u64(msg.len() as u64).await {
						backoff.run().await;
						break;
					}

					if let Err(_) = stream.write_all(&msg).await {
						backoff.run().await;
						break;
					}
				}
			} else {
				backoff.run().await;
			}
		}
	});

	return tx;
}


fn spawn_block_requester(smux: Arc<StreamMuxerBox>) -> tokio::sync::mpsc::Sender<BlockAnnounce>
{
	// MPSC queue for block request stream
	let (tx, mut rx) = tokio::sync::mpsc::channel::<BlockAnnounce>(100);
	task::spawn(async move {
		loop {
			let smux = smux.clone();
			let mut buf = Box::new([0u8; 1000]);

			println!("block req outbound start");

			// Open a block request stream
			let mut block_req = outbound_from_ref_and_wrap(smux).await.unwrap();

			println!("block req outbound stream");

			block_req.write_all(b"\x13/multistream/1.0.0\n").await.unwrap();
			block_req.write_all(b"\x0c/dot/sync/2\n").await.unwrap();

			let size = block_req.read(&mut buf[..]).await.unwrap();
			// println!("Proto message: {:?}", &buf[0..size]);
			assert!(size == 33, "block req handshake failure");

			let len: usize = buf[20].into();
			let proto = std::str::from_utf8(&buf[21..21+len-1]).unwrap();

			println!("proto: {}", proto);

			let ann = rx.recv().await.unwrap();

			// println!("{:?}", ann);

			let br = api::BlockRequest {
				fields: 31u32.to_be(),
				from_block: Some(api::block_request::FromBlock::Hash(ann.header.hash().encode())),
				to_block: ann.header.hash().encode(),
				direction: 0,
				max_blocks: 1,
			};

			let mut buf = vec![];
			br.encode(&mut buf).unwrap();

			println!("Buf: {}, {}: {:?}", br.encoded_len(), buf.len(), &buf[..]);
			block_req.write_all(&[buf.len() as u8]).await.unwrap();
			block_req.write_all(&mut buf[..]).await.unwrap();
			// block_req.shutdown().await.unwrap();

			let size = block_req.read(&mut buf[..]).await.unwrap();
			println!("Block message: {:?}", &buf[0..size]);

			// if let Err(_) = stream.write_u64(msg.len() as u64).await {
			// 	backoff.run().await;
			// 	break;
			// }

			// if let Err(_) = stream.write_all(&msg).await {
			// 	backoff.run().await;
			// 	break;
			// }
		}
	});

	return tx;
}

async fn lin_main() -> Result<(), Box<dyn Error>> {
	// Private and public keys configuration.
	let key_path = PathBuf::from("gateway_dot.key");
	let keys = NodeKey { file: key_path };
	let local_identity = identity::Keypair::Ed25519(keys.load_or_generate(false).unwrap());
	let local_public = local_identity.public();
	let local_peer_id = local_public.clone().into_peer_id();
	println!("🏷 Local node identity is: {}", local_peer_id.to_base58());

	let transport = tcp::TcpConfig::new();

	let mut noise_legacy = noise::LegacyConfig::default();
	noise_legacy.send_legacy_handshake = true;

	let authentication_config = {
		let noise_keypair_legacy = noise::Keypair::<noise::X25519>::new().into_authentic(&local_identity)
			.expect("can only fail in case of a hardware bug; since this signing is performed only \
				once and at initialization, we're taking the bet that the inconvenience of a very \
				rare panic here is basically zero");

		let noise_keypair_spec = noise::Keypair::<noise::X25519Spec>::new().into_authentic(&local_identity)
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

		yamux_config.set_window_update_mode(libp2p::yamux::WindowUpdateMode::OnRead);

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

	let mut listener = transport.listen_on("/ip4/127.0.0.1/tcp/8000".parse()?)?;

	task::spawn(async move {
		let tx = spawn_bridge_task();

		loop {
			let le = listener.next().await.unwrap().unwrap();
			match le {
				ListenerEvent::NewAddress(_multiaddr) => {
					println!("New multiaddr");
				}
				ListenerEvent::Upgrade {
					/// The upgrade.
					upgrade,
					/// The local address which produced this upgrade.
					local_addr: _,
					/// The remote address which produced this upgrade.
					remote_addr: _,
				} => {
					// println!("New upgrade");
					let tx = tx.clone();
					task::spawn(async move {
						let smux = Arc::new(upgrade.await.unwrap().1);

						loop {
							let event = event_from_ref_and_wrap(smux.clone()).await.unwrap();
							match event {
								StreamMuxerEvent::InboundSubstream(mut sst) => {
									// task::sleep(Duration::from_secs(1)).await;
									let mut tx = tx.clone();
									let smux = smux.clone();
									task::spawn(async move {
										let mut buf = Box::new([0u8; 1000000]);

										let size = sst.read(&mut buf[..]).await.unwrap();
										// println!("Proto message: {:?}", &buf[0..size]);

										let len: usize = buf[20].into();
										let proto = std::str::from_utf8(&buf[21..21+len-1]).unwrap();

										// println!("proto: {}", proto);

										if proto == "/ipfs/ping/1.0.0" {
											sst.write(&buf[0..size]).await.unwrap();
											loop {
												// println!("Ping message: {:?}", &buf[0..size]);
												// println!("Ping message");
												// task::sleep(Duration::from_secs(5)).await;
												let size = sst.read(&mut buf[..]).await.unwrap();

												if size == 0 {
													// println!("Ping rekt");
													break
												}

												sst.write(&buf[0..size]).await.unwrap();
											}
										} else if proto == "/dot/block-announces/1" {
											sst.write(&buf[0..size]).await.unwrap();

											// Handshake
											let mut size = sst.read(&mut buf[..]).await.unwrap();
											let expected: usize = buf[0].into();
											let mut idx: usize = size;
											while idx < expected + 1 {
												let size = sst.read(&mut buf[idx..]).await.unwrap();
												idx += size;
											}
											size = expected + 1;
											sst.write(&buf[0..size]).await.unwrap();
											
											// Block requester
											let mut brtx = spawn_block_requester(smux.clone());

											loop {

												// println!("Block announce message");
												// task::sleep(Duration::from_secs(5)).await;
												let size = sst.read(&mut buf[..]).await.unwrap();
												// println!("Block announce message: {} bytes: {:?}", size, &buf[0..size]);
												if size == 0 {
													// println!("Block announce rekt");
													break
												}

												tx.send(buf[0..size].iter().cloned().collect()).await.unwrap();
												sst.write(&[0]).await.unwrap();

												// Decode announcement
												let mut idx: usize = 0;
												let mut len: usize = 0;
												while buf[idx] > 128 {
													len = len*128 + buf[idx] as usize;
													idx += 1;
												}
												len = len*128 + buf[idx] as usize;
												idx += 1;
												let announce = BlockAnnounce::decode(&mut &buf[idx..idx+len]);
												match announce {
													Ok(announce) => {
														println!("New block announce: {}", announce.header.number);
														brtx.send(announce).await.unwrap();
													},
													Err(err) => println!("Block announce error: {}", err.what()),
												};
											}
										} else if proto == "ls" {
											loop {
												// println!("Ls message: {:?}", &buf[0..size]);
												// println!("Ls message");
												// task::sleep(Duration::from_secs(5)).await;
												// sst.write(&[19u8]).await.unwrap();
												// sst.write(b"/multistream/1.0.0\n").await.unwrap();
												let protos = [
													// b"/multistream/1.0.0\n".to_vec(),
													b"/ipfs/ping/1.0.0\n".to_vec(),
													// b"/ipfs/id/1.0.0\n".to_vec(),
													// b"/dot/kad\n".to_vec(),
													b"/dot/block-announces/1\n".to_vec(),
													b"/dot/transactions/1\n".to_vec(),
													b"/paritytech/grandpa/1\n".to_vec(),
												];

												let mut combined = vec![0u8; 0];
												for p in protos.iter() {
													combined.push(p.len() as u8);
													combined.append(&mut p.clone());
													// sst.write(&[p.len() as u8]).await.unwrap();
													// sst.write(p).await.unwrap();
												}

												// sst.write(&combined).await.unwrap();
												let mut temp = vec![combined.len() as u8 + 2, protos.len() as u8];
												temp.append(&mut combined);
												temp.push('\n' as u8);
												// println!("Temp message: {:?}", &temp[..]);
												// println!("Temp message");
												sst.write(&temp).await.unwrap();

												let size = sst.read(&mut buf[..]).await.unwrap();

												if size == 0 {
													// println!("Ls rekt");
													break
												}
											}
										} else if proto == "/dot/transactions/1" {
											sst.write(&buf[0..size]).await.unwrap();

											// Handshake
											let size = sst.read(&mut buf[..]).await.unwrap();
											sst.write(&buf[0..size]).await.unwrap();

											loop {
												// println!("Tx message: {:?}", &buf[0..size]);
												// println!("Tx message");
												// task::sleep(Duration::from_secs(5)).await;
												let size = sst.read(&mut buf[..]).await.unwrap();

												if size == 0 {
													// println!("Tx rekt");
													break
												}

												// sst.write(&buf[0..size]).await.unwrap();
											}
										} else if proto == "/paritytech/grandpa/1" {
											sst.write(&buf[0..size]).await.unwrap();

											// Handshake
											let size = sst.read(&mut buf[..]).await.unwrap();
											sst.write(&buf[0..size]).await.unwrap();

											loop {
												// println!("Grandpa message: {:?}", &buf[0..size]);
												// println!("Grandpa message");
												// task::sleep(Duration::from_secs(5)).await;
												let size = sst.read(&mut buf[..]).await.unwrap();
												// println!("Grandpa message: {:?}", &buf[0..size]);
												if size == 0 {
													// println!("Grandpa rekt");
													break
												}

												// sst.write(&buf[0..size]).await.unwrap();
											}
										// } else if proto == "/dot/kad" {
										// 	sst.write(&buf[0..size]).await.unwrap();
										// 	loop {
										// 		// task::sleep(Duration::from_secs(5)).await;
										// 		let size = sst.read(&mut buf[..]).await.unwrap();
										// 		println!("Kad message: {:?}", &buf[0..size]);
										// 		if size == 0 {
										// 			println!("Kad rekt");
										// 			break
										// 		}
										// 		sst.write(&[0]).await.unwrap();
										// 	}
										// } else if proto == "/ipfs/id/1.0.0" {
										// 	sst.write(&buf[0..size]).await.unwrap();
										// 	loop {
										// 		// task::sleep(Duration::from_secs(5)).await;
										// 		let size = sst.read(&mut buf[..]).await.unwrap();
										// 		println!("Id message: {:?}", &buf[0..size]);
										// 		if size == 0 {
										// 			println!("Id rekt");
										// 			break
										// 		}
										// 		sst.write(&[0]).await.unwrap();
										// 	}
										} else {
											sst.write(&buf[0..(buf[0] + 1).into()]).await.unwrap();
											sst.write(b"\x03na\n").await.unwrap();

											loop {
												// println!("Unknown message: {:?}", &buf[0..size]);
												// println!("Unknown message");
												// task::sleep(Duration::from_secs(5)).await;
												let size = sst.read(&mut buf[..]).await.unwrap();
												// sst.write(&[0]).await.unwrap();

												if size == 0 {
													// println!("Unknown rekt");
													break
												}
											}
										}
									});
								}
								_ => {}
							}
						}
					});
				}
				ListenerEvent::AddressExpired(_multiaddr) => {
					println!("New expired");
				}
				ListenerEvent::Error(_terr) => {
					println!("New error");
				}
			}
		}
	}).await.unwrap();

	Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
	lin_main().await
}
