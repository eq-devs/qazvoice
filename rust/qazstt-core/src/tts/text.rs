//! Text front end for Kazakh TTS: written text → speakable words → phonemes.
//!
//! - numbers: cardinals, ordinals (`2024-ші`, `1990 жылы`, `XXI ғасыр`, `29 қазан`),
//!   case suffixes (`25-ке`), decimals (`3,5`), percent, currency, time, dates
//! - abbreviations (`ҚР`, `т.б.`, `км` after a number…); unknown capitalised
//!   consonant-only tokens are spelled letter by letter
//! - Latin script (2021 Kazakh alphabet, also plain ASCII names) → Cyrillic
//! - sentence splitting, then [`super::g2p`] per word, with Piper's punctuation
//!   conventions (`,` `;` `:` keep a following space; `.` `!` `?` end a sentence)

use super::g2p;

const UNITS: [&str; 10] = ["нөл", "бір", "екі", "үш", "төрт", "бес", "алты", "жеті", "сегіз", "тоғыз"];
const TENS: [&str; 10] = ["", "он", "жиырма", "отыз", "қырық", "елу", "алпыс", "жетпіс", "сексен", "тоқсан"];
const SCALES: [&str; 5] = ["", "мың", "миллион", "миллиард", "триллион"];
const MONTHS: [&str; 12] =
    ["қаңтар", "ақпан", "наурыз", "сәуір", "мамыр", "маусым", "шілде", "тамыз", "қыркүйек", "қазан", "қараша", "желтоқсан"];

/// Cardinal number in words: 1990 → "мың тоғыз жүз тоқсан".
pub fn cardinal(mut n: u64) -> String {
    if n == 0 {
        return UNITS[0].into();
    }
    let mut groups = Vec::new();
    while n > 0 {
        groups.push((n % 1000) as usize);
        n /= 1000;
    }
    let mut words: Vec<&str> = Vec::new();
    for (scale, &g) in groups.iter().enumerate().rev() {
        if g == 0 || scale >= SCALES.len() {
            continue;
        }
        // "мың", not "бір мың"; "бір миллион" keeps the "бір".
        if !(g == 1 && scale == 1) {
            hundreds(g, &mut words);
        }
        if scale > 0 {
            words.push(SCALES[scale]);
        }
    }
    words.join(" ")
}

fn hundreds(g: usize, words: &mut Vec<&'static str>) {
    let (h, t, u) = (g / 100, g / 10 % 10, g % 10);
    if h > 0 {
        if h > 1 {
            words.push(UNITS[h]);
        }
        words.push("жүз");
    }
    if t > 0 {
        words.push(TENS[t]);
    }
    if u > 0 {
        words.push(UNITS[u]);
    }
}

/// Ordinal: 2024 → "екі мың жиырма төртінші".
pub fn ordinal(n: u64) -> String {
    let c = cardinal(n);
    let (head, last) = match c.rfind(' ') {
        Some(i) => (&c[..=i], &c[i + 1..]),
        None => ("", c.as_str()),
    };
    let ord = match last {
        "жиырма" => "жиырмасыншы".to_string(),
        "қырық" => "қырқыншы".to_string(),
        w if ends_with_vowel(w) => format!("{w}{}", if is_back(w) { "ншы" } else { "нші" }),
        w => format!("{w}{}", if is_back(w) { "ыншы" } else { "інші" }),
    };
    format!("{head}{ord}")
}

fn ends_with_vowel(w: &str) -> bool {
    w.chars().last().is_some_and(|c| "аәеиоөұүыіэюяу".contains(c))
}

/// Vowel harmony: is the last vowel of `w` a back vowel?
fn is_back(w: &str) -> bool {
    for c in w.chars().rev() {
        if "аоұыя".contains(c) {
            return true;
        }
        if "әеөүіэи".contains(c) {
            return false;
        }
    }
    true
}

fn digits_value(d: &str) -> Option<u64> {
    if d.is_empty() || d.len() > 15 {
        return None;
    }
    d.parse().ok()
}

fn spell_digits(d: &str) -> String {
    d.chars().filter_map(|c| c.to_digit(10)).map(|v| UNITS[v as usize]).collect::<Vec<_>>().join(" ")
}

