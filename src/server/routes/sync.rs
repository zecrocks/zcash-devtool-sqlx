use std::sync::Arc;

use axum::{
    extract::{Path, Query, State},
    Json,
};
use uuid::Uuid;

use crate::server::{
    error::ApiError,
    types::{PaginationParams, SyncOverviewResponse, SyncStatusEntry, SyncWalletStatus},
    AppState,
};

pub(crate) async fn sync_status(
    State(state): State<Arc<AppState>>,
    Query(params): Query<PaginationParams>,
) -> Result<Json<SyncOverviewResponse>, ApiError> {
    let registry = state.registry.lock().await;
    let (statuses, total) = registry.all_sync_statuses_paginated(params.page, params.per_page)?;

    let wallets = statuses
        .into_iter()
        .map(|info| SyncWalletStatus {
            id: info.id,
            name: info.name,
            network: info.network,
            sync: info.sync,
        })
        .collect();

    Ok(Json(SyncOverviewResponse {
        wallets,
        page: params.page,
        per_page: params.per_page,
        total,
    }))
}

pub(crate) async fn wallet_sync_status(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
) -> Result<Json<SyncStatusEntry>, ApiError> {
    let registry = state.registry.lock().await;

    // Verify the wallet exists and is not deleted.
    registry
        .get_wallet(id)?
        .ok_or_else(|| ApiError::NotFound(format!("wallet {id} not found")))?;

    let entry = registry
        .get_sync_state(id)?
        .ok_or_else(|| ApiError::NotFound(format!("sync state for wallet {id} not found")))?;

    Ok(Json(entry))
}
