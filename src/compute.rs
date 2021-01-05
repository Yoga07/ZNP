use crate::comms_handler::{CommsError, Event};
use crate::compute_raft::{CommittedItem, ComputeRaft};
use crate::configurations::ComputeNodeConfig;
use crate::constants::{PARTITION_LIMIT, PEER_LIMIT, UNICORN_LIMIT};
use crate::interfaces::{
    BlockStoredInfo, CommonBlockInfo, ComputeInterface, ComputeRequest, Contract, MineRequest,
    MinedBlockExtraInfo, NodeType, ProofOfWork, Response, StorageRequest,
};
use crate::unicorn::UnicornShard;
use crate::utils::{
    format_parition_pow_address, get_partition_entry_key, loop_connnect_to_peers_async,
    serialize_block_for_pow, validate_pow_block, validate_pow_for_address,
};
use crate::Node;
use bincode::{deserialize, serialize};
use bytes::Bytes;
use naom::primitives::asset::TokenAmount;
use naom::primitives::block::Block;
use naom::primitives::transaction::Transaction;
use naom::primitives::transaction_utils::construct_tx_hash;
use naom::script::utils::tx_ins_are_valid;
use rand::{self, Rng};
use serde::Serialize;
use sodiumoxide::crypto::secretbox::Key;
use std::collections::{BTreeMap, BTreeSet};
use std::{
    collections::HashMap,
    error::Error,
    fmt,
    future::Future,
    net::{IpAddr, Ipv4Addr, SocketAddr},
};
use tokio::task;
use tracing::{debug, error_span, info, trace, warn};
use tracing_futures::Instrument;

/// Result wrapper for compute errors
pub type Result<T> = std::result::Result<T, ComputeError>;

#[derive(Debug)]
pub enum ComputeError {
    ConfigError(&'static str),
    Network(CommsError),
    Serialization(bincode::Error),
    AsyncTask(task::JoinError),
}

impl fmt::Display for ComputeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ConfigError(err) => write!(f, "Config error: {}", err),
            Self::Network(err) => write!(f, "Network error: {}", err),
            Self::AsyncTask(err) => write!(f, "Async task error: {}", err),
            Self::Serialization(err) => write!(f, "Serialization error: {}", err),
        }
    }
}

impl Error for ComputeError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::ConfigError(_) => None,
            Self::Network(ref e) => Some(e),
            Self::AsyncTask(ref e) => Some(e),
            Self::Serialization(ref e) => Some(e),
        }
    }
}

impl From<CommsError> for ComputeError {
    fn from(other: CommsError) -> Self {
        Self::Network(other)
    }
}

impl From<bincode::Error> for ComputeError {
    fn from(other: bincode::Error) -> Self {
        Self::Serialization(other)
    }
}

impl From<task::JoinError> for ComputeError {
    fn from(other: task::JoinError) -> Self {
        Self::AsyncTask(other)
    }
}

/// Druid pool structure for checking and holding participants
#[derive(Debug, Clone)]
pub struct MinedBlock {
    pub nonce: Vec<u8>,
    pub block: Block,
    pub block_tx: BTreeMap<String, Transaction>,
    pub mining_transaction: (String, Transaction),
}

/// Druid pool structure for checking and holding participants
#[derive(Debug, Clone)]
pub struct DruidDroplet {
    participants: usize,
    tx: BTreeMap<String, Transaction>,
}

#[derive(Debug)]
pub struct ComputeNode {
    node: Node,
    node_raft: ComputeRaft,
    pub current_mined_block: Option<MinedBlock>,
    pub druid_pool: BTreeMap<String, DruidDroplet>,
    pub unicorn_limit: usize,
    pub current_random_num: Vec<u8>,
    pub last_coinbase_hash: Option<(SocketAddr, String, bool)>,
    pub partition_key: Option<Key>,
    pub partition_list: (Vec<ProofOfWork>, Vec<SocketAddr>),
    pub request_list: BTreeSet<SocketAddr>,
    pub request_list_first_flood: Option<usize>,
    pub storage_addr: SocketAddr,
    pub unicorn_list: HashMap<SocketAddr, UnicornShard>,
}

