//! Local SQLite store — conversations, personal context, and publish log.
//!
//! This data never leaves the machine. The egregore feed is for network-facing
//! actions; this store is for everything private.

pub mod context;
pub mod conversations;
pub mod sessions;
pub mod snapshots;
pub mod usage;

use std::path::Path;

use rand::RngCore;
use rusqlite::Connection;
use tracing::warn;

use crate::error::{FamiliarError, Result};

/// Key file path: ~/.familiar/store.key
fn key_file_path() -> Option<std::path::PathBuf> {
    dirs::home_dir().map(|h| h.join(".familiar").join("store.key"))
}

/// Retrieve an existing encryption key from file, or generate and
/// store a new 32-byte random key on first use. Returns `None` when
/// the key file directory is unavailable.
fn get_or_create_key() -> Option<String> {
    let key_path = key_file_path()?;

    // Try to read existing key.
    if key_path.exists() {
        match std::fs::read_to_string(&key_path) {
            Ok(existing) => {
                let trimmed = existing.trim().to_string();
                if is_valid_hex_key(&trimmed) {
                    return Some(trimmed);
                }
                warn!("store.key contains invalid data, regenerating");
            }
            Err(err) => {
                warn!("failed to read store.key: {err}");
                return None;
            }
        }
    }

    // Generate a new 32-byte random key.
    let mut raw = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut raw);
    let hex_key = hex::encode(raw);

    // Write with restrictive permissions (set mode before content to avoid TOCTOU).
    #[cfg(unix)]
    {
        use std::fs::OpenOptions;
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;

        match OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&key_path)
        {
            Ok(mut f) => {
                if let Err(err) = f.write_all(hex_key.as_bytes()) {
                    warn!("failed to write store.key: {err}");
                    return None;
                }
            }
            Err(err) => {
                warn!("failed to create store.key: {err}");
                return None;
            }
        }
    }
    #[cfg(not(unix))]
    {
        if let Err(err) = std::fs::write(&key_path, &hex_key) {
            warn!("failed to write store.key: {err}");
            return None;
        }
    }

    Some(hex_key)
}

/// Validate that a key string is exactly 64 hex characters (256-bit key).
fn is_valid_hex_key(s: &str) -> bool {
    s.len() == 64 && s.chars().all(|c| c.is_ascii_hexdigit())
}

/// Local store backed by SQLite.
pub struct Store {
    conn: Connection,
}

impl Store {
    /// Open or create the store at the given path.
    ///
    /// When the OS keyring is available the database is encrypted with
    /// SQLCipher. On headless / CI environments where no keyring exists the
    /// store falls back to an unencrypted database with a warning.
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let conn = Connection::open(path).map_err(|e| FamiliarError::Store {
            reason: format!("failed to open database: {}", e),
        })?;

        // Apply SQLCipher encryption key when available.
        if let Some(hex_key) = get_or_create_key() {
            if !is_valid_hex_key(&hex_key) {
                warn!("keyring returned invalid hex key (len={}, expected 64 hex chars), database will be unencrypted", hex_key.len());
            } else {
                conn.execute_batch(&format!("PRAGMA key = \"x'{}'\";", hex_key))
                    .map_err(|e| FamiliarError::Store {
                        reason: format!("failed to set encryption key: {}", e),
                    })?;

                // Verify the key is correct by reading from the DB
                conn.execute_batch("SELECT count(*) FROM sqlite_master;")
                    .map_err(|e| FamiliarError::Store {
                        reason: format!("encryption key mismatch or corrupt database: {}", e),
                    })?;
            }
        }

        let store = Self { conn };
        store.migrate()?;
        Ok(store)
    }

    /// Open an in-memory store (for testing).
    #[cfg(test)]
    pub fn in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        let store = Self { conn };
        store.migrate()?;
        Ok(store)
    }

    /// Open an unencrypted store at a path (for testing — bypasses SQLCipher).
    pub fn open_unencrypted(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path).map_err(|e| FamiliarError::Store {
            reason: format!("failed to open database: {}", e),
        })?;
        let store = Self { conn };
        store.migrate()?;
        Ok(store)
    }

    /// Run schema migrations.
    fn migrate(&self) -> Result<()> {
        self.conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS conversations (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                role TEXT NOT NULL,
                content TEXT NOT NULL,
                tool_calls TEXT,
                created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
            );

            CREATE TABLE IF NOT EXISTS context (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL,
                updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
            );

            CREATE TABLE IF NOT EXISTS published (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                hash TEXT NOT NULL,
                content_type TEXT NOT NULL,
                summary TEXT,
                metadata_json TEXT,
                published_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
            );

            CREATE INDEX IF NOT EXISTS idx_conversations_created
                ON conversations(created_at);
            CREATE INDEX IF NOT EXISTS idx_published_type
                ON published(content_type);

            CREATE TABLE IF NOT EXISTS sessions (
                id TEXT PRIMARY KEY,
                slug TEXT NOT NULL,
                created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
                updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
                active_thread_id TEXT
            );

            CREATE TABLE IF NOT EXISTS threads (
                id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                channel TEXT NOT NULL,
                external_id TEXT,
                created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
            );

            CREATE INDEX IF NOT EXISTS idx_threads_session
                ON threads(session_id);

            CREATE TABLE IF NOT EXISTS snapshots (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                file_path TEXT NOT NULL,
                content_hash TEXT NOT NULL,
                created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
            );

            CREATE INDEX IF NOT EXISTS idx_snapshots_session
                ON snapshots(session_id);

            CREATE TABLE IF NOT EXISTS usage (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                model TEXT NOT NULL,
                input_tokens INTEGER NOT NULL DEFAULT 0,
                output_tokens INTEGER NOT NULL DEFAULT 0,
                cache_read_tokens INTEGER NOT NULL DEFAULT 0,
                cache_write_tokens INTEGER NOT NULL DEFAULT 0,
                reasoning_tokens INTEGER NOT NULL DEFAULT 0,
                estimated_usd REAL NOT NULL DEFAULT 0.0,
                created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
            );

            CREATE INDEX IF NOT EXISTS idx_usage_created
                ON usage(created_at);
            ",
        )?;

        // Add thread_id column to conversations if not present (migration for existing DBs)
        let has_thread_id: bool = {
            let mut stmt = self.conn.prepare("PRAGMA table_info(conversations)")?;
            let columns: Vec<String> = stmt
                .query_map([], |row| row.get::<_, String>(1))?
                .filter_map(|r| r.ok())
                .collect();
            columns.iter().any(|c| c == "thread_id")
        };
        if !has_thread_id {
            self.conn
                .execute_batch("ALTER TABLE conversations ADD COLUMN thread_id TEXT;")?;
        }

        let has_metadata_json: bool = {
            let mut stmt = self.conn.prepare("PRAGMA table_info(published)")?;
            let columns: Vec<String> = stmt
                .query_map([], |row| row.get::<_, String>(1))?
                .filter_map(|r| r.ok())
                .collect();
            columns.iter().any(|c| c == "metadata_json")
        };
        if !has_metadata_json {
            self.conn
                .execute_batch("ALTER TABLE published ADD COLUMN metadata_json TEXT;")?;
        }

        Ok(())
    }

    /// Get a reference to the connection (for submodules).
    pub(crate) fn conn(&self) -> &Connection {
        &self.conn
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_in_memory() {
        let store = Store::in_memory().unwrap();
        // Tables should exist
        let count: i64 = store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='conversations'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
    }
}
