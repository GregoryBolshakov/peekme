//! peekme: select text in an agent CLI's output, press Alt+P, and read an
//! explanation that opens inline under it.

use peekme::agent::Agent;
use peekme::{app, install, jobctl, launch, log, log_path};

use std::io::IsTerminal;

const HELP: &str = "\
peekme {version}
Select text in Claude Code, Codex CLI, Pi, GitHub Copilot CLI or Kiro CLI
output with the mouse, press Alt+P (Option+P on a Mac), and an explanation from
your chat's model opens right next to it. Esc closes it. Alt+Q (Option+Q), Q for
question, asks first: type a question about the selection, and the chat's own
model answers it with the whole conversation.

USAGE:
    peekme                     run the first installed of claude, codex, pi, copilot, kiro-cli
    peekme claude [ARGS...]    run claude with peekme
    peekme codex [ARGS...]     run codex with peekme
    peekme pi [ARGS...]        run pi with peekme
    peekme copilot [ARGS...]   run copilot with peekme
    peekme kiro-cli [ARGS...]  run kiro-cli with peekme
    peekme install             make typing `claude`, `codex`, `pi`, `copilot`, `kiro-cli`
                               open them with peekme
    peekme install --shell     also start every new terminal's shell with peekme, so
                               Alt+P works on any text there
    peekme uninstall           undo `peekme install`
    peekme uninstall --shell   only stop starting the shell with peekme
    peekme doctor              check the setup
    peekme -- COMMAND [ARGS]   run any other command with peekme

After `peekme install`, `command claude` (or codex, pi, copilot, kiro-cli) runs
the agent without peekme once.

ENVIRONMENT:
    PEEKME_CLAUDE_MODEL  model for Claude Code explanations (default: the chat's model)
    PEEKME_CLAUDE_BIN    claude binary used for the explainer (default: the one on PATH)
    PEEKME_MODEL         model for Codex explanations (default: the chat's model)
    PEEKME_CODEX_BIN     codex binary used for the explainer (default: the one on PATH)
    PEEKME_PI_MODEL      model for Pi explanations, provider/id (default: the chat's own)
    PEEKME_PI_BIN        pi binary used for the explainer (default: the one on PATH)
    PEEKME_COPILOT_MODEL model for Copilot explanations (default: the chat's model)
    PEEKME_COPILOT_BIN   copilot binary used for the explainer (default: the one on PATH)
    PEEKME_KIRO_MODEL    model for Kiro explanations (default: the chat's model)
    PEEKME_KIRO_BIN      kiro-cli binary used for the explainer (default: the one on PATH)
    PEEKME_CLAUDE_ASK_MODEL, PEEKME_CODEX_ASK_MODEL, PEEKME_PI_ASK_MODEL,
    PEEKME_COPILOT_ASK_MODEL, PEEKME_KIRO_ASK_MODEL
                         model for typed questions (default: the chat's own model)
";

fn main() {
    let mut argv = std::env::args();
    let argv0 = argv.next().unwrap_or_default();
    let mut args: Vec<String> = argv.collect();

    // The helper can start under an agent's name: on macOS current_exe() is
    // the path we were started with, often our `claude` link, and a copy or a
    // hard link has that name everywhere. Agents must never get the flag.
    if args.first().map(String::as_str) == Some(jobctl::HELPER_ARG) {
        jobctl::helper_main(&args[1..]);
    }

    // Started through one of our links (`codex`, `claude`, made by `peekme install`).
    let as_agent = Agent::from_program(&argv0);
    let mut plain_run = false;
    if as_agent.is_none() {
        match args.first().map(String::as_str) {
            Some("-h" | "--help") => {
                print!("{}", HELP.replace("{version}", env!("CARGO_PKG_VERSION")));
                return;
            }
            Some("-V" | "--version") => {
                println!("peekme {}", env!("CARGO_PKG_VERSION"));
                return;
            }
            Some("install") => {
                let shell = shell_flag(&args[1..]);
                std::process::exit(report(install::install(shell)))
            }
            Some("uninstall") => {
                let shell = shell_flag(&args[1..]);
                std::process::exit(report(install::uninstall(shell)))
            }
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
    let nested = launch::nested(
        std::env::var_os(launch::ACTIVE_ENV).as_deref(),
        agent.is_some(),
    );
    let tty = std::io::stdin().is_terminal() && std::io::stdout().is_terminal();
    // `pi` may be another program of that name (see `Agent::is_agent_binary`).
    let other = agent.is_some_and(|a| !a.is_agent_binary(std::path::Path::new(&program)));
    if nested || !tty || other || agent.is_some_and(|a| !a.is_interactive(&rest)) {
        exec(&program, &rest);
    }

    if plain_run {
        install::first_run_question();
    }
    // An older `peekme install` may predate some agents: fix that quietly
    // before starting (only if the user set peekme up at all).
    if let Some(note) = install::refresh() {
        eprintln!("{note}");
    }

    // A panic is logged, never printed over the child's screen. The loop in
    // `app` catches panics in the peek code and keeps passing the agent through.
    std::panic::set_hook(Box::new(|info| log(&format!("panic: {info}"))));

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

/// `--shell` after `install` or `uninstall`; anything else is an error.
fn shell_flag(rest: &[String]) -> bool {
    match rest {
        [] => false,
        [flag] if flag == "--shell" => true,
        _ => {
            eprintln!(
                "peekme: unknown argument `{}`. See: peekme --help",
                rest.join(" ")
            );
            std::process::exit(2);
        }
    }
}

/// Plain `peekme`: the first agent that is installed.
fn default_agent() -> Agent {
    Agent::ALL
        .into_iter()
        .find(|a| a.find().is_some())
        .unwrap_or(Agent::Claude)
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
