# Data Storage Overview

## On-Disk Layout

```
{data_dir}/                          (default: ./zcash-indexer-data)
├── registry.sqlite                  # Central registry DB (WAL mode)
└── wallets/
    └── {wallet-uuid}/               # One directory per registered UFVK
        ├── keys.toml                # Wallet config (network, birthday)
        ├── data.sqlite              # zcash_client_sqlite wallet DB
        ├── blockmeta.sqlite         # Block metadata cache
        ├── blocks/                  # Downloaded compact blocks (binary)
        └── tor/                     # (optional) Tor state
```

## 1. Registry Database (`registry.sqlite`)

Opened once at startup, held in `AppState` behind `Arc<Mutex<…>>`. Uses SQLite WAL mode.

### `watched_wallets` table

Master list of all registered UFVKs.

| Column | Type | Purpose |
|---|---|---|
| `id` | TEXT (PK) | UUID |
| `ufvk` | TEXT | Full viewing key string |
| `ufvk_hash` | TEXT | SHA-256 for dedup |
| `name` | TEXT, nullable | Optional user label |
| `network` | TEXT | `"main"` or `"test"` (derived from UFVK decode) |
| `birthday` | INTEGER | Start scan height |
| `wallet_dir` | TEXT, UNIQUE | Absolute path to wallet directory |
| `created_at` | TEXT | Timestamp (default: now) |
| `deleted_at` | TEXT, nullable | Soft-delete timestamp |

### `sync_state` table

One row per wallet tracking sync progress.

| Column | Type | Purpose |
|---|---|---|
| `wallet_id` | TEXT (PK, FK) | References `watched_wallets(id)` |
| `status` | TEXT | `"pending"` / `"syncing"` / `"synced"` / `"error"` |
| `last_synced_height` | INTEGER, nullable | Last successfully synced block height |
| `chain_tip_height` | INTEGER, nullable | Latest known block height |
| `sync_progress` | REAL, nullable | 0.0 -- 1.0 |
| `error_message` | TEXT, nullable | Error description if sync failed |
| `last_sync_at` | TEXT, nullable | Timestamp of last sync attempt |

## 2. Per-Wallet `data.sqlite`

Managed entirely by `zcash_client_sqlite`. Contains accounts, notes, transactions, and commitment tree state. Created at registration time, populated continuously by the sync manager.

- **Written by:** `SyncManager` background threads -- imports subtree roots, scans blocks, stores transparent UTXOs.
- **Read by:** HTTP handlers for balance and transactions -- opened read-only on demand with retry/backoff for lock contention.

## 3. Per-Wallet `blockmeta.sqlite` + `blocks/`

Block cache managed by `zcash_client_backend`'s `FsBlockDb`. Compact blocks are downloaded from lightwalletd, written to `blocks/`, and indexed in `blockmeta.sqlite`. The sync manager reads from here during scanning.

## 4. Per-Wallet `keys.toml`

Wallet configuration file containing network, birthday height, and an optional encrypted mnemonic (if the wallet was initialized with one). Created at registration time.

## 5. In-Memory State (`AppState`)

| Field | Type | What it holds |
|---|---|---|
| `registry` | `Arc<Mutex<WalletRegistry>>` | Single connection to `registry.sqlite` |
| `sync_tx` | `mpsc::Sender<SyncCommand>` | Channel to start/stop wallet syncs |
| `config` | `DaemonConfig` | Data dir, lightwalletd endpoints, connection mode, sync interval |
| `start_time` | `Instant` | For uptime reporting |

No wallet data is cached in memory. Individual wallet DBs are opened fresh per request.

## Data Flow

### Registration (`POST /ufvks`)

1. Create wallet directory `{data_dir}/wallets/{new_uuid}/`
2. Write `keys.toml`
3. Initialize `data.sqlite` and `blockmeta.sqlite`
4. Import UFVK into wallet DB
5. Insert wallet metadata and initial sync state into registry
6. Signal SyncManager to start syncing

### Syncing (background, per-wallet thread)

1. Read wallet metadata from registry
2. Open wallet DB (`data.sqlite`) and block DB (`blockmeta.sqlite`)
3. Connect to lightwalletd
4. Download subtree roots, chain tip, and compact blocks
5. Write blocks to `blocks/` directory
6. Scan blocks into `data.sqlite` (notes, transactions, tree state)
7. Update `sync_state` in registry
8. Sleep for `sync_interval` seconds, then repeat

### Reading data (HTTP)

- **Balance** (`GET /ufvks/{id}/balance`): opens `data.sqlite` read-only, queries via `zcash_client_sqlite` API.
- **Transactions** (`GET /ufvks/{id}/transactions`): opens `data.sqlite` read-only, queries `v_transactions` view.
- **Sync status** (`GET /sync/status`): reads from registry DB's `sync_state` table.