impl ComputeNode {
    /// Generates a new compute node instance
    ///
    /// ### Arguments
    ///
    /// * `address` - Address for the current compute node
    pub async fn new(config: ComputeNodeConfig) -> Result<ComputeNode> {
        let addr = config
            .compute_nodes
            .get(config.compute_node_idx)
            .ok_or(ComputeError::ConfigError("Invalid compute index"))?
            .address;
        let storage_addr = config
            .storage_nodes
            .get(config.compute_node_idx)
            .ok_or(ComputeError::ConfigError("Invalid storage index"))?
            .address;

        let node = Node::new(addr, PEER_LIMIT, NodeType::Compute).await?;
        let node_raft = ComputeRaft::new(&config).await;
        Ok(ComputeNode {
            node,
            node_raft,
            unicorn_list: HashMap::new(),
            unicorn_limit: UNICORN_LIMIT,
            current_mined_block: None,
            druid_pool: BTreeMap::new(),
            current_random_num: Self::generate_random_num(),
            last_coinbase_hash: None,
            request_list: BTreeSet::new(),
            request_list_first_flood: Some(PARTITION_LIMIT),
            partition_list: (Vec::new(), Vec::new()),
            partition_key: None,
            storage_addr,
        })
    }

    /// Returns the compute node's public endpoint.
    pub fn address(&self) -> SocketAddr {
        self.node.address()
    }

    /// Check if the node has a current block.
    pub fn has_current_mined_block(&self) -> bool {
        self.current_mined_block.is_some()
    }

    /// Connect to a storage peer on the network.
    pub async fn connect_to_storage(&mut self) -> Result<()> {
        self.node.connect_to(self.storage_addr).await?;
        Ok(())
    }

    pub fn inject_next_event(
        &self,
        from_peer_addr: SocketAddr,
        data: impl Serialize,
    ) -> Result<()> {
        Ok(self.node.inject_next_event(from_peer_addr, data)?)
    }

    /// Connect to a raft peer on the network.
    pub fn connect_to_raft_peers(&self) -> impl Future<Output = ()> {
        loop_connnect_to_peers_async(
            self.node.clone(),
            self.node_raft.raft_peer_to_connect().cloned().collect(),
        )
    }

    /// The current utxo_set including block being mined and previous block mining txs.
    pub fn get_committed_utxo_set(&self) -> &BTreeMap<String, Transaction> {
        &self.node_raft.get_committed_utxo_set()
    }

    /// The current tx_pool that will be used to generate next block
    pub fn get_committed_tx_pool(&self) -> &BTreeMap<String, Transaction> {
        self.node_raft.get_committed_tx_pool()
    }

    /// Return the raft loop to spawn in it own task.
    pub fn raft_loop(&self) -> impl Future<Output = ()> {
        self.node_raft.raft_loop()
    }

    /// Signal to the raft loop to complete
    pub async fn close_raft_loop(&mut self) {
        self.node_raft.close_raft_loop().await
    }

    /// Processes a dual double entry transaction
    ///
    /// ### Arguments
    ///
    /// * `transaction` - Transaction to process
    pub async fn process_dde_tx(&mut self, transaction: Transaction) -> Response {
        if let Some(druid) = transaction.clone().druid {
            // If this transaction is meant to join others
            #[allow(clippy::map_entry)]
            if self.druid_pool.contains_key(&druid) {
                self.process_tx_druid(druid, transaction);
                self.node_raft.propose_local_druid_transactions().await;

                return Response {
                    success: true,
                    reason: "Transaction added to corresponding DRUID droplets",
                };

            // If we haven't seen this DRUID yet
            } else {
                let mut droplet = DruidDroplet {
                    participants: transaction.druid_participants.unwrap(),
                    tx: BTreeMap::new(),
                };

                let tx_hash = construct_tx_hash(&transaction);
                droplet.tx.insert(tx_hash, transaction);

                self.druid_pool.insert(druid, droplet);
                return Response {
                    success: true,
                    reason: "Transaction added to DRUID pool. Awaiting other parties",
                };
            }
        }

        Response {
            success: false,
            reason: "Dual double entry transaction doesn't contain a DRUID",
        }
    }

