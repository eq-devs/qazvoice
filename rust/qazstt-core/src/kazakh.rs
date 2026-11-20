//! Kazakh-specific text post-processing.
//!
//! Everything a Kazakh-only engine needs after the acoustic model has spoken:
//! - clean-up of ASR output (spacing, punctuation, stray Latin lookalikes)
//! - optional Cyrillic → Latin transliteration (2021 state standard)
//! - hotword snapping (fix names / terms the model almost got)
//! - optional spoken-number → digit conversion
//!
//! All functions are pure and allocation-light; call them from any thread.

use std::collections::HashMap;

/// Kazakh Cyrillic letters (lowercase). Used to decide whether a token is "a Kazakh word".
pub const KAZAKH_LOWER: &str = "аәбвгғдеёжзийкқлмнңоөпрстуұүфхһцчшщъыіьэюя";

/// Latin characters that commonly leak into Cyrillic output (OCR-style confusions,
/// also produced by bilingual kk-ru models on code-switched speech). Mapped to the
/// Cyrillic glyph they look like. Applied only inside otherwise-Cyrillic tokens.
fn latin_lookalike(c: char) -> Option<char> {
    Some(match c {
        'a' => 'а',
        'e' => 'е',
        'o' => 'о',
        'p' => 'р',
        'c' => 'с',
        'x' => 'х',
        'y' => 'у',
        'k' => 'к',
        'h' => 'һ',
        'i' => 'і',
        'A' => 'А',
        'E' => 'Е',
        'O' => 'О',
        'P' => 'Р',
        'C' => 'С',
        'X' => 'Х',
        'Y' => 'У',
        'K' => 'К',
        'H' => 'Һ',
        'I' => 'І',
        'B' => 'В',
        'M' => 'М',
        'T' => 'Т',
        _ => return None,
    })
}

fn is_cyrillic(c: char) -> bool {
    ('\u{0400}'..='\u{04FF}').contains(&c)
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '-' || c == '\'' || c == '’'
}

/// Undoes a quirk of the NVIDIA kk-ru tokenizer: its training text had "б"
/// expanded to "басқа" and "т.б." to "тағы басқа" inside words, so the model
/// sometimes writes "тағы басқаиғи" for "табиғи" or "басқаасшысы" for
/// "басшысы". Neither sequence occurs in Kazakh ("тағы басқа" is always
/// followed by a space; "басқа" is never followed directly by a vowel).
pub fn fix_tokenizer_artifacts(text: &str) -> String {
    if !text.contains("басқа") {
        return text.to_string();
    }
    const VOWELS: &str = "аәеёиоөұүыіэюя";
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(i) = rest.find("басқа") {
        let after = &rest[i + "басқа".len()..];
        let next = after.chars().next();
        let before = &rest[..i];
        if before.ends_with("тағы ") && next.is_some_and(|c| c.is_alphabetic()) {
            // "тағы басқаиғи" → "табиғи"
            out.push_str(&before[..before.len() - "тағы ".len()]);
            out.push_str("таб");
        } else if next.is_some_and(|c| VOWELS.contains(c.to_lowercase().next().unwrap_or(c))) {
            // "басқаасшысы" → "басшысы", "басқаол" → "бол"
            out.push_str(before);
            out.push('б');
        } else {
            out.push_str(before);
            out.push_str("басқа");
        }
        rest = after;
    }
    out.push_str(rest);
    out
}

