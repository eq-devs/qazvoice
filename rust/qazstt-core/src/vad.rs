//! Voice activity detection + utterance segmentation.
//!
//! An adaptive energy / spectral-flatness VAD. It is deliberately simple and
//! dependency-free so it runs identically on every target. It tracks a noise
//! floor, decides speech per 20 ms frame, and emits utterances bounded by
//! silence, with a hard length cap so memory and latency stay bounded.
//!
//! With an ONNX backend, [`silero::SileroVad`] (a neural VAD) can replace the
//! energy detector via [`Segmenter::with_detector`]; the segmentation logic stays the same.

use crate::audio::TARGET_SAMPLE_RATE;

/// 20 ms frames at 16 kHz.
pub const FRAME_LEN: usize = (TARGET_SAMPLE_RATE / 50) as usize; // 320

/// Per-frame speech decision provider.
pub trait SpeechDetector: Send {
    /// Return true if this 20 ms frame contains speech.
    fn is_speech(&mut self, frame: &[f32]) -> bool;
    fn reset(&mut self);
}

/// Adaptive energy VAD.
///
/// Noise floor is tracked with an asymmetric exponential average (fast to drop,
/// slow to rise) so a sudden loud utterance does not get absorbed into "noise".
/// A frame is speech if its RMS exceeds `floor * snr_ratio` AND its zero-crossing
/// rate is in a plausible voiced/unvoiced band (rejects pure hiss / clicks).
pub struct EnergyVad {
    noise_floor: f32,
    snr_ratio: f32,
    min_rms: f32,
}

impl Default for EnergyVad {
    fn default() -> Self {
        let c = SegmenterConfig::default();
        Self::new(c.vad_snr, c.vad_min_rms)
    }
}

impl EnergyVad {
    /// `snr_ratio`: how far above the noise floor counts as speech (linear, ~3 = +9.5 dB).
    /// `min_rms`: absolute floor so digital silence never flips to speech.
    pub fn new(snr_ratio: f32, min_rms: f32) -> Self {
        Self { noise_floor: min_rms, snr_ratio, min_rms }
    }

    fn rms(frame: &[f32]) -> f32 {
        if frame.is_empty() {
            return 0.0;
        }
        (frame.iter().map(|s| s * s).sum::<f32>() / frame.len() as f32).sqrt()
    }

    fn zero_crossing_rate(frame: &[f32]) -> f32 {
        if frame.len() < 2 {
            return 0.0;
        }
        let crossings = frame
            .windows(2)
            .filter(|w| (w[0] >= 0.0) != (w[1] >= 0.0))
            .count();
        crossings as f32 / (frame.len() - 1) as f32
    }
}

impl SpeechDetector for EnergyVad {
    fn is_speech(&mut self, frame: &[f32]) -> bool {
        let rms = Self::rms(frame);
        let zcr = Self::zero_crossing_rate(frame);

        // The floor starts at `min_rms` and adapts only on non-speech frames. (Seeding
        // it from the first frame would treat a recording that starts mid-speech as
        // "noise" and drop the whole utterance.)
        let threshold = (self.noise_floor * self.snr_ratio).max(self.min_rms);
        // Speech ZCR at 16 kHz is roughly 0.02..0.35; broadband noise sits higher.
        let speech = rms > threshold && (0.01..=0.45).contains(&zcr);

        // Update the floor only from non-speech frames (fast decay, slow attack).
        if !speech {
            let alpha = if rms < self.noise_floor { 0.3 } else { 0.02 };
            self.noise_floor = (1.0 - alpha) * self.noise_floor + alpha * rms;
            self.noise_floor = self.noise_floor.max(self.min_rms);
        }
        speech
    }

    fn reset(&mut self) {
        self.noise_floor = self.min_rms;
    }
}

/// Segmentation tuning. All durations in milliseconds.
#[derive(Debug, Clone)]
pub struct SegmenterConfig {
    /// Silence needed to close an utterance.
    pub min_silence_ms: u32,
    /// Speech shorter than this is dropped as noise.
    pub min_speech_ms: u32,
    /// Hard cap; longer utterances are split at the quietest recent frame.
    pub max_segment_ms: u32,
    /// Audio kept before the first speech frame.
    pub pre_roll_ms: u32,
    /// Audio kept after the last speech frame.
    pub post_roll_ms: u32,
    /// Energy VAD: speech when frame RMS > noise_floor × this (linear; 3 ≈ +9.5 dB).
    pub vad_snr: f32,
    /// Energy VAD: absolute RMS floor (0.0003 ≈ −70 dBFS). Keep low — read-speech
    /// corpora like FLEURS have utterances peaking around −45 dBFS.
    pub vad_min_rms: f32,
}

