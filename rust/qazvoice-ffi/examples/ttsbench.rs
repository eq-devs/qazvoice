//! TTS speed through the C ABI: `ttsbench <voice.onnx> <threads>`.
use std::ffi::{c_char, CStr, CString};
use std::time::Instant;

use qazvoice::*;
use serde_json::Value;

fn s(p: *mut c_char) -> String {
    unsafe { let r = CStr::from_ptr(p).to_str().unwrap().to_string(); qv_string_free(p); r }
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let threads: u32 = a[2].parse().unwrap();
    let text = std::env::var("QV_TEXT").unwrap_or_else(|_| "Бүгін Алматыда ауа райы өте жақсы болады, күн ашық, жел әлсіз, сондықтан саябақта серуендеуге болады. Ертең жаңбыр жаууы мүмкін, сондықтан қолшатыр алып шығыңыз. 2024 жылы қалада 25 мың жаңа ағаш отырғызылды.".into());
    let text = text.as_str();
    if std::env::var("QV_NOSPIN").is_ok() { qazstt_core::set_thread_spinning(false); }
    let t0 = Instant::now();
    let mut err: *mut c_char = std::ptr::null_mut();
    let path = CString::new(a[1].clone()).unwrap();
    let h = unsafe { qv_tts_new(path.as_ptr(), threads, &mut err) };
    assert!(!h.is_null(), "{}", s(err));
    let load = t0.elapsed();
    let plan: Value = serde_json::from_str(&s(unsafe { qv_tts_plan(CString::new(text).unwrap().as_ptr()) })).unwrap();
    let t1 = Instant::now();
    let mut first = None;
    let mut samples = 0usize;
    if std::env::var("QV_SHOW").is_ok() {
        for item in plan.as_array().unwrap() { eprintln!("piece: {}", item["piece"]); }
    }
    for item in plan.as_array().unwrap() {
        let piece = CString::new(item["piece"].as_str().unwrap()).unwrap();
        let mut n = 0usize;
        let p = unsafe { qv_tts_synthesize(h, piece.as_ptr(), 0, 1.0, &mut n, &mut err) };
        first.get_or_insert(t1.elapsed());
        samples += n;
        unsafe { qv_pcm_free(p, n) };
    }
    let total = t1.elapsed().as_secs_f64();
    let audio = samples as f64 / 22050.0;
    println!("threads={threads} load={:.2}s first_audio={:.2}s audio={:.1}s synth={:.2}s ({:.1}x real time)",
        load.as_secs_f64(), first.unwrap().as_secs_f64(), audio, total, audio / total);
    unsafe { qv_tts_free(h) };
}
