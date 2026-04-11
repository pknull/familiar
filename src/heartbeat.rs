//! Heartbeat — periodic proactive check system.
//!
//! Runs a background loop that reads a checklist from the store,
//! asks the LLM to evaluate it, and prints actionable items.
//! Also evaluates scheduled triggers from HEARTBEAT.md and
//! triggers daily compaction at day boundaries.

use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use chrono::{Datelike, Local, Timelike};
use tokio::time;

use crate::agent::providers::{Message, Provider};
use crate::store::Store;
use crate::workspace::heartbeat::{self as heartbeat_config, Trigger};

const SYSTEM_PROMPT: &str =
    "You are running a periodic health check. Review the following checklist items \
     and ONLY report items that need attention right now. If everything looks fine, \
     respond with just 'OK'.";

/// Background heartbeat that periodically checks a user-defined checklist.
pub struct Heartbeat {
    provider: Box<dyn Provider>,
    store_path: String,
    workspace_daily_dir: String,
    interval: Duration,
    quiet_start: u32,
    quiet_end: u32,
    /// Scheduled triggers from HEARTBEAT.md (heartbeat-driven only).
    triggers: Vec<Trigger>,
    /// Last run date per trigger action (for schedule enforcement).
    trigger_last_run: HashMap<String, chrono::NaiveDate>,
    /// Last date compaction was triggered.
    last_compaction_date: Option<chrono::NaiveDate>,
}

impl Heartbeat {
    /// Create a new heartbeat.
    pub fn new(
        provider: Box<dyn Provider>,
        store_path: String,
        workspace_daily_dir: String,
        interval: Duration,
        quiet_start: u32,
        quiet_end: u32,
    ) -> Self {
        // Load HEARTBEAT.md for scheduled triggers
        let workspace_dir = crate::config::Config::expand_path("~/.familiar/workspace");
        let heartbeat_path = std::path::Path::new(&workspace_dir).join("HEARTBEAT.md");
        let triggers = match std::fs::read_to_string(&heartbeat_path) {
            Ok(content) => {
                let config = heartbeat_config::parse(&content);
                let filtered: Vec<Trigger> = config.triggers.into_iter()
                    .filter(|t| t.is_heartbeat())
                    .collect();
                if !filtered.is_empty() {
                    tracing::info!(count = filtered.len(), "loaded heartbeat triggers from HEARTBEAT.md");
                }
                filtered
            }
            Err(_) => Vec::new(),
        };

        Self {
            provider,
            store_path,
            workspace_daily_dir,
            interval,
            quiet_start,
            quiet_end,
            triggers,
            trigger_last_run: HashMap::new(),
            last_compaction_date: None,
        }
    }

    /// Run the heartbeat loop. This never returns under normal operation.
    pub async fn run(mut self) {
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
    async fn tick(&mut self) -> crate::error::Result<()> {
        // Check quiet hours
        let now = Local::now();
        let hour = now.hour();
        if self.in_quiet_hours(hour) {
            tracing::debug!(hour, "heartbeat skipped (quiet hours)");
            return Ok(());
        }

        // Daily compaction check
        let today = now.date_naive();
        if self.last_compaction_date != Some(today) {
            self.last_compaction_date = Some(today);
            self.trigger_daily_compaction();
        }

        // Evaluate scheduled triggers
        self.evaluate_scheduled_triggers(today);

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

            // Append heartbeat finding to daily log
            self.append_daily_log(&format!(
                "- [{}] heartbeat: {}",
                now.format("%H:%M"),
                trimmed
            ));
        }

        Ok(())
    }

    /// Evaluate scheduled triggers against the current date.
    fn evaluate_scheduled_triggers(&mut self, today: chrono::NaiveDate) {
        for trigger in &self.triggers {
            let schedule = match trigger.schedule.as_deref() {
                Some(s) => s,
                None => continue,
            };

            let should_fire = match schedule {
                "daily" => {
                    self.trigger_last_run.get(&trigger.action) != Some(&today)
                }
                "hourly" => true, // Fire every tick (tick interval controls frequency)
                "weekly" => {
                    let last = self.trigger_last_run.get(&trigger.action);
                    match last {
                        Some(d) => (today - *d).num_days() >= 7,
                        None => true,
                    }
                }
                _ => false,
            };

            if should_fire {
                tracing::info!(
                    action = %trigger.action,
                    schedule,
                    "scheduled trigger fired"
                );
                self.trigger_last_run.insert(trigger.action.clone(), today);

                self.append_daily_log(&format!(
                    "- [{}] scheduled:{} fired",
                    Local::now().format("%H:%M"),
                    trigger.action,
                ));
            }
        }
    }

    /// Trigger daily compaction and session pruning.
    fn trigger_daily_compaction(&self) {
        tracing::info!("daily housekeeping triggered");

        match Store::open(Path::new(&self.store_path)) {
            Ok(store) => {
                // Check conversation history size
                match store.turn_count() {
                    Ok(count) if count > 50 => {
                        tracing::info!(turns = count, "conversation history eligible for compaction");
                        self.append_daily_log(&format!(
                            "- [{}] compaction: {} turns in history",
                            Local::now().format("%H:%M"),
                            count,
                        ));
                    }
                    Ok(_) => {
                        tracing::debug!("conversation history too short for compaction");
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "failed to count turns for compaction");
                    }
                }

                // Prune sessions idle > 30 days
                match store.prune_idle_sessions(30 * 86400) {
                    Ok(removed) if removed > 0 => {
                        tracing::info!(removed, "pruned idle sessions");
                        self.append_daily_log(&format!(
                            "- [{}] pruned {} idle sessions",
                            Local::now().format("%H:%M"),
                            removed,
                        ));
                    }
                    Ok(_) => {}
                    Err(e) => {
                        tracing::warn!(error = %e, "failed to prune idle sessions");
                    }
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "failed to open store for daily housekeeping");
            }
        }
    }

    /// Check whether the given hour falls within quiet hours.
    fn in_quiet_hours(&self, hour: u32) -> bool {
        is_quiet_hour(self.quiet_start, self.quiet_end, hour)
    }

    /// Append an entry to today's daily log in the workspace.
    fn append_daily_log(&self, entry: &str) {
        let workspace_dir = std::path::PathBuf::from(&self.workspace_daily_dir);

        if !workspace_dir.exists() {
            let _ = std::fs::create_dir_all(&workspace_dir);
        }

        let today = Local::now().format("%Y-%m-%d");
        let path = workspace_dir.join(format!("{}.md", today));

        let mut content = std::fs::read_to_string(&path).unwrap_or_default();
        if !content.is_empty() && !content.ends_with('\n') {
            content.push('\n');
        }
        content.push_str(entry);
        content.push('\n');

        if let Err(e) = std::fs::write(&path, content) {
            tracing::warn!(error = %e, "failed to write daily log");
        }
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
