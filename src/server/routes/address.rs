use std::sync::Arc;

use axum::{
    extract::{Path, State},
    Json,
};
use rand::rngs::OsRng;
use zcash_client_backend::data_api::{Account, WalletWrite};
use zcash_client_sqlite::{util::SystemClock, WalletDb};
use zcash_keys::keys::{ReceiverRequirement, UnifiedAddressRequest, UnifiedFullViewingKey};
use zcash_protocol::consensus;
use zip32::DiversifierIndex;

use crate::{
    commands::select_account,
    data::get_db_paths,
    server::{
        error::ApiError,
        types::{AddressResponse, GenerateAddressRequest, ReceiverSelection},
        AppState,
    },
};

pub(crate) async fn generate_address(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(req): Json<GenerateAddressRequest>,
) -> Result<Json<AddressResponse>, ApiError> {
    let (wallet, synced) = {
        let registry = state.registry.lock().await;
        let wallet = registry
            .get_wallet(&id)?
            .ok_or_else(|| ApiError::NotFound(format!("Wallet {id} not found")))?;
        let sync_state = registry.get_sync_state(&id)?;
        let synced = sync_state
            .as_ref()
            .map_or(false, |s| s.last_synced_height.is_some());
        (wallet, synced)
    };

    // Auto-incrementing requires the wallet to have synced at least once,
    // so the diversifier counter reflects on-chain reality.
    if req.diversifier_index.is_none() && !synced {
        return Err(ApiError::BadRequest(
            "Cannot generate next address: wallet has not completed initial sync. \
             Either sync the wallet first or provide an explicit diversifier_index."
                .into(),
        ));
    }

    let network_str = wallet.network.clone();

    let ua_request = match req.receivers {
        None | Some(ReceiverSelection::Orchard) => UnifiedAddressRequest::ORCHARD,
        Some(ReceiverSelection::Sapling) => UnifiedAddressRequest::unsafe_custom(
            ReceiverRequirement::Omit,
            ReceiverRequirement::Require,
            ReceiverRequirement::Omit,
        ),
        Some(ReceiverSelection::Shielded) => UnifiedAddressRequest::SHIELDED,
        Some(ReceiverSelection::All) => UnifiedAddressRequest::AllAvailableKeys,
    };

    // Explicit diversifier index: pure cryptography, no wallet DB needed.
    if let Some(di_value) = req.diversifier_index {
        let params: consensus::Network = match network_str.as_str() {
            "main" => consensus::Network::MainNetwork,
            "test" => consensus::Network::TestNetwork,
            _ => return Err(ApiError::Internal("Invalid network in registry".into())),
        };

        let ufvk = UnifiedFullViewingKey::decode(&params, &wallet.ufvk)
            .map_err(|e| ApiError::Internal(format!("Failed to decode UFVK: {e}")))?;

        let di = DiversifierIndex::try_from(di_value)
            .map_err(|_| ApiError::BadRequest("Invalid diversifier index".into()))?;

        let ua = ufvk.address(di, ua_request).map_err(|e| {
            ApiError::BadRequest(format!(
                "No valid address at the given diversifier index: {e}"
            ))
        })?;

        let address = ua.encode(&params);
        let diversifier_index: u128 = di.into();
        return Ok(Json(AddressResponse {
            id: id.clone(),
            address,
            diversifier_index,
        }));
    }

    // Auto-increment path: needs wallet DB to read/write the diversifier counter.
    let wallet_dir = wallet.wallet_dir.clone();

    let response = tokio::task::spawn_blocking(move || -> Result<AddressResponse, ApiError> {
        let params: consensus::Network = match network_str.as_str() {
            "main" => consensus::Network::MainNetwork,
            "test" => consensus::Network::TestNetwork,
            _ => return Err(ApiError::Internal("Invalid network in registry".into())),
        };

        let (_, db_data_path) = get_db_paths(Some(&wallet_dir));

        for attempt in 0..6 {
            if attempt > 0 {
                std::thread::sleep(std::time::Duration::from_secs(attempt * 2));
            }

            let conn = match crate::server::db::open_wallet_connection(&db_data_path) {
                Ok(c) => c,
                Err(e) => {
                    let msg = e.to_string();
                    if msg.contains("database is locked") {
                        continue;
                    }
                    return Err(ApiError::Internal(format!("Failed to open wallet db: {e}")));
                }
            };
            let mut db_data = WalletDb::from_connection(conn, params, SystemClock, OsRng);

            let account = match select_account(&db_data, None) {
                Ok(a) => a,
                Err(e) => {
                    let msg = e.to_string();
                    if msg.contains("database is locked") {
                        continue;
                    }
                    return Err(ApiError::Internal(format!("Failed to select account: {e}")));
                }
            };

            let result = db_data.get_next_available_address(account.id(), ua_request);

            match result {
                Ok(Some((ua, di))) => {
                    let address = ua.encode(&params);
                    let diversifier_index: u128 = di.into();
                    return Ok(AddressResponse {
                        id,
                        address,
                        diversifier_index,
                    });
                }
                Ok(None) => {
                    return Err(ApiError::Internal(
                        "No address could be generated for this account".into(),
                    ));
                }
                Err(e) => {
                    let msg = format!("{e}");
                    if msg.contains("database is locked") {
                        continue;
                    }
                    return Err(ApiError::Internal(format!(
                        "Failed to generate address: {e}"
                    )));
                }
            }
        }

        Err(ApiError::ServiceUnavailable(
            "Wallet database is currently locked by sync, try again later".into(),
        ))
    })
    .await
    .map_err(|e| ApiError::Internal(format!("Task join error: {e}")))??;

    Ok(Json(response))
}
