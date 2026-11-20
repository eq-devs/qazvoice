//! Acoustic-model backends.
//!
//! The production backend is [`fastconformer::FastConformerCtc`]: NVIDIA
//! FastConformer-Hybrid (kk-ru) with its CTC head exported to ONNX, run through
//! ONNX Runtime (`ort` crate, feature `onnx`). Everything model-agnostic — CTC
//! greedy decoding, SentencePiece token handling, timestamps — lives here in
//! dependency-free Rust and is unit-tested without the runtime.

use std::fmt;
use std::path::{Path, PathBuf};

use crate::features::{MelExtractor, HOP_LENGTH, SAMPLE_RATE};

/// Encoder frame rate after FastConformer's 8× subsampling: 80 ms per output frame.
pub const SUBSAMPLING: usize = 8;
pub const FRAME_MS: u32 = (HOP_LENGTH * SUBSAMPLING * 1000 / SAMPLE_RATE) as u32; // 80

/// One decoded piece of text with timestamps relative to the audio given to
/// [`Backend::transcribe`].
#[derive(Debug, Clone, PartialEq)]
pub struct RawSegment {
    pub start_ms: u32,
    pub end_ms: u32,
    pub text: String,
}

#[derive(Debug)]
pub enum BackendError {
    ModelLoad(String),
    Inference(String),
    NotAvailable(&'static str),
}

impl fmt::Display for BackendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BackendError::ModelLoad(s) => write!(f, "model load failed: {s}"),
            BackendError::Inference(s) => write!(f, "inference failed: {s}"),
            BackendError::NotAvailable(s) => write!(f, "backend not available: {s}"),
        }
    }
}
impl std::error::Error for BackendError {}

/// Decoding options.
#[derive(Debug, Clone)]
pub struct DecodeOptions {
    pub threads: usize,
    /// Split the CTC output into separate segments on gaps of blank frames at
    /// least this long (ms). 0 = never split (one segment per utterance).
    pub split_on_blank_ms: u32,
}

impl Default for DecodeOptions {
    fn default() -> Self {
        Self {
            threads: std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).min(8),
            split_on_blank_ms: 0,
        }
    }
}

pub trait Backend: Send {
    /// Transcribe one utterance of 16 kHz mono f32 audio.
    fn transcribe(&mut self, audio: &[f32], opts: &DecodeOptions) -> Result<Vec<RawSegment>, BackendError>;
    fn name(&self) -> &'static str;
    /// Where the acoustic model runs: `"cpu"`, `"cuda"`, `"directml"` or `"coreml"`.
    fn device(&self) -> &'static str {
        "cpu"
    }
}

/// Returns a canned string for every segment — for tests and UI scaffolding.
pub struct MockBackend {
    pub text: String,
}

impl Backend for MockBackend {
    fn transcribe(&mut self, audio: &[f32], _opts: &DecodeOptions) -> Result<Vec<RawSegment>, BackendError> {
        let end_ms = (audio.len() as f32 / 16.0) as u32;
        Ok(vec![RawSegment { start_ms: 0, end_ms, text: self.text.clone() }])
    }
    fn name(&self) -> &'static str {
        "mock"
    }
}

// ───────────────────────── model files ─────────────────────────

/// Locates `encoder.onnx` (or `*.onnx`) and `tokens.txt` given a directory or a file path.
#[derive(Debug, Clone)]
pub struct ModelFiles {
    pub encoder: PathBuf,
    pub tokens: PathBuf,
}

impl ModelFiles {
    pub fn locate(model_path: &Path) -> Result<Self, BackendError> {
        let (dir, encoder) = if model_path.is_dir() {
            let enc = ["encoder.onnx", "model.onnx", "encoder.int8.onnx"]
                .iter()
                .map(|n| model_path.join(n))
                .find(|p| p.exists())
                .or_else(|| {
                    std::fs::read_dir(model_path).ok()?.flatten().map(|e| e.path()).find(|p| {
                        p.extension().map(|e| e == "onnx").unwrap_or(false)
                    })
                })
                .ok_or_else(|| BackendError::ModelLoad(format!("no .onnx in {}", model_path.display())))?;
            (model_path.to_path_buf(), enc)
        } else {
            let dir = model_path.parent().unwrap_or(Path::new(".")).to_path_buf();
            (dir, model_path.to_path_buf())
        };
        let tokens = dir.join("tokens.txt");
        if !encoder.exists() {
            return Err(BackendError::ModelLoad(format!("encoder not found: {}", encoder.display())));
        }
        if !tokens.exists() {
            return Err(BackendError::ModelLoad(format!("tokens.txt not found next to encoder: {}", tokens.display())));
        }
        Ok(Self { encoder, tokens })
    }
}

// ───────────────────────── tokens + CTC decoding ─────────────────────────

/// SentencePiece vocabulary as exported to `tokens.txt` (one `piece id` per line,
/// sherpa-onnx style, or one piece per line). The blank is the last id.
#[derive(Debug, Clone)]
pub struct Vocab {
    pieces: Vec<String>,
    pub blank_id: usize,
}

impl Vocab {
    pub fn load(path: &Path) -> Result<Self, BackendError> {
        let text = std::fs::read_to_string(path).map_err(|e| BackendError::ModelLoad(format!("{}: {e}", path.display())))?;
        Self::parse(&text)
    }

