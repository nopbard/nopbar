//! The calculator row: arithmetic typed into the search field.
//!
//! + - * / ^ ( ), decimals, × and ÷. `%` after a number is a percent
//! ("15%" = 0.15, "200*15%" = 30); between two numbers it is the remainder
//! ("10 % 3" = 1). A missing ")" at the end is fine (the user is still
//! typing). Plain numbers are not shown: something must be calculated.
//!
//! Bounded: at most MAX_INPUT characters and MAX_TOKENS tokens, and
//! parentheses / exponents nest at most MAX_DEPTH deep (signs are read in a
//! loop), so no input can run the worker out of stack.
//!
//! The row also converts units ("5 km to mi", see units) and counts dates
//! ("today + 30 days", "2026-12-25 - today", see dates): `answer` tries
//! each. Picking the row copies the result as a plain number (ASCII digits,
//! "." for decimals) or, for a date, as "2026-12-25".

use super::dates::{self, DateValue, Ymd};

/// Longer input is not arithmetic anyone types.
pub const MAX_INPUT: usize = 256;
const MAX_TOKENS: usize = 128;
/// Parentheses and exponents inside each other.
const MAX_DEPTH: u32 = 32;

#[derive(Clone, Copy, Debug, PartialEq)]
enum Tok {
    Num(f64),
    Op(char),
    Open,
    Close,
}

fn lex(s: &str) -> Option<Vec<Tok>> {
    let mut out = Vec::new();
    let cs: Vec<char> = s.chars().take(MAX_INPUT + 1).collect();
    if cs.len() > MAX_INPUT {
        return None;
    }
    let mut i = 0;
    while i < cs.len() {
        let c = cs[i];
        match c {
            ' ' | '\t' => i += 1,
            '0'..='9' | '.' => {
                let start = i;
                while i < cs.len() && (cs[i].is_ascii_digit() || cs[i] == '.') {
                    i += 1;
                }
                let text: String = cs[start..i].iter().collect();
                if text == "." || text.matches('.').count() > 1 {
                    return None;
                }
                out.push(Tok::Num(text.parse().ok()?));
            }
            '+' | '-' | '*' | '/' | '^' | '%' => {
                out.push(Tok::Op(c));
                i += 1;
            }
            '×' | 'x' | 'X' if !out.is_empty() => {
                out.push(Tok::Op('*'));
                i += 1;
            }
            '÷' => {
                out.push(Tok::Op('/'));
                i += 1;
            }
            '(' => {
                out.push(Tok::Open);
                i += 1;
            }
            ')' => {
                out.push(Tok::Close);
                i += 1;
            }
            _ => return None,
        }
        if out.len() > MAX_TOKENS {
            return None;
        }
    }
    Some(out)
}

struct Parser {
    t: Vec<Tok>,
    i: usize,
    /// Something was calculated (not just a number).
    worked: bool,
    /// Parentheses / exponents open right now.
    depth: u32,
}

impl Parser {
    fn peek(&self) -> Option<Tok> {
        self.t.get(self.i).copied()
    }
    fn peek2(&self) -> Option<Tok> {
        self.t.get(self.i + 1).copied()
    }

    fn expr(&mut self) -> Option<f64> {
        let mut v = self.term()?;
        while let Some(Tok::Op(op @ ('+' | '-'))) = self.peek() {
            self.i += 1;
            let r = self.term()?;
            v = if op == '+' { v + r } else { v - r };
            self.worked = true;
        }
        Some(v)
    }

    fn term(&mut self) -> Option<f64> {
        let mut v = self.unary()?;
        while let Some(Tok::Op(op @ ('*' | '/' | '%'))) = self.peek() {
            self.i += 1;
            let r = self.unary()?;
            v = match op {
                '*' => v * r,
                '/' => v / r,
                _ => v % r,
            };
            self.worked = true;
        }
        Some(v)
    }

    /// Runs `f` one level deeper; None past MAX_DEPTH.
    fn nested(&mut self, f: impl FnOnce(&mut Self) -> Option<f64>) -> Option<f64> {
        if self.depth >= MAX_DEPTH {
            return None;
        }
        self.depth += 1;
        let v = f(self);
        self.depth -= 1;
        v
    }

    fn unary(&mut self) -> Option<f64> {
        // Signs in a loop, not by recursion.
        let mut negative = false;
        while let Some(Tok::Op(op @ ('-' | '+'))) = self.peek() {
            self.i += 1;
            if op == '-' {
                negative = !negative;
            }
        }
        let v = self.power()?;
        Some(if negative { -v } else { v })
    }

