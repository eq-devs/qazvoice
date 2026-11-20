//! Text helpers that need no model: Latin → Cyrillic for whole texts, digits →
//! words, sentence spans (with UTF-16 offsets for Dart) and the TTS plan.

use qazstt_core::tts::text as ttext;

/// Letters of the Latin script (ASCII + Latin-1 supplement + Latin Extended-A/B),
/// which covers the 2021 Kazakh alphabet (ä ğ ı i̇ ñ ö ş ū ü).
fn is_latin_letter(c: char) -> bool {
    c.is_ascii_alphabetic() || (('\u{00C0}'..='\u{024F}').contains(&c) && c.is_alphabetic() && c != '×' && c != '÷')
}

/// Latin-script words → Kazakh Cyrillic; everything else is kept as is.
/// Capitalization of each word is restored (Title or ALL CAPS).
pub fn to_cyrillic(text: &str) -> String {
    let mut out = String::with_capacity(text.len() * 2);
    let mut word = String::new();
    let flush = |word: &mut String, out: &mut String| {
        if word.is_empty() {
            return;
        }
        let cyr = ttext::latin_to_cyrillic(word);
        let letters: Vec<char> = word.chars().filter(|c| c.is_alphabetic()).collect();
        let all_caps = letters.len() > 1 && letters.iter().all(|c| c.is_uppercase());
        let title = letters.first().is_some_and(|c| c.is_uppercase());
        if all_caps {
            out.push_str(&cyr.to_uppercase());
        } else if title {
            let mut cs = cyr.chars();
            if let Some(f) = cs.next() {
                out.extend(f.to_uppercase());
                out.push_str(cs.as_str());
            }
        } else {
            out.push_str(&cyr);
        }
        word.clear();
    };
    for c in text.chars() {
        // U+0307 (combining dot, from "İ".to_lowercase()) stays inside the word.
        if is_latin_letter(c) || (c == '\u{0307}' && !word.is_empty()) {
            word.push(c);
        } else {
            flush(&mut word, &mut out);
            out.push(c);
        }
    }
    flush(&mut word, &mut out);
    out
}

/// Every run of digits → Kazakh number words ("25 адам" → "жиырма бес адам").
/// Runs too long for a number are read digit by digit.
pub fn spell_numbers(text: &str) -> String {
    let mut out = String::with_capacity(text.len() * 3);
    let mut digits = String::new();
    let flush = |digits: &mut String, out: &mut String| {
        if digits.is_empty() {
            return;
        }
        // Up to 15 digits is a number people say; longer is an id / phone number.
        let words = match digits.parse::<u64>() {
            Ok(n) if digits.len() <= 15 => ttext::cardinal(n),
            _ => digits.chars().map(|d| ttext::cardinal(d.to_digit(10).unwrap_or(0) as u64)).collect::<Vec<_>>().join(" "),
        };
        out.push_str(&words);
        digits.clear();
    };
    for c in text.chars() {
        if c.is_ascii_digit() {
            digits.push(c);
        } else {
            flush(&mut digits, &mut out);
            out.push(c);
        }
    }
    flush(&mut digits, &mut out);
    out
}

fn is_punct(c: char) -> bool {
    matches!(c, '.' | ',' | '!' | '?' | ';' | ':' | '…')
}

