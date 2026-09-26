//! Development helper: run one explanation for a selection inside some screen
//! text, without a terminal. Prints the prompt sent and the streamed answer.
//!
//!     cargo run --example peek_text -- "<screen text>" "<selection>" [--deep]

#[path = "../src/context.rs"]
mod context;
#[path = "../src/explain.rs"]
mod explain;
#[allow(dead_code)]
#[path = "../src/launch.rs"]
mod launch;

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let (text, selected) = (&args[1], &args[2]);
    let deep = args.iter().any(|a| a == "--deep");
    let chars: Vec<char> = text.chars().collect();
    let sel: Vec<char> = selected.chars().collect();
    let start = (0..=chars.len() - sel.len())
        .rev()
        .find(|&i| chars[i..i + sel.len()] == *sel)
        .expect("selection not in text");
    let screen = context::ScreenSel {
        text: chars,
        start,
        end: start + sel.len(),
    };
    let cwd = std::env::current_dir()?.to_string_lossy().into_owned();
    // SAFETY: single-threaded at this point.
    unsafe { std::env::set_var("PEEKME_DEBUG_PROMPT", "1") };
    let t0 = std::time::Instant::now();
    let server = explain::AppServer::start()?;
    let req = explain::Request {
        screen,
        cwd,
        max_lines: 10,
        deep,
    };
    explain::explain(&server, req, |p| match p {
        explain::Progress::Started { model, source } => {
            let prompt =
                std::fs::read_to_string(std::env::temp_dir().join("peekme-last-prompt.txt"))
                    .unwrap_or_default();
            println!(
                "===== prompt ({} chars) =====\n{prompt}\n===== {model} · {source} · {:.1}s =====",
                prompt.chars().count(),
                t0.elapsed().as_secs_f32()
            );
        }
        explain::Progress::Delta(d) => print!("{d}"),
        explain::Progress::Done => {
            println!("\n===== done in {:.1}s =====", t0.elapsed().as_secs_f32())
        }
        explain::Progress::Failed(e) => println!("\n===== failed: {e} ====="),
    });
    server.shutdown();
    Ok(())
}
