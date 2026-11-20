//! `unzst <in.zst> <out>`: time the decoder.
fn main() {
    let a: Vec<String> = std::env::args().collect();
    let t = std::time::Instant::now();
    let (n, h) = qazvoice::unpack::unzstd_file(a[1].as_ref(), a[2].as_ref(), 1 << 33).unwrap();
    println!("{n} bytes in {:.2}s sha256={h}", t.elapsed().as_secs_f64());
}
