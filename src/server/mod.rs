use std::sync::Arc;
use std::time::Instant;

use tokio::sync::{mpsc, Mutex};

use crate::remote::{ConnectionMode, Servers};

pub(crate) mod db;
pub(crate) mod error;
pub(crate) mod registry;
pub(crate) mod routes;
pub(crate) mod sync_manager;
pub(crate) mod types;

/// Configuration for the daemon.
#[derive(Debug, Clone)]
pub(crate) struct DaemonConfig {
    pub data_dir: String,
    pub mainnet_server: Servers,
    pub testnet_server: Servers,
    pub connection_mode: ConnectionMode,
    pub sync_interval_secs: u64,
}

/// Command sent from the HTTP layer to the sync manager.
#[derive(Debug)]
pub(crate) enum SyncCommand {
    /// Start syncing a wallet with the given ID.
    StartSync(String),
    /// Stop syncing a wallet with the given ID.
    StopSync(String),
}

/// Shared application state, passed to all HTTP handlers via axum's State extractor.
pub(crate) struct AppState {
    pub registry: Arc<Mutex<registry::WalletRegistry>>,
    pub sync_tx: mpsc::Sender<SyncCommand>,
    pub config: DaemonConfig,
    pub start_time: Instant,
}
