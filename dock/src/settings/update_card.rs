//! The About page's updates (under the app's name and version, in its
//! card) (status, "Check now", "Download" or
//! "Update now" / "Restart to update", and "Automatic updates": one switch
//! for checking and, for a copy that updates itself, installing). "Check
//! now" runs crate::updates' check on a short-lived thread; the answer
//! comes back through update.json, which the dock's scheduler shares.
//! While an automatic update moves (downloading, checking, installing)
//! the card reads update.json once a second.

use windows::core::Result;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::Graphics::Direct2D::Common::{D2D1_COLOR_F, D2D_RECT_F};
use windows::Win32::Graphics::Direct2D::{ID2D1DeviceContext, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE, D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT};
use windows::Win32::Graphics::DirectWrite::DWRITE_FONT_WEIGHT_NORMAL;
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};
use windows_numerics::Vector2;

use super::rows::Mirror;
use super::{clip_to, extra, App, Hit, FONT};
use crate::i18n::{t, tf};
use crate::updates::{self, Channel, Phase, Status};

/// "Check now" finished (update.json has the answer).
pub const WM_UPDATE_DONE: u32 = WM_APP + 11;
/// Reads update.json again while an update moves.
pub const UPD_POLL_TIMER: usize = 0x5550;

/// What the card says about an automatic update of a newer version, if
/// one is under way (None: the plain check status).
pub fn phase_text(st: &updates::State, current: &str) -> Option<String> {
    let p = &st.pending;
    if !updates::is_newer(&p.version, current) {
        return None;
    }
    let v = [("v", p.version.as_str())];
    Some(match p.phase {
        Phase::None => return None,
        Phase::Downloading => tf("about.updates.downloading", &[("v", &p.version), ("n", &p.progress.min(100).to_string())]),
        Phase::Verifying => tf("about.updates.verifying", &v),
        Phase::Ready => tf("about.updates.ready", &v),
        Phase::Installing => tf("about.updates.installing", &v),
        Phase::Failed => tf("about.updates.install_failed", &v),
    })
}

/// An update is moving: the card keeps reading update.json.
pub fn moving(st: &updates::State) -> bool {
    matches!(st.pending.phase, Phase::Downloading | Phase::Verifying | Phase::Installing)
}

fn rect(l: f32, t: f32, r: f32, b: f32) -> D2D_RECT_F {
    D2D_RECT_F { left: l, top: t, right: r, bottom: b }
}

impl App {
    pub(super) fn text_width(&self, s: &str, size: f32) -> f32 {
        self.gfx.text_layout(s, FONT, size, DWRITE_FONT_WEIGHT_NORMAL).map(|l| l.1).unwrap_or(60.0)
    }

