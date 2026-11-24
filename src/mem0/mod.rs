//! Native Mem0 long-term memory client.
//!
//! Provides automatic pre-turn recall (`POST /search`) and asynchronous post-turn
//! fact extraction (`POST /memories`) with hard bucket isolation:
//! - Private DMs use `user_id = "dm:<slack_user_id>"` (never cross-matched with
//!   other users' DMs or public channels).
//! - Channels use `user_id = "shared"` (shared team knowledge per agent).

use std::collections::HashSet;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use crate::core::{Mem0Config, Mem0Scope};

/// Runtime origin of a message, used to pick the right isolated Mem0 bucket.
#[derive(Debug, Clone, Default)]
pub struct Mem0Context {
    pub is_dm: bool,
    pub user_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Mem0Entry {
    #[serde(default)]
    pub id: String,
    pub memory: String,
    #[serde(default)]
    pub score: Option<f32>,
    #[serde(default)]
    pub user_id: Option<String>,
    #[serde(default)]
    pub agent_id: Option<String>,
}

pub struct Mem0Client {
    base_url: String,
    api_key: Option<String>,
    top_k: usize,
    min_score: f32,
    timeout: Duration,
    http: reqwest::Client,
}

impl Mem0Client {
    pub fn new(cfg: &Mem0Config) -> Self {
        let api_key = cfg.api_key.as_deref().map(resolve_env_ref).filter(|s| !s.is_empty());
        Self {
            base_url: cfg.url.trim_end_matches('/').to_string(),
            api_key,
            top_k: cfg.top_k.max(1),
            min_score: cfg.min_score,
            timeout: Duration::from_secs(cfg.timeout_secs.max(1)),
            http: reqwest::Client::new(),
        }
    }

    fn apply_auth(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        if let Some(ref key) = self.api_key {
            req.bearer_auth(key)
        } else {
            req
        }
    }

    /// Health check (`GET /health`).
    pub async fn health(&self) -> anyhow::Result<serde_json::Value> {
        let url = format!("{}/health", self.base_url);
        let resp = self
            .apply_auth(self.http.get(&url).timeout(self.timeout))
            .send()
            .await?;
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            anyhow::bail!("Mem0 health check failed ({}): {}", status, body);
        }
        Ok(serde_json::from_str(&body).unwrap_or_else(|_| serde_json::json!({ "status": "ok" })))
    }

