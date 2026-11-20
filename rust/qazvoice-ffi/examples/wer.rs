//! Word error rate + speed on a manifest (jsonl lines {"audio","text"}) through
//! the plugin's C ABI. `cargo run --release --example wer -- <stt-dir> <manifest> [punct-dir]`
use std::ffi::{c_char, CStr, CString};
use std::time::Instant;

use qazvoice::*;
use serde_json::Value;

fn s(p: *mut c_char) -> String {
    unsafe {
        let r = CStr::from_ptr(p).to_str().unwrap().to_string();
        qv_string_free(p);
        r
    }
}

fn norm(t: &str) -> Vec<String> {
    t.to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() || c.is_whitespace() { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .map(String::from)
        .collect()
}

fn edits(a: &[String], b: &[String]) -> usize {
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for i in 1..=a.len() {
        let mut cur = vec![i; b.len() + 1];
        for j in 1..=b.len() {
            cur[j] = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + (a[i - 1] != b[j - 1]) as usize);
        }
        prev = cur;
    }
    prev[b.len()]
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mut cfg = serde_json::json!({"model_path": args[1], "threads": 4});
    if let Some(p) = args.get(3) {
        cfg["punct_model_path"] = p.clone().into();
    }
    let cfg = CString::new(cfg.to_string()).unwrap();
    let mut err: *mut c_char = std::ptr::null_mut();
    let t0 = Instant::now();
    let h = unsafe { qv_stt_new(cfg.as_ptr(), &mut err) };
    assert!(!h.is_null());
    eprintln!("load {:?}", t0.elapsed());
    let (mut e, mut n, mut audio_s, mut busy) = (0usize, 0usize, 0f64, 0f64);
    for line in std::fs::read_to_string(&args[2]).unwrap().lines() {
        let v: Value = serde_json::from_str(line).unwrap();
        let path = v["audio"].as_str().unwrap().replace("/qazstt/data/", "/qazstt/repo/data/");
        let pcm = qazstt_core::read_wav(std::path::Path::new(&path)).unwrap();
        audio_s += pcm.duration_secs() as f64;
        let i16s: Vec<i16> = pcm.samples.iter().map(|x| (x * 32767.0) as i16).collect();
        let t = Instant::now();
        let mut out = String::new();
        unsafe {
            for c in i16s.chunks(pcm.sample_rate as usize / 10 * pcm.channels as usize) {
                let r: Value = serde_json::from_str(&s(qv_stt_feed_i16(h, c.as_ptr(), c.len(), pcm.sample_rate, pcm.channels))).unwrap();
                for f in r["finals"].as_array().unwrap() { out.push_str(f["text"].as_str().unwrap()); out.push(' '); }
            }
            let r: Value = serde_json::from_str(&s(qv_stt_finish(h))).unwrap();
            for f in r["finals"].as_array().unwrap() { out.push_str(f["text"].as_str().unwrap()); out.push(' '); }
        }
        busy += t.elapsed().as_secs_f64();
        let (r, hyp) = (norm(v["text"].as_str().unwrap()), norm(&out));
        e += edits(&r, &hyp);
        n += r.len();
    }
    unsafe { qv_stt_free(h) };
    println!("files audio={:.1}s  WER={:.2}%  compute={:.1}s  RTF={:.3} ({:.0}x real time)", audio_s, 100.0 * e as f64 / n as f64, busy, busy / audio_s, audio_s / busy);
}
