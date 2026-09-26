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
    println!("-- cursor {:?}", snap.cursor);
}
