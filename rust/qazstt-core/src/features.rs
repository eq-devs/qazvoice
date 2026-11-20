//! NeMo-compatible log-mel feature extraction (`AudioToMelSpectrogramPreprocessor`).
//!
//! Reproduces the exact preprocessing of NVIDIA FastConformer models so the
//! exported ONNX encoder sees the same input it saw in training:
//!
//! ```text
//! sample_rate 16000 · pre-emphasis 0.97 · n_fft 512 · win 400 (25 ms) · hop 160 (10 ms)
//! periodic Hann · center=true (reflect pad) · power 2 · 80 slaney-normalized mel bins
//! log(x + 2^-24) · per-feature mean/std normalization (unbiased std, +1e-5)
//! ```
//!
//! Dependency-free: a radix-2 FFT is implemented here. Output layout is
//! `[n_mels][n_frames]` flattened row-major, i.e. exactly the `audio_signal`
//! tensor `[1, 80, T]` the encoder expects.

pub const SAMPLE_RATE: usize = 16_000;
pub const N_FFT: usize = 512;
pub const WIN_LENGTH: usize = 400;
pub const HOP_LENGTH: usize = 160;
pub const N_MELS: usize = 80;
pub const PREEMPH: f32 = 0.97;
const LOG_ZERO_GUARD: f32 = 5.960_464_5e-8; // 2^-24
const F_MIN: f64 = 0.0;
const F_MAX: f64 = 8_000.0;

/// Precomputed window + mel filterbank. Build once, reuse for every utterance.
pub struct MelExtractor {
    window: Vec<f32>,            // WIN_LENGTH, zero-padded to N_FFT when applied
    mel_fb: Vec<f32>,            // [N_MELS][N_FFT/2 + 1]
    twiddle_re: Vec<f32>,        // N_FFT/2
    twiddle_im: Vec<f32>,
    bit_rev: Vec<usize>,         // N_FFT
}

impl Default for MelExtractor {
    fn default() -> Self {
        Self::new()
    }
}

impl MelExtractor {
    pub fn new() -> Self {
        // torch.hann_window(periodic=True): 0.5 - 0.5 cos(2πn/N)
        let window: Vec<f32> = (0..WIN_LENGTH)
            .map(|n| 0.5 - 0.5 * (2.0 * std::f64::consts::PI * n as f64 / WIN_LENGTH as f64).cos())
            .map(|v| v as f32)
            .collect();

        let mel_fb = slaney_mel_filterbank();

        let half = N_FFT / 2;
        let mut twiddle_re = Vec::with_capacity(half);
        let mut twiddle_im = Vec::with_capacity(half);
        for k in 0..half {
            let a = -2.0 * std::f64::consts::PI * k as f64 / N_FFT as f64;
            twiddle_re.push(a.cos() as f32);
            twiddle_im.push(a.sin() as f32);
        }
        let bits = N_FFT.trailing_zeros();
        let bit_rev = (0..N_FFT).map(|i| i.reverse_bits() >> (usize::BITS - bits)).collect();

        Self { window, mel_fb, twiddle_re, twiddle_im, bit_rev }
    }

    /// Number of frames NeMo/torch.stft produce for `n` samples with center=true.
    pub fn n_frames(n_samples: usize) -> usize {
        n_samples / HOP_LENGTH + 1
    }

    /// Compute normalized log-mel features. Returns `(features, n_frames)` with
    /// `features.len() == N_MELS * n_frames`, laid out `[mel][frame]`.
    pub fn compute(&self, audio: &[f32]) -> (Vec<f32>, usize) {
        let (mut mel, n_frames, valid) = self.log_mel(audio);
        normalize_per_feature(&mut mel, n_frames, valid);
        (mel, n_frames)
    }

    /// Un-normalized log-mel `[mel][frame]`, total frames, valid frames.
    fn log_mel(&self, audio: &[f32]) -> (Vec<f32>, usize, usize) {
        if audio.is_empty() {
            return (Vec::new(), 0, 0);
        }
        // 1. pre-emphasis
        let mut x = Vec::with_capacity(audio.len());
        x.push(audio[0]);
        for i in 1..audio.len() {
            x.push(audio[i] - PREEMPH * audio[i - 1]);
        }
        // 2. reflect pad by n_fft/2 on both sides (torch.stft center=True, pad_mode="reflect")
        let pad = N_FFT / 2;
        let padded = reflect_pad(&x, pad);
        let n_frames = Self::n_frames(x.len());
        let n_bins = N_FFT / 2 + 1;

        // 3. STFT → power → mel → log
        let mut mel = vec![0.0f32; N_MELS * n_frames];
        let mut re = vec![0.0f32; N_FFT];
        let mut im = vec![0.0f32; N_FFT];
        let mut power = vec![0.0f32; n_bins];
        for t in 0..n_frames {
            let start = t * HOP_LENGTH;
            re.iter_mut().for_each(|v| *v = 0.0);
            im.iter_mut().for_each(|v| *v = 0.0);
            // torch pads the 400-sample window to 512 centred: offset (512-400)/2 = 56
            let off = (N_FFT - WIN_LENGTH) / 2;
            for n in 0..WIN_LENGTH {
                let idx = start + off + n;
                let s = if idx < padded.len() { padded[idx] } else { 0.0 };
                re[off + n] = s * self.window[n];
            }
            self.fft_in_place(&mut re, &mut im);
            for k in 0..n_bins {
                power[k] = re[k] * re[k] + im[k] * im[k];
            }
            for m in 0..N_MELS {
                let fb = &self.mel_fb[m * n_bins..(m + 1) * n_bins];
                let mut acc = 0.0f32;
                for k in 0..n_bins {
                    acc += fb[k] * power[k];
                }
                mel[m * n_frames + t] = (acc + LOG_ZERO_GUARD).ln();
            }
        }

        let valid = Self::valid_frames(x.len()).min(n_frames);
        (mel, n_frames, valid)
    }

