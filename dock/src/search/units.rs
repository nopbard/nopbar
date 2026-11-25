//! Unit conversions on the calculator row: "5 km to mi", "5km in mi",
//! "5 km mi", "5킬로를 마일로".
//!
//! Length (CSS px / pt / rem too: 96px = 1in, 1rem = 16px), mass,
//! temperature, data (KB = 1000 B, KiB = 1024 B) and time. Every factor is
//! exact (the definitions, not measurements), all offline. Anything not
//! understood gives no row: the text then goes on as arithmetic or a search.
//!
//! Bounded: the text is at most calc::MAX_INPUT characters (checked by the
//! caller), the amount is the calculator's own bounded parse and the units
//! part is at most MAX_WORDS words.

/// "5 km to mi": two units and a connecting word.
const MAX_WORDS: usize = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Dim {
    Length,
    Mass,
    Temp,
    Data,
    Time,
}

/// base = (value + offset) * factor; the base units are m, g, K, B and s.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Unit {
    /// Shown after the result.
    sym: &'static str,
    dim: Dim,
    factor: f64,
    offset: f64,
}

const fn u(sym: &'static str, dim: Dim, factor: f64) -> Unit {
    Unit { sym, dim, factor, offset: 0.0 }
}

const INCH: f64 = 0.0254;
const PX: f64 = INCH / 96.0;

const MM: Unit = u("mm", Dim::Length, 0.001);
const CM: Unit = u("cm", Dim::Length, 0.01);
const M: Unit = u("m", Dim::Length, 1.0);
const KM: Unit = u("km", Dim::Length, 1000.0);
const IN: Unit = u("in", Dim::Length, INCH);
const FT: Unit = u("ft", Dim::Length, 0.3048);
const YD: Unit = u("yd", Dim::Length, 0.9144);
const MI: Unit = u("mi", Dim::Length, 1609.344);
const PXU: Unit = u("px", Dim::Length, PX);
const PT: Unit = u("pt", Dim::Length, INCH / 72.0);
const REM: Unit = u("rem", Dim::Length, 16.0 * PX);

const G: Unit = u("g", Dim::Mass, 1.0);
const KG: Unit = u("kg", Dim::Mass, 1000.0);
const LB: Unit = u("lb", Dim::Mass, 453.592_37);
const OZ: Unit = u("oz", Dim::Mass, 28.349_523_125);

const C: Unit = Unit { sym: "°C", dim: Dim::Temp, factor: 1.0, offset: 273.15 };
const F: Unit = Unit { sym: "°F", dim: Dim::Temp, factor: 5.0 / 9.0, offset: 459.67 };
const K: Unit = u("K", Dim::Temp, 1.0);

const B: Unit = u("B", Dim::Data, 1.0);
const KB: Unit = u("KB", Dim::Data, 1e3);
const MB: Unit = u("MB", Dim::Data, 1e6);
const GB: Unit = u("GB", Dim::Data, 1e9);
const TB: Unit = u("TB", Dim::Data, 1e12);
const KIB: Unit = u("KiB", Dim::Data, 1024.0);
const MIB: Unit = u("MiB", Dim::Data, 1_048_576.0);
const GIB: Unit = u("GiB", Dim::Data, 1_073_741_824.0);
const TIB: Unit = u("TiB", Dim::Data, 1_099_511_627_776.0);

const S: Unit = u("s", Dim::Time, 1.0);
const MIN: Unit = u("min", Dim::Time, 60.0);
const H: Unit = u("h", Dim::Time, 3600.0);
const DAY: Unit = u("d", Dim::Time, 86400.0);

