//! C ABI used by the `qazvoice` Flutter plugin through `dart:ffi`.
//!
//! Conventions:
//! * Strings in and out are NUL-terminated UTF-8. Every `*mut c_char` returned
//!   must be released with [`qv_string_free`]; PCM buffers with [`qv_pcm_free`].
//! * Structured results are JSON, so no struct layout crosses the boundary.
//!   Failures come back as `{"error": "...", "kind": "..."}`.
//! * Every entry point catches panics, so a bug in the engine surfaces as an
//!   error string instead of aborting the host app.
//! * Handles are thread-safe (a mutex each), but the plugin drives each one
//!   from a single background isolate.

use std::ffi::{c_char, CStr, CString};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::Path;
use std::sync::Mutex;

use qazstt_core::{kazakh, tts, Engine, EngineConfig, Script, Transcript};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

mod text;

// ───────────────────────────── helpers ─────────────────────────────

fn out(s: String) -> *mut c_char {
    // Interior NULs cannot occur in our JSON (serde escapes them) but may in raw text.
    CString::new(s.replace('\0', "")).unwrap_or_default().into_raw()
}

fn out_json(v: Value) -> *mut c_char {
    out(v.to_string())
}

fn err_json(kind: &str, msg: impl std::fmt::Display) -> Value {
    json!({"error": msg.to_string(), "kind": kind})
}

/// # Safety
/// `p` must be null or a valid NUL-terminated string.
unsafe fn arg<'a>(p: *const c_char) -> Result<&'a str, String> {
    if p.is_null() {
        return Err("null string".into());
    }
    CStr::from_ptr(p).to_str().map_err(|_| "string is not UTF-8".to_string())
}

fn panic_msg(e: Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = e.downcast_ref::<&str>() {
        s.to_string()
    } else if let Some(s) = e.downcast_ref::<String>() {
        s.clone()
    } else {
        "panic".into()
    }
}

/// Runs `f`, turning a panic into an error JSON.
fn guard_json(f: impl FnOnce() -> Value) -> *mut c_char {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(v) => out_json(v),
        Err(e) => out_json(err_json("panic", panic_msg(e))),
    }
}

fn write_err(err_out: *mut *mut c_char, v: Value) {
    if !err_out.is_null() {
        // SAFETY: caller passes a valid pointer to a pointer slot (or null, checked).
        unsafe { *err_out = out_json(v) };
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    // A panic while holding the lock was already reported; keep serving.
    m.lock().unwrap_or_else(|p| p.into_inner())
}

fn backend_kind(e: &qazstt_core::BackendError) -> &'static str {
    match e {
        qazstt_core::BackendError::ModelLoad(_) => "model",
        _ => "inference",
    }
}

fn engine_err(e: qazstt_core::EngineError) -> Value {
    match &e {
        qazstt_core::EngineError::Config(_) => err_json("config", &e),
        qazstt_core::EngineError::Audio(_) => err_json("audio", &e),
        qazstt_core::EngineError::Backend(b) => err_json(backend_kind(b), &e),
    }
}

// ───────────────────────────── misc ─────────────────────────────

/// Version of the native library (the plugin checks it matches the Dart side).
#[no_mangle]
pub extern "C" fn qv_version() -> *const c_char {
    static V: std::sync::OnceLock<CString> = std::sync::OnceLock::new();
    V.get_or_init(|| CString::new(env!("CARGO_PKG_VERSION")).unwrap()).as_ptr()
}

/// # Safety
/// `s` must be null or a string returned by this library, not freed before.
#[no_mangle]
pub unsafe extern "C" fn qv_string_free(s: *mut c_char) {
    if !s.is_null() {
        drop(CString::from_raw(s));
    }
}

/// # Safety
/// `p`/`len` must come from one [`qv_tts_synthesize`] call, not freed before.
#[no_mangle]
pub unsafe extern "C" fn qv_pcm_free(p: *mut i16, len: usize) {
    if !p.is_null() {
        drop(Vec::from_raw_parts(p, len, len));
    }
}

// ───────────────────────────── text (no model) ─────────────────────────────

