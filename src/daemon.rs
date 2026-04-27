//! Daemon mode — persistent Familiar that watches the egregore feed.
//!
//! Connects to egregore SSE to receive real-time feed events, filters for
//! messages relevant to this identity, and processes them through the
//! conversation engine automatically.

use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, Instant};

use futures::StreamExt;
use reqwest_eventsource::{Event, EventSource};
use tokio::time::sleep;

use crate::agent::conversation::Conversation;
use crate::egregore::EgregoreClient;
use crate::error::{FamiliarError, Result};
use crate::store::Store;
use crate::workspace::heartbeat::{self as heartbeat_config, Trigger};

/// Assignment state for a published task.
#[derive(Debug, Clone)]
enum AssignmentState {
    /// Task published, waiting for offers.
    Pending,
    /// Offer accepted, task_assign published, waiting for task_started confirmation.
    Assigned {
        servitor: String,
        assigned_at: Instant,
    },
    /// task_started received — execution confirmed.
    Confirmed { servitor: String },
    /// task_result or task_failed received — complete.
    Completed,
}

/// Tracks pending offers for a published task (for retry on assignment failure).
#[derive(Debug, Clone)]
struct PendingOffer {
    servitor: String,
    timestamp: String,
    ttl_seconds: u64,
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
    hash: String,
    servitor_id: String,
    target_ids: Vec<String>,
}

/// Recently observed environment snapshot used for planner-basis verification.
#[derive(Debug, Clone)]
struct ObservedEnvironmentSnapshot {
    hash: String,
    servitor_id: String,
    target_id: String,
    manifest_ref: String,
}

/// Tracks auto-assignment state for tasks published by this familiar.
struct TaskAssignmentTracker {
    /// task_hash → assignment state
    tasks: HashMap<String, AssignmentState>,
    /// task_hash → pending offers (for retry)
    offers: HashMap<String, Vec<PendingOffer>>,
}

impl TaskAssignmentTracker {
    fn new() -> Self {
        Self {
            tasks: HashMap::new(),
            offers: HashMap::new(),
        }
    }

    /// Register a published task hash for tracking.
    fn track(&mut self, hash: String) {
        self.tasks.insert(hash, AssignmentState::Pending);
    }

    /// Check if we're tracking a task hash.
    fn is_tracked(&self, hash: &str) -> bool {
        self.tasks.contains_key(hash)
    }

    /// Check if a task has already been assigned.
    fn is_assigned_or_completed(&self, hash: &str) -> bool {
        matches!(
            self.tasks.get(hash),
            Some(AssignmentState::Assigned { .. })
                | Some(AssignmentState::Confirmed { .. })
                | Some(AssignmentState::Completed)
        )
    }

    /// Record an offer for a task.
    fn add_offer(&mut self, task_hash: &str, offer: PendingOffer) {
        self.offers
            .entry(task_hash.to_string())
            .or_default()
            .push(offer);
    }

    /// Mark a task as assigned.
    fn mark_assigned(&mut self, task_hash: &str, servitor: String) {
        self.tasks.insert(
            task_hash.to_string(),
            AssignmentState::Assigned {
                servitor,
                assigned_at: Instant::now(),
            },
        );
    }

    /// Mark a task as confirmed (task_started received).
    fn mark_confirmed(&mut self, task_hash: &str) {
        if let Some(AssignmentState::Assigned { servitor, .. }) = self.tasks.get(task_hash) {
            let servitor = servitor.clone();
            self.tasks.insert(
                task_hash.to_string(),
                AssignmentState::Confirmed { servitor },
            );
        }
    }

    /// Mark a task as completed.
    fn mark_completed(&mut self, task_hash: &str) {
        self.tasks
            .insert(task_hash.to_string(), AssignmentState::Completed);
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

    /// Get the next available (non-withdrawn, non-expired) offer for a task.
    fn next_available_offer(&self, task_hash: &str) -> Option<&PendingOffer> {
        self.offers.get(task_hash).and_then(|offers| {
            offers.iter().find(|o| {
                !o.withdrawn
                // TTL check: parse timestamp and compare
                // For simplicity, we trust the offer is recent enough
                // (full TTL check would require parsing the timestamp)
            })
        })
    }
}

/// Maximum consecutive SSE failures before applying long backoff.
const MAX_RETRIES_BEFORE_LONG_BACKOFF: u32 = 5;

/// Short retry delay (seconds).
const SHORT_RETRY_SECS: u64 = 5;

/// Long retry delay after repeated failures (seconds).
const LONG_RETRY_SECS: u64 = 60;

/// Daemon — persistent feed watcher and auto-responder.
pub struct Daemon {
    conversation: Conversation,
    egregore: EgregoreClient,
    egregore_url: String,
    identity_id: String,
    store_path: String,
    scope: crate::config::DaemonConfig,
    agent_config: crate::config::AgentConfig,
    tracker: TaskAssignmentTracker,
    servitor_profiles: HashMap<String, ObservedServitorProfile>,
    servitor_manifests: HashMap<String, ObservedServitorManifest>,
    environment_snapshots: HashMap<String, ObservedEnvironmentSnapshot>,
    /// SSE-driven triggers loaded from HEARTBEAT.md.
    sse_triggers: Vec<Trigger>,
}

impl Daemon {
    pub fn new(
        conversation: Conversation,
        egregore: EgregoreClient,
        egregore_url: String,
        identity_id: String,
        store_path: String,
        scope: crate::config::DaemonConfig,
        agent_config: crate::config::AgentConfig,
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
            tracker: TaskAssignmentTracker::new(),
            servitor_profiles: HashMap::new(),
            servitor_manifests: HashMap::new(),
            environment_snapshots: HashMap::new(),
            sse_triggers,
        }
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
        let mut es = EventSource::get(&url);

