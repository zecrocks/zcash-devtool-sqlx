use std::sync::Arc;

use axum::{
    extract::{Path, State},
    Json,
};
use rand::rngs::OsRng;
use zcash_address::{
    unified::{self, Container},
    TryFromAddress, ZcashAddress,
};
use zcash_client_backend::data_api::{Account, WalletWrite};
use zcash_client_sqlite::{util::SystemClock, WalletDb};
use zcash_keys::keys::{ReceiverRequirement, UnifiedAddressRequest, UnifiedFullViewingKey};
use zcash_protocol::consensus::{self, NetworkType};
use zip32::DiversifierIndex;

use crate::{
    commands::select_account,
    data::get_db_paths,
    server::{
        error::ApiError,
        types::{
            AddressBalanceRequest, AddressBalanceResponse, AddressResponse,
            GenerateAddressRequest, ReceiverSelection, ResolveAddressRequest,
            ResolveAddressResponse,
        },
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

// Helper to parse a ZcashAddress into a unified::Address + NetworkType
struct ParsedUa {
    net: NetworkType,
    ua: unified::Address,
}

impl TryFromAddress for ParsedUa {
    type Error = &'static str;

    fn try_from_unified(
        net: NetworkType,
        data: unified::Address,
    ) -> Result<Self, zcash_address::ConversionError<Self::Error>> {
        Ok(ParsedUa { net, ua: data })
    }
}

pub(crate) async fn resolve_address(
    State(state): State<Arc<AppState>>,
    Json(req): Json<ResolveAddressRequest>,
) -> Result<Json<ResolveAddressResponse>, ApiError> {
    if req.wallet_ids.is_empty() {
        return Err(ApiError::BadRequest(
            "wallet_ids must not be empty".into(),
        ));
    }

    // Parse the address string as a UA
    let zaddr: ZcashAddress = req
        .address
        .parse()
        .map_err(|e| ApiError::BadRequest(format!("Invalid Zcash address: {e}")))?;

    let parsed = zaddr
        .convert::<ParsedUa>()
        .map_err(|_| ApiError::BadRequest("Address is not a Unified Address".into()))?;

    let params: consensus::Network = match parsed.net {
        NetworkType::Main => consensus::Network::MainNetwork,
        NetworkType::Test => consensus::Network::TestNetwork,
        _ => {
            return Err(ApiError::BadRequest(
                "Unsupported network type".into(),
            ))
        }
    };

    // Extract Sapling and Orchard receivers from the UA
    let mut sapling_bytes: Option<[u8; 43]> = None;
    let mut orchard_bytes: Option<[u8; 43]> = None;

    for receiver in parsed.ua.items() {
        match receiver {
            unified::Receiver::Sapling(data) => sapling_bytes = Some(data),
            unified::Receiver::Orchard(data) => {
                orchard_bytes = Some(data.try_into().map_err(|_| {
                    ApiError::Internal("Unexpected Orchard receiver length".into())
                })?)
            }
            _ => {}
        }
    }

    if sapling_bytes.is_none() && orchard_bytes.is_none() {
        return Err(ApiError::BadRequest(
            "UA contains no Sapling or Orchard receivers to resolve".into(),
        ));
    }

    // Try each wallet
    let registry = state.registry.lock().await;

    for wallet_id in &req.wallet_ids {
        let wallet = registry
            .get_wallet(wallet_id)?
            .ok_or_else(|| ApiError::NotFound(format!("Wallet {wallet_id} not found")))?;

        let ufvk = UnifiedFullViewingKey::decode(&params, &wallet.ufvk)
            .map_err(|e| ApiError::Internal(format!("Failed to decode UFVK: {e}")))?;

        let mut matched_pools = Vec::new();
        let mut matched_di: Option<u128> = None;

        // Try Sapling
        if let Some(sapling_data) = sapling_bytes {
            if let Some(dfvk) = ufvk.sapling() {
                if let Some(addr) = sapling::PaymentAddress::from_bytes(&sapling_data) {
                    if let Some((di, _scope)) = dfvk.decrypt_diversifier(&addr) {
                        let di_val: u128 = di.into();
                        matched_pools.push("sapling".to_string());
                        matched_di = Some(di_val);
                    }
                }
            }
        }

        // Try Orchard
        if let Some(orchard_data) = orchard_bytes {
            if let Some(fvk) = ufvk.orchard() {
                if let Some(addr) =
                    orchard::Address::from_raw_address_bytes(&orchard_data).into()
                {
                    let ivk = fvk.to_ivk(orchard::keys::Scope::External);
                    if let Some(di) = ivk.diversifier_index(&addr) {
                        let di_val: u128 = di.into();
                        matched_pools.push("orchard".to_string());
                        matched_di = Some(di_val);
                    }
                }
            }
        }

        if let Some(diversifier_index) = matched_di {
            return Ok(Json(ResolveAddressResponse {
                wallet_id: wallet_id.clone(),
                diversifier_index,
                matched_pools,
            }));
        }
    }

    Err(ApiError::NotFound(
        "No matching wallet found for the given address".into(),
    ))
}

pub(crate) async fn address_balance(
    State(state): State<Arc<AppState>>,
    Json(req): Json<AddressBalanceRequest>,
) -> Result<Json<AddressBalanceResponse>, ApiError> {
    if req.wallet_ids.is_empty() {
        return Err(ApiError::BadRequest(
            "wallet_ids must not be empty".into(),
        ));
    }

    // Parse the address string as a UA
    let zaddr: ZcashAddress = req
        .address
        .parse()
        .map_err(|e| ApiError::BadRequest(format!("Invalid Zcash address: {e}")))?;

    let parsed = zaddr
        .convert::<ParsedUa>()
        .map_err(|_| ApiError::BadRequest("Address is not a Unified Address".into()))?;

    // Validate: must have exactly 1 receiver and it must be Orchard
    let items = parsed.ua.items();
    if items.len() != 1 {
        return Err(ApiError::BadRequest(
            "Address must contain exactly one receiver (Orchard only)".into(),
        ));
    }
    let orchard_bytes: [u8; 43] = match &items[0] {
        unified::Receiver::Orchard(data) => data.clone().try_into().map_err(|_| {
            ApiError::Internal("Unexpected Orchard receiver length".into())
        })?,
        _ => {
            return Err(ApiError::BadRequest(
                "The single receiver must be Orchard".into(),
            ))
        }
    };

    let params: consensus::Network = match parsed.net {
        NetworkType::Main => consensus::Network::MainNetwork,
        NetworkType::Test => consensus::Network::TestNetwork,
        _ => {
            return Err(ApiError::BadRequest(
                "Unsupported network type".into(),
            ))
        }
    };

    let orchard_addr: orchard::Address =
        Option::from(orchard::Address::from_raw_address_bytes(&orchard_bytes))
            .ok_or_else(|| ApiError::BadRequest("Invalid Orchard receiver bytes".into()))?;

    // Try each wallet to find a match
    let registry = state.registry.lock().await;

    for wallet_id in &req.wallet_ids {
        let wallet = registry
            .get_wallet(wallet_id)?
            .ok_or_else(|| ApiError::NotFound(format!("Wallet {wallet_id} not found")))?;

        let ufvk = UnifiedFullViewingKey::decode(&params, &wallet.ufvk)
            .map_err(|e| ApiError::Internal(format!("Failed to decode UFVK: {e}")))?;

        let fvk = match ufvk.orchard() {
            Some(fvk) => fvk,
            None => continue,
        };

        let ivk = fvk.to_ivk(orchard::keys::Scope::External);
        let di = match ivk.diversifier_index(&orchard_addr) {
            Some(di) => di,
            None => continue,
        };

        let di_val: u128 = di.into();

        // Convert diversifier index to big-endian bytes for DB lookup
        let di_bytes = di.as_bytes();
        let mut di_be = di_bytes.to_vec();
        di_be.reverse();

        let wallet_dir = wallet.wallet_dir.clone();
        let wallet_id = wallet_id.clone();

        // Drop the registry lock before the blocking task
        drop(registry);

        let response =
            tokio::task::spawn_blocking(move || -> Result<AddressBalanceResponse, ApiError> {
                let db_data_path =
                    std::path::PathBuf::from(&wallet_dir).join("data.sqlite");

                for attempt in 0..6u64 {
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
                            return Err(ApiError::Internal(format!(
                                "Failed to open wallet db: {e}"
                            )));
                        }
                    };

                    // Get the single account's numeric id
                    let account_id: u32 = match conn.query_row(
                        "SELECT id FROM accounts LIMIT 1",
                        [],
                        |row| row.get(0),
                    ) {
                        Ok(id) => id,
                        Err(e) => {
                            let msg = e.to_string();
                            if msg.contains("database is locked") {
                                continue;
                            }
                            return Err(ApiError::Internal(format!(
                                "Failed to query account: {e}"
                            )));
                        }
                    };

                    let result: Result<(u64, u64, Option<u32>), rusqlite::Error> = (|| {
                        let mut stmt = conn.prepare(
                            "SELECT
                                COALESCE(SUM(rn.value), 0),
                                MAX(t.mined_height),
                                COALESCE(SUM(CASE
                                    WHEN rn.id NOT IN (
                                        SELECT orchard_received_note_id
                                        FROM orchard_received_note_spends rns
                                        JOIN transactions stx ON stx.id_tx = rns.transaction_id
                                        WHERE stx.mined_height IS NOT NULL
                                    ) THEN rn.value
                                    ELSE 0
                                END), 0)
                            FROM orchard_received_notes rn
                            JOIN transactions t ON t.id_tx = rn.transaction_id
                            JOIN addresses a ON a.id = rn.address_id
                            WHERE a.account_id = :account_id
                              AND a.diversifier_index_be = :di_be
                              AND t.mined_height IS NOT NULL",
                        )?;

                        stmt.query_row(
                            rusqlite::named_params! {
                                ":account_id": account_id,
                                ":di_be": di_be,
                            },
                            |row| {
                                let total_received: u64 = row.get(0)?;
                                let last_received_height: Option<u32> = row.get(1)?;
                                let balance: u64 = row.get(2)?;
                                Ok((total_received, balance, last_received_height))
                            },
                        )
                    })();

                    match result {
                        Ok((total_received, balance, last_received_height)) => {
                            return Ok(AddressBalanceResponse {
                                wallet_id,
                                diversifier_index: di_val,
                                balance,
                                total_received,
                                last_received_height,
                            });
                        }
                        Err(e) => {
                            let msg = format!("{e}");
                            if msg.contains("database is locked") {
                                continue;
                            }
                            return Err(ApiError::Internal(format!(
                                "Failed to query balance: {e}"
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

        return Ok(Json(response));
    }

    Err(ApiError::NotFound(
        "No matching wallet found for the given address".into(),
    ))
}