    /// Search relevant memories for an agent turn and format them into a prompt block.
    /// Non-fatal: returns `None` on network errors or when no memories pass `min_score`.
    pub async fn recall_prompt_block(
        &self,
        agent_name: &str,
        scope: Mem0Scope,
        ctx: &Mem0Context,
        raw_input: &str,
    ) -> Option<String> {
        let query = strip_slack_context_wrapper(raw_input);
        if is_trivial_message(&query) {
            return None;
        }

        let buckets = resolve_read_buckets(scope, ctx);
        let mut combined: Vec<Mem0Entry> = Vec::new();
        let mut seen_ids: HashSet<String> = HashSet::new();

        for user_bucket in buckets {
            match self
                .search_bucket(agent_name, Some(&user_bucket), &query, self.top_k)
                .await
            {
                Ok(entries) => {
                    for e in entries {
                        if e.score.unwrap_or(1.0) < self.min_score {
                            continue;
                        }
                        if !e.id.is_empty() && !seen_ids.insert(e.id.clone()) {
                            continue;
                        }
                        combined.push(e);
                    }
                }
                Err(e) => {
                    warn!(
                        "[mem0] search failed for agent='{}' bucket='{}' (non-fatal): {}",
                        agent_name, user_bucket, e
                    );
                }
            }
        }

        if combined.is_empty() {
            return None;
        }

        // Highest similarity first, capped to top_k.
        combined.sort_by(|a, b| {
            b.score
                .unwrap_or(0.0)
                .partial_cmp(&a.score.unwrap_or(0.0))
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        combined.truncate(self.top_k);

        let lines: Vec<String> = combined
            .iter()
            .map(|e| format!("- {}", e.memory.trim()))
            .collect();

        Some(format!(
            "LONG-TERM MEMORY (background context from past conversations; always verify current ticket, PR, or doc statuses against live tools and treat live tools as the source of truth):\n{}\n\n",
            lines.join("\n")
        ))
    }

    /// Persist a completed user/assistant exchange to Mem0 (`POST /memories`).
    /// Intended to be called in a background `tokio::spawn` task so replies are never delayed.
    pub async fn remember_exchange(
        &self,
        agent_name: &str,
        scope: Mem0Scope,
        ctx: &Mem0Context,
        raw_user_input: &str,
        assistant_output: &str,
    ) {
        let user_msg = strip_slack_context_wrapper(raw_user_input);
        let assistant_msg = assistant_output.trim();

        if is_trivial_message(&user_msg) || assistant_msg.is_empty() {
            return;
        }

        let user_bucket = resolve_write_bucket(scope, ctx);
        let agent_id = normalize_entity_id(agent_name);
        let url = format!("{}/memories", self.base_url);
        let body = serde_json::json!({
            "messages": [
                { "role": "user", "content": user_msg },
                { "role": "assistant", "content": assistant_msg }
            ],
            "agent_id": agent_id,
            "user_id": user_bucket,
        });

        // Write calls invoke an LLM inside Mem0 for fact extraction, so allow up to 30s.
        let write_timeout = self.timeout.max(Duration::from_secs(30));
        match self
            .apply_auth(self.http.post(&url).json(&body).timeout(write_timeout))
            .send()
            .await
        {
            Ok(resp) if resp.status().is_success() => {
                debug!(
                    "[mem0] stored exchange for agent='{}' bucket='{}'",
                    agent_name, user_bucket
                );
            }
            Ok(resp) => {
                let status = resp.status();
                let err_body = resp.text().await.unwrap_or_default();
                warn!(
                    "[mem0] POST /memories failed ({}) for agent='{}': {}",
                    status,
                    agent_name,
                    &err_body[..err_body.len().min(200)]
                );
            }
            Err(e) => {
                warn!(
                    "[mem0] POST /memories error for agent='{}' (non-fatal): {}",
                    agent_name, e
                );
            }
        }
    }

    /// Raw search helper (also used by `agenta mem0 search`).
    pub async fn search_bucket(
        &self,
        agent_name: &str,
        user_id: Option<&str>,
        query: &str,
        limit: usize,
    ) -> anyhow::Result<Vec<Mem0Entry>> {
        let agent_id = normalize_entity_id(agent_name);
        let url = format!("{}/search", self.base_url);
        let mut body = serde_json::json!({
            "query": query,
            "agent_id": agent_id,
            "limit": limit,
        });
        if let Some(uid) = user_id {
            body["user_id"] = serde_json::Value::String(uid.to_string());
        }

        let resp = self
            .apply_auth(self.http.post(&url).json(&body).timeout(self.timeout))
            .send()
            .await?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            anyhow::bail!("Mem0 POST /search failed ({}): {}", status, text);
        }

        let val: serde_json::Value = serde_json::from_str(&text)?;
        Ok(parse_mem0_entries(&val))
    }

    /// List memories for an agent (and optional `user_id` bucket).
    pub async fn list_memories(
        &self,
        agent_name: &str,
        user_id: Option<&str>,
        limit: usize,
    ) -> anyhow::Result<Vec<Mem0Entry>> {
        let agent_id = normalize_entity_id(agent_name);
        let url = format!("{}/memories", self.base_url);
        let limit_str = limit.to_string();
        let mut query_params = vec![
            ("agent_id", agent_id.as_str()),
            ("limit", limit_str.as_str()),
        ];
        if let Some(uid) = user_id {
            query_params.push(("user_id", uid));
        }

        let resp = self
            .apply_auth(
                self.http
                    .get(&url)
                    .query(&query_params)
                    .timeout(self.timeout),
            )
            .send()
            .await?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            anyhow::bail!("Mem0 GET /memories failed ({}): {}", status, text);
        }

        let val: serde_json::Value = serde_json::from_str(&text)?;
        Ok(parse_mem0_entries(&val))
    }