    /// Processes a dual double entry transaction's DRUID with the current pool
    ///
    /// ### Arguments
    ///
    /// * `druid`       - DRUID to match on
    /// * `transaction` - Transaction to process
    pub fn process_tx_druid(&mut self, druid: String, transaction: Transaction) {
        let mut current_droplet = self.druid_pool.get(&druid).unwrap().clone();
        let tx_hash = construct_tx_hash(&transaction);
        current_droplet.tx.insert(tx_hash, transaction);

        // Execute the tx if it's ready
        if current_droplet.tx.len() == current_droplet.participants {
            self.execute_dde_tx(current_droplet);
            let _removal = self.druid_pool.remove(&druid);
        }
    }

    /// Executes a waiting dual double entry transaction that is ready to execute
    ///
    /// ### Arguments
    ///
    /// * `droplet`  - DRUID droplet of transactions to execute
    pub fn execute_dde_tx(&mut self, droplet: DruidDroplet) {
        let mut txs_valid = true;

        for tx in droplet.tx.values() {
            if !tx_ins_are_valid(tx.inputs.clone(), self.node_raft.get_committed_utxo_set()) {
                txs_valid = false;
                break;
            }
        }

        if txs_valid {
            self.node_raft.append_to_tx_druid_pool(droplet.tx);

            println!(
                "Transactions for dual double entry execution are valid. Adding to pending block"
            );
        } else {
            println!("Transactions for dual double entry execution are invalid");
        }
    }

    pub fn get_mining_block(&self) -> &Option<Block> {
        self.node_raft.get_mining_block()
    }

    pub fn set_committed_mining_block(
        &mut self,
        block: Block,
        block_tx: BTreeMap<String, Transaction>,
    ) {
        self.node_raft.set_committed_mining_block(block, block_tx)
    }

    /// Generates a garbage random num for use in network testing
    fn generate_random_num() -> Vec<u8> {
        let mut rng = rand::thread_rng();
        (0..10).map(|_| rng.gen_range(1, 200)).collect()
    }

    /// Gets a decremented socket address of peer for storage
    ///
    /// ### Arguments
    ///
    /// * `address`    - Address to decrement
    fn get_storage_address(&self, address: SocketAddr) -> SocketAddr {
        let mut storage_address = address;
        storage_address.set_ip(IpAddr::V4(Ipv4Addr::new(0, 0, 0, 0)));
        storage_address.set_port(address.port() - 1);

        storage_address
    }

