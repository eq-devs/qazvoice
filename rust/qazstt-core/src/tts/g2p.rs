//! Kazakh grapheme → phoneme rules, producing the IPA symbol set the Piper Kazakh
//! voices were trained on.
//!
//! Piper normally gets these from espeak-ng (GPL-3.0). Kazakh spelling is close to
//! phonemic, so a letter table plus two context rules reproduces espeak-ng's `kk`
//! output exactly (stress included) on 4,197 of 4,219 distinct words from the UD
//! Kazakh treebank; the rest are letter-by-letter abbreviations, which
//! [`super::text`] expands to words before they get here.
//!
//! Rules (derived by comparing outputs, not from espeak-ng's sources):
//! - `л` is clear `l` before a front vowel (е і ә ө ү э), velarized `ɫ` otherwise;
//! - `у` is always the glide `w`, `и` is `əj`, `ю` / `я` are `ju` / `ja`;
//! - word-initial `стр` gets an epenthetic `ə` (`sətɾ…`);
//! - stress (`ˈ`) goes on the vowel right before the first `ə` (ы, и); a word whose
//!   first vowel is `ə` is unstressed; a word without `ə` is stressed on its last vowel.

/// Phonemes for one lowercase Cyrillic word. Characters outside the Kazakh
/// alphabet are dropped.
pub fn word(w: &str) -> String {
    let chars: Vec<char> = w.chars().collect();
    let mut out = String::with_capacity(w.len() * 2);
    // Word-initial "стр" takes an epenthetic vowel: стратегия → sətɾɑtˈeɡəjja.
    let epenthesis = w.starts_with("стр");
    for (i, &c) in chars.iter().enumerate() {
        let next = chars.get(i + 1).copied();
        let p = match c {
            'а' => "ɑ",
            'ә' => "æ",
            'б' => "b",
            'в' => "v",
            'г' => "ɡ",
            'ғ' => "ʀ",
            'д' => "d",
            'е' | 'э' => "e",
            'ё' => "jo",
            'ж' => "ʒ",
            'з' => "z",
            'и' => "əj",
            'й' => "j",
            'к' => "k",
            'қ' => "q",
            'л' => {
                if matches!(next, Some('е' | 'і' | 'ә' | 'ө' | 'ү' | 'э')) {
                    "l"
                } else {
                    "ɫ"
                }
            }
            'м' => "m",
            'н' => "n",
            'ң' => "ŋ",
            'о' => "o",
            'ө' => "ɵ",
            'п' => "p",
            'р' => "ɾ",
            'с' => "s",
            'т' => "t",
            'у' => "w",
            'ұ' => "ʊ",
            'ү' => "u",
            'ф' => "f",
            'х' => "x",
            'һ' => "h",
            'ц' => "ts",
            'ч' => "tʃ",
            'ш' => "ʃ",
            'щ' => "ʃʃ",
            'ъ' | 'ь' => "ʔ",
            'ы' => "ə",
            'і' => "ɪ",
            'ю' => "ju",
            'я' => "ja",
            _ => "",
        };
        out.push_str(p);
    }
    let mut out = stress(&out);
    if epenthesis {
        // Inserted after stress placement: the extra ə must not count as a vowel.
        out.insert(1, 'ə');
    }
    out
}

fn is_vowel(c: char) -> bool {
    matches!(c, 'ɑ' | 'æ' | 'e' | 'o' | 'ɵ' | 'ʊ' | 'u' | 'ə' | 'ɪ' | 'a')
}

fn stress(p: &str) -> String {
    let chars: Vec<char> = p.chars().collect();
    let vowels: Vec<usize> = (0..chars.len()).filter(|&i| is_vowel(chars[i])).collect();
    let Some(&last) = vowels.last() else { return p.to_string() };
    let at = match vowels.iter().position(|&i| chars[i] == 'ə') {
        None => last,
        Some(0) => return p.to_string(),
        Some(n) => vowels[n - 1],
    };
    let mut out = String::with_capacity(p.len() + 2);
    for (i, &c) in chars.iter().enumerate() {
        if i == at {
            out.push('ˈ');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::word;

    #[test]
    fn matches_reference_phonemes() {
        // Reference strings: espeak-ng 1.52 `kk` voice as bundled with piper-tts.
        for (w, ipa) in [
            ("сәлем", "sælˈem"),
            ("қазақстан", "qɑzɑqstˈɑn"),
            ("республикасы", "ɾˈespwbɫəjkɑsə"),
            ("бүгін", "buɡˈɪn"),
            ("мектепке", "mektepkˈe"),
            ("бардым", "bˈɑɾdəm"),
            ("қызметі", "qəzmetɪ"),
            ("айқындалады", "ˈɑjqəndɑɫɑdə"),
            ("шамасымен", "ʃɑmˈɑsəmen"),
            ("экономиканың", "ekonˈoməjkɑnəŋ"),
            ("көркеюін", "kɵɾkejuˈɪn"),
            ("даяр", "dɑjˈaɾ"),
            ("бөлінді", "bɵlɪndˈɪ"),
            ("медаль", "medˈɑɫʔ"),
            ("тұщы", "tˈʊʃʃə"),
            ("марченко", "mɑɾtʃenkˈo"),
            ("полиция", "pˈoɫəjtsəjja"),
            ("стратегиялық", "sətɾɑtˈeɡəjjaɫəq"),
        ] {
            assert_eq!(word(w), ipa, "{w}");
        }
    }
}
