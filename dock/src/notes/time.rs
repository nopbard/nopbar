//! Bounded, locale-aware reminder recognition. Offsets are UTF-8 byte ranges.
//! The pure core uses local civil seconds; Win32 supplies the current local clock.
use crate::search::dates::{civil, days, Ymd};
use std::ops::Range;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found {
    pub at: i64,
    pub span: Range<usize>,
}

pub fn now() -> i64 {
    let t = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
    days(Ymd::new(t.wYear as i32, t.wMonth as u32, t.wDay as u32)) * 86400
        + t.wHour as i64 * 3600
        + t.wMinute as i64 * 60
        + t.wSecond as i64
}
pub fn clock(at: i64) -> String {
    format!(
        "{}:{:02}",
        at.rem_euclid(86400) / 3600,
        at.rem_euclid(3600) / 60
    )
}
pub fn label(at: i64, now: i64, lang: &str) -> String {
    let d = civil(at.div_euclid(86400));
    if lang == "ko" {
        let day = match at.div_euclid(86400) - now.div_euclid(86400) {
            0 => crate::i18n::in_language(lang, "topbar.panel.calendar.today").into(),
            1 => crate::i18n::in_language(lang, "agenda.tomorrow").into(),
            _ => format!("{}/{}", d.m, d.d),
        };
        let h = at.rem_euclid(86400) / 3600;
        format!(
            "{day} {} {}:{:02}",
            if h < 12 { crate::i18n::in_language(lang, "notes.time.am") } else { crate::i18n::in_language(lang, "notes.time.pm") },
            if h % 12 == 0 { 12 } else { h % 12 },
            at.rem_euclid(3600) / 60
        )
    } else {
        format!("{}/{} {}", d.m, d.d, clock(at))
    }
}
fn number(s: &str) -> Option<(u32, usize)> {
    let n = s.bytes().take_while(u8::is_ascii_digit).count();
    if n == 0 || n > 6 {
        None
    } else {
        Some((s[..n].parse().ok()?, n))
    }
}
fn spaces(s: &str) -> usize {
    s.len() - s.trim_start().len()
}
fn word(s: &str, w: &str) -> bool {
    s.starts_with(w)
        && s[w.len()..]
            .chars()
            .next()
            .is_none_or(|c| !c.is_ascii_alphabetic())
}