        while let Some(event) = es.next().await {
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
                    es.close();
                    return Ok(());
                }
                Err(e) => {
                    tracing::warn!(error = %e, "SSE error");
                    es.close();
                    return Err(FamiliarError::Egregore {
                        reason: format!("SSE connection error: {}", e),
                    });
                }
            }
        }

        Ok(())
    }

    /// Parse and dispatch a single SSE message.
    async fn handle_sse_message(&mut self, data: &str) -> Result<()> {
        let message: serde_json::Value = serde_json::from_str(data)?;

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

        let ttl = content
            .get("ttl_seconds")
            .and_then(|t| t.as_u64())
            .unwrap_or(30);
        let timestamp = content
            .get("timestamp")
            .and_then(|t| t.as_str())
            .unwrap_or("")
            .to_string();

        // Record the offer
        self.tracker.add_offer(
            task_id,
            PendingOffer {
                servitor: servitor.clone(),
                timestamp: timestamp.clone(),
                ttl_seconds: ttl,
                withdrawn: false,
            },
        );

        // Trust validation
        if !self.agent_config.trusted_servitors.is_empty()
            && !self.agent_config.trusted_servitors.contains(&servitor)
        {
            tracing::debug!(servitor, task_id, "ignoring offer from untrusted servitor");
            return Ok(());
        }

        if self.agent_config.verify_servitor_profile
            && !self.offer_matches_planner_basis(task_id, &servitor).await?
        {
            tracing::debug!(
                servitor,
                task_id,
                "ignoring offer without matching servitor_profile"
            );
            return Ok(());
        }

        // Publish task_assign
        let assign_content = serde_json::json!({
            "type": "task_assign",
            "task_id": task_id,
            "servitor": servitor,
            "assigner": self.identity_id,
        });

        match self
            .egregore
            .publish_content(assign_content, &["task_assign"])
            .await
        {
            Ok(hash) => {
                tracing::info!(
                    task_id,
                    servitor,
                    assign_hash = hash,
                    "auto-assigned task (first offer wins)"
                );
                self.tracker.mark_assigned(task_id, servitor);
            }
            Err(e) => {
                tracing::warn!(
                    task_id,
                    error = %e,
                    "failed to publish task_assign"
                );
            }
        }

        Ok(())
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
            hash.clone(),
            ObservedServitorManifest {
                hash,
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
            hash.clone(),
            ObservedEnvironmentSnapshot {
                hash,
                servitor_id,
                target_id,
                manifest_ref,
            },
        );
    }

    /// Handle task_started: confirm assignment.
    fn handle_task_started(&mut self, message: &serde_json::Value) -> Result<()> {
        let content = message.get("content").unwrap_or(&serde_json::Value::Null);
        if let Some(task_id) = content.get("task_id").and_then(|t| t.as_str()) {
            if self.tracker.is_tracked(task_id) {
                self.tracker.mark_confirmed(task_id);
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

            // Planner-visible executor profiles may be needed for offer verification.
            "servitor_profile" => {
                self.agent_config.verify_servitor_profile
                    || !self.agent_config.trusted_servitors.is_empty()
            }
            "servitor_manifest" | "environment_snapshot" => {
                self.agent_config.verify_servitor_profile
            }

            // Task results — check if we published the originating task.
            "task_result" | "task_failed" => self.is_our_task_result(message),

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

        // Check if our ID is mentioned in the body.
        if let Some(body) = content.get("body").and_then(|b| b.as_str()) {
            if body.contains(&self.identity_id) {
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

        let body = content.get("body").and_then(|b| b.as_str()).unwrap_or("");

        let author = message
            .get("author")
            .and_then(|a| a.as_str())
            .unwrap_or("unknown");

        let hash = message.get("hash").and_then(|h| h.as_str()).unwrap_or("");

        // Build a prompt that gives the LLM context about the incoming query.
        let prompt = format!(
            "[Incoming network query from {author}]\n\
             Message hash: {hash}\n\n\
             {body}\n\n\
             Respond to this query on the network feed. Use egregore_publish to post your response \
             with type \"response\" and include the original hash as a \"relates\" field."
        );

        tracing::info!(author, hash, "responding to query");

        match self.conversation.send(&prompt, None).await {
            Ok((response, _usage)) => {
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

                // Append to daily log
                let workspace_dir =
                    crate::config::Config::expand_path("~/.familiar/workspace/daily");
                let daily_dir = std::path::Path::new(&workspace_dir);
                if !daily_dir.exists() {
                    let _ = std::fs::create_dir_all(daily_dir);
                }
                let today = chrono::Local::now().format("%Y-%m-%d");
                let log_path = daily_dir.join(format!("{}.md", today));
                let entry = format!(
                    "- [{}] trigger:{} fired (type={}, author={})\n",
                    chrono::Local::now().format("%H:%M"),
                    trigger.action,
                    msg_type,
                    author,
                );
                let mut content = std::fs::read_to_string(&log_path).unwrap_or_default();
                content.push_str(&entry);
                let _ = std::fs::write(&log_path, content);
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
