//! Audio input: WAV decoding, channel mixing, and resampling to 16 kHz mono.
//!
//! Dependency-free on purpose: this is the part of the engine that must compile
//! for every target (desktop, iOS, Android, WASM) without a native toolchain
//! surprise. Formats beyond PCM WAV (mp3, ogg, m4a) are the caller's job —
//! decode them with the platform decoder and hand PCM to [`crate::Engine::feed`].

use std::fmt;

/// The model's sample rate. Everything inside the engine runs at this rate.
pub const TARGET_SAMPLE_RATE: u32 = 16_000;

#[derive(Debug)]
pub enum AudioError {
    NotWav,
    UnsupportedFormat(String),
    Truncated,
    Io(std::io::Error),
}

impl fmt::Display for AudioError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AudioError::NotWav => write!(f, "not a RIFF/WAVE file"),
            AudioError::UnsupportedFormat(s) => write!(f, "unsupported WAV format: {s}"),
            AudioError::Truncated => write!(f, "WAV data truncated"),
            AudioError::Io(e) => write!(f, "io error: {e}"),
        }
    }
}

impl std::error::Error for AudioError {}

impl From<std::io::Error> for AudioError {
    fn from(e: std::io::Error) -> Self {
        AudioError::Io(e)
    }
}

/// Decoded PCM audio, interleaved, f32 in [-1, 1].
#[derive(Debug, Clone)]
pub struct PcmAudio {
    pub sample_rate: u32,
    pub channels: u16,
    pub samples: Vec<f32>,
}

impl PcmAudio {
    /// Mix down to mono by averaging channels.
    pub fn to_mono(&self) -> Vec<f32> {
        if self.channels <= 1 {
            return self.samples.clone();
        }
        let ch = self.channels as usize;
        self.samples
            .chunks_exact(ch)
            .map(|frame| frame.iter().sum::<f32>() / ch as f32)
            .collect()
    }

    /// Mono, 16 kHz — what the engine wants.
    pub fn to_engine_format(&self) -> Vec<f32> {
        let mono = self.to_mono();
        resample(&mono, self.sample_rate, TARGET_SAMPLE_RATE)
    }

    pub fn duration_secs(&self) -> f32 {
        if self.channels == 0 || self.sample_rate == 0 {
            return 0.0;
        }
        self.samples.len() as f32 / self.channels as f32 / self.sample_rate as f32
    }
}

/// Sample rates and channel counts [`decode_wav`] accepts.
pub const MIN_RATE: u32 = 4_000;
pub const MAX_RATE: u32 = 384_000;
pub const MAX_CHANNELS: u16 = 8;

