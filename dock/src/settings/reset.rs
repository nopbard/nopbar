//! "Reset all settings" at the foot of General (the one reset; it is asked
//! once more inline, being the one thing that cannot be taken back). A
//! reset is an edit like any other: applied and saved at once. The same
//! inline question asks before a dock is made "Same as Dock 1"
//! (displays_view.rs).

use windows::core::Result;
use windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F;
use windows::Win32::Graphics::Direct2D::{ID2D1DeviceContext, D2D1_ROUNDED_RECT};

use super::rows::Mirror;
use super::{clip_to, model, App, Hit};
use crate::i18n::t;

/// Which reset is waiting for its "Reset" (on the page shown).
#[derive(Clone, Copy, PartialEq, Debug)]
pub(super) enum Confirm {
    All,
    /// "Same as Dock 1" turned on (its own settings and apps go).
    SameAsMain,
}

impl Confirm {
    /// About one dock (the one picked when it was asked): it holds only
    /// while that dock is still the one picked (confirm_target).
    pub(super) fn targets_a_dock(self) -> bool {
        matches!(self, Confirm::SameAsMain)
    }
}

/// A question about one dock still means that dock: the display picked
/// now (key, main?) is the one it was asked for, and each display still
/// has a dock of its own (the docks' display choice unchanged). Pure:
/// tested.
pub(super) fn target_holds(asked: Option<&(String, bool)>, now: Option<(String, bool)>, per_display: bool) -> bool {
    per_display && asked.is_some() && asked.cloned() == now
}

fn rect(l: f32, t: f32, r: f32, b: f32) -> D2D_RECT_F {
    D2D_RECT_F { left: l, top: t, right: r, bottom: b }
}

impl App {
    /// [Reset] [Cancel] at the far end of a card row centred on `cy`;
    /// returns where they start (the text stops before it).
    unsafe fn confirm_buttons(&self, dc: &ID2D1DeviceContext, hits: &mut Vec<(D2D_RECT_F, Hit)>, cy: f32, m: Mirror, clip: D2D_RECT_F, yes: &str) -> Result<f32> {
        let no = t("settings.button.cancel");
        let yw = self.text_w(yes, super::rows::tok::BODY, false) + 32.0;
        let nw = self.text_w(no, super::rows::tok::BODY, false) + 32.0;
        let x1 = m.right - super::rows::tok::ROW_PAD;
        let h = super::rows::tok::CONTROL_H / 2.0;
        let y_r = m.r(rect(x1 - yw, cy - h, x1, cy + h));
        let n_r = m.r(rect(x1 - yw - 8.0 - nw, cy - h, x1 - yw - 8.0, cy + h));
        self.note(super::layout::Kind::Ctl { kind: super::layout::CtlKind::Button, cy }, "confirm", m.r(rect(x1 - yw - 8.0 - nw, cy - h, x1, cy + h)));
        self.button(dc, n_r, no, self.hover == Some(Hit::ResetNo))?;
        self.accent_button(dc, y_r, yes, self.hover == Some(Hit::ResetYes))?;
        // Reading order: Cancel, then Reset.
        if let Some(r) = clip_to(n_r, clip) {
            hits.push((r, Hit::ResetNo));
        }
        if let Some(r) = clip_to(y_r, clip) {
            hits.push((r, Hit::ResetYes));
        }
        Ok(x1 - yw - 8.0 - nw - 16.0)
    }

    /// A question card from `y` (a card of its own): the text, then
    /// [Cancel] [`yes`]; returns where the card ends.
    #[allow(clippy::too_many_arguments)]
    pub(super) unsafe fn confirm_card(&self, dc: &ID2D1DeviceContext, hits: &mut Vec<(D2D_RECT_F, Hit)>, y: f32, left: f32, right: f32, clip: D2D_RECT_F, q: &str, yes: &str) -> Result<f32> {
        self.question_row(dc, hits, "confirm", y, left, right, clip, q, yes)
    }

