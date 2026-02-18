use std::collections::{BTreeSet, HashMap};
use std::panic::AssertUnwindSafe;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use futures_util::TryStreamExt;
use orchard::tree::MerkleHashOrchard;
use prost::Message;
use rand::rngs::OsRng;
use tokio::sync::{mpsc, Mutex};
use tokio::{fs::File, io::AsyncWriteExt};
use tokio_util::sync::CancellationToken;
use tonic::{transport::Channel, Code};
use tracing::{error, info, warn};
use zcash_client_backend::{
    data_api::{
        chain::{
            error::Error as ChainError, scan_cached_blocks, BlockSource, ChainState,
            CommitmentTreeRoot,
        },
        scanning::{ScanPriority, ScanRange},
        wallet::{decrypt_and_store_transaction, ConfirmationsPolicy},
        TransactionDataRequest, TransactionStatus, WalletCommitmentTrees, WalletRead, WalletWrite,
    },
    proto::service::{self, compact_tx_streamer_client::CompactTxStreamerClient, BlockId},
};
use zcash_client_sqlite::{
    chain::{init::init_blockmeta_db, BlockMeta},
    util::SystemClock,
    FsBlockDb, FsBlockDbError, WalletDb,
};
use zcash_primitives::{merkle_tree::HashSer, transaction::{Transaction, TxId}};
use zcash_protocol::consensus::{self, BlockHeight, BranchId, Parameters};

use crate::{
    data::{get_block_path, get_db_paths},
    error,
    remote::ConnectionMode,
    server::{registry::WalletRegistry, DaemonConfig, SyncCommand},
};

#[cfg(feature = "transparent-inputs")]
use {
    ::transparent::{
        address::Script,
        bundle::{OutPoint, TxOut},
    },
    zcash_client_backend::wallet::WalletTransparentOutput,
    zcash_client_sqlite::AccountUuid,
    zcash_keys::encoding::AddressCodec,
    zcash_protocol::value::Zatoshis,
    zcash_script::script,
};

const BATCH_SIZE: u32 = 10_000;

/// The sync manager listens for commands and manages per-wallet sync tasks.
pub(crate) struct SyncManager {
    config: DaemonConfig,
    registry: Arc<Mutex<WalletRegistry>>,
    cancel_token: CancellationToken,
    wallet_tokens: HashMap<String, (CancellationToken, std::thread::JoinHandle<()>)>,
}

impl SyncManager {
    pub fn new(
        config: DaemonConfig,
        registry: Arc<Mutex<WalletRegistry>>,
        cancel_token: CancellationToken,
    ) -> Self {
        Self {
            config,
            registry,
            cancel_token,
            wallet_tokens: HashMap::new(),
        }
    }

    /// Run the sync manager: listen for commands, manage wallet sync tasks.
    pub async fn run(mut self, mut cmd_rx: mpsc::Receiver<SyncCommand>) {
        // On startup, start syncing all existing active wallets.
        let startup_ids = {
            let registry = self.registry.lock().await;
            registry.list_active_wallet_ids().unwrap_or_else(|e| {
                error!("Failed to list active wallets on startup: {e}");
                vec![]
            })
        };
        for id in startup_ids {
            self.spawn_wallet_sync(id);
        }

        loop {
            tokio::select! {
                _ = self.cancel_token.cancelled() => {
                    info!("Sync manager shutting down...");
                    for (token, _) in self.wallet_tokens.values() {
                        token.cancel();
                    }
                    // Join all threads with a timeout so the process exits cleanly.
                    for (id, (_, handle)) in self.wallet_tokens.drain() {
                        info!("Waiting for sync thread for wallet {id} to finish...");
                        match handle.join() {
                            Ok(()) => info!("Sync thread for wallet {id} exited cleanly"),
                            Err(_) => warn!("Sync thread for wallet {id} had panicked"),
                        }
                    }
                    break;
                }
                cmd = cmd_rx.recv() => {
                    match cmd {
                        Some(SyncCommand::StartSync(id)) => {
                            self.spawn_wallet_sync(id);
                        }
                        Some(SyncCommand::StopSync(id)) => {
                            if let Some((token, handle)) = self.wallet_tokens.remove(&id) {
                                info!("Stopping sync for wallet {id}");
                                token.cancel();
                                // Join the thread so it doesn't leak.
                                let _ = handle.join();
                            }
                        }
                        None => {
                            info!("Sync command channel closed, shutting down");
                            break;
                        }
                    }
                }
                _ = tokio::time::sleep(Duration::from_secs(60)) => {
                    self.check_and_respawn_dead_threads();
                }
            }
        }
    }