fn number_words(d: &str) -> String {
    match digits_value(d) {
        Some(v) if !(d.len() > 1 && d.starts_with('0')) => cardinal(v),
        _ => spell_digits(d),
    }
}

fn roman_value(s: &str) -> Option<u64> {
    if s.is_empty() || s.len() > 8 {
        return None;
    }
    let val = |c| match c {
        'I' | 'І' => Some(1),
        'V' => Some(5),
        'X' | 'Х' => Some(10),
        'L' => Some(50),
        'C' | 'С' => Some(100),
        _ => None,
    };
    let vals: Option<Vec<u64>> = s.chars().map(val).collect();
    let vals = vals?;
    let mut total = 0i64;
    for (i, &v) in vals.iter().enumerate() {
        if vals.get(i + 1).is_some_and(|&n| n > v) {
            total -= v as i64;
        } else {
            total += v as i64;
        }
    }
    (total > 0).then_some(total as u64)
}

const ORDINAL_SUFFIXES: [&str; 6] = ["ыншы", "інші", "ншы", "нші", "шы", "ші"];

/// Abbreviations read as words. Matched on the token as written (dots included).
fn abbreviation(tok: &str) -> Option<&'static str> {
    Some(match tok {
        "ҚР" => "Қазақстан Республикасы",
        "РФ" => "Ресей Федерациясы",
        "АҚШ" => "Америка Құрама Штаттары",
        "БҰҰ" => "Біріккен Ұлттар Ұйымы",
        "ТМД" => "Тәуелсіз Мемлекеттер Достастығы",
        "ЕО" => "Еуропалық Одақ",
        "ҚХР" => "Қытай Халық Республикасы",
        "КСРО" => "Кеңестік Социалистік Республикалар Одағы",
        "ЖШС" => "жауапкершілігі шектеулі серіктестік",
        "АҚ" => "акционерлік қоғам",
        "ҰБТ" => "ұлттық бірыңғай тестілеу",
        "ЖОО" => "жоғары оқу орны",
        "ІІМ" => "Ішкі істер министрлігі",
        "ҰҚК" => "Ұлттық қауіпсіздік комитеті",
        "ТЖМ" => "Төтенше жағдайлар министрлігі",
        "ЖІӨ" => "жалпы ішкі өнім",
        "ҚазҰУ" => "Қазақ ұлттық университеті",
        "ЕҰУ" => "Еуразия ұлттық университеті",
        "т.б." | "т.б" => "тағы басқа",
        "т.с.с." | "т.с.с" => "тағы сол сияқты",
        "т.с." => "тағы сол",
        "млн" | "млн." => "миллион",
        "млрд" | "млрд." => "миллиард",
        "трлн" | "трлн." => "триллион",
        "км" => "километр",
        "кг" => "килограмм",
        "см" => "сантиметр",
        "мм" => "миллиметр",
        "га" => "гектар",
        "тг" | "тг." => "теңге",
        "мин." => "минут",
        "сағ." => "сағат",
        "сек." => "секунд",
        "проф." => "профессор",
        "акад." => "академик",
        "обл." => "облысы",
        "көш." => "көшесі",
        "№" => "нөмір",
        "&" => "және",
        "₸" => "теңге",
        "$" => "доллар",
        "€" => "еуро",
        "₽" => "рубль",
        "%" => "пайыз",
        _ => return None,
    })
}

/// Units that are only expanded right after a number (`5 м`, `3 т`).
fn unit_after_number(tok: &str) -> Option<&'static str> {
    Some(match tok {
        "м" => "метр",
        "т" => "тонна",
        "л" => "литр",
        "г" => "грамм",
        "ж." | "ж" => "жылы",
        "ғ." | "ғ" => "ғасыр",
        _ => return None,
    })
}

fn letter_name(c: char) -> &'static str {
    match c {
        'Б' => "бы",
        'В' => "вэ",
        'Г' => "гэ",
        'Ғ' => "ғы",
        'Д' => "ды",
        'Ж' => "жы",
        'З' => "зы",
        'Й' => "ий",
        'К' => "кы",
        'Қ' => "қы",
        'Л' => "ыл",
        'М' => "ым",
        'Н' => "ын",
        'Ң' => "ың",
        'П' => "пы",
        'Р' => "ыр",
        'С' => "ыс",
        'Т' => "ты",
        'Ф' => "эф",
        'Х' => "хы",
        'Һ' => "һы",
        'Ц' => "цы",
        'Ч' => "чы",
        'Ш' => "шы",
        'Щ' => "щы",
        _ => "",
    }
}

