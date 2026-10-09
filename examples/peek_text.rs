//! Development helper: run one explanation for a selection inside some screen
//! text, without a terminal. Prints the prompt sent and the streamed answer.
//!
//!     cargo run --example peek_text -- "<screen text>" "<selection>" [--deep] [--claude|--copilot|--kiro] [--pid PID]
//!
//! Codex by default. With `--claude`, `--copilot` or `--kiro`, that agent; `--pid` is the
//! process id of a running interactive session, whose transcript then gives the
//! context.

use peekme::agent::Agent;
use peekme::context;
use peekme::explain::{Explainer, Progress, Request};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let (text, selected) = (&args[1], &args[2]);
    let has = |f: &str| args.iter().any(|a| a == f);
    let agent = if has("--claude") {
        Agent::Claude
    } else if has("--copilot") {
        Agent::Copilot
    } else if has("--kiro") {
        Agent::Kiro
    } else {
        Agent::Codex
    };
    let agent_pid = args
        .iter()
        .position(|a| a == "--pid")
        .and_then(|i| args.get(i + 1))
        .and_then(|p| p.parse().ok());
    let chars: Vec<char> = text.chars().collect();
    let sel: Vec<char> = selected.chars().collect();
    let start = (0..=chars.len() - sel.len())
        .rev()
        .find(|&i| chars[i..i + sel.len()] == *sel)
        .expect("selection not in text");
    let req = Request {
        screen: context::ScreenSel {
            text: chars,
            start,
            end: start + sel.len(),
        },
        cwd: std::env::current_dir()
            .unwrap()
            .to_string_lossy()
            .into_owned(),
        max_lines: 10,
        deep: has("--deep"),
        force: true,
        agent_pid,
        nested: None,
    };
    // SAFETY: single-threaded at this point.
    unsafe { std::env::set_var("PEEKME_DEBUG_PROMPT", "1") };
    let t0 = std::time::Instant::now();
    let explainer = Explainer::new(agent);
    explainer.explain(req, |p| match p {
        Progress::Started { model, source } => {
            let prompt =
                std::fs::read_to_string(std::env::temp_dir().join("peekme-last-prompt.txt"))
                    .unwrap_or_default();
            println!(
                "===== prompt ({} chars) =====\n{prompt}\n===== {model} · {source} · {:.1}s =====",
                prompt.chars().count(),
                t0.elapsed().as_secs_f32()
            );
        }
        Progress::Delta(d) => print!("{d}"),
        Progress::Done => println!("\n===== done in {:.1}s =====", t0.elapsed().as_secs_f32()),
        Progress::Failed(e) => println!("\n===== failed: {e} ====="),
        Progress::TooBig { tokens, ratio } => println!("too big: {tokens} tokens ({ratio}x)"),
    });
    explainer.shutdown();
}