    /// Check all stored JoinHandles and respawn any that have finished.
    fn check_and_respawn_dead_threads(&mut self) {
        let dead_ids: Vec<String> = self
            .wallet_tokens
            .iter()
            .filter(|(_, (_, handle))| handle.is_finished())
            .map(|(id, _)| id.clone())
            .collect();

        for id in dead_ids {
            if let Some((_, handle)) = self.wallet_tokens.remove(&id) {
                match handle.join() {
                    Ok(()) => warn!("Sync thread for wallet {id} exited unexpectedly, respawning"),
                    Err(_) => {
                        warn!("Sync thread for wallet {id} panicked, respawning")
                    }
                }
                self.spawn_wallet_sync(id);
            }
        }
    }

    fn spawn_wallet_sync(&mut self, wallet_id: String) {
        if self.wallet_tokens.contains_key(&wallet_id) {
            info!("Wallet {wallet_id} is already syncing");
            return;
        }

        let child_token = self.cancel_token.child_token();
        let config = self.config.clone();
        let registry = self.registry.clone();
        let thread_token = child_token.clone();

        // Use std::thread to avoid Send bounds on FsBlockDb/WalletDb.
        // Each wallet sync runs on its own OS thread with a single-threaded tokio runtime.
        let thread_name = if wallet_id.len() >= 8 {
            format!("sync-{}", &wallet_id[..8])
        } else {
            format!("sync-{wallet_id}")
        };
        let thread_wallet_id = wallet_id.clone();
        let handle = match std::thread::Builder::new()
            .name(thread_name)
            .spawn(move || {
                let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
                    let rt = match tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                    {
                        Ok(rt) => rt,
                        Err(e) => {
                            error!("Failed to build sync runtime for wallet {thread_wallet_id}: {e}");
                            return;
                        }
                    };
                    rt.block_on(wallet_sync_loop(
                        &thread_wallet_id,
                        config,
                        registry,
                        thread_token,
                    ));
                }));
                if let Err(panic_info) = result {
                    let msg = if let Some(s) = panic_info.downcast_ref::<&str>() {
                        s.to_string()
                    } else if let Some(s) = panic_info.downcast_ref::<String>() {
                        s.clone()
                    } else {
                        "unknown panic".to_string()
                    };
                    error!("Sync thread panicked: {msg}");
                }
            }) {
            Ok(handle) => handle,
            Err(e) => {
                error!("Failed to spawn sync thread for wallet {wallet_id}: {e}");
                return;
            }
        };

        info!("Started sync task for wallet {wallet_id}");
        self.wallet_tokens
            .insert(wallet_id, (child_token, handle));
    }
}

async fn wallet_sync_loop(
    wallet_id: &str,
    config: DaemonConfig,
    registry: Arc<Mutex<WalletRegistry>>,
    cancel_token: CancellationToken,
) {
    loop {
        if cancel_token.is_cancelled() {
            info!("Sync loop for wallet {wallet_id} cancelled");
            return;
        }

        match run_sync_cycle(wallet_id, &config, &registry).await {
            Ok(()) => {
                let _ = registry.lock().await.update_sync_state(
                    wallet_id,
                    "synced",
                    None,
                    None,
                    Some(1.0),
                    None,
                );
            }
            Err(e) => {
                warn!("Sync error for wallet {wallet_id}: {e}");
                let _ = registry.lock().await.update_sync_state(
                    wallet_id,
                    "error",
                    None,
                    None,
                    None,
                    Some(&e.to_string()),
                );
            }
        }

        // Sleep until next cycle or cancellation
        tokio::select! {
            _ = cancel_token.cancelled() => {
                info!("Sync loop for wallet {wallet_id} cancelled during sleep");
                return;
            }
            _ = tokio::time::sleep(Duration::from_secs(config.sync_interval_secs)) => {}
        }
    }
}