fn is_kazakh_vowel(c: char) -> bool {
    "аәеёиоөұүыіэюяуАӘЕЁИОӨҰҮЫІЭЮЯУ".contains(c)
}

fn is_cyrillic(c: char) -> bool {
    ('\u{0400}'..='\u{04FF}').contains(&c)
}

/// Latin (2021 Kazakh alphabet or plain ASCII) → Cyrillic, lowercase.
pub fn latin_to_cyrillic(word: &str) -> String {
    let w: Vec<char> = word.to_lowercase().chars().collect();
    let mut out = String::with_capacity(word.len() * 2);
    let mut i = 0;
    while i < w.len() {
        let c = w[i];
        let n = w.get(i + 1).copied();
        let two = |s: &str| n.is_some_and(|n| s.chars().nth(1) == Some(n) && s.starts_with(c));
        if two("sh") {
            out.push('ш');
            i += 2;
            continue;
        }
        if two("ch") {
            out.push('ч');
            i += 2;
            continue;
        }
        if two("zh") {
            out.push('ж');
            i += 2;
            continue;
        }
        if c == 'ı' {
            match n {
                Some('a') => {
                    out.push('я');
                    i += 2;
                    continue;
                }
                Some('u') => {
                    out.push('ю');
                    i += 2;
                    continue;
                }
                Some('o') => {
                    out.push('ё');
                    i += 2;
                    continue;
                }
                _ => {}
            }
            let after_vowel = out.chars().last().is_some_and(is_kazakh_vowel);
            out.push(if after_vowel { 'й' } else { 'и' });
            i += 1;
            continue;
        }
        let m = match c {
            'a' => "а",
            'ä' => "ә",
            'b' => "б",
            'c' => "к",
            'ç' => "ч",
            'd' => "д",
            'e' => "е",
            'f' => "ф",
            'g' => "г",
            'ğ' => "ғ",
            'h' => "х",
            'i' | 'İ' => "і",
            'j' => "ж",
            'k' => "к",
            'l' => "л",
            'm' => "м",
            'n' => "н",
            'ñ' => "ң",
            'o' => "о",
            'ö' => "ө",
            'p' => "п",
            'q' => "қ",
            'r' => "р",
            's' => "с",
            'ş' => "ш",
            't' => "т",
            'u' => "у",
            'ū' => "ұ",
            'ü' => "ү",
            'v' => "в",
            'w' => "у",
            'x' => "кс",
            'y' => "ы",
            'z' => "з",
            _ => "",
        };
        out.push_str(m);
        i += 1;
    }
    out
}

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Word(String),
    Num(String),
    Punct(char),
    Other(String),
}

/// Tokens, each with whether whitespace preceded it ("3,5" vs "3, 5").
fn tokenize(text: &str) -> (Vec<Tok>, Vec<bool>) {
    let mut toks = Vec::new();
    let mut spaced = Vec::new();
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    let mut space = true;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            space = true;
            i += 1;
            continue;
        }
        spaced.resize(toks.len(), false);
        spaced.push(space);
        space = false;
        if c.is_ascii_digit() {
            let s = i;
            while i < chars.len() && chars[i].is_ascii_digit() {
                i += 1;
            }
            toks.push(Tok::Num(chars[s..i].iter().collect()));
            continue;
        }
        if c.is_alphabetic() {
            let s = i;
            while i < chars.len() && (chars[i].is_alphabetic() || chars[i] == '\'' || chars[i] == '’') {
                i += 1;
            }
            // Keep the dots of abbreviations like "т.б." / "млн." attached.
            let mut word: String = chars[s..i].iter().collect();
            let mut j = i;
            while j < chars.len() && chars[j] == '.' {
                let cand: String = chars[s..=j].iter().collect();
                let mut k = j + 1;
                while k < chars.len() && chars[k].is_alphabetic() {
                    k += 1;
                }
                let longer: String = chars[s..k].iter().collect::<String>() + ".";
                if k > j + 1 && k < chars.len() && chars[k] == '.' && (abbreviation(&longer).is_some() || is_multi_abbr(&longer)) {
                    word = longer;
                    j = k;
                    i = k + 1;
                    continue;
                }
                if abbreviation(&cand).is_some() || unit_after_number(&cand).is_some() {
                    word = cand;
                    i = j + 1;
                }
                break;
            }
            toks.push(Tok::Word(word));
            continue;
        }
        match c {
            '.' | ',' | ';' | ':' | '!' | '?' | '…' => toks.push(Tok::Punct(if c == '…' { '.' } else { c })),
            '-' | '–' | '—' => toks.push(Tok::Punct('-')),
            _ => toks.push(Tok::Other(c.to_string())),
        }
        i += 1;
    }
    spaced.truncate(toks.len());
    (toks, spaced)
}

