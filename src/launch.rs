//! Finding the real agent binaries without going through our own shims or
//! functions, and where those shims live.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

/// Set in the environment of everything peekme starts, so a nested agent
/// (or peekme) steps aside instead of wrapping twice.
pub const ACTIVE_ENV: &str = "PEEKME_ACTIVE";

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