impl Default for SegmenterConfig {
    fn default() -> Self {
        Self {
            min_silence_ms: 600,
            min_speech_ms: 250,
            max_segment_ms: 25_000,
            pre_roll_ms: 200,
            post_roll_ms: 200,
            vad_snr: 3.0,
            vad_min_rms: 0.0003,
        }
    }
}

/// A speech segment in engine-rate samples, with absolute offset in the stream.
#[derive(Debug, Clone, PartialEq)]
pub struct Segment {
    /// Offset of `samples[0]` from stream start, in samples at 16 kHz.
    pub start_sample: u64,
    pub samples: Vec<f32>,
}

impl Segment {
    pub fn start_secs(&self) -> f32 {
        self.start_sample as f32 / TARGET_SAMPLE_RATE as f32
    }
    pub fn end_secs(&self) -> f32 {
        (self.start_sample + self.samples.len() as u64) as f32 / TARGET_SAMPLE_RATE as f32
    }
}

/// Streaming utterance segmenter. Push 16 kHz mono samples in any chunk size;
/// collect complete utterances with [`Segmenter::take_ready`]; call
/// [`Segmenter::flush`] at end of stream.
pub struct Segmenter {
    cfg: SegmenterConfig,
    detector: Box<dyn SpeechDetector>,
    frame_buf: Vec<f32>,
    pre_roll: std::collections::VecDeque<f32>,
    current: Vec<f32>,
    current_start: u64,
    in_speech: bool,
    silence_frames: u32,
    speech_frames: u32,
    stream_pos: u64,
    ready: Vec<Segment>,
    // energy per frame of `current`, for choosing a split point on forced cuts
    frame_energy: Vec<f32>,
}

impl Segmenter {
    pub fn new(cfg: SegmenterConfig) -> Self {
        let vad = EnergyVad::new(cfg.vad_snr, cfg.vad_min_rms);
        Self::with_detector(cfg, Box::new(vad))
    }

    pub fn with_detector(cfg: SegmenterConfig, detector: Box<dyn SpeechDetector>) -> Self {
        Self {
            cfg,
            detector,
            frame_buf: Vec::with_capacity(FRAME_LEN),
            pre_roll: std::collections::VecDeque::new(),
            current: Vec::new(),
            current_start: 0,
            in_speech: false,
            silence_frames: 0,
            speech_frames: 0,
            stream_pos: 0,
            ready: Vec::new(),
            frame_energy: Vec::new(),
        }
    }

    fn ms_to_frames(ms: u32) -> u32 {
        ms / 20
    }

    fn pre_roll_samples(&self) -> usize {
        (self.cfg.pre_roll_ms as usize * TARGET_SAMPLE_RATE as usize) / 1000
    }

    /// Push samples (16 kHz mono f32).
    pub fn push(&mut self, samples: &[f32]) {
        for &s in samples {
            self.frame_buf.push(s);
            if self.frame_buf.len() == FRAME_LEN {
                let frame = std::mem::replace(&mut self.frame_buf, Vec::with_capacity(FRAME_LEN));
                self.process_frame(frame);
            }
        }
    }

