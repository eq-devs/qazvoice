//! Punctuation + true-casing restoration.
//!
//! Model: `1-800-BAD-CODE/xlm-roberta_punctuation_fullstop_truecase` (Apache-2.0),
//! an XLM-R encoder with per-subtoken heads that covers 47 languages including
//! Kazakh and Russian. A model directory holds `model.onnx`, `sp.model`
//! (SentencePiece unigram) and `config.yaml` (label lists).
//!
//! FastConformer emits lowercase text without punctuation; this adds `. , ?` and
//! capitals. On FLEURS kk references: `.` F1 0.88, `,` F1 0.74, capitalization
//! 98% of words (int8 model, scripts/punct_eval.py).
//!
//! The SentencePiece tokenizer is implemented here (unigram Viterbi over the
//! pieces in `sp.model`) so no C++ library is needed on any target. Normalization
//! is whitespace-only; the full `nmt_nfkc` map is an identity on ASR output
//! (lowercase letters, digits, spaces); ids match Python SentencePiece on all 471
//! FLEURS reference and ASR lines tried (`examples/punct.rs --ids`).

use crate::backend::BackendError;
use ort::session::Session;
use ort::value::Tensor;
use std::collections::HashMap;
use std::path::Path;

const SPACE: char = '▁';

/// SentencePiece unigram model, parsed straight from the protobuf.
pub struct SentencePiece {
    pieces: HashMap<String, (u32, f32)>,
    id_to_piece: Vec<String>,
    max_piece_chars: usize,
    unk_id: u32,
    unk_score: f32,
    pub bos_id: u32,
    pub eos_id: u32,
}

impl SentencePiece {
    pub fn load(path: &Path) -> Result<Self, BackendError> {
        let bytes = std::fs::read(path).map_err(|e| BackendError::ModelLoad(format!("{}: {e}", path.display())))?;
        Self::from_bytes(&bytes).map_err(|e| BackendError::ModelLoad(format!("{}: {e}", path.display())))
    }

    pub fn from_bytes(buf: &[u8]) -> Result<Self, String> {
        let mut pieces = HashMap::new();
        let mut id_to_piece = Vec::new();
        let (mut unk_id, mut bos_id, mut eos_id) = (0, 1, 2);
        let mut min_score = 0f32;
        let mut max_piece_chars = 1;
        let mut r = Reader { buf, pos: 0 };
        while let Some((field, wire)) = r.key()? {
            if field == 1 && wire == 2 {
                let msg = r.bytes()?;
                let (piece, score, kind) = parse_piece(msg)?;
                let id = id_to_piece.len() as u32;
                match kind {
                    2 => unk_id = id,
                    3 => match piece.as_str() {
                        "<s>" => bos_id = id,
                        "</s>" => eos_id = id,
                        _ => {}
                    },
                    _ => {
                        min_score = min_score.min(score);
                        max_piece_chars = max_piece_chars.max(piece.chars().count());
                        pieces.insert(piece.clone(), (id, score));
                    }
                }
                id_to_piece.push(piece);
            } else {
                r.skip(wire)?;
            }
        }
        if id_to_piece.is_empty() {
            return Err("no pieces".into());
        }
        Ok(Self { pieces, id_to_piece, max_piece_chars, unk_id, unk_score: min_score - 10.0, bos_id, eos_id })
    }

    pub fn piece(&self, id: u32) -> &str {
        self.id_to_piece.get(id as usize).map(String::as_str).unwrap_or("")
    }