/// ASR output clean-up: collapse whitespace, fix punctuation spacing, repair
/// Latin lookalikes inside Cyrillic words, strip bracketed non-speech tags
/// like `[музыка]` / `(аплодисменттер)`.
pub fn clean(text: &str) -> String {
    let mut s = strip_bracketed(text);

    // Repair mixed-script tokens.
    s = s
        .split_whitespace()
        .map(repair_token)
        .collect::<Vec<_>>()
        .join(" ");

    // Punctuation spacing: no space before , . ! ? ; : — but one after.
    let mut out = String::with_capacity(s.len());
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match c {
            ',' | '.' | '!' | '?' | ';' | ':' => {
                while out.ends_with(' ') {
                    out.pop();
                }
                out.push(c);
                // Collapse runs like "!!!" or "..." into one (keep "…" style as single char).
                let mut j = i + 1;
                while j < chars.len() && chars[j] == c {
                    j += 1;
                }
                i = j;
                if i < chars.len() && chars[i] != ' ' && is_word_char(chars[i]) {
                    out.push(' ');
                }
                continue;
            }
            ' ' => {
                if !out.ends_with(' ') && !out.is_empty() {
                    out.push(' ');
                }
            }
            _ => out.push(c),
        }
        i += 1;
    }
    out.trim().to_string()
}

fn strip_bracketed(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut depth_sq = 0i32;
    let mut depth_par = 0i32;
    for c in text.chars() {
        match c {
            '[' => depth_sq += 1,
            ']' => depth_sq = (depth_sq - 1).max(0),
            '(' => depth_par += 1,
            ')' => depth_par = (depth_par - 1).max(0),
            _ => {
                if depth_sq == 0 && depth_par == 0 {
                    out.push(c);
                }
            }
        }
    }
    out
}

fn repair_token(tok: &str) -> String {
    let n_cyr = tok.chars().filter(|c| is_cyrillic(*c)).count();
    let n_lat = tok.chars().filter(|c| c.is_ascii_alphabetic()).count();
    if n_cyr == 0 || n_lat == 0 {
        return tok.to_string();
    }
    // Majority Cyrillic → repair latin lookalikes; otherwise leave (likely a real Latin word).
    if n_cyr >= n_lat {
        tok.chars()
            .map(|c| latin_lookalike(c).unwrap_or(c))
            .collect()
    } else {
        tok.to_string()
    }
}

/// Cyrillic → Latin, Kazakh 2021 standard alphabet
/// (Ä Ğ Ñ Ö Ş Ü Ū; И/Й → I ı; І → İ i; Ы → Y y).
pub fn to_latin(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 8);
    let chars: Vec<char> = text.chars().collect();
    for (idx, &c) in chars.iter().enumerate() {
        // Capitalization rule for multi-char outputs: uppercase if next char is
        // also uppercase (word in caps) else title-case.
        let next_upper = chars
            .get(idx + 1)
            .map(|n| n.is_uppercase())
            .unwrap_or(false);
        let push_multi = |out: &mut String, lower: &str, upper_first: bool| {
            if !upper_first {
                out.push_str(lower);
            } else if next_upper {
                out.push_str(&lower.to_uppercase());
            } else {
                let mut it = lower.chars();
                if let Some(f) = it.next() {
                    out.extend(f.to_uppercase());
                    out.push_str(it.as_str());
                }
            }
        };
        match c {
            'а' => out.push('a'),
            'ә' => out.push('ä'),
            'б' => out.push('b'),
            'в' => out.push('v'),
            'г' => out.push('g'),
            'ғ' => out.push('ğ'),
            'д' => out.push('d'),
            'е' => out.push('e'),
            'ё' => out.push_str("ıo"),
            'ж' => out.push('j'),
            'з' => out.push('z'),
            'и' | 'й' => out.push('ı'),
            'к' => out.push('k'),
            'қ' => out.push('q'),
            'л' => out.push('l'),
            'м' => out.push('m'),
            'н' => out.push('n'),
            'ң' => out.push('ñ'),
            'о' => out.push('o'),
            'ө' => out.push('ö'),
            'п' => out.push('p'),
            'р' => out.push('r'),
            'с' => out.push('s'),
            'т' => out.push('t'),
            'у' => out.push('u'),
            'ұ' => out.push('ū'),
            'ү' => out.push('ü'),
            'ф' => out.push('f'),
            'х' | 'һ' => out.push('h'),
            'ц' => out.push_str("ts"),
            'ч' => out.push('ç'),
            'ш' => out.push('ş'),
            'щ' => out.push_str("şç"),
            'ъ' | 'ь' => {}
            'ы' => out.push('y'),
            'і' => out.push('i'),
            'э' => out.push('e'),
            'ю' => out.push_str("ıu"),
            'я' => out.push_str("ıa"),

            'А' => out.push('A'),
            'Ә' => out.push('Ä'),
            'Б' => out.push('B'),
            'В' => out.push('V'),
            'Г' => out.push('G'),
            'Ғ' => out.push('Ğ'),
            'Д' => out.push('D'),
            'Е' => out.push('E'),
            'Ё' => push_multi(&mut out, "ıo", true),
            'Ж' => out.push('J'),
            'З' => out.push('Z'),
            'И' | 'Й' => out.push('I'),
            'К' => out.push('K'),
            'Қ' => out.push('Q'),
            'Л' => out.push('L'),
            'М' => out.push('M'),
            'Н' => out.push('N'),
            'Ң' => out.push('Ñ'),
            'О' => out.push('O'),
            'Ө' => out.push('Ö'),
            'П' => out.push('P'),
            'Р' => out.push('R'),
            'С' => out.push('S'),
            'Т' => out.push('T'),
            'У' => out.push('U'),
            'Ұ' => out.push('Ū'),
            'Ү' => out.push('Ü'),
            'Ф' => out.push('F'),
            'Х' | 'Һ' => out.push('H'),
            'Ц' => push_multi(&mut out, "ts", true),
            'Ч' => out.push('Ç'),
            'Ш' => out.push('Ş'),
            'Щ' => push_multi(&mut out, "şç", true),
            'Ъ' | 'Ь' => {}
            'Ы' => out.push('Y'),
            'І' => out.push('İ'),
            'Э' => out.push('E'),
            'Ю' => push_multi(&mut out, "ıu", true),
            'Я' => push_multi(&mut out, "ıa", true),
            other => out.push(other),
        }
    }
    out
}