    /// Util function to get a socket address for PID table checks
    ///
    /// ### Arguments
    ///
    /// * `address`    - Peer's address
    fn get_comms_address(&self, address: SocketAddr) -> SocketAddr {
        let comparison_port = address.port() + 1;
        let mut comparison_addr = address;

        comparison_addr.set_ip(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)));
        comparison_addr.set_port(comparison_port);

        comparison_addr
    }

    /// Sends notification of a block found and stored to the winner.
    pub async fn send_bf_notification(&mut self) -> Result<()> {
        if let Some((peer, win_coinbase, true)) = self.last_coinbase_hash.take() {
            self.node
                .send(peer, MineRequest::NotifyBlockFound { win_coinbase })
                .await?;
        }
        Ok(())
    }

    /// Sends the latest block to storage
    pub async fn send_first_block_to_storage(&mut self) -> Result<()> {
        let block_txs = self.node_raft.get_committed_utxo_set().clone();
        let mut block = Block::new();
        block.transactions = block_txs.keys().cloned().collect();

        let request = StorageRequest::SendBlock {
            common: CommonBlockInfo { block, block_txs },
            mined_info: MinedBlockExtraInfo {
                nonce: Vec::new(),
                mining_tx: (String::new(), Transaction::new()),
            },
        };
        self.node.send(self.storage_addr, request).await?;

        Ok(())
    }

    /// Sends the latest block to storage
    pub async fn send_block_to_storage(&mut self) -> Result<()> {
        // Only the first call will send to storage.
        let mined_block = self.current_mined_block.take().unwrap();
        let block = mined_block.block;
        let block_txs = mined_block.block_tx;
        let nonce = mined_block.nonce;
        let mining_tx = mined_block.mining_transaction;

        let request = StorageRequest::SendBlock {
            common: CommonBlockInfo { block, block_txs },
            mined_info: MinedBlockExtraInfo { nonce, mining_tx },
        };
        self.node.send(self.storage_addr, request).await?;

        Ok(())
    }

    /// Floods all peers with a PoW for UnicornShard creation
    /// TODO: Add in comms handling for sending and receiving requests
    ///
    /// ### Arguments
    ///
    /// * `address` - Address of the contributing node
    /// * `pow`     - PoW to flood
    pub fn flood_pow_to_peers(&self, _address: SocketAddr, _pow: &[u8]) {
        println!("Flooding PoW to peers not implemented");
    }

    /// Floods all peers with a PoW commit for UnicornShard creation
    /// TODO: Add in comms handling for sending and receiving requests
    ///
    /// ### Arguments
    ///
    /// * `address` - Address of the contributing node
    /// * `pow`     - PoW to flood
    pub fn flood_commit_to_peers(&self, _address: SocketAddr, _commit: &ProofOfWork) {
        println!("Flooding commit to peers not implemented");
    }

    /// Listens for new events from peers and handles them.
    /// The future returned from this function should be executed in the runtime. It will block execution.
    pub async fn handle_next_event(&mut self) -> Option<Result<Response>> {
        loop {
            // Process pending submission.
            self.node_raft.propose_block_with_last_info().await;

            // State machines are not keept between iterations or calls.
            // All selection calls (between = and =>), need to be dropable
            // i.e they should only await a channel.
            tokio::select! {
                event = self.node.next_event() => {
                    trace!("handle_next_event evt {:?}", event);
                    if let res @ Some(_) = self.handle_event(event?).await.transpose() {
                        return res;
                    }
                }
                Some(commit_data) = self.node_raft.next_commit() => {
                    trace!("handle_next_event commit {:?}", commit_data);
                    match self.node_raft.received_commit(commit_data).await {
                        Some(CommittedItem::FirstBlock) => {
                            self.node_raft.generate_first_block();
                            return Some(Ok(Response{
                                success: true,
                                reason: "First Block committed",
                            }));
                        }
                        Some(CommittedItem::Block) => {
                            self.validate_wining_miner_tx();
                            self.node_raft.generate_block();
                            return Some(Ok(Response{
                                success: true,
                                reason: "Block committed",
                            }));
                        }
                        Some(CommittedItem::Transactions) => {
                            return Some(Ok(Response{
                                success: true,
                                reason: "Transactions committed",
                            }));
                        }
                        None => {}
                    }
                }
                Some((addr, msg)) = self.node_raft.next_msg() => {
                    trace!("handle_next_event msg {:?}: {:?}", addr, msg);
                    match self.node.send(
                        addr,
                        ComputeRequest::SendRaftCmd(msg)).await {
                            Err(e) => info!("Msg not sent to {}, from {}: {:?}", addr, self.address(), e),
                            Ok(()) => trace!("Msg sent to {}, from {}", addr, self.address()),
                        };
                }
                _ = self.node_raft.timeout_propose_transactions() => {
                    trace!("handle_next_event timeout transactions");
                    self.node_raft.propose_local_transactions_at_timeout().await;
                    self.node_raft.propose_local_druid_transactions().await;
                }
            }
        }
    }

    async fn handle_event(&mut self, event: Event) -> Result<Option<Response>> {
        match event {
            Event::NewFrame { peer, frame } => {
                let peer_span = error_span!("peer", ?peer);
                self.handle_new_frame(peer, frame)
                    .instrument(peer_span)
                    .await
            }
        }
    }

    /// Hanldes a new incoming message from a peer.
    async fn handle_new_frame(
        &mut self,
        peer: SocketAddr,
        frame: Bytes,
    ) -> Result<Option<Response>> {
        let req = deserialize::<ComputeRequest>(&frame).map_err(|error| {
            warn!(?error, "frame-deserialize");
            error
        })?;

        let req_span = error_span!("request", ?req);
        let response = self.handle_request(peer, req).instrument(req_span).await;
        debug!(?response, ?peer, "response");

        Ok(response)
    }

    /// Handles a compute request.
    async fn handle_request(&mut self, peer: SocketAddr, req: ComputeRequest) -> Option<Response> {
        use ComputeRequest::*;
        trace!("handle_request");

        match req {
            SendBlockStored(info) => Some(self.receive_block_stored(peer, info)),
            SendPoW { nonce, coinbase } => Some(self.receive_pow(peer, nonce, coinbase)),
            SendPartitionEntry { partition_entry } => {
                Some(self.receive_partition_entry(peer, partition_entry))
            }
            SendTransactions { transactions } => Some(self.receive_transactions(transactions)),
            SendPartitionRequest => Some(self.receive_partition_request(peer)),
            SendRaftCmd(msg) => {
                self.node_raft.received_message(msg).await;
                None
            }
        }
    }

    /// Receive a partition request from a miner node
    /// TODO: This may need to be part of the ComputeInterface depending on key agreement
    fn receive_partition_request(&mut self, peer: SocketAddr) -> Response {
        self.request_list.insert(peer);
        if self.request_list_first_flood == Some(self.request_list.len()) {
            self.request_list_first_flood = None;
            Response {
                success: true,
                reason: "Received first full partition request",
            }
        } else {
            Response {
                success: true,
                reason: "Received partition request successfully",
            }
        }
    }

    /// Receives the light POW for partition inclusion
    fn receive_partition_entry(
        &mut self,
        peer: SocketAddr,
        partition_entry: ProofOfWork,
    ) -> Response {
        if self.partition_list.0.len() >= PARTITION_LIMIT {
            return Response {
                success: false,
                reason: "Partition list is already full",
            };
        }

        if format_parition_pow_address(peer) != partition_entry.address
            || !validate_pow_for_address(&partition_entry, &Some(&self.current_random_num))
        {
            return Response {
                success: false,
                reason: "PoW received is invalid",
            };
        }

        self.partition_list.0.push(partition_entry);
        self.partition_list.1.push(peer);

        if self.partition_list.0.len() < PARTITION_LIMIT {
            return Response {
                success: true,
                reason: "Partition PoW received successfully",
            };
        }

        self.partition_key = Some(get_partition_entry_key(&self.partition_list.0));

        Response {
            success: true,
            reason: "Partition list is full",
        }
    }

    /// Floods the random number to everyone who requested
    pub async fn flood_rand_num_to_requesters(&mut self) -> Result<()> {
        let rnum = self.current_random_num.clone();
        info!("RANDOM NUMBER IN COMPUTE: {:?}", rnum);

        self.node
            .send_to_all(
                self.request_list.iter().copied(),
                MineRequest::SendRandomNum { rnum },
            )
            .await
            .unwrap();

        Ok(())
    }

    /// Floods the current block to participants for mining
    pub async fn flood_block_to_partition(&mut self) -> Result<()> {
        info!("BLOCK TO SEND: {:?}", self.node_raft.get_mining_block());
        let block: &Block = self.node_raft.get_mining_block().as_ref().unwrap();
        let block = serialize(block).unwrap();

        self.node
            .send_to_all(
                self.partition_list.1.iter().copied(),
                MineRequest::SendBlock { block },
            )
            .await
            .unwrap();

        Ok(())
    }

    /// Floods all peers with the full partition list
    pub async fn flood_list_to_partition(&mut self) -> Result<()> {
        self.node
            .send_to_all(
                self.partition_list.1.iter().copied(),
                MineRequest::SendPartitionList {
                    p_list: self.partition_list.0.clone(),
                },
            )
            .await
            .unwrap();
        Ok(())
    }

    pub fn mining_block_mined(
        &mut self,
        nonce: Vec<u8>,
        miner: SocketAddr,
        mining_transaction: (String, Transaction),
    ) {
        // Take mining block info: no more mining for it.
        let (block, block_tx) = self.node_raft.take_mining_block();
        self.current_random_num = Self::generate_random_num();
        self.partition_list = (Vec::new(), Vec::new());
        self.partition_key = None;

        // Update latest coinbase to notify winner
        self.last_coinbase_hash = Some((miner, mining_transaction.0.clone(), false));

        // Set mined block
        self.current_mined_block = Some(MinedBlock {
            nonce,
            block,
            block_tx,
            mining_transaction,
        });
    }

    fn validate_wining_miner_tx(&mut self) {
        if let Some((miner, tx_hash, false)) = self.last_coinbase_hash.take() {
            if self
                .node_raft
                .get_committed_tx_pool()
                .contains_key(&tx_hash)
            {
                self.last_coinbase_hash = Some((miner, tx_hash, true));
            }
        }
    }
}