    fn power(&mut self) -> Option<f64> {
        let base = self.postfix()?;
        if self.peek() == Some(Tok::Op('^')) {
            self.i += 1;
            // Right-associative; the exponent may have its own sign.
            let e = self.nested(|p| p.unary())?;
            self.worked = true;
            return Some(base.powf(e));
        }
        Some(base)
    }

    fn postfix(&mut self) -> Option<f64> {
        let mut v = self.primary()?;
        // A percent sign not followed by an operand.
        while self.peek() == Some(Tok::Op('%')) && !matches!(self.peek2(), Some(Tok::Num(_) | Tok::Open)) {
            self.i += 1;
            v /= 100.0;
            self.worked = true;
        }
        Some(v)
    }

    fn primary(&mut self) -> Option<f64> {
        match self.peek()? {
            Tok::Num(n) => {
                self.i += 1;
                Some(n)
            }
            Tok::Open => {
                self.i += 1;
                let v = self.nested(|p| p.expr())?;
                match self.peek() {
                    Some(Tok::Close) => self.i += 1,
                    // Still typing: the closing parenthesis can wait.
                    None => {}
                    _ => return None,
                }
                Some(v)
            }
            _ => None,
        }
    }
}

fn eval_with(s: &str, need_work: bool) -> Option<f64> {
    let t = lex(s.trim())?;
    if t.is_empty() {
        return None;
    }
    let mut p = Parser { t, i: 0, worked: false, depth: 0 };
    let v = p.expr()?;
    if p.i != p.t.len() || (need_work && !p.worked) || !v.is_finite() {
        return None;
    }
    Some(if v == 0.0 { 0.0 } else { v })
}

/// The value of `s` if it is arithmetic with at least one operation.
pub fn eval(s: &str) -> Option<f64> {
    eval_with(s, true)
}

/// The value of `s` as a number or arithmetic (an amount: "5", "(2+3)").
pub fn eval_number(s: &str) -> Option<f64> {
    eval_with(s, false)
}

/// What the calculator row shows.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Value {
    Number(f64),
    /// A converted amount and its unit's symbol.
    Quantity(f64, &'static str),
    Date(Ymd),
    /// Days between two dates.
    Days(i64),
}

/// Significant digits of a converted amount (the factors are exact; more
/// would only show float noise).
const UNIT_DIGITS: i32 = 10;

/// What the text calculates, if anything: a date calculation, a unit
/// conversion or arithmetic. `today` is the day "today" means.
pub fn answer(s: &str, today: Ymd) -> Option<Value> {
    if s.chars().nth(MAX_INPUT).is_some() {
        return None;
    }
    match dates::eval(s, today) {
        Some(DateValue::Date(d)) => return Some(Value::Date(d)),
        Some(DateValue::Days(n)) => return Some(Value::Days(n)),
        None => {}
    }
    if let Some((v, sym)) = super::units::convert(s) {
        return Some(Value::Quantity(v, sym));
    }
    eval(s).map(Value::Number)
}

/// The row's text (after "= ") and what picking it copies.
pub fn shown(v: Value) -> (String, String) {
    match v {
        Value::Number(n) => (format(n), format(n)),
        Value::Quantity(n, sym) => {
            let s = format_sig(n, UNIT_DIGITS);
            (format!("{s} {sym}"), s)
        }
        Value::Date(d) => (dates::show(d), d.iso()),
        Value::Days(n) => {
            let key = if n.abs() == 1 { "search.calc.day" } else { "search.calc.days" };
            (crate::i18n::tf(key, &[("n", &n.to_string())]), n.to_string())
        }
    }
}

/// A result as people write it: no float noise, no trailing zeros.
pub fn format(v: f64) -> String {
    format_sig(v, 12)
}

