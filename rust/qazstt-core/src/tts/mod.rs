//! Kazakh text-to-speech: own text front end and G2P (no espeak-ng) driving a
//! Piper / VITS voice exported to ONNX.
//!
//! ```ignore
//! let mut v = qazstt_core::tts::Voice::load("models/tts-kk".as_ref(), 2)?;
//! let pcm = v.synthesize("Сәлем, әлем!", &Default::default())?; // f32 mono at v.sample_rate()
//! ```
//!
//! | module  | role                                                     | deps |
//! |---------|----------------------------------------------------------|------|
//! | `text`  | numbers, abbreviations, Latin → Cyrillic, sentences      | —    |
//! | `g2p`   | Kazakh letters → Piper IPA phonemes, stress              | —    |
//! | `voice` | phoneme ids → waveform (VITS, ONNX Runtime)              | ort  |

pub mod g2p;
pub mod text;
#[cfg(feature = "ort-backend")]
mod voice;
#[cfg(feature = "ort-backend")]
pub use voice::Voice;

/// Per-request synthesis settings.
#[derive(Debug, Clone)]
pub struct SynthOptions {
    /// Speaker index (multi-speaker voices); `None` = the voice's default.
    pub speaker: Option<usize>,
    /// 1.0 = natural pace; 1.25 = 25 % faster.
    pub speed: f32,
    /// Silence inserted after each sentence.
    pub sentence_pause_ms: u32,
}

impl Default for SynthOptions {
    fn default() -> Self {
        Self { speaker: None, speed: 1.0, sentence_pause_ms: 250 }
    }
}

/// `"key": value` pairs and the `phoneme_id_map` / `speaker_id_map` objects of a
/// Piper `.onnx.json`, read without a JSON dependency (the core crate has none).
#[derive(Debug, Clone)]
pub struct VoiceConfig {
    pub sample_rate: u32,
    pub noise_scale: f32,
    pub length_scale: f32,
    pub noise_w: f32,
    pub phoneme_ids: std::collections::HashMap<char, i64>,
    /// Speaker names in id order.
    pub speakers: Vec<String>,
}

impl VoiceConfig {
    pub fn parse(src: &str) -> Result<Self, String> {
        let num = |key: &str| -> Option<f32> {
            let at = src.find(&format!("\"{key}\""))? + key.len() + 2;
            let rest = src[at..].trim_start().strip_prefix(':')?.trim_start();
            let end = rest.find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-' || c == 'e')).unwrap_or(rest.len());
            rest[..end].parse().ok()
        };
        let mut phoneme_ids = std::collections::HashMap::new();
        for (k, v) in object_entries(src, "phoneme_id_map")? {
            let mut ks = k.chars();
            if let (Some(c), None) = (ks.next(), ks.next()) {
                let id = v.trim_matches(|c| c == '[' || c == ']' || c == ' ').split(',').next().unwrap_or("").trim();
                phoneme_ids.insert(c, id.parse().map_err(|_| format!("bad phoneme id for {k:?}: {v}"))?);
            }
        }
        let mut speakers: Vec<(usize, String)> = object_entries(src, "speaker_id_map")
            .unwrap_or_default()
            .into_iter()
            .filter_map(|(k, v)| Some((v.trim().parse().ok()?, k)))
            .collect();
        speakers.sort();
        Ok(Self {
            sample_rate: num("sample_rate").ok_or("sample_rate missing")? as u32,
            noise_scale: num("noise_scale").unwrap_or(0.667),
            length_scale: num("length_scale").unwrap_or(1.0),
            noise_w: num("noise_w").unwrap_or(0.8),
            phoneme_ids,
            speakers: speakers.into_iter().map(|(_, n)| n).collect(),
        })
    }

    /// Piper's id sequence: `^ _ (p _)* $`.
    pub fn ids(&self, phonemes: &str) -> Vec<i64> {
        let id = |c: char| self.phoneme_ids.get(&c).copied();
        let pad = id('_').unwrap_or(0);
        let mut out = vec![id('^').unwrap_or(1), pad];
        for c in phonemes.chars() {
            if let Some(i) = id(c) {
                out.push(i);
                out.push(pad);
            }
        }
        out.push(id('$').unwrap_or(2));
        out
    }
}

/// `"name": { "k": v, … }` → [(k, raw v)], for flat objects whose values are
/// numbers or `[numbers]`. Keys may be any JSON string (escapes `\"` `\\` handled).
fn object_entries(src: &str, name: &str) -> Result<Vec<(String, String)>, String> {
    let at = src.find(&format!("\"{name}\"")).ok_or(format!("{name} missing"))?;
    let open = at + src[at..].find('{').ok_or(format!("{name}: no object"))?;
    let body: Vec<char> = src[open + 1..].chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    loop {
        while i < body.len() && (body[i].is_whitespace() || body[i] == ',') {
            i += 1;
        }
        if i >= body.len() || body[i] == '}' {
            break;
        }
        if body[i] != '"' {
            return Err(format!("{name}: expected a key"));
        }
        i += 1;
        let mut key = String::new();
        while i < body.len() && body[i] != '"' {
            if body[i] == '\\' && i + 1 < body.len() {
                i += 1;
                key.push(match body[i] {
                    'n' => '\n',
                    't' => '\t',
                    'u' if i + 4 < body.len() => {
                        let hex: String = body[i + 1..i + 5].iter().collect();
                        i += 4;
                        u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32).unwrap_or('\u{fffd}')
                    }
                    c => c,
                });
            } else {
                key.push(body[i]);
            }
            i += 1;
        }
        i += 1;
        while i < body.len() && (body[i].is_whitespace() || body[i] == ':') {
            i += 1;
        }
        let start = i;
        let mut depth = 0;
        while i < body.len() {
            match body[i] {
                '[' => depth += 1,
                ']' => depth -= 1,
                ',' | '}' if depth == 0 => break,
                _ => {}
            }
            i += 1;
        }
        out.push((key, body[start..i].iter().collect::<String>().trim().to_string()));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_piper_config() {
        let cfg = VoiceConfig::parse(
            r#"{"audio": {"sample_rate": 22050}, "inference": {"noise_scale": 0.667, "length_scale": 1, "noise_w": 0.8},
            "phoneme_id_map": {"_": [0], "^": [1], "$": [2], " ": [3], "\"": [150], "ɑ": [51], "ˈ": [120]},
            "num_speakers": 2, "speaker_id_map": {"F1": 1, "M1": 0}}"#,
        )
        .unwrap();
        assert_eq!(cfg.sample_rate, 22050);
        assert_eq!(cfg.speakers, vec!["M1", "F1"]);
        assert_eq!(cfg.phoneme_ids[&'"'], 150);
        assert_eq!(cfg.ids("ˈɑ ɑ"), vec![1, 0, 120, 0, 51, 0, 3, 0, 51, 0, 2]);
    }
}
