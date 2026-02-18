use rusqlite::Connection;
use std::path::Path;
use std::time::Duration;

/// Open a wallet DB connection with WAL mode and busy_timeout.
/// WAL allows concurrent readers with a single writer.
pub(crate) fn open_wallet_connection(path: &Path) -> Result<Connection, rusqlite::Error> {
    let conn = Connection::open(path)?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.busy_timeout(Duration::from_secs(5))?;
    rusqlite::vtab::array::load_module(&conn)?;
    Ok(conn)
}
