//! Development helper: replay a captured byte stream through the same emulator
//! peekme uses and print the resulting screen.
//!
//!     cargo run --example vtdump -- <file> <cols> <rows> [byte-offset]

use peekme::shadow;

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
    if std::env::var_os("VTDUMP_UNDERLINE").is_some() {
        // Print runs of underlined cells (how peekme marks answered text).
        use alacritty_terminal::term::cell::Flags;
        for (r, row) in snap.rows.iter().enumerate() {
            let mut run = String::new();
            let blank = alacritty_terminal::term::cell::Cell::default();
            for cell in row.iter().chain(std::iter::once(&blank)) {
                if cell.flags.intersects(Flags::ALL_UNDERLINES) {
                    run.push(cell.c);
                } else if !run.is_empty() {
                    println!("-- underline row {r}: {run:?}");
                    run.clear();
                }
            }
        }
    }
    println!("-- cursor {:?}", snap.cursor);
}
