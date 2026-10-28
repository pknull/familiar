//! Daemon mode — persistent Familiar that watches the egregore feed.
//!
//! Connects to egregore SSE to receive real-time feed events, filters for
//! messages relevant to this identity, and processes them through the
//! conversation engine automatically.

use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use chrono::Timelike;
use futures::StreamExt;
use reqwest_eventsource::{Event, EventSource};
use tokio::time::sleep;

use crate::agent::conversation::Conversation;
use crate::egregore::EgregoreClient;
use crate::error::{FamiliarError, Result};
use crate::store::Store;
use crate::workspace::heartbeat::{self as heartbeat_config, Trigger};

/// Assignment state for a published task.
///
/// Kept as a fieldless enum because the only consumer
/// (`is_assigned_or_completed`) lumps Assigned and Completed together as
/// "not Pending"; per-state metadata (servitor identity, timestamps) is
/// available on the `assign_task` / `assign_task_result` / `task_result` /
/// `task_failed` feed messages and doesn't need shadow-storage on the local tracker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AssignmentState {
    /// Task published, waiting for offers / not yet assigned.
    Pending,
    /// assign_task published; tracker considers this task in flight.
    Assigned,
    /// task_result or task_failed received — terminal.
    Completed,
}

/// Tracks pending offers for a published task (for retry on assignment failure).
#[derive(Debug, Clone)]
struct PendingOffer {
    servitor: String,
    withdrawn: bool,
}

/// Recently observed servitor profile used for offer verification.
#[derive(Debug, Clone)]
struct ObservedServitorProfile {
    author: String,
    servitor_id: String,
    timestamp: String,
    manifest_ref: Option<String>,
}

/// Recently observed servitor manifest used for planner-basis verification.
#[derive(Debug, Clone)]
struct ObservedServitorManifest {
    servitor_id: String,
    target_ids: Vec<String>,
}

/// Recently observed environment snapshot used for planner-basis verification.
#[derive(Debug, Clone)]
struct ObservedEnvironmentSnapshot {
    servitor_id: String,
    target_id: String,
    manifest_ref: String,
}

/// Owner outcome known for an issued command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CommandStatus {
    /// Published (or attempted); no owner result seen. The outcome is
    /// unknown until the owner answers, however long that takes.
    Issued,
    /// Owner accepted. Terminal.
    Accepted,
    /// Owner rejected or superseded. Terminal: the owner replays this result
    /// for the same command id, so its replay can never change anything.
    Rejected,
}

/// An `AssignTaskV1` command this familiar issued, kept so the owner's
/// `assign_task_result` can be matched to it and a retry reuses its identity.
#[derive(Debug, Clone)]
struct IssuedCommand {
    task_id: String,
    offer_hash: String,
    executor: String,
    status: CommandStatus,
    /// When the unanswered command is next reconciled with the owner.
    next_check: chrono::DateTime<chrono::Utc>,
    /// Offset into the executor's feed where the next lookup resumes after
    /// a scan that hit the page cap.
    scan_offset: usize,
    /// Reconciliation rounds so far (feed lookups), for backoff.
    checks: u32,
    /// Republications already sent for this command id.
    replays: u32,
}

/// Selection identity: (task id, offer hash, executor).
type Selection = (String, String, String);

/// Most commands kept in memory, counted separately for the two classes.
/// Resolved history beyond the bound is dropped oldest first (a dropped
/// command's replayed result is then ignored). Unresolved commands are never
/// dropped: at the bound, new commands are refused until results arrive.
const MAX_TRACKED_COMMANDS: usize = 1024;

/// How long past `expires_at` an unanswered command waits before the first
/// replay; doubled after every replay.
const COMMAND_REPLAY_GRACE_SECS: i64 = 120;

/// Republications of one command id with a fresh `attempt_id`. Feed
/// lookups are counted separately and continue after this budget is spent;
/// the outcome stays unknown and the task stays assigned until the owner's
/// result is found: Familiar never reassigns on its own clock.
const MAX_COMMAND_REPLAYS: u32 = 5;

/// Longest wait between reconciliation rounds for one command.
const MAX_CHECK_BACKOFF_SECS: i64 = 3600;

/// Fresh command identities issued for one selection after `expired`
/// rejections, for the life of the task.
const MAX_ASSIGN_RENEWALS: u32 = 3;

/// Rejection reasons after which the same selection may be commanded again:
/// the offer is unaffected. Every other reason (`offer_unavailable`,
/// `stale_predecessor`, `task_resolved`, `task_mismatch`, `unauthorized`,
/// `assigner_mismatch`) describes the offer, task or authority and is final.
const RETRYABLE_REJECTION_REASONS: &[&str] = &["expired"];

/// Why `begin_command` issued no command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BeginRefusal {
    /// The owner rejected this selection; its outcome is final.
    Rejected,
    /// The selection used up its renewals.
    RenewalsExhausted,
    /// Too many unresolved commands.
    AtCapacity,
}

/// A selection to command again after a retryable rejection.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Renewal {
    task_id: String,
    offer_hash: String,
    executor: String,
}

/// An unanswered command due for a replay to the owner.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Replay {
    scan_offset: usize,
    /// Whether the republication budget still allows sending the command.
    may_publish: bool,
    command_id: String,
    task_id: String,
    offer_hash: String,
    executor: String,
}

/// Tracks auto-assignment state for tasks published by this familiar.
struct TaskAssignmentTracker {
    /// task_hash → assignment state
    tasks: HashMap<String, AssignmentState>,
    /// task_hash → pending offers (for retry)
    offers: HashMap<String, Vec<PendingOffer>>,
    /// command_id → issued assignment command
    commands: HashMap<String, IssuedCommand>,
    /// selection → command_id (direct idempotency lookup)
    selections: HashMap<Selection, String>,
    /// task_id → the command currently allowed to move the assignment
    active: HashMap<String, String>,
    /// ids of commands still `Issued`
    unresolved: std::collections::HashSet<String>,
    /// resolved command ids, oldest first, for bounding history
    resolved_order: std::collections::VecDeque<String>,
    /// task_id → selection → `expired` rejections seen; kept until the task
    /// completes so an offer replay cannot restart the renewal cycle
    expired_rejections: HashMap<String, HashMap<(String, String), u32>>,
}

impl TaskAssignmentTracker {
    fn new() -> Self {
        Self {
            tasks: HashMap::new(),
            offers: HashMap::new(),
            commands: HashMap::new(),
            selections: HashMap::new(),
            active: HashMap::new(),
            unresolved: std::collections::HashSet::new(),
            resolved_order: std::collections::VecDeque::new(),
            expired_rejections: HashMap::new(),
        }
    }

    fn expired_count(&self, task_id: &str, offer_hash: &str, executor: &str) -> u32 {
        self.expired_rejections
            .get(task_id)
            .and_then(|m| m.get(&(offer_hash.to_string(), executor.to_string())))
            .copied()
            .unwrap_or(0)
    }

    /// Command id to publish for this selection, made the task's active
    /// command. A retry reuses the existing id (RFC 0003 §8 idempotency key).
    fn begin_command(
        &mut self,
        task_id: &str,
        offer_hash: &str,
        executor: &str,
        expires_at: chrono::DateTime<chrono::Utc>,
    ) -> std::result::Result<String, BeginRefusal> {
        let key = (
            task_id.to_string(),
            offer_hash.to_string(),
            executor.to_string(),
        );
        let next_check = expires_at + chrono::Duration::seconds(COMMAND_REPLAY_GRACE_SECS);
        if let Some(id) = self.selections.get(&key).cloned() {
            let status = self.commands.get(&id).map(|c| c.status);
            if status == Some(CommandStatus::Rejected) {
                return Err(BeginRefusal::Rejected);
            }
            if status == Some(CommandStatus::Issued) {
                if let Some(c) = self.commands.get_mut(&id) {
                    c.next_check = next_check;
                }
            }
            self.active.insert(task_id.to_string(), id.clone());
            return Ok(id);
        }

        if self.expired_count(task_id, offer_hash, executor) > MAX_ASSIGN_RENEWALS {
            return Err(BeginRefusal::RenewalsExhausted);
        }
        if self.unresolved.len() >= MAX_TRACKED_COMMANDS {
            return Err(BeginRefusal::AtCapacity);
        }
        let id = format!("cmd-{}", random_hex());
        self.commands.insert(
            id.clone(),
            IssuedCommand {
                task_id: task_id.to_string(),
                offer_hash: offer_hash.to_string(),
                executor: executor.to_string(),
                status: CommandStatus::Issued,
                next_check,
                scan_offset: 0,
                checks: 0,
                replays: 0,
            },
        );
        self.unresolved.insert(id.clone());
        self.selections.insert(key, id.clone());
        self.active.insert(task_id.to_string(), id.clone());
        Ok(id)
    }

