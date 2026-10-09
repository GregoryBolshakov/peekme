//! Kiro CLI: explanations through its Agent Client Protocol server, the
//! conversation from its session log, and which `kiro-cli` commands get
//! peekme. Inline it leaves the mouse to the terminal (the system selection);
//! full screen its own selection arrives as OSC 52 (see `osc52.rs`).

pub mod cli;
pub mod explain;
pub mod screen;
pub mod transcript;
