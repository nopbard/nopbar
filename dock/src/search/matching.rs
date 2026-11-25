//! How well a name matches what was typed, search ranking: the whole name
//! > its start > the start of a word > anywhere > word initials ("vsc" for
//! Visual Studio Code). Hangul initial consonants match their syllables
//! ("ㅋㅋㅇ" finds 카카오톡), and the syllable still being composed matches
//! what it will become ("카카옽" while typing 카카오톡).

/// Initial consonants (choseong) in syllable order, as compatibility jamo.
const CHO: [char; 19] = ['ㄱ', 'ㄲ', 'ㄴ', 'ㄷ', 'ㄸ', 'ㄹ', 'ㅁ', 'ㅂ', 'ㅃ', 'ㅅ', 'ㅆ', 'ㅇ', 'ㅈ', 'ㅉ', 'ㅊ', 'ㅋ', 'ㅌ', 'ㅍ', 'ㅎ'];
const SYL_FIRST: u32 = 0xAC00;
const SYL_LAST: u32 = 0xD7A3;

/// Match tiers (higher is better).
pub const EXACT: u32 = 6;
pub const PREFIX: u32 = 5;
pub const WORD: u32 = 4;
pub const INSIDE: u32 = 3;
pub const TERMS: u32 = 2;
pub const INITIALS: u32 = 1;

/// (choseong index, jungseong index, jongseong index) of a Hangul syllable.
fn parts(c: char) -> Option<(u32, u32, u32)> {
    let u = c as u32;
    if !(SYL_FIRST..=SYL_LAST).contains(&u) {
        return None;
    }
    let i = u - SYL_FIRST;
    Some((i / 588, (i % 588) / 28, i % 28))
}

fn syllable(cho: u32, jung: u32, jong: u32) -> char {
    char::from_u32(SYL_FIRST + cho * 588 + jung * 28 + jong).unwrap_or('\u{FFFD}')
}

fn is_consonant(c: char) -> bool {
    CHO.contains(&c)
}

/// The initial consonant of a syllable, as a compatibility jamo.
pub fn choseong(c: char) -> Option<char> {
    parts(c).map(|(cho, _, _)| CHO[cho as usize])
}

/// A final consonant that the next syllable would take as its initial when
/// a vowel follows: (what stays as the final, the consonant that moves).
/// "옽" + ㅗ becomes "오토": (0, ㅌ). "닭" splits as (ㄹ, ㄱ).
fn split_final(jong: u32) -> Option<(u32, char)> {
    let simple = |c: char| Some((0, c));
    match jong {
        1 => simple('ㄱ'),
        2 => simple('ㄲ'),
        3 => Some((1, 'ㅅ')),
        4 => simple('ㄴ'),
        5 => Some((4, 'ㅈ')),
        6 => Some((4, 'ㅎ')),
        7 => simple('ㄷ'),
        8 => simple('ㄹ'),
        9 => Some((8, 'ㄱ')),
        10 => Some((8, 'ㅁ')),
        11 => Some((8, 'ㅂ')),
        12 => Some((8, 'ㅅ')),
        13 => Some((8, 'ㅌ')),
        14 => Some((8, 'ㅍ')),
        15 => Some((8, 'ㅎ')),
        16 => simple('ㅁ'),
        17 => simple('ㅂ'),
        18 => Some((17, 'ㅅ')),
        19 => simple('ㅅ'),
        20 => simple('ㅆ'),
        21 => simple('ㅇ'),
        22 => simple('ㅈ'),
        23 => simple('ㅊ'),
        24 => simple('ㅋ'),
        25 => simple('ㅌ'),
        26 => simple('ㅍ'),
        27 => simple('ㅎ'),
        _ => None,
    }
}

/// One typed character against one name character.
fn same(q: char, c: char) -> bool {
    q == c || (is_consonant(q) && choseong(c) == Some(q))
}

/// Lowercased characters (one per input character).
pub fn fold(s: &str) -> Vec<char> {
    s.chars().map(|c| c.to_lowercase().next().unwrap_or(c)).collect()
}

/// `c` without its accents ("é" -> "e", "ő" -> "o", "ệ" -> "e"), one
/// character for one. Only alphabets below U+2000 (Latin, Greek, Cyrillic…):
/// Hangul syllables must stay whole (matching takes them apart itself) and
/// kana voicing marks change the word.
pub fn strip_marks(c: char) -> char {
    use windows::Win32::Globalization::{NormalizeString, NormalizationD};
    if c.is_ascii() || c as u32 >= 0x2000 {
        return c;
    }
    // Letters with a stroke have no decomposition.
    match c {
        'ł' => return 'l',
        'đ' => return 'd',
        'ø' => return 'o',
        'ı' => return 'i',
        'ħ' => return 'h',
        _ => {}
    }
    let mut src = [0u16; 2];
    let src = c.encode_utf16(&mut src);
    let mut out = [0u16; 8];
    let n = unsafe { NormalizeString(NormalizationD, src, Some(&mut out)) };
    if n <= 0 {
        return c;
    }
    char::decode_utf16(out[..n as usize].iter().copied()).next().and_then(|r| r.ok()).filter(|b| !('\u{300}'..='\u{36F}').contains(b)).unwrap_or(c)
}

