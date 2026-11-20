//! # qazstt-core
//!
//! Kazakh offline speech-to-text. One crate, one `Engine`, three calls.
//! Acoustic model: NVIDIA FastConformer-Hybrid Large (kk-ru), CTC head, ONNX.
//!
//! ```ignore
//! // (requires the `onnx` feature, on by default)
//! use qazstt_core::{Engine, EngineConfig};
//!
//! let mut engine = Engine::new(EngineConfig::new("models/fastconformer-kk-ru"))?;
//! // microphone callback:
//! let pcm_i16: Vec<i16> = vec![];           // interleaved samples from the mic
//! let finals = engine.feed_i16(&pcm_i16, 48_000, 2)?;
//! for t in finals { println!("[{}..{} ms] {}", t.start_ms, t.end_ms, t.text); }
//! // when the user stops talking / recording ends:
//! let tail = engine.finish()?;
//! # Ok::<(), qazstt_core::EngineError>(())
//! ```
//!
//! Layering (top is public API):
//!
//! | module     | role                                                            | deps |
//! |------------|-----------------------------------------------------------------|------|
//! | `engine`   | `Engine` / `EngineConfig` / `Transcript`                        | —    |
//! | `backend`  | `Backend` trait, tokens, CTC greedy decode; `fastconformer` (ort) | ort (optional) |
//! | `features` | NeMo-exact log-mel front end (own FFT)                          | —    |
//! | `vad`      | energy / Silero VAD + utterance segmenter                       | ort (Silero only) |
//! | `punct`    | punctuation + true-casing (XLM-R, own SentencePiece)            | ort  |
//! | `kazakh`   | clean-up, capitalization, Latin transliteration, hotwords, numbers | — |
//! | `audio`    | WAV decode/encode, mixdown, resampling                          | —    |
//! | `tts`      | Kazakh text-to-speech: text front end, G2P, Piper/VITS voice    | ort (voice only) |

pub mod audio;
pub mod backend;
pub mod engine;
pub mod features;
pub mod features_gigaam;
pub mod kazakh;
#[cfg(feature = "ort-backend")]
pub mod punct;
pub mod tts;
pub mod vad;

pub use audio::{decode_wav, read_wav, PcmAudio, TARGET_SAMPLE_RATE};
pub use backend::{ctc_greedy_decode, Backend, BackendError, DecodeOptions, MockBackend, ModelFiles, RawSegment, Vocab};
pub use engine::{Engine, EngineConfig, EngineError, Script, Transcript, Vad};
pub use features::MelExtractor;
pub use vad::SegmenterConfig;

#[cfg(feature = "ort-backend")]
pub use backend::fastconformer::{set_thread_spinning, ACCELERATOR};

/// Library version, for FFI consumers to log.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
