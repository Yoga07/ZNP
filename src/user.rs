use crate::comms_handler::{CommsError, Event};
use crate::interfaces::{
    Asset, Contract, HandshakeRequest, NodeType, Response, UseInterface, UserRequest,
};
use crate::primitives::transaction::{OutPoint, Transaction, TxConstructor, TxIn, TxOut};
use crate::script::lang::Script;
use crate::Node;

use bincode::deserialize;
use bytes::Bytes;
use std::{error::Error, fmt, net::SocketAddr};
use tokio::{sync::RwLock, task};
use tracing::{debug, info, info_span, warn};

/// Result wrapper for miner errors
pub type Result<T> = std::result::Result<T, UserError>;

#[derive(Debug)]
pub enum UserError {
    Network(CommsError),
    AsyncTask(task::JoinError),
    Serialization(bincode::Error),
}

impl fmt::Display for UserError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            UserError::Network(err) => write!(f, "Network error: {}", err),
            UserError::AsyncTask(err) => write!(f, "Async task error: {}", err),
            UserError::Serialization(err) => write!(f, "Serialization error: {}", err),
        }
    }
}

impl Error for UserError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Network(ref e) => Some(e),
            Self::Serialization(ref e) => Some(e),
            Self::AsyncTask(ref e) => Some(e),
        }
    }
}

impl From<CommsError> for UserError {
    fn from(other: CommsError) -> Self {
        Self::Network(other)
    }
}

impl From<task::JoinError> for UserError {
    fn from(other: task::JoinError) -> Self {
        Self::AsyncTask(other)
    }
}

impl From<bincode::Error> for UserError {
    fn from(other: bincode::Error) -> Self {
        Self::Serialization(other)
    }
}

// An instance of a MinerNode
#[derive(Debug, Clone)]
pub struct UserNode {
    node: Node,
    assets: Vec<Asset>,
    network: usize,
}

impl UserNode {
    /// Returns the miner node's public endpoint.
    pub fn address(&self) -> SocketAddr {
        self.node.address()
    }

    /// Constructs a transaction to pay a receiver
    ///
    /// ### Arguments
    ///
    /// * `tx_ins`              - Address/es to pay from
    /// * `receiver_address`    - Address to send to
    /// * `amount`              - Number of tokens to send
    pub fn create_payment_tx(
        &self,
        tx_ins: Vec<TxIn>,
        receiver_address: Vec<u8>,
        amount: u64,
    ) -> Transaction {
        let mut tx = Transaction::new();
        let mut tx_out = TxOut::new();

        tx_out.value = Some(Asset::Token(amount));
        tx_out.script_public_key = Some(receiver_address);

        tx.outputs = vec![tx_out];
        tx.inputs = tx_ins;
        tx.version = 0;

        tx
    }

    /// Constructs a set of TxIns for a payment
    ///
    /// ### Arguments
    ///
    /// * `tx_values`   - Series of values required for TxIn construction
    pub fn create_payment_tx_ins(&self, tx_values: Vec<TxConstructor>) -> Vec<TxIn> {
        let mut tx_ins = Vec::new();

        for entry in tx_values {
            let mut new_tx_in = TxIn::new();
            new_tx_in.script_signature = Script::pay2pkh(
                entry.prev_hash.clone(),
                entry.signatures[0],
                entry.pub_keys[0],
            );
            new_tx_in.previous_out = Some(OutPoint::new(entry.prev_hash, entry.prev_n));

            tx_ins.push(new_tx_in);
        }

        tx_ins
    }

    /// Start the user node on the network.
    pub async fn start(&mut self) -> Result<()> {
        Ok(self.node.listen().await?)
    }

    /// Connect to a peer on the network.
    pub async fn connect_to(&mut self, peer: SocketAddr) -> Result<()> {
        self.node.connect_to(peer).await?;
        self.node
            .send(
                peer,
                HandshakeRequest {
                    node_type: NodeType::Miner,
                },
            )
            .await?;
        Ok(())
    }

    /// Listens for new events from peers and handles them.
    /// The future returned from this function should be executed in the runtime. It will block execution.
    pub async fn handle_next_event(&mut self) -> Option<Result<Response>> {
        let event = self.node.next_event().await?;
        self.handle_event(event).await.into()
    }

    async fn handle_event(&mut self, event: Event) -> Result<Response> {
        match event {
            Event::NewFrame { peer, frame } => Ok(self.handle_new_frame(peer, frame).await?),
        }
    }

    /// Hanldes a new incoming message from a peer.
    async fn handle_new_frame(&mut self, peer: SocketAddr, frame: Bytes) -> Result<Response> {
        info_span!("peer", ?peer).in_scope(|| {
            let req = deserialize::<UserRequest>(&frame).map_err(|error| {
                warn!(?error, "frame-deserialize");
                error
            })?;

            info_span!("request", ?req).in_scope(|| {
                let response = self.handle_request(peer, req);
                debug!(?response, ?peer, "response");

                Ok(response)
            })
        })
    }

    /// Handles a compute request.
    fn handle_request(&mut self, peer: SocketAddr, req: UserRequest) -> Response {
        use UserRequest::*;
        match req {
            AdvertiseContract { contract, peers } => self.check_contract(contract, peers),
            SendAssets { assets } => self.receive_assets(assets),
        }
    }
}

impl UseInterface for UserNode {
    fn new(address: SocketAddr, network: usize) -> UserNode {
        UserNode {
            node: Node::new(address, 2),
            assets: Vec::new(),
            network: network,
        }
    }

    fn check_contract<UserNode>(&self, contract: Contract, peers: Vec<UserNode>) -> Response {
        Response {
            success: false,
            reason: "Not implemented yet",
        }
    }

    fn receive_assets(&mut self, assets: Vec<Asset>) -> Response {
        self.assets = assets;

        Response {
            success: true,
            reason: "Successfully received assets",
        }
    }
}