impl ComputeInterface for ComputeNode {
    fn receive_commit(&mut self, address: SocketAddr, commit: ProofOfWork) -> Response {
        if let Some(entry) = self.unicorn_list.get_mut(&address) {
            if entry.is_valid(commit.clone()) {
                self.flood_commit_to_peers(address, &commit);
                return Response {
                    success: true,
                    reason: "Commit received successfully",
                };
            }

            return Response {
                success: false,
                reason: "Commit not valid. Rejecting...",
            };
        }

        Response {
            success: false,
            reason: "The node submitting a commit never submitted a promise",
        }
    }

    fn receive_pow(
        &mut self,
        address: SocketAddr,
        nonce: Vec<u8>,
        coinbase: Transaction,
    ) -> Response {
        info!(?address, "Received PoW");
        let coinbase_hash = construct_tx_hash(&coinbase);

        let mut mining_block = if let Some(mining_block) = self.node_raft.get_mining_block() {
            serialize_block_for_pow(mining_block)
        } else {
            // TODO: Verify the pow block is expected block.
            return Response {
                success: false,
                reason: "Not mining given block",
            };
        };

        let coinbase_amount = TokenAmount(12000);
        if !coinbase.is_coinbase() || coinbase.outputs[0].amount != coinbase_amount {
            return Response {
                success: false,
                reason: "Coinbase transaction invalid",
            };
        }

        if !validate_pow_block(&mut mining_block, &coinbase_hash, &nonce) {
            return Response {
                success: false,
                reason: "Invalid PoW for block",
            };
        }

        self.mining_block_mined(nonce, address, (coinbase_hash, coinbase));

        Response {
            success: true,
            reason: "Received PoW successfully",
        }
    }