async fn run_sync_cycle(
    wallet_id: &str,
    config: &DaemonConfig,
    registry: &Arc<Mutex<WalletRegistry>>,
) -> Result<(), anyhow::Error> {
    // Get wallet info from registry
    let wallet = {
        let reg = registry.lock().await;
        reg.get_wallet(wallet_id)?
            .ok_or_else(|| anyhow::anyhow!("Wallet {wallet_id} not found in registry"))?
    };

    let params: consensus::Network = match wallet.network.as_str() {
        "main" => consensus::Network::MainNetwork,
        "test" => consensus::Network::TestNetwork,
        _ => return Err(anyhow::anyhow!("Invalid network: {}", wallet.network)),
    };

    // Update status to syncing
    {
        let reg = registry.lock().await;
        reg.update_sync_state(wallet_id, "syncing", None, None, None, None)?;
    }

    // Pick server based on network
    let servers = match wallet.network.as_str() {
        "main" => &config.mainnet_server,
        _ => &config.testnet_server,
    };
    let server = servers.pick(params)?;

    // Connect to lightwalletd
    let mut client = match &config.connection_mode {
        ConnectionMode::Direct => server.connect_direct().await?,
        ConnectionMode::SocksProxy(addr) => server.connect_over_socks(*addr).await?,
        ConnectionMode::BuiltInTor => server.connect_direct().await?,
    };

    // Open databases (owned by this task, on a single thread — no Send required)
    let (fsblockdb_root, db_data_path) = get_db_paths(Some(&wallet.wallet_dir));
    let fsblockdb_root_path = fsblockdb_root.as_path();
    let mut db_cache = FsBlockDb::for_path(fsblockdb_root_path).map_err(error::Error::from)?;
    init_blockmeta_db(&mut db_cache)?;
    let conn = crate::server::db::open_wallet_connection(&db_data_path)?;
    let mut db_data = WalletDb::from_connection(conn, params, SystemClock, OsRng);

    // 1. Update subtree roots
    update_subtree_roots(&mut client, &mut db_data).await?;

    // 2. Update chain tip
    let chain_tip = update_chain_tip(&mut client, &mut db_data).await?;

    // Update registry with chain tip
    {
        let reg = registry.lock().await;
        reg.update_sync_state(
            wallet_id,
            "syncing",
            None,
            Some(u32::from(chain_tip)),
            None,
            None,
        )?;
    }

    // 3. Refresh UTXOs
    #[cfg(feature = "transparent-inputs")]
    for account_id in db_data.get_account_ids()? {
        refresh_utxos(
            &params,
            &mut client,
            &mut db_data,
            account_id,
            BlockHeight::from(0),
        )
        .await?;
    }

    // 4. Suggest scan ranges and process them
    let mut scan_ranges = db_data.suggest_scan_ranges()?;

    // Handle verify ranges first
    loop {
        match scan_ranges.first() {
            Some(scan_range) if scan_range.priority() == ScanPriority::Verify => {
                let block_meta = download_blocks(
                    &mut client,
                    fsblockdb_root_path,
                    &db_cache,
                    scan_range,
                )
                .await?;

                let chain_state =
                    download_chain_state(&mut client, scan_range.block_range().start - 1).await?;

                let scan_ranges_updated = do_scan_blocks(
                    &params,
                    fsblockdb_root_path,
                    &mut db_cache,
                    &mut db_data,
                    &chain_state,
                    scan_range,
                )?;

                delete_cached_blocks_sync(fsblockdb_root_path, block_meta);

                if scan_ranges_updated {
                    scan_ranges = db_data.suggest_scan_ranges()?;
                } else {
                    break;
                }
            }
            _ => break,
        }
    }

    // Process remaining scan ranges
    let scan_ranges = db_data.suggest_scan_ranges()?;
    for scan_range in scan_ranges.into_iter().flat_map(|r| {
        (0..).scan(r, |acc, _| {
            if acc.is_empty() {
                None
            } else if let Some((cur, next)) = acc.split_at(acc.block_range().start + BATCH_SIZE) {
                *acc = next;
                Some(cur)
            } else {
                let cur = acc.clone();
                let end = acc.block_range().end;
                *acc = ScanRange::from_parts(end..end, acc.priority());
                Some(cur)
            }
        })
    }) {
        let block_meta = download_blocks(
            &mut client,
            fsblockdb_root_path,
            &db_cache,
            &scan_range,
        )
        .await?;

        let chain_state =
            download_chain_state(&mut client, scan_range.block_range().start - 1).await?;

        let scan_ranges_updated = do_scan_blocks(
            &params,
            fsblockdb_root_path,
            &mut db_cache,
            &mut db_data,
            &chain_state,
            &scan_range,
        )?;

        delete_cached_blocks_sync(fsblockdb_root_path, block_meta);

        // Update progress in registry
        if let Ok(Some(s)) = db_data.get_wallet_summary(ConfirmationsPolicy::default()) {
            let scan_progress = s.progress().scan();
            let progress = if *scan_progress.denominator() > 0 {
                (*scan_progress.numerator() as f64) / (*scan_progress.denominator() as f64)
            } else {
                0.0
            };
            let synced_height = u32::from(s.chain_tip_height());
            let _ = registry.lock().await.update_sync_state(
                wallet_id,
                "syncing",
                Some(synced_height),
                Some(u32::from(chain_tip)),
                Some(progress),
                None,
            );
        }

        if scan_ranges_updated {
            return Ok(());
        }
    }

    // 5. Enhance transactions (fetch full tx data, decrypt memos)
    info!("Enhancing transactions for wallet {wallet_id}");
    enhance_transactions(&mut client, &params, &mut db_data, chain_tip).await?;

    Ok(())
}

