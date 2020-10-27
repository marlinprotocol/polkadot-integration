use std::convert::From;
use std::io;
use std::iter;
use std::string::FromUtf8Error;
use std::pin::Pin;
use futures::{Future, StreamExt};
use bytes::Bytes;
use libp2p::core::{InboundUpgrade, OutboundUpgrade, UpgradeInfo};
use libp2p::swarm::protocols_handler::OneShotHandler;
use libp2p::core::upgrade::{ReadOneError, read_one, write_one};
use wasm_timer;

use futures::io::{AsyncRead, AsyncWrite};
use futures::future::BoxFuture;
use sp_runtime::traits::Block;
use std::time::Instant;
// use sc_network::config::Client;
use crate::schema;

const MAX_MESSAGE_SIZE: usize = 10240;  // bytes

#[derive(Debug)]
pub enum PolkadotProtocolError {
    ReadError(ReadOneError),
    ParseError(FromUtf8Error),
}

impl From<ReadOneError> for PolkadotProtocolError {
    fn from(err: ReadOneError) -> Self {
        PolkadotProtocolError::ReadError(err)
    }
}

impl From<FromUtf8Error> for PolkadotProtocolError {
    fn from(err: FromUtf8Error) -> Self {
        PolkadotProtocolError::ParseError(err)
    }
}

#[derive(Debug, Clone, Default)]
pub struct PolkadotProtocolConfig {}

impl PolkadotProtocolConfig {
    pub fn new() -> PolkadotProtocolConfig {
        PolkadotProtocolConfig {}
    }
}

impl UpgradeInfo for PolkadotProtocolConfig {
    type Info = Bytes;
    type InfoIter = iter::Once<Self::Info>;

    fn protocol_info(&self) -> Self::InfoIter {
        let proto = String::from("/dot/block-announces/1");
        iter::once(Bytes::from(proto))
    }
}

impl<TSocket> InboundUpgrade<TSocket> for PolkadotProtocolConfig
where
    B: Block,
    TSocket: AsyncRead + AsyncWrite + Send + Unpin + 'static,
{
    type Output = PolkadotProtocolEvent<B,TSocket>;
    type Error = PolkadotProtocolError;
    type Future = BoxFuture<'static, Result<Self::Output, Self::Error>>;
    //type Future = ReadOneThen<TSocket, fn(Vec<u8>) -> Result<String, PolkadotProtocolError>>;

    fn upgrade_inbound(self,mut socket: TSocket, _: Self::Info) -> Self::Future {
        let handling_start = wasm_timer::Instant::now();
        let future = async move {
            let len = 1024 * 1024;
            let vec = read_one(&mut socket, len).await?;
            match schema::v1::BlockRequest::decode(&vec[..]) {
                Ok(r) => Ok(PolkadotProtocolEvent::Request(r, socket, handling_start)),
                Err(e) => Err(ReadOneError::Io(io::Error::new(io::ErrorKind::Other, e)))
            }
        };
        future.boxed()
        // Box::pin( async move{
        // let bytes = read_one(&mut socket, MAX_MESSAGE_SIZE).await?;
        // let message = String::from_utf8(bytes)?;
            // println!("c upgrade_inbound {:?}",message);

        // Ok(message)
        // })
    }
}

#[derive(Debug, Clone)]
pub struct PolkadotProtocolMessage(String);

impl PolkadotProtocolMessage {
    pub fn new(str: String) -> Self {
        PolkadotProtocolMessage(str)
    }
}

impl UpgradeInfo for PolkadotProtocolMessage {
    type Info = Bytes;
    type InfoIter = iter::Once<Self::Info>;

    fn protocol_info(&self) -> Self::InfoIter {
        iter::once(Bytes::from(String::from("/dot/block-announces/1")))
    }
}

impl<TSocket> OutboundUpgrade<TSocket> for PolkadotProtocolMessage
where
    TSocket: AsyncRead + AsyncWrite + Send + Unpin + 'static,
{
    type Output = ();
    type Error = io::Error;
    //type Future = WriteOne<TSocket>;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Output, Self::Error>> + Send>>;
    fn upgrade_outbound(self, mut socket: TSocket, _: Self::Info) -> Self::Future {
        println!("upgrade_outbound");
        Box::pin( async move{
            let bytes = self.0.into_bytes();
            write_one(&mut socket, bytes).await?;
            Ok(())
        })
    }
}

#[derive(Debug)]
pub enum PolkadotProtocolEvent<B: Block, T> {
    Request(schema::v1::BlockRequest, T, Instant),
    Response(message::BlockRequest<B>, schema::v1::BlockResponse),
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
