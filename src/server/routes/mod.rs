use std::sync::Arc;

use axum::{
    routing::{delete, get, post},
    Router,
};
use tower_http::cors::CorsLayer;

use super::AppState;

pub(crate) mod address;
pub(crate) mod balance;
pub(crate) mod health;
pub(crate) mod sync;
pub(crate) mod transactions;
pub(crate) mod ufvk;

pub(crate) fn build_router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/health", get(health::health))
        .route("/ufvks", post(ufvk::register_ufvk))
        .route("/ufvks", get(ufvk::list_ufvks))
        .route("/ufvks/{id}", get(ufvk::get_ufvk))
        .route("/ufvks/{id}", delete(ufvk::delete_ufvk))
        .route("/ufvks/{id}/address", post(address::generate_address))
        .route("/ufvks/{id}/balance", get(balance::get_balance))
        .route(
            "/ufvks/{id}/transactions",
            get(transactions::get_transactions),
        )
        .route("/address/resolve", post(address::resolve_address))
        .route("/sync/status", get(sync::sync_status))
        .route("/ufvks/{id}/sync", get(sync::wallet_sync_status))
        .layer(CorsLayer::very_permissive())
        .with_state(state)
}
