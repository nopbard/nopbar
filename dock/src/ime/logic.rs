//! The input indicator's and the per-app memory's decisions, free of
//! Win32 (unit tested): what the cell shows for a language and IME
//! conversion state, how to switch native / Latin input for each IME, how
//! an app is keyed, and which of rule / memory / nothing applies.

/// IME_CMODE_NATIVE: Hangul, kana or Chinese input (else alphanumeric).
pub const CMODE_NATIVE: u32 = 0x0001;
/// IME_CMODE_KATAKANA (with NATIVE: katakana instead of hiragana).
pub const CMODE_KATAKANA: u32 = 0x0002;
/// IME_CMODE_FULLSHAPE.
pub const CMODE_FULLSHAPE: u32 = 0x0008;

/// The writing systems the indicator knows an IME for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Script {
    Korean,
    Japanese,
    Chinese,
    /// Everything else: the layout's short name (EN, DE, ...).
    Other,
}

/// The script of a keyboard layout's language (the HKL's low word).
pub fn script_of(langid: u16) -> Script {
    match langid & 0x03FF {
        0x12 => Script::Korean,
        0x11 => Script::Japanese,
        0x04 => Script::Chinese,
        _ => Script::Other,
    }
}

/// What an IME window answered: open status and conversion mode.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct ImeState {
    pub open: bool,
    pub conv: u32,
}

impl ImeState {
    /// Native input (Hangul, kana, Chinese) is on.
    pub fn native(self) -> bool {
        self.open && self.conv & CMODE_NATIVE != 0
    }
}

/// What the foreground window types with right now.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Shown {
    /// The keyboard layout's language (0 = unknown).
    pub lang: u16,
    /// Its short name ("EN", "KO").
    pub short: String,
    /// The IME's state (CJK layouts whose IME answered).
    pub ime: Option<ImeState>,
    /// The window runs above our rights (UIPI): shown, never changed.
    pub locked: bool,
}

impl Shown {
    pub fn script(&self) -> Script {
        script_of(self.lang)
    }

    /// Native input is on (None: not an IME layout, or it did not answer).
    pub fn native(&self) -> Option<bool> {
        match self.script() {
            Script::Other => None,
            _ => self.ime.map(|s| s.native()),
        }
    }
}

/// The cell's text: 가 / A (Korean), あ / カ / A (Japanese), 中 / 英
/// (Chinese), else the layout's short name. An IME that did not answer
/// shows the layout's short name too.
pub fn label(s: &Shown) -> String {
    let short = || if s.short.is_empty() { "?".to_string() } else { s.short.clone() };
    let Some(ime) = s.ime else { return short() };
    match s.script() {
        Script::Korean => if ime.native() { "가" } else { "A" }.into(),
        Script::Japanese if ime.native() && ime.conv & CMODE_KATAKANA != 0 => "カ".into(),
        Script::Japanese => if ime.native() { "あ" } else { "A" }.into(),
        Script::Chinese => if ime.native() { "中" } else { "英" }.into(),
        Script::Other => short(),
    }
}

/// The text key naming the mode (for screen readers); None: a plain
/// layout (named by its language instead).
pub fn mode_key(s: &Shown) -> Option<&'static str> {
    let ime = s.ime?;
    Some(match s.script() {
        Script::Korean => if ime.native() { "input.mode.hangul" } else { "input.mode.english" },
        Script::Japanese if ime.native() && ime.conv & CMODE_KATAKANA != 0 => "input.mode.katakana",
        Script::Japanese => if ime.native() { "input.mode.hiragana" } else { "input.mode.alphanumeric" },
        Script::Chinese => if ime.native() { "input.mode.chinese" } else { "input.mode.english" },
        Script::Other => return None,
    })
}

/// "en-US" -> "EN", "zh-Hant-TW" -> "ZH"; "" for nothing usable.
pub fn short_from_locale(name: &str) -> String {
    let lang = name.split(['-', '_']).next().unwrap_or("");
    if lang.is_empty() || !lang.chars().all(|c| c.is_ascii_alphabetic()) {
        return String::new();
    }
    lang.to_ascii_uppercase()
}

