use std::sync::Arc;

use axum::{
    extract::{Path, State},
    Json,
};
use axum_extra::extract::Query;
use rusqlite::{named_params, Connection};
use zcash_protocol::memo::{Memo, MemoBytes};

use crate::server::{
    error::ApiError,
    types::{
        PaginationParams, SortOrder, TransactionEntry, TransactionListResponse,
        TransactionOutputEntry,
    },
    AppState,
};

pub(crate) async fn get_transactions(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(params): Query<PaginationParams>,
) -> Result<Json<TransactionListResponse>, ApiError> {
    // Look up the wallet from the registry
    let wallet = {
        let registry = state.registry.lock().await;
        registry
            .get_wallet(&id)?
            .ok_or_else(|| ApiError::NotFound(format!("Wallet {id} not found")))?
    };

    let wallet_dir = wallet.wallet_dir.clone();
    let page = params.page;
    let per_page = params.per_page;
    let sort = params.sort;

    let result = tokio::task::spawn_blocking(move || -> Result<TransactionListResponse, ApiError> {
        let db_path = std::path::PathBuf::from(&wallet_dir).join("data.sqlite");

        // Retry loop to handle DB lock contention during active sync
        let mut last_err = None;
        for attempt in 0..6u64 {
            if attempt > 0 {
                std::thread::sleep(std::time::Duration::from_secs(attempt * 2));
            }

            let conn = match Connection::open_with_flags(
                &db_path,
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
            ) {
                Ok(c) => c,
                Err(e) => {
                    last_err = Some(format!("Failed to open wallet db: {e}"));
                    continue;
                }
            };
            let _ = conn.pragma_update(None, "journal_mode", "WAL");
            let _ = conn.busy_timeout(std::time::Duration::from_secs(10));
            let _ = rusqlite::vtab::array::load_module(&conn);

            match query_transactions(&conn, page, per_page, sort) {
                Ok(result) => return Ok(result),
                Err(e) => {
                    let msg = format!("{e}");
                    if msg.contains("database is locked") {
                        last_err = Some(msg);
                        continue;
                    }
                    return Err(e);
                }
            }
        }

        Err(ApiError::Internal(last_err.unwrap_or_else(|| "Wallet is currently syncing, try again later".into())))
    })
    .await
    .map_err(|e| ApiError::Internal(format!("Task join error: {e}")))?;

    Ok(Json(result?))
}

