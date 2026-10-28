//! aycod: guardrails around any AI coding agent. The checks decide, not the agent.
#![forbid(unsafe_code)]

pub mod agents;
pub mod check;
pub mod config;
pub mod githooks;
pub mod install;
pub mod policy;
pub mod shell;
