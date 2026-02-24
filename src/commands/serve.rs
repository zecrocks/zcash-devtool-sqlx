use std::sync::Arc;
use std::time::Instant;

use anyhow::anyhow;
use clap::Args;
use tokio::sync::{mpsc, Mutex};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::{
    remote::{ConnectionMode, Servers},
    server::{
        registry::WalletRegistry, routes::build_router, sync_manager::SyncManager, AppState,
        DaemonConfig,
    },
};

fn parse_connection_mode(s: &str) -> Result<ConnectionMode, String> {
    match s {
        "direct" => Ok(ConnectionMode::Direct),
        "tor" => Ok(ConnectionMode::BuiltInTor),
        s if s.starts_with("socks5://") => {
            let url_part = s.strip_prefix("socks5://").unwrap();
            let addr = url_part
                .parse()
                .map_err(|_| format!("Invalid SOCKS5 proxy address: {url_part}"))?;
            Ok(ConnectionMode::SocksProxy(addr))
        }
        _ => Err(
            "Invalid connection mode. Use 'direct', 'tor', or 'socks5://<host>:<port>'".to_string(),
        ),
    }
}

/// Run the UFVK indexer HTTP daemon
#[derive(Debug, Args)]
pub(crate) struct Command {
    /// HTTP bind address
    #[arg(long, default_value = "127.0.0.1:8080")]
    bind: String,

    /// Data directory for wallets and registry database
    #[arg(long, default_value = "./zcash-indexer-data")]
    data_dir: String,

    /// Lightwalletd server for mainnet
    #[arg(short, long, default_value = "zecrocks", value_parser = Servers::parse)]
    server: Servers,

    /// Lightwalletd server for testnet
    #[arg(long, default_value = "zecrocks", value_parser = Servers::parse)]
    testnet_server: Servers,

    /// Connection mode: direct, tor, or socks5://host:port
    #[arg(long, default_value = "direct", value_parser = parse_connection_mode)]
    connection: ConnectionMode,

    /// Seconds between sync cycles
    #[arg(long, default_value = "60")]
    sync_interval: u64,
}

/// Remove wallet directories on disk that don't correspond to any active registry entry.
/// This cleans up orphans from crashes during registration or soft-deleted wallets.
fn vacuum_orphaned_dirs(data_dir: &str, registry: &WalletRegistry) {
    let wallets_path = format!("{data_dir}/wallets");

    let active_dirs = match registry.list_active_wallet_dirs() {
        Ok(dirs) => dirs,
        Err(e) => {
            warn!("Failed to query active wallet dirs, skipping vacuum: {e}");
            return;
        }
    };

    let entries = match std::fs::read_dir(&wallets_path) {
        Ok(entries) => entries,
        Err(e) => {
            warn!("Failed to read wallets directory, skipping vacuum: {e}");
            return;
        }
    };

    for entry in entries {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                warn!("Failed to read directory entry: {e}");
                continue;
            }
        };

        let path = entry.path();
        if !path.is_dir() {
            continue;
        }

        let dir_name = match entry.file_name().into_string() {
            Ok(name) => name,
            Err(_) => continue,
        };

        if !active_dirs.contains(&dir_name) {
            info!("Removing orphaned wallet directory: {dir_name}");
            if let Err(e) = std::fs::remove_dir_all(&path) {
                warn!("Failed to remove orphaned directory {}: {e}", path.display());
            }
        }
    }
}

impl Command {
    pub(crate) async fn run(self) -> Result<(), anyhow::Error> {
        // Create data directory
        std::fs::create_dir_all(&self.data_dir)?;
        std::fs::create_dir_all(format!("{}/wallets", self.data_dir))?;

        let config = DaemonConfig {
            data_dir: self.data_dir.clone(),
            mainnet_server: self.server,
            testnet_server: self.testnet_server,
            connection_mode: self.connection,
            sync_interval_secs: self.sync_interval,
        };

        // Open registry database
        let registry_path = format!("{}/registry.sqlite", self.data_dir);
        let registry = WalletRegistry::open(&registry_path)?;
        // Vacuum orphaned wallet directories before starting
        vacuum_orphaned_dirs(&self.data_dir, &registry);

        let registry = Arc::new(Mutex::new(registry));

        // Create cancellation token for graceful shutdown
        let cancel_token = CancellationToken::new();

        // Create sync manager command channel
        let (sync_tx, sync_rx) = mpsc::channel(64);

        let state = Arc::new(AppState {
            registry: registry.clone(),
            sync_tx,
            config: config.clone(),
            start_time: Instant::now(),
        });

        // Listen for Ctrl-C and cancel the token
        let shutdown_token = cancel_token.clone();
        tokio::spawn(async move {
            if let Err(e) = tokio::signal::ctrl_c().await {
                tracing::error!("Failed to listen for Ctrl-C: {e}");
            }
            info!("Received shutdown signal");
            shutdown_token.cancel();
        });

        // Start sync manager
        let sync_manager = SyncManager::new(config, registry, cancel_token.clone());
        tokio::spawn(async move {
            sync_manager.run(sync_rx).await;
        });

        // Build router
        let app = build_router(state);

        // Start HTTP server
        let listener = tokio::net::TcpListener::bind(&self.bind)
            .await
            .map_err(|e| anyhow!("Failed to bind to {}: {e}", self.bind))?;
        info!("Listening on {}", self.bind);

        // Serve with graceful shutdown
        axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                cancel_token.cancelled().await;
            })
            .await?;

        info!("Server shut down");
        Ok(())
    }
}