/// Parse a RIFF/WAVE byte buffer. Supports PCM 8/16/24/32-bit integer and
/// 32-bit IEEE float, 1–8 channels, 4–384 kHz. WAVE_FORMAT_EXTENSIBLE
/// is accepted when its sub-format is PCM or float. Rates outside
/// [`MIN_RATE`]..[`MAX_RATE`] and more than [`MAX_CHANNELS`] channels are refused.
pub fn decode_wav(bytes: &[u8]) -> Result<PcmAudio, AudioError> {
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err(AudioError::NotWav);
    }

    let mut pos = 12usize;
    let mut fmt: Option<(u16, u16, u32, u16)> = None; // (format_tag, channels, rate, bits)
    let mut data: Option<&[u8]> = None;

    while pos + 8 <= bytes.len() {
        let id = &bytes[pos..pos + 4];
        let size = u32::from_le_bytes([bytes[pos + 4], bytes[pos + 5], bytes[pos + 6], bytes[pos + 7]])
            as usize;
        let body_start = pos + 8;
        let body_end = body_start.saturating_add(size).min(bytes.len());
        let body = &bytes[body_start..body_end];

        match id {
            b"fmt " => {
                if body.len() < 16 {
                    return Err(AudioError::Truncated);
                }
                let mut tag = u16::from_le_bytes([body[0], body[1]]);
                let channels = u16::from_le_bytes([body[2], body[3]]);
                let rate = u32::from_le_bytes([body[4], body[5], body[6], body[7]]);
                let bits = u16::from_le_bytes([body[14], body[15]]);
                if tag == 0xFFFE && body.len() >= 26 {
                    // WAVE_FORMAT_EXTENSIBLE: sub-format GUID's first two bytes carry the real tag.
                    tag = u16::from_le_bytes([body[24], body[25]]);
                }
                fmt = Some((tag, channels, rate, bits));
            }
            b"data" => {
                data = Some(body);
            }
            _ => {}
        }
        // Chunks are word-aligned.
        pos = body_start + size + (size & 1);
    }

    let (tag, channels, rate, bits) = fmt.ok_or(AudioError::Truncated)?;
    let data = data.ok_or(AudioError::Truncated)?;
    // A header can claim anything. Resampling from 1 Hz to 16 kHz would
    // multiply the data 16 000 times, so only rates and channel counts that
    // real recordings use are accepted.
    if !(MIN_RATE..=MAX_RATE).contains(&rate) {
        return Err(AudioError::UnsupportedFormat(format!("sample rate {rate} Hz (supported: {MIN_RATE}..{MAX_RATE})")));
    }
    if !(1..=MAX_CHANNELS).contains(&channels) {
        return Err(AudioError::UnsupportedFormat(format!("{channels} channels (supported: 1..{MAX_CHANNELS})")));
    }

    let samples: Vec<f32> = match (tag, bits) {
        (1, 8) => data.iter().map(|&b| (b as f32 - 128.0) / 128.0).collect(),
        (1, 16) => data
            .chunks_exact(2)
            .map(|c| i16::from_le_bytes([c[0], c[1]]) as f32 / 32768.0)
            .collect(),
        (1, 24) => data
            .chunks_exact(3)
            .map(|c| {
                let v = i32::from_le_bytes([0, c[0], c[1], c[2]]) >> 8;
                v as f32 / 8_388_608.0
            })
            .collect(),
        (1, 32) => data
            .chunks_exact(4)
            .map(|c| i32::from_le_bytes([c[0], c[1], c[2], c[3]]) as f32 / 2_147_483_648.0)
            .collect(),
        (3, 32) => data
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            // NaN or infinity would poison every feature frame they touch.
            .map(|v| if v.is_finite() { v.clamp(-1.0, 1.0) } else { 0.0 })
            .collect(),
        _ => {
            return Err(AudioError::UnsupportedFormat(format!(
                "format_tag={tag} bits={bits}"
            )))
        }
    };

    Ok(PcmAudio {
        sample_rate: rate,
        channels,
        samples,
    })
}

/// Read and decode a WAV file from disk.
pub fn read_wav(path: &std::path::Path) -> Result<PcmAudio, AudioError> {
    let bytes = std::fs::read(path)?;
    decode_wav(&bytes)
}