    /// The page's primary button: accent fill, text in the accent's ink.
    pub(super) unsafe fn accent_button(&self, dc: &ID2D1DeviceContext, r: D2D_RECT_F, label: &str, hot: bool) -> Result<()> {
        self.note(super::layout::Kind::Button, label, r);
        let mut c = self.accent;
        if hot && self.hc.is_none() {
            c.a *= 0.88;
        }
        self.fill_round(dc, c, r, self.ctl_r())?;
        let (layout, tw, th) = self.gfx.text_layout(label, FONT, super::rows::tok::BODY, DWRITE_FONT_WEIGHT_NORMAL)?;
        let ink = dc.CreateSolidColorBrush(&self.on_accent(), None)?;
        let room = (r.right - r.left - 12.0).max(1.0);
        dc.PushAxisAlignedClip(&rect(r.left + 4.0, r.top, r.right - 4.0, r.bottom), D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
        dc.DrawTextLayout(Vector2 { X: (r.left + r.right - tw.min(room)) / 2.0, Y: (r.top + r.bottom - th) / 2.0 }, &layout, &ink, D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT);
        dc.PopAxisAlignedClip();
        Ok(())
    }

    /// Text clipped to [x, x + maxw).
    unsafe fn label_in(&self, dc: &ID2D1DeviceContext, s: &str, x: f32, y: f32, size: f32, bold: bool, maxw: f32, c: D2D1_COLOR_F) -> Result<()> {
        // Right to left it ends at the box's right edge.
        self.text_box(dc, s, x, y, size, bold, maxw, c, crate::i18n::rtl())
    }

    /// The Updates card: "Updates" and where it stands (Check now, and
    /// Update now / Restart / Download when there is one, at the row's
    /// end), then "Automatic updates" with its switch (its "i": what it
    /// does and never sends). Returns where the card ends.
    /// `joined`: its rows continue the card over them (About: the app's).
    #[allow(clippy::too_many_arguments)]
    pub(super) unsafe fn paint_updates(&self, dc: &ID2D1DeviceContext, hits: &mut Vec<(D2D_RECT_F, Hit)>, y: f32, left: f32, right: f32, clip: D2D_RECT_F, joined: bool) -> Result<f32> {
        use super::rows::tok;
        let m = Mirror { on: crate::i18n::rtl(), left, right };
        let (tl, cr) = (left + tok::ROW_PAD, right - tok::ROW_PAD);
        // Two lines: the name, where it stands under it.
        let h1 = 52.0;
        // Store: it updates NopBar; just say so.
        if self.upd_channel != Channel::Direct {
            let r = self.card_row(dc, "updates", y, h1, left, right, !joined, true)?;
            let inner = cr - tl;
            self.label_in(dc, t("about.updates"), m.span(tl, inner), r.top + 8.0, tok::BODY, false, inner, self.ink())?;
            self.label_in(dc, t("about.updates.store"), m.span(tl, inner), r.top + 28.0, tok::FOOTNOTE, false, inner, self.dim())?;
            return Ok(r.bottom);
        }
        let status = updates::status(&self.upd, self.upd_checking, updates::CURRENT);
        let available = matches!(status, Status::Available(_));
        let status_text = match (&status, phase_text(&self.upd, updates::CURRENT)) {
            (Status::Checking, _) => t("about.updates.checking").to_string(),
            (Status::Available(_), Some(p)) => p,
            (Status::Available(v), None) => tf("about.updates.available", &[("v", v)]),
            (Status::Failed, _) => t("about.updates.failed").to_string(),
            (Status::Never, _) => t("about.updates.never").to_string(),
            (Status::UpToDate, _) => t("about.updates.up_to_date").to_string(),
        };
        if moving(&self.upd) {
            self.poll_updates();
        }
        // This copy installs it itself: "Update now" (download and install)
        // or, once staged, "Restart to update". Otherwise the page.
        let installable = self.upd_self && crate::autoupdate::target_here(&self.upd).is_some();
        let install_label = match (installable, self.upd.pending.phase) {
            (false, _) | (true, Phase::Downloading | Phase::Verifying | Phase::Installing) => None,
            (true, Phase::Ready) if self.upd.pending.version == self.upd.latest => Some(t("about.updates.restart")),
            (true, _) => Some(t("about.updates.update_now")),
        };
        let row = self.card_row(dc, "updates", y, h1, left, right, !joined, false)?;
        let y = row.top;
        let ch = tok::CONTROL_H / 2.0;

        // Buttons, right to left: Update now / Restart to update, or
        // Download (when there is one), then Check now.
        let cy = y + h1 / 2.0;
        let mut bx = cr;
        let x0 = bx;
        if let Some(label) = install_label {
            let r = rect(bx - self.text_width(label, tok::BODY) - 32.0, cy - ch, bx, cy + ch);
            self.accent_button(dc, m.r(r), label, self.hover == Some(Hit::UpdateInstall))?;
            if let Some(r) = clip_to(m.r(r), clip) {
                hits.push((r, Hit::UpdateInstall));
            }
            bx = r.left - 8.0;
        } else if available && !installable {
            let label = t("about.updates.download");
            let r = rect(bx - self.text_width(label, tok::BODY) - 32.0, cy - ch, bx, cy + ch);
            self.accent_button(dc, m.r(r), label, self.hover == Some(Hit::UpdateDownload))?;
            if let Some(r) = clip_to(m.r(r), clip) {
                hits.push((r, Hit::UpdateDownload));
            }
            bx = r.left - 8.0;
        }
        let label = t("about.updates.check_now");
        let r = rect(bx - self.text_width(label, tok::BODY) - 32.0, cy - ch, bx, cy + ch);
        self.button(dc, m.r(r), label, self.hover == Some(Hit::UpdateCheck) && !self.upd_checking)?;
        if let Some(r) = clip_to(m.r(r), clip) {
            hits.push((r, Hit::UpdateCheck));
        }
        self.note(super::layout::Kind::Ctl { kind: super::layout::CtlKind::Button, cy }, "updates", m.r(rect(r.left, cy - ch, x0, cy + ch)));
        let maxw = r.left - tok::ROW_PAD - tl;
        let tx = m.span(tl, maxw);
        self.label_in(dc, t("about.updates"), tx, y + 8.0, tok::BODY, false, maxw, self.ink())?;
        self.label_in(dc, &status_text, tx, y + 28.0, tok::FOOTNOTE, available, maxw, if available { self.ink() } else { self.dim() })?;

        // Automatic updates: checked and (for a copy that can) installed.
        let row = self.card_row(dc, "updates.auto", row.bottom, tok::ROW_MIN, left, right, false, true)?;
        let ry = row.top + tok::ROW_MIN / 2.0;
        self.note(super::layout::Kind::Ctl { kind: super::layout::CtlKind::Toggle, cy: ry }, "updates.auto", m.r(rect(cr - tok::SWITCH_W, ry - tok::SWITCH_H / 2.0, cr, ry + tok::SWITCH_H / 2.0)));
        self.row_text(dc, hits, "updates.auto", row, ry, cr - tok::SWITCH_W, t("about.updates.auto"), Some(super::tooltip::card_tip("about.updates.auto.hint")), clip)?;
        self.toggle_switch(dc, m, cr, ry, self.cfg.updates.auto_check, self.hover == Some(Hit::UpdateAuto), "updates.auto_check")?;
        if let Some(r) = clip_to(m.r(rect(cr - tok::SWITCH_W - 8.0, ry - 16.0, cr + 4.0, ry + 16.0)), clip) {
            hits.push((r, Hit::UpdateAuto));
        }
        Ok(row.bottom)
    }

    /// Reads update.json every second from now on (until nothing moves).
    fn poll_updates(&self) {
        if !self.upd_polling.replace(true) {
            unsafe {
                windows::Win32::UI::WindowsAndMessaging::SetTimer(Some(self.hwnd), UPD_POLL_TIMER, 1000, None);
            }
        }
    }

    /// The poll timer: read update.json again; stop once nothing moves.
    pub(super) fn update_poll(&mut self) {
        if !self.upd_checking {
            self.upd = updates::load_state();
        }
        if !moving(&self.upd) && self.upd_polling.replace(false) {
            unsafe {
                let _ = windows::Win32::UI::WindowsAndMessaging::KillTimer(Some(self.hwnd), UPD_POLL_TIMER);
            }
        }
        self.redraw();
    }

    pub(super) fn click_update(&mut self, h: Hit) {
        match h {
            Hit::UpdateCheck => {
                if self.upd_checking || self.upd_channel != Channel::Direct {
                    return;
                }
                self.upd_checking = true;
                let hwnd = self.hwnd.0 as isize;
                let spawned = std::thread::Builder::new().name("update-check".into()).spawn(move || {
                    updates::check_from_settings();
                    unsafe {
                        let _ = PostMessageW(Some(HWND(hwnd as *mut _)), WM_UPDATE_DONE, WPARAM(0), LPARAM(0));
                    }
                });
                if spawned.is_err() {
                    self.upd_checking = false;
                }
                self.redraw();
            }
            Hit::UpdateDownload => {
                if updates::is_https_url(&self.upd.url) {
                    extra::open_url(&self.upd.url);
                }
            }
            Hit::UpdateAuto => {
                let on = !self.cfg.updates.auto_check;
                (self.cfg.updates.auto_check, self.cfg.updates.auto_install) = (on, on);
                self.animate_switch("updates.auto_check");
                self.touch("updates.auto_check");
                self.touch("updates.auto_install");
                self.send_preview();
            }
            Hit::UpdateInstall => {
                if !self.upd_self || self.upd_checking || moving(&self.upd) {
                    return;
                }
                // The dock does it (it quits for the install); with no dock
                // running, a helper process of ours does.
                if !self.client.install_update(false) {
                    let started = std::env::current_exe().and_then(|exe| std::process::Command::new(exe).arg("--install-update").spawn());
                    if let Err(e) = started {
                        crate::log!("settings: could not start the update: {e}");
                    }
                }
                // Show "downloading" / "installing" as it happens.
                self.poll_updates();
                self.redraw();
            }
            _ => {}
        }
    }

    /// "Check now" is done: show what update.json says now.
    pub(super) fn update_done(&mut self) {
        self.upd = updates::load_state();
        self.upd_checking = false;
        self.redraw();
    }
}

#[cfg(test)]
mod tests {
    /// Messages posted to the settings window must not share a number
    /// (WM_APP + 10 was taken twice once: "show page" and "check done").
    #[test]
    fn settings_window_messages_are_distinct() {
        let ids = [super::super::extra::WM_ICON_READY, super::super::WM_SHOW_PAGE_NOW, super::WM_UPDATE_DONE, super::super::WM_GPU_LOST, super::super::WM_ASK_LP_RESET];
        for (i, a) in ids.iter().enumerate() {
            for b in &ids[i + 1..] {
                assert_ne!(a, b, "duplicate settings message id {a}");
            }
        }
    }
}