// ── Transaction enhancement helpers ──
// Ported from commands/wallet/enhance.rs, made generic over P: Parameters.

fn parse_raw_transaction<P: Parameters>(
    params: &P,
    chain_tip: BlockHeight,
    tx: service::RawTransaction,
) -> Result<(Transaction, Option<BlockHeight>), anyhow::Error> {
    let mined_height = (tx.height > 0 && tx.height <= u64::from(u32::MAX))
        .then(|| BlockHeight::from_u32(u32::try_from(tx.height).unwrap()));

    let tx = Transaction::read(
        &tx.data[..],
        // We assume unmined transactions are created with the current consensus branch ID.
        BranchId::for_height(params, mined_height.unwrap_or(chain_tip)),
    )?;

    Ok((tx, mined_height))
}

async fn fetch_transaction<P: Parameters>(
    client: &mut CompactTxStreamerClient<Channel>,
    params: &P,
    chain_tip: BlockHeight,
    txid: TxId,
) -> Result<Option<(Transaction, Option<BlockHeight>)>, anyhow::Error> {
    let request = service::TxFilter {
        hash: txid.as_ref().to_vec(),
        ..Default::default()
    };

    let raw_tx = match client.get_transaction(request).await {
        Ok(response) => Ok(Some(response.into_inner())),
        Err(status) => {
            if status.code() == Code::NotFound {
                Ok(None)
            } else {
                Err(status)
            }
        }
    }?;

    raw_tx
        .map(|raw_tx| parse_raw_transaction(params, chain_tip, raw_tx))
        .transpose()
}