/// Encode mono f32 PCM as 16-bit WAV bytes (used by the CLI for segment dumps).
pub fn encode_wav_mono16(samples: &[f32], sample_rate: u32) -> Vec<u8> {
    let data_len = (samples.len() * 2) as u32;
    let mut out = Vec::with_capacity(44 + data_len as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&1u16.to_le_bytes()); // mono
    out.extend_from_slice(&sample_rate.to_le_bytes());
    out.extend_from_slice(&(sample_rate * 2).to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for &s in samples {
        let v = (s.clamp(-1.0, 1.0) * 32767.0).round() as i16;
        out.extend_from_slice(&v.to_le_bytes());
    }
    out
}

/// Convert interleaved i16 PCM to f32.
pub fn i16_to_f32(input: &[i16]) -> Vec<f32> {
    input.iter().map(|&s| s as f32 / 32768.0).collect()
}

const HALF_TAPS: isize = 16;

/// One output sample of the windowed-sinc filter: `center` is its position in
/// input samples, `at(k)` the input sample with absolute index `k` (`None` past
/// either end of the stream; those taps are left out and the rest renormalized).
fn sinc_tap(center: f64, cutoff: f64, at: impl Fn(isize) -> Option<f32>) -> f32 {
    let base = center.floor() as isize;
    let mut acc = 0.0f64;
    let mut wsum = 0.0f64;
    for k in (base - HALF_TAPS + 1)..=(base + HALF_TAPS) {
        let Some(v) = at(k) else { continue };
        let x = (k as f64 - center) * cutoff;
        let sinc = if x.abs() < 1e-9 { 1.0 } else { (std::f64::consts::PI * x).sin() / (std::f64::consts::PI * x) };
        // Blackman window over the tap span.
        let t = (k as f64 - center) / HALF_TAPS as f64; // -1..1
        let w = 0.42 + 0.5 * (std::f64::consts::PI * t).cos() + 0.08 * (2.0 * std::f64::consts::PI * t).cos();
        let coeff = sinc * w;
        acc += v as f64 * coeff;
        wsum += coeff;
    }
    if wsum.abs() > 1e-12 {
        (acc / wsum) as f32
    } else {
        0.0
    }
}

/// Low-pass cutoff relative to the input rate: Nyquist of the lower rate.
fn sinc_cutoff(from_rate: u32, to_rate: u32) -> f64 {
    (if to_rate < from_rate { to_rate as f64 / from_rate as f64 } else { 1.0 }) * 0.95
}

/// Windowed-sinc resampler (Blackman window, 16 taps per side) for a whole
/// buffer. Quality is more than enough for ASR; for extreme ratios it still
/// behaves because the low-pass cutoff tracks the lower of the two rates.
/// For audio that arrives in pieces use [`Resampler`]: calling this per piece
/// distorts every piece boundary.
pub fn resample(input: &[f32], from_rate: u32, to_rate: u32) -> Vec<f32> {
    if from_rate == to_rate || input.is_empty() {
        return input.to_vec();
    }
    let ratio = to_rate as f64 / from_rate as f64;
    let out_len = ((input.len() as f64) * ratio).round() as usize;
    let cutoff = sinc_cutoff(from_rate, to_rate);
    (0..out_len)
        .map(|n| sinc_tap(n as f64 / ratio, cutoff, |k| (k >= 0).then(|| input.get(k as usize).copied()).flatten()))
        .collect()
}

/// Streaming version of [`resample`]: feed mono audio in pieces of any size and
/// get the same samples as one call over the whole stream (history is kept
/// across pieces, so there is nothing to hear at the seams).
pub struct Resampler {
    from_rate: u32,
    to_rate: u32,
    ratio: f64,
    cutoff: f64,
    /// Input still needed by future outputs; `buf[0]` has absolute index `offset`.
    buf: Vec<f32>,
    offset: usize,
    /// Input samples received so far.
    total_in: usize,
    /// Output samples produced so far.
    produced: usize,
}

impl Resampler {
    pub fn new(from_rate: u32, to_rate: u32) -> Self {
        Self {
            from_rate,
            to_rate,
            ratio: to_rate as f64 / from_rate.max(1) as f64,
            cutoff: sinc_cutoff(from_rate.max(1), to_rate),
            buf: Vec::new(),
            offset: 0,
            total_in: 0,
            produced: 0,
        }
    }

    pub fn rates(&self) -> (u32, u32) {
        (self.from_rate, self.to_rate)
    }

    /// Feed a piece; returns the output samples that are now final.
    pub fn process(&mut self, input: &[f32]) -> Vec<f32> {
        if self.from_rate == self.to_rate {
            return input.to_vec();
        }
        self.buf.extend_from_slice(input);
        self.total_in += input.len();
        self.drain(false)
    }

    /// End of stream: the last few samples, which were waiting for input that will not come.
    pub fn flush(&mut self) -> Vec<f32> {
        if self.from_rate == self.to_rate {
            return Vec::new();
        }
        let out = self.drain(true);
        self.buf.clear();
        self.offset = self.total_in;
        out
    }

    fn drain(&mut self, last: bool) -> Vec<f32> {
        let mut out = Vec::new();
        let total_out = (self.total_in as f64 * self.ratio).round() as usize;
        loop {
            let center = self.produced as f64 / self.ratio;
            let base = center.floor() as isize;
            let ready = if last { self.produced < total_out } else { base + HALF_TAPS < self.total_in as isize };
            if !ready {
                break;
            }
            let (buf, offset) = (&self.buf, self.offset as isize);
            out.push(sinc_tap(center, self.cutoff, |k| (k >= offset).then(|| buf.get((k - offset) as usize).copied()).flatten()));
            self.produced += 1;
        }
        // Drop input no future output can reach.
        let next_base = (self.produced as f64 / self.ratio).floor() as isize;
        let keep_from = (next_base - HALF_TAPS + 1).max(self.offset as isize) as usize;
        if keep_from > self.offset {
            self.buf.drain(..(keep_from - self.offset).min(self.buf.len()));
            self.offset = keep_from;
        }
        out
    }
}

/// Interleaved frames → mono by averaging channels (borrowing the input).
pub fn mix_to_mono(samples: &[f32], channels: u16) -> std::borrow::Cow<'_, [f32]> {
    if channels <= 1 {
        return std::borrow::Cow::Borrowed(samples);
    }
    let ch = channels as usize;
    std::borrow::Cow::Owned(samples.chunks_exact(ch).map(|f| f.iter().sum::<f32>() / ch as f32).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(rate: u32, hz: f32, secs: f32) -> Vec<f32> {
        let n = (rate as f32 * secs) as usize;
        (0..n)
            .map(|i| (2.0 * std::f32::consts::PI * hz * i as f32 / rate as f32).sin() * 0.5)
            .collect()
    }

    #[test]
    fn wav_roundtrip() {
        let s = sine(16_000, 440.0, 0.1);
        let bytes = encode_wav_mono16(&s, 16_000);
        let pcm = decode_wav(&bytes).unwrap();
        assert_eq!(pcm.sample_rate, 16_000);
        assert_eq!(pcm.channels, 1);
        assert_eq!(pcm.samples.len(), s.len());
        let max_err = pcm
            .samples
            .iter()
            .zip(&s)
            .map(|(a, b)| (a - b).abs())
            .fold(0.0f32, f32::max);
        assert!(max_err < 1e-3, "max_err={max_err}");
    }

    #[test]
    fn resample_preserves_tone() {
        // 48k -> 16k on a 440 Hz tone should stay a 440 Hz tone with ~same amplitude.
        let s = sine(48_000, 440.0, 0.5);
        let r = resample(&s, 48_000, 16_000);
        assert!((r.len() as i64 - 8_000).abs() <= 1);
        // Compare against a directly generated 16k tone, skipping the edges.
        let ref16 = sine(16_000, 440.0, 0.5);
        let err: f32 = r[100..7_900]
            .iter()
            .zip(&ref16[100..7_900])
            .map(|(a, b)| (a - b).abs())
            .sum::<f32>()
            / 7_800.0;
        assert!(err < 0.01, "mean abs err {err}");
    }

    #[test]
    fn resample_upsamples() {
        let s = sine(8_000, 300.0, 0.25);
        let r = resample(&s, 8_000, 16_000);
        assert_eq!(r.len(), 4_000);
    }

    #[test]
    fn streaming_resampler_matches_whole_buffer() {
        for (from, to) in [(48_000, 16_000), (44_100, 16_000), (8_000, 16_000), (22_050, 24_000)] {
            let s = sine(from, 440.0, 0.37);
            let whole = resample(&s, from, to);
            for piece in [1usize, 7, 160, 4_096, s.len()] {
                let mut r = Resampler::new(from, to);
                let mut out = Vec::new();
                for c in s.chunks(piece) {
                    out.extend(r.process(c));
                }
                out.extend(r.flush());
                assert_eq!(out.len(), whole.len(), "{from}->{to} piece {piece}");
                let max = out.iter().zip(&whole).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
                assert!(max < 1e-6, "{from}->{to} piece {piece}: max diff {max}");
            }
        }
    }

    #[test]
    fn stereo_mixdown() {
        let pcm = PcmAudio {
            sample_rate: 16_000,
            channels: 2,
            samples: vec![1.0, 0.0, 0.5, 0.5, -1.0, 1.0],
        };
        assert_eq!(pcm.to_mono(), vec![0.5, 0.5, 0.0]);
    }

    #[test]
    fn rejects_garbage() {
        assert!(matches!(decode_wav(b"hello"), Err(AudioError::NotWav)));
    }
}
