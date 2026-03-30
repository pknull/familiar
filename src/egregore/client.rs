//! Egregore publish client — HTTP client for publishing and querying.

use reqwest::Client;
use serde::{Deserialize, Serialize};

use crate::error::{FamiliarError, Result};

#[derive(Clone)]
pub struct EgregoreClient {
    http: Client,
    api_url: String,
    api_token: Option<String>,
}

impl EgregoreClient {
    pub fn new(api_url: &str, api_token: Option<String>) -> Self {
        Self {
            http: Client::new(),
            api_url: api_url.trim_end_matches('/').to_string(),
            api_token,
        }
    }

    /// Add auth header to a request builder if a token is configured.
    fn auth(&self, builder: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        if let Some(ref token) = self.api_token {
            builder.bearer_auth(token)
        } else {
            builder
        }
    }

    pub fn api_url(&self) -> &str {
        &self.api_url
    }

    /// Check if this client has an auth token configured.
    pub fn has_auth_token(&self) -> bool {
        self.api_token.is_some()
    }

    /// Check if egregore requires auth by probing the API.
    /// Returns true if a 401 is received without a token.
    pub async fn requires_auth(&self) -> bool {
        if self.api_token.is_some() {
            return false; // We have a token, no problem
        }
        let url = format!("{}/v1/status", self.api_url);
        match self.http.get(&url).send().await {
            Ok(response) => response.status() == reqwest::StatusCode::UNAUTHORIZED,
            Err(_) => false, // Can't reach daemon — not an auth issue
        }
    }

    /// Publish arbitrary content to your egregore feed.
    pub async fn publish_content(
        &self,
        content: serde_json::Value,
        tags: &[&str],
    ) -> Result<String> {
        // Preemptive auth check: refuse to publish if daemon requires auth and no token configured
        if self.api_token.is_none() {
            if self.requires_auth().await {
                return Err(FamiliarError::Egregore {
                    reason: "egregore API requires authentication but no api_token is configured in familiar.toml".into(),
                });
            }
        }

        let response = self.publish_raw(content, tags, None, None).await?;
        Ok(response.hash)
    }

    /// Publish with full control over trace metadata.
    async fn publish_raw(
        &self,
        content: serde_json::Value,
        tags: &[&str],
        trace_id: Option<&str>,
        span_id: Option<&str>,
    ) -> Result<PublishedMessage> {
        let url = format!("{}/v1/publish", self.api_url);

        let request = PublishRequest {
            content,
            tags: tags.iter().map(|s| s.to_string()).collect(),
            trace_id: trace_id.map(str::to_string),
            span_id: span_id.map(str::to_string),
        };

        let response = self
            .auth(self.http.post(&url))
            .json(&request)
            .send()
            .await
            .map_err(|e| FamiliarError::Egregore {
                reason: format!("publish request failed: {}", e),
            })?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(FamiliarError::Egregore {
                reason: format!("publish failed with {}: {}", status, body),
            });
        }

        let envelope: ApiResponse<PublishedMessage> =
            response.json().await.map_err(|e| FamiliarError::Egregore {
                reason: format!("failed to parse publish response: {}", e),
            })?;

        envelope.data.ok_or_else(|| FamiliarError::Egregore {
            reason: "publish response missing data field".into(),
        })
    }

    /// Query messages from egregore feeds.
    pub async fn query_messages(
        &self,
        author: Option<&str>,
        content_type: Option<&str>,
        tag: Option<&str>,
        search: Option<&str>,
        limit: usize,
    ) -> Result<Vec<serde_json::Value>> {
        let mut url = format!("{}/v1/feed?limit={}", self.api_url, limit);
        if let Some(a) = author {
            url.push_str(&format!("&author={}", a));
        }
        if let Some(ct) = content_type {
            url.push_str(&format!("&content_type={}", ct));
        }
        if let Some(t) = tag {
            url.push_str(&format!("&tag={}", t));
        }
        if let Some(s) = search {
            url.push_str(&format!("&search={}", urlencoding::encode(s)));
        }

        let response = self
            .auth(self.http.get(&url))
            .send()
            .await
            .map_err(|e| FamiliarError::Egregore {
                reason: format!("query request failed: {}", e),
            })?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(FamiliarError::Egregore {
                reason: format!("query failed with {}: {}", status, body),
            });
        }

        let envelope: ApiResponse<Vec<serde_json::Value>> =
            response.json().await.map_err(|e| FamiliarError::Egregore {
                reason: format!("failed to parse query response: {}", e),
            })?;

        Ok(envelope.data.unwrap_or_default())
    }

    /// Check if egregore daemon is reachable.
    pub async fn health_check(&self) -> Result<bool> {
        let url = format!("{}/v1/status", self.api_url);
        match self.http.get(&url).send().await {
            Ok(response) => Ok(response.status().is_success()),
            Err(_) => Ok(false),
        }
    }
}

#[derive(Debug, Serialize)]
struct PublishRequest {
    content: serde_json::Value,
    tags: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    trace_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    span_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ApiResponse<T> {
    #[allow(dead_code)]
    success: bool,
    data: Option<T>,
    #[allow(dead_code)]
    error: Option<ApiError>,
}

#[derive(Debug, Deserialize)]
struct ApiError {
    #[allow(dead_code)]
    code: String,
    #[allow(dead_code)]
    message: String,
}

#[derive(Debug, Deserialize)]
struct PublishedMessage {
    hash: String,
    #[allow(dead_code)]
    sequence: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_creation() {
        let client = EgregoreClient::new("http://127.0.0.1:7654", None);
        assert_eq!(client.api_url, "http://127.0.0.1:7654");
    }

    #[test]
    fn client_trims_trailing_slash() {
        let client = EgregoreClient::new("http://127.0.0.1:7654/", None);
        assert_eq!(client.api_url, "http://127.0.0.1:7654");
    }
}
