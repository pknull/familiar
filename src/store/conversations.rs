//! Conversation history — stored locally, never published to the feed.

use rusqlite::{params, OptionalExtension};

use crate::error::Result;
use crate::store::Store;

/// A conversation turn.
#[derive(Debug, Clone)]
pub struct Turn {
    pub id: i64,
    pub role: String,
    pub content: String,
    pub tool_calls: Option<String>,
    pub created_at: String,
}

impl Store {
    /// Record a conversation turn.
    pub fn add_turn(&self, role: &str, content: &str, tool_calls: Option<&str>) -> Result<i64> {
        self.conn().execute(
            "INSERT INTO conversations (role, content, tool_calls) VALUES (?1, ?2, ?3)",
            params![role, content, tool_calls],
        )?;
        Ok(self.conn().last_insert_rowid())
    }

    /// Get recent conversation turns (most recent last).
    pub fn recent_turns(&self, limit: usize) -> Result<Vec<Turn>> {
        let mut stmt = self.conn().prepare(
            "SELECT id, role, content, tool_calls, created_at
             FROM conversations
             ORDER BY id DESC
             LIMIT ?1",
        )?;

        let turns: Vec<Turn> = stmt
            .query_map(params![limit as i64], |row| {
                Ok(Turn {
                    id: row.get(0)?,
                    role: row.get(1)?,
                    content: row.get(2)?,
                    tool_calls: row.get(3)?,
                    created_at: row.get(4)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        // Reverse so oldest is first (natural conversation order)
        let mut turns = turns;
        turns.reverse();
        Ok(turns)
    }

    /// Count total conversation turns.
    pub fn turn_count(&self) -> Result<i64> {
        let count = self
            .conn()
            .query_row("SELECT COUNT(*) FROM conversations", [], |row| row.get(0))?;
        Ok(count)
    }

    /// Delete all conversation turns with id less than the given id.
    pub fn delete_turns_before(&self, id: i64) -> Result<()> {
        self.conn()
            .execute("DELETE FROM conversations WHERE id < ?1", params![id])?;
        Ok(())
    }

    /// Get the oldest N conversation turns (oldest first).
    pub fn oldest_turns(&self, limit: usize) -> Result<Vec<Turn>> {
        let mut stmt = self.conn().prepare(
            "SELECT id, role, content, tool_calls, created_at
             FROM conversations
             ORDER BY id ASC
             LIMIT ?1",
        )?;

        let turns = stmt
            .query_map(params![limit as i64], |row| {
                Ok(Turn {
                    id: row.get(0)?,
                    role: row.get(1)?,
                    content: row.get(2)?,
                    tool_calls: row.get(3)?,
                    created_at: row.get(4)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        Ok(turns)
    }

    /// Log a published message (for local reference).
    pub fn log_published(
        &self,
        hash: &str,
        content_type: &str,
        summary: Option<&str>,
        metadata: Option<&serde_json::Value>,
    ) -> Result<()> {
        let metadata_json = metadata
            .map(serde_json::to_string)
            .transpose()
            .map_err(crate::error::FamiliarError::from)?;
        self.conn().execute(
            "INSERT INTO published (hash, content_type, summary, metadata_json) VALUES (?1, ?2, ?3, ?4)",
            params![hash, content_type, summary, metadata_json],
        )?;
        Ok(())
    }

    /// Check if we published a message with the given hash.
    pub fn has_published_hash(&self, hash: &str) -> Result<bool> {
        let count: i64 = self.conn().query_row(
            "SELECT COUNT(*) FROM published WHERE hash = ?1",
            params![hash],
            |row| row.get(0),
        )?;
        Ok(count > 0)
    }

    /// Retrieve locally stored metadata for a published message hash.
    pub fn published_metadata(&self, hash: &str) -> Result<Option<serde_json::Value>> {
        let metadata_json: Option<String> = self
            .conn()
            .query_row(
                "SELECT metadata_json FROM published WHERE hash = ?1 ORDER BY id DESC LIMIT 1",
                params![hash],
                |row| row.get(0),
            )
            .optional()?;

        metadata_json
            .map(|json| serde_json::from_str(&json).map_err(crate::error::FamiliarError::from))
            .transpose()
    }
}

#[cfg(test)]
mod tests {
    use crate::store::Store;

    #[test]
    fn add_and_retrieve_turns() {
        let store = Store::in_memory().unwrap();

        store.add_turn("user", "Hello", None).unwrap();
        store.add_turn("assistant", "Hi there!", None).unwrap();
        store.add_turn("user", "What's up?", None).unwrap();

        let turns = store.recent_turns(10).unwrap();
        assert_eq!(turns.len(), 3);
        assert_eq!(turns[0].role, "user");
        assert_eq!(turns[0].content, "Hello");
        assert_eq!(turns[2].content, "What's up?");
    }

    #[test]
    fn recent_turns_respects_limit() {
        let store = Store::in_memory().unwrap();

        for i in 0..10 {
            store
                .add_turn("user", &format!("Message {}", i), None)
                .unwrap();
        }

        let turns = store.recent_turns(3).unwrap();
        assert_eq!(turns.len(), 3);
        // Should be the 3 most recent, in chronological order
        assert_eq!(turns[0].content, "Message 7");
        assert_eq!(turns[2].content, "Message 9");
    }

    #[test]
    fn log_published() {
        let store = Store::in_memory().unwrap();
        store
            .log_published("abc123", "insight", Some("Bloom filter bug"), None)
            .unwrap();

        let count: i64 = store
            .conn()
            .query_row("SELECT COUNT(*) FROM published", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn has_published_hash() {
        let store = Store::in_memory().unwrap();

        assert!(!store.has_published_hash("abc123").unwrap());

        store
            .log_published("abc123", "task", Some("Test task"), None)
            .unwrap();

        assert!(store.has_published_hash("abc123").unwrap());
        assert!(!store.has_published_hash("xyz789").unwrap());
    }

    #[test]
    fn published_metadata_round_trips() {
        let store = Store::in_memory().unwrap();
        let metadata = serde_json::json!({
            "type": "task",
            "hash": "task-123",
            "context": {
                "planner_basis": {
                    "target_id": "staging-web"
                }
            }
        });

        store
            .log_published("task-123", "task", Some("Deploy"), Some(&metadata))
            .unwrap();

        let stored = store.published_metadata("task-123").unwrap().unwrap();
        assert_eq!(
            stored["context"]["planner_basis"]["target_id"],
            "staging-web"
        );
    }
}
