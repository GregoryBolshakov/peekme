//! Development helper: replay a captured byte stream through the same emulator
//! peekme uses and print the screen after every synchronized update that ends
//! in `[from, to)`, and at each extra offset given, as JSON lines
//! `{"off": N, "rows": [...]}`.
//!
//!     cargo run --example vtframes -- <file> <cols> <rows> <from> <to> [offset...]

use peekme::shadow;

const SYNC_END: &[u8] = b"\x1b[?2026l";

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let bytes = std::fs::read(&args[1]).expect("read capture");
    let cols: u16 = args[2].parse().unwrap();
    let rows: u16 = args[3].parse().unwrap();
    let from: usize = args[4].parse().unwrap();
    let to = args[5].parse::<usize>().unwrap().min(bytes.len());
    let mut stops: Vec<usize> = args[6..].iter().map(|s| s.parse().unwrap()).collect();
    stops.extend(
        bytes
            .windows(SYNC_END.len())
            .enumerate()
            .filter(|(i, w)| *w == SYNC_END && (from..to).contains(&(i + SYNC_END.len())))
            .map(|(i, _)| i + SYNC_END.len()),
    );
    stops.sort_unstable();
    stops.dedup();
    let mut term = shadow::Shadow::new(cols, rows);
    let mut at = 0;
    for stop in stops {
        let stop = stop.min(bytes.len());
        term.advance(&bytes[at..stop]);
        at = stop;
        let snap = term.snapshot();
        let lines: Vec<String> = (0..rows as usize)
            .map(|r| snap.rows_text(r, r + 1).trim_end_matches('\n').to_string())
            .collect();
        println!("{}", serde_json::json!({"off": stop, "rows": lines}));
    }
}