/// How to get native (`want`) or Latin input from IME state `cur`:
/// (open status to set, conversion mode to set). Japanese IMEs switch to
/// Latin by closing (半角英数); Korean and Chinese keep the IME open and
/// flip the native bit (what the 한/영 and Shift keys do).
pub fn plan(script: Script, cur: ImeState, want: bool) -> (Option<bool>, Option<u32>) {
    if cur.native() == want {
        return (None, None);
    }
    match script {
        Script::Other => (None, None),
        Script::Japanese if !want => (Some(false), None),
        Script::Japanese => {
            // Hiragana, full width, unless it already had a kana mode.
            let conv = if cur.conv & CMODE_NATIVE != 0 { cur.conv } else { cur.conv | CMODE_NATIVE | CMODE_FULLSHAPE };
            ((!cur.open).then_some(true), (conv != cur.conv).then_some(conv))
        }
        Script::Korean | Script::Chinese => {
            let conv = if want { cur.conv | CMODE_NATIVE } else { cur.conv & !CMODE_NATIVE };
            let open = (want && !cur.open).then_some(true);
            (open, (conv != cur.conv).then_some(conv))
        }
    }
}

// ---- apps ---------------------------------------------------------------------

/// Who an app is: its AUMID (packaged apps, and apps that set one), else
/// its exe path. Both lowercase.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct AppIdent {
    pub aumid: Option<String>,
    pub exe: Option<String>,
}

impl AppIdent {
    pub fn new(aumid: Option<&str>, exe: Option<&str>) -> Self {
        let clean = |s: Option<&str>| s.map(|s| s.trim().to_lowercase()).filter(|s| !s.is_empty());
        Self { aumid: clean(aumid), exe: clean(exe) }
    }

    /// The memory's key ("aumid:..." first, else "exe:..."); None for a
    /// window we cannot tell (nothing is kept for it).
    pub fn key(&self) -> Option<String> {
        match (&self.aumid, &self.exe) {
            (Some(a), _) => Some(format!("aumid:{a}")),
            (None, Some(e)) => Some(format!("exe:{e}")),
            _ => None,
        }
    }

    /// Rule `app` names this app: "aumid:<id>", "exe:<full path>" or
    /// "name:<file name>" (any exe called so: the suggestions, whose
    /// install folders differ per PC). Case does not matter.
    pub fn matches(&self, app: &str) -> bool {
        let app = app.trim().to_lowercase();
        if let Some(a) = app.strip_prefix("aumid:") {
            return self.aumid.as_deref() == Some(a);
        }
        if let Some(e) = app.strip_prefix("exe:") {
            return self.exe.as_deref() == Some(e);
        }
        if let Some(n) = app.strip_prefix("name:") {
            return self.exe.as_deref().and_then(|e| e.rsplit(['\\', '/']).next()) == Some(n);
        }
        false
    }
}

// ---- rules ----------------------------------------------------------------------

/// What a per-app rule does.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RuleMode {
    /// Always native (Hangul, kana, Chinese) when the app comes to the front.
    Native,
    /// Always English (Latin).
    Alpha,
    /// The last mode it had, even with the global switch off.
    Remember,
    /// Never touched, never remembered.
    Ignore,
}

/// Config values, in the settings' cycling order.
pub const RULE_MODES: [(&str, RuleMode); 4] =
    [("alpha", RuleMode::Alpha), ("native", RuleMode::Native), ("remember", RuleMode::Remember), ("ignore", RuleMode::Ignore)];

impl RuleMode {
    /// Lenient: an unknown value (a typo, a newer build's) does nothing.
    pub fn parse(s: &str) -> RuleMode {
        RULE_MODES.iter().find(|(n, _)| n.eq_ignore_ascii_case(s.trim())).map(|(_, m)| *m).unwrap_or(RuleMode::Ignore)
    }

    pub fn name(self) -> &'static str {
        RULE_MODES.iter().find(|(_, m)| *m == self).map(|(n, _)| *n).unwrap_or("ignore")
    }

    /// The next mode in the settings' order (a click on the rule's button).
    pub fn next(self) -> RuleMode {
        let i = RULE_MODES.iter().position(|(_, m)| *m == self).unwrap_or(0);
        RULE_MODES[(i + 1) % RULE_MODES.len()].1
    }
}

/// The first rule naming the app (rules are checked in their order).
pub fn rule_for<R>(rules: &[R], app: &AppIdent, name: impl Fn(&R) -> (&str, &str)) -> Option<RuleMode> {
    rules.iter().map(&name).find(|(a, _)| app.matches(a)).map(|(_, m)| RuleMode::parse(m))
}

/// What to put the app in when it comes to the front: a rule wins over
/// the memory; the memory counts only where remembering applies. None:
/// leave it alone.
pub fn decide(rule: Option<RuleMode>, remember_on: bool, remembered: Option<bool>) -> Option<bool> {
    match rule {
        Some(RuleMode::Native) => Some(true),
        Some(RuleMode::Alpha) => Some(false),
        Some(RuleMode::Ignore) => None,
        Some(RuleMode::Remember) => remembered,
        None if remember_on => remembered,
        None => None,
    }
}