    /// Encode text to piece ids (no BOS/EOS), best-scoring segmentation.
    pub fn encode(&self, text: &str) -> Vec<u32> {
        if text.trim().is_empty() {
            return Vec::new();
        }
        let norm: String = std::iter::once(SPACE)
            .chain(text.split_whitespace().collect::<Vec<_>>().join(" ").chars().map(|c| if c == ' ' { SPACE } else { c }))
            .collect();
        let chars: Vec<(usize, char)> = norm.char_indices().collect();
        let n = chars.len();
        let byte_at = |i: usize| if i == n { norm.len() } else { chars[i].0 };
        // best[i] = (score, prev, id) for the prefix of i chars.
        let mut best: Vec<(f32, usize, u32)> = vec![(f32::NEG_INFINITY, 0, 0); n + 1];
        best[0].0 = 0.0;
        for i in 0..n {
            let base = best[i].0;
            if base == f32::NEG_INFINITY {
                continue;
            }
            let mut matched_single = false;
            for len in 1..=self.max_piece_chars.min(n - i) {
                if let Some(&(id, score)) = self.pieces.get(&norm[byte_at(i)..byte_at(i + len)]) {
                    matched_single |= len == 1;
                    if base + score > best[i + len].0 {
                        best[i + len] = (base + score, i, id);
                    }
                }
            }
            if !matched_single && base + self.unk_score > best[i + 1].0 {
                best[i + 1] = (base + self.unk_score, i, self.unk_id);
            }
        }
        let mut ids = Vec::new();
        let mut i = n;
        while i > 0 {
            let (_, prev, id) = best[i];
            ids.push(id);
            i = prev;
        }
        ids.reverse();
        ids
    }
}

fn parse_piece(buf: &[u8]) -> Result<(String, f32, u64), String> {
    let (mut piece, mut score, mut kind) = (String::new(), 0f32, 1u64);
    let mut r = Reader { buf, pos: 0 };
    while let Some((field, wire)) = r.key()? {
        match (field, wire) {
            (1, 2) => piece = String::from_utf8(r.bytes()?.to_vec()).map_err(|e| e.to_string())?,
            (2, 5) => score = f32::from_le_bytes(r.fixed::<4>()?),
            (3, 0) => kind = r.varint()?,
            _ => r.skip(wire)?,
        }
    }
    Ok((piece, score, kind))
}

/// Minimal protobuf wire reader.
struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn varint(&mut self) -> Result<u64, String> {
        let mut v = 0u64;
        for shift in (0..64).step_by(7) {
            let b = *self.buf.get(self.pos).ok_or("truncated varint")?;
            self.pos += 1;
            v |= u64::from(b & 0x7f) << shift;
            if b & 0x80 == 0 {
                return Ok(v);
            }
        }
        Err("varint too long".into())
    }
    fn key(&mut self) -> Result<Option<(u64, u64)>, String> {
        if self.pos >= self.buf.len() {
            return Ok(None);
        }
        let k = self.varint()?;
        Ok(Some((k >> 3, k & 7)))
    }
    fn bytes(&mut self) -> Result<&'a [u8], String> {
        let len = self.varint()? as usize;
        let s = self.buf.get(self.pos..self.pos + len).ok_or("truncated bytes")?;
        self.pos += len;
        Ok(s)
    }
    fn fixed<const N: usize>(&mut self) -> Result<[u8; N], String> {
        let s = self.buf.get(self.pos..self.pos + N).ok_or("truncated fixed")?;
        self.pos += N;
        Ok(s.try_into().unwrap())
    }
    fn skip(&mut self, wire: u64) -> Result<(), String> {
        match wire {
            0 => self.varint().map(|_| ()),
            1 => self.fixed::<8>().map(|_| ()),
            2 => self.bytes().map(|_| ()),
            5 => self.fixed::<4>().map(|_| ()),
            w => Err(format!("unsupported wire type {w}")),
        }
    }
}

/// `post_labels:` from `config.yaml`. The file is small and flat, one quoted label
/// per line, so it is read without a YAML parser.
fn read_post_labels(path: &Path) -> Result<Vec<String>, BackendError> {
    let txt = std::fs::read_to_string(path).map_err(|e| BackendError::ModelLoad(format!("{}: {e}", path.display())))?;
    let start = txt.find("post_labels:").ok_or_else(|| BackendError::ModelLoad("config.yaml: no post_labels".into()))?;
    let body = &txt[start..];
    let body = &body[..body.find(']').unwrap_or(body.len())];
    let labels: Vec<String> = body
        .lines()
        .skip(1)
        .map(|l| l.trim().trim_end_matches(',').trim())
        .filter(|t| t.len() >= 2 && t.starts_with('"') && t.ends_with('"'))
        .map(|t| t[1..t.len() - 1].to_string())
        .collect();
    if labels.is_empty() {
        return Err(BackendError::ModelLoad("config.yaml: empty post_labels".into()));
    }
    Ok(labels)
}

/// Punctuation + true-casing model.
pub struct Punctuator {
    session: Session,
    sp: SentencePiece,
    post_labels: Vec<String>,
    max_len: usize,
}