/// fold, also without accents (one per input character).
pub fn fold_loose(s: &str) -> Vec<char> {
    fold(s).into_iter().map(strip_marks).collect()
}

/// Length of name it takes to match all of `q` at `at`, or None. The last
/// typed character may still be in composition: a syllable without a final
/// also matches the same syllable with one ("토" ~ "톡"), and one with a
/// final also matches the syllable without it followed by a syllable that
/// starts with that consonant ("옽" ~ "오토").
fn match_at(name: &[char], q: &[char], at: usize) -> Option<usize> {
    let mut i = at;
    for (k, &qc) in q.iter().enumerate() {
        let c = *name.get(i)?;
        if same(qc, c) {
            i += 1;
            continue;
        }
        if k + 1 != q.len() {
            return None;
        }
        let (qcho, qjung, qjong) = parts(qc)?;
        let (ccho, cjung, cjong) = parts(c)?;
        if qcho != ccho || qjung != cjung {
            return None;
        }
        if qjong == 0 && cjong != 0 {
            return Some(i + 1 - at);
        }
        let (stay, moved) = split_final(qjong)?;
        if syllable(qcho, qjung, stay) != c {
            return None;
        }
        let next = *name.get(i + 1)?;
        return (same(moved, next)).then_some(i + 2 - at);
    }
    Some(i - at)
}

fn separator(c: char) -> bool {
    c.is_whitespace() || matches!(c, '-' | '_' | '.' | '(' | ')' | '[' | ']' | '/' | '\\' | '&' | '+' | ',' | ':' | '·')
}

/// Where words start.
fn word_starts(name: &[char]) -> Vec<usize> {
    (0..name.len()).filter(|&i| !separator(name[i]) && (i == 0 || separator(name[i - 1]))).collect()
}

/// A typed query, folded once.
pub struct Query {
    q: Vec<char>,
    terms: Vec<Vec<char>>,
}

impl Query {
    pub fn new(text: &str) -> Self {
        let q = fold(text.trim());
        let terms = q.split(|c| c.is_whitespace()).filter(|t| !t.is_empty()).map(|t| t.to_vec()).collect();
        Query { q, terms }
    }

    /// A query that ignores accents as well as case: use score_loose.
    pub fn loose(text: &str) -> Self {
        let q = fold_loose(text.trim());
        let terms = q.split(|c| c.is_whitespace()).filter(|t| !t.is_empty()).map(|t| t.to_vec()).collect();
        Query { q, terms }
    }

    pub fn is_empty(&self) -> bool {
        self.q.is_empty()
    }

    /// score, for a Query::loose (the name's accents are dropped too).
    pub fn score_loose(&self, name: &str) -> Option<u32> {
        self.score_of(fold_loose(name))
    }

    /// (tier, position, name length) as one number, higher is better; None
    /// if the name does not match.
    pub fn score(&self, name: &str) -> Option<u32> {
        self.score_of(fold(name))
    }

    fn score_of(&self, n: Vec<char>) -> Option<u32> {
        let (tier, pos) = self.tier(&n)?;
        Some(tier * 1_000_000 + (999 - pos.min(999) as u32) * 1000 + (999 - n.len().min(999) as u32))
    }

