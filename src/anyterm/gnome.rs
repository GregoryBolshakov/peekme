//! GNOME on Wayland: the selection and the pointer belong to GNOME Shell, so
//! peekme asks its helper extension (`anyterm/gnome-helper/`) over D-Bus: the
//! PRIMARY text, when it last changed, and a nudge of the pointer.
//!
//! Calls go through `busctl --json` (part of systemd, there on every GNOME
//! system), which keeps a D-Bus library out of the build.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::time::{Duration, Instant};

use serde_json::Value;

use super::Installed;

pub const UUID: &str = "peekme-helper@peekme.dev";
const NAME: &str = "dev.peekme.Helper";
const PATH: &str = "/dev/peekme/Helper";
const IFACE: &str = "dev.peekme.Helper1";

/// The helper's files, one build for GNOME 42-44 and one (ES modules) for 45+.
const LEGACY_JS: &str = include_str!("../../anyterm/gnome-helper/legacy/extension.js");
const LEGACY_META: &str = include_str!("../../anyterm/gnome-helper/legacy/metadata.json");
const ESM_JS: &str = include_str!("../../anyterm/gnome-helper/esm/extension.js");
const ESM_META: &str = include_str!("../../anyterm/gnome-helper/esm/metadata.json");

/// A GNOME Wayland session on this machine (not over SSH).
pub fn session() -> bool {
    let var = |k| std::env::var(k).unwrap_or_default();
    var("XDG_SESSION_TYPE") == "wayland"
        && var("XDG_CURRENT_DESKTOP")
            .split(':')
            .any(|d| d.eq_ignore_ascii_case("GNOME"))
        && std::env::var_os("SSH_CONNECTION").is_none()
}

/// 0: not asked yet, 1: running, 2: not running.
static RUNNING: AtomicU8 = AtomicU8::new(0);

/// The helper answers on the session bus. Asked once; `forget` asks again.
pub fn available() -> bool {
    match RUNNING.load(Ordering::Relaxed) {
        1 => true,
        2 => false,
        _ => {
            let up = call(IFACE_CALL, &["Version"]).is_some();
            RUNNING.store(if up { 1 } else { 2 }, Ordering::Relaxed);
            up
        }
    }
}

/// The helper was just installed or may have gone: ask again next time.
pub fn forget() {
    RUNNING.store(0, Ordering::Relaxed);
}

/// The selected text (PRIMARY).
pub fn primary() -> Option<String> {
    if !available() {
        return None;
    }
    let v = call(IFACE_CALL, &["Primary"])?;
    let text = v.get("data")?.get(0)?.as_str()?.to_string();
    (!text.is_empty()).then_some(text)
}

/// When PRIMARY last got a new selection, if since the helper started.
pub fn primary_changed_at() -> Option<Instant> {
    if !available() {
        return None;
    }
    let usec = call(IFACE_CALL, &["SelectionChanged"])?
        .get("data")?
        .get(0)?
        .as_i64()?;
    let now = monotonic_usec();
    (usec > 0 && usec <= now)
        .then(|| Instant::now().checked_sub(Duration::from_micros((now - usec) as u64)))
        .flatten()
}

