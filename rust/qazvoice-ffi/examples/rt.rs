//! Round trip one sentence: `rt <voice.onnx> <stt-dir> <text> <numbers_to_digits:0|1>`.
use std::ffi::{c_char, CStr, CString};
use qazvoice::*;
use serde_json::Value;
fn s(p: *mut c_char) -> String { unsafe { let r = CStr::from_ptr(p).to_str().unwrap().to_string(); qv_string_free(p); r } }
fn main() {
    let a: Vec<String> = std::env::args().collect();
    let mut err: *mut c_char = std::ptr::null_mut();
    let tts = unsafe { qv_tts_new(CString::new(a[1].clone()).unwrap().as_ptr(), 4, &mut err) };
    let plan: Value = serde_json::from_str(&s(unsafe { qv_tts_plan(CString::new(a[3].clone()).unwrap().as_ptr()) })).unwrap();
    let mut pcm = vec![0i16; 11025];
    for it in plan.as_array().unwrap() {
        let mut n = 0usize;
        let p = unsafe { qv_tts_synthesize(tts, CString::new(it["piece"].as_str().unwrap()).unwrap().as_ptr(), 3, 1.0, &mut n, &mut err) };
        pcm.extend_from_slice(unsafe { std::slice::from_raw_parts(p, n) });
        unsafe { qv_pcm_free(p, n) };
    }
    pcm.extend(vec![0i16; 22050]);
    let cfg = CString::new(serde_json::json!({"model_path": a[2], "numbers_to_digits": a[4] == "1", "capitalize": false}).to_string()).unwrap();
    let stt = unsafe { qv_stt_new(cfg.as_ptr(), &mut err) };
    let mut out = String::new();
    unsafe {
        let r: Value = serde_json::from_str(&s(qv_stt_feed_i16(stt, pcm.as_ptr(), pcm.len(), 22050, 1))).unwrap();
        for f in r["finals"].as_array().unwrap() { out += f["text"].as_str().unwrap(); out.push(' '); }
        let r: Value = serde_json::from_str(&s(qv_stt_finish(stt))).unwrap();
        for f in r["finals"].as_array().unwrap() { out += f["text"].as_str().unwrap(); }
    }
    println!("{out}");
}
