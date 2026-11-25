//! The Input page's per-app rules (crate::ime): a list of apps, each with
//! its mode (Always English / Always Hangul / Remember / Don't touch, one
//! click to the next) and ×; "Add app…" lists the apps running now; the
//! suggestions (terminals, code editors: English) are one click each and
//! never applied by themselves. Changes show on the dock right away, like
//! any other setting.

use windows::core::Result;
use windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F;
use windows::Win32::Graphics::Direct2D::ID2D1DeviceContext;

use super::pins::{cancel_scan, scan_waiting, Purpose};
use super::rows::Mirror;
use super::{clip_to, App, Hit};
use crate::config::InputRule;
use crate::ime::logic::{AppIdent, RuleMode, RULE_MODES, SUGGESTIONS};
use crate::i18n::t;


fn rect(l: f32, t: f32, r: f32, b: f32) -> D2D_RECT_F {
    D2D_RECT_F { left: l, top: t, right: r, bottom: b }
}

/// Of `wins` (the apps with a window open now), those `rules` have none
/// for: by name, each once, with the rule's app ("Add app…"). Reads
/// version resources: the reading thread's (pins::read_running).
pub(super) fn rule_apps_from(wins: &[crate::apps::AppWindow], rules: &[InputRule]) -> Vec<(String, String)> {
    let me = std::env::current_exe().ok().map(|p| p.to_string_lossy().to_lowercase());
    let mut out: Vec<(String, String)> = Vec::new();
    for w in wins {
        if w.exe.is_some() && w.exe == me {
            continue;
        }
        let id = AppIdent::new(w.aumid.as_deref(), w.exe.as_deref());
        let Some(key) = id.key() else { continue };
        if out.iter().any(|(_, k)| *k == key) || rules.iter().any(|r| id.matches(&r.app)) {
            continue;
        }
        out.push((crate::apps::display_name(w), key));
    }
    out.sort_by_key(|(n, _)| n.to_lowercase());
    out
}

/// The text key of a rule mode's name.
pub(super) fn mode_text(m: RuleMode) -> &'static str {
    match m {
        RuleMode::Native => t("settings.input.mode.native"),
        RuleMode::Alpha => t("settings.input.mode.alpha"),
        RuleMode::Remember => t("settings.input.mode.remember"),
        RuleMode::Ignore => t("settings.input.mode.ignore"),
    }
}