    /// Record a terminal outcome for an unresolved command and trim the
    /// resolved history.
    fn resolve(&mut self, command_id: &str, status: CommandStatus) {
        let Some(c) = self.commands.get_mut(command_id) else {
            return;
        };
        c.status = status;
        if self.unresolved.remove(command_id) {
            self.resolved_order.push_back(command_id.to_string());
            while self.resolved_order.len() > MAX_TRACKED_COMMANDS {
                if let Some(old) = self.resolved_order.pop_front() {
                    self.forget_command(&old);
                }
            }
        }
    }

    fn forget_command(&mut self, command_id: &str) {
        if let Some(c) = self.commands.remove(command_id) {
            let key = (c.task_id.clone(), c.offer_hash, c.executor);
            if self.selections.get(&key).map(String::as_str) == Some(command_id) {
                self.selections.remove(&key);
            }
            if self.active.get(&c.task_id).map(String::as_str) == Some(command_id) {
                self.active.remove(&c.task_id);
            }
        }
    }

    /// Active unanswered commands whose check time has passed. Each is
    /// rescheduled with doubling backoff (capped), whatever the round's
    /// outcome. Nothing here changes task state: an unanswered command keeps
    /// its task assigned until the owner answers.
    fn due_checks(&mut self, now: chrono::DateTime<chrono::Utc>) -> Vec<Replay> {
        let mut due = Vec::new();
        for id in &self.unresolved {
            let Some(c) = self.commands.get_mut(id) else {
                continue;
            };
            let is_active = self.active.get(&c.task_id) == Some(id);
            let completed = matches!(self.tasks.get(&c.task_id), Some(AssignmentState::Completed));
            if !is_active || completed || c.next_check > now {
                continue;
            }
            c.checks += 1;
            let wait = (COMMAND_REPLAY_GRACE_SECS << c.checks.min(10)).min(MAX_CHECK_BACKOFF_SECS);
            c.next_check = now + chrono::Duration::seconds(wait);
            due.push(Replay {
                scan_offset: c.scan_offset,
                may_publish: c.replays < MAX_COMMAND_REPLAYS,
                command_id: id.clone(),
                task_id: c.task_id.clone(),
                offer_hash: c.offer_hash.clone(),
                executor: c.executor.clone(),
            });
        }
        due
    }

    /// Remember where the next feed scan for this command resumes.
    fn set_scan_offset(&mut self, command_id: &str, offset: usize) {
        if let Some(c) = self.commands.get_mut(command_id) {
            c.scan_offset = offset;
        }
    }

    /// A capped scan made progress: continue at the next reconcile instead of
    /// after the doubled backoff, and do not count the round toward it.
    fn continue_scan_next_tick(&mut self, command_id: &str, now: chrono::DateTime<chrono::Utc>) {
        if let Some(c) = self.commands.get_mut(command_id) {
            c.next_check = now;
            c.checks = c.checks.saturating_sub(1);
        }
    }

    /// Count one republication against the command's budget.
    fn record_replay(&mut self, command_id: &str) {
        if let Some(c) = self.commands.get_mut(command_id) {
            c.replays += 1;
        }
    }

    fn is_unresolved(&self, command_id: &str) -> bool {
        self.unresolved.contains(command_id)
    }

    /// Whether an offer was withdrawn by its executor.
    fn offer_withdrawn(&self, task_id: &str, servitor: &str) -> bool {
        self.offers
            .get(task_id)
            .is_some_and(|o| o.iter().any(|o| o.servitor == servitor && o.withdrawn))
    }

    /// Register a published task hash for tracking.
    fn track(&mut self, hash: String) {
        self.tasks.insert(hash, AssignmentState::Pending);
    }

    /// Check if we're tracking a task hash.
    fn is_tracked(&self, hash: &str) -> bool {
        self.tasks.contains_key(hash)
    }

    /// Check if a task has moved past Pending (Assigned or Completed).
    fn is_assigned_or_completed(&self, hash: &str) -> bool {
        matches!(
            self.tasks.get(hash),
            Some(AssignmentState::Assigned) | Some(AssignmentState::Completed)
        )
    }

    /// Record an offer for a task.
    fn add_offer(&mut self, task_hash: &str, offer: PendingOffer) {
        self.offers
            .entry(task_hash.to_string())
            .or_default()
            .push(offer);
    }

    /// Mark a task as assigned (assign_task published).
    fn mark_assigned(&mut self, task_hash: &str) {
        if self.is_completed(task_hash) {
            return;
        }
        self.tasks
            .insert(task_hash.to_string(), AssignmentState::Assigned);
    }

    /// Whether the task reached a terminal state.
    fn is_completed(&self, hash: &str) -> bool {
        matches!(self.tasks.get(hash), Some(AssignmentState::Completed))
    }

    /// Return an assigned task to Pending after the owner declined the command.
    fn reopen(&mut self, task_hash: &str) {
        if self.is_completed(task_hash) {
            return;
        }
        self.tasks
            .insert(task_hash.to_string(), AssignmentState::Pending);
    }

    /// Mark a task as completed (task_result or task_failed received).
    fn mark_completed(&mut self, task_hash: &str) {
        self.expired_rejections.remove(task_hash);
        // The task ran, so its commands need no further reconciliation: the
        // current one is accepted in effect, any other is moot.
        let open: Vec<String> = self
            .unresolved
            .iter()
            .filter(|id| {
                self.commands
                    .get(*id)
                    .is_some_and(|c| c.task_id == task_hash)
            })
            .cloned()
            .collect();
        for id in open {
            let status = if self.active.get(task_hash) == Some(&id) {
                CommandStatus::Accepted
            } else {
                CommandStatus::Rejected
            };
            self.resolve(&id, status);
        }
        self.active.remove(task_hash);
        self.tasks
            .insert(task_hash.to_string(), AssignmentState::Completed);
    }

    /// Apply an owner `assign_task_result`, whenever it arrives. The result
    /// must come from the executor the command targeted. An acceptance is the
    /// owner's truth and makes that command current. A rejection or
    /// supersession reopens the task only while that command is the task's
    /// current one; a replayed result for an older or already-resolved
    /// command changes nothing. A retryable rejection (offer still valid)
    /// additionally yields a [`Renewal`] for the same selection, bounded per
    /// selection for the life of the task.
    fn apply_assign_result(
        &mut self,
        author: &str,
        content: &serde_json::Value,
    ) -> Option<Renewal> {
        let command_id = content.get("command_id").and_then(|c| c.as_str())?;
        let command = self.commands.get(command_id).cloned()?;
        if author != command.executor {
            tracing::warn!(
                command_id,
                signing_author = %author,
                "ignoring assign_task_result not signed by the targeted executor"
            );
            return None;
        }
        if command.status != CommandStatus::Issued {
            return None;
        }

        match content.get("status").and_then(|s| s.as_str()) {
            Some("accepted") => {
                tracing::info!(command_id, task_id = %command.task_id, "assignment accepted");
                self.resolve(command_id, CommandStatus::Accepted);
                self.active
                    .insert(command.task_id.clone(), command_id.to_string());
                if !self.is_completed(&command.task_id) {
                    self.mark_assigned(&command.task_id);
                }
                None
            }
            Some(status @ ("rejected" | "superseded")) => {
                self.resolve(command_id, CommandStatus::Rejected);
                let is_current =
                    self.active.get(&command.task_id).map(String::as_str) == Some(command_id);
                if !is_current {
                    tracing::debug!(command_id, "rejection of a non-current command recorded");
                    return None;
                }
                let reason = content.get("reason").and_then(|r| r.as_str()).unwrap_or("");
                let retryable =
                    status == "rejected" && RETRYABLE_REJECTION_REASONS.contains(&reason);
                tracing::warn!(
                    command_id,
                    task_id = %command.task_id,
                    status,
                    reason,
                    retryable,
                    "assignment not accepted"
                );
                self.active.remove(&command.task_id);
                if !self.is_completed(&command.task_id) {
                    self.reopen(&command.task_id);
                }
                if !retryable {
                    self.mark_offer_withdrawn(&command.task_id, &command.executor);
                    return None;
                }
                // The rejected command keeps its terminal outcome; the
                // selection is freed so a new command identity can be issued.
                let key = (
                    command.task_id.clone(),
                    command.offer_hash.clone(),
                    command.executor.clone(),
                );
                if self.selections.get(&key).map(String::as_str) == Some(command_id) {
                    self.selections.remove(&key);
                }
                let count = self
                    .expired_rejections
                    .entry(command.task_id.clone())
                    .or_default()
                    .entry((command.offer_hash.clone(), command.executor.clone()))
                    .or_insert(0);
                *count += 1;
                let can_renew = *count <= MAX_ASSIGN_RENEWALS
                    && !self.is_completed(&command.task_id)
                    && !self.offer_withdrawn(&command.task_id, &command.executor);
                can_renew.then_some(Renewal {
                    task_id: command.task_id,
                    offer_hash: command.offer_hash,
                    executor: command.executor,
                })
            }
            _ => None,
        }
    }