/// Whether the app's mode is kept when the user leaves it (or changes it).
pub fn records(rule: Option<RuleMode>, remember_on: bool) -> bool {
    match rule {
        Some(RuleMode::Remember) => true,
        Some(_) => false,
        None => remember_on,
    }
}

/// How long after a restore a different mode counts as the user saying no.
pub const OVERRIDE_MS: u64 = 1000;

/// A restore we made: when (ms) and to what.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Restored {
    pub at_ms: u64,
    pub native: bool,
}

/// What to do with mode `observed` seen at `now_ms` in an app.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Observation {
    /// Keep it as the app's mode.
    pub store: Option<bool>,
    /// The user switched back right after our restore: never fight it
    /// (no further restore in this visit; it is the new remembered mode).
    pub user_override: bool,
}

pub fn observe(recording: bool, restored: Option<Restored>, now_ms: u64, observed: bool) -> Observation {
    let user_override = restored.is_some_and(|r| r.native != observed && now_ms.saturating_sub(r.at_ms) <= OVERRIDE_MS);
    Observation { store: recording.then_some(observed), user_override }
}

/// Right before a restore is applied: what to do, from the native mode
/// seen at activation, the one seen now, and whether the window's layout
/// changed meanwhile.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Before {
    /// Set `want`.
    Apply,
    /// It is in `want` already: nothing to do.
    AlreadyThere,
    /// The user changed the mode (or the layout) while the restore
    /// waited: their choice stands.
    UserChanged,
    /// The IME does not answer now: hands off.
    Unknown,
}

pub fn before_restore(at_activation: Option<bool>, now: Option<bool>, layout_changed: bool, want: bool) -> Before {
    if layout_changed {
        return Before::UserChanged;
    }
    match now {
        None => Before::Unknown,
        Some(n) if n == want => Before::AlreadyThere,
        Some(n) if at_activation.is_some_and(|a| a != n) => Before::UserChanged,
        Some(_) => Before::Apply,
    }
}

/// Apps worth a one-click rule (offered, never applied by themselves):
/// (rule app, display name or text key, mode). Terminals and code
/// editors type commands: English.
pub const SUGGESTIONS: [(&str, &str, RuleMode); 5] = [
    ("name:windowsterminal.exe", "Windows Terminal", RuleMode::Alpha),
    ("name:code.exe", "Visual Studio Code", RuleMode::Alpha),
    ("name:powershell.exe", "Windows PowerShell", RuleMode::Alpha),
    ("name:pwsh.exe", "PowerShell", RuleMode::Alpha),
    ("name:cmd.exe", "settings.input.suggest.cmd", RuleMode::Alpha),
];

#[cfg(test)]
mod tests {
    use super::*;

    fn ime(open: bool, conv: u32) -> Option<ImeState> {
        Some(ImeState { open, conv })
    }

    fn shown(lang: u16, ime: Option<ImeState>) -> Shown {
        Shown { lang, short: "XX".into(), ime, locked: false }
    }

    #[test]
    fn labels_per_language_and_conversion_flags() {
        // Korean (0x0412): the native bit is Hangul.
        assert_eq!(label(&shown(0x0412, ime(true, CMODE_NATIVE | CMODE_FULLSHAPE))), "가");
        assert_eq!(label(&shown(0x0412, ime(true, 0))), "A");
        assert_eq!(label(&shown(0x0412, ime(false, CMODE_NATIVE))), "A", "a closed IME types Latin");
        // Japanese (0x0411): hiragana, katakana, alphanumeric.
        assert_eq!(label(&shown(0x0411, ime(true, CMODE_NATIVE | CMODE_FULLSHAPE))), "あ");
        assert_eq!(label(&shown(0x0411, ime(true, CMODE_NATIVE | CMODE_KATAKANA | CMODE_FULLSHAPE))), "カ");
        assert_eq!(label(&shown(0x0411, ime(false, CMODE_NATIVE))), "A");
        // Chinese, simplified (0x0804) and traditional (0x0404).
        assert_eq!(label(&shown(0x0804, ime(true, CMODE_NATIVE))), "中");
        assert_eq!(label(&shown(0x0804, ime(true, 0))), "英");
        assert_eq!(label(&shown(0x0404, ime(true, CMODE_NATIVE))), "中");
        // A plain layout, or an IME that did not answer: the short name.
        assert_eq!(label(&shown(0x0409, None)), "XX");
        assert_eq!(label(&shown(0x0412, None)), "XX");
        assert_eq!(label(&Shown::default()), "?");
    }