/// Names, lowercase. A name may mean two units ("킬로" is km or kg): the
/// other unit decides.
const NAMES: &[(&str, &[Unit])] = &[
    ("mm", &[MM]),
    ("millimeter", &[MM]),
    ("millimeters", &[MM]),
    ("millimetre", &[MM]),
    ("millimetres", &[MM]),
    ("밀리미터", &[MM]),
    ("밀리", &[MM]),
    ("cm", &[CM]),
    ("centimeter", &[CM]),
    ("centimeters", &[CM]),
    ("centimetre", &[CM]),
    ("centimetres", &[CM]),
    ("센티미터", &[CM]),
    ("센티", &[CM]),
    ("센치", &[CM]),
    ("m", &[M]),
    ("meter", &[M]),
    ("meters", &[M]),
    ("metre", &[M]),
    ("metres", &[M]),
    ("미터", &[M]),
    ("km", &[KM]),
    ("kilometer", &[KM]),
    ("kilometers", &[KM]),
    ("kilometre", &[KM]),
    ("kilometres", &[KM]),
    ("킬로미터", &[KM]),
    ("키로미터", &[KM]),
    ("킬로", &[KM, KG]),
    ("키로", &[KM, KG]),
    ("in", &[IN]),
    ("inch", &[IN]),
    ("inches", &[IN]),
    ("\"", &[IN]),
    ("인치", &[IN]),
    ("ft", &[FT]),
    ("foot", &[FT]),
    ("feet", &[FT]),
    ("'", &[FT]),
    ("피트", &[FT]),
    ("yd", &[YD]),
    ("yard", &[YD]),
    ("yards", &[YD]),
    ("야드", &[YD]),
    ("mi", &[MI]),
    ("mile", &[MI]),
    ("miles", &[MI]),
    ("마일", &[MI]),
    ("px", &[PXU]),
    ("pixel", &[PXU]),
    ("pixels", &[PXU]),
    ("픽셀", &[PXU]),
    ("pt", &[PT]),
    ("rem", &[REM]),
    ("g", &[G]),
    ("gram", &[G]),
    ("grams", &[G]),
    ("그램", &[G]),
    ("kg", &[KG]),
    ("kilogram", &[KG]),
    ("kilograms", &[KG]),
    ("킬로그램", &[KG]),
    ("키로그램", &[KG]),
    ("lb", &[LB]),
    ("lbs", &[LB]),
    ("pound", &[LB]),
    ("pounds", &[LB]),
    ("파운드", &[LB]),
    ("oz", &[OZ]),
    ("ounce", &[OZ]),
    ("ounces", &[OZ]),
    ("온스", &[OZ]),
    ("c", &[C]),
    ("°c", &[C]),
    ("℃", &[C]),
    ("celsius", &[C]),
    ("섭씨", &[C]),
    ("f", &[F]),
    ("°f", &[F]),
    ("℉", &[F]),
    ("fahrenheit", &[F]),
    ("화씨", &[F]),
    ("k", &[K]),
    ("kelvin", &[K]),
    ("켈빈", &[K]),
    ("b", &[B]),
    ("byte", &[B]),
    ("bytes", &[B]),
    ("바이트", &[B]),
    ("kb", &[KB]),
    ("킬로바이트", &[KB]),
    ("mb", &[MB]),
    ("메가바이트", &[MB]),
    ("메가", &[MB]),
    ("gb", &[GB]),
    ("기가바이트", &[GB]),
    ("기가", &[GB]),
    ("tb", &[TB]),
    ("테라바이트", &[TB]),
    ("테라", &[TB]),
    ("kib", &[KIB]),
    ("mib", &[MIB]),
    ("gib", &[GIB]),
    ("tib", &[TIB]),
    ("s", &[S]),
    ("sec", &[S]),
    ("secs", &[S]),
    ("second", &[S]),
    ("seconds", &[S]),
    ("초", &[S]),
    ("min", &[MIN]),
    ("mins", &[MIN]),
    ("minute", &[MIN]),
    ("minutes", &[MIN]),
    ("분", &[MIN]),
    ("h", &[H]),
    ("hr", &[H]),
    ("hrs", &[H]),
    ("hour", &[H]),
    ("hours", &[H]),
    ("시간", &[H]),
    ("d", &[DAY]),
    ("day", &[DAY]),
    ("days", &[DAY]),
    ("일", &[DAY]),
];

/// Words between the two units.
const JOINS: &[&str] = &["to", "in", "as", "->", "→", "=", "into"];

/// Korean particles after the first unit ("킬로를") and the second ("마일로").
const FROM_PARTICLES: &[&str] = &["에서", "을", "를", "은", "는", "이", "가"];
const TO_PARTICLES: &[&str] = &["으로", "로"];

fn named(word: &str) -> Option<&'static [Unit]> {
    let w = word.to_lowercase();
    NAMES.iter().find(|(n, _)| *n == w).map(|(_, u)| *u)
}

/// A unit word, maybe with one of `particles` after it. The bare word wins
/// ("킬로" itself ends in "로").
fn unit_word(word: &str, particles: &[&str]) -> Option<&'static [Unit]> {
    named(word).or_else(|| particles.iter().find_map(|p| word.strip_suffix(p).filter(|w| !w.is_empty()).and_then(named)))
}