/// First time expression, with an adjacent day and clock recognized together.
/// Unqualified Korean hours choose the nearest future AM/PM occurrence.
pub fn parse(text: &str, now: i64, lang: &str, date_hour: Option<u8>) -> Option<Found> {
    if text.len() > 16384 {
        return None;
    }
    let ko = lang == "ko";
    let en = lang.starts_with("en");
    let day = now.div_euclid(86400);
    let today = civil(day);
    for (start, _) in text.char_indices() {
        if start > 0
            && text[..start]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_ascii_alphanumeric() || c == ':' || c == '/')
        {
            continue;
        }
        let s = &text[start..];
        // Relative forms are recognized before clock forms (2시간 is not 2시).
        let prefix = if en && s.starts_with("in ") { 3 } else { 0 };
        if let Some((n, k)) = number(&s[prefix..]) {
            let tail = &s[prefix + k..];
            let gap = spaces(tail);
            let tail = &tail[gap..];
            let unit = if ko && tail.starts_with("시간") {
                Some((3600, 6))
            } else if ko && tail.starts_with('분') || en && prefix > 0 && word(tail, "min") {
                Some((60, 3))
            } else if en && prefix > 0 && word(tail, "minutes") {
                Some((60, 7))
            } else if en && prefix > 0 && word(tail, "hours") {
                Some((3600, 5))
            } else {
                None
            };
            if let Some((factor, u)) = unit {
                let end = prefix + k + gap + u;
                let rest = &s[end..];
                let ws = spaces(rest);
                if n > 0 && n <= 10080 && (en || rest[ws..].starts_with('뒤')) {
                    return Some(Found {
                        at: now + n as i64 * factor,
                        span: start..start + end + if ko { ws + 3 } else { 0 },
                    });
                }
            }
        }
        let mut date = None;
        let mut used = 0;
        if ko || en {
            for (w, delta) in if ko {
                vec![("오늘", 0), ("내일", 1), ("모레", 2)]
            } else {
                vec![("tomorrow", 1), ("today", 0)]
            } {
                if (ko && s.starts_with(w)) || (!ko && word(s, w)) {
                    date = Some(day + delta);
                    used = w.len();
                    break;
                }
            }
            if ko && date.is_none() {
                let next = s.starts_with("다음 주 ") || s.starts_with("다음주 ");
                let p = if s.starts_with("다음 주 ") {
                    "다음 주 ".len()
                } else if s.starts_with("다음주 ") {
                    "다음주 ".len()
                } else {
                    0
                };
                for (idx, w) in [
                    "월요일",
                    "화요일",
                    "수요일",
                    "목요일",
                    "금요일",
                    "토요일",
                    "일요일",
                ]
                .iter()
                .enumerate()
                {
                    if s[p..].starts_with(w) {
                        let weekday = (day + 3).rem_euclid(7);
                        let mut delta = (idx as i64 - weekday).rem_euclid(7);
                        if next {
                            delta = 7 - weekday + idx as i64;
                        } else if delta == 0 {
                            delta = 7;
                        }
                        date = Some(day + delta);
                        used = p + w.len();
                        break;
                    }
                }
            }
            if date.is_none() {
                if let Some((m, k)) = number(s) {
                    let sep = if s[k..].starts_with('/') {
                        1
                    } else if ko && s[k..].starts_with('월') {
                        3
                    } else {
                        0
                    };
                    if sep > 0 {
                        let p = k + sep + spaces(&s[k + sep..]);
                        if let Some((d, n)) = number(&s[p..]) {
                            let end = p + n;
                            let suffix = if sep == 3 {
                                if !s[end..].starts_with('일') {
                                    continue;
                                }
                                3
                            } else {
                                0
                            };
                            let mut y = today.y;
                            let mut dt = Ymd::new(y, m, d).valid();
                            if dt.is_some_and(|d| days(d) < day) {
                                y += 1;
                                dt = Ymd::new(y, m, d).valid();
                            }
                            if let Some(dt) = dt {
                                date = Some(days(dt));
                                used = end + suffix;
                            } else {
                                continue;
                            }
                        }
                    }
                }
            }
        }
        let p = used + spaces(&s[used..]);
        if let Some((hour, min, len, ambiguous)) = time_at(&s[p..], ko, en) {
            let base = date.unwrap_or(day) * 86400;
            let mut at = base + hour as i64 * 3600 + min as i64 * 60;
            if date.is_none() {
                if ambiguous {
                    while at <= now {
                        at += 12 * 3600;
                    }
                } else if at <= now {
                    at += 86400;
                }
            } else if ambiguous && date == Some(day) && at <= now && hour < 12 {
                at += 12 * 3600;
            }
            return Some(Found {
                at,
                span: start..start + p + len,
            });
        }
        if let Some(d) = date {
            if let Some(h) = date_hour.filter(|h| *h < 24) {
                return Some(Found {
                    at: d * 86400 + h as i64 * 3600,
                    span: start..start + used,
                });
            }
        }
    }
    None
}
fn time_at(s: &str, ko: bool, en: bool) -> Option<(u32, u32, usize, bool)> {
    let (prefix, pm) = if ko && s.starts_with("오후 ") {
        (7, Some(true))
    } else if ko && s.starts_with("오전 ") {
        (7, Some(false))
    } else {
        (0, None)
    };
    let (h, n) = number(s.get(prefix..)?)?;
    let k = prefix + n;
    let mut hour = h;
    let mut min = 0;
    let mut end = k;
    let mut ambiguous = false;
    if s[k..].starts_with(':') {
        let (m, n) = number(&s[k + 1..])?;
        if n != 2 || m >= 60 || h >= 24 {
            return None;
        }
        min = m;
        end = k + 1 + n;
    } else if ko && s[k..].starts_with('시') && !s[k..].starts_with("시간") {
        if h >= 24 {
            return None;
        }
        end += 3;
        let gap = spaces(&s[end..]);
        let tail = &s[end + gap..];
        if tail.starts_with('반') {
            min = 30;
            end += gap + 3;
        } else if let Some((m, n)) = number(tail) {
            if tail[n..].starts_with('분') {
                if m >= 60 {
                    return None;
                }
                min = m;
                end += gap + n + 3;
            }
        }
        ambiguous = pm.is_none() && h > 0 && h <= 12;
        if ambiguous {
            hour %= 12;
        }
    } else if en {
        let gap = spaces(&s[k..]);
        let tail = &s[k + gap..];
        let pm = if tail.starts_with("pm") || tail.starts_with("PM") {
            true
        } else if tail.starts_with("am") || tail.starts_with("AM") {
            false
        } else {
            return None;
        };
        if !(1..=12).contains(&h) {
            return None;
        }
        hour = h % 12 + if pm { 12 } else { 0 };
        end = k + gap + 2;
    } else {
        return None;
    }
    if let Some(pm) = pm {
        if !(1..=12).contains(&h) {
            return None;
        }
        hour = h % 12 + if pm { 12 } else { 0 };
    }
    if s[end..]
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_digit() || c == ':' || c.is_ascii_alphabetic())
    {
        return None;
    }
    Some((hour, min, end, ambiguous))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn base() -> i64 {
        days(Ymd::new(2026, 11, 15)) * 86400 + 10 * 3600
    }
    #[test]
    fn recognition_table_over_forty_cases() {
        let cases = [
            ("3시", "ko", 5 * 60),
            ("오후 3시", "ko", 300),
            ("15:00", "ko", 300),
            ("3시 반", "ko", 330),
            ("내일 9시", "ko", 1380),
            ("모레", "ko", 2820),
            ("금요일", "ko", 7140),
            ("다음 주 월요일", "ko", 1380),
            ("30분 뒤", "ko", 30),
            ("2시간 뒤", "ko", 120),
            ("11/20", "ko", 7140),
            ("11월 20일", "ko", 7140),
            ("3pm", "en", 300),
            ("15:00", "en", 300),
            ("tomorrow", "en", 1380),
            ("in 30 min", "en", 30),
            ("15:00", "ja", 300),
            ("09:00", "ko", 1380),
            ("오전 3시", "ko", 1020),
            ("12시", "ko", 120),
            ("12시 반", "ko", 150),
            ("0시", "ko", 840),
            ("23시", "ko", 780),
            ("3시 15분", "ko", 315),
            ("오후 12시", "ko", 120),
            ("오전 12시", "ko", 840),
            ("12am", "en", 840),
            ("12pm", "en", 120),
            ("3 PM", "en", 300),
            ("in 2 hours", "en", 120),
            ("in 5 minutes", "en", 5),
            ("1분 뒤", "ko", 1),
            ("120분 뒤", "ko", 120),
            ("내일 오후 3시", "ko", 1740),
            ("모레 15:30", "ko", 3210),
            ("오늘 오후 3시", "ko", 300),
            ("월요일", "ko", 1380),
            ("일요일", "ko", 10020),
            ("다음주 월요일", "ko", 1380),
            ("11월20일 15:00", "ko", 7500),
            ("회의 15:00 준비", "fr", 300),
            ("회의 3시 반 출력", "ko", 330),
            ("tomorrow 15:00", "en", 1740),
            ("오늘 11:01", "ko", 61),
        ];
        for (s, lang, minutes) in cases {
            let f = parse(s, base(), lang, Some(9)).unwrap_or_else(|| panic!("{s}"));
            assert_eq!(f.at, base() + minutes * 60, "{s}");
            assert!(!s[f.span].is_empty());
        }
    }
    #[test]
    fn rejects_invalid_and_other_language_words() {
        for s in [
            "25:00",
            "15:60",
            "15:0",
            "123:00",
            "24시",
            "3시간",
            "13pm",
            "0pm",
            "11/31",
            "2월 30일",
            "0분 뒤",
            "version15:00x",
        ] {
            assert!(parse(s, base(), "ko", Some(9)).is_none(), "{s}");
        }
        assert!(parse("tomorrow", base(), "ja", Some(9)).is_none());
        assert!(parse("3시", base(), "fr", Some(9)).is_none());
    }
    #[test]
    fn dates_and_rollover() {
        assert!(parse("내일", base(), "ko", None).is_none());
        assert!(parse("내일 9시", base(), "ko", None).is_some());
        let n = days(Ymd::new(2026, 12, 31)) * 86400 + 23 * 3600;
        assert_eq!(
            parse("01:00", n, "en", Some(9)).unwrap().at,
            days(Ymd::new(2027, 1, 1)) * 86400 + 3600
        );
        let n = days(Ymd::new(2028, 2, 28)) * 86400;
        assert_eq!(
            parse("모레", n, "ko", Some(10)).unwrap().at,
            days(Ymd::new(2028, 3, 1)) * 86400 + 36000
        );
    }
    #[test]
    fn spans_are_safe_for_unicode() {
        let s = "📌 내일 오후 3시 회의";
        let f = parse(s, base(), "ko", Some(9)).unwrap();
        assert_eq!(&s[f.span], "내일 오후 3시");
    }
    #[test]
    fn next_week_does_not_mean_this_week() {
        let monday = base() + 86400;
        let f = parse("다음 주 금요일 회의", monday, "ko", Some(9)).unwrap();
        assert_eq!(f.at, monday + 11 * 86400 - 3600);
        assert_eq!(&"다음 주 금요일 회의"[f.span], "다음 주 금요일");
    }
}