    #[test]
    fn mode_names_follow_the_label() {
        assert_eq!(mode_key(&shown(0x0412, ime(true, CMODE_NATIVE))), Some("input.mode.hangul"));
        assert_eq!(mode_key(&shown(0x0412, ime(true, 0))), Some("input.mode.english"));
        assert_eq!(mode_key(&shown(0x0411, ime(true, CMODE_NATIVE | CMODE_KATAKANA))), Some("input.mode.katakana"));
        assert_eq!(mode_key(&shown(0x0411, ime(false, 0))), Some("input.mode.alphanumeric"));
        assert_eq!(mode_key(&shown(0x0804, ime(true, CMODE_NATIVE))), Some("input.mode.chinese"));
        assert_eq!(mode_key(&shown(0x0409, None)), None);
    }

    #[test]
    fn scripts_and_short_names() {
        assert_eq!(script_of(0x0412), Script::Korean);
        assert_eq!(script_of(0x0411), Script::Japanese);
        assert_eq!(script_of(0x0804), Script::Chinese);
        assert_eq!(script_of(0x0C04), Script::Chinese);
        assert_eq!(script_of(0x0409), Script::Other);
        assert_eq!(script_of(0x0407), Script::Other);
        assert_eq!(short_from_locale("en-US"), "EN");
        assert_eq!(short_from_locale("zh-Hant-TW"), "ZH");
        assert_eq!(short_from_locale("de"), "DE");
        assert_eq!(short_from_locale(""), "");
        assert_eq!(short_from_locale("x1-YY"), "");
    }

    #[test]
    fn switching_plans_per_ime() {
        // Korean: flip the native bit, keep the rest; open a closed IME.
        assert_eq!(plan(Script::Korean, ImeState { open: true, conv: CMODE_FULLSHAPE }, true), (None, Some(CMODE_FULLSHAPE | CMODE_NATIVE)));
        assert_eq!(plan(Script::Korean, ImeState { open: true, conv: CMODE_NATIVE }, false), (None, Some(0)));
        assert_eq!(plan(Script::Korean, ImeState { open: false, conv: 0 }, true), (Some(true), Some(CMODE_NATIVE)));
        assert_eq!(plan(Script::Korean, ImeState { open: true, conv: CMODE_NATIVE }, true), (None, None), "already there");
        // Japanese: Latin by closing; kana by opening (hiragana, full width).
        assert_eq!(plan(Script::Japanese, ImeState { open: true, conv: CMODE_NATIVE | CMODE_FULLSHAPE }, false), (Some(false), None));
        assert_eq!(plan(Script::Japanese, ImeState { open: false, conv: CMODE_NATIVE | CMODE_FULLSHAPE }, true), (Some(true), None));
        assert_eq!(plan(Script::Japanese, ImeState { open: true, conv: 0 }, true), (None, Some(CMODE_NATIVE | CMODE_FULLSHAPE)));
        // Chinese: like Korean.
        assert_eq!(plan(Script::Chinese, ImeState { open: true, conv: CMODE_NATIVE }, false), (None, Some(0)));
        // Not an IME: nothing.
        assert_eq!(plan(Script::Other, ImeState::default(), true), (None, None));
    }

    #[test]
    fn apps_are_keyed_by_aumid_then_exe() {
        let packaged = AppIdent::new(Some("Microsoft.WindowsTerminal_8wekyb3d8bbwe!App"), Some("C:\\Program Files\\WindowsApps\\X\\WindowsTerminal.exe"));
        assert_eq!(packaged.key().as_deref(), Some("aumid:microsoft.windowsterminal_8wekyb3d8bbwe!app"));
        let plain = AppIdent::new(None, Some("C:\\Apps\\Kakao\\KakaoTalk.exe"));
        assert_eq!(plain.key().as_deref(), Some("exe:c:\\apps\\kakao\\kakaotalk.exe"));
        assert_eq!(AppIdent::new(Some("  "), Some("")).key(), None, "nothing to key by");
        // Rules by AUMID, full path or file name.
        assert!(packaged.matches("aumid:Microsoft.WindowsTerminal_8wekyb3d8bbwe!App"));
        assert!(packaged.matches("name:windowsterminal.exe"));
        assert!(plain.matches("EXE:c:\\apps\\kakao\\KAKAOTALK.exe"));
        assert!(plain.matches("name:KakaoTalk.exe"));
        assert!(!plain.matches("name:talk.exe"));
        assert!(!plain.matches("c:\\apps\\kakao\\kakaotalk.exe"), "a rule needs its kind");
        assert!(!plain.matches("aumid:kakaotalk"));
    }