impl Punctuator {
    /// `dir` holds `model.onnx`, `sp.model`, `config.yaml`.
    pub fn load(dir: &Path, threads: usize) -> Result<Self, BackendError> {
        let onnx = dir.join("model.onnx");
        let session = crate::backend::fastconformer::cpu_session(&onnx, threads)?;
        Ok(Self { session, sp: SentencePiece::load(&dir.join("sp.model"))?, post_labels: read_post_labels(&dir.join("config.yaml"))?, max_len: 256 })
    }

    /// Restore punctuation and capitals. Existing capitals are kept (hotword casing).
    pub fn apply(&mut self, text: &str) -> Result<String, BackendError> {
        let ids = self.sp.encode(text);
        if ids.is_empty() {
            return Ok(String::new());
        }
        let mut out = String::with_capacity(text.len() + 16);
        for chunk in ids.chunks(self.max_len - 2) {
            self.apply_chunk(chunk, &mut out)?;
        }
        Ok(out)
    }

    fn apply_chunk(&mut self, chunk: &[u32], out: &mut String) -> Result<(), BackendError> {
        let inf = |e: ort::Error| BackendError::Inference(e.to_string());
        let mut ids = Vec::with_capacity(chunk.len() + 2);
        ids.push(self.sp.bos_id as i64);
        ids.extend(chunk.iter().map(|&i| i as i64));
        ids.push(self.sp.eos_id as i64);
        let t = ids.len();
        let input = Tensor::from_array(([1usize, t], ids.clone())).map_err(inf)?;
        let inputs = ort::inputs!["input_ids" => input];
        let outputs = match crate::backend::fastconformer::run_options() {
            Some(o) => self.session.run_with_options(inputs, o),
            None => self.session.run(inputs),
        }
        .map_err(inf)?;
        let (_, post) = outputs["post_preds"].try_extract_tensor::<i64>().map_err(inf)?;
        let (cap_shape, cap) = outputs["cap_preds"].try_extract_tensor::<bool>().map_err(inf)?;
        let cap_w = cap_shape[2] as usize;
        for i in 1..t - 1 {
            let piece = self.sp.piece(ids[i] as u32);
            let mut chars = piece.chars().peekable();
            if chars.peek() == Some(&SPACE) {
                chars.next();
                if !out.is_empty() {
                    out.push(' ');
                }
            }
            let skip = usize::from(piece.starts_with(SPACE));
            for (j, ch) in chars.enumerate() {
                if j + skip < cap_w && cap[i * cap_w + j + skip] {
                    out.extend(ch.to_uppercase());
                } else {
                    out.push(ch);
                }
            }
            // A bare "▁" piece carries no text; punctuation predicted on it would dangle.
            if piece == SPACE.to_string() {
                continue;
            }
            match self.post_labels.get(post[i] as usize).map(String::as_str) {
                None | Some("<NULL>") | Some("<ACRONYM>") => {}
                Some(p) => out.push_str(p),
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../models/punct-kk")
    }

    /// Expected ids from Python `SentencePieceProcessor.EncodeAsIds` (sp.model of the model above).
    #[test]
    fn sentencepiece_matches_python() {
        let p = dir().join("sp.model");
        if !p.exists() {
            eprintln!("skip: {} missing", p.display());
            return;
        }
        let sp = SentencePiece::load(&p).unwrap();
        let ids = sp.encode("ақш тың гимнастика 10 000 жыл");
        let pieces: Vec<&str> = ids.iter().map(|&i| sp.piece(i)).collect();
        assert_eq!(pieces, ["▁ақ", "ш", "▁", "тың", "▁гимнастик", "а", "▁10", "▁000", "▁жыл"]);
    }

    #[test]
    fn restores_punctuation() {
        if !dir().join("model.onnx").exists() {
            eprintln!("skip: punct model missing");
            return;
        }
        let mut p = Punctuator::load(&dir(), 2).unwrap();
        let out = p.apply("ешкілер алғаш рет қол үйретілген уақыт иранның загрос тауларында шамамен 10 000 жыл бұрын деп саналады").unwrap();
        assert_eq!(out, "Ешкілер алғаш рет қол үйретілген уақыт Иранның Загрос тауларында шамамен 10 000 жыл бұрын деп саналады.");
    }
}
