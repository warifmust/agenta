use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// A durable piece of user feedback / preference that shapes an agent's behavior
/// on future runs — "corrective memory" (AQL §16). Stored per-agent and injected
/// into the agent's context at run time. Distinct from RAG knowledge bases: this
/// is a small set of user-editable rules, not a retrieved corpus.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Memory {
    pub id: String,
    /// Agent this applies to, by name (e.g. "MIND").
    pub scope: String,
    /// Free-form category: "preference" | "correction" | "note". Defaults to "note".
    pub kind: String,
    pub content: String,
    /// Inactive memories are kept for history but not injected.
    pub active: bool,
    pub created_at: DateTime<Utc>,
}

impl Memory {
    pub fn new(
        scope: impl Into<String>,
        kind: impl Into<String>,
        content: impl Into<String>,
    ) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            scope: scope.into(),
            kind: kind.into(),
            content: content.into(),
            active: true,
            created_at: Utc::now(),
        }
    }
}