fn is_multi_abbr(s: &str) -> bool {
    matches!(s, "т.с.с.")
}

fn is_ordinal_suffix(s: &str) -> bool {
    ORDINAL_SUFFIXES.contains(&s)
}

fn starts_ordinal_context(word: &str) -> bool {
    let w = word.to_lowercase();
    w.starts_with("жыл") || w.starts_with("ғасыр") || MONTHS.iter().any(|m| w.starts_with(m))
}

/// Written text → lowercase speakable text (only Cyrillic words and punctuation).
pub fn normalize(text: &str) -> String {
    let (toks, spaced) = tokenize(text);
    let glued = |k: usize| !spaced.get(k).copied().unwrap_or(true);
    let mut out: Vec<String> = Vec::new();
    let push_punct = |out: &mut Vec<String>, p: char| {
        if let Some(last) = out.last_mut() {
            if last.ends_with(['.', ',', ';', ':', '!', '?']) {
                return;
            }
            last.push(p);
        }
    };
    let mut hyphen = false;
    let mut i = 0;
    while i < toks.len() {
        if hyphen && !matches!(toks[i], Tok::Word(_)) {
            hyphen = false;
        }
        match &toks[i] {
            Tok::Num(d) => {
                let d = d.clone();
                let next = toks.get(i + 1);
                let next2 = toks.get(i + 2);
                let next3 = toks.get(i + 3);
                let next4 = toks.get(i + 4);
                // dd.mm.yyyy
                if let (Some(Tok::Punct('.')), Some(Tok::Num(m)), Some(Tok::Punct('.')), Some(Tok::Num(y))) = (next, next2, next3, next4) {
                    if let (Some(dv), Some(mv), Some(yv)) = (digits_value(&d), digits_value(m), digits_value(y)) {
                        if (1..=31).contains(&dv) && (1..=12).contains(&mv) && y.len() == 4 {
                            out.push(format!("{} {} {} жыл", ordinal(yv), ordinal(dv), MONTHS[mv as usize - 1]));
                            i += 5;
                            continue;
                        }
                    }
                }
                // hh:mm
                if let (Some(Tok::Punct(':')), Some(Tok::Num(m))) = (next, next2) {
                    if d.len() <= 2 && m.len() == 2 {
                        out.push(format!("{} {}", number_words(&d), number_words(m)));
                        i += 3;
                        continue;
                    }
                }
                // decimals: 3,5 / 3.5
                if let (Some(Tok::Punct(',' | '.')), Some(Tok::Num(f))) = (next, next2) {
                    if f.len() <= 3 && glued(i + 1) && glued(i + 2) {
                        let denom = ["оннан", "жүзден", "мыңнан"][f.len() - 1];
                        out.push(format!("{} бүтін {denom} {}", number_words(&d), number_words(f.trim_start_matches('0').max("0"))));
                        i += 3;
                        continue;
                    }
                }
                // thousands written with spaces: 1 000 000
                let mut whole = d.clone();
                let mut j = i + 1;
                while let Some(Tok::Num(g)) = toks.get(j) {
                    if g.len() == 3 && whole.len() <= 12 && spaced[j] {
                        whole.push_str(g);
                        j += 1;
                    } else {
                        break;
                    }
                }
                if whole.len() > d.len() && d.len() > 3 {
                    whole = d.clone();
                    j = i + 1;
                }
                // 25-ке, 2024-ші
                if let (Some(Tok::Punct('-')), Some(Tok::Word(suf))) = (toks.get(j), toks.get(j + 1)) {
                    let suf_l = suf.to_lowercase();
                    if suf_l.chars().all(is_cyrillic) && suf_l.chars().count() <= 5 {
                        let v = digits_value(&whole);
                        let w = match v {
                            Some(v) if is_ordinal_suffix(&suf_l) => ordinal(v),
                            _ => format!("{}{suf_l}", number_words(&whole)),
                        };
                        out.push(w);
                        i = j + 2;
                        continue;
                    }
                }
                // 15%, 100 ₸, 5 км, 1990 ж.
                let after = toks.get(j);
                let ord = matches!(after, Some(Tok::Word(w)) if starts_ordinal_context(w) || matches!(w.as_str(), "ж." | "ж" | "ғ." | "ғ"));
                let words = match digits_value(&whole) {
                    Some(v) if ord => ordinal(v),
                    _ => number_words(&whole),
                };
                out.push(words);
                i = j;
                match after {
                    Some(Tok::Other(s)) if abbreviation(s).is_some() => {
                        out.push(abbreviation(s).unwrap().into());
                        i += 1;
                    }
                    Some(Tok::Word(w)) if unit_after_number(w).is_some() => {
                        out.push(unit_after_number(w).unwrap().into());
                        i += 1;
                    }
                    _ => {}
                }
            }
            Tok::Word(w) => {
                // XXI ғасыр
                if let (Some(v), Some(Tok::Word(n))) = (roman_value(w), toks.get(i + 1)) {
                    if n.to_lowercase().starts_with("ғасыр") || matches!(n.as_str(), "ғ." | "ғ") {
                        out.push(ordinal(v));
                        i += 1;
                        continue;
                    }
                }
                if let Some(a) = abbreviation(w) {
                    out.push(a.to_lowercase());
                } else if w.chars().any(|c| c.is_ascii_alphabetic() || "äğñöşūüıçİ".contains(c)) {
                    out.push(latin_to_cyrillic(w));
                } else if w.chars().count() > 1 && w.chars().all(|c| c.is_uppercase()) && !w.chars().any(is_kazakh_vowel) {
                    out.push(w.chars().map(letter_name).collect::<Vec<_>>().join(" "));
                } else if w.chars().count() == 1 && w.chars().all(|c| c.is_uppercase()) && !letter_name(w.chars().next().unwrap()).is_empty() && matches!(toks.get(i + 1), Some(Tok::Punct('.'))) {
                    // An initial: "Г. Марченко".
                    out.push(letter_name(w.chars().next().unwrap()).into());
                } else {
                    let t = w.trim_end_matches('.').to_lowercase();
                    match out.last_mut() {
                        Some(last) if hyphen => {
                            last.push('-');
                            last.push_str(&t);
                        }
                        _ => out.push(t),
                    }
                    if w.ends_with('.') {
                        push_punct(&mut out, '.');
                    }
                }
                hyphen = false;
                i += 1;
            }
            Tok::Punct(p) => {
                match p {
                    '-' => {
                        // A dash between words is a pause; a hyphen inside a compound is not.
                        if !glued(i) || !glued(i + 1) {
                            push_punct(&mut out, ';');
                        } else if matches!(toks.get(i + 1), Some(Tok::Word(_))) && out.last().is_some_and(|w| w.ends_with(|c: char| c.is_alphabetic())) {
                            // Compound ("қарым-қатынас"): one word, each part stressed.
                            hyphen = true;
                        }
                    }
                    p => push_punct(&mut out, *p),
                }
                i += 1;
            }
            Tok::Other(s) => {
                if let Some(a) = abbreviation(s) {
                    // Currency before the number: "$100" → "жүз доллар".
                    if matches!(s.as_str(), "$" | "€" | "₽" | "₸") && matches!(toks.get(i + 1), Some(Tok::Num(_))) {
                        if let Some(Tok::Num(d)) = toks.get(i + 1) {
                            out.push(format!("{} {a}", number_words(d)));
                            i += 2;
                            continue;
                        }
                    }
                    out.push(a.into());
                }
                i += 1;
            }
        }
    }
    out.retain(|w| !w.is_empty());
    out.join(" ")
}