    /// A row card asking `q` with [Cancel] [`yes`] at its end (a tone
    /// deeper than the rows: it waits for an answer).
    #[allow(clippy::too_many_arguments)]
    unsafe fn question_row(&self, dc: &ID2D1DeviceContext, hits: &mut Vec<(D2D_RECT_F, Hit)>, key: &str, y: f32, left: f32, right: f32, clip: D2D_RECT_F, q: &str, yes: &str) -> Result<f32> {
        use super::rows::tok;
        let rtl = crate::i18n::rtl();
        let m = Mirror { on: rtl, left, right };
        let buttons = self.text_w(yes, tok::BODY, false) + self.text_w(t("settings.button.cancel"), tok::BODY, false) + 64.0 + 8.0;
        let tw = (right - left - tok::ROW_PAD * 3.0 - buttons).max(80.0);
        let th = self.para_h(q, tok::BODY, tw);
        let h = (th + tok::ROW_VPAD * 2.0).max(tok::ROW_MIN);
        let r = self.single_row(dc, key, y, h, left, right)?;
        self.fill_round(dc, self.wash(0.04), m.r(r), self.card_r())?;
        self.outline_hc(dc, m.r(r))?;
        let cy = r.top + h / 2.0;
        self.confirm_buttons(dc, hits, cy, m, clip, yes)?;
        self.para_dir(dc, q, m.span(left + tok::ROW_PAD, tw), cy - th / 2.0, tok::BODY, false, tw, self.ink(), rtl)?;
        Ok(r.bottom)
    }

    /// A notice card from `y` (a card of its own): the text and one
    /// button (`hit`) at its end; returns where the card ends.
    #[allow(clippy::too_many_arguments)]
    pub(super) unsafe fn notice_card(&self, dc: &ID2D1DeviceContext, hits: &mut Vec<(D2D_RECT_F, Hit)>, y: f32, left: f32, right: f32, clip: D2D_RECT_F, s: &str, button: &str, hit: Hit) -> Result<f32> {
        use super::rows::tok;
        let rtl = crate::i18n::rtl();
        let m = Mirror { on: rtl, left, right };
        let bw = self.text_w(button, tok::BODY, false) + 32.0;
        let tw = (right - left - tok::ROW_PAD * 3.0 - bw).max(40.0);
        let th = self.para_h(s, tok::BODY, tw);
        let h = (th + tok::ROW_VPAD * 2.0).max(tok::ROW_MIN);
        let r = self.single_row(dc, "notice", y, h, left, right)?;
        let cy = r.top + h / 2.0;
        self.row_button(dc, hits, m, "notice", right - tok::ROW_PAD, cy, button, hit, clip)?;
        self.para_dir(dc, s, m.span(left + tok::ROW_PAD, tw), cy - th / 2.0, tok::BODY, false, tw, self.ink(), rtl)?;
        Ok(r.bottom)
    }

    /// High contrast only: an outline in Highlight around `r` (a question
    /// shows by its tone otherwise).
    unsafe fn outline_hc(&self, dc: &ID2D1DeviceContext, r: D2D_RECT_F) -> Result<()> {
        if let Some(h) = self.hc {
            let b = dc.CreateSolidColorBrush(&super::rgba(h.highlight), None)?;
            dc.DrawRoundedRectangle(&D2D1_ROUNDED_RECT { rect: rect(r.left + 1.0, r.top + 1.0, r.right - 1.0, r.bottom - 1.0), radiusX: crate::theme::concentric(self.card_r(), 1.0), radiusY: crate::theme::concentric(self.card_r(), 1.0) }, &b, 2.0, None);
        }
        Ok(())
    }

    /// The General page's "Reset all settings": a row card (its label,
    /// [Reset] at its end); asked, the question in its place.
    pub(super) unsafe fn paint_reset_all(&self, dc: &ID2D1DeviceContext, hits: &mut Vec<(D2D_RECT_F, Hit)>, y: f32, left: f32, right: f32, clip: D2D_RECT_F) -> Result<f32> {
        use super::rows::tok;
        if self.confirm == Some(Confirm::All) {
            return self.question_row(dc, hits, "reset.all", y, left, right, clip, t("settings.reset.all.confirm"), t("settings.reset.confirm"));
        }
        let m = Mirror { on: crate::i18n::rtl(), left, right };
        let r = self.single_row(dc, "reset.all", y, tok::ROW_MIN, left, right)?;
        let cy = r.top + tok::ROW_MIN / 2.0;
        let b = self.row_button(dc, hits, m, "reset.all", right - tok::ROW_PAD, cy, t("settings.reset.confirm"), Hit::ResetAll, clip)?;
        self.row_text(dc, hits, "reset.all", r, cy, b.left, t("settings.reset.all"), None, clip)?;
        Ok(r.bottom)
    }

