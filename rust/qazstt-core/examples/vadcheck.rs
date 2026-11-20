//! VAD diagnostic: how much of each file does the segmenter keep?
//!
//!   cargo run --release -p qazstt-core --no-default-features --example vadcheck -- a.wav b.wav ...
//!
//! Prints per file: duration, speech-level stats, segments, kept fraction.
//! A file with 0 segments would produce an empty transcript.

use qazstt_core::{audio, vad::{Segmenter, SegmenterConfig}};

fn main() {
    let files: Vec<String> = std::env::args().skip(1).collect();
    let (mut zero, mut total, mut low_cov) = (0, 0, 0);
    for f in &files {
        let pcm = audio::read_wav(std::path::Path::new(f)).expect("wav");
        let x = pcm.to_engine_format();
        let dur = x.len() as f32 / 16_000.0;
        let mut frames: Vec<f32> = x.chunks(320).map(|c| (c.iter().map(|s| s * s).sum::<f32>() / c.len() as f32).sqrt()).collect();
        frames.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let p = |q: f32| frames[((frames.len() - 1) as f32 * q) as usize];
        let mut cfg = SegmenterConfig::default();
        if let Ok(v) = std::env::var("VAD_MIN_RMS") { cfg.vad_min_rms = v.parse().unwrap(); }
        if let Ok(v) = std::env::var("VAD_SNR") { cfg.vad_snr = v.parse().unwrap(); }
        let mut seg = Segmenter::new(cfg);
        seg.push(&x);
        let mut segs = seg.take_ready();
        segs.extend(seg.flush());
        let kept: f32 = segs.iter().map(|s| s.samples.len() as f32 / 16_000.0).sum();
        total += 1;
        if segs.is_empty() {
            zero += 1;
        }
        if kept / dur < 0.6 {
            low_cov += 1;
        }
        let name = std::path::Path::new(f).file_name().unwrap().to_string_lossy();
        println!(
            "{name:>26} {dur:5.1}s  rms p10 {:.4} p50 {:.4} p90 {:.4}  segs {:2}  kept {:4.1}s ({:3.0}%)",
            p(0.1), p(0.5), p(0.9), segs.len(), kept, 100.0 * kept / dur
        );
    }
    println!("\n{total} files · {zero} with NO segments · {low_cov} with < 60% kept");
}