    fn process_frame(&mut self, frame: Vec<f32>) {
        let speech = self.detector.is_speech(&frame);
        let frame_start = self.stream_pos;
        self.stream_pos += FRAME_LEN as u64;

        if self.in_speech {
            let e = frame.iter().map(|x| x * x).sum::<f32>();
            self.frame_energy.push(e);
            self.current.extend_from_slice(&frame);
            if speech {
                self.silence_frames = 0;
                self.speech_frames += 1;
            } else {
                self.silence_frames += 1;
            }

            let max_frames = Self::ms_to_frames(self.cfg.max_segment_ms);
            if self.silence_frames >= Self::ms_to_frames(self.cfg.min_silence_ms) {
                self.close_segment(false);
            } else if (self.current.len() / FRAME_LEN) as u32 >= max_frames {
                self.close_segment(true);
            }
        } else {
            if speech {
                self.in_speech = true;
                self.silence_frames = 0;
                self.speech_frames = 1;
                // Seed with pre-roll.
                let pre: Vec<f32> = self.pre_roll.iter().copied().collect();
                self.current_start = frame_start - pre.len() as u64;
                self.current = pre;
                self.frame_energy.clear();
                // pre-roll frames count as "quiet" for split purposes
                for _ in 0..(self.current.len() / FRAME_LEN) {
                    self.frame_energy.push(0.0);
                }
                self.current.extend_from_slice(&frame);
                self.frame_energy.push(frame.iter().map(|x| x * x).sum());
                self.pre_roll.clear();
            } else {
                let cap = self.pre_roll_samples();
                for &s in &frame {
                    if self.pre_roll.len() >= cap {
                        self.pre_roll.pop_front();
                    }
                    self.pre_roll.push_back(s);
                }
            }
        }
    }

    fn close_segment(&mut self, forced: bool) {
        let min_speech = Self::ms_to_frames(self.cfg.min_speech_ms);
        let silence_tail = self.silence_frames as usize * FRAME_LEN;
        let post_roll = (self.cfg.post_roll_ms as usize * TARGET_SAMPLE_RATE as usize) / 1000;

        if forced {
            // Split at the lowest-energy frame in the last 40% of the segment,
            // keep the remainder as the start of the next segment.
            let n = self.frame_energy.len();
            let search_from = (n as f32 * 0.6) as usize;
            let (cut_frame, _) = self.frame_energy[search_from..]
                .iter()
                .enumerate()
                .fold((search_from, f32::MAX), |(bi, be), (i, &e)| {
                    if e < be {
                        (search_from + i, e)
                    } else {
                        (bi, be)
                    }
                });
            let cut = cut_frame * FRAME_LEN;
            let rest = self.current.split_off(cut);
            let rest_energy = self.frame_energy.split_off(cut_frame);
            let seg = Segment {
                start_sample: self.current_start,
                samples: std::mem::take(&mut self.current),
            };
            if self.speech_frames >= min_speech {
                self.ready.push(seg);
            }
            self.current_start += cut as u64;
            self.current = rest;
            self.frame_energy = rest_energy;
            self.speech_frames = 0;
            self.silence_frames = 0;
            // stay in_speech
            return;
        }

        // Normal close: trim trailing silence beyond post_roll.
        let keep = self.current.len().saturating_sub(silence_tail.saturating_sub(post_roll));
        self.current.truncate(keep);
        let seg = Segment {
            start_sample: self.current_start,
            samples: std::mem::take(&mut self.current),
        };
        if self.speech_frames >= min_speech && !seg.samples.is_empty() {
            self.ready.push(seg);
        }
        self.frame_energy.clear();
        self.in_speech = false;
        self.silence_frames = 0;
        self.speech_frames = 0;
    }

    /// Drain completed utterances.
    pub fn take_ready(&mut self) -> Vec<Segment> {
        std::mem::take(&mut self.ready)
    }

    /// End of stream: close any open utterance.
    pub fn flush(&mut self) -> Vec<Segment> {
        if !self.frame_buf.is_empty() {
            let mut f = std::mem::take(&mut self.frame_buf);
            f.resize(FRAME_LEN, 0.0);
            self.process_frame(f);
        }
        if self.in_speech {
            self.close_segment(false);
        }
        self.take_ready()
    }

    pub fn reset(&mut self) {
        self.detector.reset();
        self.frame_buf.clear();
        self.pre_roll.clear();
        self.current.clear();
        self.frame_energy.clear();
        self.in_speech = false;
        self.silence_frames = 0;
        self.speech_frames = 0;
        self.stream_pos = 0;
        self.ready.clear();
    }

    /// The utterance still open (speech started, not yet closed by silence):
    /// absolute start sample and audio so far. For interim (partial) results.
    pub fn open_utterance(&self) -> Option<(u64, &[f32])> {
        self.in_speech.then_some((self.current_start, self.current.as_slice()))
    }

    /// Samples consumed so far.
    pub fn position(&self) -> u64 {
        self.stream_pos
    }
}