/// Move the pointer a pixel and back; if no report came, a bigger step (see
/// `x11::nudge`, the same on X11).
pub fn nudge(answered: Arc<AtomicBool>) {
    if call(IFACE_CALL, &["Nudge", "i", "1"]).is_none() {
        return;
    }
    let until = Instant::now() + Duration::from_millis(80);
    while Instant::now() < until {
        if answered.load(Ordering::Relaxed) {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    if !answered.load(Ordering::Relaxed) {
        let _ = call(IFACE_CALL, &["Nudge", "i", "24"]);
    }
}

/// Install the helper: from extensions.gnome.org, which GNOME Shell loads at
/// once after its own confirmation dialog; failing that (not published for
/// this GNOME, offline), a local copy that starts at the next login.
pub fn install() -> Installed {
    let remote = busctl(
        &[
            "--timeout=600",
            "org.gnome.Shell",
            "/org/gnome/Shell",
            "org.gnome.Shell.Extensions",
            "InstallRemoteExtension",
            "s",
            UUID,
        ],
        Duration::from_secs(610),
    );
    match remote
        .as_ref()
        .ok()
        .and_then(|v| v.get("data")?.get(0)?.as_str().map(String::from))
        .as_deref()
    {
        Some("successful") => {
            forget();
            return Installed::Now;
        }
        Some("cancelled") => return Installed::Cancelled,
        _ => {}
    }
    crate::log(&format!(
        "anyterm: GNOME could not install {UUID} from extensions.gnome.org ({}); installing a local copy",
        remote.err().unwrap_or_default()
    ));
    match install_local() {
        Ok(()) => Installed::AfterLogin,
        Err(e) => Installed::Failed(e),
    }
}

fn install_local() -> Result<(), String> {
    let major = shell_major().ok_or("gnome-shell --version gave no version")?;
    let (js, meta) = if major >= 45 {
        (ESM_JS, ESM_META)
    } else {
        (LEGACY_JS, LEGACY_META)
    };
    let dir = extensions_dir().ok_or("no home folder")?.join(UUID);
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    for (name, text) in [("extension.js", js), ("metadata.json", meta)] {
        std::fs::write(dir.join(name), text).map_err(|e| format!("{name}: {e}"))?;
    }
    enable_at_login()
}

/// Where GNOME looks for the user's extensions.
fn extensions_dir() -> Option<PathBuf> {
    let data = std::env::var_os("XDG_DATA_HOME")
        .filter(|d| !d.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| Path::new(&h).join(".local/share")))?;
    Some(data.join("gnome-shell/extensions"))
}

/// `GNOME Shell 42.9` -> 42.
fn shell_major() -> Option<u32> {
    let out = Command::new("gnome-shell")
        .arg("--version")
        .stderr(Stdio::null())
        .output()
        .ok()?;
    parse_major(&String::from_utf8_lossy(&out.stdout))
}

fn parse_major(version: &str) -> Option<u32> {
    version
        .split_whitespace()
        .last()?
        .split('.')
        .next()?
        .parse()
        .ok()
}

/// Add the helper to GNOME's enabled extensions (a running shell cannot load
/// a new local extension on Wayland, so it starts at the next login).
fn enable_at_login() -> Result<(), String> {
    let out = Command::new("gsettings")
        .args(["get", "org.gnome.shell", "enabled-extensions"])
        .output()
        .map_err(|e| format!("gsettings: {e}"))?;
    let list = String::from_utf8_lossy(&out.stdout);
    let Some(new) = with_uuid(&list) else {
        return Ok(());
    };
    let ok = Command::new("gsettings")
        .args(["set", "org.gnome.shell", "enabled-extensions", &new])
        .status()
        .map_err(|e| format!("gsettings: {e}"))?
        .success();
    if ok {
        Ok(())
    } else {
        Err("gsettings could not enable the helper".into())
    }
}

/// GVariant text of a string list with the helper added; None if it is there.
fn with_uuid(list: &str) -> Option<String> {
    let list = list.trim().trim_start_matches("@as").trim();
    let inner = list.strip_prefix('[')?.strip_suffix(']')?.trim();
    let quoted = format!("'{UUID}'");
    if inner.split(',').any(|item| item.trim() == quoted) {
        return None;
    }
    Some(if inner.is_empty() {
        format!("[{quoted}]")
    } else {
        format!("[{inner}, {quoted}]")
    })
}

/// Arguments that call a method of the helper.
const IFACE_CALL: &[&str] = &[NAME, PATH, IFACE];

fn call(target: &[&str], method: &[&str]) -> Option<Value> {
    let args: Vec<&str> = target.iter().chain(method).copied().collect();
    busctl(&args, Duration::from_secs(2)).ok()
}

/// `busctl --user --json=short call ARGS`, its JSON reply, within `limit`.
fn busctl(args: &[&str], limit: Duration) -> Result<Value, String> {
    // Options go before the call's own arguments.
    let (opts, rest): (Vec<&str>, Vec<&str>) = args.iter().partition(|a| a.starts_with("--"));
    let mut child = Command::new("busctl")
        .args(["--user", "--json=short"])
        .args(opts)
        .arg("call")
        .args(rest)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("busctl: {e}"))?;
    let until = Instant::now() + limit;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() >= until => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("busctl timed out".into());
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(5)),
            Err(e) => return Err(format!("busctl: {e}")),
        }
    }
    let out = child
        .wait_with_output()
        .map_err(|e| format!("busctl: {e}"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    serde_json::from_slice(&out.stdout).map_err(|e| format!("busctl output: {e}"))
}

/// CLOCK_MONOTONIC in microseconds: GLib's `g_get_monotonic_time`, which the
/// helper uses for `SelectionChanged`.
fn monotonic_usec() -> i64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
    ts.tv_sec * 1_000_000 + ts.tv_nsec / 1_000
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_versions() {
        assert_eq!(parse_major("GNOME Shell 42.9\n"), Some(42));
        assert_eq!(parse_major("GNOME Shell 48.rc"), Some(48));
        assert_eq!(parse_major(""), None);
    }

    #[test]
    fn enabled_list_gets_the_helper_once() {
        assert_eq!(
            with_uuid("@as []").as_deref(),
            Some("['peekme-helper@peekme.dev']")
        );
        assert_eq!(
            with_uuid("['ding@rastersoft.com', 'ubuntu-dock@ubuntu.com']\n").as_deref(),
            Some("['ding@rastersoft.com', 'ubuntu-dock@ubuntu.com', 'peekme-helper@peekme.dev']")
        );
        assert_eq!(with_uuid("['peekme-helper@peekme.dev']"), None);
    }

    #[test]
    fn the_helper_files_match() {
        // Both builds carry the same uuid and differ only in the module style.
        for meta in [LEGACY_META, ESM_META] {
            let v: Value = serde_json::from_str(meta).unwrap();
            assert_eq!(v["uuid"], UUID);
        }
        let body = |js: &str| {
            js[js.find("const IFACE").unwrap()
                ..js.find("let helper").or(js.find("export default")).unwrap()]
                .to_string()
        };
        assert_eq!(body(LEGACY_JS), body(ESM_JS));
    }
}