macro_rules! text_fn {
    ($name:ident, $f:expr) => {
        /// # Safety
        /// `s` must be a valid NUL-terminated UTF-8 string.
        #[no_mangle]
        pub unsafe extern "C" fn $name(s: *const c_char) -> *mut c_char {
            let input = match arg(s) {
                Ok(v) => v.to_string(),
                Err(e) => return out_json(err_json("argument", e)),
            };
            match catch_unwind(AssertUnwindSafe(|| ($f)(input.as_str()))) {
                Ok(r) => out(r),
                Err(_) => out(input),
            }
        }
    };
}

text_fn!(qv_to_latin, |s: &str| kazakh::to_latin(s));
text_fn!(qv_to_cyrillic, |s: &str| text::to_cyrillic(s));
text_fn!(qv_numbers_to_digits, |s: &str| kazakh::numbers_to_digits(s));
text_fn!(qv_spell_numbers, |s: &str| text::spell_numbers(s));

/// Text → JSON array of `{"text", "start", "end"}` sentences (UTF-16 offsets).
///
/// # Safety
/// `s` must be a valid NUL-terminated UTF-8 string.
#[no_mangle]
pub unsafe extern "C" fn qv_split(s: *const c_char) -> *mut c_char {
    let input = match arg(s) {
        Ok(v) => v.to_string(),
        Err(e) => return out_json(err_json("argument", e)),
    };
    guard_json(|| {
        let v: Vec<Value> = text::sentence_spans(&input)
            .into_iter()
            .map(|sp| json!({"text": &input[sp.byte_start..sp.byte_end], "start": sp.u16_start, "end": sp.u16_end}))
            .collect();
        Value::Array(v)
    })
}

// ───────────────────────────── text-to-speech ─────────────────────────────

pub struct QvTts {
    voice: Mutex<tts::Voice>,
    sample_rate: u32,
    speakers: Vec<String>,
}

/// Loads a Piper voice (`voice.onnx` + `voice.onnx.json`, or a directory with one pair).
/// Returns null and sets `*err_out` on failure.
///
/// # Safety
/// `path` must be a valid string; `err_out` null or a valid pointer slot.
#[no_mangle]
pub unsafe extern "C" fn qv_tts_new(path: *const c_char, threads: u32, err_out: *mut *mut c_char) -> *mut QvTts {
    let path = match arg(path) {
        Ok(p) => p.to_string(),
        Err(e) => {
            write_err(err_out, err_json("argument", e));
            return std::ptr::null_mut();
        }
    };
    let r = catch_unwind(AssertUnwindSafe(|| {
        let threads = if threads == 0 { 2 } else { threads as usize };
        tts::Voice::load(Path::new(&path), threads)
    }));
    match r {
        Ok(Ok(v)) => Box::into_raw(Box::new(QvTts { sample_rate: v.sample_rate(), speakers: v.speakers().to_vec(), voice: Mutex::new(v) })),
        Ok(Err(e)) => {
            write_err(err_out, err_json(backend_kind(&e), &e));
            std::ptr::null_mut()
        }
        Err(e) => {
            write_err(err_out, err_json("panic", panic_msg(e)));
            std::ptr::null_mut()
        }
    }
}

/// `{"sample_rate", "speakers": [...]}`.
///
/// # Safety
/// `h` must come from [`qv_tts_new`].
#[no_mangle]
pub unsafe extern "C" fn qv_tts_info(h: *const QvTts) -> *mut c_char {
    if h.is_null() {
        return out_json(err_json("argument", "null handle"));
    }
    let t = &*h;
    out_json(json!({"sample_rate": t.sample_rate, "speakers": t.speakers}))
}

/// Plans the synthesis of written text: JSON array of
/// `{"piece", "sentence", "start", "end", "pause"}` where `piece` is speakable
/// text for one synthesis call, `start`/`end` the UTF-16 span of its source
/// sentence in the input and `pause` whether a sentence pause follows it.
/// Needs no model.
///
/// # Safety
/// `s` must be a valid string.
#[no_mangle]
pub unsafe extern "C" fn qv_tts_plan(s: *const c_char) -> *mut c_char {
    let input = match arg(s) {
        Ok(v) => v.to_string(),
        Err(e) => return out_json(err_json("argument", e)),
    };
    guard_json(|| {
        let plan = text::tts_plan(&input);
        Value::Array(
            plan.into_iter()
                .map(|p| json!({"piece": p.piece, "sentence": p.sentence, "start": p.u16_start, "end": p.u16_end, "pause": p.pause}))
                .collect(),
        )
    })
}