    /// Best tier and where it matched.
    pub fn tier(&self, n: &[char]) -> Option<(u32, usize)> {
        if self.q.is_empty() {
            return None;
        }
        if self.q == n {
            return Some((EXACT, 0));
        }
        if match_at(n, &self.q, 0).is_some() {
            return Some((PREFIX, 0));
        }
        let starts = word_starts(n);
        if let Some(&s) = starts.iter().find(|&&s| match_at(n, &self.q, s).is_some()) {
            return Some((WORD, s));
        }
        if let Some(s) = (1..n.len()).find(|&s| match_at(n, &self.q, s).is_some()) {
            return Some((INSIDE, s));
        }
        // Several words typed: each must start a word of the name.
        if self.terms.len() > 1 && self.terms.iter().all(|t| starts.iter().any(|&s| match_at(n, t, s).is_some())) {
            return Some((TERMS, 0));
        }
        // Initials: "vsc" -> Visual Studio Code.
        if self.q.len() >= 2 && !self.q.iter().any(|c| c.is_whitespace()) {
            let initials: Vec<char> = starts.iter().map(|&s| n[s]).collect();
            if initials.len() >= 2 && match_at(&initials, &self.q, 0).is_some() {
                return Some((INITIALS, 0));
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tier(q: &str, name: &str) -> Option<u32> {
        Query::new(q).tier(&fold(name)).map(|t| t.0)
    }

    #[test]
    fn tiers_in_order() {
        assert_eq!(tier("notepad", "Notepad"), Some(EXACT));
        assert_eq!(tier("note", "Notepad"), Some(PREFIX));
        assert_eq!(tier("code", "Visual Studio Code"), Some(WORD));
        assert_eq!(tier("pad", "Notepad"), Some(INSIDE));
        assert_eq!(tier("vsc", "Visual Studio Code"), Some(INITIALS));
        assert_eq!(tier("studio vis", "Visual Studio Code"), Some(TERMS));
        assert_eq!(tier("xyz", "Notepad"), None);
        assert_eq!(tier("", "Notepad"), None);
        let q = Query::new("no");
        assert!(q.score("Notepad").unwrap() > q.score("Snow").unwrap());
        // Same tier: the shorter name first.
        assert!(q.score("Notes").unwrap() > q.score("Notepad").unwrap());
        // A word start beats a match inside a word.
        let q = Query::new("te");
        assert!(q.score("Microsoft Teams").unwrap() > q.score("Notepad").unwrap());
    }

    #[test]
    fn case_and_separators() {
        assert_eq!(tier("WORD", "Microsoft Word"), Some(WORD));
        assert_eq!(tier("update", "Windows-Update"), Some(WORD));
        assert_eq!(tier("ms", "Microsoft Store"), Some(INITIALS));
    }

    #[test]
    fn korean_initials() {
        assert_eq!(tier("ㅋㅋㅇ", "카카오톡"), Some(PREFIX));
        assert_eq!(tier("ㅋㅋㅇㅌ", "카카오톡"), Some(PREFIX));
        assert_eq!(tier("카카오톡", "카카오톡"), Some(EXACT));
        assert_eq!(tier("ㅇㅌ", "카카오톡"), Some(INSIDE));
        assert_eq!(tier("ㄱㅈ", "메모장 그림판"), None);
        assert_eq!(tier("ㄱㄹ", "메모장 그림판"), Some(WORD));
        // Mixed: a syllable and initials.
        assert_eq!(tier("카ㅋ", "카카오톡"), Some(PREFIX));
        assert_eq!(tier("ㅋㅋㅇ", "카톡"), None);
        assert_eq!(choseong('톡'), Some('ㅌ'));
        assert_eq!(choseong('a'), None);
    }

    #[test]
    fn loose_ignores_accents_only_when_asked() {
        let loose = |q: &str, name: &str| Query::loose(q).score_loose(name).is_some();
        assert!(loose("demarrer", "Démarrer avec Windows"));
        assert!(loose("ÉCRAN", "écran de veille"));
        assert!(loose("zlut", "Žluťoučký kůň"));
        assert!(loose("lodz", "Łódź"));
        assert!(loose("tieng viet", "Tiếng Việt"));
        assert!(loose("CAFÉ", "cafe"), "accents typed are dropped too");
        assert!(!loose("demarrer", "Arrêter"));
        // Hangul is left whole: initials and composing still work.
        assert!(loose("ㅈㅇ", "작업 표시줄"));
        assert!(loose("작어", "작업 표시줄"));
        assert_eq!(strip_marks('카'), '카');
        assert_eq!(strip_marks('が'), 'が', "kana keep their marks");
        assert_eq!(fold_loose("Ñandú").len(), 5, "one for one");
        // The strict query still tells them apart.
        assert!(Query::new("demarrer").score("Démarrer").is_none());
    }

    #[test]
    fn korean_while_composing() {
        // The final consonant will move on once the next vowel comes.
        assert_eq!(tier("카카옽", "카카오톡"), Some(PREFIX));
        // A syllable still waiting for its final.
        assert_eq!(tier("카카오토", "카카오톡"), Some(PREFIX));
        // Compound final: 닭 = 달 + ㄱ.
        assert_eq!(tier("달", "달걀"), Some(PREFIX));
        assert_eq!(tier("닭", "달걀"), Some(PREFIX));
        // Only the last character gets this leeway.
        assert_eq!(tier("토카", "톡카"), None);
        // A final that is not the next initial: no.
        assert_eq!(tier("카카옫", "카카오톡"), None);
    }
}
