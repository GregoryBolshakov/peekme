//! Job control, so Ctrl+Z inside the child suspends everything the way it does
//! without peekme.
//!
//! Codex suspends itself with `kill(0, SIGTSTP)`. A process started straight
//! in a PTY is the session leader, so its process group has no parent in the
//! same session and is "orphaned": Linux discards stop signals sent to it and
//! Ctrl+Z silently does nothing. A shell avoids that by putting each job in its
//! own process group under itself, and so does this helper:
//!
//! - the PTY runs `peekme --internal-jobctl-helper <cmd> <args>`: the helper is
//!   the session leader;
//! - it forks the real command into a new process group and makes that the
//!   terminal's foreground group, so the group is not orphaned;
//! - when the command stops, the helper tells the wrapper (its parent) with
//!   SIGUSR1; the wrapper suspends itself in the user's shell;
//! - when the user resumes, the wrapper sends SIGUSR2 and the helper hands the
//!   terminal back and continues the command, which then runs its own resume
//!   code (Codex re-applies its modes, asks for the cursor, redraws).

pub const HELPER_ARG: &str = "--internal-jobctl-helper";

/// Signal the helper sends the wrapper when the child stopped.
pub const STOPPED: i32 = libc::SIGUSR1;
/// Signal the wrapper sends the helper to continue the child.
pub const CONTINUE: i32 = libc::SIGUSR2;

/// Entry point of the helper process. Never returns.
pub fn helper_main(args: &[String]) -> ! {
    use std::ffi::CString;

    let Some(program) = args.first() else {
        eprintln!("peekme: {HELPER_ARG} needs a command");
        unsafe { libc::_exit(2) };
    };
    let c_args: Vec<CString> = args
        .iter()
        .map(|a| CString::new(a.as_str()).unwrap_or_default())
        .collect();
    let c_program = c_args[0].clone();
    let mut argv: Vec<*const libc::c_char> = c_args.iter().map(|a| a.as_ptr()).collect();
    argv.push(std::ptr::null());

    unsafe {
        // Taking the terminal's foreground from a background group sends SIGTTOU.
        libc::signal(libc::SIGTTOU, libc::SIG_IGN);
        // SIGUSR2 is collected with sigwait, so keep it blocked (children unblock it).
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        libc::sigaddset(&mut set, CONTINUE);
        libc::sigprocmask(libc::SIG_BLOCK, &set, std::ptr::null_mut());

        let child = libc::fork();
        if child < 0 {
            eprintln!("peekme: fork failed");
            libc::_exit(127);
        }
        if child == 0 {
            libc::setpgid(0, 0);
            libc::tcsetpgrp(0, libc::getpid());
            libc::signal(libc::SIGTTOU, libc::SIG_DFL);
            libc::sigprocmask(libc::SIG_UNBLOCK, &set, std::ptr::null_mut());
            libc::execvp(c_program.as_ptr(), argv.as_ptr());
            let err = std::io::Error::last_os_error();
            eprintln!("peekme: could not start `{program}`: {err}");
            libc::_exit(127);
        }
        // Set it from both sides so neither process races the other.
        libc::setpgid(child, child);
        libc::tcsetpgrp(0, child);

        loop {
            let mut status = 0;
            let r = libc::waitpid(child, &mut status, libc::WUNTRACED);
            if r < 0 {
                if std::io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
                    continue;
                }
                libc::_exit(1);
            }
            if libc::WIFSTOPPED(status) {
                libc::kill(libc::getppid(), STOPPED);
                let mut sig = 0;
                libc::sigwait(&set, &mut sig);
                libc::tcsetpgrp(0, child);
                libc::kill(-child, libc::SIGCONT);
                continue;
            }
            if libc::WIFEXITED(status) {
                libc::_exit(libc::WEXITSTATUS(status));
            }
            if libc::WIFSIGNALED(status) {
                // Die the same way, so the wrapper reports the same status.
                let sig = libc::WTERMSIG(status);
                libc::signal(sig, libc::SIG_DFL);
                libc::kill(libc::getpid(), sig);
                libc::_exit(128 + sig);
            }
        }
    }
}

/// Suspend this process (the wrapper) in the user's shell, with the terminal
/// restored, and come back in raw mode once the user resumes it.
pub fn suspend_self() {
    let _ = crossterm::terminal::disable_raw_mode();
    unsafe {
        libc::raise(libc::SIGTSTP);
    }
    // Execution continues here after `fg` (SIGCONT).
    let _ = crossterm::terminal::enable_raw_mode();
}

/// Ask the helper to hand the terminal back to the child and continue it.
pub fn continue_child(helper_pid: u32) {
    unsafe {
        libc::kill(helper_pid as libc::pid_t, CONTINUE);
    }
}