/// One planned piece → 16-bit mono PCM at the voice's sample rate.
/// Returns the buffer (free with [`qv_pcm_free`]) and its length in `*out_len`;
/// null with `*out_len == 0` when there is nothing to say; null with `*err_out`
/// set on failure.
///
/// # Safety
/// `h` from [`qv_tts_new`]; `piece` a valid string; `out_len` valid; `err_out` null or valid.
#[no_mangle]
pub unsafe extern "C" fn qv_tts_synthesize(
    h: *const QvTts,
    piece: *const c_char,
    speaker: u32,
    speed: f32,
    out_len: *mut usize,
    err_out: *mut *mut c_char,
) -> *mut i16 {
    if !out_len.is_null() {
        *out_len = 0;
    }
    if h.is_null() || out_len.is_null() {
        write_err(err_out, err_json("argument", "null pointer"));
        return std::ptr::null_mut();
    }
    let piece = match arg(piece) {
        Ok(p) => p.to_string(),
        Err(e) => {
            write_err(err_out, err_json("argument", e));
            return std::ptr::null_mut();
        }
    };
    let t = &*h;
    let r = catch_unwind(AssertUnwindSafe(|| {
        let ph = tts::text::phonemize(&piece);
        if ph.is_empty() {
            return Ok(Vec::new());
        }
        let opts = tts::SynthOptions { speaker: Some(speaker as usize), speed, ..Default::default() };
        lock(&t.voice).synthesize_phonemes(&ph, &opts)
    }));
    match r {
        Ok(Ok(audio)) => {
            if audio.is_empty() {
                return std::ptr::null_mut();
            }
            let pcm: Vec<i16> = audio.iter().map(|&x| (x.clamp(-1.0, 1.0) * 32767.0) as i16).collect();
            let mut b = pcm.into_boxed_slice();
            *out_len = b.len();
            let p = b.as_mut_ptr();
            std::mem::forget(b);
            p
        }
        Ok(Err(e)) => {
            write_err(err_out, err_json(backend_kind(&e), &e));
            std::ptr::null_mut()
        }
        Err(e) => {
            write_err(err_out, err_json("panic", panic_msg(e)));
            std::ptr::null_mut()
        }
    }
}

/// # Safety
/// `h` must come from [`qv_tts_new`] and not be used afterwards.
#[no_mangle]
pub unsafe extern "C" fn qv_tts_free(h: *mut QvTts) {
    if !h.is_null() {
        let _ = catch_unwind(AssertUnwindSafe(|| drop(Box::from_raw(h))));
    }
}

// ───────────────────────────── speech-to-text ─────────────────────────────

#[derive(Deserialize)]
#[serde(default)]
struct SttConfig {
    model_path: String,
    punct_model_path: String,
    latin: bool,
    numbers_to_digits: bool,
    capitalize: bool,
    hotwords: Vec<String>,
    threads: u32,
    min_silence_ms: u32,
    max_segment_ms: u32,
}

impl Default for SttConfig {
    fn default() -> Self {
        Self {
            model_path: String::new(),
            punct_model_path: String::new(),
            latin: false,
            numbers_to_digits: false,
            capitalize: true,
            hotwords: Vec::new(),
            threads: 0,
            min_silence_ms: 0,
            max_segment_ms: 0,
        }
    }
}

const HOTWORD_RATIO: f32 = 0.25;

impl SttConfig {
    fn engine_config(&self) -> EngineConfig {
        let mut cfg = EngineConfig::new(&self.model_path);
        cfg.use_gpu = false;
        cfg.script = if self.latin { Script::Latin } else { Script::Cyrillic };
        cfg.numbers_to_digits = self.numbers_to_digits;
        cfg.capitalize = self.capitalize;
        cfg.hotwords = self.hotwords.clone();
        cfg.hotword_max_ratio = HOTWORD_RATIO;
        if self.threads > 0 {
            cfg.decode.threads = self.threads as usize;
        }
        if self.min_silence_ms > 0 {
            cfg.segmenter.min_silence_ms = self.min_silence_ms;
        }
        if self.max_segment_ms > 0 {
            cfg.segmenter.max_segment_ms = self.max_segment_ms;
        }
        if !self.punct_model_path.is_empty() {
            cfg.punct_model = Some(self.punct_model_path.clone().into());
        }
        cfg
    }
}

