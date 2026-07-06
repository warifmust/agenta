use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use super::agent::{Agent, SideEffect, ToolResource};

/// How risky applying a proposal is. Drives how much friction the approval step
/// gets: `Low` can be auto-approved in trust-mode; `Destructive` is always gated.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, ToSchema, Default)]
#[serde(rename_all = "snake_case")]
pub enum Risk {
    /// Reversible and touches nothing sensitive (e.g. create a read-only tool).
    #[default]
    Low,
    /// Reaches a secret or an external system (e.g. a tool that reads a token).
    Elevated,
    /// Irreversible / high blast radius (e.g. delete an agent).
    Destructive,
}

/// Lifecycle of a proposal.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, ToSchema, Default)]
#[serde(rename_all = "snake_case")]
pub enum ProposalStatus {
    /// Awaiting a human decision.
    #[default]
    Pending,
    /// Rejected by the user; never applied.
    Rejected,
    /// Approved and applied successfully.
    Applied,
    /// Approved but the apply failed (see `result`).
    Failed,
}

/// The concrete mutation a proposal will perform when approved. Tagged so the
/// apply engine can dispatch, and so the payload rides along as structured data.
/// New builder capabilities add variants here (CreateAgent, AttachKb, DeleteAgent…).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(tag = "action", content = "payload", rename_all = "snake_case")]
pub enum ProposalAction {
    /// Create a first-class tool. Payload is the tool to create.
    CreateTool(ToolResource),
    /// Create an agent. Payload is the agent to create.
    CreateAgent(Agent),
}

impl ProposalAction {
    /// Short human label for lists.
    pub fn summary(&self) -> String {
        match self {
            ProposalAction::CreateTool(t) => format!("create tool '{}'", t.name),
            ProposalAction::CreateAgent(a) => format!("create agent '{}'", a.name),
        }
    }

    /// Classify the blast radius of applying this action.
    pub fn risk(&self) -> Risk {
        match self {
            ProposalAction::CreateTool(t) => match t.side_effect {
                SideEffect::Destructive => Risk::Destructive,
                SideEffect::Write => Risk::Elevated,
                // A read-only tool that can still read a secret is elevated.
                SideEffect::ReadOnly if !t.secrets.is_empty() => Risk::Elevated,
                SideEffect::ReadOnly => Risk::Low,
            },
            // Creating an agent is reversible and inert until run; its tools are
            // themselves gated. Low risk.
            ProposalAction::CreateAgent(_) => Risk::Low,
        }
    }
}

/// A human-gated mutation drafted by an agent (MIND). Agents never mutate the
/// system directly — they create a Proposal, and a human approves it before the
/// apply engine runs the underlying CRUD.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct Proposal {
    pub id: String,
    pub action: ProposalAction,
    /// The agent's justification — shown to the user at approval time.
    pub rationale: String,
    pub risk: Risk,
    pub status: ProposalStatus,
    /// Agent id (or name) that drafted this.
    pub proposed_by: String,
    pub created_at: DateTime<Utc>,
    pub resolved_at: Option<DateTime<Utc>>,
    /// Apply outcome or error, set once resolved.
    pub result: Option<String>,
}

impl Proposal {
    pub fn new(action: ProposalAction, rationale: String, proposed_by: String) -> Self {
        let risk = action.risk();
        Self {
            id: Uuid::new_v4().to_string(),
            action,
            rationale,
            risk,
            status: ProposalStatus::Pending,
            proposed_by,
            created_at: Utc::now(),
            resolved_at: None,
            result: None,
        }
    }

    /// One-line summary of what this proposal does (for list views).
    pub fn summary(&self) -> String {
        self.action.summary()
    }
}
