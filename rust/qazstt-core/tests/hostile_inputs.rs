//! Untrusted input must never panic: the server is built with `panic = "abort"`,
//! so one bad request would take every user's session down with it. These
//! tests push random and mutated input through every public entry point that
//! sees request data. A tiny xorshift keeps them dependency-free and repeatable.

use qazstt_core::tts::text;
use qazstt_core::{audio, Engine, EngineConfig, MockBackend};

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

/// Characters a request could plausibly (or maliciously) contain.
const POOL: &[&str] = &[
    "а", "ә", "Қ", "ң", "ө", "ұ", "ү", "һ", "і", "ё", "Ъ", "a", "Z", "q", "x", "w", "ç", "ş", "ğ", "ı", "0", "1", "5", "9",
    " ", "  ", "\t", "\n", "\r", ".", ",", "!", "?", ";", ":", "-", "—", "–", "…", "%", "$", "€", "₸", "№", "/", "+", "=",
    "(", ")", "\"", "'", "«", "»", "#", "@", "&", "*", "1.5", "3,14", "12:30", "2026-11-11", "1000000000000000000000",
    "18446744073709551616", "XIV", "км", "кг", "т.б.", "ж.", "млн", "\u{0301}", "\u{200b}", "\u{feff}", "😀", "👍🏽",
    "中", "ع", "\u{0}", "\u{7f}", "\u{10ffff}", "ﬁ", "Ⅻ", "²", "½", "٣",
];

fn random_text(r: &mut Rng, max_pieces: usize) -> String {
    (0..r.below(max_pieces) + 1).map(|_| POOL[r.below(POOL.len())]).collect()
}

#[test]
fn text_front_end_never_panics() {
    let mut r = Rng(0x9e37_79b9_7f4a_7c15);
    for i in 0..20_000 {
        let s = random_text(&mut r, if i % 100 == 0 { 2_000 } else { 40 });
        let n = text::normalize(&s);
        for piece in text::sentences(&n, 200).iter().chain(text::sentences_fast_start(&n, 80, 200).iter()) {
            assert!(piece.chars().count() <= 300, "piece too long: {} chars", piece.chars().count());
            let _ = text::phonemize(piece);
        }
        let _ = text::text_to_phonemes(&s);
    }
}

#[test]
fn huge_numbers_and_long_words_are_bounded() {
    for s in ["9".repeat(5_000), "а".repeat(5_000), "1,".repeat(2_500), "-".repeat(5_000), ".".repeat(5_000)] {
        let n = text::normalize(&s);
        for piece in text::sentences(&n, 200) {
            assert!(piece.chars().count() <= 300, "piece too long: {} chars", piece.chars().count());
            let _ = text::phonemize(&piece);
        }
    }
}

fn wav(tag: u16, channels: u16, rate: u32, bits: u16, data: &[u8]) -> Vec<u8> {
    let mut v = Vec::new();
    v.extend_from_slice(b"RIFF");
    v.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
    v.extend_from_slice(b"WAVEfmt ");
    v.extend_from_slice(&16u32.to_le_bytes());
    v.extend_from_slice(&tag.to_le_bytes());
    v.extend_from_slice(&channels.to_le_bytes());
    v.extend_from_slice(&rate.to_le_bytes());
    v.extend_from_slice(&(rate.wrapping_mul((channels as u32).wrapping_mul(bits as u32) / 8)).to_le_bytes());
    v.extend_from_slice(&(channels.wrapping_mul(bits) / 8).to_le_bytes());
    v.extend_from_slice(&bits.to_le_bytes());
    v.extend_from_slice(b"data");
    v.extend_from_slice(&(data.len() as u32).to_le_bytes());
    v.extend_from_slice(data);
    v
}

#[test]
fn wav_decoder_survives_mutation() {
    let mut r = Rng(42);
    let good = wav(1, 1, 16_000, 16, &vec![7u8; 3_200]);
    for _ in 0..20_000 {
        let mut b = good.clone();
        for _ in 0..r.below(8) + 1 {
            let i = r.below(b.len());
            b[i] = r.next() as u8;
        }
        b.truncate(r.below(b.len() + 1).max(r.below(b.len() + 1)));
        let _ = audio::decode_wav(&b);
    }
}

/// Every format the decoder accepts must also be safe to hand to the engine:
/// odd rates and channel counts, and float data that is not finite.
#[test]
fn engine_accepts_whatever_the_decoder_accepts() {
    let mut nan = Vec::new();
    for v in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, 1e30, -1e30, 0.5] {
        for _ in 0..800 {
            nan.extend_from_slice(&v.to_le_bytes());
        }
    }
    let cases = [
        wav(3, 1, 16_000, 32, &nan),
        wav(1, 1, 8_000, 8, &[200u8; 8_000]),
        wav(1, 2, 44_100, 24, &[1u8; 44_100 * 6 / 10]),
        wav(1, 7, 22_050, 16, &[3u8; 22_050 * 14 / 10]),
    ];
    for b in cases {
        let Ok(pcm) = audio::decode_wav(&b) else { continue };
        let mut e = Engine::with_backend(Box::new(MockBackend { text: "сәлем".into() }), EngineConfig::new(std::path::PathBuf::from("mock")));
        let _ = e.feed(&pcm.samples, pcm.sample_rate, pcm.channels);
        let _ = e.partial();
        let _ = e.finish();
    }
}

/// A WAV header can claim any sample rate. Rates the service would never see
/// from a real recorder must be refused by the decoder rather than resampled,
/// because resampling from 1 Hz to 16 kHz multiplies the data 16 000 times.
#[test]
fn absurd_sample_rates_are_refused() {
    for rate in [1u32, 100, 1_000, 1_000_000, u32::MAX] {
        let b = wav(1, 1, rate, 16, &[0u8; 1_000]);
        assert!(audio::decode_wav(&b).is_err(), "rate {rate} accepted");
    }
    for ch in [0u16, 9, 255, u16::MAX] {
        let b = wav(1, ch, 16_000, 16, &[0u8; 1_000]);
        assert!(audio::decode_wav(&b).is_err(), "{ch} channels accepted");
    }
}