    /// Clear memories for an agent (and optional `user_id` bucket).
    pub async fn clear_memories(
        &self,
        agent_name: &str,
        user_id: Option<&str>,
    ) -> anyhow::Result<()> {
        let agent_id = normalize_entity_id(agent_name);
        let url = format!("{}/memories", self.base_url);
        let mut query_params = vec![("agent_id", agent_id.as_str())];
        if let Some(uid) = user_id {
            query_params.push(("user_id", uid));
        }

        let resp = self
            .apply_auth(
                self.http
                    .delete(&url)
                    .query(&query_params)
                    .timeout(Duration::from_secs(15)),
            )
            .send()
            .await?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("Mem0 DELETE /memories failed ({}): {}", status, text);
        }
        Ok(())
    }
}

// ── Bucket isolation, stripping & parsing helpers ─────────────────────────────

/// Determine which `user_id` bucket(s) an agent may READ from.
/// Uses explicit `"dm:<uid>"` vs `"shared"` buckets so Qdrant filters never leak
/// private DM memories into channels or other users' DMs.
pub fn resolve_read_buckets(scope: Mem0Scope, ctx: &Mem0Context) -> Vec<String> {
    match scope {
        Mem0Scope::Agent => vec!["shared".to_string()],
        Mem0Scope::User => {
            let uid = ctx.user_id.as_deref().unwrap_or("default");
            vec![format!("dm:{}", uid)]
        }
        Mem0Scope::Auto => {
            if ctx.is_dm {
                let uid = ctx.user_id.as_deref().unwrap_or("default");
                // Private DM reads both its own private bucket and shared channel knowledge.
                vec![format!("dm:{}", uid), "shared".to_string()]
            } else {
                // Channels read ONLY the shared bucket — never any private dm:* bucket.
                vec!["shared".to_string()]
            }
        }
    }
}

/// Determine the single `user_id` bucket where new memories from this turn are WRITTEN.
pub fn resolve_write_bucket(scope: Mem0Scope, ctx: &Mem0Context) -> String {
    match scope {
        Mem0Scope::Agent => "shared".to_string(),
        Mem0Scope::User => {
            let uid = ctx.user_id.as_deref().unwrap_or("default");
            format!("dm:{}", uid)
        }
        Mem0Scope::Auto => {
            if ctx.is_dm {
                let uid = ctx.user_id.as_deref().unwrap_or("default");
                format!("dm:{}", uid)
            } else {
                "shared".to_string()
            }
        }
    }
}

/// Strip the prepended Slack thread/history wrapper so Mem0 only extracts facts
/// from the latest user message (instead of re-ingesting older history lines).
pub fn strip_slack_context_wrapper(raw: &str) -> String {
    // Format from slack_gateway:
    //   "[Slack DM recent messages]\n...\n\n[Slack DM — latest message from Alice]\n<actual text>"
    // or without history:
    //   "[Slack DM]\n<actual text>"
    for marker in ["latest message from "] {
        if let Some(pos) = raw.rfind(marker) {
            if let Some(nl) = raw[pos..].find('\n') {
                return raw[pos + nl + 1..].trim().to_string();
            }
        }
    }
    for prefix in ["[Slack DM]\n", "[Slack channel]\n"] {
        if let Some(rest) = raw.strip_prefix(prefix) {
            return rest.trim().to_string();
        }
    }
    raw.trim().to_string()
}

/// Skip trivial greetings / acknowledgments so Mem0 doesn't waste LLM calls or store noise.
pub fn is_trivial_message(msg: &str) -> bool {
    let cleaned = msg
        .trim()
        .trim_end_matches(|c: char| matches!(c, '.' | '!' | '?'))
        .to_ascii_lowercase();
    if cleaned.len() < 10 {
        return true;
    }
    matches!(
        cleaned.as_str(),
        "hey"
            | "hi"
            | "hello"
            | "hey bro"
            | "yo"
            | "thanks"
            | "thank you"
            | "tq"
            | "ok"
            | "okay"
            | "got it"
            | "noted"
            | "nice"
            | "cool"
    )
}

