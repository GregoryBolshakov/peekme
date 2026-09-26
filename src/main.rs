//! peekme: select text in Codex CLI output, press Alt+P, and read an
//! explanation that opens inline under it.

mod app;
mod context;
mod explain;
mod input;
mod install;
mod jobctl;
mod launch;
mod overlay;
mod render;
mod select;
mod shadow;

use std::io::IsTerminal;

const HELP: &str = "\
peekme {version}
Select text in Codex's output with the mouse, press Alt+P, and an explanation
from a smaller model opens right next to it. Esc closes it.

USAGE:
    peekme                     run codex with peekme
    peekme codex [ARGS...]     same, with arguments for codex
    peekme install             make typing `codex` open Codex with peekme
    peekme uninstall           undo `peekme install`
    peekme doctor              check the setup
    peekme -- COMMAND [ARGS]   run any other command with peekme

After `peekme install`, `command codex` runs plain Codex once.

ENVIRONMENT:
    PEEKME_MODEL        model for explanations (default: the account's fast model)
    PEEKME_CODEX_BIN    codex binary used for the explainer (default: the one on PATH)
";

fn main() {
    let mut argv = std::env::args();
    let argv0 = argv.next().unwrap_or_default();
    let mut args: Vec<String> = argv.collect();

    // Started through our `codex` link (installed by `peekme install`).
    let as_codex = std::path::Path::new(&argv0).file_name() == Some(std::ffi::OsStr::new("codex"));
    let mut plain_run = false;
    if !as_codex {
        match args.first().map(String::as_str) {
            Some(jobctl::HELPER_ARG) => jobctl::helper_main(&args[1..]),
            Some("-h" | "--help") => {
                print!("{}", HELP.replace("{version}", env!("CARGO_PKG_VERSION")));
                return;
            }
            Some("-V" | "--version") => {
                println!("peekme {}", env!("CARGO_PKG_VERSION"));
                return;
            }
            Some("install") => std::process::exit(report(install::install())),
            Some("uninstall") => std::process::exit(report(install::uninstall())),
            Some("doctor") => std::process::exit(install::doctor()),
            Some("--") => {
                args.remove(0);
            }
            None => plain_run = true,
            _ => {}
        }
    }
    let (program, rest) = if as_codex {
        ("codex".to_string(), args)
    } else {
        match args.split_first() {
            Some((p, r)) => (p.clone(), r.to_vec()),
            None => ("codex".to_string(), Vec::new()),
        }
    };

    // For Codex, always start the real binary, never our own link or function.
    let is_codex =
        std::path::Path::new(&program).file_name() == Some(std::ffi::OsStr::new("codex"));
    let program = if is_codex && !program.contains('/') {
        match launch::find_real_codex() {
            Some(p) => p.to_string_lossy().into_owned(),
            None => {
                eprintln!("peekme: codex not found on PATH. Install it: npm i -g @openai/codex");
                std::process::exit(127);
            }
        }
    } else {
        program
    };

    // Step aside when there is nothing to overlay: already inside peekme, not
    // a terminal, or a Codex command without its interactive screen.
    let nested = std::env::var_os(launch::ACTIVE_ENV).is_some();
    let tty = std::io::stdin().is_terminal() && std::io::stdout().is_terminal();
    if nested || !tty || (is_codex && !launch::is_interactive_codex(&rest)) {
        exec(&program, &rest);
    }

    if plain_run {
        install::first_run_question();
    }

    // A panic is logged, never printed over the child's screen. The loop in
    // `app` catches panics in the peek code and keeps passing Codex through.
    std::panic::set_hook(Box::new(|info| log(&format!("panic: {info}"))));

    if crossterm::terminal::enable_raw_mode().is_err() {
        exec(&program, &rest);
    }
    let result = std::panic::catch_unwind(|| app::run(&program, &rest));
    restore_terminal();
    let code = match result {
        Ok(Ok(code)) => code,
        // Fail open: if peekme could not even start, run the command without it.
        Ok(Err(app::Failure::Setup(e))) => {
            log(&format!("setup failed, running {program} directly: {e:#}"));
            exec(&program, &rest);
        }
        Ok(Err(app::Failure::Runtime(e))) => {
            eprintln!("peekme: {e:#}");
            1
        }
        Err(_) => {
            eprintln!("peekme: internal error, see {}", log_path().display());
            1
        }
    };
    std::process::exit(code);
}

fn report(r: anyhow::Result<()>) -> i32 {
    match r {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("peekme: {e:#}");
            1
        }
    }
}

/// Replace this process with the command (keeps its pid, signals and exit code).
fn exec(program: &str, args: &[String]) -> ! {
    use std::os::unix::process::CommandExt;
    let err = std::process::Command::new(program).args(args).exec();
    eprintln!("peekme: could not start `{program}`: {err}");
    std::process::exit(127);
}

fn log_path() -> std::path::PathBuf {
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::Path::new(&h).join(".local/state")))
        .unwrap_or_else(std::env::temp_dir);
    base.join("peekme").join("peekme.log")
}

/// Append a line to the log file. Never fails loudly: this runs inside a panic hook.
pub fn log(msg: &str) {
    let path = log_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let _ = std::io::Write::write_all(&mut f, format!("{secs} {msg}\n").as_bytes());
    }
}

fn restore_terminal() {
    let _ = crossterm::terminal::disable_raw_mode();
    // End any half-written synchronized update and make the cursor visible.
    print!("\x1b[?2026l\x1b[0m\x1b[?25h");
    let _ = std::io::Write::flush(&mut std::io::stdout());
}
