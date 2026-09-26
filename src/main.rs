//! peekme: select text in Codex CLI output, press Alt+P, and read an
//! explanation that opens inline under it.

mod app;
mod context;
mod explain;
mod input;
mod overlay;
mod render;
mod select;
mod shadow;

use std::io::IsTerminal;

const HELP: &str = "\
peekme {version}
Select text in Codex's output with the mouse, press Alt+P, and an explanation
from a smaller model opens inline under it. Esc closes it.

USAGE:
    peekme [--] [COMMAND [ARGS...]]    (COMMAND defaults to `codex`)

EXAMPLES:
    peekme                         run codex
    peekme codex resume --last     arguments go to codex
    alias codex='peekme codex'

ENVIRONMENT:
    PEEKME_MODEL        model for explanations (default: the account's fast model)
    PEEKME_CODEX_BIN    codex binary used for the explainer (default: codex)
";

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("-h" | "--help") => {
            print!("{}", HELP.replace("{version}", env!("CARGO_PKG_VERSION")));
            return;
        }
        Some("-V" | "--version") => {
            println!("peekme {}", env!("CARGO_PKG_VERSION"));
            return;
        }
        Some("--") => {
            args.remove(0);
        }
        _ => {}
    }
    let (program, rest) = match args.split_first() {
        Some((p, r)) => (p.clone(), r.to_vec()),
        None => ("codex".to_string(), Vec::new()),
    };

    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        // Nothing to overlay: behave exactly like the wrapped command.
        let status = std::process::Command::new(&program).args(&rest).status();
        std::process::exit(match status {
            Ok(s) => s.code().unwrap_or(1),
            Err(e) => {
                eprintln!("peekme: could not start `{program}`: {e}");
                127
            }
        });
    }

    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore_terminal();
        default_hook(info);
    }));

    if let Err(e) = crossterm::terminal::enable_raw_mode() {
        eprintln!("peekme: could not switch the terminal to raw mode: {e}");
        std::process::exit(1);
    }
    let code = match app::run(&program, &rest) {
        Ok(code) => code,
        Err(e) => {
            restore_terminal();
            eprintln!("peekme: {e:#}");
            1
        }
    };
    restore_terminal();
    std::process::exit(code);
}

fn restore_terminal() {
    let _ = crossterm::terminal::disable_raw_mode();
    // End any half-written synchronized update and make the cursor visible.
    print!("\x1b[?2026l\x1b[0m\x1b[?25h");
    let _ = std::io::Write::flush(&mut std::io::stdout());
}