    #[test]
    fn rule_beats_memory_beats_nothing() {
        assert_eq!(decide(Some(RuleMode::Native), false, Some(false)), Some(true));
        assert_eq!(decide(Some(RuleMode::Alpha), true, Some(true)), Some(false));
        assert_eq!(decide(Some(RuleMode::Ignore), true, Some(true)), None);
        assert_eq!(decide(Some(RuleMode::Remember), false, Some(true)), Some(true), "a Remember rule works with the switch off");
        assert_eq!(decide(Some(RuleMode::Remember), false, None), None);
        assert_eq!(decide(None, true, Some(false)), Some(false));
        assert_eq!(decide(None, false, Some(true)), None, "off by default: nothing is touched");
        assert_eq!(decide(None, true, None), None, "never seen: nothing to restore");
        // Only Remember (or no rule with the switch on) keeps modes.
        assert!(records(Some(RuleMode::Remember), false));
        assert!(records(None, true));
        assert!(!records(None, false));
        assert!(!records(Some(RuleMode::Alpha), true));
        assert!(!records(Some(RuleMode::Ignore), true));
        // The first matching rule counts.
        let rules = [("name:code.exe", "alpha"), ("exe:c:\\x\\code.exe", "native")];
        let app = AppIdent::new(None, Some("C:\\x\\Code.exe"));
        assert_eq!(rule_for(&rules, &app, |r| (r.0, r.1)), Some(RuleMode::Alpha));
        assert_eq!(rule_for(&rules, &AppIdent::new(None, Some("c:\\y.exe")), |r| (r.0, r.1)), None);
    }

    #[test]
    fn rule_modes_parse_leniently_and_cycle() {
        assert_eq!(RuleMode::parse("Native"), RuleMode::Native);
        assert_eq!(RuleMode::parse(" alpha "), RuleMode::Alpha);
        assert_eq!(RuleMode::parse("zz"), RuleMode::Ignore);
        for (n, m) in RULE_MODES {
            assert_eq!(RuleMode::parse(n), m);
            assert_eq!(m.name(), n);
        }
        let mut m = RuleMode::Alpha;
        for _ in 0..RULE_MODES.len() {
            m = m.next();
        }
        assert_eq!(m, RuleMode::Alpha, "cycles through all four");
    }

    #[test]
    fn manual_change_right_after_a_restore_wins() {
        let r = Some(Restored { at_ms: 10_000, native: true });
        // Switched back within a second: the user's choice, kept.
        let o = observe(true, r, 10_600, false);
        assert!(o.user_override);
        assert_eq!(o.store, Some(false));
        // Same mode as restored: not an override.
        assert!(!observe(true, r, 10_300, true).user_override);
        // Later changes are ordinary changes (still kept when recording).
        let late = observe(true, r, 11_500, false);
        assert!(!late.user_override);
        assert_eq!(late.store, Some(false));
        // Not recording (a fixed rule): the override is seen, nothing kept.
        let fixed = observe(false, r, 10_200, false);
        assert!(fixed.user_override);
        assert_eq!(fixed.store, None);
        assert!(!observe(true, None, 5, true).user_override);
    }

    #[test]
    fn a_change_while_the_restore_waits_is_the_users() {
        // Always English; it was English at activation, the user typed 한/영
        // during the wait: Hangul stays.
        assert_eq!(before_restore(Some(false), Some(true), false, false), Before::UserChanged);
        // It came up in Hangul and is still Hangul: ours to set.
        assert_eq!(before_restore(Some(true), Some(true), false, false), Before::Apply);
        assert_eq!(before_restore(None, Some(true), false, false), Before::Apply);
        // Already there, or the IME went quiet.
        assert_eq!(before_restore(Some(true), Some(false), false, false), Before::AlreadyThere);
        assert_eq!(before_restore(Some(true), None, false, false), Before::Unknown);
        // A layout switch meanwhile is the user's too.
        assert_eq!(before_restore(Some(true), Some(true), true, false), Before::UserChanged);
    }

    #[test]
    fn suggestions_are_rules_for_english() {
        for (app, _, mode) in SUGGESTIONS {
            assert!(app.starts_with("name:"), "{app}");
            assert_eq!(mode, RuleMode::Alpha);
        }
        let wt = AppIdent::new(None, Some("C:\\Program Files\\WindowsApps\\Microsoft.WindowsTerminal\\WindowsTerminal.exe"));
        assert!(SUGGESTIONS.iter().any(|(a, ..)| wt.matches(a)));
    }
}