pub struct QvStt {
    engine: Mutex<Engine>,
}

#[derive(Serialize)]
struct Item {
    start_ms: u64,
    end_ms: u64,
    text: String,
}

fn items(v: Vec<Transcript>) -> Vec<Item> {
    v.into_iter().map(|t| Item { start_ms: t.start_ms, end_ms: t.end_ms, text: t.text }).collect()
}

fn build_engine(cfg: EngineConfig, mock: Option<String>) -> Result<Engine, Value> {
    if let Some(text) = mock {
        return Ok(Engine::with_backend(Box::new(qazstt_core::MockBackend { text }), cfg));
    }
    #[cfg(any(feature = "onnx", feature = "onnx-dynamic", feature = "onnx-static"))]
    {
        Engine::new(cfg).map_err(engine_err)
    }
    #[cfg(not(any(feature = "onnx", feature = "onnx-dynamic", feature = "onnx-static")))]
    {
        let _ = cfg;
        Err(err_json("model", "built without ONNX Runtime"))
    }
}

/// `config_json`: `{"model_path", "punct_model_path", "latin", "numbers_to_digits",
/// "capitalize", "hotwords": [...], "threads", "min_silence_ms", "max_segment_ms",
/// "mock_text"}`. `mock_text` (tests) swaps the acoustic model for one that
/// always "hears" that text. Returns null and sets `*err_out` on failure.
///
/// # Safety
/// `config_json` a valid string; `err_out` null or valid.
#[no_mangle]
pub unsafe extern "C" fn qv_stt_new(config_json: *const c_char, err_out: *mut *mut c_char) -> *mut QvStt {
    let src = match arg(config_json) {
        Ok(s) => s.to_string(),
        Err(e) => {
            write_err(err_out, err_json("argument", e));
            return std::ptr::null_mut();
        }
    };
    let r = catch_unwind(AssertUnwindSafe(|| -> Result<Engine, Value> {
        let raw: Value = serde_json::from_str(&src).map_err(|e| err_json("config", e))?;
        let mock = raw.get("mock_text").and_then(|v| v.as_str()).map(String::from);
        let cfg: SttConfig = serde_json::from_value(raw).map_err(|e| err_json("config", e))?;
        if cfg.model_path.is_empty() && mock.is_none() {
            return Err(err_json("config", "model_path is required"));
        }
        build_engine(cfg.engine_config(), mock)
    }));
    match r {
        Ok(Ok(engine)) => Box::into_raw(Box::new(QvStt { engine: Mutex::new(engine) })),
        Ok(Err(v)) => {
            write_err(err_out, v);
            std::ptr::null_mut()
        }
        Err(e) => {
            write_err(err_out, err_json("panic", panic_msg(e)));
            std::ptr::null_mut()
        }
    }
}

fn feed_result(e: &mut Engine, r: Result<Vec<Transcript>, qazstt_core::EngineError>) -> Value {
    match r {
        Ok(v) => json!({"finals": items(v), "speaking": e.open_utterance_ms().is_some()}),
        Err(err) => engine_err(err),
    }
}

/// Push interleaved 16-bit PCM. → `{"finals": [...], "speaking": bool}`.
///
/// # Safety
/// `h` from [`qv_stt_new`]; `pcm` points to `n` samples (or `n == 0`).
#[no_mangle]
pub unsafe extern "C" fn qv_stt_feed_i16(h: *const QvStt, pcm: *const i16, n: usize, rate: u32, channels: u16) -> *mut c_char {
    if h.is_null() || (pcm.is_null() && n > 0) {
        return out_json(err_json("argument", "null pointer"));
    }
    let samples: &[i16] = if n == 0 { &[] } else { std::slice::from_raw_parts(pcm, n) };
    guard_json(|| {
        let mut e = lock(&(*h).engine);
        let r = e.feed_i16(samples, rate, channels);
        feed_result(&mut e, r)
    })
}