    /// The display picked now: (its key, is it the main one).
    pub(super) fn picked_target(&self) -> Option<(String, bool)> {
        self.screens.get(self.display_sel).map(|d| (d.key.clone(), d.primary))
    }

    /// The open question still means what it asked (always, unless it is
    /// about one dock: then that dock must still be the one picked).
    pub(super) fn confirm_holds(&self) -> bool {
        match self.confirm {
            Some(c) if c.targets_a_dock() => target_holds(self.confirm_target.as_ref(), self.picked_target(), self.per_display()),
            _ => true,
        }
    }

    /// After the displays, the main one or the docks' display choice
    /// changed: a question about a dock that is no longer the one picked
    /// goes (it would act on another dock).
    pub(super) fn drop_stale_confirm(&mut self) {
        if !self.confirm_holds() {
            self.confirm = None;
            self.confirm_target = None;
        }
    }

    /// "Reset all settings" (after its "Reset"): every page back to its
    /// defaults (the pinned apps stay).
    pub(super) fn reset_all(&mut self) {
        self.confirm = None;
        let defaults = model::defaults(crate::config::first_pins(Vec::new()));
        let lang = self.cfg.language.clone();
        let keys = model::reset_all(&mut self.cfg, &defaults, &self.sections);
        let builtins = keys.iter().any(|k| k.starts_with("shortcuts."));
        for k in keys {
            self.touch(&k);
        }
        // Built-in shortcuts back: the free keys found before are stale.
        if builtins {
            self.alts.borrow_mut().clear();
        }
        if self.cfg.language != lang {
            self.language_changed();
        }
        if self.keyboard {
            self.focus = Some(Hit::ResetAll);
        }
        self.send_preview();
    }

    /// The reset buttons and their question.
    pub(super) fn click_reset(&mut self, h: Hit) {
        match h {
            Hit::ResetAll => {
                self.confirm = Some(Confirm::All);
                self.confirm_target = None;
                // The keyboard lands on the safe answer.
                self.focus = Some(Hit::ResetNo);
            }
            Hit::ResetYes => {
                // The dock it was asked about went (unplugged, the main
                // display changed, one dock again): nothing is reset.
                if !self.confirm_holds() {
                    self.confirm = None;
                    self.confirm_target = None;
                    self.redraw();
                    return;
                }
                match self.confirm {
                    Some(Confirm::SameAsMain) => self.same_as_main_yes(),
                    Some(Confirm::All) => self.reset_all(),
                    None => {}
                }
                return;
            }
            _ => {
                let was = self.confirm.take();
                self.focus = match was {
                    Some(Confirm::SameAsMain) => self.rows().iter().position(|r| r.key == model::SAME_AS_MAIN).map(Hit::Toggle),
                    _ => Some(Hit::ResetAll),
                };
            }
        }
        self.redraw();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_question_about_a_dock_holds_only_for_that_dock() {
        let dock2 = ("DEL40F2-ABC".to_string(), false);
        let main = ("GSM5B09-X".to_string(), true);
        assert!(target_holds(Some(&dock2), Some(dock2.clone()), true));
        // Dock 2 unplugged: the pick falls back to the main display.
        assert!(!target_holds(Some(&dock2), Some(main.clone()), true));
        // The main display changed: the same display is not "main" now.
        assert!(!target_holds(Some(&main), Some((main.0.clone(), false)), true));
        // One dock again (the display choice changed).
        assert!(!target_holds(Some(&dock2), Some(dock2.clone()), false));
        // Nothing recorded, or no display at all.
        assert!(!target_holds(None, Some(dock2.clone()), true));
        assert!(!target_holds(Some(&dock2), None, true));
        assert!(Confirm::SameAsMain.targets_a_dock());
        assert!(!Confirm::All.targets_a_dock());
    }
}