/// Levenshtein distance over chars.
pub fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a.is_empty() {
        return b.len();
    }
    if b.is_empty() {
        return a.len();
    }
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0usize; b.len() + 1];
    for i in 1..=a.len() {
        cur[0] = i;
        for j in 1..=b.len() {
            let cost = if a[i - 1] == b[j - 1] { 0 } else { 1 };
            cur[j] = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

/// Snaps near-misses onto a hotword list (names, places, product terms).
///
/// A token is replaced if its lowercase form is within `max_ratio * len` edits
/// of a hotword and at least as close to that hotword as to any other.
/// Multi-word hotwords are matched against sliding windows of the same length.
pub struct HotwordCorrector {
    // (lowercase tokens, original form)
    entries: Vec<(Vec<String>, String)>,
    max_ratio: f32,
    min_len: usize,
}

impl HotwordCorrector {
    /// `max_ratio` — allowed edits as a fraction of hotword length (0.25 is a sane default).
    /// `min_len` — tokens shorter than this are never snapped (avoids mangling short function words).
    pub fn new<I, S>(hotwords: I, max_ratio: f32, min_len: usize) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut entries = Vec::new();
        for hw in hotwords {
            let hw = hw.as_ref().trim();
            if hw.is_empty() {
                continue;
            }
            let toks: Vec<String> = hw.split_whitespace().map(|t| t.to_lowercase()).collect();
            entries.push((toks, hw.to_string()));
        }
        // Longer phrases first so multi-word matches win over single-word ones.
        entries.sort_by(|a, b| b.0.len().cmp(&a.0.len()));
        Self {
            entries,
            max_ratio,
            min_len,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn apply(&self, text: &str) -> String {
        if self.entries.is_empty() {
            return text.to_string();
        }
        let tokens: Vec<String> = text.split_whitespace().map(|t| t.to_string()).collect();
        let mut out: Vec<String> = Vec::with_capacity(tokens.len());
        let mut i = 0;
        'outer: while i < tokens.len() {
            for (hw_toks, original) in &self.entries {
                let n = hw_toks.len();
                if i + n > tokens.len() {
                    continue;
                }
                let window: Vec<(String, String)> = tokens[i..i + n]
                    .iter()
                    .map(|t| split_punct(t))
                    .collect();
                let core_joined = window
                    .iter()
                    .map(|(w, _)| w.to_lowercase())
                    .collect::<Vec<_>>()
                    .join(" ");
                let hw_joined = hw_toks.join(" ");
                if core_joined.chars().count() < self.min_len {
                    continue;
                }
                let d = edit_distance(&core_joined, &hw_joined);
                let allowed = (hw_joined.chars().count() as f32 * self.max_ratio).floor() as usize;
                if d <= allowed {
                    // Preserve trailing punctuation of the last token.
                    let trailing = &window[n - 1].1;
                    let mut replaced = original.clone();
                    // Preserve capitalization if the source token started uppercase.
                    if window[0].0.chars().next().map(|c| c.is_uppercase()).unwrap_or(false) {
                        replaced = capitalize_first(&replaced);
                    }
                    replaced.push_str(trailing);
                    out.push(replaced);
                    i += n;
                    continue 'outer;
                }
            }
            out.push(tokens[i].clone());
            i += 1;
        }
        out.join(" ")
    }
}

fn split_punct(tok: &str) -> (String, String) {
    let core: String = tok.trim_end_matches(|c: char| !is_word_char(c)).to_string();
    let trailing = tok[core.len()..].to_string();
    (core, trailing)
}

fn capitalize_first(s: &str) -> String {
    let mut it = s.chars();
    match it.next() {
        Some(f) => f.to_uppercase().collect::<String>() + it.as_str(),
        None => String::new(),
    }
}

/// Capitalize the first letter of the text and of every sentence after `.`, `!`, `?`.
/// FastConformer emits lowercase, so this is on by default in the engine.
pub fn capitalize_sentences(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut at_start = true;
    for c in text.chars() {
        if at_start && c.is_alphabetic() {
            out.extend(c.to_uppercase());
            at_start = false;
        } else if at_start && c.is_alphanumeric() {
            out.push(c); // "25 адам": a leading number ends the sentence start
            at_start = false;
        } else {
            out.push(c);
            if matches!(c, '.' | '!' | '?') {
                at_start = true;
            }
        }
    }
    out
}

/// Kazakh cardinal number words.
fn number_lexicon() -> HashMap<&'static str, (u64, NumKind)> {
    use NumKind::*;
    let mut m = HashMap::new();
    for (w, v) in [
        ("нөл", 0),
        ("бір", 1),
        ("екі", 2),
        ("үш", 3),
        ("төрт", 4),
        ("бес", 5),
        ("алты", 6),
        ("жеті", 7),
        ("сегіз", 8),
        ("тоғыз", 9),
        ("он", 10),
        ("жиырма", 20),
        ("отыз", 30),
        ("қырық", 40),
        ("елу", 50),
        ("алпыс", 60),
        ("жетпіс", 70),
        ("сексен", 80),
        ("тоқсан", 90),
    ] {
        m.insert(w, (v, Unit));
    }
    m.insert("жүз", (100, Scale));
    m.insert("мың", (1_000, Scale));
    m.insert("миллион", (1_000_000, Scale));
    m.insert("миллиард", (1_000_000_000, Scale));
    m.insert("триллион", (1_000_000_000_000, Scale));
    m
}

#[derive(Clone, Copy, PartialEq)]
enum NumKind {
    Unit,
    Scale,
}

/// Convert runs of Kazakh number words to digits: "екі жүз отыз бір" → "231".
/// Conservative: a lone "бір" (which also means "one/a") is left alone unless
/// followed by another number word, so "бір адам" stays as-is.
/// Ordinal number word (`бірінші`, `қырқыншы`, `жиырмасыншы`…) → its cardinal entry
/// and the written suffix (`ші`/`шы`, by vowel harmony).
fn ordinal_base(word: &str, lex: &HashMap<&'static str, (u64, NumKind)>) -> Option<((u64, NumKind), &'static str)> {
    for suf in ["ыншы", "інші", "ншы", "нші"] {
        let Some(base) = word.strip_suffix(suf) else { continue };
        let tail = if suf.ends_with('ы') { "шы" } else { "ші" };
        // қырық → қырқыншы drops a vowel; жиырма → жиырмасыншы inserts с.
        let candidates = [base.to_string(), base.strip_suffix('с').unwrap_or(base).to_string(), base.replace("қырқ", "қырық")];
        if let Some(v) = candidates.iter().find_map(|c| lex.get(c.as_str()).copied()) {
            return Some((v, tail));
        }
    }
    None
}

pub fn numbers_to_digits(text: &str) -> String {
    let lex = number_lexicon();
    let tokens: Vec<String> = text.split_whitespace().map(|s| s.to_string()).collect();
    let mut out: Vec<String> = Vec::with_capacity(tokens.len());
    let mut i = 0;
    while i < tokens.len() {
        // Collect a maximal run of number words.
        let mut j = i;
        let mut run: Vec<(u64, NumKind, String)> = Vec::new();
        let mut trailing = String::new();
        let mut ordinal: Option<&str> = None;
        while j < tokens.len() {
            let (core, tr) = split_punct(&tokens[j]);
            let lower = core.to_lowercase();
            match lex.get(lower.as_str()) {
                Some(&(v, k)) => {
                    run.push((v, k, core.clone()));
                    trailing = tr.clone();
                    j += 1;
                    if !tr.is_empty() {
                        break; // punctuation ends the run
                    }
                }
                None => {
                    // An ordinal closes the run: "мың тоғыз жүз қырқыншы" → 1940-шы.
                    if let Some(((v, k), suf)) = ordinal_base(&lower, &lex) {
                        run.push((v, k, core.clone()));
                        trailing = tr.clone();
                        ordinal = Some(suf);
                        j += 1;
                    }
                    break;
                }
            }
        }
        let is_lone_one = run.len() == 1 && run[0].0 == 1;
        // Lone small ordinals read better as words ("бірінші рет", "екінші").
        let lone_small_ordinal = ordinal.is_some() && run.len() == 1 && run[0].0 < 10;
        if !run.is_empty() && !is_lone_one && !lone_small_ordinal {
            if let Some(v) = compose_number(&run) {
                // Years drop the ordinal suffix in writing: "1940 жылы".
                let year_follows = tokens.get(j).map(|t| t.to_lowercase().starts_with("жыл")).unwrap_or(false);
                let suf = match ordinal {
                    Some(s) if !year_follows => format!("-{s}"),
                    _ => String::new(),
                };
                out.push(format!("{v}{suf}{trailing}"));
                i = j;
                continue;
            }
        }
        out.push(tokens[i].clone());
        i += 1;
    }
    out.join(" ")
}

fn compose_number(run: &[(u64, NumKind, String)]) -> Option<u64> {
    // Standard "total + current, multiply current on scale" algorithm.
    let mut total: u64 = 0;
    let mut current: u64 = 0;
    let mut last_scale: u64 = u64::MAX;
    for &(v, kind, _) in run {
        match kind {
            NumKind::Unit => {
                current = current.checked_add(v)?;
            }
            NumKind::Scale => {
                let base = if current == 0 { 1 } else { current };
                if v >= 1_000 {
                    // Big scale: fold everything so far.
                    if v >= last_scale {
                        return None; // "мың мың" — malformed
                    }
                    total = total.checked_add(base.checked_mul(v)?)?;
                    current = 0;
                    last_scale = v;
                } else {
                    // жүз
                    current = base.checked_mul(v)?;
                }
            }
        }
    }
    Some(total.checked_add(current)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenizer_artifacts() {
        assert_eq!(fix_tokenizer_artifacts("елде көптеген тағы басқаиғи ресурстар"), "елде көптеген табиғи ресурстар");
        assert_eq!(fix_tokenizer_artifacts("мектеп басқаасшысы келді"), "мектеп басшысы келді");
        assert_eq!(fix_tokenizer_artifacts("басқаолды"), "болды");
        // Real words and the real phrase stay.
        for ok in ["тағы басқа жемістер", "басқару", "басқаша", "басқалар", "басқаға", "басқа адам", "басқарма"] {
            assert_eq!(fix_tokenizer_artifacts(ok), ok);
        }
    }

    #[test]
    fn clean_fixes_spacing_and_tags() {
        assert_eq!(clean("Сәлем ,  әлем !"), "Сәлем, әлем!");
        assert_eq!(clean("[музыка] Қалайсың?"), "Қалайсың?");
        assert_eq!(clean("Иә...иә"), "Иә. иә");
    }

    #[test]
    fn clean_repairs_latin_lookalikes() {
        // 'a' and 'o' are Latin here.
        assert_eq!(clean("Қaзaқстaн Респyбликaсы"), "Қазақстан Республикасы");
        // Pure Latin word is untouched.
        assert_eq!(clean("Hello Қазақстан"), "Hello Қазақстан");
    }

    #[test]
    fn transliterates_2021_standard() {
        // и → ı (dotless), і → i per the 2021 standard.
        assert_eq!(to_latin("Қазақстан Республикасы"), "Qazaqstan Respublıkasy");
        assert_eq!(to_latin("Әсем өнер ұлттық үй"), "Äsem öner ūlttyq üı");
        assert_eq!(to_latin("Жаңа шаң ғылым"), "Jaña şañ ğylym");
        assert_eq!(to_latin("ІШКІ"), "İŞKİ");
        assert_eq!(to_latin("Юра ЮРА"), "Iura IURA");
    }

    #[test]
    fn hotwords_snap_near_misses() {
        let hw = HotwordCorrector::new(["Назарбаев Университеті", "Алматы", "Qazstt"], 0.25, 4);
        assert_eq!(hw.apply("Мен Алмата қаласындамын."), "Мен Алматы қаласындамын.");
        assert_eq!(
            hw.apply("назарбаев универститеті жақсы"),
            "Назарбаев Университеті жақсы"
        );
        // Far-off words are not touched.
        assert_eq!(hw.apply("Астана қаласы"), "Астана қаласы");
    }

    #[test]
    fn numbers_compose() {
        assert_eq!(numbers_to_digits("екі жүз отыз бір"), "231");
        assert_eq!(numbers_to_digits("бес мың үш жүз"), "5300");
        assert_eq!(numbers_to_digits("жиырма бес адам"), "25 адам");
        assert_eq!(numbers_to_digits("бір адам келді"), "бір адам келді");
        assert_eq!(numbers_to_digits("екі миллион жеті жүз мың"), "2700000");
        assert_eq!(numbers_to_digits("Бағасы он тоғыз, екі жүз"), "Бағасы 19, 200");
    }

    #[test]
    fn numbers_ordinals() {
        assert_eq!(numbers_to_digits("мың тоғыз жүз қырқыншы жылы"), "1940 жылы");
        assert_eq!(numbers_to_digits("жиырмасыншы ғасыр"), "20-шы ғасыр");
        assert_eq!(numbers_to_digits("он бесінші сынып"), "15-ші сынып");
        assert_eq!(numbers_to_digits("екі мыңыншы жылдың"), "2000 жылдың");
        assert_eq!(numbers_to_digits("бірінші рет"), "бірінші рет");
        assert_eq!(numbers_to_digits("алтыншы"), "алтыншы");
    }

    #[test]
    fn capitalizes_sentence_starts() {
        assert_eq!(capitalize_sentences("сәлем. қалайсың? жақсы"), "Сәлем. Қалайсың? Жақсы");
        assert_eq!(capitalize_sentences("25 адам"), "25 адам");
    }

    #[test]
    fn edit_distance_basic() {
        assert_eq!(edit_distance("алма", "алма"), 0);
        assert_eq!(edit_distance("алма", "алмa"), 1);
        assert_eq!(edit_distance("", "abc"), 3);
    }
}
