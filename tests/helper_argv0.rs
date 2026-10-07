//! The job-control helper must work when it starts under an agent's name:
//! on macOS `current_exe()` keeps the link's name, and a copy or a hard link
//! has it on every platform. Before the fix the flag went on to the agent.

use std::process::{Command, Stdio};

#[cfg(unix)]
#[test]
fn helper_started_as_claude_runs_the_command() {
    let dir = std::env::temp_dir().join(format!("peekme-helper-argv0-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let link = dir.join("claude");
    let _ = std::fs::remove_file(&link);
    std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_peekme"), &link).unwrap();
    let out = Command::new(&link)
        .args(["--internal-jobctl-helper", "/bin/echo", "hi"])
        .env_remove("PEEKME_ACTIVE")
        // No real agent to fall through to if this ever breaks again.
        .env("PATH", &dir)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(String::from_utf8_lossy(&out.stdout), "hi\n");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}
