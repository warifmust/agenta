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
