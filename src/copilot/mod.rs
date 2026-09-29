//! GitHub Copilot CLI: explanations through its headless server (the Copilot
//! SDK's JSON-RPC), the conversation from its session's `events.jsonl`, and
//! which `copilot` commands get peekme. Its full-screen selection arrives as
//! OSC 52 (see `osc52.rs`) and its input box looks like Claude Code's.

pub mod cli;
pub mod explain;
pub mod transcript;