/// Split normalized text into sentences at `.` `!` `?`, then cut sentences longer
/// than `max_chars` at commas (or spaces) so one synthesis call stays short.
pub fn sentences(normalized: &str, max_chars: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    // A "word" with no spaces in it (a pasted URL, a run of digits or letters)
    // can be longer than any sentence: cut it so no voice call gets it whole.
    let max_word = max_chars.max(1);
    let words = normalized.split(' ').flat_map(|w| {
        let chars: Vec<char> = w.chars().collect();
        if chars.len() <= max_word {
            vec![w.to_string()]
        } else {
            chars.chunks(max_word).map(|c| c.iter().collect()).collect()
        }
    });
    let hard_max = (max_chars * 3 / 2).max(max_word);
    let mut cur_len = 0;
    for word in words {
        let word = word.as_str();
        let len = word.chars().count();
        // Never let a piece grow past the hard limit, even with no comma in sight.
        if cur_len > 0 && cur_len + 1 + len > hard_max {
            out.push(std::mem::take(&mut cur));
            cur_len = 0;
        }
        if cur_len > 0 {
            cur.push(' ');
            cur_len += 1;
        }
        cur.push_str(word);
        cur_len += len;
        let ends = word.ends_with(['.', '!', '?']);
        let long = cur_len >= max_chars && word.ends_with([',', ';', ':']);
        if ends || long {
            out.push(std::mem::take(&mut cur));
            cur_len = 0;
        }
    }
    if !cur.trim().is_empty() {
        out.push(cur);
    }
    out
}

