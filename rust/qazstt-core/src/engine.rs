//! The engine: the one object every platform binds to.
//!
//! ```text
//! Engine::new(config)        load the ONNX encoder + tokens, build VAD + post-processor
//! engine.feed(pcm, rate, ch) push audio in any rate/channel layout; returns
//!                            utterances that became final
//! engine.finish()            flush the tail; returns the last utterances
//! ```
//!
//! Internally: resample → segment on silence → log-mel → FastConformer CTC →
//! Kazakh clean-up → (hotwords) → (numbers) → capitalization → (Latin).

use std::path::PathBuf;

use crate::audio::{self, TARGET_SAMPLE_RATE};
use crate::backend::{Backend, BackendError, DecodeOptions};
use crate::kazakh::{self, HotwordCorrector};
use crate::vad::{Segment, Segmenter, SegmenterConfig};

/// Which script to return.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Script {
    Cyrillic,
    Latin,
}

/// Which voice activity detector cuts the stream into utterances.
#[derive(Debug, Clone, PartialEq)]
pub enum Vad {
    /// Silero if `silero_vad.onnx` sits next to the acoustic model, else energy.
    Auto,
    /// Dependency-free adaptive energy VAD.
    Energy,
    /// Silero VAD v5 from this file.
    Silero(PathBuf),
}

#[derive(Debug, Clone)]
pub struct EngineConfig {
    /// Directory containing `encoder.onnx` + `tokens.txt`, or the path to the .onnx itself.
    pub model_path: PathBuf,
    /// Use CUDA / CoreML when the build has that feature; CPU otherwise.
    pub use_gpu: bool,
    pub decode: DecodeOptions,
    pub segmenter: SegmenterConfig,
    pub vad: Vad,
    /// Silero speech-probability threshold (0..1).
    pub silero_threshold: f32,
    /// Punctuation + true-casing model directory (`model.onnx`, `sp.model`,
    /// `config.yaml`); `None` = off. Replaces `capitalize` when set.
    pub punct_model: Option<PathBuf>,
    pub script: Script,
    pub numbers_to_digits: bool,
    /// The model emits lowercase; capitalize the first letter of each utterance.
    pub capitalize: bool,
    /// Names / terms to snap near-misses onto (also restores their casing).
    pub hotwords: Vec<String>,
    /// 0.0..1.0, edit budget as a fraction of hotword length.
    pub hotword_max_ratio: f32,
}

impl EngineConfig {
    pub fn new(model_path: impl Into<PathBuf>) -> Self {
        Self {
            model_path: model_path.into(),
            use_gpu: true,
            decode: DecodeOptions::default(),
            segmenter: SegmenterConfig::default(),
            vad: Vad::Auto,
            silero_threshold: 0.5,
            punct_model: None,
            script: Script::Cyrillic,
            numbers_to_digits: false,
            capitalize: true,
            hotwords: Vec::new(),
            hotword_max_ratio: 0.25,
        }
    }
}

/// A final transcript piece with absolute stream timestamps.
#[derive(Debug, Clone, PartialEq)]
pub struct Transcript {
    pub start_ms: u64,
    pub end_ms: u64,
    pub text: String,
}

#[derive(Debug)]
pub enum EngineError {
    Backend(BackendError),
    Audio(audio::AudioError),
    Config(String),
}

impl std::fmt::Display for EngineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EngineError::Backend(e) => write!(f, "{e}"),
            EngineError::Audio(e) => write!(f, "{e}"),
            EngineError::Config(s) => write!(f, "config error: {s}"),
        }
    }
}
impl std::error::Error for EngineError {}
impl From<BackendError> for EngineError {
    fn from(e: BackendError) -> Self {
        EngineError::Backend(e)
    }
}
impl From<audio::AudioError> for EngineError {
    fn from(e: audio::AudioError) -> Self {
        EngineError::Audio(e)
    }
}

pub struct Engine {
    backend: Box<dyn Backend>,
    decode: DecodeOptions,
    segmenter: Segmenter,
    hotwords: HotwordCorrector,
    script: Script,
    numbers_to_digits: bool,
    capitalize: bool,
    max_segment_ms: u32,
    vad_name: &'static str,
    /// Keeps filter history between `feed` calls when the input is not 16 kHz.
    resampler: Option<audio::Resampler>,
    #[cfg(feature = "ort-backend")]
    punct: Option<crate::punct::Punctuator>,
}

