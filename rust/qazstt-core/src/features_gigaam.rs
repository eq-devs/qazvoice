//! GigaAM log-mel features (`modeling_gigaam.FeatureExtractor`).
//!
//! GigaAM Multilingual (ai-sage, MIT) was trained on plain torchaudio features, not NeMo's:
//!
//! ```text
//! torchaudio.transforms.MelSpectrogram(sample_rate=16000, n_fft=320, win_length=320,
//!     hop_length=160, n_mels=64, center=False)   # periodic Hann, power 2, HTK mel, no norm
//! log(clamp(x, 1e-9, 1e9))
//! ```
//!
//! No pre-emphasis, no padding, no per-feature normalization. `n_fft = 320` is not a power of
//! two, so this uses a precomputed real DFT table (161 bins × 320 taps ≈ 5 M MAC per second of
//! audio — negligible next to the encoder). Output layout `[n_mels][n_frames]`, i.e. the
//! `features [1, 64, T]` tensor of the exported ONNX graph.

use crate::features::SAMPLE_RATE;

pub const N_FFT: usize = 320;
pub const HOP: usize = 160;
pub const N_MELS: usize = 64;
const N_BINS: usize = N_FFT / 2 + 1;

pub struct GigaMel {
    window: Vec<f32>,
    cos: Vec<f32>, // [N_BINS][N_FFT]
    sin: Vec<f32>,
    fb: Vec<f32>, // [N_MELS][N_BINS]
}

impl Default for GigaMel {
    fn default() -> Self {
        Self::new()
    }
}

impl GigaMel {
    pub fn new() -> Self {
        let tau = 2.0 * std::f64::consts::PI;
        // torch.hann_window(320, periodic=True)
        let window = (0..N_FFT).map(|n| (0.5 - 0.5 * (tau * n as f64 / N_FFT as f64).cos()) as f32).collect();
        let mut cos = vec![0.0f32; N_BINS * N_FFT];
        let mut sin = vec![0.0f32; N_BINS * N_FFT];
        for k in 0..N_BINS {
            for n in 0..N_FFT {
                let a = tau * ((k * n) % N_FFT) as f64 / N_FFT as f64;
                cos[k * N_FFT + n] = a.cos() as f32;
                sin[k * N_FFT + n] = a.sin() as f32;
            }
        }
        Self { window, cos, sin, fb: htk_filterbank() }
    }

    /// Frames for `n` samples with center=False: `(n - n_fft) / hop + 1` (0 if shorter).
    /// The same formula gives the `lengths` input of the ONNX graph.
    pub fn n_frames(n_samples: usize) -> usize {
        if n_samples < N_FFT { 0 } else { (n_samples - N_FFT) / HOP + 1 }
    }

    /// Log-mel `[64][T]` and `T`.
    pub fn compute(&self, audio: &[f32]) -> (Vec<f32>, usize) {
        let t = Self::n_frames(audio.len());
        let mut out = vec![0.0f32; N_MELS * t];
        let mut frame = [0.0f32; N_FFT];
        let mut power = [0.0f32; N_BINS];
        for f in 0..t {
            let s = &audio[f * HOP..f * HOP + N_FFT];
            for n in 0..N_FFT {
                frame[n] = s[n] * self.window[n];
            }
            for k in 0..N_BINS {
                let (c, sn) = (&self.cos[k * N_FFT..(k + 1) * N_FFT], &self.sin[k * N_FFT..(k + 1) * N_FFT]);
                let (mut re, mut im) = (0.0f32, 0.0f32);
                for n in 0..N_FFT {
                    re += frame[n] * c[n];
                    im += frame[n] * sn[n];
                }
                power[k] = re * re + im * im;
            }
            for m in 0..N_MELS {
                let w = &self.fb[m * N_BINS..(m + 1) * N_BINS];
                let acc: f32 = w.iter().zip(power.iter()).map(|(a, b)| a * b).sum();
                out[m * t + f] = acc.clamp(1e-9, 1e9).ln();
            }
        }
        (out, t)
    }
}

fn hz_to_mel(f: f64) -> f64 {
    2595.0 * (1.0 + f / 700.0).log10()
}

fn mel_to_hz(m: f64) -> f64 {
    700.0 * (10f64.powf(m / 2595.0) - 1.0)
}

/// `torchaudio.functional.melscale_fbanks(161, 0, 8000, 64, 16000, norm=None, mel_scale="htk")`,
/// transposed to `[mel][bin]`.
fn htk_filterbank() -> Vec<f32> {
    let f_max = SAMPLE_RATE as f64 / 2.0;
    let all: Vec<f64> = (0..N_BINS).map(|k| f_max * k as f64 / (N_BINS - 1) as f64).collect();
    let (m0, m1) = (hz_to_mel(0.0), hz_to_mel(f_max));
    let f_pts: Vec<f64> = (0..N_MELS + 2).map(|i| mel_to_hz(m0 + (m1 - m0) * i as f64 / (N_MELS + 1) as f64)).collect();
    let mut fb = vec![0.0f32; N_MELS * N_BINS];
    for m in 0..N_MELS {
        let (lo, c, hi) = (f_pts[m], f_pts[m + 1], f_pts[m + 2]);
        for (k, &f) in all.iter().enumerate() {
            let down = (f - lo) / (c - lo);
            let up = (hi - f) / (hi - c);
            fb[m * N_BINS + k] = down.min(up).max(0.0) as f32;
        }
    }
    fb
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_match_center_false() {
        assert_eq!(GigaMel::n_frames(16_000), 99);
        assert_eq!(GigaMel::n_frames(320), 1);
        assert_eq!(GigaMel::n_frames(319), 0);
    }

    #[test]
    fn dft_matches_naive() {
        let g = GigaMel::new();
        let x: Vec<f32> = (0..N_FFT).map(|i| ((i * 7) % 13) as f32 / 13.0 - 0.5).collect();
        for k in [0usize, 1, 17, 80, 160] {
            let (mut re, mut im) = (0.0f64, 0.0f64);
            for (n, &v) in x.iter().enumerate() {
                let a = 2.0 * std::f64::consts::PI * (k * n) as f64 / N_FFT as f64;
                re += v as f64 * a.cos();
                im += v as f64 * a.sin();
            }
            let r: f32 = (0..N_FFT).map(|n| x[n] * g.cos[k * N_FFT + n]).sum();
            let i: f32 = (0..N_FFT).map(|n| x[n] * g.sin[k * N_FFT + n]).sum();
            assert!((r as f64 - re).abs() < 1e-3 && (i as f64 - im).abs() < 1e-3);
        }
    }

    #[test]
    fn tone_peaks_at_htk_bin() {
        // 1 kHz in HTK mel: 2595·log10(1+1000/700) ≈ 999.99; mel(8000) ≈ 2840.02; 65 steps →
        // centre index ≈ 999.99/2840.02·65 − 1 ≈ 21.9
        let g = GigaMel::new();
        let a: Vec<f32> = (0..16_000).map(|i| (2.0 * std::f32::consts::PI * 1000.0 * i as f32 / 16_000.0).sin() * 0.5).collect();
        let (f, t) = g.compute(&a);
        assert_eq!(t, 99);
        let col: Vec<f32> = (0..N_MELS).map(|m| f[m * t + 50]).collect();
        let best = (0..N_MELS).max_by(|&i, &j| col[i].total_cmp(&col[j])).unwrap();
        assert!((21..=23).contains(&best), "peak {best}");
        assert!(f.iter().all(|v| v.is_finite() && *v >= (1e-9f32).ln() - 1e-3));
    }
}
