use std::sync::Arc;
use std::time::Instant;

use anyhow::anyhow;
use clap::Args;
use tokio::sync::{mpsc, Mutex};
use tokio_util::sync::CancellationToken;
use tracing::info;

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
    #[arg(long, default_value = "ecc", value_parser = Servers::parse)]
    testnet_server: Servers,

    /// Connection mode: direct, tor, or socks5://host:port
    #[arg(long, default_value = "direct", value_parser = parse_connection_mode)]
    connection: ConnectionMode,

    /// Seconds between sync cycles
    #[arg(long, default_value = "60")]
    sync_interval: u64,
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
