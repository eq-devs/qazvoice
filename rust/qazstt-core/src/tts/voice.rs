//! VITS inference (Piper ONNX export): inputs `input` [1, n] i64, `input_lengths` [1],
//! `scales` [noise, length, noise_w] f32, optional `sid` [1]; output `output` f32 audio.

use std::path::{Path, PathBuf};

use ort::session::Session;
use ort::value::Tensor;

use super::{text, SynthOptions, VoiceConfig};
use crate::backend::BackendError;

pub struct Voice {
    session: Session,
    config: VoiceConfig,
    multi_speaker: bool,
    device: &'static str,
}

impl Voice {
    /// `path` is the `.onnx` file (config next to it as `.onnx.json`) or a
    /// directory holding exactly one such pair.
    pub fn load(path: &Path, threads: usize) -> Result<Self, BackendError> {
        Self::load_on(path, threads, false)
    }

    /// As [`Voice::load`]; with `use_gpu` the voice runs on the build's
    /// accelerator (`cuda`, …) when that can be set up, else on the CPU.
    pub fn load_on(path: &Path, threads: usize, use_gpu: bool) -> Result<Self, BackendError> {
        crate::backend::fastconformer::ensure_runtime()?;
        let onnx = resolve(path)?;
        let json = PathBuf::from(format!("{}.json", onnx.display()));
        let src = std::fs::read_to_string(&json).map_err(|e| BackendError::ModelLoad(format!("{}: {e}", json.display())))?;
        let config = VoiceConfig::parse(&src).map_err(|e| BackendError::ModelLoad(format!("{}: {e}", json.display())))?;
        let (session, device) = crate::backend::fastconformer::open_session(&onnx, threads, use_gpu)?;
        let multi_speaker = session.inputs().iter().any(|i| i.name() == "sid");
        Ok(Self { session, config, multi_speaker, device })
    }

    /// Where the voice runs: `"cpu"`, or the accelerator (`"cuda"`, …).
    pub fn device(&self) -> &'static str {
        self.device
    }

    pub fn sample_rate(&self) -> u32 {
        self.config.sample_rate
    }

    /// Speaker names in id order (empty for single-speaker voices).
    pub fn speakers(&self) -> &[String] {
        if self.multi_speaker {
            &self.config.speakers
        } else {
            &[]
        }
    }

    pub fn config(&self) -> &VoiceConfig {
        &self.config
    }

    /// Whole text → audio, sentences joined with `sentence_pause_ms` of silence.
    pub fn synthesize(&mut self, text_in: &str, opts: &SynthOptions) -> Result<Vec<f32>, BackendError> {
        let mut out = Vec::new();
        for ph in text::text_to_phonemes(text_in) {
            out.extend(self.synthesize_phonemes(&ph, opts)?);
            out.extend(std::iter::repeat(0.0).take(self.pause_samples(opts)));
        }
        Ok(out)
    }

    pub fn pause_samples(&self, opts: &SynthOptions) -> usize {
        (self.config.sample_rate as u64 * opts.sentence_pause_ms as u64 / 1000) as usize
    }

    /// One sentence of Piper phonemes → audio in [-1, 1].
    pub fn synthesize_phonemes(&mut self, phonemes: &str, opts: &SynthOptions) -> Result<Vec<f32>, BackendError> {
        let inf = |e: ort::Error| BackendError::Inference(e.to_string());
        let ids = self.config.ids(phonemes);
        let n = ids.len();
        let speed = if opts.speed.is_finite() { opts.speed.clamp(0.5, 2.0) } else { 1.0 };
        let scales = vec![self.config.noise_scale, self.config.length_scale / speed, self.config.noise_w];
        let input = Tensor::from_array(([1usize, n], ids)).map_err(inf)?;
        let lengths = Tensor::from_array(([1usize], vec![n as i64])).map_err(inf)?;
        let scales = Tensor::from_array(([3usize], scales)).map_err(inf)?;
        let outputs = if self.multi_speaker {
            let sid = opts.speaker.unwrap_or(0).min(self.config.speakers.len().saturating_sub(1)) as i64;
            let sid = Tensor::from_array(([1usize], vec![sid])).map_err(inf)?;
            run(&mut self.session, ort::inputs!["input" => input, "input_lengths" => lengths, "scales" => scales, "sid" => sid]).map_err(inf)?
        } else {
            run(&mut self.session, ort::inputs!["input" => input, "input_lengths" => lengths, "scales" => scales]).map_err(inf)?
        };
        let (_, audio) = outputs[0].try_extract_tensor::<f32>().map_err(inf)?;
        // Piper normalizes each utterance to its peak; keep that, with headroom.
        let peak = audio.iter().fold(0.0f32, |m, &x| m.max(x.abs())).max(0.01);
        let gain = 0.95 / peak;
        Ok(audio.iter().map(|&x| x * gain).collect())
    }
}

fn resolve(path: &Path) -> Result<PathBuf, BackendError> {
    if !path.is_dir() {
        return Ok(path.to_path_buf());
    }
    let mut found: Vec<PathBuf> = std::fs::read_dir(path)
        .map_err(|e| BackendError::ModelLoad(format!("{}: {e}", path.display())))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "onnx"))
        .collect();
    found.sort();
    match found.len() {
        1 => Ok(found.remove(0)),
        0 => Err(BackendError::ModelLoad(format!("{}: no .onnx voice found", path.display()))),
        _ => Err(BackendError::ModelLoad(format!("{}: several .onnx files; pass one", path.display()))),
    }
}

fn run<'s, const N: usize>(
    session: &'s mut ort::session::Session,
    inputs: impl Into<ort::session::SessionInputs<'s, 's, N>>,
) -> ort::Result<ort::session::SessionOutputs<'s>> {
    match crate::backend::fastconformer::run_options() {
        Some(o) => session.run_with_options(inputs, o),
        None => session.run(inputs),
    }
}
