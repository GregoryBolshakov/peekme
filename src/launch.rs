//! Finding the real agent binaries without going through our own shims or
//! functions, and where those shims live.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

/// Set in the environment of everything peekme starts, so a nested agent
/// (or peekme) steps aside instead of wrapping twice.
pub const ACTIVE_ENV: &str = "PEEKME_ACTIVE";
/// Its value under terminal mode (a shell started with peekme): an agent
/// started in that shell still gets its own peekme, which knows the agent's
/// screen, and the outer one lets it have the keys (`foreground_is_peekme`).
pub const ACTIVE_TERMINAL: &str = "terminal";

/// Under the peekme whose environment this is, should a peekme for `agent`
/// step aside? Only an agent inside terminal mode runs a peekme of its own.
pub fn nested(active: Option<&OsStr>, agent: bool) -> bool {
    match active {
        None => false,
        Some(v) => !(agent && v == ACTIVE_TERMINAL),
    }
}

/// The program of `pid` is a peekme binary (a link to one counts: the kernel
/// knows the file, not the name it was started by).
pub fn is_peekme(pid: u32) -> bool {
    let Some(exe) = exe_of(pid) else {
        return false;
    };
    let me = std::env::current_exe().and_then(|p| p.canonicalize()).ok();
    me.as_deref() == Some(exe.as_path()) || exe.file_name().is_some_and(|n| n == "peekme")
}

#[cfg(target_os = "linux")]
fn exe_of(pid: u32) -> Option<PathBuf> {
    std::fs::read_link(format!("/proc/{pid}/exe")).ok()
}

#[cfg(target_os = "macos")]
fn exe_of(pid: u32) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    let mut buf = vec![0u8; 4096];
    let n = unsafe { libc::proc_pidpath(pid as i32, buf.as_mut_ptr().cast(), buf.len() as u32) };
    (n > 0).then(|| PathBuf::from(OsStr::from_bytes(&buf[..n as usize])))
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn exe_of(_pid: u32) -> Option<PathBuf> {
    None
}

/// The terminal `fd` (a pty's master side) runs a peekme as its foreground
/// job, other than `ours` (our job-control helper).
pub fn foreground_is_peekme(fd: i32, ours: Option<u32>) -> bool {
    let group = unsafe { libc::tcgetpgrp(fd) };
    group > 0 && Some(group as u32) != ours && is_peekme(group as u32)
}

/// The directory holding our `codex` and `claude` shims (links to the peekme binary).
pub fn shim_dir() -> Option<PathBuf> {
    let data = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| home().map(|h| h.join(".local/share")))?;
    Some(data.join("peekme").join("bin"))
}

pub fn home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
}

/// The first `name` (`codex`, `claude`) on PATH that is not peekme itself (our
/// shims link to it) and not in our shim directory.
pub fn find_real(name: &str) -> Option<PathBuf> {
    find_real_in(
        name,
        std::env::var_os("PATH").as_deref().unwrap_or_default(),
        std::env::current_exe().ok().as_deref(),
        shim_dir().as_deref(),
    )
}

fn find_real_in(
    name: &str,
    path: &OsStr,
    me: Option<&Path>,
    shim: Option<&Path>,
) -> Option<PathBuf> {
    let me = me.and_then(|p| p.canonicalize().ok());
    let shim = shim.and_then(|p| p.canonicalize().ok());
    for dir in std::env::split_paths(path) {
        if dir.as_os_str().is_empty() {
            continue;
        }
        if shim.is_some() && dir.canonicalize().ok() == shim {
            continue;
        }
        let candidate = dir.join(name);
        let Ok(real) = candidate.canonicalize() else {
            continue; // missing, or a dangling link
        };
        // Another peekme (a second install, or a link to one) is not the agent.
        let other_peekme = real.file_name().is_some_and(|n| n == "peekme");
        if me.as_ref() == Some(&real) || other_peekme || !is_executable(&real) {
            continue;
        }
        return Some(candidate);
    }
    None
}

/// Direct children of a process. Linux has /proc; macOS has no /proc, but
/// pgrep works on both.
pub fn children(pid: u32) -> Vec<u32> {
    let text = std::fs::read_to_string(format!("/proc/{pid}/task/{pid}/children"))
        .ok()
        .or_else(|| {
            let out = std::process::Command::new("pgrep")
                .args(["-P", &pid.to_string()])
                .output()
                .ok()?;
            Some(String::from_utf8_lossy(&out.stdout).into_owned())
        })
        .unwrap_or_default();
    text.split_whitespace()
        .filter_map(|p| p.parse().ok())
        .collect()
}

/// A process and everything below it (an npm launcher starts the real binary
/// as its child, for example).
pub fn process_tree(pid: u32) -> Vec<u32> {
    let mut all = vec![pid];
    let mut i = 0;
    while i < all.len() && all.len() < 256 {
        let kids = children(all[i]);
        all.extend(kids);
        i += 1;
    }
    all
}

fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    p.metadata()
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_an_agent_under_terminal_mode_gets_its_own_peekme() {
        let (agent, terminal) = (OsStr::new("1"), OsStr::new(ACTIVE_TERMINAL));
        assert!(!nested(None, true) && !nested(None, false));
        assert!(nested(Some(agent), true) && nested(Some(agent), false));
        assert!(!nested(Some(terminal), true));
        // A shell under a shell with peekme: one terminal mode is enough.
        assert!(nested(Some(terminal), false));
    }

    #[test]
    fn our_own_exe_counts_as_a_peekme() {
        // The test binary is not named peekme, but it is our own exe.
        assert!(is_peekme(std::process::id()));
        assert!(!is_peekme(u32::MAX));
    }

    #[test]
    fn real_codex_skips_our_shim_and_dangling_links() {
        let tmp = std::env::temp_dir().join(format!("peekme-test-{}", std::process::id()));
        let (shim, real, broken) = (tmp.join("shim"), tmp.join("real"), tmp.join("broken"));
        for d in [&shim, &real, &broken] {
            std::fs::create_dir_all(d).unwrap();
        }
        let me = tmp.join("peekme-binary");
        std::fs::write(&me, b"#!/bin/sh\n").unwrap();
        let real_codex = real.join("codex");
        std::fs::write(&real_codex, b"#!/bin/sh\n").unwrap();
        for p in [&me, &real_codex] {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let _ = std::os::unix::fs::symlink(&me, shim.join("codex"));
        let _ = std::os::unix::fs::symlink(tmp.join("gone"), broken.join("codex"));
        // A link to some other peekme binary is skipped too.
        let other = tmp.join("other");
        std::fs::create_dir_all(&other).unwrap();
        std::fs::copy(&me, other.join("peekme")).unwrap();
        let _ = std::os::unix::fs::symlink(other.join("peekme"), broken.join("claude"));
        let path = std::env::join_paths([&broken, &shim, &real]).unwrap();
        assert_eq!(
            find_real_in("codex", &path, Some(&me), Some(&shim)),
            Some(real_codex.clone())
        );
        // Even without knowing the shim directory, a link to ourselves is skipped.
        assert_eq!(
            find_real_in("codex", &path, Some(&me), None),
            Some(real_codex)
        );
        assert_eq!(find_real_in("claude", &path, Some(&me), None), None);
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