    pub fn parse(text: &str) -> Result<Self, BackendError> {
        let mut pieces: Vec<(usize, String)> = Vec::new();
        for (i, line) in text.lines().enumerate() {
            if line.is_empty() {
                continue;
            }
            // "piece id" or just "piece"
            let (piece, id) = match line.rsplit_once(' ') {
                Some((p, id)) if id.parse::<usize>().is_ok() => (p.to_string(), id.parse().unwrap()),
                _ => (line.to_string(), i),
            };
            pieces.push((id, piece));
        }
        if pieces.is_empty() {
            return Err(BackendError::ModelLoad("empty tokens.txt".into()));
        }
        pieces.sort_by_key(|(id, _)| *id);
        let max_id = pieces.last().unwrap().0;
        let mut table = vec![String::new(); max_id + 1];
        for (id, p) in pieces {
            table[id] = p;
        }
        // Blank: explicit <blk>/<blank> if present, else the last id.
        let blank_id = table
            .iter()
            .position(|p| matches!(p.as_str(), "<blk>" | "<blank>" | "<b>"))
            .unwrap_or(table.len() - 1);
        Ok(Self { pieces: table, blank_id })
    }

    pub fn len(&self) -> usize {
        self.pieces.len()
    }

    pub fn piece(&self, id: usize) -> &str {
        self.pieces.get(id).map(String::as_str).unwrap_or("")
    }
}

/// Greedy CTC decoding over `[T][V]` log-probabilities.
/// Returns segments split on long blank gaps (see [`DecodeOptions::split_on_blank_ms`]).
pub fn ctc_greedy_decode(logprobs: &[f32], n_frames: usize, vocab: &Vocab, opts: &DecodeOptions) -> Vec<RawSegment> {
    ctc_greedy_decode_frames(logprobs, n_frames, vocab, opts, FRAME_MS)
}

/// [`ctc_greedy_decode`] for an encoder whose output frame is `frame_ms` long
/// (FastConformer 80 ms, GigaAM 40 ms).
pub fn ctc_greedy_decode_frames(logprobs: &[f32], n_frames: usize, vocab: &Vocab, opts: &DecodeOptions, frame_ms: u32) -> Vec<RawSegment> {
    let v = vocab.len();
    if n_frames == 0 || logprobs.len() < n_frames * v {
        return Vec::new();
    }
    let split_frames = if opts.split_on_blank_ms == 0 { usize::MAX } else { (opts.split_on_blank_ms / frame_ms).max(1) as usize };

    let mut segments = Vec::new();
    let mut cur = String::new();
    let mut cur_start: Option<usize> = None;
    let mut cur_end = 0usize;
    let mut prev = vocab.blank_id;
    let mut blank_run = 0usize;

    let flush = |cur: &mut String, start: &mut Option<usize>, end: usize, out: &mut Vec<RawSegment>| {
        if let Some(s) = *start {
            let text = cur.trim().to_string();
            if !text.is_empty() {
                out.push(RawSegment {
                    start_ms: (s as u32) * frame_ms,
                    end_ms: ((end + 1) as u32) * frame_ms,
                    text,
                });
            }
        }
        cur.clear();
        *start = None;
    };

    for t in 0..n_frames {
        let row = &logprobs[t * v..(t + 1) * v];
        let mut best = 0usize;
        let mut best_v = f32::MIN;
        for (i, &p) in row.iter().enumerate() {
            if p > best_v {
                best_v = p;
                best = i;
            }
        }
        if best == vocab.blank_id {
            blank_run += 1;
            if blank_run >= split_frames && cur_start.is_some() {
                flush(&mut cur, &mut cur_start, cur_end, &mut segments);
            }
            prev = best;
            continue;
        }
        blank_run = 0;
        if best != prev {
            let piece = vocab.piece(best);
            if cur_start.is_none() {
                cur_start = Some(t);
            }
            cur_end = t;
            append_piece(&mut cur, piece);
        } else {
            cur_end = t;
        }
        prev = best;
    }
    flush(&mut cur, &mut cur_start, cur_end, &mut segments);
    segments
}

fn append_piece(out: &mut String, piece: &str) {
    if let Some(rest) = piece.strip_prefix('▁') {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(rest);
    } else if piece.starts_with('<') && piece.ends_with('>') {
        // <unk> and friends — drop
    } else {
        out.push_str(piece);
    }
}

// ───────────────────────── TDT (transducer) decoding ─────────────────────────

/// One prediction-network + joint step of a Token-and-Duration Transducer.
/// Implemented by the ONNX `decoder_joint` graph; mocked in tests.
pub trait TdtStep {
    type State: Clone;
    fn initial_state(&self) -> Self::State;
    /// Joint logits (`V+1` token logits, then one per duration) for encoder
    /// frame `frame` given the last emitted token, and the state after it.
    fn step(&mut self, frame: &[f32], last_token: usize, state: &Self::State) -> Result<(Vec<f32>, Self::State), BackendError>;
}

