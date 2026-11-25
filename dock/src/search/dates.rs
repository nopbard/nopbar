//! Dates on the calculator row: "today + 30 days", "오늘+30일",
//! "2026-12-25 - today" (days between) and a bare "2026-12-25" (its
//! weekday). Plain proleptic Gregorian arithmetic on whole days; the result
//! is written by Windows (see show).
//!
//! Bounded: the text is at most calc::MAX_INPUT characters (checked by the
//! caller), at most MAX_TERMS terms, each step's count at most MAX_COUNT,
//! and every date stays within the years SYSTEMTIME and people use
//! (MIN_YEAR..=MAX_YEAR).

/// "today + 1 year + 2 months + 3 days" and a little more.
const MAX_TERMS: usize = 8;
/// Larger steps leave the year range anyway.
const MAX_COUNT: i64 = 10_000_000;
const MIN_YEAR: i32 = 1601;
const MAX_YEAR: i32 = 9999;

/// A calendar day.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ymd {
    pub y: i32,
    pub m: u32,
    pub d: u32,
}

impl Ymd {
    pub const fn new(y: i32, m: u32, d: u32) -> Self {
        Ymd { y, m, d }
    }

    /// The date if it exists and is in range.
    pub(crate) fn valid(self) -> Option<Self> {
        ((MIN_YEAR..=MAX_YEAR).contains(&self.y) && (1..=12).contains(&self.m) && self.d >= 1 && self.d <= month_len(self.y, self.m)).then_some(self)
    }

    /// "2026-12-25": what the row copies.
    pub fn iso(self) -> String {
        format!("{:04}-{:02}-{:02}", self.y, self.m, self.d)
    }
}

fn leap(y: i32) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