/// [`sentences`] for streaming playback: if the first sentence is longer than
/// `first_chars`, it is cut at its first clause mark (`,` `;` `:`) so the
/// listener hears something after a short first synthesis instead of waiting
/// for a whole long sentence. Later sentences are left whole (better prosody).
pub fn sentences_fast_start(normalized: &str, first_chars: usize, max_chars: usize) -> Vec<String> {
    const MIN_HEAD: usize = 20; // shorter than this is not worth a separate call
    let mut out = sentences(normalized, max_chars);
    let Some(first) = out.first() else { return out };
    if first.chars().count() <= first_chars {
        return out;
    }
    let words: Vec<&str> = first.split(' ').collect();
    let mut n = 0;
    for (i, w) in words.iter().enumerate() {
        n += w.chars().count() + 1;
        if n > first_chars * 2 {
            break;
        }
        if n >= MIN_HEAD && i + 1 < words.len() && w.ends_with([',', ';', ':']) {
            let (head, tail) = (words[..=i].join(" "), words[i + 1..].join(" "));
            out[0] = tail;
            out.insert(0, head);
            break;
        }
    }
    out
}

/// One normalized sentence → Piper phoneme string ("sælˈem, ælˈem!").
pub fn phonemize(sentence: &str) -> String {
    let mut out = String::with_capacity(sentence.len() * 2);
    for word in sentence.split(' ').filter(|w| !w.is_empty()) {
        let punct: Option<char> = word.chars().rev().find(|c| matches!(c, '.' | ',' | ';' | ':' | '!' | '?'));
        let ph: String = word.split('-').map(|part| g2p::word(&part.chars().filter(|c| is_cyrillic(*c)).collect::<String>())).collect();
        if ph.is_empty() && punct.is_none() {
            continue;
        }
        if !out.is_empty() && !out.ends_with(' ') {
            out.push(' ');
        }
        out.push_str(&ph);
        if let Some(p) = punct {
            out.push(p);
            if matches!(p, ',' | ';' | ':') {
                out.push(' ');
            }
        }
    }
    out.trim_end().to_string()
}

