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
use zcash_protocol::consensus::{self, NetworkUpgrade, Parameters};

use crate::{
    config::WalletConfig,
    data::{init_dbs, Network},
    remote::ConnectionMode,
    server::{
        error::ApiError,
        types::{
            DeleteResponse, LookupUfvkEntry, LookupUfvkRequest, LookupUfvkResponse,
            PaginationParams, RegisterUfvkRequest, RegisterUfvkResponse, UfvkDetailResponse,
            UfvkListResponse, UpdateWalletPrefsRequest, UpdateWalletPrefsResponse,
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

    // 4. Resolve birthday
    let raw_birthday = req.birthday.unwrap_or(0);

    // Use raw birthday for wallet ID (deterministic, independent of network params)
    let wallet_id = derive_wallet_id(&req.ufvk, raw_birthday);

    // Compute effective birthday: clamp to Sapling activation height
    let sapling_activation = params
        .activation_height(NetworkUpgrade::Sapling)
        .expect("Sapling activation must be defined");
    let effective_birthday = if raw_birthday < u32::from(sapling_activation) {
        sapling_activation
    } else {
        consensus::BlockHeight::from_u32(raw_birthday)
    };

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

    // 6-10. Create wallet, init databases, fetch tree state, import UFVK, register.
    // Wrap everything after directory creation in a block so we can clean up on any error.
    let setup_result: Result<(), ApiError> = async {
        // 6. Create wallet directory and init keys.toml
        WalletConfig::init_without_mnemonic(Some(&wallet_dir_str), effective_birthday, params)
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

        let tip_height: consensus::BlockHeight = client
            .get_latest_block(service::ChainSpec::default())
            .await
            .map_err(|e| ApiError::Internal(format!("Failed to get latest block: {e}")))?
            .get_ref()
            .height
            .try_into()
            .map_err(|_| ApiError::Internal("Invalid block height from server".into()))?;

        let tree_request = service::BlockId {
            height: u32::from(effective_birthday).saturating_sub(1).into(),
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
        let registry = state.registry.lock().await;
        registry.insert_wallet(
            &wallet_id,
            &req.ufvk,
            &ufvk_hash,
            req.name.as_deref(),
            network.name(),
            raw_birthday,
            &wallet_dir_str,
            req.transparent_sync,
        )?;

        Ok(())
    }
    .await;

    // Clean up wallet directory on error
    if let Err(e) = setup_result {
        let _ = std::fs::remove_dir_all(&wallet_dir_path);
        return Err(e);
    }

    // 11. Signal sync manager to start syncing
    let _ = state.sync_tx.send(SyncCommand::StartSync(wallet_id.clone())).await;

    Ok((
        StatusCode::CREATED,
        Json(RegisterUfvkResponse {
            id: wallet_id,
            network: network.name().to_string(),
            birthday: raw_birthday,
            name: req.name,
            transparent_sync: req.transparent_sync,
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
        transparent_sync: wallet.transparent_sync,
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

pub(crate) async fn update_wallet_prefs(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(req): Json<UpdateWalletPrefsRequest>,
) -> Result<Json<UpdateWalletPrefsResponse>, ApiError> {
    let registry = state.registry.lock().await;

    // Get current wallet to verify it exists and get current values
    let wallet = registry
        .get_wallet(&id)?
        .ok_or_else(|| ApiError::NotFound(format!("Wallet {id} not found")))?;

    let transparent_sync = req.transparent_sync.unwrap_or(wallet.transparent_sync);

    if transparent_sync != wallet.transparent_sync {
        registry.update_transparent_sync(&id, transparent_sync)?;
    }

    Ok(Json(UpdateWalletPrefsResponse {
        id,
        transparent_sync,
    }))
}

pub(crate) async fn lookup_ufvk(
    State(state): State<Arc<AppState>>,
    Json(req): Json<LookupUfvkRequest>,
) -> Result<Json<LookupUfvkResponse>, ApiError> {
    let ufvk_hash = {
        let mut hasher = Sha256::new();
        hasher.update(req.ufvk.as_bytes());
        hex::encode(hasher.finalize())
    };

    let registry = state.registry.lock().await;
    let rows = registry.lookup_by_ufvk_hash(&ufvk_hash)?;

    if rows.is_empty() {
        return Err(ApiError::NotFound(
            "No wallets found for this UFVK".to_string(),
        ));
    }

    let wallets = rows
        .into_iter()
        .map(|(id, _name, network, birthday)| LookupUfvkEntry {
            id,
            network,
            birthday,
        })
        .collect();

    Ok(Json(LookupUfvkResponse { wallets }))
}
