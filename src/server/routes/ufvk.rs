use std::path::PathBuf;
use std::sync::Arc;

use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use axum_extra::extract::Query;
use sha2::{Digest, Sha256};
use zcash_address::unified::{self, Encoding};
use zcash_client_backend::{
    data_api::{AccountBirthday, AccountPurpose, WalletWrite},
    proto::service,
};
use zcash_keys::keys::UnifiedFullViewingKey;
use zcash_protocol::consensus;

use crate::{
    config::WalletConfig,
    data::{init_dbs, Network},
    remote::ConnectionMode,
    server::{
        error::ApiError,
        types::{
            DeleteResponse, PaginationParams, RegisterUfvkRequest, RegisterUfvkResponse,
            UfvkDetailResponse, UfvkListResponse,
        },
        AppState, SyncCommand,
    },
};

/// Derive a deterministic wallet ID from a UFVK and birthday height.
/// Format: last 36 chars of UFVK + "-" + birthday height
fn derive_wallet_id(ufvk: &str, birthday: u32) -> String {
    let suffix_len = 36.min(ufvk.len());
    let ufvk_suffix = &ufvk[ufvk.len() - suffix_len..];
    format!("{ufvk_suffix}-{birthday}")
}

pub(crate) async fn register_ufvk(
    State(state): State<Arc<AppState>>,
    Json(req): Json<RegisterUfvkRequest>,
) -> Result<(StatusCode, Json<RegisterUfvkResponse>), ApiError> {
    // 1. Decode UFVK — validates encoding
    let (network_type, ufvk_parsed) = unified::Ufvk::decode(&req.ufvk)
        .map_err(|e| ApiError::BadRequest(format!("Invalid UFVK encoding: {e}")))?;

    // 2. Parse key material
    let ufvk = UnifiedFullViewingKey::parse(&ufvk_parsed)
        .map_err(|e| ApiError::BadRequest(format!("Invalid UFVK key material: {e}")))?;

    // 3. Validate network
    let params = match network_type {
        consensus::NetworkType::Main => Ok(consensus::Network::MainNetwork),
        consensus::NetworkType::Test => Ok(consensus::Network::TestNetwork),
        consensus::NetworkType::Regtest => {
            Err(ApiError::Unprocessable("regtest UFVKs are not supported".into()))
        }
    }?;

    let network = Network::from(params);

    // 4. Derive deterministic wallet ID
    let wallet_id = derive_wallet_id(&req.ufvk, req.birthday);

    // 5. Check if this ID already exists (same UFVK + birthday)
    {
        let registry = state.registry.lock().await;
        if let Some(_) = registry.get_wallet(&wallet_id)? {
            return Err(ApiError::Conflict(format!(
                "UFVK already registered as wallet {wallet_id}"
            )));
        }
    }

    // SHA-256 hash for the ufvk_hash column
    let ufvk_hash = {
        let mut hasher = Sha256::new();
        hasher.update(req.ufvk.as_bytes());
        hex::encode(hasher.finalize())
    };

    let wallet_dir_path = PathBuf::from(&state.config.data_dir)
        .join("wallets")
        .join(&wallet_id);
    let wallet_dir_str = wallet_dir_path.to_string_lossy().to_string();

    // 6. Create wallet directory and init keys.toml
    let birthday_height = zcash_protocol::consensus::BlockHeight::from_u32(req.birthday);
    WalletConfig::init_without_mnemonic(Some(&wallet_dir_str), birthday_height, params)
        .map_err(|e| ApiError::Internal(format!("Failed to init wallet config: {e}")))?;

    // 7. Init databases
    let mut db_data = init_dbs(params, Some(&wallet_dir_str))
        .map_err(|e| ApiError::Internal(format!("Failed to init databases: {e}")))?;

    // 8. Connect to lightwalletd and fetch tree state at birthday-1
    let servers = match network {
        Network::Main => &state.config.mainnet_server,
        Network::Test => &state.config.testnet_server,
    };
    let server = servers
        .pick(params)
        .map_err(|e| ApiError::Internal(format!("No server for network: {e}")))?;
    let mut client = match &state.config.connection_mode {
        ConnectionMode::Direct => server.connect_direct().await,
        ConnectionMode::SocksProxy(addr) => server.connect_over_socks(*addr).await,
        ConnectionMode::BuiltInTor => {
            // For the server context, we use direct connection as default
            server.connect_direct().await
        }
    }
    .map_err(|e| ApiError::Internal(format!("Failed to connect to lightwalletd: {e}")))?;

    let tip_height: zcash_protocol::consensus::BlockHeight = client
        .get_latest_block(service::ChainSpec::default())
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to get latest block: {e}")))?
        .get_ref()
        .height
        .try_into()
        .map_err(|_| ApiError::Internal("Invalid block height from server".into()))?;

    let tree_request = service::BlockId {
        height: (req.birthday.saturating_sub(1)).into(),
        ..Default::default()
    };
    let treestate = client
        .get_tree_state(tree_request)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to get tree state: {e}")))?
        .into_inner();

    let birthday = AccountBirthday::from_treestate(treestate, Some(tip_height))
        .map_err(|_| ApiError::Internal("Invalid tree state from server".into()))?;

    // 9. Import the UFVK as view-only
    let name = req.name.clone().unwrap_or_else(|| wallet_id.clone());
    db_data
        .import_account_ufvk(&name, &ufvk, &birthday, AccountPurpose::ViewOnly, None)
        .map_err(|e| ApiError::Internal(format!("Failed to import UFVK: {e}")))?;

    // 10. Insert into registry
    {
        let registry = state.registry.lock().await;
        registry.insert_wallet(
            &wallet_id,
            &req.ufvk,
            &ufvk_hash,
            req.name.as_deref(),
            network.name(),
            req.birthday,
            &wallet_dir_str,
        )?;
    }

    // 11. Signal sync manager to start syncing
    let _ = state.sync_tx.send(SyncCommand::StartSync(wallet_id.clone())).await;

    Ok((
        StatusCode::CREATED,
        Json(RegisterUfvkResponse {
            id: wallet_id,
            network: network.name().to_string(),
            birthday: req.birthday,
            name: req.name,
        }),
    ))
}

