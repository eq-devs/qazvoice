//! Punctuate lines from stdin, or print SentencePiece ids with `--ids` (parity checks).
//!
//!   cargo run --release -p qazstt-core --example punct -- models/punct-kk [--ids] < lines.txt
use std::io::BufRead;
use std::path::Path;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let dir = Path::new(args.first().expect("model dir"));
    let ids_only = args.iter().any(|a| a == "--ids");
    let sp = qazstt_core::punct::SentencePiece::load(&dir.join("sp.model")).unwrap();
    let mut p = if ids_only { None } else { Some(qazstt_core::punct::Punctuator::load(dir, 4).unwrap()) };
    for line in std::io::stdin().lock().lines() {
        let line = line.unwrap();
        match p.as_mut() {
            None => println!("{}", sp.encode(&line).iter().map(|i| i.to_string()).collect::<Vec<_>>().join(" ")),
            Some(p) => println!("{}", p.apply(&line).unwrap()),
        }
    }
}