/// Greedy TDT decoding, step for step as NeMo's `GreedyTDTInfer`: per frame, emit
/// tokens until a non-zero duration moves time forward; a token updates the
/// decoder state, a blank does not; after `max_symbols` steps on one frame, advance by one.
/// `frames` yields the encoder output for frame `t`. Segments split like
/// [`ctc_greedy_decode`] (on gaps ≥ `split_on_blank_ms` between tokens).
pub fn tdt_greedy_decode<S: TdtStep>(
    n_frames: usize,
    frame: impl Fn(usize) -> Vec<f32>,
    step: &mut S,
    vocab: &Vocab,
    durations: &[usize],
    max_symbols: usize,
    opts: &DecodeOptions,
) -> Result<Vec<RawSegment>, BackendError> {
    let nd = durations.len();
    let blank = vocab.blank_id;
    let mut state = step.initial_state();
    let mut last = blank;
    let mut tokens: Vec<(usize, usize)> = Vec::new(); // (token, frame)
    let mut t = 0usize;
    while t < n_frames {
        let f = frame(t);
        let mut symbols = 0;
        while symbols < max_symbols {
            let (logits, next) = step.step(&f, last, &state)?;
            if logits.len() < nd + 1 {
                return Err(BackendError::Inference(format!("joint output too small: {}", logits.len())));
            }
            let (tok_logits, dur_logits) = logits.split_at(logits.len() - nd);
            let k = argmax(tok_logits);
            let skip = durations[argmax(dur_logits)];
            if k != blank {
                tokens.push((k, t));
                last = k;
                state = next;
            }
            symbols += 1;
            t += skip;
            if skip != 0 {
                break;
            }
        }
        if symbols == max_symbols {
            t += 1;
        }
    }

    let split_frames = if opts.split_on_blank_ms == 0 { usize::MAX } else { (opts.split_on_blank_ms / FRAME_MS).max(1) as usize };
    let mut segments = Vec::new();
    let mut cur = String::new();
    let (mut start, mut end) = (0usize, 0usize);
    for (i, &(k, ft)) in tokens.iter().enumerate() {
        if i > 0 && ft.saturating_sub(end) >= split_frames {
            push_segment(&mut segments, &mut cur, start, end);
        }
        if cur.is_empty() {
            start = ft;
        }
        end = ft;
        append_piece(&mut cur, vocab.piece(k));
    }
    push_segment(&mut segments, &mut cur, start, end);
    Ok(segments)
}

fn push_segment(out: &mut Vec<RawSegment>, cur: &mut String, start: usize, end: usize) {
    let text = cur.trim();
    if !text.is_empty() {
        out.push(RawSegment { start_ms: start as u32 * FRAME_MS, end_ms: (end as u32 + 1) * FRAME_MS, text: text.to_string() });
    }
    cur.clear();
}

fn argmax(v: &[f32]) -> usize {
    let mut best = 0;
    for (i, &x) in v.iter().enumerate() {
        if x > v[best] {
            best = i;
        }
    }
    best
}

/// Shared pre/post processing so any ONNX runtime wrapper only has to run the graph.
pub struct CtcPipeline {
    pub vocab: Vocab,
    pub mel: MelExtractor,
}

impl CtcPipeline {
    pub fn new(vocab: Vocab) -> Self {
        Self { vocab, mel: MelExtractor::new() }
    }

    /// Features for the encoder: `[1, 80, T]` row-major, `T`, and the valid
    /// length to feed as `length` (NeMo: `T - 1`, last frame masked).
    pub fn features(&self, audio: &[f32]) -> (Vec<f32>, usize, usize) {
        // The encoder's 8× subsampling needs a minimum length; pad very short input to 0.5 s.
        let padded;
        let a: &[f32] = if audio.len() < SAMPLE_RATE / 2 {
            padded = { let mut p = audio.to_vec(); p.resize(SAMPLE_RATE / 2, 0.0); p };
            &padded
        } else {
            audio
        };
        let (f, t) = self.mel.compute(a);
        (f, t, MelExtractor::valid_frames(a.len()).min(t))
    }
}

// ───────────────────────── ONNX Runtime backend ─────────────────────────

#[cfg(feature = "ort-backend")]
pub mod fastconformer {
    use super::*;
    use ort::environment::Environment;
    use ort::session::{builder::GraphOptimizationLevel, Session};
    use ort::value::Tensor;

    /// FastConformer CTC encoder (NeMo export) via ONNX Runtime.
    ///
    /// Expected graph: inputs `audio_signal` f32 `[B, 80, T]`, `length` i64 `[B]`;
    /// output `logprobs` f32 `[B, T', V]` (NeMo's `model.export()` with
    /// `decoder_type="ctc"` produces exactly this).
    pub struct FastConformerCtc {
        session: Session,
        pipe: CtcPipeline,
        input_signal: String,
        input_length: Option<String>,
        device: &'static str,
    }

    impl FastConformerCtc {
        pub fn load(model_path: &Path, threads: usize, use_gpu: bool) -> Result<Self, BackendError> {
            let files = ModelFiles::locate(model_path)?;
            let vocab = Vocab::load(&files.tokens)?;

            let (session, device) = open_session(&files.encoder, threads, use_gpu)?;

            let names: Vec<String> = session.inputs().iter().map(|i| i.name().to_string()).collect();
            let input_signal = names
                .iter()
                .find(|n| n.contains("signal") || n.contains("audio") || n.contains("input"))
                .cloned()
                .unwrap_or_else(|| names.first().cloned().unwrap_or_else(|| "audio_signal".into()));
            let input_length = names.iter().find(|n| n.contains("length")).cloned();

            Ok(Self { session, pipe: CtcPipeline::new(vocab), input_signal, input_length, device })
        }

        pub fn vocab(&self) -> &Vocab {
            &self.pipe.vocab
        }
    }