async fn enhance_transactions<P: Parameters>(
    client: &mut CompactTxStreamerClient<Channel>,
    params: &P,
    db_data: &mut WalletDb<rusqlite::Connection, P, SystemClock, OsRng>,
    chain_tip: BlockHeight,
) -> Result<(), anyhow::Error> {
    let mut satisfied_requests = BTreeSet::new();
    loop {
        let mut new_request_encountered = false;
        for data_request in db_data.transaction_data_requests()? {
            if satisfied_requests.contains(&data_request) {
                continue;
            } else {
                new_request_encountered = true;
            }

            info!("Fetching data for request {:?}", data_request);
            match &data_request {
                TransactionDataRequest::GetStatus(txid) => {
                    let status = fetch_transaction(client, params, chain_tip, *txid)
                        .await?
                        .map_or(TransactionStatus::TxidNotRecognized, |(_, mined_height)| {
                            mined_height.map_or(
                                TransactionStatus::NotInMainChain,
                                TransactionStatus::Mined,
                            )
                        });
                    info!("Got status {:?}", status);
                    db_data.set_transaction_status(*txid, status)?;
                }
                TransactionDataRequest::Enhancement(txid) => {
                    match fetch_transaction(client, params, chain_tip, *txid).await? {
                        None => {
                            info!("Txid not recognized {:?}", txid);
                            db_data.set_transaction_status(
                                *txid,
                                TransactionStatus::TxidNotRecognized,
                            )?;
                        }
                        Some((tx, mined_height)) => {
                            info!(
                                "Enhancing tx {:?} with mined height {:?}",
                                txid, mined_height
                            );
                            decrypt_and_store_transaction(params, db_data, &tx, mined_height)?;
                        }
                    }
                }
                #[cfg(feature = "transparent-inputs")]
                TransactionDataRequest::TransactionsInvolvingAddress(tia) => {
                    let address = tia.address().encode(params);
                    let request = service::TransparentAddressBlockFilter {
                        address: address.clone(),
                        range: Some(service::BlockRange {
                            start: Some(service::BlockId {
                                height: u64::from(tia.block_range_start()),
                                ..Default::default()
                            }),
                            end: tia.block_range_end().map(|h| service::BlockId {
                                height: u64::from(h - 1), // `BlockRange` end is inclusive.
                                ..Default::default()
                            }),
                            pool_types: Default::default(),
                        }),
                    };

                    let mut stream = client.get_taddress_txids(request).await?.into_inner();
                    while let Some(raw_tx) = stream.try_next().await? {
                        let (tx, mined_height) =
                            parse_raw_transaction(params, chain_tip, raw_tx)?;
                        info!(
                            "Found tx {:?} for address {} with mined height {:?}",
                            tx.txid(),
                            address,
                            mined_height
                        );
                        decrypt_and_store_transaction(params, db_data, &tx, mined_height)?;
                    }
                }
                #[cfg(not(feature = "transparent-inputs"))]
                TransactionDataRequest::TransactionsInvolvingAddress(_) => {
                    warn!("TransactionsInvolvingAddress request ignored: transparent-inputs feature not enabled");
                }
            }

            satisfied_requests.insert(data_request);
        }

        if !new_request_encountered {
            break;
        }
    }

    Ok(())
}

// ── Self-contained sync helper functions ──
// These mirror the sync.rs functions but without ShutdownListener/TUI dependencies.

