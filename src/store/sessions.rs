//! Session persistence — multi-thread sessions with fork and snapshot support.
//!
//! Each session contains one or more threads (conversations). Sessions are
//! identified by UUID + human-readable slug. Threads are keyed by
//! (user + channel + external_thread_id) to prevent duplicates.

use rusqlite::params;

use crate::error::{FamiliarError, Result};
use crate::store::Store;

/// Session metadata.
#[derive(Debug, Clone)]
pub struct Session {
    pub id: String,
    pub slug: String,
    pub created_at: String,
    pub updated_at: String,
    pub active_thread_id: Option<String>,
}

/// Thread within a session.
#[derive(Debug, Clone)]
pub struct Thread {
    pub id: String,
    pub session_id: String,
    pub channel: String,
    pub external_id: Option<String>,
    pub created_at: String,
}

impl Store {
    /// Create a new session with a generated UUID and slug.
    pub fn create_session(&self, slug: &str) -> Result<String> {
        let id = uuid_v4();
        self.conn().execute(
            "INSERT INTO sessions (id, slug) VALUES (?1, ?2)",
            params![id, slug],
        )?;
        Ok(id)
    }

    /// Get a session by ID.
    pub fn get_session(&self, session_id: &str) -> Result<Option<Session>> {
        let mut stmt = self.conn().prepare(
            "SELECT id, slug, created_at, updated_at, active_thread_id FROM sessions WHERE id = ?1",
        )?;

        let session = stmt
            .query_row(params![session_id], |row| {
                Ok(Session {
                    id: row.get(0)?,
                    slug: row.get(1)?,
                    created_at: row.get(2)?,
                    updated_at: row.get(3)?,
                    active_thread_id: row.get(4)?,
                })
            })
            .optional()?;

        Ok(session)
    }

