//! Claude Code: explanations through `claude -p`, the conversation from its
//! transcript, its input area, and which `claude` commands get peekme. Its
//! full-screen selection arrives as OSC 52 (see `osc52.rs`).

pub mod cli;
pub mod explain;
pub mod screen;
pub mod transcript;