/// Push interleaved float PCM in [-1, 1]. Same result as [`qv_stt_feed_i16`].
///
/// # Safety
/// `h` from [`qv_stt_new`]; `pcm` points to `n` samples (or `n == 0`).
#[no_mangle]
pub unsafe extern "C" fn qv_stt_feed_f32(h: *const QvStt, pcm: *const f32, n: usize, rate: u32, channels: u16) -> *mut c_char {
    if h.is_null() || (pcm.is_null() && n > 0) {
        return out_json(err_json("argument", "null pointer"));
    }
    let samples: &[f32] = if n == 0 { &[] } else { std::slice::from_raw_parts(pcm, n) };
    guard_json(|| {
        let mut e = lock(&(*h).engine);
        let r = e.feed(samples, rate, channels);
        feed_result(&mut e, r)
    })
}

/// Interim text of the open utterance → `{"partial": {...} | null}`.
///
/// # Safety
/// `h` from [`qv_stt_new`].
#[no_mangle]
pub unsafe extern "C" fn qv_stt_partial(h: *const QvStt) -> *mut c_char {
    if h.is_null() {
        return out_json(err_json("argument", "null handle"));
    }
    guard_json(|| match lock(&(*h).engine).partial() {
        Ok(p) => json!({"partial": p.map(|t| Item { start_ms: t.start_ms, end_ms: t.end_ms, text: t.text })}),
        Err(e) => engine_err(e),
    })
}

/// End of stream → `{"finals": [...], "speaking": false}`. The engine is reset
/// for the next stream afterwards.
///
/// # Safety
/// `h` from [`qv_stt_new`].
#[no_mangle]
pub unsafe extern "C" fn qv_stt_finish(h: *const QvStt) -> *mut c_char {
    if h.is_null() {
        return out_json(err_json("argument", "null handle"));
    }
    guard_json(|| {
        let mut e = lock(&(*h).engine);
        let r = e.finish();
        e.reset();
        match r {
            Ok(v) => json!({"finals": items(v), "speaking": false}),
            Err(err) => engine_err(err),
        }
    })
}

/// Drop buffered audio (keeps the model).
///
/// # Safety
/// `h` from [`qv_stt_new`].
#[no_mangle]
pub unsafe extern "C" fn qv_stt_reset(h: *const QvStt) {
    if !h.is_null() {
        let _ = catch_unwind(AssertUnwindSafe(|| lock(&(*h).engine).reset()));
    }
}

/// `hotwords_json`: JSON array of strings. → `{}` or an error.
///
/// # Safety
/// `h` from [`qv_stt_new`]; `hotwords_json` a valid string.
#[no_mangle]
pub unsafe extern "C" fn qv_stt_set_hotwords(h: *const QvStt, hotwords_json: *const c_char) -> *mut c_char {
    if h.is_null() {
        return out_json(err_json("argument", "null handle"));
    }
    let src = match arg(hotwords_json) {
        Ok(s) => s.to_string(),
        Err(e) => return out_json(err_json("argument", e)),
    };
    guard_json(|| match serde_json::from_str::<Vec<String>>(&src) {
        Ok(words) => {
            lock(&(*h).engine).set_hotwords(&words, HOTWORD_RATIO);
            json!({})
        }
        Err(e) => err_json("argument", e),
    })
}

/// `{"vad", "punctuation", "backend"}`.
///
/// # Safety
/// `h` from [`qv_stt_new`].
#[no_mangle]
pub unsafe extern "C" fn qv_stt_info(h: *const QvStt) -> *mut c_char {
    if h.is_null() {
        return out_json(err_json("argument", "null handle"));
    }
    guard_json(|| {
        let e = lock(&(*h).engine);
        json!({"vad": e.vad_name(), "punctuation": e.has_punctuation(), "backend": e.backend_name()})
    })
}

/// # Safety
/// `h` must come from [`qv_stt_new`] and not be used afterwards.
#[no_mangle]
pub unsafe extern "C" fn qv_stt_free(h: *mut QvStt) {
    if !h.is_null() {
        let _ = catch_unwind(AssertUnwindSafe(|| drop(Box::from_raw(h))));
    }
}

