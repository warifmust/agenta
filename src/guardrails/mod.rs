//! Guardrails — automated, always-on access policy for agents.
//!
//! Distinct from propose→approve (which gates *mutations* on a human decision):
//! guardrails fire on *every* run, with or without a human present, and decide
//! what an agent may *touch*. Together they're the two locks — a guardrail
//! confines the agent, propose→approve gates its changes.
//!
//! First guardrail: filesystem access ([`fs`]). More (Telegram sender allowlist,
//! command/destructive-action enforcement) will land as siblings — the shared
//! abstraction earns its way in once there are two or three, not before.

pub mod fs;
pub mod trust;

/// Master switch for guardrail *enforcement*. On by default; set `AGENTA_FS_GUARD=off`
/// (or `0`/`false`/`disabled`) to fall back to pre-guardrail behaviour. This is the
/// escape hatch for a default-deny feature: a decision is always *logged*, but when
/// this is off a Deny doesn't actually block — so a surprising breakage is a flag
/// flip, not a downgrade.
pub fn enforcement_enabled() -> bool {
    match std::env::var("AGENTA_FS_GUARD") {
        Ok(v) => !matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "off" | "0" | "false" | "no" | "disabled"
        ),
        Err(_) => true,
    }
}