    /// List all sessions, most recent first.
    pub fn list_sessions(&self) -> Result<Vec<Session>> {
        let mut stmt = self.conn().prepare(
            "SELECT id, slug, created_at, updated_at, active_thread_id FROM sessions ORDER BY updated_at DESC",
        )?;

        let sessions = stmt
            .query_map([], |row| {
                Ok(Session {
                    id: row.get(0)?,
                    slug: row.get(1)?,
                    created_at: row.get(2)?,
                    updated_at: row.get(3)?,
                    active_thread_id: row.get(4)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        Ok(sessions)
    }

    /// Touch session updated_at timestamp.
    pub fn touch_session(&self, session_id: &str) -> Result<()> {
        self.conn().execute(
            "UPDATE sessions SET updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now') WHERE id = ?1",
            params![session_id],
        )?;
        Ok(())
    }

    /// Set the active thread for a session.
    pub fn set_active_thread(&self, session_id: &str, thread_id: &str) -> Result<()> {
        self.conn().execute(
            "UPDATE sessions SET active_thread_id = ?2 WHERE id = ?1",
            params![session_id, thread_id],
        )?;
        Ok(())
    }

    /// Create a thread within a session.
    pub fn create_thread(
        &self,
        session_id: &str,
        channel: &str,
        external_id: Option<&str>,
    ) -> Result<String> {
        let id = uuid_v4();
        self.conn().execute(
            "INSERT INTO threads (id, session_id, channel, external_id) VALUES (?1, ?2, ?3, ?4)",
            params![id, session_id, channel, external_id],
        )?;
        Ok(id)
    }

    /// Resolve a thread by (session + channel + external_id), creating if not found.
    pub fn resolve_thread(
        &self,
        session_id: &str,
        channel: &str,
        external_id: Option<&str>,
    ) -> Result<String> {
        // Try to find existing thread
        let existing: Option<String> = if let Some(ext_id) = external_id {
            self.conn()
                .query_row(
                    "SELECT id FROM threads WHERE session_id = ?1 AND channel = ?2 AND external_id = ?3",
                    params![session_id, channel, ext_id],
                    |row| row.get(0),
                )
                .optional()?
        } else {
            self.conn()
                .query_row(
                    "SELECT id FROM threads WHERE session_id = ?1 AND channel = ?2 AND external_id IS NULL",
                    params![session_id, channel],
                    |row| row.get(0),
                )
                .optional()?
        };

        match existing {
            Some(id) => Ok(id),
            None => self.create_thread(session_id, channel, external_id),
        }
    }

    /// Add a turn to a specific thread (session-aware version of add_turn).
    pub fn add_session_turn(
        &self,
        thread_id: &str,
        role: &str,
        content: &str,
        tool_calls: Option<&str>,
    ) -> Result<()> {
        self.conn().execute(
            "INSERT INTO conversations (thread_id, role, content, tool_calls) VALUES (?1, ?2, ?3, ?4)",
            params![thread_id, role, content, tool_calls],
        )?;
        Ok(())
    }

    /// Get recent turns for a specific thread.
    pub fn thread_recent_turns(
        &self,
        thread_id: &str,
        limit: usize,
    ) -> Result<Vec<(String, String, Option<String>)>> {
        let mut stmt = self.conn().prepare(
            "SELECT role, content, tool_calls FROM conversations WHERE thread_id = ?1 ORDER BY id DESC LIMIT ?2",
        )?;

        let turns: Vec<_> = stmt
            .query_map(params![thread_id, limit as i64], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        // Reverse to get chronological order
        Ok(turns.into_iter().rev().collect())
    }

    /// Fork a session: clone all turns up to a given message ID into a new session.
    pub fn fork_session(
        &self,
        source_session_id: &str,
        up_to_turn_id: i64,
        new_slug: &str,
    ) -> Result<String> {
        let new_session_id = self.create_session(new_slug)?;

        // Get threads from source session
        let mut stmt = self
            .conn()
            .prepare("SELECT id, channel, external_id FROM threads WHERE session_id = ?1")?;

        let threads: Vec<(String, String, Option<String>)> = stmt
            .query_map(params![source_session_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        // Clone threads and their turns
        for (old_thread_id, channel, external_id) in threads {
            let new_thread_id =
                self.create_thread(&new_session_id, &channel, external_id.as_deref())?;

            // Copy turns up to the specified ID
            self.conn().execute(
                "INSERT INTO conversations (thread_id, role, content, tool_calls, created_at)
                 SELECT ?1, role, content, tool_calls, created_at
                 FROM conversations
                 WHERE thread_id = ?2 AND id <= ?3
                 ORDER BY id",
                params![new_thread_id, old_thread_id, up_to_turn_id],
            )?;
        }

        Ok(new_session_id)
    }

    /// Delete sessions idle longer than the given duration.
    pub fn prune_idle_sessions(&self, max_idle_secs: i64) -> Result<usize> {
        let deleted = self.conn().execute(
            "DELETE FROM sessions WHERE id IN (
                SELECT id FROM sessions
                WHERE julianday('now') - julianday(updated_at) > ?1 / 86400.0
            )",
            params![max_idle_secs],
        )?;
        Ok(deleted)
    }

    /// Record a file snapshot entry.
    pub fn record_snapshot(
        &self,
        session_id: &str,
        file_path: &str,
        content_hash: &str,
    ) -> Result<()> {
        self.conn().execute(
            "INSERT INTO snapshots (session_id, file_path, content_hash) VALUES (?1, ?2, ?3)",
            params![session_id, file_path, content_hash],
        )?;
        Ok(())
    }

    /// Get snapshots for a session, ordered by time.
    pub fn get_snapshots(&self, session_id: &str) -> Result<Vec<(i64, String, String, String)>> {
        let mut stmt = self.conn().prepare(
            "SELECT id, file_path, content_hash, created_at FROM snapshots WHERE session_id = ?1 ORDER BY id",
        )?;

        let snaps = stmt
            .query_map(params![session_id], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        Ok(snaps)
    }

    /// Get all snapshot content hashes referenced by any session (for orphan sweep).
    pub fn all_snapshot_hashes(&self) -> Result<std::collections::HashSet<String>> {
        let mut stmt = self
            .conn()
            .prepare("SELECT DISTINCT content_hash FROM snapshots")?;

        let hashes: std::collections::HashSet<String> = stmt
            .query_map([], |row| row.get::<_, String>(0))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(hashes)
    }
}

/// Generate a UUID v4 (random) using the rand crate.
fn uuid_v4() -> String {
    use rand::RngCore;
    use std::fmt::Write;

    let mut bytes = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut bytes);
    bytes[6] = (bytes[6] & 0x0f) | 0x40; // version 4
    bytes[8] = (bytes[8] & 0x3f) | 0x80; // variant 1

    let mut s = String::with_capacity(36);
    for (i, byte) in bytes.iter().enumerate() {
        if i == 4 || i == 6 || i == 8 || i == 10 {
            s.push('-');
        }
        write!(s, "{:02x}", byte).unwrap();
    }
    s
}

/// Extension trait for optional query results.
trait OptionalExt<T> {
    fn optional(self) -> std::result::Result<Option<T>, rusqlite::Error>;
}

impl<T> OptionalExt<T> for std::result::Result<T, rusqlite::Error> {
    fn optional(self) -> std::result::Result<Option<T>, rusqlite::Error> {
        match self {
            Ok(v) => Ok(Some(v)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_and_list_sessions() {
        let store = Store::in_memory().unwrap();
        let id = store.create_session("test-session").unwrap();
        let sessions = store.list_sessions().unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, id);
        assert_eq!(sessions[0].slug, "test-session");
    }

    #[test]
    fn thread_resolution_creates_once() {
        let store = Store::in_memory().unwrap();
        let session_id = store.create_session("test").unwrap();

        let t1 = store.resolve_thread(&session_id, "repl", None).unwrap();
        let t2 = store.resolve_thread(&session_id, "repl", None).unwrap();
        assert_eq!(t1, t2); // Same thread returned

        let t3 = store
            .resolve_thread(&session_id, "discord", Some("guild-123"))
            .unwrap();
        assert_ne!(t1, t3); // Different channel = different thread
    }

    #[test]
    fn session_turns_persist() {
        let store = Store::in_memory().unwrap();
        let session_id = store.create_session("test").unwrap();
        let thread_id = store.resolve_thread(&session_id, "repl", None).unwrap();

        store
            .add_session_turn(&thread_id, "user", "hello", None)
            .unwrap();
        store
            .add_session_turn(&thread_id, "assistant", "hi there", None)
            .unwrap();

        let turns = store.thread_recent_turns(&thread_id, 10).unwrap();
        assert_eq!(turns.len(), 2);
        assert_eq!(turns[0].0, "user");
        assert_eq!(turns[1].0, "assistant");
    }

    #[test]
    fn fork_session_clones_history() {
        let store = Store::in_memory().unwrap();
        let session_id = store.create_session("original").unwrap();
        let thread_id = store.resolve_thread(&session_id, "repl", None).unwrap();

        store
            .add_session_turn(&thread_id, "user", "msg 1", None)
            .unwrap();
        store
            .add_session_turn(&thread_id, "assistant", "reply 1", None)
            .unwrap();
        store
            .add_session_turn(&thread_id, "user", "msg 2", None)
            .unwrap();

        // Fork after turn 2 (the assistant reply)
        let turns = store.thread_recent_turns(&thread_id, 10).unwrap();
        // Get the ID of the second turn — we need to query for it
        let turn_2_id: i64 = store
            .conn()
            .query_row(
                "SELECT id FROM conversations WHERE thread_id = ?1 ORDER BY id LIMIT 1 OFFSET 1",
                params![thread_id],
                |row| row.get(0),
            )
            .unwrap();

        let forked_id = store
            .fork_session(&session_id, turn_2_id, "forked")
            .unwrap();
        let forked_sessions = store.list_sessions().unwrap();
        assert_eq!(forked_sessions.len(), 2);

        // Forked session should have a thread with 2 turns (not 3)
        let forked_thread = store.resolve_thread(&forked_id, "repl", None).unwrap();
        let forked_turns = store.thread_recent_turns(&forked_thread, 10).unwrap();
        assert_eq!(forked_turns.len(), 2);
    }
}
