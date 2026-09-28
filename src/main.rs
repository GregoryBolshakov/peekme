//! peekme: select text in an agent CLI's output, press Alt+P, and read an
//! explanation that opens inline under it.

use peekme::agent::Agent;
use peekme::{app, install, jobctl, launch, log, log_path};

use std::io::IsTerminal;

const HELP: &str = "\
peekme {version}
Select text in Codex CLI or Claude Code output with the mouse, press Alt+P
(Option+P on a Mac), and an explanation from a smaller model opens right next
to it. Esc closes it.

USAGE:
    peekme                     run codex (or claude, if only that is installed)
    peekme codex [ARGS...]     run codex with peekme
    peekme claude [ARGS...]    run claude with peekme
    peekme install             make typing `codex` and `claude` open them with peekme
    peekme uninstall           undo `peekme install`
    peekme doctor              check the setup
    peekme -- COMMAND [ARGS]   run any other command with peekme

After `peekme install`, `command codex` or `command claude` runs the agent
without peekme once.

ENVIRONMENT:
    PEEKME_MODEL         model for Codex explanations (default: the account's fast model)
    PEEKME_CODEX_BIN     codex binary used for the explainer (default: the one on PATH)
    PEEKME_CLAUDE_MODEL  model for Claude Code explanations (default: haiku)
    PEEKME_CLAUDE_BIN    claude binary used for the explainer (default: the one on PATH)
";

fn main() {
    let mut argv = std::env::args();
    let argv0 = argv.next().unwrap_or_default();
    let mut args: Vec<String> = argv.collect();

    // Started through one of our links (`codex`, `claude`, made by `peekme install`).
    let as_agent = Agent::from_program(&argv0);
    let mut plain_run = false;
    if as_agent.is_none() {
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
    let (program, rest) = match as_agent {
        Some(agent) => (agent.command().to_string(), args),
        None => match args.split_first() {
            Some((p, r)) => (p.clone(), r.to_vec()),
            None => (default_agent().command().to_string(), Vec::new()),
        },
    };

    // For an agent, always start the real binary, never our own link or function.
    let agent = Agent::from_program(&program);
    let program = match agent {
        Some(a) if !program.contains('/') => match launch::find_real(a.command()) {
            Some(p) => p.to_string_lossy().into_owned(),
            None => {
                eprintln!(
                    "peekme: {} not found on PATH. Install it: {}",
                    a.command(),
                    a.install_hint()
                );
                std::process::exit(127);
            }
        },
        _ => program,
    };

    // Step aside when there is nothing to overlay: already inside peekme, not
    // a terminal, or an agent command without its interactive screen.
    let nested = std::env::var_os(launch::ACTIVE_ENV).is_some();
    let tty = std::io::stdin().is_terminal() && std::io::stdout().is_terminal();
    if nested || !tty || agent.is_some_and(|a| !a.is_interactive(&rest)) {
        exec(&program, &rest);
    }

    if plain_run {
        install::first_run_question();
    }

    // A panic is logged, never printed over the child's screen. The loop in
    // `app` catches panics in the peek code and keeps passing the agent through.
    std::panic::set_hook(Box::new(|info| log(&format!("panic: {info}"))));

    peekme::input::set_mac_keyboard(peekme::input::detect_mac_keyboard());

    if crossterm::terminal::enable_raw_mode().is_err() {
        exec(&program, &rest);
    }
    let result = std::panic::catch_unwind(|| app::run(&program, &rest, agent));
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

/// Plain `peekme`: Codex, as before, unless only Claude Code is installed.
fn default_agent() -> Agent {
    if launch::find_real("codex").is_none() && launch::find_real("claude").is_some() {
        Agent::Claude
    } else {
        Agent::Codex
    }
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

fn restore_terminal() {
    let _ = crossterm::terminal::disable_raw_mode();
    // End any half-written synchronized update and make the cursor visible.
    print!("\x1b[?2026l\x1b[0m\x1b[?25h");
    let _ = std::io::Write::flush(&mut std::io::stdout());
}