// ───────────────────────────── punctuation ─────────────────────────────

#[cfg(any(feature = "onnx", feature = "onnx-dynamic", feature = "onnx-static"))]
pub struct QvPunct {
    p: Mutex<qazstt_core::punct::Punctuator>,
}
#[cfg(not(any(feature = "onnx", feature = "onnx-dynamic", feature = "onnx-static")))]
pub struct QvPunct;

/// Loads the punctuation + true-casing model directory (`model.onnx`, `sp.model`, `config.yaml`).
///
/// # Safety
/// `dir` a valid string; `err_out` null or valid.
#[no_mangle]
pub unsafe extern "C" fn qv_punct_new(dir: *const c_char, threads: u32, err_out: *mut *mut c_char) -> *mut QvPunct {
    let dir = match arg(dir) {
        Ok(p) => p.to_string(),
        Err(e) => {
            write_err(err_out, err_json("argument", e));
            return std::ptr::null_mut();
        }
    };
    #[cfg(any(feature = "onnx", feature = "onnx-dynamic", feature = "onnx-static"))]
    {
        let r = catch_unwind(AssertUnwindSafe(|| qazstt_core::punct::Punctuator::load(Path::new(&dir), threads.clamp(1, 4) as usize)));
        match r {
            Ok(Ok(p)) => Box::into_raw(Box::new(QvPunct { p: Mutex::new(p) })),
            Ok(Err(e)) => {
                write_err(err_out, err_json(backend_kind(&e), &e));
                std::ptr::null_mut()
            }
            Err(e) => {
                write_err(err_out, err_json("panic", panic_msg(e)));
                std::ptr::null_mut()
            }
        }
    }
    #[cfg(not(any(feature = "onnx", feature = "onnx-dynamic", feature = "onnx-static")))]
    {
        let _ = (dir, threads);
        write_err(err_out, err_json("model", "built without ONNX Runtime"));
        std::ptr::null_mut()
    }
}

/// → `{"text": "..."}` or an error.
///
/// # Safety
/// `h` from [`qv_punct_new`]; `text` a valid string.
#[no_mangle]
pub unsafe extern "C" fn qv_punct_apply(h: *const QvPunct, text: *const c_char) -> *mut c_char {
    if h.is_null() {
        return out_json(err_json("argument", "null handle"));
    }
    let input = match arg(text) {
        Ok(s) => s.to_string(),
        Err(e) => return out_json(err_json("argument", e)),
    };
    #[cfg(any(feature = "onnx", feature = "onnx-dynamic", feature = "onnx-static"))]
    {
        guard_json(|| {
            // The model expects lowercase-ish ASR text; keep existing capitals (names).
            let cleaned = kazakh::clean(&input);
            if cleaned.is_empty() {
                return json!({"text": ""});
            }
            match lock(&(*h).p).apply(&cleaned) {
                Ok(t) => json!({"text": text::tidy_punctuation(&cleaned, &t)}),
                Err(e) => err_json("inference", e),
            }
        })
    }
    #[cfg(not(any(feature = "onnx", feature = "onnx-dynamic", feature = "onnx-static")))]
    {
        let _ = input;
        out_json(err_json("model", "built without ONNX Runtime"))
    }
}

