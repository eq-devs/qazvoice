//! Voice quality proxy: synthesize sentences, recognize them back, report WER and
//! speed. `ttsqual <voice.onnx> <stt-dir> <manifest.jsonl> <n> [wav-out-dir]`
use std::ffi::{c_char, CStr, CString};
use std::time::Instant;

use qazvoice::*;
use serde_json::Value;

fn s(p: *mut c_char) -> String {
    unsafe { let r = CStr::from_ptr(p).to_str().unwrap().to_string(); qv_string_free(p); r }
}

fn norm(t: &str) -> Vec<String> {
    t.to_lowercase().chars().map(|c| if c.is_alphanumeric() || c.is_whitespace() { c } else { ' ' })
        .collect::<String>().split_whitespace().map(String::from).collect()
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
    let a: Vec<String> = std::env::args().collect();
    let n: usize = a[4].parse().unwrap();
    let mut err: *mut c_char = std::ptr::null_mut();
    let t0 = Instant::now();
    let tts = unsafe { qv_tts_new(CString::new(a[1].clone()).unwrap().as_ptr(), 4, &mut err) };
    assert!(!tts.is_null(), "{}", s(err));
    let load = t0.elapsed().as_secs_f64();
    let cfg = CString::new(serde_json::json!({"model_path": a[2], "threads": 4, "numbers_to_digits": false}).to_string()).unwrap();
    let stt = unsafe { qv_stt_new(cfg.as_ptr(), &mut err) };
    assert!(!stt.is_null());
    let (mut e, mut words, mut synth, mut audio) = (0usize, 0usize, 0f64, 0f64);
    for (k, line) in std::fs::read_to_string(&a[3]).unwrap().lines().take(n).enumerate() {
        let v: Value = serde_json::from_str(line).unwrap();
        let text = v["text"].as_str().unwrap();
        let plan: Value = serde_json::from_str(&s(unsafe { qv_tts_plan(CString::new(text).unwrap().as_ptr()) })).unwrap();
        let mut pcm: Vec<i16> = vec![0; 11025];
        let mut spoken = String::new();
        let t = Instant::now();
        for item in plan.as_array().unwrap() {
            spoken.push_str(item["piece"].as_str().unwrap());
            spoken.push(' ');
            let piece = CString::new(item["piece"].as_str().unwrap()).unwrap();
            let mut len = 0usize;
            let p = unsafe { qv_tts_synthesize(tts, piece.as_ptr(), 0, 1.0, &mut len, &mut err) };
            if !p.is_null() {
                pcm.extend_from_slice(unsafe { std::slice::from_raw_parts(p, len) });
                unsafe { qv_pcm_free(p, len) };
            }
            pcm.extend(std::iter::repeat_n(0, 5512));
        }
        synth += t.elapsed().as_secs_f64();
        pcm.extend(std::iter::repeat_n(0, 22050));
        audio += pcm.len() as f64 / 22050.0;
        if let Some(dir) = a.get(5) {
            let f: Vec<f32> = pcm.iter().map(|&x| x as f32 / 32768.0).collect();
            std::fs::write(format!("{dir}/{k:02}.wav"), qazstt_core::audio::encode_wav_mono16(&f, 22050)).unwrap();
        }
        let mut out = String::new();
        unsafe {
            for c in pcm.chunks(2205) {
                let r: Value = serde_json::from_str(&s(qv_stt_feed_i16(stt, c.as_ptr(), c.len(), 22050, 1))).unwrap();
                for f in r["finals"].as_array().unwrap() { out.push_str(f["text"].as_str().unwrap()); out.push(' '); }
            }
            let r: Value = serde_json::from_str(&s(qv_stt_finish(stt))).unwrap();
            for f in r["finals"].as_array().unwrap() { out.push_str(f["text"].as_str().unwrap()); out.push(' '); }
        }
        // Reference = what the voice was asked to say (numbers already spelled out).
        let (r, h) = (norm(&spoken), norm(&out));
        e += edits(&r, &h);
        words += r.len();
    }
    println!("load={load:.2}s  synth {synth:.1}s for {audio:.1}s audio ({:.1}x real time)  round-trip WER={:.2}%",
        audio / synth, 100.0 * e as f64 / words as f64);
    unsafe { qv_tts_free(tts); qv_stt_free(stt) };
}