/// `v` to `sig` significant digits, no trailing zeros: whole numbers in
/// full below 1e15, small values with as many decimals as their digits
/// need ("0.000000000001") down to 1e-15, exponents beyond ("1e-20",
/// "1.5e18"). The same text is shown and copied.
pub fn format_sig(v: f64, sig: i32) -> String {
    let sig = sig.clamp(1, 17);
    if v == 0.0 || !v.is_finite() {
        return if v.is_finite() { "0".into() } else { v.to_string() };
    }
    // Significant digits (not decimals) hide binary noise (0.1 + 0.2).
    let exp = v.abs().log10().floor() as i32;
    if !(-15..15).contains(&exp) {
        let s = format!("{:.*e}", (sig - 1) as usize, v);
        let (mantissa, e) = s.split_once('e').unwrap_or((&s, "0"));
        let mantissa = if mantissa.contains('.') { mantissa.trim_end_matches('0').trim_end_matches('.') } else { mantissa };
        return format!("{mantissa}e{e}");
    }
    let decimals = (sig - 1 - exp).max(0);
    let s = format!("{:.*}", decimals as usize, v);
    let s = if s.contains('.') { s.trim_end_matches('0').trim_end_matches('.').to_string() } else { s };
    if s == "-0" {
        "0".into()
    } else {
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn calc(s: &str) -> Option<String> {
        eval(s).map(format)
    }

    #[test]
    fn arithmetic() {
        assert_eq!(calc("1+2"), Some("3".into()));
        assert_eq!(calc("2+3*4"), Some("14".into()));
        assert_eq!(calc("(2+3)*4"), Some("20".into()));
        assert_eq!(calc("10 / 4"), Some("2.5".into()));
        assert_eq!(calc("2^10"), Some("1024".into()));
        assert_eq!(calc("2^3^2"), Some("512".into()));
        assert_eq!(calc("-2^2"), Some("-4".into()));
        assert_eq!(calc("2^-1"), Some("0.5".into()));
        assert_eq!(calc("0.1+0.2"), Some("0.3".into()));
        assert_eq!(calc("1.5*2"), Some("3".into()));
        assert_eq!(calc("3 × 4 ÷ 2"), Some("6".into()));
        assert_eq!(calc("-(3-5)"), Some("2".into()));
        assert_eq!(calc("1/3"), Some("0.333333333333".into()));
    }

    #[test]
    fn percent_and_remainder() {
        assert_eq!(calc("15%"), Some("0.15".into()));
        assert_eq!(calc("200*15%"), Some("30".into()));
        assert_eq!(calc("10 % 3"), Some("1".into()));
        assert_eq!(calc("10%(4)"), Some("2".into()));
    }

    #[test]
    fn still_typing() {
        assert_eq!(calc("(1+2"), Some("3".into()));
        assert_eq!(calc("1+"), None);
        assert_eq!(calc("("), None);
    }

    #[test]
    fn not_arithmetic() {
        for s in ["", "42", "-5", "(7)", "notepad", "1+a", "1..2", "2(3)", "1)", "x2", "1/0", "0/0"] {
            assert_eq!(calc(s), None, "{s}");
        }
    }

    #[test]
    fn hostile_input_is_refused_not_crashing() {
        // A wall of signs (read in a loop) is fine while short...
        assert_eq!(calc("--1+1"), Some("2".into()));
        assert_eq!(calc(&format!("{}1+1", "-".repeat(100))), Some("2".into()));
        // ...and refused when long, as are deep parentheses and exponents.
        assert_eq!(calc(&format!("{}1+1", "-".repeat(100_000))), None);
        assert_eq!(calc(&format!("{}1+1{}", "(".repeat(100_000), ")".repeat(100_000))), None);
        assert_eq!(calc(&format!("{}1+1{}", "(".repeat(40), ")".repeat(40))), None);
        assert_eq!(calc(&format!("{}1+1{}", "(".repeat(10), ")".repeat(10))), Some("2".into()));
        assert_eq!(calc(&format!("{}1", "2^".repeat(60))), None);
        assert_eq!(calc(&format!("{}1", "1+".repeat(100))), None);
    }

    const TODAY: Ymd = Ymd::new(2026, 11, 8);

    #[test]
    fn answers_dates_units_then_arithmetic() {
        assert_eq!(answer("1+2", TODAY), Some(Value::Number(3.0)));
        assert_eq!(answer("1 in to cm", TODAY), Some(Value::Quantity(2.54, "cm")));
        assert_eq!(answer("today + 30 days", TODAY), Some(Value::Date(Ymd::new(2026, 12, 8))));
        assert_eq!(answer("2026-12-25 - today", TODAY), Some(Value::Days(47)));
        // A written-out date is a date, not a subtraction (2026-12-25 = 1989).
        assert_eq!(answer("2026-12-25", TODAY), Some(Value::Date(Ymd::new(2026, 12, 25))));
        // Text that only starts like a date is arithmetic (or an amount).
        assert_eq!(answer("2026-13-01", TODAY), Some(Value::Number(2012.0)));
        assert_eq!(answer("2026-12-25+1", TODAY), Some(Value::Number(1990.0)));
        assert_eq!(answer("2026/12/25*2", TODAY).map(|v| shown(v).1), Some("13.5066666667".into()));
        assert_eq!(answer("2026-12-25 + 3", TODAY), Some(Value::Number(1992.0)));
        assert_eq!(answer("2026-12-25 km to m", TODAY), Some(Value::Quantity(1_989_000.0, "m")));
        assert_eq!(answer("2026 - 12 - 25", TODAY), Some(Value::Number(1989.0)));
        // "30 days to h" is a conversion: no date in it.
        assert_eq!(answer("30 days to h", TODAY), Some(Value::Quantity(720.0, "h")));
    }

    #[test]
    fn no_row_answers() {
        for s in ["", "42", "notepad", "today", "오늘", "5 km", "5 km to kg", "km to mi", "today + 30", "chrome 2", "2 chrome"] {
            assert_eq!(answer(s, TODAY), None, "{s}");
        }
    }

    #[test]
    fn answers_are_bounded() {
        let long = format!("5 km to mi{}", " ".repeat(MAX_INPUT));
        assert_eq!(answer(&long, TODAY), None);
        assert_eq!(answer(&format!("today{}", " + 1 day".repeat(30)), TODAY), None);
        assert_eq!(answer(&format!("{}5 km to mi", "(".repeat(40)), TODAY), None);
        assert_eq!(answer(&format!("{}5 km to mi", "(".repeat(5)), TODAY).map(|v| shown(v).1), Some("3.106855961".into()));
    }

    #[test]
    fn shown_and_copied() {
        let _l = crate::i18n::test_lock("ko");
        assert_eq!(shown(Value::Number(0.1 + 0.2)), ("0.3".into(), "0.3".into()));
        assert_eq!(shown(Value::Quantity(5.0 * 1000.0 / 1609.344, "mi")), ("3.106855961 mi".into(), "3.106855961".into()));
        assert_eq!(shown(Value::Quantity(-40.0, "°F")), ("-40 °F".into(), "-40".into()));
        assert_eq!(shown(Value::Days(47)), ("47일".into(), "47".into()));
        let (text, copy) = shown(Value::Date(Ymd::new(2026, 12, 8)));
        assert_eq!(copy, "2026-12-08");
        assert!(text.ends_with("(화)"), "{text}");
        let _l2 = { drop(_l); crate::i18n::test_lock("en") };
        assert_eq!(shown(Value::Days(1)).0, "1 day");
        assert_eq!(shown(Value::Days(-47)), ("-47 days".into(), "-47".into()));
    }

    #[test]
    fn small_and_large_values_keep_their_digits() {
        let _l = crate::i18n::test_lock("en");
        let q = |s: &str| answer(s, TODAY).map(shown);
        assert_eq!(q("1 B to TB"), Some(("0.000000000001 TB".into(), "0.000000000001".into())));
        assert_eq!(q("0.00000001 mm to km"), Some(("0.00000000000001 km".into(), "0.00000000000001".into())));
        assert_eq!(q("1 B to TiB").map(|v| v.1), Some("0.0000000000009094947018".into()));
        assert_eq!(q("0.000001 mm to km").map(|v| v.1), Some("0.000000000001".into()));
        assert_eq!(q("1 mm to mi").map(|v| v.1), Some("0.0000006213711922".into()));
        assert_eq!(format_sig(-1e-12, 10), "-0.000000000001", "the sign stays");
        assert_eq!(format_sig(1e-20, 10), "1e-20");
        assert_eq!(format_sig(-2.5e-18, 10), "-2.5e-18");
        assert_eq!(format_sig(1.0000000000000001e18, 10), "1e18");
        assert_eq!(format_sig(1.23456789012345e20, 10), "1.23456789e20");
        // Rounding up across a power of ten.
        assert_eq!(format_sig(9.99999999999, 10), "10");
        assert_eq!(format_sig(0.0999999999999, 10), "0.1");
        assert_eq!(format_sig(0.0, 10), "0");
        assert_eq!(format_sig(-0.0, 10), "0");
        // Whole numbers in full; arithmetic as before.
        assert_eq!(format_sig(1_099_511_627_776.0, 10), "1099511627776");
        assert_eq!(format(0.001 + 0.002), "0.003");
        assert_eq!(format(1e-7 / 3.0), "0.0000000333333333333");
    }

    #[test]
    fn formatting() {
        assert_eq!(format(-0.0), "0");
        assert_eq!(format(1234567.0), "1234567");
        assert_eq!(format(1e20), "1e20");
        assert_eq!(format(-2.5), "-2.5");
    }
}