    #[cfg(test)]
    fn rename_command(&mut self, from: &str, to: &str) {
        let c = self.commands.remove(from).unwrap();
        self.selections.insert(
            (c.task_id.clone(), c.offer_hash.clone(), c.executor.clone()),
            to.into(),
        );
        self.active.insert(c.task_id.clone(), to.into());
        self.unresolved.remove(from);
        self.unresolved.insert(to.into());
        self.commands.insert(to.into(), c);
    }

    /// Mark an offer as withdrawn.
    fn mark_offer_withdrawn(&mut self, task_hash: &str, servitor: &str) {
        if let Some(offers) = self.offers.get_mut(task_hash) {
            for offer in offers.iter_mut() {
                if offer.servitor == servitor {
                    offer.withdrawn = true;
                }
            }
        }
    }
}

/// Lifetime of a published `assign_task` command before the owner rejects it.
const ASSIGN_COMMAND_TTL_SECS: i64 = 600;

/// A 64-character lowercase hex feed envelope hash.
fn is_feed_hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// 128 random bits as lowercase hex, for command and attempt identities.
fn random_hex() -> String {
    use rand::RngCore;
    let mut bytes = [0_u8; 16];
    rand::thread_rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
}

/// How often unanswered assignment commands are reconciled while idle.
const RECONCILE_INTERVAL_SECS: u64 = 30;

/// Page size and page cap when scanning an executor's feed for a result.
const RECONCILE_PAGE_SIZE: usize = 100;
const RECONCILE_MAX_PAGES: usize = 20;

/// Outcome of scanning the feed for an owner result.
enum Lookup {
    Found(serde_json::Value),
    /// The whole feed was read and holds none.
    Absent,
    /// The page cap was reached first; absence is not established. Carries
    /// the offset the next round resumes from.
    Incomplete(usize),
}

/// Event sources that can be shut down after a terminal event.
pub trait CloseEvents {
    fn close_events(&mut self);
}

impl CloseEvents for EventSource {
    fn close_events(&mut self) {
        self.close();
    }
}

impl<T> CloseEvents for futures::stream::Pending<T> {
    fn close_events(&mut self) {}
}

/// Maximum consecutive SSE failures before applying long backoff.
const MAX_RETRIES_BEFORE_LONG_BACKOFF: u32 = 5;

/// Short retry delay (seconds).
const SHORT_RETRY_SECS: u64 = 5;

/// Long retry delay after repeated failures (seconds).
const LONG_RETRY_SECS: u64 = 60;

/// Maximum model-generated answer size accepted for a network response.
const MAX_QUERY_RESPONSE_BYTES: usize = 16 * 1024;

