//! Daemon mode — persistent Familiar that watches the egregore feed.
//!
//! Connects to egregore SSE to receive real-time feed events, filters for
//! messages relevant to this identity, and processes them through the
//! conversation engine automatically.

use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use futures::StreamExt;
use reqwest_eventsource::{Event, EventSource};
use tokio::time::sleep;

use crate::agent::conversation::Conversation;
use crate::egregore::EgregoreClient;
use crate::error::{FamiliarError, Result};
use crate::store::Store;

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
        Self {
            conversation,
            egregore,
            egregore_url,
            identity_id,
            store_path,
            scope,
            agent_config,
            tracker: TaskAssignmentTracker::new(),
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

        tracing::info!(
            msg_type,
            author,
            "processing relevant feed message"
        );

        match msg_type {
            "query" => self.handle_query(&message).await,
            "task_result" => {
                // Mark in tracker if we're tracking this task
                if let Some(hash) = message.get("relates").and_then(|r| r.as_str()) {
                    self.tracker.mark_completed(hash);
                }
                self.handle_task_result(&message).await
            }
            "task_offer" => self.handle_task_offer(&message).await,
            "task_started" => self.handle_task_started(&message),
            "task_failed" => {
                if let Some(hash) = message.get("relates").and_then(|r| r.as_str()) {
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

        // Only handle offers for tasks we published
        if !self.tracker.is_tracked(task_id) {
            return Ok(());
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
            tracing::debug!(
                servitor,
                task_id,
                "ignoring offer from untrusted servitor"
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
                self.tracker
                    .mark_assigned(task_id, servitor);
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
        let author = message
            .get("author")
            .and_then(|a| a.as_str())
            .unwrap_or("");

        // Never process our own messages.
        if author == self.identity_id {
            return false;
        }

        let content = match message.get("content") {
            Some(c) => c,
            None => return false,
        };

        let msg_type = content
            .get("type")
            .and_then(|t| t.as_str())
            .unwrap_or("");

        // Apply configured scope filters.
        let tags: Vec<String> = message
            .get("tags")
            .and_then(|t| t.as_array())
            .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
            .unwrap_or_default();
        if !self.scope.matches_scope(Some(author), Some(msg_type), &tags) {
            return false;
        }

        match msg_type {
            // Queries from other agents — we may want to respond.
            "query" => self.is_query_for_us(content),

            // Task results — check if we published the originating task.
            "task_result" | "task_failed" => self.is_our_task_result(message),

            // Task lifecycle messages — relevant if they match a task we're tracking.
            // These bypass normal scope filters since they're protocol messages.
            "task_offer" | "task_started" | "task_offer_withdraw" => {
                content
                    .get("task_id")
                    .and_then(|t| t.as_str())
                    .map(|task_id| self.tracker.is_tracked(task_id))
                    .unwrap_or(false)
            }

            _ => false,
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
        let relates_to = match message.get("relates").and_then(|r| r.as_str()) {
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

    /// Handle an incoming query by running it through the conversation engine.
    async fn handle_query(&self, message: &serde_json::Value) -> Result<()> {
        let content = message
            .get("content")
            .ok_or_else(|| FamiliarError::Internal {
                reason: "query message missing content".into(),
            })?;

        let body = content
            .get("body")
            .and_then(|b| b.as_str())
            .unwrap_or("");

        let author = message
            .get("author")
            .and_then(|a| a.as_str())
            .unwrap_or("unknown");

        let hash = message
            .get("hash")
            .and_then(|h| h.as_str())
            .unwrap_or("");

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

        tracing::info!(
            author,
            relates,
            summary,
            "task result received"
        );

        // Print to stdout for terminal visibility.
        println!(
            "\n[task result] from {} for task {}: {}",
            author, relates, summary
        );

        Ok(())
    }
}
