//! Deciding whether a `codex ...` invocation gets peekme, and finding the real
//! `codex` binary without going through our own shim or function.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

/// Set in the environment of everything peekme starts, so a nested `codex`
/// (or peekme) steps aside instead of wrapping twice.
pub const ACTIVE_ENV: &str = "PEEKME_ACTIVE";

/// Codex options that take a value (from `codex --help`, 0.154.0).
const VALUE_OPTIONS: &[&str] = &[
    "-c",
    "--config",
    "--enable",
    "--disable",
    "--remote",
    "--remote-auth-token-env",
    "-m",
    "--model",
    "--local-provider",
    "-p",
    "--profile",
    "-s",
    "--sandbox",
    "-C",
    "--cd",
    "--add-dir",
    "-a",
    "--ask-for-approval",
];

/// Subcommands that open Codex's own interactive screen, where peeking helps.
/// Everything else (`exec`, `login`, `mcp`, `app-server`, ...) runs directly.
const INTERACTIVE_SUBCOMMANDS: &[&str] = &["resume", "fork"];

/// Every Codex subcommand name (0.154.0), used to tell a subcommand from a prompt.
const SUBCOMMANDS: &[&str] = &[
    "exec",
    "e",
    "review",
    "login",
    "logout",
    "mcp",
    "plugin",
    "app-server",
    "remote-control",
    "completion",
    "update",
    "doctor",
    "sandbox",
    "debug",
    "apply",
    "a",
    "resume",
    "queue",
    "archive",
    "delete",
    "migrate-rollouts",
    "unarchive",
    "fork",
    "cloud",
    "exec-server",
    "features",
    "help",
    "agents",
];

/// True when `codex <args>` starts Codex's interactive screen: no subcommand
/// (maybe a prompt), or `resume` / `fork`. Help and version also run directly.
pub fn is_interactive_codex(args: &[String]) -> bool {
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        match a {
            "-h" | "--help" | "-V" | "--version" => return false,
            "--" => return true,
            "-i" | "--image" => {
                // One or more files follow.
                i += 1;
                while i < args.len() && !args[i].starts_with('-') {
                    i += 1;
                }
                continue;
            }
            _ if VALUE_OPTIONS.contains(&a) => i += 2,
            _ if a.starts_with('-') => i += 1,
            _ => {
                return !SUBCOMMANDS.contains(&a) || INTERACTIVE_SUBCOMMANDS.contains(&a);
            }
        }
    }
    true
}

/// The directory holding our `codex` shim (a link to the peekme binary).
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

/// The first `codex` on PATH that is not peekme itself (our shim links to it)
/// and not in our shim directory.
pub fn find_real_codex() -> Option<PathBuf> {
    find_real_in(
        std::env::var_os("PATH").as_deref().unwrap_or_default(),
        std::env::current_exe().ok().as_deref(),
        shim_dir().as_deref(),
    )
}

fn find_real_in(path: &OsStr, me: Option<&Path>, shim: Option<&Path>) -> Option<PathBuf> {
    let me = me.and_then(|p| p.canonicalize().ok());
    let shim = shim.and_then(|p| p.canonicalize().ok());
    for dir in std::env::split_paths(path) {
        if dir.as_os_str().is_empty() {
            continue;
        }
        if shim.is_some() && dir.canonicalize().ok() == shim {
            continue;
        }
        let candidate = dir.join("codex");
        let Ok(real) = candidate.canonicalize() else {
            continue; // missing, or a dangling link
        };
        if me.as_ref() == Some(&real) || !is_executable(&real) {
            continue;
        }
        return Some(candidate);
    }
    None
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

    fn v(s: &[&str]) -> Vec<String> {
        s.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn interactive_or_not() {
        assert!(is_interactive_codex(&v(&[])));
        assert!(is_interactive_codex(&v(&["fix the build"])));
        assert!(is_interactive_codex(&v(&[
            "-m",
            "gpt-5.6-luna",
            "resume",
            "--last"
        ])));
        assert!(is_interactive_codex(&v(&["--yolo", "fork"])));
        assert!(is_interactive_codex(&v(&[
            "-c",
            "model=\"x\"",
            "-i",
            "a.png",
            "b.png",
            "--search"
        ])));
        // A prompt that happens to be a subcommand's name needs `--`.
        assert!(is_interactive_codex(&v(&["--", "exec"])));
        assert!(!is_interactive_codex(&v(&["exec", "do it"])));
        assert!(!is_interactive_codex(&v(&["-m", "resume", "e", "x"])));
        assert!(!is_interactive_codex(&v(&["login"])));
        assert!(!is_interactive_codex(&v(&["mcp", "list"])));
        assert!(!is_interactive_codex(&v(&["app-server"])));
        assert!(!is_interactive_codex(&v(&["--version"])));
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
        let path = std::env::join_paths([&broken, &shim, &real]).unwrap();
        assert_eq!(
            find_real_in(&path, Some(&me), Some(&shim)),
            Some(real_codex.clone())
        );
        // Even without knowing the shim directory, a link to ourselves is skipped.
        assert_eq!(find_real_in(&path, Some(&me), None), Some(real_codex));
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
