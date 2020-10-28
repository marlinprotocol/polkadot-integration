use std::convert::From;
use std::{io,time::Duration};
use std::iter;
use std::string::FromUtf8Error;
use std::pin::Pin;
use futures::{Future, StreamExt};
use bytes::Bytes;
use libp2p::core::{InboundUpgrade, OutboundUpgrade, UpgradeInfo};
use codec::{Encode, Decode, Input, Output, Error};

use libp2p::swarm::protocols_handler::OneShotHandler;
use libp2p::core::upgrade::{ReadOneError, read_one, write_one};
use wasm_timer;

use futures::io::{AsyncRead, AsyncWrite};
use futures::future::BoxFuture;
use sp_runtime::traits::Block;
use std::time::Instant;
// use sc_network::config::Client;
use crate::schema;
use bitflags::bitflags;

const MAX_MESSAGE_SIZE: usize = 10240;  // bytes

bitflags! {
	pub struct BlockAttributes: u8 {
		const HEADER = 0b00000001;
		const BODY = 0b00000010;
		const RECEIPT = 0b00000100;
		const MESSAGE_QUEUE = 0b00001000;
		const JUSTIFICATION = 0b00010000;
	}
}

impl BlockAttributes {
    pub fn to_be_u32(&self) -> u32 {
        u32::from_be_bytes([self.bits(), 0, 0, 0])
    }

    pub fn from_be_u32(encoded: u32) -> Result<Self, Error> {
        BlockAttributes::from_bits(encoded.to_be_bytes()[0])
            .ok_or_else(|| Error::from("Invalid BlockAttributes"))
    }
}

impl Encode for BlockAttributes {
    fn encode_to<T: Output>(&self, dest: &mut T) {
        dest.push_byte(self.bits())
    }
}

impl codec::EncodeLike for BlockAttributes {}

impl Decode for BlockAttributes {
    fn decode<I: Input>(input: &mut I) -> Result<Self, Error> {
        Self::from_bits(input.read_byte()?).ok_or_else(|| Error::from("Invalid bytes"))
    }
}

#[derive(Debug, PartialEq, Eq, Clone, Encode, Decode)]
pub struct gBlockRequest<Hash, Number> {
    pub id: u64,
    pub fields: BlockAttributes,
    pub from: FromBlock<Hash, Number>,
    pub to: Option<Hash>,
    pub direction: Direction,
    pub max: Option<u32>,
}

pub type Justification = Vec<u8>;

#[derive(Debug, PartialEq, Eq, Clone, Encode, Decode)]
pub struct BlockData<Header, Hash, Extrinsic> {
    pub hash: Hash,
    pub header: Option<Header>,
    pub body: Option<Vec<Extrinsic>>,
    pub receipt: Option<Vec<u8>>,
    pub message_queue: Option<Vec<u8>>,
    pub justification: Option<Justification>,
}

pub struct gBlockResponse<Header, Hash, Extrinsic> {
    pub id: u64,
    pub blocks: Vec<BlockData<Header, Hash, Extrinsic>>,
}

pub type BlockResponse<B> = gBlockResponse<
    <B as BlockT>::Header,
    <B as BlockT>::Hash,
    <B as BlockT>::Extrinsic,
>;

pub type BlockRequest<B> = gBlockRequest<
    <B as BlockT>::Hash,
    <<B as BlockT>::Header as HeaderT>::Number,
>;

#[derive(Debug, Clone)]
pub struct PolkadotInProtocol<B> {
    pub marker: Phantom<B>,
}

impl<B: Block>  PolkadotInProtocol <B>{
    pub fn new() -> PolkadotProtocolConfig {
        PolkadotProtocolConfig {}
    }
}

impl<B: Block> UpgradeInfo for PolkadotInProtocol<B> {
    type Info = Bytes;
    type InfoIter = iter::Once<Self::Info>;

    fn protocol_info(&self) -> Self::InfoIter {
        let proto = String::from("/dot/block-announces/1");
        let proto1 = String::from("/dot/sync/2");
        iter::once(Bytes::from(proto))
    }
}

impl<B: Block,TSocket> InboundUpgrade<TSocket> for PolkadotInProtocol<B>
where
    B: Block,
    TSocket: AsyncRead + AsyncWrite + Send + Unpin + 'static,
{
    type Output = PolkadotProtocolEvent<B,TSocket>;
    type Error = ReadOneError;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Output, Self::Error>> + Send>>;

    fn upgrade_inbound(self,mut socket: TSocket, _: Self::Info) -> Self::Future {
        let handling_start = wasm_timer::Instant::now();
        let future = Box::pin(async move {
            let len = 1024 * 1024;
            let vec = read_one(&mut socket, len).await?;
            match schema::v1::BlockRequest::decode(&vec[..]) {
                Ok(r) => Ok(PolkadotProtocolEvent::Request(r, socket, handling_start)),
                Err(e) => Err(ReadOneError::Io(io::Error::new(io::ErrorKind::Other, e)))
            }
        });
        future
    }
}