impl Engine {
    /// Load the model and build the pipeline. Requires the `onnx` feature.
    #[cfg(feature = "ort-backend")]
    pub fn new(cfg: EngineConfig) -> Result<Self, EngineError> {
        if !cfg.model_path.exists() {
            return Err(EngineError::Config(format!("model not found: {}", cfg.model_path.display())));
        }
        use crate::backend::fastconformer::{FastConformerCtc, FastConformerTdt, GigaAmCtc};
        // A model directory with decoder_joint.onnx is the transducer (TDT) export;
        // model.json naming GigaAM selects the GigaAM char-CTC backend (own features).
        let backend: Box<dyn Backend> = if GigaAmCtc::is_gigaam_model(&cfg.model_path) {
            Box::new(GigaAmCtc::load(&cfg.model_path, cfg.decode.threads, cfg.use_gpu)?)
        } else if FastConformerTdt::is_tdt_model(&cfg.model_path) {
            Box::new(FastConformerTdt::load(&cfg.model_path, cfg.decode.threads, cfg.use_gpu)?)
        } else {
            Box::new(FastConformerCtc::load(&cfg.model_path, cfg.decode.threads, cfg.use_gpu)?)
        };
        let silero = match &cfg.vad {
            Vad::Energy => None,
            Vad::Silero(p) => Some(p.clone()),
            Vad::Auto => {
                let dir = if cfg.model_path.is_dir() { cfg.model_path.clone() } else { cfg.model_path.parent().map(PathBuf::from).unwrap_or_default() };
                Some(dir.join("silero_vad.onnx")).filter(|p| p.exists())
            }
        };
        let mut engine = Self::with_backend(backend, cfg.clone());
        if let Some(p) = silero {
            let vad = crate::vad::silero::SileroVad::load(&p, cfg.silero_threshold)?;
            engine.segmenter = Segmenter::with_detector(cfg.segmenter, Box::new(vad));
            engine.vad_name = "silero";
        }
        if let Some(dir) = &cfg.punct_model {
            engine.punct = Some(crate::punct::Punctuator::load(dir, cfg.decode.threads.min(4))?);
        }
        Ok(engine)
    }

    /// Build the pipeline around any backend (mock, custom, etc.).
    pub fn with_backend(backend: Box<dyn Backend>, cfg: EngineConfig) -> Self {
        Self {
            backend,
            decode: cfg.decode,
            max_segment_ms: cfg.segmenter.max_segment_ms,
            segmenter: Segmenter::new(cfg.segmenter),
            hotwords: HotwordCorrector::new(cfg.hotwords.iter(), cfg.hotword_max_ratio, 4),
            script: cfg.script,
            numbers_to_digits: cfg.numbers_to_digits,
            capitalize: cfg.capitalize,
            vad_name: "energy",
            resampler: None,
            #[cfg(feature = "ort-backend")]
            punct: None,
        }
    }

