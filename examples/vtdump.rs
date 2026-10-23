//! Development helper: replay a captured byte stream through the same emulator
//! peekme uses and print the resulting screen.
//!
//!     cargo run --example vtdump -- <file> <cols> <rows> [byte-offset]
//!
//! With `VTDUMP_HISTORY=N`, up to N rows of history come first.

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
    // VTDUMP_HISTORY=N: up to N rows of the terminal's history first.
    let history = std::env::var("VTDUMP_HISTORY")
        .ok()
        .and_then(|n| n.parse().ok());
    let snap = match history {
        Some(n) => term.snapshot_with_history(n),
        None => term.snapshot(),
    };
    print!("{}", snap.rows_text(0, snap.rows.len()));
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
    if std::env::var_os("VTDUMP_STYLE").is_some() {
        // Print each row's colours and flags at its first, third and last cells.
        for (r, row) in snap.rows.iter().enumerate() {
            let at = |c: usize| row.get(c).map(|x| (x.c, x.fg, x.bg, x.flags));
            println!(
                "-- style row {r}: {:?} {:?} {:?}",
                at(0),
                at(2),
                at(row.len() - 1)
            );
        }
    }
    if std::env::var_os("VTDUMP_RUNS").is_some() {
        // Each row as runs of one style: "[fg bg flags]text".
        use alacritty_terminal::term::cell::Flags;
        let name = |c: &alacritty_terminal::vte::ansi::Color| match c {
            alacritty_terminal::vte::ansi::Color::Named(n) => format!("{n:?}"),
            alacritty_terminal::vte::ansi::Color::Spec(c) => {
                format!("#{:02x}{:02x}{:02x}", c.r, c.g, c.b)
            }
            alacritty_terminal::vte::ansi::Color::Indexed(i) => format!("i{i}"),
        };
        for (r, row) in snap.rows.iter().enumerate() {
            let mut out = String::new();
            let mut pen = None;
            for cell in row {
                if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                    continue;
                }
                let flags = cell.flags & !(Flags::WIDE_CHAR | Flags::WRAPLINE);
                let now = (name(&cell.fg), name(&cell.bg), flags);
                if pen.as_ref() != Some(&now) {
                    let f = format!("{:?}", now.2)
                        .replace("Flags(", "")
                        .replace(')', "");
                    out.push_str(&format!("[{} {} {}]", now.0, now.1, f));
                    pen = Some(now);
                }
                out.push(cell.c);
            }
            if row.iter().any(|c| !c.c.is_whitespace()) {
                println!("{r:3} {}", out.trim_end());
            }
        }
    }
    println!("-- cursor {:?}", snap.cursor);
}