async fn update_subtree_roots<P: Parameters>(
    client: &mut CompactTxStreamerClient<Channel>,
    db_data: &mut WalletDb<rusqlite::Connection, P, SystemClock, OsRng>,
) -> Result<(), anyhow::Error> {
    let mut request = service::GetSubtreeRootsArg::default();
    request.set_shielded_protocol(service::ShieldedProtocol::Sapling);
    let sapling_roots: Vec<CommitmentTreeRoot<sapling::Node>> = client
        .get_subtree_roots(request)
        .await?
        .into_inner()
        .and_then(|root| async move {
            let root_hash = sapling::Node::read(&root.root_hash[..])?;
            Ok(CommitmentTreeRoot::from_parts(
                BlockHeight::from_u32(root.completing_block_height as u32),
                root_hash,
            ))
        })
        .try_collect()
        .await?;
    info!("Sapling tree has {} subtrees", sapling_roots.len());
    db_data.put_sapling_subtree_roots(0, &sapling_roots)?;

    let mut request = service::GetSubtreeRootsArg::default();
    request.set_shielded_protocol(service::ShieldedProtocol::Orchard);
    let orchard_roots: Vec<CommitmentTreeRoot<MerkleHashOrchard>> = client
        .get_subtree_roots(request)
        .await?
        .into_inner()
        .and_then(|root| async move {
            let root_hash = MerkleHashOrchard::read(&root.root_hash[..])?;
            Ok(CommitmentTreeRoot::from_parts(
                BlockHeight::from_u32(root.completing_block_height as u32),
                root_hash,
            ))
        })
        .try_collect()
        .await?;
    info!("Orchard tree has {} subtrees", orchard_roots.len());
    db_data.put_orchard_subtree_roots(0, &orchard_roots)?;

    Ok(())
}

async fn update_chain_tip<P: Parameters>(
    client: &mut CompactTxStreamerClient<Channel>,
    db_data: &mut WalletDb<rusqlite::Connection, P, SystemClock, OsRng>,
) -> Result<BlockHeight, anyhow::Error> {
    let tip_height: BlockHeight = client
        .get_latest_block(service::ChainSpec::default())
        .await?
        .get_ref()
        .height
        .try_into()
        .map_err(|_| error::Error::InvalidAmount)?;
    info!("Latest block height is {}", tip_height);
    db_data.update_chain_tip(tip_height)?;
    Ok(tip_height)
}

async fn download_blocks(
    client: &mut CompactTxStreamerClient<Channel>,
    fsblockdb_root: &Path,
    db_cache: &FsBlockDb,
    scan_range: &ScanRange,
) -> Result<Vec<BlockMeta>, anyhow::Error> {
    info!("Fetching {}", scan_range);
    let mut start = service::BlockId::default();
    start.height = scan_range.block_range().start.into();
    let mut end = service::BlockId::default();
    end.height = (scan_range.block_range().end - 1).into();
    let range = service::BlockRange {
        start: Some(start),
        end: Some(end),
        pool_types: Default::default(),
    };
    let block_meta_stream = client
        .get_block_range(range)
        .await
        .map_err(anyhow::Error::from)?
        .into_inner()
        .and_then(|block| async move {
            let (sapling_outputs_count, orchard_actions_count) = block
                .vtx
                .iter()
                .map(|tx| (tx.outputs.len() as u32, tx.actions.len() as u32))
                .fold((0, 0), |(acc_sapling, acc_orchard), (sapling, orchard)| {
                    (acc_sapling + sapling, acc_orchard + orchard)
                });

            let meta = BlockMeta {
                height: block.height(),
                block_hash: block.hash(),
                block_time: block.time,
                sapling_outputs_count,
                orchard_actions_count,
            };

            let encoded = block.encode_to_vec();
            let mut block_file = File::create(get_block_path(fsblockdb_root, &meta)).await?;
            block_file.write_all(&encoded).await?;

            Ok(meta)
        });
    tokio::pin!(block_meta_stream);

    let mut block_meta = vec![];
    while let Some(block) = block_meta_stream.try_next().await? {
        block_meta.push(block);
    }

    db_cache
        .write_block_metadata(&block_meta)
        .map_err(error::Error::from)?;

    Ok(block_meta)
}

async fn download_chain_state(
    client: &mut CompactTxStreamerClient<Channel>,
    block_height: BlockHeight,
) -> Result<ChainState, anyhow::Error> {
    let tree_state = client
        .get_tree_state(BlockId {
            height: block_height.into(),
            hash: vec![],
        })
        .await?;
    Ok(tree_state.into_inner().to_chain_state()?)
}