fn query_transactions(
    conn: &Connection,
    page: u64,
    per_page: u64,
    sort: SortOrder,
) -> Result<TransactionListResponse, ApiError> {
    let total: u64 = conn
        .query_row("SELECT COUNT(*) FROM v_transactions", [], |row| row.get(0))
        .map_err(|e| ApiError::Internal(format!("Failed to count transactions: {e}")))?;

    let chain_tip: Option<u32> = conn
        .query_row(
            "SELECT MAX(height) FROM blocks",
            [],
            |row| row.get(0),
        )
        .unwrap_or(None);

    let offset = (page.saturating_sub(1)) * per_page;

    let order = match sort {
        SortOrder::Asc => "ASC NULLS LAST",
        SortOrder::Desc => "DESC NULLS FIRST",
    };

    let query = format!(
        "SELECT mined_height, txid, expiry_height, account_balance_delta, fee_paid,
                sent_note_count, received_note_count, memo_count, block_time, expired_unmined,
                COALESCE(
                    mined_height,
                    CASE WHEN expiry_height == 0 THEN NULL ELSE expiry_height END
                ) AS sort_height
         FROM v_transactions
         ORDER BY sort_height {order}
         LIMIT :limit OFFSET :offset"
    );

    let mut stmt_txs = conn
        .prepare(&query)
        .map_err(|e| ApiError::Internal(format!("Failed to prepare query: {e}")))?;

    let mut stmt_outputs = conn
        .prepare(
            "SELECT vto.output_pool, vto.output_index, vto.to_address, vto.value, vto.is_change, vto.memo,
                    a.diversifier_index_be
             FROM v_tx_outputs vto
             LEFT JOIN sapling_received_notes srn
                 ON srn.transaction_id = vto.transaction_id AND srn.output_index = vto.output_index AND vto.output_pool = 2
             LEFT JOIN orchard_received_notes orn
                 ON orn.transaction_id = vto.transaction_id AND orn.action_index = vto.output_index AND vto.output_pool = 3
             LEFT JOIN addresses a
                 ON a.id = COALESCE(srn.address_id, orn.address_id)
             WHERE vto.txid = :txid",
        )
        .map_err(|e| ApiError::Internal(format!("Failed to prepare output query: {e}")))?;

    let transactions = stmt_txs
        .query_map(
            named_params! {":limit": per_page, ":offset": offset},
            |row| {
                let txid_bytes: Vec<u8> = row.get("txid")?;
                let mined_height: Option<u32> = row.get("mined_height")?;
                let account_balance_delta: i64 = row.get("account_balance_delta")?;
                let fee_paid: Option<u64> = row.get("fee_paid")?;
                let sent_note_count: u64 = row.get("sent_note_count")?;
                let received_note_count: u64 = row.get("received_note_count")?;
                let memo_count: u64 = row.get("memo_count")?;
                let block_time: Option<i64> = row.get("block_time")?;
                let expired_unmined: bool = row.get("expired_unmined")?;

                Ok((
                    txid_bytes,
                    mined_height,
                    account_balance_delta,
                    fee_paid,
                    sent_note_count,
                    received_note_count,
                    memo_count,
                    block_time,
                    expired_unmined,
                ))
            },
        )
        .map_err(|e| ApiError::Internal(format!("Failed to query transactions: {e}")))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| ApiError::Internal(format!("Failed to collect transactions: {e}")))?;

    let mut entries = Vec::with_capacity(transactions.len());
    for (
        txid_bytes,
        mined_height,
        account_balance_delta,
        fee_paid,
        sent_note_count,
        received_note_count,
        memo_count,
        block_time,
        expired_unmined,
    ) in transactions
    {
        let txid_hex = hex::encode(&txid_bytes);

        let outputs = stmt_outputs
            .query_map(named_params! {":txid": txid_bytes}, |out_row| {
                let pool_code: i64 = out_row.get("output_pool")?;
                let output_index: u32 = out_row.get("output_index")?;
                let to_address: Option<String> = out_row.get("to_address")?;
                let value: i64 = out_row.get("value")?;
                let is_change: bool = out_row.get("is_change")?;
                let memo_bytes: Option<Vec<u8>> = out_row.get("memo")?;

                let pool = match pool_code {
                    0 => "transparent",
                    2 => "sapling",
                    3 => "orchard",
                    _ => "unknown",
                }
                .to_string();

                let memo = memo_bytes.and_then(|b| {
                    MemoBytes::from_bytes(&b)
                        .ok()
                        .and_then(|mb| Memo::try_from(mb).ok())
                        .and_then(|m| match m {
                            Memo::Empty => None,
                            Memo::Text(t) => Some(t.to_string()),
                            Memo::Future(_) => Some("[future memo]".to_string()),
                            Memo::Arbitrary(_) => Some("[arbitrary data]".to_string()),
                        })
                });

                let di_bytes: Option<Vec<u8>> = out_row.get("diversifier_index_be")?;
                let diversifier_index = di_bytes.map(|mut b| {
                    b.reverse(); // big-endian -> little-endian
                    let mut arr = [0u8; 16];
                    arr[..b.len().min(16)].copy_from_slice(&b[..b.len().min(16)]);
                    u128::from_le_bytes(arr)
                });

                Ok(TransactionOutputEntry {
                    pool,
                    output_index,
                    to_address,
                    value: value as u64,
                    is_change,
                    memo,
                    diversifier_index,
                })
            })
            .map_err(|e| ApiError::Internal(format!("Failed to query outputs: {e}")))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| ApiError::Internal(format!("Failed to collect outputs: {e}")))?;

        let confirmations = match (mined_height, chain_tip) {
            (Some(h), Some(tip)) if tip >= h => Some(tip - h + 1),
            _ => None,
        };

        entries.push(TransactionEntry {
            txid: txid_hex,
            mined_height,
            block_time,
            confirmations,
            account_balance_delta,
            fee_paid,
            sent_note_count,
            received_note_count,
            memo_count,
            expired_unmined,
            outputs,
        });
    }

    Ok(TransactionListResponse {
        transactions: entries,
        page,
        per_page,
        total,
    })
}