/// # Safety
/// `h` must come from [`qv_punct_new`] and not be used afterwards.
#[no_mangle]
pub unsafe extern "C" fn qv_punct_free(h: *mut QvPunct) {
    if !h.is_null() {
        let _ = catch_unwind(AssertUnwindSafe(|| drop(Box::from_raw(h))));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(p: *mut c_char) -> String {
        unsafe {
            let r = CStr::from_ptr(p).to_str().unwrap().to_string();
            qv_string_free(p);
            r
        }
    }

    fn c(t: &str) -> CString {
        CString::new(t).unwrap()
    }

    #[test]
    fn text_functions() {
        unsafe {
            assert_eq!(s(qv_to_latin(c("Қазақстан").as_ptr())), "Qazaqstan");
            assert_eq!(s(qv_numbers_to_digits(c("жиырма бес адам").as_ptr())), "25 адам");
            assert_eq!(s(qv_to_cyrillic(c("Qazaqstan").as_ptr())), "Қазақстан");
            let v: Value = serde_json::from_str(&s(qv_split(c("Сәлем! Қалың қалай?").as_ptr()))).unwrap();
            assert_eq!(v.as_array().unwrap().len(), 2);
            assert_eq!(v[1]["text"], "Қалың қалай?");
            assert_eq!(v[1]["start"], 7);
        }
    }

    #[test]
    fn null_arguments_are_errors() {
        unsafe {
            let v: Value = serde_json::from_str(&s(qv_split(std::ptr::null()))).unwrap();
            assert!(v.get("error").is_some());
            let mut err: *mut c_char = std::ptr::null_mut();
            assert!(qv_tts_new(std::ptr::null(), 1, &mut err).is_null());
            assert!(s(err).contains("null"));
            let v: Value = serde_json::from_str(&s(qv_stt_feed_i16(std::ptr::null(), std::ptr::null(), 0, 16000, 1))).unwrap();
            assert!(v.get("error").is_some());
        }
    }

    #[test]
    fn missing_model_is_a_model_error() {
        unsafe {
            let mut err: *mut c_char = std::ptr::null_mut();
            let h = qv_tts_new(c("/nonexistent/voice.onnx").as_ptr(), 1, &mut err);
            assert!(h.is_null());
            let v: Value = serde_json::from_str(&s(err)).unwrap();
            assert!(v["error"].as_str().unwrap().len() > 0);
            let mut err: *mut c_char = std::ptr::null_mut();
            let h = qv_stt_new(c(r#"{"model_path":"/nonexistent"}"#).as_ptr(), &mut err);
            assert!(h.is_null());
            let v: Value = serde_json::from_str(&s(err)).unwrap();
            assert_eq!(v["kind"], "config");
        }
    }

    #[test]
    fn mock_stt_session() {
        unsafe {
            let mut err: *mut c_char = std::ptr::null_mut();
            let h = qv_stt_new(c(r#"{"mock_text":"жиырма бес адам","numbers_to_digits":true}"#).as_ptr(), &mut err);
            assert!(!h.is_null());
            let mut pcm: Vec<i16> = vec![0; 8000];
            pcm.extend((0..16000).map(|i| ((i as f32 * 0.07).sin() * 9000.0) as i16));
            let v: Value = serde_json::from_str(&s(qv_stt_feed_i16(h, pcm.as_ptr(), pcm.len(), 16000, 1))).unwrap();
            assert_eq!(v["speaking"], true);
            let p: Value = serde_json::from_str(&s(qv_stt_partial(h))).unwrap();
            assert_eq!(p["partial"]["text"], "25 адам");
            let v: Value = serde_json::from_str(&s(qv_stt_set_hotwords(h, c(r#"["Алматы"]"#).as_ptr()))).unwrap();
            assert!(v.get("error").is_none());
            let f: Value = serde_json::from_str(&s(qv_stt_finish(h))).unwrap();
            assert_eq!(f["finals"][0]["text"], "25 адам");
            let info: Value = serde_json::from_str(&s(qv_stt_info(h))).unwrap();
            assert_eq!(info["vad"], "energy");
            qv_stt_free(h);
        }
    }

    #[test]
    fn plan_has_spans() {
        unsafe {
            let v: Value = serde_json::from_str(&s(qv_tts_plan(c("Сәлем, әлем! 2024 жыл.").as_ptr()))).unwrap();
            let a = v.as_array().unwrap();
            assert_eq!(a.len(), 2);
            assert_eq!(a[0]["start"], 0);
            assert_eq!(a[0]["end"], 12);
            assert_eq!(a[0]["pause"], true);
            assert!(a[1]["piece"].as_str().unwrap().starts_with("екі мың"));
        }
    }

    #[test]
    fn pcm_roundtrip_free() {
        unsafe {
            let b = vec![1i16, 2, 3].into_boxed_slice();
            let len = b.len();
            let p = Box::into_raw(b) as *mut i16;
            qv_pcm_free(p, len);
        }
    }
}
