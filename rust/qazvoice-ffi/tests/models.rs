//! End-to-end through the C ABI with the real models. Skipped unless
//! `QAZVOICE_MODELS` points at a directory with `tts-kk/`, `stt-kk/` and
//! (optionally) `punct-kk/` laid out like the plugin installs them.

use std::ffi::{c_char, CStr, CString};

use qazvoice::*;
use serde_json::Value;

fn s(p: *mut c_char) -> String {
    unsafe {
        let r = CStr::from_ptr(p).to_str().unwrap().to_string();
        qv_string_free(p);
        r
    }
}

fn models() -> Option<std::path::PathBuf> {
    std::env::var_os("QAZVOICE_MODELS").map(Into::into)
}

#[test]
fn tts_then_stt_round_trip() {
    let Some(dir) = models() else { return eprintln!("QAZVOICE_MODELS not set; skipped") };
    unsafe {
        let mut err: *mut c_char = std::ptr::null_mut();
        let path = CString::new(dir.join("tts-kk/voice.onnx").to_str().unwrap()).unwrap();
        let tts = qv_tts_new(path.as_ptr(), 4, &mut err);
        assert!(!tts.is_null(), "{}", s(err));
        let info: Value = serde_json::from_str(&s(qv_tts_info(tts))).unwrap();
        let rate = info["sample_rate"].as_u64().unwrap() as u32;
        assert_eq!(rate, 22050);
        assert_eq!(info["speakers"].as_array().unwrap().len(), 6);

        let text = "Бүгін ауа райы жақсы. Біз саябаққа барамыз.";
        let plan: Value = serde_json::from_str(&s(qv_tts_plan(CString::new(text).unwrap().as_ptr()))).unwrap();
        let mut pcm: Vec<i16> = vec![0; rate as usize / 2];
        for item in plan.as_array().unwrap() {
            let piece = CString::new(item["piece"].as_str().unwrap()).unwrap();
            let mut n = 0usize;
            let mut err: *mut c_char = std::ptr::null_mut();
            let p = qv_tts_synthesize(tts, piece.as_ptr(), 0, 1.0, &mut n, &mut err);
            assert!(!p.is_null(), "{}", s(err));
            pcm.extend_from_slice(std::slice::from_raw_parts(p, n));
            qv_pcm_free(p, n);
            pcm.extend(std::iter::repeat_n(0i16, rate as usize / 4));
        }
        pcm.extend(std::iter::repeat_n(0i16, rate as usize));
        qv_tts_free(tts);

        let mut cfg = serde_json::json!({"model_path": dir.join("stt-kk").to_str().unwrap(), "threads": 4});
        if dir.join("punct-kk/model.onnx").exists() {
            cfg["punct_model_path"] = dir.join("punct-kk").to_str().unwrap().into();
        }
        let cfg = CString::new(cfg.to_string()).unwrap();
        let mut err: *mut c_char = std::ptr::null_mut();
        let stt = qv_stt_new(cfg.as_ptr(), &mut err);
        assert!(!stt.is_null(), "{}", s(err));
        let info: Value = serde_json::from_str(&s(qv_stt_info(stt))).unwrap();
        assert_eq!(info["vad"], "silero");
        let mut text_out = String::new();
        for chunk in pcm.chunks(2205) {
            let v: Value = serde_json::from_str(&s(qv_stt_feed_i16(stt, chunk.as_ptr(), chunk.len(), rate, 1))).unwrap();
            assert!(v.get("error").is_none(), "{v}");
            for f in v["finals"].as_array().unwrap() {
                text_out.push_str(f["text"].as_str().unwrap());
                text_out.push(' ');
            }
        }
        let v: Value = serde_json::from_str(&s(qv_stt_finish(stt))).unwrap();
        for f in v["finals"].as_array().unwrap() {
            text_out.push_str(f["text"].as_str().unwrap());
        }
        qv_stt_free(stt);
        eprintln!("transcript: {text_out}");
        let low = text_out.to_lowercase();
        for w in ["бүгін", "ауа", "райы", "саябаққа"] {
            assert!(low.contains(w), "missing {w} in {text_out}");
        }
    }
}

#[test]
fn punctuation_model() {
    let Some(dir) = models() else { return };
    if !dir.join("punct-kk/model.onnx").exists() {
        return;
    }
    unsafe {
        let mut err: *mut c_char = std::ptr::null_mut();
        let p = qv_punct_new(CString::new(dir.join("punct-kk").to_str().unwrap()).unwrap().as_ptr(), 2, &mut err);
        assert!(!p.is_null(), "{}", s(err));
        let v: Value = serde_json::from_str(&s(qv_punct_apply(p, CString::new("сәлем қалың қалай мен жақсымын").unwrap().as_ptr()))).unwrap();
        let t = v["text"].as_str().unwrap();
        eprintln!("punct: {t}");
        assert!(t.starts_with('С'));
        assert!(t.contains('?') || t.contains('.') || t.contains(','));
        qv_punct_free(p);
    }
}
