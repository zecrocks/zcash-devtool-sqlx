use serde::{Deserialize, Serialize};

// ── Request types ──

#[derive(Debug, Deserialize)]
pub(crate) struct RegisterUfvkRequest {
    pub ufvk: String,
    #[serde(default)]
    pub birthday: Option<u32>,
    pub name: Option<String>,
    #[serde(default = "default_true")]
    pub transparent_sync: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Deserialize)]
pub(crate) struct PaginationParams {
    #[serde(default = "default_page")]
    pub page: u64,
    #[serde(default = "default_per_page")]
    pub per_page: u64,
    #[serde(default = "default_sort")]
    pub sort: SortOrder,
    #[serde(default)]
    pub confirmed: Option<bool>,
}

fn default_page() -> u64 {
    1
}
fn default_per_page() -> u64 {
    20
}
fn default_sort() -> SortOrder {
    SortOrder::Desc
}

#[derive(Debug, Deserialize, Clone, Copy)]
#[serde(rename_all = "lowercase")]
pub(crate) enum SortOrder {
    Asc,
    Desc,
}

// ── Response types ──

#[derive(Debug, Serialize)]
pub(crate) struct HealthResponse {
    pub status: String,
    pub version: String,
    pub uptime_seconds: u64,
    pub wallets: WalletCounts,
}

#[derive(Debug, Serialize)]
pub(crate) struct WalletCounts {
    pub mainnet: u64,
    pub testnet: u64,
}

#[derive(Debug, Serialize)]
pub(crate) struct RegisterUfvkResponse {
    pub id: String,
    pub network: String,
    pub birthday: u32,
    pub name: Option<String>,
    pub transparent_sync: bool,
}

#[derive(Debug, Serialize)]
pub(crate) struct UfvkListResponse {
    pub wallets: Vec<UfvkSummary>,
    pub page: u64,
    pub per_page: u64,
    pub total: u64,
}

#[derive(Debug, Serialize)]
pub(crate) struct UfvkSummary {
    pub id: String,
    pub name: Option<String>,
    pub network: String,
    pub birthday: u32,
    pub created_at: String,
    pub sync_status: Option<SyncStatusEntry>,
}

#[derive(Debug, Serialize)]
pub(crate) struct UfvkDetailResponse {
    pub id: String,
    pub ufvk: String,
    pub name: Option<String>,
    pub network: String,
    pub birthday: u32,
    pub created_at: String,
    pub sync_status: Option<SyncStatusEntry>,
    pub transparent_sync: bool,
}

#[derive(Debug, Serialize, Clone)]
pub(crate) struct SyncStatusEntry {
    pub status: String,
    pub last_synced_height: Option<u32>,
    pub chain_tip_height: Option<u32>,
    pub sync_progress: Option<f64>,
    pub error_message: Option<String>,
    pub last_sync_at: Option<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct BalanceResponse {
    pub id: String,
    pub chain_tip_height: Option<u32>,
    pub scan_progress: Option<f64>,
    pub total: u64,
    pub sapling_spendable: u64,
    pub orchard_spendable: u64,
    #[cfg(feature = "transparent-inputs")]
    pub unshielded_spendable: u64,
}

#[derive(Debug, Serialize)]
pub(crate) struct TransactionListResponse {
    pub transactions: Vec<TransactionEntry>,
    pub page: u64,
    pub per_page: u64,
    pub total: u64,
}

#[derive(Debug, Serialize)]
pub(crate) struct TransactionEntry {
    pub txid: String,
    pub mined_height: Option<u32>,
    pub block_time: Option<i64>,
    pub confirmations: Option<u32>,
    pub account_balance_delta: i64,
    pub fee_paid: Option<u64>,
    pub sent_note_count: u64,
    pub received_note_count: u64,
    pub memo_count: u64,
    pub expired_unmined: bool,
    pub outputs: Vec<TransactionOutputEntry>,
}

#[derive(Debug, Serialize)]
pub(crate) struct TransactionOutputEntry {
    pub pool: String,
    pub output_index: u32,
    pub to_address: Option<String>,
    pub value: u64,
    pub is_change: bool,
    pub memo: Option<String>,
    pub diversifier_index: Option<u128>,
}

#[derive(Debug, Serialize)]
pub(crate) struct SyncOverviewResponse {
    pub wallets: Vec<SyncWalletStatus>,
    pub page: u64,
    pub per_page: u64,
    pub total: u64,
}

#[derive(Debug, Serialize)]
pub(crate) struct SyncWalletStatus {
    pub id: String,
    pub name: Option<String>,
    pub network: String,
    #[serde(flatten)]
    pub sync: SyncStatusEntry,
}

#[derive(Debug, Serialize)]
pub(crate) struct DeleteResponse {
    pub id: String,
    pub deleted: bool,
}

#[derive(Debug, Deserialize)]
pub(crate) struct UpdateWalletPrefsRequest {
    pub transparent_sync: Option<bool>,
}

#[derive(Debug, Serialize)]
pub(crate) struct UpdateWalletPrefsResponse {
    pub id: String,
    pub transparent_sync: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ReceiverSelection {
    Orchard,
    Sapling,
    Shielded,
    All,
}

#[derive(Debug, Deserialize)]
pub(crate) struct GenerateAddressRequest {
    pub diversifier_index: Option<u128>,
    pub receivers: Option<ReceiverSelection>,
}

#[derive(Debug, Serialize)]
pub(crate) struct AddressResponse {
    pub id: String,
    pub address: String,
    pub diversifier_index: u128,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ResolveAddressRequest {
    pub address: String,
    pub wallet_ids: Vec<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct ResolveAddressResponse {
    pub address: String,
    pub wallet_id: String,
    pub diversifier_index: u128,
    pub matched_pools: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct AddressBalanceRequest {
    pub address: String,
    pub wallet_ids: Vec<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct AddressBalanceResponse {
    pub address: String,
    pub wallet_id: String,
    pub diversifier_index: u128,
    pub balance: u64,
    pub total_received: u64,
    pub last_received_height: Option<u32>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct LookupUfvkRequest {
    pub ufvk: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct LookupUfvkResponse {
    pub wallets: Vec<LookupUfvkEntry>,
}

#[derive(Debug, Serialize)]
pub(crate) struct LookupUfvkEntry {
    pub id: String,
    pub network: String,
    pub birthday: u32,
}

#[derive(Debug, Serialize)]
pub(crate) struct ErrorResponse {
    pub error: String,
}
