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
    /// Attach a knowledge base to an existing agent (by names).
    AttachKb { agent: String, kb: String },
    /// Detach a knowledge base from an existing agent (by names).
    DetachKb { agent: String, kb: String },
    /// Revise an existing agent in place. Only the fields present are changed —
    /// deliberately limited to prompt/description/model so a proposal can refine
    /// an agent but never destroy one (there is no delete variant by design).
    UpdateAgent {
        agent: String,
        system_prompt: Option<String>,
        description: Option<String>,
        model: Option<String>,
    },
    /// Replace an existing tool's definition. Payload is the full new tool; it
    /// keeps the old tool's identity and is pushed out to agents using it.
    /// `previous_name` is how we find those agents when the tool is renamed.
    UpdateTool {
        previous_name: String,
        tool: ToolResource,
    },
}

impl ProposalAction {
    /// Short human label for lists.
    pub fn summary(&self) -> String {
        match self {
            ProposalAction::CreateTool(t) => format!("create tool '{}'", t.name),
            ProposalAction::CreateAgent(a) => format!("create agent '{}'", a.name),
            ProposalAction::AttachKb { agent, kb } => format!("attach kb '{}' to '{}'", kb, agent),
            ProposalAction::DetachKb { agent, kb } => format!("detach kb '{}' from '{}'", kb, agent),
            ProposalAction::UpdateAgent { agent, .. } => format!("update agent '{}'", agent),
            ProposalAction::UpdateTool { tool, .. } => format!("update tool '{}'", tool.name),
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
            // Attaching/detaching a KB only changes what context an agent retrieves;
            // fully reversible. Low risk.
            ProposalAction::AttachKb { .. } | ProposalAction::DetachKb { .. } => Risk::Low,
            // Rewriting an agent's prompt changes how it behaves on the next run,
            // and the old wording is gone once applied — worth a second look, but
            // it can't touch tools, data, or the agent's existence.
            ProposalAction::UpdateAgent { .. } => Risk::Elevated,
            // A handler is executable code, and this rewrites a tool you already
            // approved and attached to agents — a read-only tool could be quietly
            // turned into something that writes. Rate it on the NEW definition and
            // never below Elevated: silently re-trusting an existing tool is the
            // thing to avoid.
            ProposalAction::UpdateTool { tool, .. } => match tool.side_effect {
                SideEffect::Destructive => Risk::Destructive,
                _ => Risk::Elevated,
            },
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