#[derive(Debug, Clone)]
pub struct PolkadotOutProtocol<B: Block>{
    pub block_protobuf_request : Vec<u8>,
    pub recv_request: BlockRequest<B>,
};

impl PolkadotOutProtocol<B> {
    pub fn new(str: String) -> Self {
        PolkadotProtocolMessage(str)
    }
}

impl<B: Block> UpgradeInfo for PolkadotOutProtocol<B> {
    type Info = Bytes;
    type InfoIter = iter::Once<Self::Info>;
    fn protocol_info(&self) -> Self::InfoIter {
        let proto = String::from("/dot/block-announces/1");
        let proto1 = String::from("/dot/sync/2");
        iter::once(Bytes::from(proto1))
    }
}

impl<B: Block, TSocket> OutboundUpgrade<TSocket> for PolkadotOutProtocol<B>
where
    TSocket: AsyncRead + AsyncWrite + Send + Unpin + 'static,
{
    type Output = PolkadotProtocolEvent<B,T>;
    type Error = ReadOneError;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Output, Self::Error>> + Send>>;
    fn upgrade_outbound(self, mut socket: TSocket, _: Self::Info) -> Self::Future {
        println!("upgrade_outbound");
        Box::pin( async move{
            write_one(&mut socket, &self.block_protobuf_request)?;
            let vec = read_one(&mut s, 16 * 1024 * 1024).await?;
            schema::v1::BlockResponse::decode(&vec[..])
                .map(|r| PolkadotProtocolEvent::Response(self.recv_request,r))
                .map_err(|e| {
                    ReadOneError::Io(io::Error::new(io::ErrorKind::Other, e))
                }).boxed()
        })
    }
}

#[derive(Debug)]
pub enum PolkadotProtocolEvent<B: Block, T> {
    Request(schema::v1::BlockRequest, T, Instant),
    Response(message::BlockRequest<B>, schema::v1::BlockResponse),
}

#[derive(Debug, PartialEq, Eq, Clone, Encode, Decode)]
pub enum FromBlock<Hash, Number> {
    Hash(Hash),
    Number(Number),
}

#[derive(Debug, PartialEq, Eq, Clone, Copy, Encode, Decode)]
pub enum Direction {
    Ascending = 0,
    Descending = 1,
}

pub(crate) fn build_protobuf_block_request<Hash: Encode, Number: Encode>(
    attributes: BlockAttributes,
    from_block: FromBlock<Hash, Number>,
    to_block: Option<Hash>,
    direction: Direction,
    max_blocks: Option<u32>,
) -> schema::v1::BlockRequest {
    schema::v1::BlockRequest {
        fields: attributes.to_be_u32(),
        from_block: match from_block {
            FromBlock::Hash(h) =>
                Some(schema::v1::block_request::FromBlock::Hash(h.encode())),
            FromBlock::Number(n) =>
                Some(schema::v1::block_request::FromBlock::Number(n.encode())),
        },
        to_block: to_block.map(|h| h.encode()).unwrap_or_default(),
        direction: match direction {
            Direction::Ascending => schema::v1::Direction::Ascending as i32,
            Direction::Descending => schema::v1::Direction::Descending as i32,
        },
        max_blocks: max_blocks.unwrap_or(0),
    }
}

// pub enum PolkadotProtocolEvent {
//     Received(String),
    // Sent
// }

// impl From<String> for PolkadotProtocolEvent {
//     fn from(str: String) -> Self {
//         PolkadotProtocolEvent::Received(str)
//     }
// }

// impl From<()> for PolkadotProtocolEvent {
//     fn from(_: ()) -> Self {
//         PolkadotProtocolEvent::Sent
//     }
// }

/*
pub(crate) fn build_protobuf_block_request<Hash: Encode, Number: Encode>(
    attributes: BlockAttributes,
    from_block: message::FromBlock<Hash, Number>,
    to_block: Option<Hash>,
    direction: message::Direction,
    max_blocks: Option<u32>,
) -> schema::v1::BlockRequest {
    schema::v1::BlockRequest {
        fields: attributes.to_be_u32(),
        from_block: match from_block {
            message::FromBlock::Hash(h) =>
                Some(schema::v1::block_request::FromBlock::Hash(h.encode())),
            message::FromBlock::Number(n) =>
                Some(schema::v1::block_request::FromBlock::Number(n.encode())),
        },
        to_block: to_block.map(|h| h.encode()).unwrap_or_default(),
        direction: match direction {
            message::Direction::Ascending => schema::v1::Direction::Ascending as i32,
            message::Direction::Descending => schema::v1::Direction::Descending as i32,
        },
        max_blocks: max_blocks.unwrap_or(0),
    }
}
*/

type T = AsyncRead + AsyncWrite + Send + Unpin + 'static;
pub type PolkadotProtocolsHandler = OneShotHandler<PolkadotProtocolConfig, PolkadotProtocolMessage, PolkadotProtocolEvent<B,T>>;