fn month_len(y: i32, m: u32) -> u32 {
    match m {
        2 if leap(y) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// Days since 1970-01-01 (Howard Hinnant's days_from_civil).
pub(crate) fn days(d: Ymd) -> i64 {
    let y = d.y as i64 - if d.m <= 2 { 1 } else { 0 };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let m = d.m as i64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d.d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The day `z` days after 1970-01-01.
pub(crate) fn civil(z: i64) -> Ymd {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    let y = (yoe + era * 400 + if m <= 2 { 1 } else { 0 }) as i32;
    Ymd { y, m, d }
}

/// `n` months later; the day is kept, or the month's last day
/// (January 31 + 1 month = February 28 / 29).
fn add_months(d: Ymd, n: i64) -> Option<Ymd> {
    let total = d.y as i64 * 12 + (d.m as i64 - 1) + n;
    let y = i32::try_from(total.div_euclid(12)).ok()?;
    let m = total.rem_euclid(12) as u32 + 1;
    if !(MIN_YEAR..=MAX_YEAR).contains(&y) {
        return None;
    }
    Ymd::new(y, m, d.d.min(month_len(y, m))).valid()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Step {
    Day,
    Week,
    Month,
    Year,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tok {
    Date(Ymd),
    /// A count and its step ("30 days", "30일").
    Span(i64, Step),
    Plus,
    Minus,
}

/// What a date calculation gives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DateValue {
    Date(Ymd),
    /// Days from the second date to the first.
    Days(i64),
}

fn step_word(w: &str) -> Option<Step> {
    Some(match w.to_lowercase().as_str() {
        "d" | "day" | "days" | "일" => Step::Day,
        "w" | "wk" | "wks" | "week" | "weeks" | "주" | "주일" => Step::Week,
        "mo" | "month" | "months" | "개월" | "달" => Step::Month,
        "y" | "yr" | "yrs" | "year" | "years" | "년" => Step::Year,
        _ => return None,
    })
}

fn day_word(w: &str, today: Ymd) -> Option<Ymd> {
    let off = match w.to_lowercase().as_str() {
        "today" | "오늘" => 0,
        "tomorrow" | "내일" => 1,
        "yesterday" | "어제" => -1,
        "모레" => 2,
        _ => return None,
    };
    civil(days(today) + off).valid()
}

/// "2026-12-25", "2026.12.25", "2026/12/25" at `i` (4-digit year, the same
/// separator twice): the date and where it ends.
fn iso_at(cs: &[char], i: usize) -> Option<(Ymd, usize)> {
    let digits = |from: usize, max: usize| -> Option<(u32, usize)> {
        let mut j = from;
        while j < cs.len() && j - from < max && cs[j].is_ascii_digit() {
            j += 1;
        }
        let n: String = cs[from..j].iter().collect();
        (j > from).then(|| n.parse().ok().map(|v| (v, j))).flatten()
    };
    let (y, a) = digits(i, 4)?;
    let sep = *cs.get(a)?;
    if a - i != 4 || !matches!(sep, '-' | '.' | '/') {
        return None;
    }
    let (m, b) = digits(a + 1, 2)?;
    if cs.get(b) != Some(&sep) {
        return None;
    }
    let (d, c) = digits(b + 1, 2)?;
    if cs.get(c).is_some_and(|c| c.is_ascii_digit()) {
        return None;
    }
    Some((Ymd::new(y as i32, m, d), c))
}

/// Tokens, or None if the text is not a date calculation. `iso` is set
/// when a written-out date was read.
fn lex(s: &str, today: Ymd, iso: &mut bool) -> Option<Vec<Tok>> {
    let cs: Vec<char> = s.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    let word_end = |mut j: usize| {
        while j < cs.len() && cs[j].is_alphabetic() {
            j += 1;
        }
        j
    };
    while i < cs.len() {
        let c = cs[i];
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        if out.len() >= MAX_TERMS * 2 {
            return None;
        }
        match c {
            '+' => {
                out.push(Tok::Plus);
                i += 1;
            }
            '-' | '−' => {
                out.push(Tok::Minus);
                i += 1;
            }
            '0'..='9' => {
                if let Some((d, end)) = iso_at(&cs, i) {
                    *iso = true;
                    out.push(Tok::Date(d.valid()?));
                    i = end;
                    continue;
                }
                let start = i;
                while i < cs.len() && cs[i].is_ascii_digit() {
                    i += 1;
                }
                if i - start > 8 {
                    return None;
                }
                let n: i64 = cs[start..i].iter().collect::<String>().parse().ok()?;
                while i < cs.len() && cs[i].is_whitespace() {
                    i += 1;
                }
                let end = word_end(i);
                let step = step_word(&cs[i..end].iter().collect::<String>())?;
                out.push(Tok::Span(n, step));
                i = end;
            }
            c if c.is_alphabetic() => {
                let end = word_end(i);
                out.push(Tok::Date(day_word(&cs[i..end].iter().collect::<String>(), today)?));
                i = end;
            }
            _ => return None,
        }
    }
    Some(out)
}

fn shift(d: Ymd, n: i64, step: Step) -> Option<Ymd> {
    if n.abs() > MAX_COUNT {
        return None;
    }
    match step {
        Step::Day => civil(days(d) + n).valid(),
        Step::Week => civil(days(d) + n * 7).valid(),
        Step::Month => add_months(d, n),
        Step::Year => add_months(d, n.checked_mul(12)?),
    }
}

/// The value of a date calculation; None unless the whole text is one
/// (it may then be arithmetic: "2026-12-25+1", "2026-13-01"). A date must
/// come first; then steps added or taken away, and at the end maybe
/// "- <date>" (days between). A bare written-out date is shown too (its
/// weekday); a bare "today" is not (the text is more likely a search).
pub fn eval(s: &str, today: Ymd) -> Option<DateValue> {
    let mut iso = false;
    let t = lex(s, today, &mut iso)?;
    let mut it = t.into_iter().peekable();
    let Some(Tok::Date(mut d)) = it.next() else { return None };
    let mut worked = iso;
    while let Some(op) = it.next() {
        let sign = match op {
            Tok::Plus => 1,
            Tok::Minus => -1,
            _ => return None,
        };
        match it.next()? {
            Tok::Span(n, step) => {
                d = shift(d, sign * n, step)?;
                worked = true;
            }
            // "<date> - <date>": the last term.
            Tok::Date(other) if sign < 0 && it.peek().is_none() => return Some(DateValue::Days(days(d) - days(other))),
            _ => return None,
        }
    }
    worked.then_some(DateValue::Date(d))
}

/// Today on this PC's clock.
pub fn today() -> Ymd {
    let t = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
    Ymd::new(t.wYear as i32, t.wMonth as u32, t.wDay as u32)
}

/// Pure: Windows long-date pattern `p` without its weekday field ("dddd"
/// outside quotes) and the separators joining it to the rest:
/// "dddd, MMMM d, yyyy" -> "MMMM d, yyyy", "yyyy'년' M'월' d'일' dddd" ->
/// "yyyy'년' M'월' d'일'".
pub fn without_weekday(p: &str) -> String {
    let cs: Vec<char> = p.chars().collect();
    let mut keep = vec![true; cs.len()];
    let sep = |c: char| !c.is_alphanumeric() && c != '\'';
    let mut quoted = false;
    let mut i = 0;
    while i < cs.len() {
        let c = cs[i];
        if c == '\'' {
            quoted = !quoted;
        } else if !quoted && c == 'd' {
            let start = i;
            while i < cs.len() && cs[i] == 'd' {
                i += 1;
            }
            if i - start >= 3 {
                let (mut a, mut b) = (start, i);
                if cs[..start].iter().all(|&c| sep(c)) {
                    // First: the separators after it go.
                    while b < cs.len() && sep(cs[b]) {
                        b += 1;
                    }
                } else {
                    while a > 0 && sep(cs[a - 1]) {
                        a -= 1;
                    }
                }
                keep[a..b].iter_mut().for_each(|k| *k = false);
            }
            continue;
        }
        i += 1;
    }
    cs.iter().zip(keep).filter(|(_, k)| *k).map(|(c, _)| *c).collect::<String>().trim().to_string()
}

/// A date as the user's Windows writes it, with the short weekday after:
/// "2026년 12월 25일 (금)", "December 25, 2026 (Fri)".
pub fn show(d: Ymd) -> String {
    use crate::i18n::date;
    let st = windows::Win32::Foundation::SYSTEMTIME { wYear: d.y as u16, wMonth: d.m as u16, wDay: d.d as u16, ..Default::default() };
    let p = without_weekday(&date::long_pattern());
    let body = if p.is_empty() { String::new() } else { date::with_pattern(&st, &p) };
    let body = if body.is_empty() { d.iso() } else { body };
    let wd = date::weekday(&st);
    if wd.is_empty() { body } else { format!("{body} ({wd})") }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TODAY: Ymd = Ymd::new(2026, 11, 8);

    fn ev(s: &str) -> Option<DateValue> {
        eval(s, TODAY)
    }
    fn date(s: &str) -> Option<String> {
        match ev(s)? {
            DateValue::Date(d) => Some(d.iso()),
            DateValue::Days(_) => None,
        }
    }
    fn between(s: &str) -> Option<i64> {
        match ev(s)? {
            DateValue::Days(n) => Some(n),
            DateValue::Date(_) => None,
        }
    }

    #[test]
    fn civil_round_trip() {
        assert_eq!(days(Ymd::new(1970, 1, 1)), 0);
        assert_eq!(days(Ymd::new(2000, 3, 1)) - days(Ymd::new(2000, 2, 28)), 2, "2000 is a leap year");
        assert_eq!(days(Ymd::new(1900, 3, 1)) - days(Ymd::new(1900, 2, 28)), 1, "1900 is not");
        for z in [-200_000, -1, 0, 1, 59, 60, 11_016, 20_000, 2_000_000] {
            assert_eq!(days(civil(z)), z);
        }
    }

    #[test]
    fn adding() {
        assert_eq!(date("today + 30 days"), Some("2026-12-08".into()));
        assert_eq!(date("오늘+30일"), Some("2026-12-08".into()));
        assert_eq!(date("오늘 + 30일"), Some("2026-12-08".into()));
        assert_eq!(date("today+30d"), Some("2026-12-08".into()));
        assert_eq!(date("today - 8 days"), Some("2026-10-31".into()));
        assert_eq!(date("tomorrow + 1 week"), Some("2026-11-16".into()));
        assert_eq!(date("내일 + 2주"), Some("2026-11-23".into()));
        assert_eq!(date("어제 - 1일"), Some("2026-11-06".into()));
        assert_eq!(date("today + 1 year + 2 months + 3 days"), Some("2028-01-11".into()));
        assert_eq!(date("오늘 + 3개월"), Some("2027-02-08".into()));
        assert_eq!(date("Today + 1 Month"), Some("2026-12-08".into()));
    }

    #[test]
    fn month_and_year_boundaries() {
        assert_eq!(date("2026-12-25 + 7 days"), Some("2027-01-01".into()));
        assert_eq!(date("2026-03-01 - 1 day"), Some("2026-02-28".into()));
        assert_eq!(date("2024-03-01 - 1 day"), Some("2024-02-29".into()));
        assert_eq!(date("2026-01-31 + 1 month"), Some("2026-02-28".into()));
        assert_eq!(date("2024-01-31 + 1 month"), Some("2024-02-29".into()));
        assert_eq!(date("2026-03-31 - 1 month"), Some("2026-02-28".into()));
        assert_eq!(date("2026-10-31 + 1 month"), Some("2026-11-30".into()));
        assert_eq!(date("2024-02-29 + 1 year"), Some("2025-02-28".into()));
        assert_eq!(date("2024-02-29 + 4 years"), Some("2028-02-29".into()));
        assert_eq!(date("2026-11-30 + 2 months"), Some("2027-01-30".into()));
        assert_eq!(date("2026-01-15 - 1 month"), Some("2025-12-15".into()));
    }

    #[test]
    fn days_between() {
        assert_eq!(between("2026-12-25 - today"), Some(47));
        assert_eq!(between("today - 2026-12-25"), Some(-47));
        assert_eq!(between("2025-01-01 - 2024-01-01"), Some(366));
        assert_eq!(between("2026-01-01 - 2025-01-01"), Some(365));
        assert_eq!(between("2026-03-01 - 2026-02-01"), Some(28));
        assert_eq!(between("2000-03-01 - 2000-02-01"), Some(29));
        assert_eq!(between("2026.12.25 - 오늘"), Some(47));
        assert_eq!(between("2026/12/25-today"), Some(47));
        assert_eq!(between("today + 1 week - today"), Some(7));
    }

    #[test]
    fn written_dates() {
        assert_eq!(date("2026-12-25"), Some("2026-12-25".into()), "a bare date shows its weekday");
        assert_eq!(date("2026-1-5"), Some("2026-01-05".into()));
        assert_eq!(date("2000-02-29"), Some("2000-02-29".into()));
        // Not (whole) date calculations: left to arithmetic.
        for s in ["2026-02-29", "1900-02-29", "2100-02-29", "2026-13-01", "2026-00-10", "2026-04-31", "2026-12-25 +", "2026-12-25 + 3", "2026-12-25+1", "2026/12/25*2", "2026-12-25 km to m"] {
            assert_eq!(ev(s), None, "{s}");
        }
    }

    #[test]
    fn no_row() {
        // Bare words, other text and arithmetic are someone else's.
        for s in ["today", "오늘", "tomorrow", "today 30 days", "today + 30", "today + 30 parsecs", "30 days + today", "1+2", "5 km to mi", "notepad", "", "today - today - today", "today + today"] {
            assert_eq!(ev(s), None, "{s}");
        }
    }

    #[test]
    fn bounded() {
        assert_eq!(ev("today + 100000000000 days"), None);
        assert_eq!(ev("today + 9999999 years"), None);
        assert_eq!(ev("today + 8000 years"), None, "past 9999");
        assert_eq!(ev("2026-01-01 - 1000 years"), None, "before 1601");
        let long = format!("today{}", " + 1 day".repeat(MAX_TERMS));
        assert_eq!(ev(&long), None);
        let ok = format!("today{}", " + 1 day".repeat(MAX_TERMS - 1));
        assert_eq!(date(&ok), Some("2026-11-15".into()));
    }

    #[test]
    fn weekday_field_comes_off() {
        assert_eq!(without_weekday("dddd, MMMM d, yyyy"), "MMMM d, yyyy");
        assert_eq!(without_weekday("yyyy'년' M'월' d'일' dddd"), "yyyy'년' M'월' d'일'");
        assert_eq!(without_weekday("dddd d MMMM yyyy"), "d MMMM yyyy");
        assert_eq!(without_weekday("yyyy'年'M'月'd'日'"), "yyyy'年'M'月'd'日'");
        assert_eq!(without_weekday("dddd, d. MMMM yyyy"), "d. MMMM yyyy");
        assert_eq!(without_weekday("d 'dddd' MMMM"), "d 'dddd' MMMM", "quoted text stays");
        assert_eq!(without_weekday("dd.MM.yyyy"), "dd.MM.yyyy", "a two-digit day is a day");
    }

    #[test]
    fn shown_with_the_weekday() {
        let _l = crate::i18n::test_lock("ko");
        let s = show(Ymd::new(2026, 12, 25));
        assert!(s.contains("2026") && s.contains("25") && s.ends_with("(금)"), "{s}");
        assert_eq!(s.matches('금').count(), 1, "the weekday once: {s}");
    }
}