    /// The process-wide ONNX Runtime environment. Since ort rc.11 every session is
    /// built against an explicit [`Environment`] and only one may exist per
    /// process, so it is created once here and shared by the encoder, the TDT
    /// joint, Silero VAD and the punctuation model.
    ///
    /// `onnx-dynamic`: dlopens libonnxruntime first. Uses `ORT_DYLIB_PATH` if
    /// set, else the platform library name (on Android the APK's
    /// `libonnxruntime.so` from onnxruntime-android is found by soname).
    pub(crate) fn environment() -> Result<&'static Environment, BackendError> {
        static ENV: std::sync::OnceLock<Result<Environment, String>> = std::sync::OnceLock::new();
        ENV.get_or_init(|| {
            #[cfg(feature = "onnx-dynamic")]
            let builder = {
                let lib = std::env::var("ORT_DYLIB_PATH").unwrap_or_else(|_| {
                    if cfg!(target_os = "macos") { "libonnxruntime.dylib".into() }
                    else if cfg!(target_os = "windows") { "onnxruntime.dll".into() }
                    else { "libonnxruntime.so".into() }
                });
                ort::init_from(&lib).map_err(|e| format!("loading {lib}: {e}"))?
            };
            #[cfg(not(feature = "onnx-dynamic"))]
            let builder = ort::init();
            builder.with_name("qazstt").build().map_err(|e| format!("ONNX Runtime environment: {e}"))
        })
        .as_ref()
        .map_err(|e| BackendError::ModelLoad(e.clone()))
    }

    /// Make sure ONNX Runtime is loaded before building any other session (Silero VAD).
    pub(crate) fn ensure_runtime() -> Result<(), BackendError> {
        environment().map(|_| ())
    }

    impl Backend for FastConformerCtc {
        fn transcribe(&mut self, audio: &[f32], opts: &DecodeOptions) -> Result<Vec<RawSegment>, BackendError> {
            let (feat, t, valid) = self.pipe.features(audio);
            let signal = Tensor::from_array(([1usize, crate::features::N_MELS, t], feat))
                .map_err(|e| BackendError::Inference(e.to_string()))?;
            let length = Tensor::from_array(([1usize], vec![valid as i64]))
                .map_err(|e| BackendError::Inference(e.to_string()))?;

            let outputs = if let Some(len_name) = &self.input_length {
                self.session
                    .run(ort::inputs![self.input_signal.as_str() => signal, len_name.as_str() => length])
                    .map_err(|e| BackendError::Inference(e.to_string()))?
            } else {
                self.session
                    .run(ort::inputs![self.input_signal.as_str() => signal])
                    .map_err(|e| BackendError::Inference(e.to_string()))?
            };

            // First output is logprobs [1, T', V]
            let (shape, data) = outputs[0]
                .try_extract_tensor::<f32>()
                .map_err(|e| BackendError::Inference(e.to_string()))?;
            let dims: Vec<usize> = shape.iter().map(|&d| d as usize).collect();
            if dims.len() != 3 {
                return Err(BackendError::Inference(format!("unexpected logprobs shape {dims:?}")));
            }
            let (mut t_out, v) = (dims[1], dims[2]);
            // Second output (if exported) is encoded_lengths [B]: decode only valid frames.
            if outputs.len() > 1 {
                if let Ok((_, lens)) = outputs[1].try_extract_tensor::<i64>() {
                    if let Some(&l) = lens.first() {
                        if l > 0 && (l as usize) < t_out {
                            t_out = l as usize;
                        }
                    }
                }
            }
            if v != self.pipe.vocab.len() {
                return Err(BackendError::Inference(format!(
                    "vocab size mismatch: model {v} vs tokens.txt {}",
                    self.pipe.vocab.len()
                )));
            }
            Ok(ctc_greedy_decode(&data[..t_out * v], t_out, &self.pipe.vocab, opts))
        }

        fn name(&self) -> &'static str {
            "fastconformer-ctc/onnxruntime"
        }

        fn device(&self) -> &'static str {
            self.device
        }
    }

    /// FastConformer TDT (transducer) via ONNX Runtime: `encoder.onnx` (encoder
    /// only, `outputs [1, D, T']`) + `decoder_joint.onnx`, as exported by
    /// `pipeline/07b_export_rnnt.py`. Durations and decoder state size come from
    /// `model.json`.
    pub struct FastConformerTdt {
        encoder: Session,
        joint: JointSession,
        pipe: CtcPipeline,
        durations: Vec<usize>,
        max_symbols: usize,
        device: &'static str,
    }

    struct JointSession {
        session: Session,
        state_shape: [usize; 3],
    }

    impl TdtStep for JointSession {
        type State = (Vec<f32>, Vec<f32>);
        fn initial_state(&self) -> Self::State {
            let n = self.state_shape.iter().product();
            (vec![0.0; n], vec![0.0; n])
        }
        fn step(&mut self, frame: &[f32], last: usize, st: &Self::State) -> Result<(Vec<f32>, Self::State), BackendError> {
            let inf = |e: ort::Error| BackendError::Inference(e.to_string());
            let enc = Tensor::from_array(([1usize, frame.len(), 1], frame.to_vec())).map_err(inf)?;
            let tgt = Tensor::from_array(([1usize, 1], vec![last as i32])).map_err(inf)?;
            let tlen = Tensor::from_array(([1usize], vec![1i32])).map_err(inf)?;
            let s1 = Tensor::from_array((self.state_shape, st.0.clone())).map_err(inf)?;
            let s2 = Tensor::from_array((self.state_shape, st.1.clone())).map_err(inf)?;
            let out = self
                .session
                .run(ort::inputs![
                    "encoder_outputs" => enc, "targets" => tgt, "target_length" => tlen,
                    "input_states_1" => s1, "input_states_2" => s2
                ])
                .map_err(inf)?;
            let (_, logits) = out["outputs"].try_extract_tensor::<f32>().map_err(inf)?;
            let (_, n1) = out["output_states_1"].try_extract_tensor::<f32>().map_err(inf)?;
            let (_, n2) = out["output_states_2"].try_extract_tensor::<f32>().map_err(inf)?;
            Ok((logits.to_vec(), (n1.to_vec(), n2.to_vec())))
        }
    }

    static SPIN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

    /// Whether ONNX Runtime worker threads busy-wait for the next piece of work
    /// (its default: lowest latency for one session on an idle machine). A
    /// server that keeps several sessions on a few cores should turn it off:
    /// spinning threads of an idle session take cores from the one that is working.
    /// Applies to sessions created afterwards.
    pub fn set_thread_spinning(on: bool) {
        SPIN.store(on, std::sync::atomic::Ordering::Relaxed);
    }

    /// The accelerator this build can use (`use_gpu`), or `"cpu"`.
    pub const ACCELERATOR: &str = if cfg!(feature = "cuda") {
        "cuda"
    } else if cfg!(feature = "directml") {
        "directml"
    } else if cfg!(feature = "coreml") {
        "coreml"
    } else {
        "cpu"
    };

    /// CPU session options shared by every model in the crate.
    ///
    /// `QAZSTT_CPU_ARENA=0` turns off ONNX Runtime's CPU memory arena: the arena
    /// keeps every buffer it ever grew to (a long utterance's activations stay
    /// allocated for the life of the process); without it, buffers go back to
    /// the system allocator after each run, at some cost in speed.
    pub(crate) fn cpu_builder(threads: usize) -> Result<ort::session::builder::SessionBuilder, ort::Error> {
        let env = environment().map_err(|e| ort::Error::new(e.to_string()))?;
        // GraphOptimizationLevel::All = rc.10's Level3 (rc.15's Level3 is ORT_ENABLE_LAYOUT,
        // which onnxruntime < 1.23 rejects).
        let builder = Session::builder(env)?
            .with_optimization_level(GraphOptimizationLevel::All)?
            .with_intra_threads(threads.max(1))?
            .with_intra_op_spinning(SPIN.load(std::sync::atomic::Ordering::Relaxed))?;
        if std::env::var("QAZSTT_CPU_ARENA").is_ok_and(|v| v == "0") {
            return Ok(builder.with_memory_pattern(false)?.with_execution_providers([ort::ep::CPU::default().with_arena_allocator(false).build()])?);
        }
        Ok(builder)
    }

    /// `QAZSTT_GPU_DEVICE` (default 0): which GPU. On Windows DirectML lists the
    /// integrated GPU first on Optimus laptops; 1 is then the discrete card.
    #[cfg(any(feature = "cuda", feature = "directml"))]
    fn gpu_device() -> i32 {
        let legacy = std::env::var("QAZSTT_DML_DEVICE").ok();
        std::env::var("QAZSTT_GPU_DEVICE").ok().or(legacy).and_then(|v| v.parse().ok()).unwrap_or(0)
    }

    /// The same options with the build's accelerator registered. Registration
    /// errors surface when the session is created (`error_on_failure`), so the
    /// caller can fall back to the CPU instead of silently running there.
    #[cfg(any(feature = "cuda", feature = "coreml", feature = "directml"))]
    fn gpu_builder(threads: usize) -> Result<ort::session::builder::SessionBuilder, ort::Error> {
        let builder = cpu_builder(threads)?;
        #[cfg(feature = "cuda")]
        let builder = {
            use ort::ep::cuda::ConvAlgorithmSearch as CuDNNConvAlgorithmSearch;
            use ort::ep::{ArenaExtendStrategy, CUDA as CUDAExecutionProvider};
            // Speech comes in a new length every time. The default (exhaustive)
            // cuDNN search benchmarks every convolution again for each new input
            // shape, which costs far more than it saves; the heuristic pick is instant.
            // The arena grows by what is asked for, not to the next power of two:
            // several models share one small card.
            let mut ep = CUDAExecutionProvider::default()
                .with_device_id(gpu_device())
                .with_conv_algorithm_search(CuDNNConvAlgorithmSearch::Heuristic)
                .with_arena_extend_strategy(ArenaExtendStrategy::SameAsRequested);
            // QAZSTT_GPU_MEM_MB: ceiling for this session's device memory arena.
            if let Some(mb) = std::env::var("QAZSTT_GPU_MEM_MB").ok().and_then(|v| v.parse::<usize>().ok()).filter(|&mb| mb > 0) {
                ep = ep.with_memory_limit(mb * 1024 * 1024);
            }
            builder.with_execution_providers([ep.build().error_on_failure()])?
        };
        #[cfg(feature = "coreml")]
        let builder = {
            use ort::ep::CoreML as CoreMLExecutionProvider;
            builder.with_execution_providers([CoreMLExecutionProvider::default().build().error_on_failure()])?
        };
        #[cfg(feature = "directml")]
        let builder = {
            // DirectML needs sequential execution and memory pattern off (ORT docs).
            use ort::ep::DirectML as DirectMLExecutionProvider;
            builder
                .with_memory_pattern(false)?
                .with_execution_providers([DirectMLExecutionProvider::default().with_device_id(gpu_device()).build().error_on_failure()])?
        };
        Ok(builder)
    }

    /// Open a model. With `use_gpu` and an accelerator in the build the session
    /// runs there; if the accelerator cannot be set up (driver or CUDA libraries
    /// missing, out of device memory) it says so on stderr and runs on the CPU.
    /// Returns the session and where it runs (`"cuda"`, …, `"cpu"`).
    pub(crate) fn open_session(path: &Path, threads: usize, use_gpu: bool) -> Result<(Session, &'static str), BackendError> {
        #[cfg(any(feature = "cuda", feature = "coreml", feature = "directml"))]
        if use_gpu {
            match gpu_builder(threads).and_then(|mut b| b.commit_from_file(path)) {
                Ok(s) => return Ok((s, ACCELERATOR)),
                Err(e) => eprintln!("warning: {}: {ACCELERATOR} unavailable ({e}); using the CPU", path.display()),
            }
        }
        #[cfg(not(any(feature = "cuda", feature = "coreml", feature = "directml")))]
        let _ = use_gpu;
        Ok((cpu_session(path, threads)?, "cpu"))
    }

    /// A CPU session for `path`. With `QAZSTT_SHARE_WEIGHTS=1`, sessions of the
    /// same model file share one pre-packed weights container. Measured on the
    /// server (2 FastConformer engines) it saved nothing and cost 20–50 MB, so
    /// it is off by default; kept as a switch for other models and ORT versions.
    pub(crate) fn cpu_session(path: &Path, threads: usize) -> Result<Session, BackendError> {
        let err = |e: ort::Error| BackendError::ModelLoad(format!("{}: {e}", path.display()));
        let builder = cpu_builder(threads).map_err(err)?;
        let mut builder = match shared_weights(path) {
            Some(w) => builder.with_prepacked_weights(&w).map_err(|e| err(e.into()))?,
            None => builder,
        };
        builder.commit_from_file(path).map_err(err)
    }

    /// `ort` does not mark the container `Send`/`Sync` because it wraps a raw
    /// pointer, but ONNX Runtime's `PrepackedWeightsContainer` locks its own
    /// state and is made to be used by sessions on any thread; the registry
    /// below only clones the handle under a mutex.
    struct SharedWeights(ort::session::builder::PrepackedWeights);
    unsafe impl Send for SharedWeights {}
    unsafe impl Sync for SharedWeights {}

    /// Run options for the large models (encoder, punctuation, voice): ONNX
    /// Runtime hands the CPU arena's unused memory back after each run, so one
    /// long input does not keep its buffers for the life of the process, while
    /// each run still allocates from the arena. Measured on the TTS voice: same
    /// speed, about 270 MB less resident memory per voice after synthesis.
    /// `QAZSTT_ARENA_SHRINK=0` keeps the arena at its high-water mark instead.
    pub(crate) fn run_options() -> Option<&'static ort::session::RunOptions> {
        use std::sync::OnceLock;
        struct Opts(Option<ort::session::RunOptions>);
        unsafe impl Send for Opts {}
        unsafe impl Sync for Opts {}
        static OPTS: OnceLock<Opts> = OnceLock::new();
        OPTS.get_or_init(|| {
            Opts((!std::env::var("QAZSTT_ARENA_SHRINK").is_ok_and(|v| v == "0")).then(|| {
                let mut o = ort::session::RunOptions::new().expect("run options");
                o.set("memory.enable_memory_arena_shrinkage", "cpu:0").expect("arena shrinkage option");
                o
            }))
        })
        .0
        .as_ref()
    }

    fn shared_weights(path: &Path) -> Option<ort::session::builder::PrepackedWeights> {
        use std::collections::HashMap;
        use std::sync::{Mutex, OnceLock};
        static WEIGHTS: OnceLock<Mutex<HashMap<std::path::PathBuf, SharedWeights>>> = OnceLock::new();
        if !std::env::var("QAZSTT_SHARE_WEIGHTS").is_ok_and(|v| v == "1") {
            return None;
        }
        let key = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        let mut map = WEIGHTS.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner());
        Some(map.entry(key).or_insert_with(|| SharedWeights(ort::session::builder::PrepackedWeights::new())).0.clone())
    }

    /// `"key": [a, b, …]` or `"key": n` from the flat `model.json` (no JSON dependency).
    fn json_numbers(src: &str, key: &str) -> Option<Vec<usize>> {
        let rest = &src[src.find(&format!("\"{key}\""))? + key.len() + 2..];
        let rest = rest[rest.find(':')? + 1..].trim_start();
        let body = if let Some(r) = rest.strip_prefix('[') { &r[..r.find(']')?] } else { &rest[..rest.find([',', '}', '\n']).unwrap_or(rest.len())] };
        body.split(',').map(|x| x.trim().parse().ok()).collect()
    }

    impl FastConformerTdt {
        /// True if `model_path` (dir or encoder file) has a `decoder_joint.onnx` next to it.
        pub fn is_tdt_model(model_path: &Path) -> bool {
            let dir = if model_path.is_dir() { model_path } else { model_path.parent().unwrap_or(Path::new(".")) };
            dir.join("decoder_joint.onnx").exists()
        }

        pub fn load(model_path: &Path, threads: usize, use_gpu: bool) -> Result<Self, BackendError> {
            ensure_runtime()?;
            let files = ModelFiles::locate(model_path)?;
            let dir = files.tokens.parent().unwrap_or(Path::new(".")).to_path_buf();
            let vocab = Vocab::load(&files.tokens)?;
            let meta = std::fs::read_to_string(dir.join("model.json")).unwrap_or_default();
            let durations = json_numbers(&meta, "tdt_durations").unwrap_or_else(|| vec![0, 1, 2, 3, 4]);
            let max_symbols = json_numbers(&meta, "max_symbols_per_step").and_then(|v| v.first().copied()).unwrap_or(10);
            let layers = json_numbers(&meta, "pred_rnn_layers").and_then(|v| v.first().copied()).unwrap_or(1);
            let hidden = json_numbers(&meta, "pred_hidden").and_then(|v| v.first().copied()).unwrap_or(640);
            let (encoder, device) = open_session(&files.encoder, threads, use_gpu)?;
            // The joint runs once per emitted token / frame on tiny tensors: one thread is fastest.
            let joint = JointSession { session: open_session(&dir.join("decoder_joint.onnx"), 1, false)?.0, state_shape: [layers, 1, hidden] };
            Ok(Self { encoder, joint, pipe: CtcPipeline::new(vocab), durations, max_symbols, device })
        }
    }

    impl Backend for FastConformerTdt {
        fn transcribe(&mut self, audio: &[f32], opts: &DecodeOptions) -> Result<Vec<RawSegment>, BackendError> {
            let inf = |e: ort::Error| BackendError::Inference(e.to_string());
            let (feat, t, valid) = self.pipe.features(audio);
            let signal = Tensor::from_array(([1usize, crate::features::N_MELS, t], feat)).map_err(inf)?;
            let length = Tensor::from_array(([1usize], vec![valid as i64])).map_err(inf)?;
            let inputs = ort::inputs!["audio_signal" => signal, "length" => length];
            let out = match run_options() {
                Some(o) => self.encoder.run_with_options(inputs, o),
                None => self.encoder.run(inputs),
            }
            .map_err(inf)?;
            let (shape, enc) = out[0].try_extract_tensor::<f32>().map_err(inf)?;
            if shape.len() != 3 {
                return Err(BackendError::Inference(format!("unexpected encoder shape {shape:?}")));
            }
            let (d, t_out) = (shape[1] as usize, shape[2] as usize);
            let n = match out[1].try_extract_tensor::<i64>() {
                Ok((_, l)) => (l.first().copied().unwrap_or(t_out as i64).max(0) as usize).min(t_out),
                Err(_) => t_out,
            };
            // [1, D, T'] row-major: frame t is the strided column.
            let enc = enc.to_vec();
            drop(out);
            tdt_greedy_decode(n, |ti| (0..d).map(|k| enc[k * t_out + ti]).collect(), &mut self.joint, &self.pipe.vocab, &self.durations, self.max_symbols, opts)
        }

        fn name(&self) -> &'static str {
            "fastconformer-tdt/onnxruntime"
        }

        fn device(&self) -> &'static str {
            self.device
        }
    }

    /// GigaAM Multilingual character-CTC (ai-sage, MIT) via ONNX Runtime, as exported by
    /// `pipeline/09_gigaam.py`: inputs `features` f32 `[B, 64, T]` (see
    /// [`crate::features_gigaam`]) + `lengths` i64 `[B]`; outputs `log_probs` `[B, T/4, C]`
    /// + `out_lengths`. `tokens.txt` holds single characters, space as `▁`, blank last.
    pub struct GigaAmCtc {
        session: Session,
        vocab: Vocab,
        mel: crate::features_gigaam::GigaMel,
        device: &'static str,
    }

    /// GigaAM encoder frame: 10 ms hop × 4 subsampling.
    pub const GIGAAM_FRAME_MS: u32 = 40;

    impl GigaAmCtc {
        /// True if the model dir's `model.json` says it is a GigaAM / char-CTC export.
        pub fn is_gigaam_model(model_path: &Path) -> bool {
            let dir = if model_path.is_dir() { model_path } else { model_path.parent().unwrap_or(Path::new(".")) };
            std::fs::read_to_string(dir.join("model.json"))
                .map(|s| s.contains("GigaAM") || s.contains("\"ctc-char\""))
                .unwrap_or(false)
        }

        /// `model_path`: the directory (picks `model.onnx`, i.e. fp32) or a specific
        /// `.onnx` file such as `model.int8.onnx`.
        pub fn load(model_path: &Path, threads: usize, use_gpu: bool) -> Result<Self, BackendError> {
            ensure_runtime()?;
            let files = ModelFiles::locate(model_path)?;
            let vocab = Vocab::load(&files.tokens)?;
            let (session, device) = open_session(&files.encoder, threads, use_gpu)?;
            Ok(Self { session, vocab, mel: crate::features_gigaam::GigaMel::new(), device })
        }
    }

    impl Backend for GigaAmCtc {
        fn transcribe(&mut self, audio: &[f32], opts: &DecodeOptions) -> Result<Vec<RawSegment>, BackendError> {
            let inf = |e: ort::Error| BackendError::Inference(e.to_string());
            // Subsampling needs a few frames; pad very short input to 0.5 s.
            let padded;
            let a: &[f32] = if audio.len() < SAMPLE_RATE / 2 {
                padded = { let mut p = audio.to_vec(); p.resize(SAMPLE_RATE / 2, 0.0); p };
                &padded
            } else {
                audio
            };
            let (feat, t) = self.mel.compute(a);
            let n_mels = crate::features_gigaam::N_MELS;
            let features = Tensor::from_array(([1usize, n_mels, t], feat)).map_err(inf)?;
            let lengths = Tensor::from_array(([1usize], vec![t as i64])).map_err(inf)?;
            let out = self.session.run(ort::inputs!["features" => features, "lengths" => lengths]).map_err(inf)?;
            let (shape, data) = out[0].try_extract_tensor::<f32>().map_err(inf)?;
            if shape.len() != 3 {
                return Err(BackendError::Inference(format!("unexpected log_probs shape {shape:?}")));
            }
            let (t_out, v) = (shape[1] as usize, shape[2] as usize);
            if v != self.vocab.len() {
                return Err(BackendError::Inference(format!("vocab size mismatch: model {v} vs tokens.txt {}", self.vocab.len())));
            }
            let n = match out.len() > 1 {
                true => out[1].try_extract_tensor::<i64>().ok().and_then(|(_, l)| l.first().copied()).map(|l| (l.max(0) as usize).min(t_out)).unwrap_or(t_out),
                false => t_out,
            };
            let mut segs = ctc_greedy_decode_frames(&data[..n * v], n, &self.vocab, opts, GIGAAM_FRAME_MS);
            for s in &mut segs {
                s.text = s.text.split_whitespace().collect::<Vec<_>>().join(" ");
            }
            segs.retain(|s| !s.text.is_empty());
            Ok(segs)
        }

        fn name(&self) -> &'static str {
            "gigaam-ctc/onnxruntime"
        }

        fn device(&self) -> &'static str {
            self.device
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vocab() -> Vocab {
        // ids: 0 <unk>, 1 ▁сә, 2 лем, 3 ▁ә, 4 лем, 5 <blk>
        Vocab::parse("<unk> 0\n▁сә 1\nлем 2\n▁ә 3\nлем 4\n<blk> 5\n").unwrap()
    }

    fn logprobs_from_ids(ids: &[usize], v: usize) -> Vec<f32> {
        let mut out = vec![-10.0f32; ids.len() * v];
        for (t, &id) in ids.iter().enumerate() {
            out[t * v + id] = 0.0;
        }
        out
    }

    #[test]
    fn vocab_parses_sherpa_and_plain_formats() {
        let v = vocab();
        assert_eq!(v.len(), 6);
        assert_eq!(v.blank_id, 5);
        assert_eq!(v.piece(1), "▁сә");
        let plain = Vocab::parse("a\nb\nc\n").unwrap();
        assert_eq!(plain.blank_id, 2); // last id when no <blk>
    }

    #[test]
    fn greedy_ctc_collapses_repeats_and_blanks() {
        let v = vocab();
        let ids = [5, 5, 1, 1, 5, 2, 2, 5, 5, 3, 4, 5];
        let lp = logprobs_from_ids(&ids, v.len());
        let segs = ctc_greedy_decode(&lp, ids.len(), &v, &DecodeOptions::default());
        assert_eq!(segs.len(), 1);
        assert_eq!(segs[0].text, "сәлем әлем");
        assert_eq!(segs[0].start_ms, 2 * FRAME_MS);
        assert_eq!(segs[0].end_ms, 11 * FRAME_MS);
    }

    #[test]
    fn greedy_ctc_splits_on_long_blank_gap() {
        let v = vocab();
        let ids = [1, 2, 5, 5, 5, 5, 5, 5, 5, 5, 3, 4];
        let lp = logprobs_from_ids(&ids, v.len());
        let opts = DecodeOptions { split_on_blank_ms: 5 * FRAME_MS, ..Default::default() };
        let segs = ctc_greedy_decode(&lp, ids.len(), &v, &opts);
        assert_eq!(segs.len(), 2);
        assert_eq!(segs[0].text, "сәлем");
        assert_eq!(segs[1].text, "әлем");
    }

    #[test]
    fn char_ctc_with_space_piece_and_40ms_frames() {
        // GigaAM tokens.txt: "▁ 0", letters, blank last
        let v = Vocab::parse("▁ 0\nс 1\nә 2\nл 3\nе 4\nм 5\n<blk> 6\n").unwrap();
        assert_eq!(v.blank_id, 6);
        let ids = [6, 1, 2, 2, 6, 3, 4, 5, 0, 0, 6, 2, 3, 4, 5, 6];
        let lp = logprobs_from_ids(&ids, v.len());
        let segs = ctc_greedy_decode_frames(&lp, ids.len(), &v, &DecodeOptions::default(), 40);
        assert_eq!(segs[0].text, "сәлем әлем");
        assert_eq!((segs[0].start_ms, segs[0].end_ms), (40, 15 * 40));
    }

    #[test]
    fn pipeline_features_have_encoder_shape() {
        let p = CtcPipeline::new(vocab());
        let audio = vec![0.01f32; 16_000];
        let (f, t, valid) = p.features(&audio);
        assert_eq!(t, 101);
        assert_eq!(valid, 100);
        assert_eq!(f.len(), 80 * 101);
        assert!((0..80).all(|m| f[m * 101 + 100] == 0.0), "last frame must be masked");
    }

    /// Replays scripted (token, duration index) joint decisions; records what it was fed.
    struct Script {
        steps: Vec<(usize, usize)>,
        i: usize,
        seen: Vec<(usize, usize, u32)>, // (frame id, last token, state)
    }

    impl TdtStep for Script {
        type State = u32;
        fn initial_state(&self) -> u32 {
            0
        }
        fn step(&mut self, frame: &[f32], last: usize, st: &u32) -> Result<(Vec<f32>, u32), BackendError> {
            self.seen.push((frame[0] as usize, last, *st));
            let (k, d) = self.steps.get(self.i).copied().unwrap_or((5, 1));
            self.i += 1;
            let mut l = vec![-9.0; 6 + 3];
            l[k] = 0.0;
            l[6 + d] = 0.0;
            Ok((l, *st + 1))
        }
    }

    #[test]
    fn tdt_greedy_follows_durations_and_keeps_state_on_blank() {
        let v = vocab();
        // durations [0, 1, 2]: frame 0: "▁сә" stay, "лем" +2 → frame 2: blank +1 → frame 3: "▁ә" +1 → end (4 frames)
        let mut s = Script { steps: vec![(1, 0), (2, 2), (5, 1), (3, 1)], i: 0, seen: vec![] };
        let segs = tdt_greedy_decode(4, |t| vec![t as f32], &mut s, &v, &[0, 1, 2], 10, &DecodeOptions::default()).unwrap();
        assert_eq!(segs.len(), 1);
        assert_eq!(segs[0].text, "сәлем ә");
        assert_eq!(s.seen, vec![(0, 5, 0), (0, 1, 1), (2, 2, 2), (3, 2, 2)]);
        assert_eq!((segs[0].start_ms, segs[0].end_ms), (0, 4 * FRAME_MS));
    }

    #[test]
    fn tdt_max_symbols_advances_one_frame() {
        let v = vocab();
        // Always blank with duration 0: must not loop forever.
        let mut s = Script { steps: vec![(5, 0); 100], i: 0, seen: vec![] };
        let segs = tdt_greedy_decode(3, |t| vec![t as f32], &mut s, &v, &[0, 1, 2], 4, &DecodeOptions::default()).unwrap();
        assert!(segs.is_empty());
        assert_eq!(s.seen.len(), 12);
    }
}