    /// Frames NeMo treats as valid (`get_seq_len`): `n_samples / hop`. The STFT
    /// tensor has one more frame; that last frame is masked to zero and the
    /// encoder receives `length = valid_frames`.
    pub fn valid_frames(n_samples: usize) -> usize {
        n_samples / HOP_LENGTH
    }

    fn fft_in_place(&self, re: &mut [f32], im: &mut [f32]) {
        let n = N_FFT;
        for i in 0..n {
            let j = self.bit_rev[i];
            if j > i {
                re.swap(i, j);
                im.swap(i, j);
            }
        }
        let mut len = 2;
        while len <= n {
            let half = len / 2;
            let step = n / len;
            let mut i = 0;
            while i < n {
                for k in 0..half {
                    let wr = self.twiddle_re[k * step];
                    let wi = self.twiddle_im[k * step];
                    let (ar, ai) = (re[i + k], im[i + k]);
                    let (br, bi) = (re[i + k + half], im[i + k + half]);
                    let tr = br * wr - bi * wi;
                    let ti = br * wi + bi * wr;
                    re[i + k] = ar + tr;
                    im[i + k] = ai + ti;
                    re[i + k + half] = ar - tr;
                    im[i + k + half] = ai - ti;
                }
                i += len;
            }
            len <<= 1;
        }
    }
}

/// NeMo `normalize_batch(per_feature)` over the valid frames (unbiased std,
/// +1e-5), then `masked_fill` the padding frames with 0.
fn normalize_per_feature(mel: &mut [f32], n_frames: usize, valid: usize) {
    for m in 0..N_MELS {
        let row = &mut mel[m * n_frames..(m + 1) * n_frames];
        let n = valid as f32;
        let mean = row[..valid].iter().sum::<f32>() / n.max(1.0);
        let var = if valid > 1 {
            row[..valid].iter().map(|v| (v - mean) * (v - mean)).sum::<f32>() / (n - 1.0)
        } else {
            0.0
        };
        let std = var.sqrt() + 1e-5;
        for v in row[..valid].iter_mut() {
            *v = (*v - mean) / std;
        }
        for v in row[valid..].iter_mut() {
            *v = 0.0;
        }
    }
}

fn reflect_pad(x: &[f32], pad: usize) -> Vec<f32> {
    let n = x.len();
    let mut out = Vec::with_capacity(n + 2 * pad);
    if n <= pad {
        // torch would raise here; the engine pads utterances to ≥ 1 s so this is
        // only reachable through direct calls. Zero-pad instead of panicking.
        out.extend(std::iter::repeat(0.0).take(pad));
        out.extend_from_slice(x);
        out.extend(std::iter::repeat(0.0).take(pad));
        return out;
    }
    for i in 0..pad {
        out.push(x[pad - i]); // reflect without repeating the edge sample
    }
    out.extend_from_slice(x);
    for i in 0..pad {
        out.push(x[n - 2 - i]);
    }
    out
}

// ─── librosa-compatible slaney mel ───

fn hz_to_mel(f: f64) -> f64 {
    let f_sp = 200.0 / 3.0;
    let min_log_hz = 1000.0;
    let min_log_mel = min_log_hz / f_sp;
    let logstep = (6.4f64).ln() / 27.0;
    if f >= min_log_hz {
        min_log_mel + (f / min_log_hz).ln() / logstep
    } else {
        f / f_sp
    }
}

fn mel_to_hz(m: f64) -> f64 {
    let f_sp = 200.0 / 3.0;
    let min_log_hz = 1000.0;
    let min_log_mel = min_log_hz / f_sp;
    let logstep = (6.4f64).ln() / 27.0;
    if m >= min_log_mel {
        min_log_hz * (logstep * (m - min_log_mel)).exp()
    } else {
        f_sp * m
    }
}

