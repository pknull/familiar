//! Heartbeat — periodic proactive check system.
//!
//! Runs a background loop that reads a checklist from the store,
//! asks the LLM to evaluate it, and prints actionable items.

use std::path::Path;
use std::time::Duration;

use chrono::Timelike;
use tokio::time;

use crate::agent::providers::{Message, Provider};
use crate::store::Store;

const SYSTEM_PROMPT: &str =
    "You are running a periodic health check. Review the following checklist items \
     and ONLY report items that need attention right now. If everything looks fine, \
     respond with just 'OK'.";

/// Background heartbeat that periodically checks a user-defined checklist.
pub struct Heartbeat {
    provider: Box<dyn Provider>,
    store_path: String,
    interval: Duration,
    quiet_start: u32,
    quiet_end: u32,
}

impl Heartbeat {
    /// Create a new heartbeat.
    pub fn new(
        provider: Box<dyn Provider>,
        store_path: String,
        interval: Duration,
        quiet_start: u32,
        quiet_end: u32,
    ) -> Self {
        Self {
            provider,
            store_path,
            interval,
            quiet_start,
            quiet_end,
        }
    }

    /// Run the heartbeat loop. This never returns under normal operation.
    pub async fn run(self) {
        let mut ticker = time::interval(self.interval);

        // First tick fires immediately — skip it so we don't check on startup.
        ticker.tick().await;

        loop {
            ticker.tick().await;

            if let Err(e) = self.tick().await {
                tracing::warn!(error = %e, "heartbeat tick failed");
            }
        }
    }

    /// Execute a single heartbeat tick.
    async fn tick(&self) -> crate::error::Result<()> {
        // Check quiet hours
        let hour = chrono::Local::now().hour();
        if self.in_quiet_hours(hour) {
            tracing::debug!(hour, "heartbeat skipped (quiet hours)");
            return Ok(());
        }

        // Open a fresh store connection (Store is !Send, so we can't hold it across awaits)
        let store = Store::open(Path::new(&self.store_path))?;
        let checklist = store.get_context("heartbeat_checklist")?;

        let checklist = match checklist {
            Some(ref text) if !text.trim().is_empty() => text.clone(),
            _ => {
                tracing::debug!("heartbeat skipped (empty checklist)");
                return Ok(());
            }
        };

        // Ask the LLM
        let response = self
            .provider
            .chat(SYSTEM_PROMPT, &[Message::user(&checklist)], &[])
            .await?;

        let text = response.text();
        let trimmed = text.trim();

        if !trimmed.is_empty() && trimmed.to_lowercase() != "ok" {
            println!("\n[heartbeat] {trimmed}\n");
        }

        Ok(())
    }

    /// Check whether the given hour falls within quiet hours.
    fn in_quiet_hours(&self, hour: u32) -> bool {
        is_quiet_hour(self.quiet_start, self.quiet_end, hour)
    }
}

/// Check whether `hour` falls within the quiet range [start, end).
/// Handles overnight wrapping (e.g. 22:00 to 08:00).
fn is_quiet_hour(quiet_start: u32, quiet_end: u32, hour: u32) -> bool {
    if quiet_start <= quiet_end {
        // e.g. quiet_start=8, quiet_end=17 — quiet during daytime
        hour >= quiet_start && hour < quiet_end
    } else {
        // e.g. quiet_start=22, quiet_end=8 — quiet overnight (wraps midnight)
        hour >= quiet_start || hour < quiet_end
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quiet_hours_overnight() {
        // 22:00 - 08:00
        assert!(!is_quiet_hour(22, 8, 12)); // noon — not quiet
        assert!(!is_quiet_hour(22, 8, 21)); // 9pm — not quiet
        assert!(is_quiet_hour(22, 8, 22));  // 10pm — quiet
        assert!(is_quiet_hour(22, 8, 23));  // 11pm — quiet
        assert!(is_quiet_hour(22, 8, 0));   // midnight — quiet
        assert!(is_quiet_hour(22, 8, 7));   // 7am — quiet
        assert!(!is_quiet_hour(22, 8, 8));  // 8am — not quiet
    }

    #[test]
    fn quiet_hours_daytime() {
        // 8:00 - 17:00
        assert!(is_quiet_hour(8, 17, 12));  // noon — quiet
        assert!(!is_quiet_hour(8, 17, 7));  // 7am — not quiet
        assert!(!is_quiet_hour(8, 17, 17)); // 5pm — not quiet
        assert!(!is_quiet_hour(8, 17, 22)); // 10pm — not quiet
    }
}
