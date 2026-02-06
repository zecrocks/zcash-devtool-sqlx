use std::sync::Arc;

use axum::{extract::State, Json};

use crate::server::{
    error::ApiError,
    types::{HealthResponse, WalletCounts},
    AppState,
};

pub(crate) async fn health(
    State(state): State<Arc<AppState>>,
) -> Result<Json<HealthResponse>, ApiError> {
    let registry = state.registry.lock().await;
    let (mainnet, testnet) = registry.count_by_network()?;

    Ok(Json(HealthResponse {
        status: "ok".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        uptime_seconds: state.start_time.elapsed().as_secs(),
        wallets: WalletCounts { mainnet, testnet },
    }))
}
