//! Personal context — preferences, ongoing threads, local knowledge.
//! Stored locally, never published to the feed.

use rusqlite::params;

use crate::error::Result;
use crate::store::Store;

impl Store {
    /// Set a context value (upsert).
    pub fn set_context(&self, key: &str, value: &str) -> Result<()> {
        self.conn().execute(
            "INSERT INTO context (key, value, updated_at)
             VALUES (?1, ?2, strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
             ON CONFLICT(key) DO UPDATE SET
                value = excluded.value,
                updated_at = excluded.updated_at",
            params![key, value],
        )?;
        Ok(())
    }

    /// Get a context value.
    pub fn get_context(&self, key: &str) -> Result<Option<String>> {
        let result = self.conn().query_row(
            "SELECT value FROM context WHERE key = ?1",
            params![key],
            |row| row.get(0),
        );

        match result {
            Ok(value) => Ok(Some(value)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Delete a context value.
    pub fn delete_context(&self, key: &str) -> Result<bool> {
        let rows = self
            .conn()
            .execute("DELETE FROM context WHERE key = ?1", params![key])?;
        Ok(rows > 0)
    }

    /// List all context keys and values.
    pub fn list_context(&self) -> Result<Vec<(String, String)>> {
        let mut stmt = self
            .conn()
            .prepare("SELECT key, value FROM context ORDER BY key")?;

        let pairs: Vec<(String, String)> = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        Ok(pairs)
    }
}

#[cfg(test)]
mod tests {
    use crate::store::Store;

    #[test]
    fn set_and_get_context() {
        let store = Store::in_memory().unwrap();

        store.set_context("work_hours", "09:00-17:00").unwrap();
        let value = store.get_context("work_hours").unwrap();
        assert_eq!(value, Some("09:00-17:00".to_string()));
    }

    #[test]
    fn upsert_context() {
        let store = Store::in_memory().unwrap();

        store.set_context("timezone", "UTC").unwrap();
        store.set_context("timezone", "America/New_York").unwrap();

        let value = store.get_context("timezone").unwrap();
        assert_eq!(value, Some("America/New_York".to_string()));
    }

    #[test]
    fn get_missing_context() {
        let store = Store::in_memory().unwrap();
        let value = store.get_context("nonexistent").unwrap();
        assert_eq!(value, None);
    }

    #[test]
    fn delete_context() {
        let store = Store::in_memory().unwrap();

        store.set_context("temp", "value").unwrap();
        assert!(store.delete_context("temp").unwrap());
        assert!(!store.delete_context("temp").unwrap());
        assert_eq!(store.get_context("temp").unwrap(), None);
    }

    #[test]
    fn list_context() {
        let store = Store::in_memory().unwrap();

        store.set_context("b_key", "b_val").unwrap();
        store.set_context("a_key", "a_val").unwrap();

        let pairs = store.list_context().unwrap();
        assert_eq!(pairs.len(), 2);
        assert_eq!(pairs[0].0, "a_key"); // sorted
        assert_eq!(pairs[1].0, "b_key");
    }
}
