# HTTP API Reference

Base URL: `http://localhost:8080`

All responses are JSON. Errors return `{"error": "message"}` with an appropriate HTTP status code.

## Health

### GET /health

Returns server status.

**Response:**
```json
{
  "status": "ok",
  "version": "0.1.0",
  "uptime_seconds": 3600,
  "wallets": {
    "mainnet": 2,
    "testnet": 1
  }
}
```

## Wallets (UFVKs)

Wallets are registered by Unified Full Viewing Key (UFVK). Each wallet gets a deterministic ID derived from the UFVK and birthday height.

### POST /ufvks

Register a new wallet.

**Request body:**
```json
{
  "ufvk": "uview1...",
  "birthday": 2400000,
  "name": "my-wallet"
}
```

- `ufvk` (string, required): Unified Full Viewing Key.
- `birthday` (u32, required): Block height at which to start scanning.
- `name` (string, optional): Human-readable label.

**Response (201):**
```json
{
  "id": "rn3rx7pmhk5yd22v8zxuz2wdk07q3c90yem3-2400000",
  "network": "main",
  "birthday": 2400000,
  "name": "my-wallet"
}
```

### GET /ufvks

List registered wallets.

**Query parameters:**
| Param | Default | Description |
|-------|---------|-------------|
| `page` | 1 | Page number |
| `per_page` | 20 | Results per page |
| `sort` | `desc` | `asc` or `desc` |

**Response:**
```json
{
  "wallets": [
    {
      "id": "...",
      "name": "my-wallet",
      "network": "main",
      "birthday": 2400000,
      "created_at": "2025-01-01T00:00:00Z",
      "sync_status": { "status": "syncing", "..." : "..." }
    }
  ],
  "page": 1,
  "per_page": 20,
  "total": 1
}
```

### POST /ufvks/lookup

Look up wallet IDs by UFVK. Useful when you have a UFVK but don't know the wallet ID. The same UFVK may be registered with different birthdays, so multiple wallets can be returned.

**Request body:**
```json
{
  "ufvk": "uview1..."
}
```

**Response (200):**
```json
{
  "wallets": [
    {
      "id": "rn3rx7pmhk5yd22v8zxuz2wdk07q3c90yem3-2400000",
      "network": "main",
      "birthday": 2400000
    }
  ]
}
```

**Response (404):** No wallets found for this UFVK.

### GET /ufvks/{id}

Get wallet details including the full UFVK.

**Response:**
```json
{
  "id": "...",
  "ufvk": "uview1...",
  "name": "my-wallet",
  "network": "main",
  "birthday": 2400000,
  "created_at": "2025-01-01T00:00:00Z",
  "sync_status": null
}
```

### DELETE /ufvks/{id}

Remove a wallet.

**Response:**
```json
{
  "id": "...",
  "deleted": true
}
```

## Balance

### GET /ufvks/{id}/balance

Get wallet balance in zatoshis (1 ZEC = 100,000,000 zatoshis).

**Response:**
```json
{
  "id": "...",
  "chain_tip_height": 2500000,
  "scan_progress": 0.95,
  "total": 150000000,
  "sapling_spendable": 100000000,
  "orchard_spendable": 50000000,
  "unshielded_spendable": 0
}
```

## Transactions

### GET /ufvks/{id}/transactions

List transactions for a wallet.

**Query parameters:**
| Param | Default | Description |
|-------|---------|-------------|
| `page` | 1 | Page number |
| `per_page` | 20 | Results per page |
| `sort` | `desc` | `asc` or `desc` |
| `confirmed` | (all) | `true` = 10+ confirmations, `false` = unconfirmed only |

**Response:**
```json
{
  "transactions": [
    {
      "txid": "abcd1234...",
      "mined_height": 2450000,
      "block_time": 1700000000,
      "confirmations": 50000,
      "account_balance_delta": 50000000,
      "fee_paid": null,
      "sent_note_count": 0,
      "received_note_count": 1,
      "memo_count": 1,
      "expired_unmined": false,
      "outputs": [
        {
          "pool": "orchard",
          "output_index": 0,
          "to_address": "u1...",
          "value": 50000000,
          "is_change": false,
          "memo": "Hello",
          "diversifier_index": 0
        }
      ]
    }
  ],
  "page": 1,
  "per_page": 20,
  "total": 5
}
```

### GET /ufvks/{id}/transactions/{txid}

Get a single transaction by its 64-character hex TXID.

**Response:** Same shape as a single entry in the transactions list above.

## Addresses

### POST /ufvks/{id}/address

Generate a new diversified address for a wallet.

**Request body:**
```json
{
  "diversifier_index": null,
  "receivers": "orchard"
}
```

- `diversifier_index` (u128, optional): Explicit index. Auto-increments if omitted.
- `receivers` (string, optional): One of `"orchard"`, `"sapling"`, `"shielded"`, `"all"`. Default: `"orchard"`.

**Response:**
```json
{
  "id": "...",
  "address": "u1...",
  "diversifier_index": 0
}
```

### POST /address/resolve

Find which wallet owns a given address.

**Request body:**
```json
{
  "address": "u1...",
  "wallet_ids": ["wallet-id-1", "wallet-id-2"]
}
```

**Response:**
```json
{
  "address": "u1...",
  "wallet_id": "wallet-id-1",
  "diversifier_index": 0,
  "matched_pools": ["orchard"]
}
```

### POST /address/balance

Get the balance received at a specific address. The address must be an Orchard-only UA.

**Request body:**
```json
{
  "address": "u1...",
  "wallet_ids": ["wallet-id-1"]
}
```

**Response:**
```json
{
  "address": "u1...",
  "wallet_id": "wallet-id-1",
  "diversifier_index": 0,
  "balance": 50000000,
  "total_received": 100000000,
  "last_received_height": 2450000
}
```

## Sync Status

### GET /sync/status

Overview of sync state for all wallets.

**Query parameters:** Same pagination as `GET /ufvks`.

**Response:**
```json
{
  "wallets": [
    {
      "id": "...",
      "name": "my-wallet",
      "network": "main",
      "status": "syncing",
      "last_synced_height": 2490000,
      "chain_tip_height": 2500000,
      "sync_progress": 0.95,
      "error_message": null,
      "last_sync_at": "2025-01-01T12:00:00Z"
    }
  ],
  "page": 1,
  "per_page": 20,
  "total": 1
}
```

### GET /ufvks/{id}/sync

Get sync status for a single wallet.

**Response:**
```json
{
  "status": "syncing",
  "last_synced_height": 2490000,
  "chain_tip_height": 2500000,
  "sync_progress": 0.95,
  "error_message": null,
  "last_sync_at": "2025-01-01T12:00:00Z"
}
```

## Notes

- **Values are in zatoshis.** 1 ZEC = 100,000,000 zatoshis.
- **Confirmation threshold:** A transaction is "confirmed" at 10+ confirmations.
- **Sync is automatic.** Registering a UFVK starts background sync from the birthday height.
- **SQLite locking.** Read endpoints retry internally with backoff when the database is locked by an active sync.