fn is_canonical_message_hash(hash: &str) -> bool {
    hash.len() == 64
        && hash
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn response_content(query_hash: &str, answer: &str) -> Option<serde_json::Value> {
    let mut end = answer.len().min(MAX_QUERY_RESPONSE_BYTES);
    while !answer.is_char_boundary(end) {
        end -= 1;
    }
    let answer = &answer[..end];
    if answer.trim().is_empty() {
        return None;
    }
    Some(serde_json::json!({
        "type": "response",
        "query_hash": query_hash,
        "answer": answer,
    }))
}

/// Daemon — persistent feed watcher and auto-responder.
pub struct Daemon {
    conversation: Conversation,
    egregore: EgregoreClient,
    egregore_url: String,
    identity_id: String,
    store_path: String,
    scope: crate::config::DaemonConfig,
    agent_config: crate::config::AgentConfig,
    workspace: crate::workspace::Workspace,
    tracker: TaskAssignmentTracker,
    servitor_profiles: HashMap<String, ObservedServitorProfile>,
    servitor_manifests: HashMap<String, ObservedServitorManifest>,
    environment_snapshots: HashMap<String, ObservedEnvironmentSnapshot>,
    /// SSE-driven triggers loaded from HEARTBEAT.md.
    sse_triggers: Vec<Trigger>,
    /// Quiet hours (start, end) — gates SSE-trigger actions like all other
    /// proactive output (shared semantics with heartbeat::is_quiet_hour).
    quiet_hours: (u32, u32),
    /// Time source for command reconciliation.
    clock: fn() -> chrono::DateTime<chrono::Utc>,
}

impl Daemon {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        conversation: Conversation,
        egregore: EgregoreClient,
        egregore_url: String,
        identity_id: String,
        store_path: String,
        scope: crate::config::DaemonConfig,
        agent_config: crate::config::AgentConfig,
        workspace: crate::workspace::Workspace,
        quiet_hours: (u32, u32),
    ) -> Self {
        // Load HEARTBEAT.md triggers for SSE-driven proactive behavior
        let workspace_dir = crate::config::Config::expand_path("~/.familiar/workspace");
        let heartbeat_path = std::path::Path::new(&workspace_dir).join("HEARTBEAT.md");
        let sse_triggers = match std::fs::read_to_string(&heartbeat_path) {
            Ok(content) => {
                let config = heartbeat_config::parse(&content);
                let triggers: Vec<Trigger> =
                    config.triggers.into_iter().filter(|t| t.is_sse()).collect();
                if !triggers.is_empty() {
                    tracing::info!(
                        count = triggers.len(),
                        "loaded SSE triggers from HEARTBEAT.md"
                    );
                }
                triggers
            }
            Err(_) => {
                tracing::debug!("no HEARTBEAT.md found, SSE triggers disabled");
                Vec::new()
            }
        };

        Self {
            conversation,
            egregore,
            egregore_url,
            identity_id,
            store_path,
            scope,
            agent_config,
            workspace,
            tracker: TaskAssignmentTracker::new(),
            servitor_profiles: HashMap::new(),
            servitor_manifests: HashMap::new(),
            environment_snapshots: HashMap::new(),
            sse_triggers,
            quiet_hours,
            clock: chrono::Utc::now,
        }
    }

    /// Replace the time source used to decide when commands are due.
    #[must_use]
    pub fn with_clock(mut self, clock: fn() -> chrono::DateTime<chrono::Utc>) -> Self {
        self.clock = clock;
        self
    }

    /// Run the daemon loop. Connects to egregore SSE and processes events.
    /// Reconnects automatically on failure with exponential backoff.
    /// Handles SIGTERM/SIGINT gracefully — completes in-progress work before exiting.
    pub async fn run(mut self) -> Result<()> {
        tracing::info!(
            identity = %self.identity_id,
            egregore = %self.egregore_url,
            "daemon starting"
        );

        if self.agent_config.trusted_servitors.is_empty() {
            tracing::warn!(
                "task auto-assignment is disabled: configure agent.trusted_servitors to enable it"
            );
        }

        let mut consecutive_failures: u32 = 0;
        let shutdown = tokio::signal::ctrl_c();
        tokio::pin!(shutdown);

        loop {
            tokio::select! {
                biased;

                _ = &mut shutdown => {
                    tracing::info!("shutdown signal received — completing in-progress work");
                    // The current SSE event processing will complete before this branch runs,
                    // since select! waits for the first branch to resolve.
                    tracing::info!("daemon stopped gracefully");
                    return Ok(());
                }

                result = self.run_sse_loop() => {
                    match result {
                        Ok(()) => {
                            tracing::info!("SSE stream ended, reconnecting");
                            consecutive_failures = 0;
                            sleep(Duration::from_secs(SHORT_RETRY_SECS)).await;
                        }
                        Err(e) => {
                            consecutive_failures += 1;
                            let delay = if consecutive_failures > MAX_RETRIES_BEFORE_LONG_BACKOFF {
                                LONG_RETRY_SECS
                            } else {
                                SHORT_RETRY_SECS * u64::from(consecutive_failures)
                            };

                            tracing::warn!(
                                error = %e,
                                consecutive_failures,
                                retry_in_secs = delay,
                                "SSE connection failed, will retry"
                            );
                            sleep(Duration::from_secs(delay)).await;
                        }
                    }
                }
            }
        }
    }

    /// Connect to SSE and process events until the stream ends or errors.
    async fn run_sse_loop(&mut self) -> Result<()> {
        let url = format!("{}/v1/events", self.egregore_url);
        let es = EventSource::get(&url);
        self.drive_events(es, Duration::from_secs(RECONCILE_INTERVAL_SECS))
            .await
    }

    /// Process events from `events` until it ends or errors, reconciling
    /// unanswered assignment commands every `tick` as well as on each
    /// message, so an idle daemon still settles them.
    pub async fn drive_events<S>(&mut self, mut events: S, tick: Duration) -> Result<()>
    where
        S: futures::Stream<Item = std::result::Result<Event, reqwest_eventsource::Error>>
            + Unpin
            + CloseEvents,
    {
        let mut ticker = tokio::time::interval(tick);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        ticker.tick().await; // the first tick completes immediately

        loop {
            let event = tokio::select! {
                event = events.next() => event,
                _ = ticker.tick() => {
                    let now = (self.clock)();
                    self.reconcile_commands(now).await;
                    continue;
                }
            };
            let Some(event) = event else {
                return Ok(());
            };
            match event {
                Ok(Event::Open) => {
                    tracing::info!("SSE connected to egregore");
                }
                Ok(Event::Message(msg)) => {
                    if let Err(e) = self.handle_sse_message(&msg.data).await {
                        tracing::warn!(error = %e, "failed to handle SSE message");
                    }
                }
                Err(reqwest_eventsource::Error::StreamEnded) => {
                    tracing::info!("SSE stream ended");
                    events.close_events();
                    return Ok(());
                }
                Err(e) => {
                    tracing::warn!(error = %e, "SSE error");
                    events.close_events();
                    return Err(FamiliarError::Egregore {
                        reason: format!("SSE connection error: {}", e),
                    });
                }
            }
        }
    }

    /// Parse and dispatch a single SSE message.
    pub async fn handle_sse_message(&mut self, data: &str) -> Result<()> {
        let message: serde_json::Value = serde_json::from_str(data)?;

        // Ask the owner again about commands left unanswered past expiry.
        self.reconcile_commands((self.clock)()).await;

        // Evaluate SSE triggers against every message (before relevance filter)
        if !self.sse_triggers.is_empty() {
            self.evaluate_sse_triggers(&message);
        }

        if !self.is_relevant(&message) {
            return Ok(());
        }

        let msg_type = message
            .get("content")
            .and_then(|c| c.get("type"))
            .and_then(|t| t.as_str())
            .unwrap_or("unknown");

        let author = message
            .get("author")
            .and_then(|a| a.as_str())
            .unwrap_or("unknown");

        tracing::info!(msg_type, author, "processing relevant feed message");

        match msg_type {
            "query" => self.handle_query(&message).await,
            "servitor_profile" => {
                self.handle_servitor_profile(&message);
                Ok(())
            }
            "servitor_manifest" => {
                self.handle_servitor_manifest(&message);
                Ok(())
            }
            "environment_snapshot" => {
                self.handle_environment_snapshot(&message);
                Ok(())
            }
            "task_result" => {
                // Mark in tracker if we're tracking this task
                if let Some(hash) = self.result_task_id(&message) {
                    self.tracker.mark_completed(hash);
                }
                self.handle_task_result(&message).await
            }
            "task_offer" => self.handle_task_offer(&message).await,
            "assign_task_result" => {
                self.handle_assign_task_result(&message).await;
                Ok(())
            }
            "task_started" => self.handle_task_started(&message),
            "task_failed" => {
                if let Some(hash) = self.result_task_id(&message) {
                    self.tracker.mark_completed(hash);
                }
                self.handle_task_result(&message).await
            }
            "task_offer_withdraw" => {
                self.handle_offer_withdraw(&message);
                Ok(())
            }
            _ => {
                tracing::debug!(msg_type, "ignoring unhandled message type");
                Ok(())
            }
        }
    }

    /// Handle a task_offer: auto-assign if it matches a task we published (first-offer-wins).
    async fn handle_task_offer(&mut self, message: &serde_json::Value) -> Result<()> {
        let content = message.get("content").unwrap_or(&serde_json::Value::Null);
        let task_id = match content.get("task_id").and_then(|t| t.as_str()) {
            Some(id) => id,
            None => return Ok(()),
        };

        // Only handle offers for tasks we published.
        // The in-memory tracker is best-effort; fall back to the local publish
        // log so daemon restarts or non-daemon publishes do not lose correlation.
        if !self.tracker.is_tracked(task_id) {
            if self.is_our_task_id(task_id) {
                self.tracker.track(task_id.to_string());
            } else {
                return Ok(());
            }
        }

        // Skip if already assigned
        if self.tracker.is_assigned_or_completed(task_id) {
            tracing::debug!(task_id, "ignoring offer — task already assigned");
            return Ok(());
        }

        let servitor = match content.get("servitor").and_then(|s| s.as_str()) {
            Some(s) => s.to_string(),
            None => return Ok(()),
        };

        // Identity binding: the claimed executor must be the signing feed
        // author. A spoofed offer is dropped without being recorded.
        let author = message.get("author").and_then(|a| a.as_str()).unwrap_or("");
        if author != servitor {
            tracing::warn!(
                claimed_servitor = %servitor,
                signing_author = %author,
                task_id,
                "rejecting task_offer: claimed executor is not the signing author"
            );
            return Ok(());
        }

        // Record the offer so an operator can act on it even when
        // auto-assignment stays disabled.
        self.tracker.add_offer(
            task_id,
            PendingOffer {
                servitor: servitor.clone(),
                withdrawn: false,
            },
        );

        let offer_hash = message.get("hash").and_then(|h| h.as_str()).unwrap_or("");
        self.assign_if_eligible(task_id, &servitor, offer_hash)
            .await
    }

    /// Eligibility checks shared by every new command identity (first
    /// assignment and renewals): configured trust, and a published profile
    /// and planner basis that match the task as they are now.
    async fn assign_if_eligible(
        &mut self,
        task_id: &str,
        servitor: &str,
        offer_hash: &str,
    ) -> Result<()> {
        // Fail-closed trust: auto-assignment requires explicit configuration.
        if self.agent_config.trusted_servitors.is_empty() {
            tracing::debug!(
                servitor,
                task_id,
                "offer recorded; auto-assignment disabled until trusted_servitors is configured"
            );
            return Ok(());
        }
        if !self
            .agent_config
            .trusted_servitors
            .iter()
            .any(|t| t == servitor)
        {
            tracing::debug!(servitor, task_id, "ignoring offer from untrusted servitor");
            return Ok(());
        }

        // Verified trust: the offer must match the servitor's published
        // profile and the task's planner basis before assignment.
        if !self.offer_matches_planner_basis(task_id, servitor).await? {
            tracing::debug!(
                servitor,
                task_id,
                "ignoring offer without matching servitor_profile"
            );
            return Ok(());
        }

        self.publish_assignment(task_id, servitor, offer_hash).await;
        Ok(())
    }

    /// Resolve the bound hashes, record the command and publish `assign_task`
    /// for one selection. Failures are logged; the task stays assignable.
    async fn publish_assignment(&mut self, task_id: &str, servitor: &str, offer_hash: &str) {
        // The command binds the task's feed hash and the offer's feed hash;
        // both must be real 64-hex envelope hashes or the owner rejects it.
        let task_hash = match self.task_feed_hash(task_id) {
            Some(hash) if is_feed_hash(&hash) => hash,
            _ => {
                tracing::warn!(task_id, "cannot assign: task envelope hash is unknown or ambiguous (same content published more than once)");
                return;
            }
        };
        if !is_feed_hash(offer_hash) {
            tracing::warn!(task_id, "cannot assign: offer carries no feed hash");
            return;
        }

        let expires_at = chrono::Utc::now() + chrono::Duration::seconds(ASSIGN_COMMAND_TTL_SECS);
        let command_id = match self
            .tracker
            .begin_command(task_id, offer_hash, servitor, expires_at)
        {
            Ok(id) => id,
            Err(BeginRefusal::Rejected) => {
                tracing::debug!(task_id, servitor, "owner already rejected this selection");
                return;
            }
            Err(BeginRefusal::RenewalsExhausted) => {
                tracing::warn!(
                    task_id,
                    servitor,
                    "assignment renewals exhausted for this selection"
                );
                return;
            }
            Err(BeginRefusal::AtCapacity) => {
                tracing::warn!(
                    task_id,
                    "cannot assign: too many unanswered assignment commands"
                );
                return;
            }
        };
        self.send_command(
            task_id,
            servitor,
            offer_hash,
            &command_id,
            &task_hash,
            expires_at,
        )
        .await;
    }

    /// Publish one attempt (fresh `attempt_id`) of a command.
    async fn send_command(
        &mut self,
        task_id: &str,
        servitor: &str,
        offer_hash: &str,
        command_id: &str,
        task_hash: &str,
        expires_at: chrono::DateTime<chrono::Utc>,
    ) {
        // Publish AssignTaskV1 (content type `assign_task`).
        let assign_content = serde_json::json!({
            "type": "assign_task",
            "task_id": task_id,
            "task_hash": task_hash,
            "offer_hash": offer_hash,
            "executor": servitor,
            "assigner": self.identity_id,
            "predecessor": offer_hash,
            "command_id": command_id,
            "attempt_id": format!("att-{}", random_hex()),
            "expires_at": expires_at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        });

        match self
            .egregore
            .publish_content(assign_content, &["assign_task"])
            .await
        {
            Ok(hash) => {
                tracing::info!(
                    task_id,
                    servitor,
                    command_id,
                    assign_hash = hash,
                    "published assign_task"
                );
                self.tracker.mark_assigned(task_id);
            }
            Err(e) => {
                tracing::warn!(
                    task_id,
                    error = %e,
                    "failed to publish assign_task"
                );
            }
        }
    }

    /// Settle commands that got no result by their expiry plus a growing
    /// grace, without ever reopening or reassigning a task on Familiar's own
    /// clock. For each due command the executor's feed is scanned for the
    /// recorded `assign_task_result`; if present it is applied exactly like a
    /// live result. Only a complete scan that finds none (the owner never
    /// saw the command) republishes the same `command_id` with a fresh
    /// `attempt_id` (RFC 0003 §8), within the republication budget. A failed
    /// or incomplete scan is not absence: nothing is sent and the lookup is
    /// retried later.
    pub async fn reconcile_commands(&mut self, now: chrono::DateTime<chrono::Utc>) {
        for r in self.tracker.due_checks(now) {
            let lookup = self.recorded_result(&r).await;
            // Resume the next round where this scan stopped; any conclusive
            // outcome restarts from the head.
            let next_offset = match &lookup {
                Ok(Lookup::Incomplete(o)) => *o,
                _ => 0,
            };
            self.tracker.set_scan_offset(&r.command_id, next_offset);
            match lookup {
                Ok(Lookup::Found(message)) => {
                    tracing::info!(command_id = %r.command_id, "applying owner result found on the feed");
                    self.handle_assign_task_result(&message).await;
                    if !self.tracker.is_unresolved(&r.command_id) {
                        continue;
                    }
                }
                Ok(Lookup::Absent) => {}
                Ok(Lookup::Incomplete(_)) => {
                    self.tracker.continue_scan_next_tick(&r.command_id, now);
                    tracing::warn!(command_id = %r.command_id, "feed scan hit the page cap; will retry later");
                    continue;
                }
                Err(e) => {
                    tracing::warn!(command_id = %r.command_id, error = %e, "feed lookup failed; will retry later");
                    continue;
                }
            }
            if !r.may_publish {
                tracing::warn!(command_id = %r.command_id, "no owner result and replay budget spent; outcome unknown");
                continue;
            }
            let Some(task_hash) = self.task_feed_hash(&r.task_id).filter(|h| is_feed_hash(h))
            else {
                continue;
            };
            tracing::info!(command_id = %r.command_id, task_id = %r.task_id, "replaying unanswered assign_task");
            self.tracker.record_replay(&r.command_id);
            let expires_at = now + chrono::Duration::seconds(ASSIGN_COMMAND_TTL_SECS);
            self.send_command(
                &r.task_id,
                &r.executor,
                &r.offer_hash,
                &r.command_id,
                &task_hash,
                expires_at,
            )
            .await;
        }
    }

    /// Scan the executor's own feed (`/v1/feed/:author`, filtered to
    /// `assign_task_result`) for this command's result. The generic
    /// `/v1/feed` ignores `author`, so it cannot be used.
    ///
    /// `Absent` means only that the executor's `assign_task_result` feed was
    /// read to its end (a short page), possibly across several rounds, with
    /// no match. No timestamp decides absence: the executor's clock is not
    /// ours. A scan that reaches the page cap returns `Incomplete` with the
    /// offset to resume from: new messages join the head and push older ones
    /// to higher offsets, so resuming at an earlier offset only re-reads.
    async fn recorded_result(&self, r: &Replay) -> Result<Lookup> {
        let mut offset = r.scan_offset;
        for _ in 0..RECONCILE_MAX_PAGES {
            let messages = self
                .egregore
                .query_author_feed(
                    &r.executor,
                    "assign_task_result",
                    RECONCILE_PAGE_SIZE,
                    offset,
                )
                .await?;
            let full_page = messages.len() >= RECONCILE_PAGE_SIZE;
            if let Some(found) = messages.into_iter().find(|m| {
                m.get("author").and_then(|a| a.as_str()) == Some(r.executor.as_str())
                    && m.get("content")
                        .and_then(|c| c.get("command_id"))
                        .and_then(|c| c.as_str())
                        == Some(r.command_id.as_str())
            }) {
                return Ok(Lookup::Found(found));
            }
            if !full_page {
                return Ok(Lookup::Absent);
            }
            offset += RECONCILE_PAGE_SIZE;
        }
        Ok(Lookup::Incomplete(offset))
    }

    /// Feed hash of a task we published, resolved from the local publish log.
    fn task_feed_hash(&self, task_id: &str) -> Option<String> {
        match Store::open(Path::new(&self.store_path)) {
            Ok(store) => store.task_feed_hash(task_id).unwrap_or(None),
            Err(e) => {
                tracing::warn!(error = %e, "failed to open store for task hash lookup");
                None
            }
        }
    }

    /// Apply the owner's `assign_task_result` to the tracker, publishing a
    /// fresh command when the rejection was retryable.
    async fn handle_assign_task_result(&mut self, message: &serde_json::Value) {
        let author = message.get("author").and_then(|a| a.as_str()).unwrap_or("");
        let content = message.get("content").unwrap_or(&serde_json::Value::Null);
        if let Some(r) = self.tracker.apply_assign_result(author, content) {
            // A new command identity passes the same eligibility checks as
            // the first assignment.
            if let Err(e) = self
                .assign_if_eligible(&r.task_id, &r.executor, &r.offer_hash)
                .await
            {
                tracing::warn!(task_id = %r.task_id, error = %e, "renewal eligibility check failed");
            }
        }
    }

    /// Cache a recently observed servitor profile for later offer verification.
    fn handle_servitor_profile(&mut self, message: &serde_json::Value) {
        let author = match message.get("author").and_then(|a| a.as_str()) {
            Some(author) => author.to_string(),
            None => return,
        };
        let timestamp = message
            .get("timestamp")
            .and_then(|t| t.as_str())
            .unwrap_or("")
            .to_string();
        let content = message.get("content").unwrap_or(&serde_json::Value::Null);
        let servitor_id = content
            .get("servitor_id")
            .and_then(|s| s.as_str())
            .unwrap_or(&author)
            .to_string();

        self.servitor_profiles.insert(
            servitor_id.clone(),
            ObservedServitorProfile {
                author,
                servitor_id,
                timestamp,
                manifest_ref: content
                    .get("manifest_ref")
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
            },
        );
    }

    /// Cache a recently observed servitor manifest.
    fn handle_servitor_manifest(&mut self, message: &serde_json::Value) {
        let hash = match message.get("hash").and_then(|h| h.as_str()) {
            Some(hash) => hash.to_string(),
            None => return,
        };
        let content = message.get("content").unwrap_or(&serde_json::Value::Null);
        let servitor_id = match content.get("servitor_id").and_then(|s| s.as_str()) {
            Some(servitor_id) => servitor_id.to_string(),
            None => return,
        };
        let target_ids = content
            .get("deployment_targets")
            .and_then(|targets| targets.as_array())
            .map(|targets| {
                targets
                    .iter()
                    .filter_map(|target| target.get("target_id").and_then(|v| v.as_str()))
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        self.servitor_manifests.insert(
            hash,
            ObservedServitorManifest {
                servitor_id,
                target_ids,
            },
        );
    }

    /// Cache a recently observed environment snapshot.
    fn handle_environment_snapshot(&mut self, message: &serde_json::Value) {
        let hash = match message.get("hash").and_then(|h| h.as_str()) {
            Some(hash) => hash.to_string(),
            None => return,
        };
        let content = message.get("content").unwrap_or(&serde_json::Value::Null);
        let servitor_id = match content.get("servitor_id").and_then(|s| s.as_str()) {
            Some(servitor_id) => servitor_id.to_string(),
            None => return,
        };
        let target_id = match content.get("target_id").and_then(|s| s.as_str()) {
            Some(target_id) => target_id.to_string(),
            None => return,
        };
        let manifest_ref = match content.get("manifest_ref").and_then(|s| s.as_str()) {
            Some(manifest_ref) => manifest_ref.to_string(),
            None => return,
        };

        self.environment_snapshots.insert(
            hash,
            ObservedEnvironmentSnapshot {
                servitor_id,
                target_id,
                manifest_ref,
            },
        );
    }

    /// Handle task_started: log execution observation.
    ///
    /// No tracker state change — `Assigned` is the only "in-flight" state in
    /// the simplified lifecycle, and the tracker doesn't distinguish
    /// pre-start from post-start. Operators see the start via this trace.
    fn handle_task_started(&mut self, message: &serde_json::Value) -> Result<()> {
        let content = message.get("content").unwrap_or(&serde_json::Value::Null);
        if let Some(task_id) = content.get("task_id").and_then(|t| t.as_str()) {
            if self.tracker.is_tracked(task_id) {
                tracing::info!(task_id, "task execution confirmed (task_started received)");
            }
        }
        Ok(())
    }

    /// Handle task_offer_withdraw: mark offer as withdrawn, retry if needed.
    fn handle_offer_withdraw(&mut self, message: &serde_json::Value) {
        let content = message.get("content").unwrap_or(&serde_json::Value::Null);
        let task_id = match content.get("task_id").and_then(|t| t.as_str()) {
            Some(id) => id,
            None => return,
        };
        let servitor = match content.get("servitor").and_then(|s| s.as_str()) {
            Some(s) => s,
            None => return,
        };

        if !self.tracker.is_tracked(task_id) {
            return;
        }

        self.tracker.mark_offer_withdrawn(task_id, servitor);
        tracing::debug!(task_id, servitor, "offer withdrawn");
    }

    /// Check whether a feed message is relevant to this daemon.
    fn is_relevant(&self, message: &serde_json::Value) -> bool {
        let author = message.get("author").and_then(|a| a.as_str()).unwrap_or("");

        // Never process our own messages.
        if author == self.identity_id {
            return false;
        }

        let content = match message.get("content") {
            Some(c) => c,
            None => return false,
        };

        let msg_type = content.get("type").and_then(|t| t.as_str()).unwrap_or("");

        // Apply configured scope filters.
        let tags: Vec<String> = message
            .get("tags")
            .and_then(|t| t.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        if !self
            .scope
            .matches_scope(Some(author), Some(msg_type), &tags)
        {
            return false;
        }

        match msg_type {
            // Queries from other agents — we may want to respond.
            "query" => self.is_query_for_us(content),

            // Planner-visible executor profiles, manifests, and snapshots are
            // needed for mandatory offer verification whenever trust is
            // configured; without trusted servitors nothing can be assigned,
            // so nothing needs caching.
            "servitor_profile" | "servitor_manifest" | "environment_snapshot" => {
                !self.agent_config.trusted_servitors.is_empty()
            }

            // Task results — check if we published the originating task.
            "task_result" | "task_failed" => self.is_our_task_result(message),

            // Owner decision on an assignment command we issued.
            "assign_task_result" => content
                .get("command_id")
                .and_then(|c| c.as_str())
                .map(|command_id| self.tracker.commands.contains_key(command_id))
                .unwrap_or(false),

            // Task lifecycle messages — relevant if they match a task we're tracking.
            // These bypass normal scope filters since they're protocol messages.
            "task_offer" | "task_started" | "task_offer_withdraw" => content
                .get("task_id")
                .and_then(|t| t.as_str())
                .map(|task_id| self.is_our_task_id(task_id))
                .unwrap_or(false),

            _ => false,
        }
    }

    fn is_our_task_id(&self, task_id: &str) -> bool {
        if self.tracker.is_tracked(task_id) {
            return true;
        }

        match Store::open(Path::new(&self.store_path)) {
            Ok(store) => store.has_published_hash(task_id).unwrap_or(false),
            Err(e) => {
                tracing::warn!(error = %e, "failed to open store for task check");
                false
            }
        }
    }

    /// Check if a query message is directed at us or matches our interests.
    fn is_query_for_us(&self, content: &serde_json::Value) -> bool {
        // Check explicit recipients list.
        if let Some(recipients) = content.get("recipients").and_then(|r| r.as_array()) {
            return recipients
                .iter()
                .any(|r| r.as_str() == Some(&self.identity_id));
        }

        // Canonical queries carry `question`; older peers used `body`.
        // Only consult the legacy field when the canonical field is absent.
        if let Some(question) = content
            .get("question")
            .and_then(|value| value.as_str())
            .or_else(|| content.get("body").and_then(|value| value.as_str()))
        {
            if question.contains(&self.identity_id) {
                return true;
            }
        }

        // Broadcast queries (no explicit recipients) — do NOT auto-respond.
        // Only respond when explicitly addressed or mentioned.
        // This prevents familiar from answering every question on the network.
        false
    }

    /// Check if a task_result relates to a task we published.
    fn is_our_task_result(&self, message: &serde_json::Value) -> bool {
        let relates_to = match self.result_task_id(message) {
            Some(hash) => hash,
            None => return false,
        };

        // Check our local published log.
        match Store::open(Path::new(&self.store_path)) {
            Ok(store) => store.has_published_hash(relates_to).unwrap_or(false),
            Err(e) => {
                tracing::warn!(error = %e, "failed to open store for task_result check");
                false
            }
        }
    }

    fn result_task_id<'a>(&self, message: &'a serde_json::Value) -> Option<&'a str> {
        message.get("relates").and_then(|r| r.as_str()).or_else(|| {
            message
                .get("content")
                .and_then(|c| c.get("task_id"))
                .and_then(|t| t.as_str())
        })
    }

    fn planner_basis_for_task(&self, task_id: &str) -> Result<Option<serde_json::Value>> {
        let store = Store::open(Path::new(&self.store_path))?;
        let metadata = match store.published_metadata(task_id)? {
            Some(metadata) => metadata,
            None => return Ok(None),
        };

        Ok(metadata
            .get("context")
            .and_then(|ctx| ctx.get("planner_basis"))
            .cloned())
    }

    async fn offer_matches_planner_basis(&mut self, task_id: &str, servitor: &str) -> Result<bool> {
        if !self.has_matching_servitor_profile(servitor).await? {
            return Ok(false);
        }

        let Some(planner_basis) = self.planner_basis_for_task(task_id)? else {
            return Ok(true);
        };

        let manifest_ref = planner_basis
            .get("manifest_ref")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let target_id = planner_basis
            .get("target_id")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let snapshot_ref = planner_basis
            .get("snapshot_ref")
            .and_then(|v| v.as_str())
            .map(str::to_string);

        if let Some(manifest_ref) = manifest_ref.as_deref() {
            let Some(profile) = self.servitor_profiles.get(servitor) else {
                return Ok(false);
            };
            if profile.manifest_ref.as_deref() != Some(manifest_ref) {
                return Ok(false);
            }

            if let Some(target_id) = target_id.as_deref() {
                if !self
                    .manifest_contains_target(servitor, manifest_ref, target_id)
                    .await?
                {
                    return Ok(false);
                }
            }
        }

        if let Some(snapshot_ref) = snapshot_ref.as_deref() {
            if !self
                .snapshot_matches(
                    servitor,
                    snapshot_ref,
                    manifest_ref.as_deref(),
                    target_id.as_deref(),
                )
                .await?
            {
                return Ok(false);
            }
        }

        Ok(true)
    }

    async fn has_matching_servitor_profile(&mut self, servitor: &str) -> Result<bool> {
        if let Some(profile) = self.servitor_profiles.get(servitor) {
            if profile.servitor_id == servitor {
                tracing::debug!(
                    servitor,
                    profile_author = %profile.author,
                    profile_timestamp = %profile.timestamp,
                    "using cached servitor_profile for verification"
                );
                return Ok(true);
            }
        }

        let messages = self
            .egregore
            .query_messages(Some(servitor), Some("servitor_profile"), None, None, 5)
            .await?;

        for message in messages {
            self.handle_servitor_profile(&message);
            if let Some(profile) = self.servitor_profiles.get(servitor) {
                if profile.servitor_id == servitor {
                    return Ok(true);
                }
            }
        }

        Ok(false)
    }

    async fn manifest_contains_target(
        &mut self,
        servitor: &str,
        manifest_ref: &str,
        target_id: &str,
    ) -> Result<bool> {
        if let Some(manifest) = self.servitor_manifests.get(manifest_ref) {
            return Ok(manifest.servitor_id == servitor
                && manifest.target_ids.iter().any(|target| target == target_id));
        }

        let messages = self
            .egregore
            .query_messages(Some(servitor), Some("servitor_manifest"), None, None, 10)
            .await?;
        for message in messages {
            self.handle_servitor_manifest(&message);
        }

        Ok(self
            .servitor_manifests
            .get(manifest_ref)
            .map(|manifest| {
                manifest.servitor_id == servitor
                    && manifest.target_ids.iter().any(|target| target == target_id)
            })
            .unwrap_or(false))
    }

    async fn snapshot_matches(
        &mut self,
        servitor: &str,
        snapshot_ref: &str,
        manifest_ref: Option<&str>,
        target_id: Option<&str>,
    ) -> Result<bool> {
        if let Some(snapshot) = self.environment_snapshots.get(snapshot_ref) {
            return Ok(snapshot.servitor_id == servitor
                && manifest_ref
                    .map(|value| snapshot.manifest_ref == value)
                    .unwrap_or(true)
                && target_id
                    .map(|value| snapshot.target_id == value)
                    .unwrap_or(true));
        }

        let messages = self
            .egregore
            .query_messages(Some(servitor), Some("environment_snapshot"), None, None, 10)
            .await?;
        for message in messages {
            self.handle_environment_snapshot(&message);
        }

        Ok(self
            .environment_snapshots
            .get(snapshot_ref)
            .map(|snapshot| {
                snapshot.servitor_id == servitor
                    && manifest_ref
                        .map(|value| snapshot.manifest_ref == value)
                        .unwrap_or(true)
                    && target_id
                        .map(|value| snapshot.target_id == value)
                        .unwrap_or(true)
            })
            .unwrap_or(false))
    }

    /// Handle an incoming query by running it through the conversation engine.
    async fn handle_query(&mut self, message: &serde_json::Value) -> Result<()> {
        let content = message
            .get("content")
            .ok_or_else(|| FamiliarError::Internal {
                reason: "query message missing content".into(),
            })?;

        let question = content
            .get("question")
            .and_then(|value| value.as_str())
            .or_else(|| content.get("body").and_then(|value| value.as_str()))
            .unwrap_or("");

        let author = message
            .get("author")
            .and_then(|a| a.as_str())
            .unwrap_or("unknown");

        let hash = message.get("hash").and_then(|h| h.as_str()).unwrap_or("");

        // Query linkage is a protocol security boundary. Reject malformed
        // hashes before untrusted content reaches even the untooled model.
        if !is_canonical_message_hash(hash) {
            tracing::warn!(hash, "ignoring query with malformed message hash");
            return Ok(());
        }

        // Ask only for opaque answer text. Trusted code below owns protocol
        // content construction and publication.
        let prompt = format!(
            "[Incoming network query from {author}]\n\
             Message hash: {hash}\n\n\
             {question}\n\n\
             Provide only the answer text."
        );

        tracing::info!(author, hash, "responding to query");

        match self.conversation.respond_untooled(&prompt).await {
            Ok((response, _usage)) => {
                let Some(content) = response_content(hash, &response) else {
                    tracing::warn!(hash, "skipping empty query response");
                    return Ok(());
                };

                match self
                    .egregore
                    .publish_related_content(content, &["response"], hash)
                    .await
                {
                    Ok(response_hash) => {
                        tracing::info!(hash, response_hash, "query response published");
                    }
                    Err(e) => {
                        tracing::error!(error = %e, hash, "failed to publish query response");
                    }
                }
                tracing::info!(
                    hash,
                    response_len = response.len(),
                    "query response generated"
                );
            }
            Err(e) => {
                tracing::error!(error = %e, hash, "failed to generate query response");
            }
        }

        Ok(())
    }

    /// Evaluate SSE triggers against a feed message.
    fn evaluate_sse_triggers(&self, message: &serde_json::Value) {
        // Trigger actions are proactive output — quiet hours apply here
        // exactly as they do to the heartbeat.
        let hour = chrono::Local::now().hour();
        if crate::heartbeat::is_quiet_hour(self.quiet_hours.0, self.quiet_hours.1, hour) {
            tracing::debug!(hour, "SSE trigger evaluation skipped (quiet hours)");
            return;
        }

        let content = match message.get("content") {
            Some(c) => c,
            None => return,
        };

        let msg_type = content.get("type").and_then(|t| t.as_str()).unwrap_or("");
        let author = message.get("author").and_then(|a| a.as_str()).unwrap_or("");

        // Build event fields from message for trigger matching
        let mut fields: Vec<(&str, &str)> = vec![("content_type", msg_type), ("author", author)];

        // Add status if present (common in task_result/task_failed)
        let status_str;
        if let Some(status) = content.get("status").and_then(|s| s.as_str()) {
            status_str = status.to_string();
            fields.push(("status", &status_str));
        }

        for trigger in &self.sse_triggers {
            if trigger.matches_event(&fields) {
                tracing::info!(
                    action = %trigger.action,
                    msg_type,
                    author,
                    "SSE trigger fired"
                );

                // Append to daily log via the canonical Workspace helper. Entry
                // is passed without a trailing \n; the helper appends one.
                let entry = format!(
                    "- [{}] trigger:{} fired (type={}, author={})",
                    chrono::Local::now().format("%H:%M"),
                    trigger.action,
                    msg_type,
                    author,
                );
                if let Err(e) = self.workspace.append_daily_log(&entry) {
                    tracing::warn!(error = %e, "failed to append SSE-trigger daily log entry");
                }
            }
        }
    }

    /// Handle an incoming task result — log it locally and optionally notify.
    async fn handle_task_result(&self, message: &serde_json::Value) -> Result<()> {
        let content = message.get("content").unwrap_or(&serde_json::Value::Null);
        let author = message
            .get("author")
            .and_then(|a| a.as_str())
            .unwrap_or("unknown");
        let relates = message
            .get("relates")
            .and_then(|r| r.as_str())
            .unwrap_or("unknown");

        let summary = content
            .get("summary")
            .or_else(|| content.get("body"))
            .and_then(|s| s.as_str())
            .unwrap_or("(no summary)");

        tracing::info!(author, relates, summary, "task result received");

        // Print to stdout for terminal visibility.
        println!(
            "\n[task result] from {} for task {}: {}",
            author, relates, summary
        );

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn begin(t: &mut TaskAssignmentTracker, task: &str, offer: &str, exec: &str) -> Option<String> {
        let at = chrono::Utc::now() + chrono::Duration::seconds(ASSIGN_COMMAND_TTL_SECS);
        t.begin_command(task, offer, exec, at).ok()
    }

    fn tracker_with_command() -> (TaskAssignmentTracker, &'static str) {
        let mut t = TaskAssignmentTracker::new();
        t.track("task-1".into());
        t.add_offer(
            "task-1",
            PendingOffer {
                servitor: "@exec".into(),
                withdrawn: false,
            },
        );
        let id = begin(&mut t, "task-1", "o", "@exec").unwrap();
        t.rename_command(&id, "cmd-1");
        t.mark_assigned("task-1");
        (t, "task-1")
    }

    #[test]
    fn rejected_result_reopens_task_for_another_offer() {
        let (mut t, task) = tracker_with_command();
        t.apply_assign_result(
            "@exec",
            &serde_json::json!({"type": "assign_task_result", "command_id": "cmd-1",
                "status": "rejected", "task_id": task, "reason": "stale_predecessor"}),
        );
        assert!(!t.is_assigned_or_completed(task));
        assert!(t.offers[task][0].withdrawn);
    }

    #[test]
    fn accepted_result_keeps_task_assigned() {
        let (mut t, task) = tracker_with_command();
        t.apply_assign_result(
            "@exec",
            &serde_json::json!({"command_id": "cmd-1", "status": "accepted"}),
        );
        assert!(t.is_assigned_or_completed(task));
    }

    #[test]
    fn result_from_other_author_or_unknown_command_is_ignored() {
        let (mut t, task) = tracker_with_command();
        t.apply_assign_result(
            "@impostor",
            &serde_json::json!({"command_id": "cmd-1", "status": "rejected"}),
        );
        t.apply_assign_result(
            "@exec",
            &serde_json::json!({"command_id": "cmd-other", "status": "rejected"}),
        );
        assert!(t.is_assigned_or_completed(task));
    }

    #[test]
    fn rejected_result_does_not_reopen_completed_task() {
        let (mut t, task) = tracker_with_command();
        t.mark_completed(task);
        t.apply_assign_result(
            "@exec",
            &serde_json::json!({"command_id": "cmd-1", "status": "rejected"}),
        );
        assert!(t.is_completed(task));
    }

    #[test]
    fn retry_of_same_selection_reuses_command_id() {
        let (mut t, task) = tracker_with_command();
        assert_eq!(begin(&mut t, task, "o", "@exec").as_deref(), Some("cmd-1"));
        assert_ne!(
            begin(&mut t, task, "other-offer", "@exec").as_deref(),
            Some("cmd-1")
        );
    }

    #[test]
    fn unresolved_commands_are_never_dropped_and_refuse_new_ones_at_the_bound() {
        let mut t = TaskAssignmentTracker::new();
        let at = chrono::Utc::now() + chrono::Duration::seconds(ASSIGN_COMMAND_TTL_SECS);
        let mut ids = Vec::new();
        for i in 0..MAX_TRACKED_COMMANDS {
            ids.push(
                t.begin_command(&format!("task-{i}"), "o", "@exec", at)
                    .unwrap(),
            );
        }
        assert_eq!(
            t.begin_command("extra", "o", "@exec", at),
            Err(BeginRefusal::AtCapacity)
        );
        // Retry of an existing selection is still served at the bound.
        assert_eq!(
            t.begin_command("task-0", "o", "@exec", at).as_ref(),
            Ok(&ids[0])
        );
        // A result frees capacity.
        t.apply_assign_result(
            "@exec",
            &serde_json::json!({"command_id": ids[0], "status": "accepted"}),
        );
        assert!(t.begin_command("extra", "o", "@exec", at).is_ok());
    }

    #[test]
    fn resolved_history_is_bounded() {
        let mut t = TaskAssignmentTracker::new();
        for i in 0..(MAX_TRACKED_COMMANDS + 50) {
            let id = begin(&mut t, &format!("task-{i}"), "o", "@exec").unwrap();
            t.apply_assign_result(
                "@exec",
                &serde_json::json!({"command_id": id, "status": "accepted"}),
            );
        }
        assert_eq!(t.commands.len(), MAX_TRACKED_COMMANDS);
        assert_eq!(t.selections.len(), MAX_TRACKED_COMMANDS);
        assert!(t.unresolved.is_empty());
    }

    #[test]
    fn unanswered_command_is_replayed_with_backoff_and_never_reopens_task() {
        let mut t = TaskAssignmentTracker::new();
        t.track("t".into());
        let id = begin(&mut t, "t", "o", "@exec").unwrap();
        t.mark_assigned("t");
        let ttl = chrono::Duration::seconds(ASSIGN_COMMAND_TTL_SECS);
        let grace = chrono::Duration::seconds(COMMAND_REPLAY_GRACE_SECS);
        let start = chrono::Utc::now();
        assert!(
            t.due_checks(start + ttl).is_empty(),
            "grace not yet elapsed"
        );
        let due = t.due_checks(start + ttl + grace + chrono::Duration::seconds(5));
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].command_id, id);
        assert!(t.is_assigned_or_completed("t"), "task stays assigned");
        // Immediately after a replay, backoff suppresses another.
        assert!(t
            .due_checks(start + ttl + grace + chrono::Duration::seconds(6))
            .is_empty());
        // Checks continue daily (capped backoff) but only the first
        // MAX_COMMAND_REPLAYS of them may publish; the task is never reopened.
        let mut now = start + ttl;
        let mut may_publish = 0;
        let mut checks = 0;
        for _ in 0..20 {
            now += chrono::Duration::days(1);
            for r in t.due_checks(now) {
                checks += 1;
                if r.may_publish {
                    may_publish += 1;
                    t.record_replay(&r.command_id);
                }
            }
        }
        assert_eq!(checks, 20, "lookups never stop");
        assert_eq!(may_publish, MAX_COMMAND_REPLAYS);
        assert!(t.is_assigned_or_completed("t"));
        // A late owner answer is still processed.
        t.apply_assign_result(
            "@exec",
            &serde_json::json!({"command_id": id, "status": "rejected", "reason": "unauthorized"}),
        );
        assert!(!t.is_assigned_or_completed("t"));
    }

    #[test]
    fn renewal_count_survives_offer_replay_after_exhaustion() {
        let mut t = TaskAssignmentTracker::new();
        t.track("t".into());
        t.add_offer(
            "t",
            PendingOffer {
                servitor: "@exec".into(),
                withdrawn: false,
            },
        );
        let at = chrono::Utc::now() + chrono::Duration::seconds(ASSIGN_COMMAND_TTL_SECS);
        let mut id = t.begin_command("t", "o", "@exec", at).unwrap();
        let mut renewed = 0;
        loop {
            t.mark_assigned("t");
            let r = t.apply_assign_result(
                "@exec",
                &serde_json::json!({"command_id": id, "status": "rejected", "reason": "expired"}),
            );
            if r.is_none() {
                break;
            }
            renewed += 1;
            id = t.begin_command("t", "o", "@exec", at).unwrap();
        }
        assert_eq!(renewed, MAX_ASSIGN_RENEWALS);
        assert_eq!(
            t.begin_command("t", "o", "@exec", at),
            Err(BeginRefusal::RenewalsExhausted)
        );
    }

    #[test]
    fn unresolved_command_survives_history_pressure_and_delayed_rejection_reopens() {
        let mut t = TaskAssignmentTracker::new();
        t.track("t".into());
        let a = begin(&mut t, "t", "oa", "@exec").unwrap();
        t.mark_assigned("t");
        // Cross the history bound with other commands that resolve.
        for i in 0..(MAX_TRACKED_COMMANDS + 50) {
            let task = format!("other-{i}");
            let id = begin(&mut t, &task, "o", "@exec").unwrap();
            t.apply_assign_result(
                "@exec",
                &serde_json::json!({"command_id": id, "status": "accepted"}),
            );
        }
        t.apply_assign_result(
            "@exec",
            &serde_json::json!({"command_id": a, "status": "rejected", "reason": "unauthorized"}),
        );
        assert!(
            !t.is_assigned_or_completed("t"),
            "delayed rejection must reopen"
        );
    }

    #[test]
    fn replayed_rejection_of_older_command_keeps_current_assignment() {
        let mut t = TaskAssignmentTracker::new();
        t.track("t".into());
        let a = begin(&mut t, "t", "oa", "@a").unwrap();
        t.apply_assign_result(
            "@a",
            &serde_json::json!({"command_id": a, "status": "rejected"}),
        );
        let b = begin(&mut t, "t", "ob", "@b").unwrap();
        t.mark_assigned("t");
        t.apply_assign_result(
            "@a",
            &serde_json::json!({"command_id": a, "status": "rejected"}),
        );
        assert!(t.is_assigned_or_completed("t"));
        assert_eq!(t.active.get("t"), Some(&b));
        assert_eq!(begin(&mut t, "t", "oa", "@a"), None);
    }

    #[test]
    fn completion_resolves_commands_and_is_never_overwritten() {
        let mut t = TaskAssignmentTracker::new();
        t.track("t".into());
        let id = begin(&mut t, "t", "o", "@exec").unwrap();
        t.mark_assigned("t");
        t.mark_completed("t");
        assert!(t.unresolved.is_empty());
        assert!(!t.active.contains_key("t"));
        t.mark_assigned("t");
        t.reopen("t");
        assert!(t.is_completed("t"));
        let later = chrono::Utc::now() + chrono::Duration::days(3);
        assert!(t.due_checks(later).is_empty());
        t.apply_assign_result(
            "@exec",
            &serde_json::json!({"command_id": id, "status": "rejected"}),
        );
        assert!(t.is_completed("t"));
    }

    #[test]
    fn feed_hash_shape() {
        assert!(is_feed_hash(&"a".repeat(64)));
        assert!(!is_feed_hash(&"A".repeat(64)));
        assert!(!is_feed_hash("task-1"));
        assert_eq!(random_hex().len(), 32);
        assert_ne!(random_hex(), random_hex());
    }
}