/// Where the amount ends: the first letter (or degree sign, quote) that is
/// not the calculator's "x" between two numbers ("2x3 km", "2 X (1+2) m").
fn split_amount(s: &str) -> (&str, &str) {
    let cs: Vec<(usize, char)> = s.char_indices().collect();
    let times = |k: usize| {
        let before = cs[..k].iter().rev().map(|&(_, c)| c).find(|c| !c.is_whitespace());
        let after = cs[k + 1..].iter().map(|&(_, c)| c).find(|c| !c.is_whitespace());
        matches!(cs[k].1, 'x' | 'X')
            && before.is_some_and(|c| c.is_ascii_digit() || matches!(c, ')' | '.' | '%'))
            && after.is_some_and(|c| c.is_ascii_digit() || matches!(c, '(' | '.' | '-' | '+'))
    };
    let at = (0..cs.len())
        .find(|&k| (cs[k].1.is_alphabetic() || matches!(cs[k].1, '°' | '℃' | '℉' | '"' | '\'' | '→')) && !times(k))
        .map_or(s.len(), |k| cs[k].0);
    s.split_at(at)
}

/// A conversion: the value in the target unit and the unit's symbol.
pub fn convert(s: &str) -> Option<(f64, &'static str)> {
    let (amount, rest) = split_amount(s.trim());
    if amount.trim().is_empty() {
        return None;
    }
    let v = super::calc::eval_number(amount)?;
    // "->" and "→" may be glued to the units ("km->mi").
    let rest = rest.replace("->", " -> ").replace('→', " → ");
    let words: Vec<&str> = rest.split_whitespace().take(MAX_WORDS + 1).collect();
    let (from, to) = match words.as_slice() {
        [a, b] => (*a, *b),
        [a, j, b] if JOINS.contains(&j.to_lowercase().as_str()) => (*a, *b),
        _ => return None,
    };
    let from = unit_word(from, FROM_PARTICLES)?;
    let to = unit_word(to, TO_PARTICLES)?;
    // The one pair of the same kind ("킬로를 마일로": km).
    let mut pairs = from.iter().flat_map(|a| to.iter().filter(move |b| b.dim == a.dim).map(move |b| (*a, *b)));
    let (a, b) = pairs.next()?;
    if pairs.next().is_some() {
        return None;
    }
    let scaled = (v + a.offset) * a.factor / b.factor;
    let mut out = scaled - b.offset;
    // Offsets cancel (32 °F - 459.67 ... = 5.7e-14 °C, not 0): round at
    // 12 digits of the larger term, so only the noise goes.
    let scale = scaled.abs().max(b.offset.abs());
    let q = 10f64.powi(scale.log10().floor() as i32 - 12);
    if q > 0.0 && q.is_finite() {
        out = (out / q).round() * q;
    }
    out.is_finite().then_some((out, b.sym))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::calc::format_sig;

    fn conv(s: &str) -> Option<String> {
        convert(s).map(|(v, sym)| format!("{} {sym}", format_sig(v, 10)))
    }

    #[test]
    fn length_mass_time() {
        assert_eq!(conv("5 km to mi"), Some("3.106855961 mi".into()));
        assert_eq!(conv("1 mi to km"), Some("1.609344 km".into()));
        assert_eq!(conv("1 in to cm"), Some("2.54 cm".into()));
        assert_eq!(conv("3 ft in in"), Some("36 in".into()));
        assert_eq!(conv("1 yd to ft"), Some("3 ft".into()));
        assert_eq!(conv("1500 mm to m"), Some("1.5 m".into()));
        assert_eq!(conv("1 kg to lb"), Some("2.204622622 lb".into()));
        assert_eq!(conv("1 lb to oz"), Some("16 oz".into()));
        assert_eq!(conv("2500 g to kg"), Some("2.5 kg".into()));
        assert_eq!(conv("90 min to h"), Some("1.5 h".into()));
        assert_eq!(conv("1 day to s"), Some("86400 s".into()));
        assert_eq!(conv("2 hours to minutes"), Some("120 min".into()));
    }

    #[test]
    fn temperature_has_offsets() {
        assert_eq!(conv("100 c to f"), Some("212 °F".into()));
        assert_eq!(conv("-40 °C to °F"), Some("-40 °F".into()));
        assert_eq!(conv("32 f to c"), Some("0 °C".into()));
        assert_eq!(conv("0 c to k"), Some("273.15 K".into()));
        assert_eq!(conv("0 K to °F"), Some("-459.67 °F".into()));
        assert_eq!(conv("98.6℉ to ℃"), Some("37 °C".into()));
        // Offsets that cancel give 0, not float noise.
        assert_eq!(conv("273.15 K to C"), Some("0 °C".into()));
        assert_eq!(conv("-17.7777777778 c to f"), Some("0 °F".into()));
        assert_eq!(conv("0.000001 c to k"), Some("273.150001 K".into()));
        // Offsets, not just factors: 10 °C is not 10 K away from 0.
        assert_eq!(conv("10 c to k"), Some("283.15 K".into()));
    }

    #[test]
    fn data_binary_and_decimal() {
        assert_eq!(conv("1 GB to MB"), Some("1000 MB".into()));
        assert_eq!(conv("1 GiB to MiB"), Some("1024 MiB".into()));
        assert_eq!(conv("1 GiB to MB"), Some("1073.741824 MB".into()));
        assert_eq!(conv("1 TB to GiB"), Some("931.3225746 GiB".into()));
        assert_eq!(conv("1 KiB to B"), Some("1024 B".into()));
        assert_eq!(conv("1 KB to B"), Some("1000 B".into()));
        assert_eq!(conv("1 TiB to B"), Some("1099511627776 B".into()));
    }

    #[test]
    fn css_units() {
        assert_eq!(conv("32px to rem"), Some("2 rem".into()));
        assert_eq!(conv("1.5 rem to px"), Some("24 px".into()));
        assert_eq!(conv("12 pt to px"), Some("16 px".into()));
        assert_eq!(conv("1 in to px"), Some("96 px".into()));
    }

    #[test]
    fn forms() {
        for s in ["5 km to mi", "5km in mi", "5 km mi", "5km mi", "5 KM TO MI", "5 km -> mi", "5km->mi", "5 km → mi", "5 kilometers to miles", "(2+3) km to mi", "10/2 km to mi"] {
            assert_eq!(conv(s), Some("3.106855961 mi".into()), "{s}");
        }
        // Korean words and particles.
        assert_eq!(conv("5킬로를 마일로"), Some("3.106855961 mi".into()));
        assert_eq!(conv("5킬로미터를 마일로"), Some("3.106855961 mi".into()));
        assert_eq!(conv("1킬로를 파운드로"), Some("2.204622622 lb".into()), "킬로 is kg next to pounds");
        assert_eq!(conv("10 마일을 킬로로"), Some("16.09344 km".into()), "킬로로: the bare word wins");
        assert_eq!(conv("100 섭씨 화씨"), Some("212 °F".into()));
        assert_eq!(conv("2시간을 분으로"), Some("120 min".into()));
        assert_eq!(conv("1기가를 메가로"), Some("1000 MB".into()));
        // The calculator's multiplication signs in the amount.
        for s in ["2x3 km to m", "2X3 km to m", "2 x 3 km to m", "2*3 km to m", "2×3 km to m", "2 x (1+2) km to m"] {
            assert_eq!(conv(s).as_deref(), Some("6000 m"), "{s}");
        }
        assert_eq!(conv("2 x km to m"), None, "x before a unit is not multiplication");
        // "in" is inches as a unit and a joining word between.
        assert_eq!(conv("12 in in ft"), Some("1 ft".into()));
        assert_eq!(conv("12 in ft"), Some("1 ft".into()));
    }

    #[test]
    fn no_row() {
        for s in [
            "5 km",
            "km to mi",
            "5 km to kg",
            "5 km to parsec",
            "5 parsec to km",
            "5 km to mi to m",
            "5 km and mi",
            "5 kmto mi",
            "abc",
            "5 c to km",
            "5 to mi",
            "notepad",
            "5 - km to mi",
        ] {
            assert_eq!(conv(s), None, "{s}");
        }
    }

    #[test]
    fn long_input_is_bounded() {
        let many = format!("5 km {}", "to mi ".repeat(10_000));
        assert_eq!(conv(&many), None);
        assert_eq!(conv(&format!("{}5 km to mi", "(".repeat(100))), None);
    }
}
