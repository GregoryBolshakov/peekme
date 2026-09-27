//! Development helper: replay a captured byte stream through the same emulator
//! peekme uses and print the resulting screen.
//!
//!     cargo run --example vtdump -- <file> <cols> <rows> [byte-offset]

#[allow(dead_code)]
#[path = "../src/shadow.rs"]
mod shadow;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let bytes = std::fs::read(&args[1]).expect("read capture");
    let cols: u16 = args[2].parse().unwrap();
    let rows: u16 = args[3].parse().unwrap();
    let end = args.get(4).map_or(bytes.len(), |s| {
        s.parse::<usize>().unwrap().min(bytes.len())
    });
    let mut term = shadow::Shadow::new(cols, rows);
    term.advance(&bytes[..end]);
    term.flush_sync();
    let snap = term.snapshot();
    print!("{}", snap.rows_text(0, rows as usize));
    if std::env::var_os("VTDUMP_INVERSE").is_some() {
        // Print runs of reverse-video cells (how Codex highlights its own selection).
        for (r, row) in snap.rows.iter().enumerate() {
            let mut run = String::new();
            for cell in row {
                if cell
                    .flags
                    .contains(alacritty_terminal::term::cell::Flags::INVERSE)
                {
                    run.push(cell.c);
                } else if !run.is_empty() {
                    println!("-- inverse row {r}: {run:?}");
                    run.clear();
                }
            }
            if !run.is_empty() {
                println!("-- inverse row {r}: {run:?}");
            }
        }
    }
    println!("-- cursor {:?}", snap.cursor);
}
