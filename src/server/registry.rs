use rusqlite::{params, Connection, OptionalExtension};

use super::types::{SyncStatusEntry, UfvkSummary};

/// Manages the registry database that maps UFVKs to wallet directories and tracks sync state.
pub(crate) struct WalletRegistry {
    conn: Connection,
}

/// A wallet with its sync status, as returned by `all_sync_statuses`.
#[derive(Debug, Clone)]
pub(crate) struct WalletSyncInfo {
    pub id: String,
    pub name: Option<String>,
    pub network: String,
    pub sync: SyncStatusEntry,
}

/// A row from the watched_wallets table.
#[derive(Debug, Clone)]
pub(crate) struct WatchedWallet {
    pub id: String,
    pub ufvk: String,
    pub name: Option<String>,
    pub network: String,
    pub birthday: u32,
    pub wallet_dir: String,
    pub created_at: String,
}

impl WalletRegistry {
    /// Open (or create) the registry database at the given path.
    pub fn open(db_path: &str) -> Result<Self, anyhow::Error> {
        let conn = Connection::open(db_path)?;
        conn.execute_batch("PRAGMA journal_mode=WAL;")?;
        let registry = Self { conn };
        registry.migrate()?;
        Ok(registry)
    }

    fn migrate(&self) -> Result<(), anyhow::Error> {
        self.conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS watched_wallets (
                id          TEXT PRIMARY KEY,
                ufvk        TEXT NOT NULL UNIQUE,
                ufvk_hash   TEXT NOT NULL UNIQUE,
                name        TEXT,
                network     TEXT NOT NULL,
                birthday    INTEGER NOT NULL,
                wallet_dir  TEXT NOT NULL UNIQUE,
                created_at  TEXT NOT NULL DEFAULT (datetime('now')),
                deleted_at  TEXT
            );

            CREATE TABLE IF NOT EXISTS sync_state (
                wallet_id           TEXT PRIMARY KEY REFERENCES watched_wallets(id),
                status              TEXT NOT NULL DEFAULT 'pending',
                last_synced_height  INTEGER,
                chain_tip_height    INTEGER,
                error_message       TEXT,
                last_sync_at        TEXT,
                sync_progress       REAL
            );

            CREATE UNIQUE INDEX IF NOT EXISTS idx_ufvk_birthday
                ON watched_wallets(ufvk_hash, birthday) WHERE deleted_at IS NULL;",
        )?;
        Ok(())
    }

    /// Insert a new watched wallet. Returns Err if the UFVK already exists.
    #[allow(clippy::too_many_arguments)]
    pub fn insert_wallet(
        &self,
        id: &str,
        ufvk: &str,
        ufvk_hash: &str,
        name: Option<&str>,
        network: &str,
        birthday: u32,
        wallet_dir: &str,
    ) -> Result<(), anyhow::Error> {
        self.conn.execute(
            "INSERT INTO watched_wallets (id, ufvk, ufvk_hash, name, network, birthday, wallet_dir)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![id, ufvk, ufvk_hash, name, network, birthday, wallet_dir],
        )?;
        self.conn.execute(
            "INSERT INTO sync_state (wallet_id) VALUES (?1)",
            params![id],
        )?;
        Ok(())
    }

    /// Get a single wallet by ID.
    pub fn get_wallet(&self, id: &str) -> Result<Option<WatchedWallet>, anyhow::Error> {
        let row = self
            .conn
            .query_row(
                "SELECT id, ufvk, name, network, birthday, wallet_dir, created_at
                 FROM watched_wallets WHERE id = ?1 AND deleted_at IS NULL",
                params![id],
                |row| {
                    Ok(WatchedWallet {
                        id: row.get(0)?,
                        ufvk: row.get(1)?,
                        name: row.get(2)?,
                        network: row.get(3)?,
                        birthday: row.get(4)?,
                        wallet_dir: row.get(5)?,
                        created_at: row.get(6)?,
                    })
                },
            )
            .optional()?;
        Ok(row)
    }

    /// List all non-deleted wallets with pagination.
    pub fn list_wallets(
        &self,
        page: u64,
        per_page: u64,
    ) -> Result<(Vec<UfvkSummary>, u64), anyhow::Error> {
        let total: u64 = self.conn.query_row(
            "SELECT COUNT(*) FROM watched_wallets WHERE deleted_at IS NULL",
            [],
            |row| row.get(0),
        )?;

        let offset = (page.saturating_sub(1)) * per_page;
        let mut stmt = self.conn.prepare(
            "SELECT w.id, w.name, w.network, w.birthday, w.created_at,
                    s.status, s.last_synced_height, s.chain_tip_height,
                    s.sync_progress, s.error_message, s.last_sync_at
             FROM watched_wallets w
             LEFT JOIN sync_state s ON w.id = s.wallet_id
             WHERE w.deleted_at IS NULL
             ORDER BY w.created_at DESC
             LIMIT ?1 OFFSET ?2",
        )?;

        let rows = stmt
            .query_map(params![per_page, offset], |row| {
                Ok(UfvkSummary {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    network: row.get(2)?,
                    birthday: row.get(3)?,
                    created_at: row.get(4)?,
                    sync_status: row.get::<_, Option<String>>(5)?.map(|status| {
                        SyncStatusEntry {
                            status,
                            last_synced_height: row.get(6).ok().flatten(),
                            chain_tip_height: row.get(7).ok().flatten(),
                            sync_progress: row.get(8).ok().flatten(),
                            error_message: row.get(9).ok().flatten(),
                            last_sync_at: row.get(10).ok().flatten(),
                        }
                    }),
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;

        Ok((rows, total))
    }

    /// Get sync state for a wallet.
    pub fn get_sync_state(
        &self,
        wallet_id: &str,
    ) -> Result<Option<SyncStatusEntry>, anyhow::Error> {
        let entry = self
            .conn
            .query_row(
                "SELECT status, last_synced_height, chain_tip_height,
                        sync_progress, error_message, last_sync_at
                 FROM sync_state WHERE wallet_id = ?1",
                params![wallet_id],
                |row| {
                    Ok(SyncStatusEntry {
                        status: row.get(0)?,
                        last_synced_height: row.get(1)?,
                        chain_tip_height: row.get(2)?,
                        sync_progress: row.get(3)?,
                        error_message: row.get(4)?,
                        last_sync_at: row.get(5)?,
                    })
                },
            )
            .optional()?;
        Ok(entry)
    }

    /// Update sync state for a wallet.
    pub fn update_sync_state(
        &self,
        wallet_id: &str,
        status: &str,
        last_synced_height: Option<u32>,
        chain_tip_height: Option<u32>,
        sync_progress: Option<f64>,
        error_message: Option<&str>,
    ) -> Result<(), anyhow::Error> {
        self.conn.execute(
            "UPDATE sync_state SET
                status = ?2,
                last_synced_height = COALESCE(?3, last_synced_height),
                chain_tip_height = COALESCE(?4, chain_tip_height),
                sync_progress = COALESCE(?5, sync_progress),
                error_message = ?6,
                last_sync_at = datetime('now')
             WHERE wallet_id = ?1",
            params![
                wallet_id,
                status,
                last_synced_height,
                chain_tip_height,
                sync_progress,
                error_message,
            ],
        )?;
        Ok(())
    }

    /// Soft-delete a wallet.
    pub fn soft_delete(&self, id: &str) -> Result<bool, anyhow::Error> {
        let affected = self.conn.execute(
            "UPDATE watched_wallets SET deleted_at = datetime('now') WHERE id = ?1 AND deleted_at IS NULL",
            params![id],
        )?;
        Ok(affected > 0)
    }

    /// List all active (non-deleted) wallet IDs.
    pub fn list_active_wallet_ids(&self) -> Result<Vec<String>, anyhow::Error> {
        let mut stmt = self
            .conn
            .prepare("SELECT id FROM watched_wallets WHERE deleted_at IS NULL")?;
        let ids = stmt
            .query_map([], |row| row.get(0))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(ids)
    }

    /// Count wallets by network.
    pub fn count_by_network(&self) -> Result<(u64, u64), anyhow::Error> {
        let mainnet: u64 = self.conn.query_row(
            "SELECT COUNT(*) FROM watched_wallets WHERE network = 'main' AND deleted_at IS NULL",
            [],
            |row| row.get(0),
        )?;
        let testnet: u64 = self.conn.query_row(
            "SELECT COUNT(*) FROM watched_wallets WHERE network = 'test' AND deleted_at IS NULL",
            [],
            |row| row.get(0),
        )?;
        Ok((mainnet, testnet))
    }

    /// Get all sync statuses with pagination for the overview endpoint.
    pub fn all_sync_statuses_paginated(
        &self,
        page: u64,
        per_page: u64,
    ) -> Result<(Vec<WalletSyncInfo>, u64), anyhow::Error> {
        let total: u64 = self.conn.query_row(
            "SELECT COUNT(*) FROM watched_wallets w
             JOIN sync_state s ON w.id = s.wallet_id
             WHERE w.deleted_at IS NULL",
            [],
            |row| row.get(0),
        )?;

        let offset = (page.saturating_sub(1)) * per_page;
        let mut stmt = self.conn.prepare(
            "SELECT w.id, w.name, w.network,
                    s.status, s.last_synced_height, s.chain_tip_height,
                    s.sync_progress, s.error_message, s.last_sync_at
             FROM watched_wallets w
             JOIN sync_state s ON w.id = s.wallet_id
             WHERE w.deleted_at IS NULL
             ORDER BY w.created_at DESC
             LIMIT ?1 OFFSET ?2",
        )?;

        let rows = stmt
            .query_map(params![per_page, offset], |row| {
                Ok(WalletSyncInfo {
                    id: row.get(0)?,
                    name: row.get::<_, Option<String>>(1)?,
                    network: row.get::<_, String>(2)?,
                    sync: SyncStatusEntry {
                        status: row.get(3)?,
                        last_synced_height: row.get(4)?,
                        chain_tip_height: row.get(5)?,
                        sync_progress: row.get(6)?,
                        error_message: row.get(7)?,
                        last_sync_at: row.get(8)?,
                    },
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;

        Ok((rows, total))
    }
}