/// Silero VAD v5 (`silero_vad.onnx`, MIT, github.com/snakers4/silero-vad) as a
/// [`SpeechDetector`]. Far more robust to noise and music than [`EnergyVad`].
///
/// The model scores 512-sample (32 ms) windows with 64 samples of context and
/// carries a recurrent state; our 20 ms frames are buffered into those windows and
/// each frame gets the latest window's decision, with hysteresis
/// (on at `threshold`, off below `threshold - 0.15`), as Silero's own iterator does.
#[cfg(feature = "ort-backend")]
pub mod silero {
    use super::SpeechDetector;
    use ort::session::Session;
    use ort::value::Tensor;
    use std::path::Path;

    const WINDOW: usize = 512;
    const CONTEXT: usize = 64;
    const STATE_LEN: usize = 2 * 128;

    pub struct SileroVad {
        session: Session,
        state: Vec<f32>,
        context: Vec<f32>,
        buf: Vec<f32>,
        threshold: f32,
        speaking: bool,
    }

    impl SileroVad {
        pub fn load(path: &Path, threshold: f32) -> Result<Self, crate::BackendError> {
            crate::backend::fastconformer::ensure_runtime()?;
            let session = crate::backend::fastconformer::cpu_session(path, 1)?;
            Ok(Self {
                session,
                state: vec![0.0; STATE_LEN],
                context: vec![0.0; CONTEXT],
                buf: Vec::with_capacity(WINDOW * 2),
                threshold,
                speaking: false,
            })
        }

        /// Speech probability of one 512-sample window.
        fn score(&mut self, window: &[f32]) -> Result<f32, ort::Error> {
            let mut x = Vec::with_capacity(CONTEXT + WINDOW);
            x.extend_from_slice(&self.context);
            x.extend_from_slice(window);
            self.context.copy_from_slice(&window[WINDOW - CONTEXT..]);
            let input = Tensor::from_array(([1usize, CONTEXT + WINDOW], x))?;
            let state = Tensor::from_array(([2usize, 1, 128], self.state.clone()))?;
            let sr = Tensor::from_array(((), vec![16_000i64]))?;
            let out = self.session.run(ort::inputs!["input" => input, "state" => state, "sr" => sr])?;
            let (_, p) = out[0].try_extract_tensor::<f32>()?;
            let (_, s) = out[1].try_extract_tensor::<f32>()?;
            self.state.copy_from_slice(&s[..STATE_LEN]);
            Ok(p[0])
        }
    }

    impl SpeechDetector for SileroVad {
        fn is_speech(&mut self, frame: &[f32]) -> bool {
            self.buf.extend_from_slice(frame);
            while self.buf.len() >= WINDOW {
                let w: Vec<f32> = self.buf.drain(..WINDOW).collect();
                // An inference error keeps the previous decision rather than dropping audio.
                if let Ok(p) = self.score(&w) {
                    if p >= self.threshold {
                        self.speaking = true;
                    } else if p < self.threshold - 0.15 {
                        self.speaking = false;
                    }
                }
            }
            self.speaking
        }

