use std::sync::Arc;

use axum::{
    extract::{Path, State},
    Json,
};
use uuid::Uuid;
use zcash_client_backend::data_api::{wallet::ConfirmationsPolicy, Account as _, WalletRead};
use zcash_client_sqlite::WalletDb;
use zcash_protocol::consensus;

use crate::{
    commands::select_account,
    data::get_db_paths,
    server::{error::ApiError, types::BalanceResponse, AppState},
};

pub(crate) async fn get_balance(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
) -> Result<Json<BalanceResponse>, ApiError> {
    // Look up the wallet from the registry
    let wallet = {
        let registry = state.registry.lock().await;
        registry
            .get_wallet(id)?
            .ok_or_else(|| ApiError::NotFound(format!("Wallet {id} not found")))?
    };

    let wallet_dir = wallet.wallet_dir.clone();
    let network_str = wallet.network.clone();

    // Read balance in a blocking task since rusqlite is !Send.
    // Retry with backoff if the database is locked by a concurrent sync.
    let balance = tokio::task::spawn_blocking(move || -> Result<BalanceResponse, ApiError> {
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

            let conn = crate::server::db::open_wallet_connection(&db_data_path)
                .map_err(|e| ApiError::Internal(format!("Failed to open wallet db: {e}")))?;
            let db_data = WalletDb::from_connection(conn, params, (), ());

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

            let wallet_summary = match db_data
                .get_wallet_summary(ConfirmationsPolicy::default())
            {
                Ok(s) => s,
                Err(e) => {
                    let msg = format!("{e}");
                    if msg.contains("database is locked") {
                        continue;
                    }
                    return Err(ApiError::Internal(format!("Failed to get wallet summary: {e}")));
                }
            };

            return match wallet_summary {
                Some(summary) => {
                    let balance = summary
                        .account_balances()
                        .get(&account.id())
                        .ok_or_else(|| ApiError::Internal("Missing account balance".into()))?;

                    let scan_progress = summary.progress().scan();
                    let progress = if *scan_progress.denominator() > 0 {
                        Some(
                            (*scan_progress.numerator() as f64)
                                / (*scan_progress.denominator() as f64),
                        )
                    } else {
                        None
                    };

                    Ok(BalanceResponse {
                        id,
                        chain_tip_height: Some(u32::from(summary.chain_tip_height())),
                        scan_progress: progress,
                        total: balance.total().into_u64(),
                        sapling_spendable: balance.sapling_balance().spendable_value().into_u64(),
                        orchard_spendable: balance.orchard_balance().spendable_value().into_u64(),
                        #[cfg(feature = "transparent-inputs")]
                        unshielded_spendable: balance
                            .unshielded_balance()
                            .spendable_value()
                            .into_u64(),
                    })
                }
                None => Ok(BalanceResponse {
                    id,
                    chain_tip_height: None,
                    scan_progress: None,
                    total: 0,
                    sapling_spendable: 0,
                    orchard_spendable: 0,
                    #[cfg(feature = "transparent-inputs")]
                    unshielded_spendable: 0,
                }),
            };
        }

        Err(ApiError::Internal("Wallet is currently syncing, try again later".into()))
    })
    .await
    .map_err(|e| ApiError::Internal(format!("Task join error: {e}")))??;

    Ok(Json(balance))
}
