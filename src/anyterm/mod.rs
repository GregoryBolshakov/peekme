//! peekme in any terminal: text the terminal itself selected in a program that
//! is not an agent CLI (a shell and whatever runs in it).
//!
//! There the terminal owns the mouse, so peekme learns the selected text (X11
//! PRIMARY) but not where it is. When that text is on screen more than once,
//! the box must still open under the copy the user selected: peekme turns on
//! any-motion mouse reports for a moment and moves the pointer by a pixel and
//! back (`nudge`), and the terminal reports the cell under the pointer, which
//! after a drag is the last selected cell.
//!
//! What differs from an agent's chat for the explainer (no transcript, other
//! wording) is in `context::Place::Terminal`; which agent explains is `pick`.

#[cfg(target_os = "linux")]
pub mod gnome;
pub mod pick;
#[cfg(target_os = "linux")]
pub mod x11;

use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

/// Any-motion mouse reports in SGR form, on for the nudge only.
pub const NUDGE_ON: &str = "\x1b[?1003h\x1b[?1006h";
pub const NUDGE_OFF: &str = "\x1b[?1003l";
/// No report this long after the nudge: open the box without it, as before.
pub const NUDGE_WAIT: Duration = Duration::from_millis(250);

/// Moves the pointer so the terminal sends a motion report; `answered` is
/// set once one came, so a second, bigger move is only made when needed.
/// Runs on its own thread. Tests use one that does nothing.
pub type Nudger = fn(Arc<AtomicBool>);

/// How this system lets peekme see the terminal's selection and pointer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Desktop {
    /// X11 (also XWayland-less X sessions of any desktop): XTEST and XFixes.
    X11,
    /// GNOME on Wayland: through the helper extension.
    GnomeWayland,
    /// Anything else (macOS, other Wayland desktops, over SSH): no nudge, no
    /// selection watch.
    Other,
}

pub fn desktop() -> Desktop {
    if std::env::var_os("SSH_CONNECTION").is_some() {
        return Desktop::Other;
    }
    #[cfg(target_os = "linux")]
    {
        if gnome::session() {
            return Desktop::GnomeWayland;
        }
        let wayland = std::env::var("XDG_SESSION_TYPE").is_ok_and(|t| t == "wayland");
        if !wayland && std::env::var_os("DISPLAY").is_some() {
            return Desktop::X11;
        }
    }
    Desktop::Other
}

/// The nudge of `desktop`, if it has one.
pub fn nudger(desktop: Desktop) -> Option<Nudger> {
    match desktop {
        #[cfg(target_os = "linux")]
        Desktop::X11 => Some(x11::nudge),
        #[cfg(target_os = "linux")]
        Desktop::GnomeWayland => Some(gnome::nudge),
        _ => None,
    }
}

/// When the selection of `desktop` last changed hands (a new selection was
/// made), where that can be watched; starts the watch.
pub fn selection_watch(desktop: Desktop) -> Option<fn() -> Option<Instant>> {
    match desktop {
        #[cfg(target_os = "linux")]
        Desktop::X11 => {
            x11::watch_primary();
            Some(x11::primary_changed_at)
        }
        #[cfg(target_os = "linux")]
        Desktop::GnomeWayland => Some(gnome::primary_changed_at),
        _ => None,
    }
}

/// How installing the GNOME helper went.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Installed {
    /// From extensions.gnome.org, running now.
    Now,
    /// Copied in place; GNOME loads it at the next login.
    AfterLogin,
    /// The user said no in GNOME's own dialog.
    Cancelled,
    Failed(String),
}

/// The GNOME helper answers (GNOME on Wayland).
pub fn helper_running() -> bool {
    #[cfg(target_os = "linux")]
    return gnome::available();
    #[cfg(not(target_os = "linux"))]
    false
}

/// Install the GNOME helper (blocks while GNOME's dialog is open).
pub fn install_helper() -> Installed {
    #[cfg(target_os = "linux")]
    return gnome::install();
    #[cfg(not(target_os = "linux"))]
    Installed::Failed("only on GNOME".into())
}

/// The explainer's system prompt for a selection in a plain terminal.
pub const SYSTEM: &str = "You are \"peek\", an explainer embedded in a terminal. The user highlighted a fragment \
     of text (marked ⟦like this⟧) in their terminal (command output, a log, a program's screen) and wants to \
     understand it. Explain what the fragment means where it appears: define the terms, name the specific \
     thing it refers to when the context shows it, say what any code, command or error does there, and what \
     it means for the user. No preamble, no headings. Answer from the provided context and general \
     knowledge only.";

/// One paragraph telling the explainer what the screen is.
pub fn environment(cwd: &str) -> String {
    format!(
        "<environment>\nThe user is in a terminal, in the directory {cwd}. The screen shows commands they \
         ran and their output, or the screen of a program running in the terminal. There is no chat with \
         an assistant on it.\n</environment>\n\n"
    )
}