    fn get_unicorn_table(&self) -> Vec<UnicornShard> {
        let mut unicorn_table = Vec::with_capacity(self.unicorn_list.len());

        for unicorn in self.unicorn_list.values() {
            unicorn_table.push(unicorn.clone());
        }

        unicorn_table
    }

    fn partition(&self, _uuids: Vec<&'static str>) -> Response {
        Response {
            success: false,
            reason: "Not implemented yet",
        }
    }

    fn get_service_levels(&self) -> Response {
        Response {
            success: false,
            reason: "Not implemented yet",
        }
    }

    fn receive_block_stored(
        &mut self,
        peer: SocketAddr,
        previous_block_info: BlockStoredInfo,
    ) -> Response {
        if peer != self.storage_addr {
            return Response {
                success: false,
                reason: "Received block stored not from our storage peer",
            };
        }

        self.node_raft.append_block_stored_info(previous_block_info);

        Response {
            success: true,
            reason: "Received block stored",
        }
    }

    fn receive_transactions(&mut self, transactions: BTreeMap<String, Transaction>) -> Response {
        if !self.node_raft.tx_pool_can_accept(transactions.len()) {
            return Response {
                success: false,
                reason: "Transaction pool for this compute node is full",
            };
        }

        let utxo_set = self.node_raft.get_committed_utxo_set();
        let valid_tx: BTreeMap<_, _> = transactions
            .iter()
            .filter(|(_, tx)| !tx.is_coinbase())
            .filter(|(_, tx)| tx_ins_are_valid(tx.inputs.clone(), utxo_set))
            .map(|(hash, tx)| (hash.clone(), tx.clone()))
            .collect();

        // At this point the tx's are considered valid
        let valid_tx_len = valid_tx.len();
        self.node_raft.append_to_tx_pool(valid_tx);

        if valid_tx_len == 0 {
            return Response {
                success: false,
                reason: "No valid transactions provided",
            };
        }

        if valid_tx_len < transactions.len() {
            return Response {
                success: true,
                reason: "Some transactions invalid. Adding valid transactions only",
            };
        }

        Response {
            success: true,
            reason: "Transactions added to tx pool",
        }
    }

    fn execute_contract(&self, _contract: Contract) -> Response {
        Response {
            success: false,
            reason: "Not implemented yet",
        }
    }

    fn get_next_block_reward(&self) -> f64 {
        0.0
    }
}