/// A suggestion's name (a product name, or a text key).
pub(super) fn suggestion_name(n: &'static str) -> &'static str {
    if n.starts_with("settings.") { t(n) } else { n }
}

/// What to draw as a rule's icon: its exe, or its packaged app; a rule by
/// file name has none.
pub(super) fn rule_icon(app: &str) -> Option<String> {
    if let Some(e) = app.strip_prefix("exe:") {
        return Some(e.to_string());
    }
    app.strip_prefix("aumid:").map(|a| format!("shell:AppsFolder\\{a}"))
}

/// The suggestions not taken yet (by their SUGGESTIONS index).
pub(super) fn open_suggestions(rules: &[InputRule]) -> Vec<usize> {
    (0..SUGGESTIONS.len()).filter(|&k| !rules.iter().any(|r| r.app.eq_ignore_ascii_case(SUGGESTIONS[k].0))).collect()
}

impl App {
    /// "Per-app rules" (its "i": how a rule and the memory go together)
    /// over one card: a row per rule (its app, its mode, ×), "None"
    /// without any, last "Add app…" ▾ (the apps running now). Then
    /// "Suggestions": a row each, Add at its end.
    pub(super) unsafe fn paint_input(&self, dc: &ID2D1DeviceContext, hits: &mut Vec<(D2D_RECT_F, Hit)>, y: f32, left: f32, right: f32, clip: D2D_RECT_F) -> Result<f32> {
        use super::rows::tok;
        let rtl = crate::i18n::rtl();
        let m = Mirror { on: rtl, left, right };
        let (tl0, cr) = (left + tok::ROW_PAD, right - tok::ROW_PAD);
        let ch = tok::CONTROL_H / 2.0;
        let mut y = self.heading(dc, hits, y, left, right, "input.rules", t("settings.input.rules"), Some(super::tooltip::card_tip("settings.input.rules.hint")), clip)?;
        let rules = &self.cfg.input.rules;
        let mut first = true;
        if rules.is_empty() {
            let r = self.card_row(dc, "input.none", y, tok::ROW_MIN, left, right, true, false)?;
            self.text_box(dc, t("settings.list.none"), m.span(tl0, cr - tl0), self.text_top(r.top + tok::ROW_MIN / 2.0, tok::BODY), tok::BODY, false, cr - tl0, self.faint(), rtl)?;
            y = r.bottom;
            first = false;
        }
        // The mode buttons share one width (the longest name).
        let mode_w = RULE_MODES.iter().map(|(_, md)| self.text_w(mode_text(*md), tok::BODY, false)).fold(0.0, f32::max) + 24.0;
        for (i, r) in rules.iter().enumerate() {
            let row = self.card_row(dc, &r.app, y, tok::ROW_MIN + 4.0, left, right, first, false)?;
            first = false;
            let cy = row.top + (row.bottom - row.top) / 2.0;
            if let Some(p) = rule_icon(&r.app) {
                self.draw_icon(dc, &p, m.r(rect(tl0, cy - 12.0, tl0 + 24.0, cy + 12.0)))?;
            }
            let x_close = cr - tok::CONTROL_H;
            let x_mode = x_close - 6.0 - mode_w;
            let tl = tl0 + 24.0 + 12.0;
            let room = x_mode - tok::ROW_PAD - tl;
            let tx = if rtl { m.x(tl + room) } else { tl };
            self.text_box(dc, &r.name, tx, self.text_top(cy, tok::BODY), tok::BODY, false, room, self.ink(), rtl)?;
            // In reading order for the keyboard: the mode, then ×.
            let mode = m.r(rect(x_mode, cy - ch, x_mode + mode_w, cy + ch));
            self.button(dc, mode, mode_text(RuleMode::parse(&r.mode)), self.hover == Some(Hit::InputMode(i)))?;
            if let Some(c) = clip_to(mode, clip) {
                hits.push((c, Hit::InputMode(i)));
            }
            let close = m.r(rect(x_close, cy - ch, cr, cy + ch));
            self.glyph_button(dc, close, crate::glyphs::Glyph::Close, self.hover == Some(Hit::InputRemove(i)))?;
            if let Some(c) = clip_to(close, clip) {
                hits.push((c, Hit::InputRemove(i)));
            }
            y = row.bottom;
        }
        // Last: Add app… ▾ (the apps running now).
        let row = self.card_row(dc, "input.add", y, tok::ROW_MIN, left, right, first, true)?;
        let cy = row.top + tok::ROW_MIN / 2.0;
        let add = t("settings.input.add");
        let aw = self.text_w(add, tok::BODY, false) + 44.0;
        let a = m.r(rect(tl0, cy - ch, tl0 + aw, cy + ch));
        let open = self.apps_menu.as_ref().is_some_and(|m| m.rules) || scan_waiting(Purpose::Rules);
        self.note(super::layout::Kind::Button, add, a);
        self.fill_round(dc, self.ctl_fill(if open || self.hover == Some(Hit::InputAdd) { crate::theme::State::Hover } else { crate::theme::State::Rest }), a, self.ctl_r())?;
        let (tx, chev) = if rtl { (a.left + 30.0, a.left + 16.0) } else { (a.left + 14.0, a.right - 16.0) };
        self.text_box(dc, add, tx, self.text_top(cy, tok::BODY), tok::BODY, false, aw - 44.0, self.ink(), rtl)?;
        self.chevron(dc, chev, cy, self.dim())?;
        if let Some(c) = clip_to(a, clip) {
            hits.push((c, Hit::InputAdd));
        }
        if open {
            self.apps_anchor.set(Some(a));
        }
        let mut y = row.bottom;

        // Suggestions: one click adds the rule.
        let sugg = open_suggestions(rules);
        if !sugg.is_empty() {
            y = self.heading(dc, hits, y, left, right, "input.suggested", t("settings.input.suggested"), None, clip)?;
            let add = t("settings.input.suggest_add");
            let n = sugg.len();
            for (j, k) in sugg.into_iter().enumerate() {
                let (_, name, mode) = SUGGESTIONS[k];
                let row = self.card_row(dc, name, y, tok::ROW_MIN, left, right, j == 0, j + 1 == n)?;
                let cy = row.top + tok::ROW_MIN / 2.0;
                let b = self.row_button(dc, hits, m, name, cr, cy, add, Hit::InputSuggest(k), clip)?;
                let line = format!("{}  ·  {}", suggestion_name(name), mode_text(mode));
                self.row_text(dc, hits, name, row, cy, b.left, &line, None, clip)?;
                y = row.bottom;
            }
        }
        Ok(y)
    }

    /// Adds a rule at the end (English to start with: the usual reason to
    /// add one), unless that app has one.
    pub(super) fn add_input_rule(&mut self, name: String, app: String, mode: RuleMode) {
        if self.cfg.input.rules.iter().any(|r| r.app.eq_ignore_ascii_case(&app)) {
            self.note = t("settings.input.already").into();
            self.redraw();
            return;
        }
        self.cfg.input.rules.push(InputRule { app, name, mode: mode.name().into() });
        self.focus = Some(Hit::InputMode(self.cfg.input.rules.len() - 1));
        self.touch("input.rules");
        self.send_preview();
    }

    pub(super) fn click_input(&mut self, h: Hit) {
        let n = self.cfg.input.rules.len();
        match h {
            Hit::InputAdd => {
                if self.apps_menu.take().is_some() {
                    self.redraw();
                    return;
                }
                // Read off this thread, as the Pins page's list (pins.rs
                // Scan); pressed again while it is read: not opened.
                if scan_waiting(Purpose::Rules) {
                    cancel_scan();
                    if self.note == t("settings.pins.loading") {
                        self.note.clear();
                    }
                } else {
                    self.ask_running_apps(Purpose::Rules);
                }
                self.redraw();
            }
            Hit::InputSuggest(k) => {
                let Some(&(app, name, mode)) = SUGGESTIONS.get(k) else { return };
                self.add_input_rule(suggestion_name(name).to_string(), app.to_string(), mode);
            }
            Hit::InputMode(i) if i < n => {
                let r = &mut self.cfg.input.rules[i];
                r.mode = RuleMode::parse(&r.mode).next().name().into();
                self.focus = Some(Hit::InputMode(i));
                self.touch("input.rules");
                self.send_preview();
            }
            Hit::InputRemove(i) if i < n => {
                self.cfg.input.rules.remove(i);
                // The keyboard stays on the list.
                self.focus = if n > 1 { Some(Hit::InputMode(i.min(n - 2))) } else { Some(Hit::InputAdd) };
                self.touch("input.rules");
                self.send_preview();
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suggestions_hide_once_taken() {
        let mut rules = Vec::new();
        assert_eq!(open_suggestions(&rules).len(), SUGGESTIONS.len());
        rules.push(InputRule { app: "NAME:code.exe".into(), name: "Code".into(), mode: "alpha".into() });
        let open = open_suggestions(&rules);
        assert_eq!(open.len(), SUGGESTIONS.len() - 1);
        assert!(open.iter().all(|&k| SUGGESTIONS[k].0 != "name:code.exe"));
    }

    #[test]
    fn rules_travel_with_their_key_and_reset_with_the_page() {
        use super::super::model;
        let mut from = crate::config::Config::default();
        from.input.rules.push(InputRule { app: "name:code.exe".into(), name: "Code".into(), mode: "alpha".into() });
        from.input.remember = true;
        from.dock.autohide = true;
        let mut to = crate::config::Config::default();
        model::apply_keys(&from, &mut to, &["input.rules".to_string()]);
        assert_eq!(to.input.rules, from.input.rules);
        assert!(!to.input.remember && !to.dock.autohide, "only the list");
        model::apply_keys(&from, &mut to, &["input.remember".to_string()]);
        assert!(to.input.remember);
        // The Input page holds the memory row and the list; its reset
        // clears both (the indicator is a Top bar row).
        let secs = model::sections();
        let page = secs.iter().find(|s| s.kind == model::Kind::Input).expect("an Input page");
        assert_eq!(model::page_keys(page), ["input.remember", "input.rules"]);
        let mut c = from.clone();
        model::reset_page(&mut c, &crate::config::Config::default(), page);
        assert!(c.input.rules.is_empty() && !c.input.remember);
        assert!(c.dock.autohide, "another page's");
    }

    #[test]
    fn input_config_defaults_and_reads_leniently() {
        let d = crate::config::InputCfg::default();
        assert!(!d.remember, "opt-in");
        assert_eq!(d.indicator_mode(), 0);
        let c: crate::config::InputCfg = serde_json::from_str(r#"{"indicator":"NEVER","rules":[{"app":"name:k.exe","mode":"native"}]}"#).unwrap();
        assert_eq!(c.indicator_mode(), 2);
        assert_eq!(c.rules[0].name, "", "missing fields default");
        assert_eq!(RuleMode::parse(&c.rules[0].mode), RuleMode::Native);
    }

    #[test]
    fn rule_icons_come_from_exe_or_package() {
        assert_eq!(rule_icon("exe:c:\\a\\b.exe").as_deref(), Some("c:\\a\\b.exe"));
        assert_eq!(rule_icon("aumid:x!app").as_deref(), Some("shell:AppsFolder\\x!app"));
        assert_eq!(rule_icon("name:code.exe"), None);
    }
}