/// Whole front end: written text → one phoneme string per sentence.
pub fn text_to_phonemes(text: &str) -> Vec<String> {
    sentences(&normalize(text), 200).iter().map(|s| phonemize(s)).filter(|p| !p.is_empty()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fast_start_cuts_only_a_long_first_sentence() {
        let long = "бүгін ауа райы өте жақсы болады, сондықтан біз қалалық саябаққа барып, достарымызбен бірге серуендеп, кешке дейін демаламыз. екінші сөйлем, ол қысқа.";
        let s = sentences_fast_start(long, 80, 200);
        assert_eq!(s[0], "бүгін ауа райы өте жақсы болады,");
        assert_eq!(s.len(), 3);
        assert_eq!(s.join(" "), long);
        assert_eq!(s[2], "екінші сөйлем, ол қысқа.");
        // Short first sentence, or no clause mark early enough: unchanged.
        assert_eq!(sentences_fast_start("сәлем, әлем. қалайсың?", 80, 200), sentences("сәлем, әлем. қалайсың?", 200));
        let no_mark = "а".repeat(30) + " " + &"б".repeat(200) + ", соңы.";
        assert_eq!(sentences_fast_start(&no_mark, 80, 200), sentences(&no_mark, 200));
        assert!(sentences_fast_start("", 80, 200).is_empty());
    }

    #[test]
    fn cardinals() {
        assert_eq!(cardinal(0), "нөл");
        assert_eq!(cardinal(25), "жиырма бес");
        assert_eq!(cardinal(100), "жүз");
        assert_eq!(cardinal(1990), "мың тоғыз жүз тоқсан");
        assert_eq!(cardinal(2024), "екі мың жиырма төрт");
        assert_eq!(cardinal(1_000_000), "бір миллион");
        assert_eq!(cardinal(3_450_017), "үш миллион төрт жүз елу мың он жеті");
    }

    #[test]
    fn ordinals() {
        assert_eq!(ordinal(1), "бірінші");
        assert_eq!(ordinal(2), "екінші");
        assert_eq!(ordinal(6), "алтыншы");
        assert_eq!(ordinal(20), "жиырмасыншы");
        assert_eq!(ordinal(40), "қырқыншы");
        assert_eq!(ordinal(1990), "мың тоғыз жүз тоқсаныншы");
        assert_eq!(ordinal(2024), "екі мың жиырма төртінші");
        assert_eq!(ordinal(21), "жиырма бірінші");
    }

    #[test]
    fn normalizes_numbers_and_abbreviations() {
        let n = |s: &str| normalize(s);
        assert_eq!(n("Мен 1990 жылы туылдым."), "мен мың тоғыз жүз тоқсаныншы жылы туылдым.");
        assert_eq!(n("2024-ші жыл"), "екі мың жиырма төртінші жыл");
        assert_eq!(n("25-ке толды"), "жиырма беске толды");
        assert_eq!(n("3,5 пайыз"), "үш бүтін оннан бес пайыз");
        assert_eq!(n("15% өсті"), "он бес пайыз өсті");
        assert_eq!(n("ҚР Президенті"), "қазақстан республикасы президенті");
        assert_eq!(n("алма, алмұрт т.б. жемістер"), "алма, алмұрт тағы басқа жемістер");
        assert_eq!(n("XXI ғасыр"), "жиырма бірінші ғасыр");
        assert_eq!(n("29 қазан"), "жиырма тоғызыншы қазан");
        assert_eq!(n("сағат 14:30"), "сағат он төрт отыз");
        assert_eq!(n("5 км жол"), "бес километр жол");
        assert_eq!(n("1 000 000 теңге"), "бір миллион теңге");
        assert_eq!(n("$100"), "жүз доллар");
        assert_eq!(n("1, 2, 3"), "бір, екі, үш");
        assert_eq!(n("Ол – мұғалім"), "ол; мұғалім");
        assert_eq!(n("әр-түрлі"), "әр-түрлі");
        assert_eq!(n("25-ке"), "жиырма беске");
    }

    #[test]
    fn latin_script() {
        assert_eq!(latin_to_cyrillic("Qazaqstan"), "қазақстан");
        assert_eq!(latin_to_cyrillic("Almaty"), "алматы");
        assert_eq!(latin_to_cyrillic("aıtty"), "айтты");
        assert_eq!(normalize("Sälem, älem!"), "сәлем, әлем!");
    }

    #[test]
    fn phonemes_follow_piper_punctuation() {
        assert_eq!(text_to_phonemes("Сәлем, әлем! Қалайсың?"), vec!["sælˈem, ælˈem!", "qɑɫˈɑjsəŋ?"]);
        assert_eq!(text_to_phonemes("құрал-жабдық"), vec!["qʊɾˈɑɫʒˈɑbdəq"]);
    }
}