pub(crate) async fn list_ufvks(
    State(state): State<Arc<AppState>>,
    Query(params): Query<PaginationParams>,
) -> Result<Json<UfvkListResponse>, ApiError> {
    let registry = state.registry.lock().await;
    let (wallets, total) = registry.list_wallets(params.page, params.per_page)?;

    Ok(Json(UfvkListResponse {
        wallets,
        page: params.page,
        per_page: params.per_page,
        total,
    }))
}

pub(crate) async fn get_ufvk(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<UfvkDetailResponse>, ApiError> {
    let registry = state.registry.lock().await;
    let wallet = registry
        .get_wallet(&id)?
        .ok_or_else(|| ApiError::NotFound(format!("Wallet {id} not found")))?;
    let sync_status = registry.get_sync_state(&id)?;

    Ok(Json(UfvkDetailResponse {
        id: wallet.id,
        ufvk: wallet.ufvk,
        name: wallet.name,
        network: wallet.network,
        birthday: wallet.birthday,
        created_at: wallet.created_at,
        sync_status,
    }))
}

pub(crate) async fn delete_ufvk(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<DeleteResponse>, ApiError> {
    let deleted = {
        let registry = state.registry.lock().await;
        registry.soft_delete(&id)?
    };

    if !deleted {
        return Err(ApiError::NotFound(format!("Wallet {id} not found")));
    }

    // Signal sync manager to stop syncing this wallet
    let _ = state.sync_tx.send(SyncCommand::StopSync(id.clone())).await;

    Ok(Json(DeleteResponse { id, deleted: true }))
}
