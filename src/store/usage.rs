//! Token usage and cost persistence.

use rusqlite::params;

use crate::error::Result;
use crate::store::Store;

impl Store {
    /// Record token usage from an LLM call.
    pub fn record_usage(
        &self,
        model: &str,
        input_tokens: u32,
        output_tokens: u32,
        cache_read_tokens: u32,
        cache_write_tokens: u32,
        reasoning_tokens: u32,
        estimated_usd: f64,
    ) -> Result<()> {
        self.conn().execute(
            "INSERT INTO usage (model, input_tokens, output_tokens, cache_read_tokens, cache_write_tokens, reasoning_tokens, estimated_usd)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                model,
                input_tokens,
                output_tokens,
                cache_read_tokens,
                cache_write_tokens,
                reasoning_tokens,
                estimated_usd,
            ],
        )?;
        Ok(())
    }

    /// Get total cost for today.
    pub fn daily_cost(&self) -> Result<f64> {
        let cost: f64 = self.conn().query_row(
            "SELECT COALESCE(SUM(estimated_usd), 0.0) FROM usage
             WHERE date(created_at) = date('now')",
            [],
            |row| row.get(0),
        )?;
        Ok(cost)
    }

    /// Get total cost for a specific date (YYYY-MM-DD).
    pub fn cost_for_date(&self, date: &str) -> Result<f64> {
        let cost: f64 = self.conn().query_row(
            "SELECT COALESCE(SUM(estimated_usd), 0.0) FROM usage
             WHERE date(created_at) = ?1",
            params![date],
            |row| row.get(0),
        )?;
        Ok(cost)
    }

    /// Get all-time total cost.
    pub fn total_cost(&self) -> Result<f64> {
        let cost: f64 = self.conn().query_row(
            "SELECT COALESCE(SUM(estimated_usd), 0.0) FROM usage",
            [],
            |row| row.get(0),
        )?;
        Ok(cost)
    }

    /// Get all-time total tokens.
    pub fn total_tokens(&self) -> Result<(u64, u64)> {
        let (input, output): (i64, i64) = self.conn().query_row(
            "SELECT COALESCE(SUM(input_tokens + cache_read_tokens + cache_write_tokens), 0),
                    COALESCE(SUM(output_tokens), 0)
             FROM usage",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        Ok((input as u64, output as u64))
    }
}

#[cfg(test)]
mod tests {
    use crate::store::Store;

    #[test]
    fn record_and_query_usage() {
        let store = Store::in_memory().unwrap();

        store
            .record_usage("claude-sonnet-4-6", 1000, 500, 0, 0, 0, 0.0075)
            .unwrap();
        store
            .record_usage("claude-sonnet-4-6", 2000, 1000, 0, 0, 0, 0.018)
            .unwrap();
        store
            .record_usage("claude-sonnet-4-6", 500, 200, 800, 0, 0, 0.005)
            .unwrap();

        let total = store.total_cost().unwrap();
        assert!((total - 0.0305).abs() < 0.0001, "total={}", total);
    }

    #[test]
    fn daily_cost_filters_by_date() {
        let store = Store::in_memory().unwrap();

        store
            .record_usage("claude-sonnet-4-6", 1000, 500, 0, 0, 0, 0.01)
            .unwrap();

        let today = store.daily_cost().unwrap();
        assert!(today > 0.0);

        let other = store.cost_for_date("2020-01-01").unwrap();
        assert!((other).abs() < 0.0001);
    }

    #[test]
    fn total_tokens_sums_correctly() {
        let store = Store::in_memory().unwrap();

        store
            .record_usage("test", 1000, 500, 200, 100, 50, 0.01)
            .unwrap();

        let (input, output) = store.total_tokens().unwrap();
        // input = input_tokens + cache_read + cache_write = 1000 + 200 + 100 = 1300
        assert_eq!(input, 1300);
        assert_eq!(output, 500);
    }

    #[test]
    fn empty_store_returns_zero() {
        let store = Store::in_memory().unwrap();

        assert!((store.total_cost().unwrap()).abs() < 0.0001);
        assert!((store.daily_cost().unwrap()).abs() < 0.0001);

        let (input, output) = store.total_tokens().unwrap();
        assert_eq!(input, 0);
        assert_eq!(output, 0);
    }
}