fn do_scan_blocks<P: Parameters + Send + 'static>(
    params: &P,
    fsblockdb_root: &Path,
    db_cache: &mut FsBlockDb,
    db_data: &mut WalletDb<rusqlite::Connection, P, SystemClock, OsRng>,
    initial_chain_state: &ChainState,
    scan_range: &ScanRange,
) -> Result<bool, anyhow::Error> {
    info!("Scanning {}", scan_range);
    let scan_result = scan_cached_blocks(
        params,
        db_cache,
        db_data,
        scan_range.block_range().start,
        initial_chain_state,
        scan_range.len(),
    );

    match scan_result {
        Err(ChainError::Scan(err)) if err.is_continuity_error() => {
            let rewind_height = err.at_height().saturating_sub(10);
            info!(
                "Chain reorg detected at {}, rewinding to {}",
                err.at_height(),
                rewind_height,
            );
            db_data.truncate_to_height(rewind_height)?;
            db_cache
                .with_blocks(Some(rewind_height + 1), None, |block| {
                    let meta = BlockMeta {
                        height: block.height(),
                        block_hash: block.hash(),
                        block_time: block.time,
                        sapling_outputs_count: 0,
                        orchard_actions_count: 0,
                    };
                    std::fs::remove_file(get_block_path(fsblockdb_root, &meta))
                        .map_err(|e| ChainError::<(), _>::BlockSource(FsBlockDbError::Fs(e)))
                })
                .map_err(|e| anyhow::anyhow!("{:?}", e))?;
            db_cache
                .truncate_to_height(rewind_height)
                .map_err(|e| anyhow::anyhow!("{:?}", e))?;
            Ok(true)
        }
        Ok(_) => {
            let latest_ranges = db_data.suggest_scan_ranges()?;
            Ok(if let Some(range) = latest_ranges.first() {
                range.priority() > scan_range.priority()
            } else {
                false
            })
        }
        Err(e) => Err(anyhow::anyhow!("{:?}", e)),
    }
}

fn delete_cached_blocks_sync(fsblockdb_root: &Path, block_meta: Vec<BlockMeta>) {
    for meta in block_meta {
        if let Err(e) = std::fs::remove_file(get_block_path(fsblockdb_root, &meta)) {
            error!("Failed to remove {:?}: {}", meta, e);
        }
    }
}

#[cfg(feature = "transparent-inputs")]
async fn refresh_utxos<P: Parameters>(
    params: &P,
    client: &mut CompactTxStreamerClient<Channel>,
    db_data: &mut WalletDb<rusqlite::Connection, P, SystemClock, OsRng>,
    account_id: AccountUuid,
    start_height: BlockHeight,
) -> Result<(), anyhow::Error> {
    let addresses = db_data
        .get_transparent_receivers(account_id, true, true)?
        .into_keys()
        .map(|addr| addr.encode(params))
        .collect::<Vec<_>>();

    if addresses.is_empty() {
        return Ok(());
    }

    let request = service::GetAddressUtxosArg {
        addresses,
        start_height: start_height.into(),
        max_entries: 0,
    };

    if request.addresses.is_empty() {
        info!("{:?} has no transparent receivers", account_id);
    } else {
        client
            .get_address_utxos_stream(request)
            .await?
            .into_inner()
            .map_err(anyhow::Error::from)
            .and_then(|reply| async move {
                WalletTransparentOutput::from_parts(
                    OutPoint::new(reply.txid[..].try_into()?, reply.index.try_into()?),
                    TxOut::new(
                        Zatoshis::from_nonnegative_i64(reply.value_zat)?,
                        Script(script::Code(reply.script)),
                    ),
                    Some(BlockHeight::from(u32::try_from(reply.height)?)),
                )
                .ok_or(anyhow::anyhow!(
                    "Received UTXO that doesn't correspond to a valid P2PKH or P2SH address"
                ))
            })
            .try_for_each(|output| {
                let res = db_data.put_received_transparent_utxo(&output).map(|_| ());
                async move { res.map_err(anyhow::Error::from) }
            })
            .await?;
    }

    Ok(())
}