/// The punctuation model adds marks after tokens that already had one
/// ("т.б." → "т.. б..."). Runs of marks that do not occur in [input] are
/// cut to their first mark.
pub fn tidy_punctuation(input: &str, output: &str) -> String {
    let mut out = String::with_capacity(output.len());
    let chars: Vec<char> = output.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if is_punct(chars[i]) {
            let mut j = i;
            while j < chars.len() && is_punct(chars[j]) {
                j += 1;
            }
            let run: String = chars[i..j].iter().collect();
            if j - i > 1 && !input.contains(&run) {
                out.push(chars[i]);
            } else {
                out.push_str(&run);
            }
            i = j;
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

#[derive(Debug, Clone, PartialEq)]
pub struct Span {
    pub byte_start: usize,
    pub byte_end: usize,
    pub u16_start: usize,
    pub u16_end: usize,
}

fn is_terminal(c: char) -> bool {
    matches!(c, '.' | '!' | '?' | '…' | '。' | '！' | '？')
}

fn is_closing(c: char) -> bool {
    matches!(c, '"' | '\'' | '»' | '”' | '’' | ')' | ']')
}

/// Sentences of `text` with byte and UTF-16 offsets, whitespace trimmed.
///
/// A sentence ends at `.` `!` `?` `…` (plus closing quotes/brackets) followed by
/// whitespace and a character that can start a sentence (not a lowercase letter,
/// so "т.б. және" stays one sentence), or at a blank line.
pub fn sentence_spans(text: &str) -> Vec<Span> {
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let mut cuts: Vec<usize> = Vec::new(); // byte offsets where a sentence ends
    let mut i = 0;
    while i < chars.len() {
        let (_, c) = chars[i];
        if is_terminal(c) {
            let mut j = i + 1;
            while j < chars.len() && (is_terminal(chars[j].1) || is_closing(chars[j].1)) {
                j += 1;
            }
            let end = chars.get(j).map(|x| x.0).unwrap_or(text.len());
            let mut k = j;
            while k < chars.len() && chars[k].1.is_whitespace() {
                k += 1;
            }
            let ws = k > j;
            let next_ok = chars.get(k).is_none_or(|(_, n)| !n.is_lowercase());
            if j >= chars.len() || (ws && next_ok) {
                cuts.push(end);
            }
            i = j;
            continue;
        }
        if c == '\n' {
            // Blank line (paragraph break) ends a sentence even without punctuation.
            let mut k = i + 1;
            while k < chars.len() && chars[k].1 != '\n' && chars[k].1.is_whitespace() {
                k += 1;
            }
            if k < chars.len() && chars[k].1 == '\n' {
                cuts.push(chars[i].0);
            }
        }
        i += 1;
    }
    cuts.push(text.len());
    let mut spans = Vec::new();
    let mut start = 0;
    for cut in cuts {
        if cut <= start {
            continue;
        }
        let piece = &text[start..cut];
        let lead = piece.len() - piece.trim_start().len();
        let trail = piece.len() - piece.trim_end().len();
        let (bs, be) = (start + lead, cut - trail);
        if be > bs {
            spans.push(Span { byte_start: bs, byte_end: be, u16_start: u16_len(&text[..bs]), u16_end: u16_len(&text[..be]) });
        }
        start = cut;
    }
    spans
}

fn u16_len(s: &str) -> usize {
    s.chars().map(char::len_utf16).sum()
}

#[derive(Debug, Clone)]
pub struct PlanItem {
    /// Speakable text for one synthesis call.
    pub piece: String,
    /// The source sentence as written.
    pub sentence: String,
    pub u16_start: usize,
    pub u16_end: usize,
    /// A sentence pause follows this piece.
    pub pause: bool,
}

/// Written text → synthesis pieces, each tied to the span of its source sentence.
/// The first sentence may be cut at a comma so playback starts sooner.
pub fn tts_plan(text: &str) -> Vec<PlanItem> {
    let mut plan = Vec::new();
    for sp in sentence_spans(text) {
        let sentence = &text[sp.byte_start..sp.byte_end];
        let norm = ttext::normalize(sentence);
        let pieces = if plan.is_empty() { ttext::sentences_fast_start(&norm, 80, 200) } else { ttext::sentences(&norm, 200) };
        let pieces: Vec<String> = pieces.into_iter().filter(|p| !p.trim().is_empty()).collect();
        let n = pieces.len();
        for (k, piece) in pieces.into_iter().enumerate() {
            let pause = k + 1 == n || piece.trim_end().ends_with(['.', '!', '?', '…']);
            plan.push(PlanItem { piece, sentence: sentence.to_string(), u16_start: sp.u16_start, u16_end: sp.u16_end, pause });
        }
    }
    plan
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cyrillic_keeps_case_and_punctuation() {
        assert_eq!(to_cyrillic("Sälem, älem!"), "Сәлем, әлем!");
        assert_eq!(to_cyrillic("QAZAQSTAN 2024"), "ҚАЗАҚСТАН 2024");
        assert_eq!(to_cyrillic("Уже кириллица"), "Уже кириллица");
    }

    #[test]
    fn doubled_marks_are_tidied() {
        assert_eq!(tidy_punctuation("т.б.", "Т.. б..."), "Т. б.");
        assert_eq!(tidy_punctuation("күт...", "Күт..."), "Күт...");
        assert_eq!(tidy_punctuation("сәлем", "Сәлем, әлем."), "Сәлем, әлем.");
    }

    #[test]
    fn numbers_spelled() {
        assert_eq!(spell_numbers("25 адам"), "жиырма бес адам");
        assert_eq!(spell_numbers("жоқ"), "жоқ");
        assert!(spell_numbers("12345678901234567890").starts_with("бір екі"));
    }

    #[test]
    fn sentences_split_with_offsets() {
        let t = "Сәлем! Қалың қалай? т.б. және т.с.с. Бітті";
        let s = sentence_spans(t);
        let texts: Vec<&str> = s.iter().map(|x| &t[x.byte_start..x.byte_end]).collect();
        assert_eq!(texts, vec!["Сәлем!", "Қалың қалай? т.б. және т.с.с.", "Бітті"]);
        assert_eq!(s[1].u16_start, 7);
    }

    #[test]
    fn utf16_offsets_count_surrogates() {
        let t = "😀 Бір. Екі.";
        let s = sentence_spans(t);
        assert_eq!(s.len(), 2);
        assert_eq!(s[0].u16_start, 0);
        assert_eq!(s[1].u16_start, 8); // 😀 is 2 UTF-16 units
    }

    #[test]
    fn paragraphs_split() {
        let s = sentence_spans("Тақырып\n\nМәтін");
        assert_eq!(s.len(), 2);
    }

    #[test]
    fn plan_ties_pieces_to_sentences() {
        let p = tts_plan("Бүгін ауа райы өте жақсы болады, сондықтан біз қалалық саябаққа барып, достарымызбен бірге серуендеп, кешке дейін демаламыз. Сау бол!");
        assert!(p.len() >= 3);
        assert!(!p[0].pause);
        assert_eq!(p[0].u16_start, p[1].u16_start);
        assert!(p.last().unwrap().pause);
        assert_eq!(p.last().unwrap().sentence, "Сау бол!");
        assert!(tts_plan("   ").is_empty());
    }
}