/// Parse Mem0 JSON responses from either `{"results": {"results": [...]}}`
/// (mem0ai v1 nested format) or `{"results": [...]}`.
pub fn parse_mem0_entries(val: &serde_json::Value) -> Vec<Mem0Entry> {
    let arr = val
        .get("results")
        .and_then(|r| {
            if r.is_array() {
                r.as_array()
            } else {
                r.get("results").and_then(|inner| inner.as_array())
            }
        })
        .or_else(|| val.as_array());

    match arr {
        Some(items) => items
            .iter()
            .filter_map(|item| serde_json::from_value::<Mem0Entry>(item.clone()).ok())
            .filter(|e| !e.memory.trim().is_empty())
            .collect(),
        None => Vec::new(),
    }
}

/// Mem0 rejects whitespace in `agent_id` / `user_id`. Normalize names like
/// `"Kaydee AI"` into `"kaydee-ai"` consistently across read/write/CLI paths.
pub fn normalize_entity_id(raw: &str) -> String {
    raw.split_whitespace()
        .collect::<Vec<_>>()
        .join("-")
        .to_ascii_lowercase()
}

fn resolve_env_ref(raw: &str) -> String {
    if let Some(var) = raw.strip_prefix('$') {
        std::env::var(var).unwrap_or_default()
    } else {
        raw.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn auto_scope_isolates_dms_and_shares_channels() {
        let arif_dm = Mem0Context {
            is_dm: true,
            user_id: Some("U_ARIF".into()),
        };
        let lillian_dm = Mem0Context {
            is_dm: true,
            user_id: Some("U_LILLIAN".into()),
        };
        let channel = Mem0Context {
            is_dm: false,
            user_id: Some("U_ARIF".into()),
        };

        assert_eq!(resolve_read_buckets(Mem0Scope::Auto, &arif_dm), vec!["dm:U_ARIF", "shared"]);
        assert_eq!(resolve_write_bucket(Mem0Scope::Auto, &arif_dm), "dm:U_ARIF");

        assert_eq!(
            resolve_read_buckets(Mem0Scope::Auto, &lillian_dm),
            vec!["dm:U_LILLIAN", "shared"]
        );
        assert_eq!(resolve_write_bucket(Mem0Scope::Auto, &lillian_dm), "dm:U_LILLIAN");

        // Channel never reads or writes any dm:* bucket
        assert_eq!(resolve_read_buckets(Mem0Scope::Auto, &channel), vec!["shared"]);
        assert_eq!(resolve_write_bucket(Mem0Scope::Auto, &channel), "shared");
    }

    #[test]
    fn strips_slack_history_wrapper_cleanly() {
        let wrapped = "[Slack DM recent messages]\nArif: Hi\nAssistant: Hey\n\n[Slack DM — latest message from Arif]\nSyaza handles AD smoke tests";
        assert_eq!(strip_slack_context_wrapper(wrapped), "Syaza handles AD smoke tests");

        let bare = "[Slack channel]\nWhat is the sprint version?";
        assert_eq!(strip_slack_context_wrapper(bare), "What is the sprint version?");
    }

    #[test]
    fn trivial_messages_are_skipped() {
        assert!(is_trivial_message("Hey"));
        assert!(is_trivial_message("Hey bro."));
        assert!(is_trivial_message("Thanks!"));
        assert!(!is_trivial_message("Syaza handles AD smoke tests"));
    }

    #[test]
    fn normalizes_entity_ids_without_whitespace() {
        assert_eq!(normalize_entity_id("Kaydee AI"), "kaydee-ai");
        assert_eq!(normalize_entity_id("  My   Agent  "), "my-agent");
    }

    #[test]
    fn parses_nested_and_flat_mem0_results() {
        let nested = json!({
            "results": {
                "results": [
                    { "id": "1", "memory": "Syaza handles AD smoke tests", "score": 0.62 }
                ]
            }
        });
        let entries = parse_mem0_entries(&nested);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].memory, "Syaza handles AD smoke tests");
        assert_eq!(entries[0].score, Some(0.62));

        let flat = json!({
            "results": [
                { "id": "2", "memory": "Arif owns AD and AE", "score": 0.51 }
            ]
        });
        assert_eq!(parse_mem0_entries(&flat).len(), 1);
    }
}
