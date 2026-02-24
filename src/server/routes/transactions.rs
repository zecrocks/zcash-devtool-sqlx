use std::sync::Arc;

use axum::{
    extract::{Path, State},
    Json,
};
use axum_extra::extract::Query;
use rusqlite::{named_params, Connection};
use zcash_address::{
    unified::{self, Container, Encoding},
    TryFromAddress, ZcashAddress,
};
use zcash_protocol::{
    consensus::NetworkType,
    memo::{Memo, MemoBytes},
};

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
    let network = wallet.network.clone();
    let page = params.page;
    let per_page = params.per_page;
    let sort = params.sort;
    let confirmed = params.confirmed;

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
                    tracing::warn!("Failed to open wallet db: {e}");
                    last_err = Some("Failed to open wallet database".into());
                    continue;
                }
            };
            let _ = conn.pragma_update(None, "journal_mode", "WAL");
            let _ = conn.busy_timeout(std::time::Duration::from_secs(10));
            let _ = rusqlite::vtab::array::load_module(&conn);

            match query_transactions(&conn, page, per_page, sort, &network, confirmed) {
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

pub(crate) async fn get_transaction(
    State(state): State<Arc<AppState>>,
    Path((id, txid)): Path<(String, String)>,
) -> Result<Json<TransactionEntry>, ApiError> {
    if txid.len() != 64 || hex::decode(&txid).is_err() {
        return Err(ApiError::BadRequest("Invalid txid: expected 64 hex characters".into()));
    }

    let wallet = {
        let registry = state.registry.lock().await;
        registry
            .get_wallet(&id)?
            .ok_or_else(|| ApiError::NotFound(format!("Wallet {id} not found")))?
    };

    let wallet_dir = wallet.wallet_dir.clone();
    let network = wallet.network.clone();

    let result = tokio::task::spawn_blocking(move || -> Result<TransactionEntry, ApiError> {
        let db_path = std::path::PathBuf::from(&wallet_dir).join("data.sqlite");

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
                    tracing::warn!("Failed to open wallet db: {e}");
                    last_err = Some("Failed to open wallet database".into());
                    continue;
                }
            };
            let _ = conn.pragma_update(None, "journal_mode", "WAL");
            let _ = conn.busy_timeout(std::time::Duration::from_secs(10));
            let _ = rusqlite::vtab::array::load_module(&conn);

            match query_transaction_by_txid(&conn, &txid, &network) {
                Ok(Some(entry)) => return Ok(entry),
                Ok(None) => return Err(ApiError::NotFound(format!("Transaction {txid} not found"))),
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

/// If `address` is a multi-receiver UA, return a new UA containing only the
/// receiver that matches `pool_code` (2=sapling, 3=orchard). Falls back to the
/// original address on any parse error or if there's only one receiver.
fn strip_to_pool_receiver(address: &str, pool_code: i64, network: &str) -> String {
    let Ok(zaddr) = address.parse::<ZcashAddress>() else {
        return address.to_string();
    };
    let Ok(parsed) = zaddr.convert::<ParsedUa>() else {
        return address.to_string();
    };

    let items = parsed.ua.items();
    if items.len() <= 1 {
        return address.to_string();
    }

    let target = match pool_code {
        2 => items.iter().find(|r| matches!(r, unified::Receiver::Sapling(_))),
        3 => items.iter().find(|r| matches!(r, unified::Receiver::Orchard(_))),
        _ => return address.to_string(),
    };

    let Some(receiver) = target else {
        return address.to_string();
    };

    let net = match network {
        "main" => NetworkType::Main,
        "test" => NetworkType::Test,
        _ => parsed.net,
    };

    match unified::Address::try_from_items(vec![receiver.clone()]) {
        Ok(new_ua) => new_ua.encode(&net),
        Err(_) => address.to_string(),
    }
}

fn query_outputs(
    stmt_outputs: &mut rusqlite::Statement,
    txid_bytes: &[u8],
    network: &str,
) -> Result<Vec<TransactionOutputEntry>, ApiError> {
    stmt_outputs
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

            let to_address = to_address.map(|addr| {
                strip_to_pool_receiver(&addr, pool_code, network)
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
        .map_err(|e| ApiError::Internal(format!("Failed to collect outputs: {e}")))
}

const OUTPUTS_QUERY: &str =
    "SELECT vto.output_pool, vto.output_index, vto.to_address, vto.value, vto.is_change, vto.memo,
            a.diversifier_index_be
     FROM v_tx_outputs vto
     LEFT JOIN sapling_received_notes srn
         ON srn.transaction_id = vto.transaction_id AND srn.output_index = vto.output_index AND vto.output_pool = 2
     LEFT JOIN orchard_received_notes orn
         ON orn.transaction_id = vto.transaction_id AND orn.action_index = vto.output_index AND vto.output_pool = 3
     LEFT JOIN addresses a
         ON a.id = COALESCE(srn.address_id, orn.address_id)
     WHERE vto.txid = :txid";

fn build_entry(
    txid_bytes: &[u8],
    mined_height: Option<u32>,
    account_balance_delta: i64,
    fee_paid: Option<u64>,
    sent_note_count: u64,
    received_note_count: u64,
    memo_count: u64,
    block_time: Option<i64>,
    expired_unmined: bool,
    chain_tip: Option<u32>,
    stmt_outputs: &mut rusqlite::Statement,
    network: &str,
) -> Result<TransactionEntry, ApiError> {
    let outputs = query_outputs(stmt_outputs, txid_bytes, network)?;
    let confirmations = match (mined_height, chain_tip) {
        (Some(h), Some(tip)) if tip >= h => Some(tip - h + 1),
        _ => None,
    };
    Ok(TransactionEntry {
        txid: hex::encode(txid_bytes),
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
    })
}

fn chain_tip(conn: &Connection) -> Option<u32> {
    conn.query_row("SELECT MAX(height) FROM blocks", [], |row| row.get(0))
        .unwrap_or(None)
}

fn query_transactions(
    conn: &Connection,
    page: u64,
    per_page: u64,
    sort: SortOrder,
    network: &str,
    confirmed: Option<bool>,
) -> Result<TransactionListResponse, ApiError> {
    let tip = chain_tip(conn);

    let where_clause = match confirmed {
        Some(true) => match tip {
            Some(t) => format!("WHERE mined_height IS NOT NULL AND ({t} - mined_height + 1) >= 10"),
            None => "WHERE 0".to_string(), // no chain tip means nothing is confirmed
        },
        _ => "WHERE NOT expired_unmined".to_string(),
    };
    let where_clause = format!("{where_clause} AND raw IS NOT NULL");

    let count_query = format!("SELECT COUNT(*) FROM v_transactions {where_clause}");
    let total: u64 = conn
        .query_row(&count_query, [], |row| row.get(0))
        .map_err(|e| ApiError::Internal(format!("Failed to count transactions: {e}")))?;
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
         {where_clause}
         ORDER BY sort_height {order}
         LIMIT :limit OFFSET :offset"
    );

    let mut stmt_txs = conn
        .prepare(&query)
        .map_err(|e| ApiError::Internal(format!("Failed to prepare query: {e}")))?;

    let mut stmt_outputs = conn
        .prepare(OUTPUTS_QUERY)
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
        entries.push(build_entry(
            &txid_bytes,
            mined_height,
            account_balance_delta,
            fee_paid,
            sent_note_count,
            received_note_count,
            memo_count,
            block_time,
            expired_unmined,
            tip,
            &mut stmt_outputs,
            network,
        )?);
    }

    Ok(TransactionListResponse {
        transactions: entries,
        page,
        per_page,
        total,
    })
}

fn query_transaction_by_txid(
    conn: &Connection,
    txid_hex: &str,
    network: &str,
) -> Result<Option<TransactionEntry>, ApiError> {
    let txid_bytes = hex::decode(txid_hex)
        .map_err(|e| ApiError::BadRequest(format!("Invalid txid hex: {e}")))?;

    let tip = chain_tip(conn);

    let row = conn
        .query_row(
            "SELECT mined_height, txid, account_balance_delta, fee_paid,
                    sent_note_count, received_note_count, memo_count, block_time, expired_unmined
             FROM v_transactions
             WHERE txid = :txid AND raw IS NOT NULL",
            named_params! {":txid": txid_bytes},
            |row| {
                Ok((
                    row.get::<_, Option<u32>>("mined_height")?,
                    row.get::<_, i64>("account_balance_delta")?,
                    row.get::<_, Option<u64>>("fee_paid")?,
                    row.get::<_, u64>("sent_note_count")?,
                    row.get::<_, u64>("received_note_count")?,
                    row.get::<_, u64>("memo_count")?,
                    row.get::<_, Option<i64>>("block_time")?,
                    row.get::<_, bool>("expired_unmined")?,
                ))
            },
        );

    let (
        mined_height,
        account_balance_delta,
        fee_paid,
        sent_note_count,
        received_note_count,
        memo_count,
        block_time,
        expired_unmined,
    ) = match row {
        Ok(r) => r,
        Err(rusqlite::Error::QueryReturnedNoRows) => return Ok(None),
        Err(e) => return Err(ApiError::Internal(format!("Failed to query transaction: {e}"))),
    };

    let mut stmt_outputs = conn
        .prepare(OUTPUTS_QUERY)
        .map_err(|e| ApiError::Internal(format!("Failed to prepare output query: {e}")))?;

    let entry = build_entry(
        &txid_bytes,
        mined_height,
        account_balance_delta,
        fee_paid,
        sent_note_count,
        received_note_count,
        memo_count,
        block_time,
        expired_unmined,
        tip,
        &mut stmt_outputs,
        network,
    )?;

    Ok(Some(entry))
}
