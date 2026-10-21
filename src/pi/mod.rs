//! Pi (`pi`, pi.dev): explanations through its RPC mode, the conversation
//! from its session file, its screen, and which `pi` commands get peekme.
//! Full screen (its default) Pi selects with its own drag and copies on
//! release, by OSC 52 too when peekme asks it to (`PI_CLIPBOARD_OSC52`, see
//! `osc52.rs`); its regular screen draws inline and leaves the mouse to the
//! terminal.

pub mod cli;
pub mod explain;
pub mod screen;
pub mod scroll;
pub mod transcript;