/// `librosa.filters.mel(sr=16000, n_fft=512, n_mels=80, fmin=0, fmax=8000)` — htk=False, norm="slaney".
fn slaney_mel_filterbank() -> Vec<f32> {
    let n_bins = N_FFT / 2 + 1;
    let fft_freqs: Vec<f64> = (0..n_bins).map(|k| k as f64 * SAMPLE_RATE as f64 / N_FFT as f64).collect();
    let m_min = hz_to_mel(F_MIN);
    let m_max = hz_to_mel(F_MAX);
    let mel_pts: Vec<f64> = (0..N_MELS + 2)
        .map(|i| mel_to_hz(m_min + (m_max - m_min) * i as f64 / (N_MELS + 1) as f64))
        .collect();
    let mut fb = vec![0.0f32; N_MELS * n_bins];
    for m in 0..N_MELS {
        let (f0, f1, f2) = (mel_pts[m], mel_pts[m + 1], mel_pts[m + 2]);
        let enorm = 2.0 / (f2 - f0);
        for (k, &f) in fft_freqs.iter().enumerate() {
            let lower = (f - f0) / (f1 - f0);
            let upper = (f2 - f) / (f2 - f1);
            let w = lower.min(upper).max(0.0);
            fb[m * n_bins + k] = (w * enorm) as f32;
        }
    }
    fb
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_count_matches_torch_stft_center() {
        assert_eq!(MelExtractor::n_frames(16_000), 101);
        assert_eq!(MelExtractor::n_frames(16_000 - 1), 100);
        assert_eq!(MelExtractor::n_frames(160), 2);
    }

    #[test]
    fn mel_points_match_librosa_slaney() {
        // mel(8000) = 15 + ln(8)/(ln 6.4/27) ≈ 45.25; one of 81 steps ≈ 0.5587 mel ≈ 37.25 Hz
        // (= librosa.mel_frequencies(n_mels=82, fmin=0, fmax=8000)[1]).
        assert!((hz_to_mel(8000.0) - 45.2456).abs() < 1e-3);
        let m_min = hz_to_mel(0.0);
        let m_max = hz_to_mel(8000.0);
        let f1 = mel_to_hz(m_min + (m_max - m_min) / 81.0);
        assert!((f1 - 37.25).abs() < 0.1, "{f1}");
        // round trip
        for f in [100.0, 999.0, 1000.0, 3456.7, 8000.0] {
            assert!((mel_to_hz(hz_to_mel(f)) - f).abs() < 1e-6);
        }
    }

    #[test]
    fn filterbank_rows_are_triangles_summing_sanely() {
        let fb = slaney_mel_filterbank();
        let n_bins = N_FFT / 2 + 1;
        for m in 0..N_MELS {
            let row = &fb[m * n_bins..(m + 1) * n_bins];
            assert!(row.iter().all(|&v| v >= 0.0));
            assert!(row.iter().any(|&v| v > 0.0), "empty mel row {m}");
        }
    }

    #[test]
    fn fft_matches_naive_dft() {
        let ex = MelExtractor::new();
        let mut re: Vec<f32> = (0..N_FFT).map(|i| ((i * 7) % 13) as f32 / 13.0 - 0.5).collect();
        let mut im = vec![0.0f32; N_FFT];
        let input = re.clone();
        ex.fft_in_place(&mut re, &mut im);
        for k in [0usize, 1, 5, 100, 255, 256] {
            let mut sr = 0.0f64;
            let mut si = 0.0f64;
            for (n, &x) in input.iter().enumerate() {
                let a = -2.0 * std::f64::consts::PI * (k * n) as f64 / N_FFT as f64;
                sr += x as f64 * a.cos();
                si += x as f64 * a.sin();
            }
            assert!((re[k] as f64 - sr).abs() < 1e-3, "k={k} re {} vs {sr}", re[k]);
            assert!((im[k] as f64 - si).abs() < 1e-3, "k={k} im {} vs {si}", im[k]);
        }
    }

    #[test]
    fn tone_lights_up_the_right_mel_bin() {
        // In the un-normalized log-mel, a 1 kHz tone must peak at the slaney mel
        // bin for 1 kHz: mel(1000) = 15 → bin ≈ 15 / 45.25 · 81 − 1 ≈ 26.
        let ex = MelExtractor::new();
        let audio: Vec<f32> = (0..16_000)
            .map(|i| (2.0 * std::f32::consts::PI * 1000.0 * i as f32 / 16_000.0).sin() * 0.5)
            .collect();
        let (raw, t, valid) = ex.log_mel(&audio);
        assert_eq!((t, valid), (101, 100));
        let mid = t / 2;
        let col: Vec<f32> = (0..N_MELS).map(|m| raw[m * t + mid]).collect();
        let argmax = col.iter().enumerate().fold((0, f32::MIN), |a, (i, &v)| if v > a.1 { (i, v) } else { a }).0;
        assert!((24..=28).contains(&argmax), "argmax bin {argmax}");

        // Normalized output: finite, padding frame masked.
        let (feat, t2) = ex.compute(&audio);
        assert_eq!(feat.len(), N_MELS * t2);
        assert!(feat.iter().all(|v| v.is_finite()));
        assert!((0..N_MELS).all(|m| feat[m * t2 + t2 - 1] == 0.0));
    }

    #[test]
    fn reflect_pad_is_symmetric() {
        let x = [1.0, 2.0, 3.0, 4.0, 5.0];
        let p = reflect_pad(&x, 2);
        assert_eq!(p, vec![3.0, 2.0, 1.0, 2.0, 3.0, 4.0, 5.0, 4.0, 3.0]);
    }
}