    pub fn backend_name(&self) -> &'static str {
        self.backend.name()
    }

    /// Where the acoustic model runs: `"cpu"`, or the accelerator (`"cuda"`, …).
    pub fn device(&self) -> &'static str {
        self.backend.device()
    }

    /// `"silero"` or `"energy"`.
    pub fn vad_name(&self) -> &'static str {
        self.vad_name
    }

    /// Feed interleaved f32 PCM at any sample rate / channel count.
    /// Returns utterances that became final during this call.
    pub fn feed(&mut self, samples: &[f32], sample_rate: u32, channels: u16) -> Result<Vec<Transcript>, EngineError> {
        if samples.is_empty() {
            return Ok(Vec::new());
        }
        if channels == 0 || sample_rate == 0 {
            return Err(EngineError::Config("sample_rate and channels must be > 0".into()));
        }
        let mono = audio::mix_to_mono(samples, channels);
        if sample_rate == TARGET_SAMPLE_RATE {
            self.resampler = None;
            self.segmenter.push(&mono);
        } else {
            // One resampler for the whole stream: resampling each call on its own
            // would distort every chunk boundary of a live stream.
            if self.resampler.as_ref().map(|r| r.rates().0) != Some(sample_rate) {
                self.resampler = Some(audio::Resampler::new(sample_rate, TARGET_SAMPLE_RATE));
            }
            let mono16k = self.resampler.as_mut().expect("set above").process(&mono);
            self.segmenter.push(&mono16k);
        }
        let ready = self.segmenter.take_ready();
        self.run_segments(ready, false)
    }

    /// Feed i16 PCM (what most microphone APIs hand you).
    pub fn feed_i16(&mut self, samples: &[i16], sample_rate: u32, channels: u16) -> Result<Vec<Transcript>, EngineError> {
        let f = audio::i16_to_f32(samples);
        self.feed(&f, sample_rate, channels)
    }

    /// End of stream. Flushes the open utterance.
    pub fn finish(&mut self) -> Result<Vec<Transcript>, EngineError> {
        if let Some(mut r) = self.resampler.take() {
            let rest = r.flush();
            self.segmenter.push(&rest);
        }
        let mut segs = self.segmenter.take_ready();
        segs.extend(self.segmenter.flush());
        self.run_segments(segs, false)
    }

    /// Interim text of the utterance being spoken, before the pause that finalizes
    /// it. Re-decodes the open utterance from its start (the encoder is not
    /// streaming), so call it on a timer (~0.5–1 s) while feeding a live stream,
    /// not per chunk. `None` while silent or for less than 0.3 s of speech.
    /// The final text from `feed`/`finish` may differ: it sees the whole utterance.
    pub fn partial(&mut self) -> Result<Option<Transcript>, EngineError> {
        let Some((start, audio)) = self.segmenter.open_utterance() else { return Ok(None) };
        if audio.len() < TARGET_SAMPLE_RATE as usize * 3 / 10 {
            return Ok(None);
        }
        let seg = Segment { start_sample: start, samples: audio.to_vec() };
        // Interim text skips the punctuation model: it is replaced within a second
        // or two, and that model would run again on every refresh.
        let items = self.run_segments(vec![seg], true)?;
        Ok(match (items.first(), items.last()) {
            (Some(a), Some(b)) => Some(Transcript {
                start_ms: a.start_ms,
                end_ms: b.end_ms,
                text: items.iter().map(|t| t.text.as_str()).collect::<Vec<_>>().join(" "),
            }),
            _ => None,
        })
    }

    /// Length of the utterance being spoken right now (speech started, no closing
    /// pause yet), in milliseconds; `None` while silent. Lets a caller space out
    /// [`Engine::partial`] calls as the utterance grows.
    pub fn open_utterance_ms(&self) -> Option<u64> {
        self.segmenter.open_utterance().map(|(_, a)| a.len() as u64 * 1000 / TARGET_SAMPLE_RATE as u64)
    }

    /// Replace the hotword list (names / terms to snap near-misses onto).
    pub fn set_hotwords(&mut self, hotwords: &[String], max_ratio: f32) {
        self.hotwords = HotwordCorrector::new(hotwords.iter(), max_ratio, 4);
    }

    /// Reset stream state (keeps the loaded model). Call between recordings.
    pub fn reset(&mut self) {
        self.segmenter.reset();
        self.resampler = None;
    }

    /// Convenience: transcribe a whole WAV file.
    ///
    /// Uses the VAD segmenter; if it finds no speech at all in a file longer than
    /// 0.5 s (e.g. an extremely quiet recording), falls back to decoding the whole
    /// file in `max_segment_ms` chunks — CTC handles silence itself.
    pub fn transcribe_wav(&mut self, path: &std::path::Path) -> Result<Vec<Transcript>, EngineError> {
        let pcm = audio::read_wav(path)?;
        self.reset();
        let mut out = self.feed(&pcm.samples, pcm.sample_rate, pcm.channels)?;
        out.extend(self.finish()?);
        if out.is_empty() && pcm.duration_secs() > 0.5 {
            out = self.transcribe_unsegmented(&pcm.to_engine_format())?;
        }
        Ok(out)
    }

    /// Decode 16 kHz mono audio without VAD, in chunks of `max_segment_ms`.
    pub fn transcribe_unsegmented(&mut self, audio16k: &[f32]) -> Result<Vec<Transcript>, EngineError> {
        let chunk = (self.max_segment_ms as usize * TARGET_SAMPLE_RATE as usize / 1000).max(TARGET_SAMPLE_RATE as usize);
        let segs = audio16k
            .chunks(chunk)
            .enumerate()
            .map(|(i, c)| Segment { start_sample: (i * chunk) as u64, samples: c.to_vec() })
            .collect();
        self.run_segments(segs, false)
    }

    /// Transcribe 16 kHz mono audio that is already one utterance (no VAD).
    pub fn transcribe_utterance(&mut self, audio16k: &[f32]) -> Result<Vec<Transcript>, EngineError> {
        self.run_segments(vec![Segment { start_sample: 0, samples: audio16k.to_vec() }], false)
    }

    /// `interim`: the text is a preview that will be replaced (see [`Engine::partial`]).
    fn run_segments(&mut self, segs: Vec<Segment>, interim: bool) -> Result<Vec<Transcript>, EngineError> {
        let mut out = Vec::new();
        for seg in segs {
            let raws = self.backend.transcribe(&seg.samples, &self.decode)?;
            let base_ms = seg.start_sample * 1000 / TARGET_SAMPLE_RATE as u64;
            for r in raws {
                let text = self.postprocess(&r.text, interim);
                if text.is_empty() {
                    continue;
                }
                out.push(Transcript { start_ms: base_ms + r.start_ms as u64, end_ms: base_ms + r.end_ms as u64, text });
            }
        }
        Ok(out)
    }

    /// Whether punctuation restoration is active.
    pub fn has_punctuation(&self) -> bool {
        #[cfg(feature = "ort-backend")]
        return self.punct.is_some();
        #[cfg(not(feature = "ort-backend"))]
        false
    }

    fn postprocess(&mut self, raw: &str, interim: bool) -> String {
        let mut t = kazakh::clean(raw);
        if t.is_empty() {
            return t;
        }
        if !self.hotwords.is_empty() {
            t = self.hotwords.apply(&t);
        }
        if self.numbers_to_digits {
            t = kazakh::numbers_to_digits(&t);
        }
        #[cfg(feature = "ort-backend")]
        if let Some(p) = self.punct.as_mut().filter(|_| !interim) {
            // On failure keep the unpunctuated text rather than losing the utterance.
            match p.apply(&t) {
                Ok(pt) if !pt.is_empty() => t = pt,
                _ => t = kazakh::capitalize_sentences(&t),
            }
        } else if self.capitalize {
            t = kazakh::capitalize_sentences(&t);
        }
        #[cfg(not(feature = "ort-backend"))]
        let _ = interim;
        #[cfg(not(feature = "ort-backend"))]
        if self.capitalize {
            t = kazakh::capitalize_sentences(&t);
        }
        if self.script == Script::Latin {
            t = kazakh::to_latin(&t);
        }
        t
    }

    /// Post-process an externally produced raw string with this engine's settings.
    pub fn postprocess_text(&mut self, raw: &str) -> String {
        self.postprocess(raw, false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::MockBackend;

    fn tone(secs: f32) -> Vec<f32> {
        let n = (16_000.0 * secs) as usize;
        (0..n).map(|i| (2.0 * std::f32::consts::PI * 180.0 * i as f32 / 16_000.0).sin() * 0.3).collect()
    }

    fn engine(text: &str, cfg: EngineConfig) -> Engine {
        Engine::with_backend(Box::new(MockBackend { text: text.into() }), cfg)
    }

    #[test]
    fn end_to_end_with_mock() {
        let mut cfg = EngineConfig::new("unused");
        cfg.hotwords = vec!["Алматы".into()];
        cfg.numbers_to_digits = true;
        // FastConformer output style: lowercase, no punctuation.
        let mut e = engine("алмата қаласында жиырма бес адам бар", cfg);

        let mut audio = vec![0.0; 8_000];
        audio.extend(tone(1.0));
        audio.extend(vec![0.0; 16_000]);
        let mut out = e.feed(&audio, 16_000, 1).unwrap();
        out.extend(e.finish().unwrap());
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].text, "Алматы қаласында 25 адам бар");
        assert!(out[0].start_ms >= 250 && out[0].start_ms <= 500, "{}", out[0].start_ms);
    }

    #[test]
    fn latin_output_and_resampling() {
        let mut cfg = EngineConfig::new("unused");
        cfg.script = Script::Latin;
        let mut e = engine("қазақстан", cfg);
        let n = 44_100;
        let mut audio = Vec::with_capacity(n * 2 + 44_100);
        audio.extend(vec![0.0; 22_050]);
        for i in 0..n {
            let v = (2.0 * std::f32::consts::PI * 180.0 * i as f32 / 44_100.0).sin() * 0.3;
            audio.push(v);
            audio.push(v);
        }
        audio.extend(vec![0.0; 44_100]);
        let mut out = e.feed(&audio, 44_100, 2).unwrap();
        out.extend(e.finish().unwrap());
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].text, "Qazaqstan");
    }

    #[test]
    fn quiet_file_falls_back_to_unsegmented() {
        // Below even the VAD floor: −80 dBFS tone. Whole-file fallback must kick in.
        let dir = std::env::temp_dir().join("qazstt_quiet_test.wav");
        let x: Vec<f32> = (0..32_000).map(|i| (i as f32 * 0.07).sin() * 0.0001).collect();
        std::fs::write(&dir, crate::audio::encode_wav_mono16(&x, 16_000)).unwrap();
        let mut e = engine("тыныш", EngineConfig::new("unused"));
        let out = e.transcribe_wav(&dir).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].text, "Тыныш");
        assert_eq!(out[0].start_ms, 0);
    }

    #[test]
    fn partial_shows_open_utterance_then_clears() {
        let mut e = engine("сәлем", EngineConfig::new("unused"));
        let mut audio = vec![0.0; 8_000];
        audio.extend(tone(1.0));
        assert!(e.feed(&audio, 16_000, 1).unwrap().is_empty());
        let p = e.partial().unwrap().expect("interim text while speaking");
        assert_eq!(p.text, "Сәлем");
        // A pause finalizes it; nothing is open any more.
        let done = e.feed(&vec![0.0; 16_000], 16_000, 1).unwrap();
        assert_eq!(done.len(), 1);
        assert!(e.partial().unwrap().is_none());
    }

    #[test]
    fn capitalization_can_be_disabled() {
        let mut cfg = EngineConfig::new("unused");
        cfg.capitalize = false;
        let mut e = engine("", cfg);
        assert_eq!(e.postprocess_text("сәлем"), "сәлем");
    }
}