        fn reset(&mut self) {
            self.state.iter_mut().for_each(|v| *v = 0.0);
            self.context.iter_mut().for_each(|v| *v = 0.0);
            self.buf.clear();
            self.speaking = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(secs: f32, amp: f32) -> Vec<f32> {
        let n = (16_000.0 * secs) as usize;
        (0..n)
            .map(|i| (2.0 * std::f32::consts::PI * 200.0 * i as f32 / 16_000.0).sin() * amp)
            .collect()
    }
    fn silence(secs: f32) -> Vec<f32> {
        vec![0.0; (16_000.0 * secs) as usize]
    }

    #[test]
    fn splits_on_silence() {
        let mut seg = Segmenter::new(SegmenterConfig::default());
        let mut audio = silence(0.5);
        audio.extend(tone(1.0, 0.3));
        audio.extend(silence(1.0));
        audio.extend(tone(1.5, 0.3));
        audio.extend(silence(0.5));
        seg.push(&audio);
        let mut segs = seg.take_ready();
        segs.extend(seg.flush());
        assert_eq!(segs.len(), 2, "got {:?}", segs.iter().map(|s| (s.start_secs(), s.end_secs())).collect::<Vec<_>>());
        assert!((segs[0].start_secs() - 0.3).abs() < 0.1);
        assert!((segs[0].end_secs() - 1.7).abs() < 0.15);
        assert!((segs[1].start_secs() - 2.3).abs() < 0.1);
    }

    #[test]
    fn keeps_speech_that_starts_at_sample_zero() {
        // No leading silence: the very first frame is speech.
        let mut seg = Segmenter::new(SegmenterConfig::default());
        let mut audio = tone(1.5, 0.3);
        audio.extend(silence(1.0));
        seg.push(&audio);
        let segs = seg.flush();
        assert_eq!(segs.len(), 1);
        assert_eq!(segs[0].start_sample, 0);
        assert!(segs[0].end_secs() > 1.4);
    }

    #[test]
    fn quiet_speech_is_detected() {
        // −32 dBFS speech-level tone over −60 dBFS background.
        let mut seg = Segmenter::new(SegmenterConfig::default());
        let mut audio: Vec<f32> = silence(0.5).iter().enumerate().map(|(i, _)| ((i * 7919) % 97) as f32 / 97.0 * 0.002 - 0.001).collect();
        audio.extend(tone(1.0, 0.035));
        audio.extend(silence(1.0));
        seg.push(&audio);
        assert_eq!(seg.flush().len(), 1);
    }

    #[test]
    fn drops_short_blips() {
        let mut seg = Segmenter::new(SegmenterConfig::default());
        let mut audio = silence(0.5);
        audio.extend(tone(0.1, 0.3)); // 100 ms < min_speech 250 ms
        audio.extend(silence(1.0));
        seg.push(&audio);
        let segs = seg.flush();
        assert!(segs.is_empty());
    }

    #[test]
    fn hard_caps_long_speech() {
        let cfg = SegmenterConfig {
            max_segment_ms: 2_000,
            ..Default::default()
        };
        let mut seg = Segmenter::new(cfg);
        let mut audio = silence(0.2);
        audio.extend(tone(7.0, 0.3));
        audio.extend(silence(1.0));
        seg.push(&audio);
        let segs = seg.flush();
        assert!(segs.len() >= 3, "len={}", segs.len());
        for s in &segs {
            assert!(s.samples.len() <= 2_000 * 16 + FRAME_LEN);
        }
        // Contiguous coverage: each segment starts where the previous ended.
        for w in segs.windows(2) {
            assert_eq!(w[0].start_sample + w[0].samples.len() as u64, w[1].start_sample);
        }
    }

    #[test]
    fn streaming_chunking_matches_batch() {
        let mut audio = silence(0.3);
        audio.extend(tone(0.8, 0.3));
        audio.extend(silence(0.8));
        audio.extend(tone(0.6, 0.3));
        audio.extend(silence(0.8));

        let mut a = Segmenter::new(SegmenterConfig::default());
        a.push(&audio);
        let batch = a.flush();

        let mut b = Segmenter::new(SegmenterConfig::default());
        for chunk in audio.chunks(173) {
            b.push(chunk);
        }
        let mut streamed = b.take_ready();
        streamed.extend(b.flush());
        assert_eq!(batch, streamed);
    }

    /// Needs `models/silero/silero_vad.onnx` (scripts/fetch_silero.sh); skipped otherwise.
    #[cfg(feature = "ort-backend")]
    #[test]
    fn silero_finds_speech_not_silence() {
        let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../models/silero/silero_vad.onnx");
        if !p.exists() {
            eprintln!("skip: {} missing", p.display());
            return;
        }
        let wav = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/fleurs_kk/wav/10021401141536914918.wav");
        let vad = silero::SileroVad::load(&p, 0.5).unwrap();
        let mut seg = Segmenter::with_detector(SegmenterConfig::default(), Box::new(vad));
        seg.push(&silence(2.0));
        assert!(seg.flush().is_empty(), "silence must not be speech");
        if let Ok(pcm) = crate::audio::read_wav(&wav) {
            seg.reset();
            seg.push(&pcm.to_engine_format());
            let segs = seg.flush();
            assert!(!segs.is_empty());
            let covered: f32 = segs.iter().map(|s| s.end_secs() - s.start_secs()).sum();
            assert!(covered > 10.0, "speech covered {covered}s of a 14.9 s utterance");
        }
    }
}
