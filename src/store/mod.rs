//! Local SQLite store — conversations, personal context, and publish log.
//!
//! This data never leaves the machine. The egregore feed is for network-facing
//! actions; this store is for everything private.

pub mod conversations;
pub mod context;

use std::path::Path;

use rusqlite::Connection;

use crate::error::{FamiliarError, Result};

/// Local store backed by SQLite.
pub struct Store {
    conn: Connection,
}

impl Store {
    /// Open or create the store at the given path.
    pub fn open(path: &Path) -> Result<Self> {
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

    /// Open an in-memory store (for testing).
    #[cfg(test)]
    pub fn in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
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
                published_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
            );

            CREATE INDEX IF NOT EXISTS idx_conversations_created
                ON conversations(created_at);
            CREATE INDEX IF NOT EXISTS idx_published_type
                ON published(content_type);
            ",
        )?;
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
