//! The first-run flow (`--settings --welcome`): two short steps over the
//! whole window. (1) Hide the Windows taskbar? (2) the look: theme,
//! language and "Start with Windows". Every choice applies and saves at
//! once, like everywhere in Settings (apply.rs). Get started, Skip, Esc or
//! closing the window end the flow (`welcome_done`), so it does not come
//! back; what was chosen stays. Back / Next move between the steps, the
//! dots show where you are.

use windows::core::Result;
use windows::Win32::Foundation::{LPARAM, WPARAM};
use windows::Win32::Graphics::Direct2D::Common::{D2D1_COLOR_F, D2D1_GRADIENT_STOP, D2D_RECT_F};
use windows::Win32::Graphics::Direct2D::{
    ID2D1DeviceContext, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE, D2D1_BUFFER_PRECISION_8BPC_UNORM, D2D1_COLOR_INTERPOLATION_MODE_STRAIGHT, D2D1_COLOR_SPACE_SRGB,
    D2D1_ELLIPSE, D2D1_EXTEND_MODE_CLAMP, D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES, D2D1_ROUNDED_RECT,
};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_CLOSE};
use windows_numerics::Vector2;

use super::model::{self, Row};
use super::rows::Mirror;
use super::{clip_to, col, rgba, App, Hit, win_h, win_w};
use crate::config::{Config, Position};
use crate::i18n::t;

/// The steps.
pub(super) const STEPS: usize = 2;
/// Title and subtitle text keys of each step ("": no subtitle).
const TITLES: [(&str, &str); STEPS] = [("welcome.taskbar.title", "welcome.taskbar.subtitle"), ("welcome.look.title", "")];
/// Step 1's cards (their pictures say the rest); 0 hides the taskbar.
const TASKBAR_CARDS: [&str; 2] = ["welcome.taskbar.hide", "welcome.taskbar.keep"];
/// The same cards as a screen reader names them ("Keep the taskbar": the
/// card's word alone does not say what it keeps).
pub(super) const TASKBAR_CARD_NAMES: [&str; 2] = ["welcome.taskbar.hide.name", "welcome.taskbar.keep.name"];
/// Side margins and the bottom bar (Back, dots, Skip, Next).
const MX: f32 = 56.0;
pub(super) const BAR: f32 = 76.0;
/// App icons in the drawings (high contrast uses its text color instead).
const ICON_COLORS: [(f32, f32, f32); 6] = [(0.30, 0.55, 0.96), (0.20, 0.78, 0.35), (1.0, 0.62, 0.04), (1.0, 0.22, 0.37), (0.69, 0.32, 0.87), (0.35, 0.78, 0.98)];

/// Where Back / Next / Skip take the flow.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(super) enum Move {
    /// Nothing to do (Back on the first step).
    Stay,
    /// Show this step.
    Step(usize),
    /// Over before the end (what was chosen stays).
    Skip,
    /// Over: keep the choices.
    Done,
}

/// The step state machine.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub(super) struct Flow {
    pub step: usize,
}

impl Flow {
    pub fn next(&mut self) -> Move {
        if self.step + 1 < STEPS {
            self.step += 1;
            Move::Step(self.step)
        } else {
            Move::Done
        }
    }

    pub fn back(&mut self) -> Move {
        if self.step > 0 {
            self.step -= 1;
            Move::Step(self.step)
        } else {
            Move::Stay
        }
    }

    pub fn skip(&self) -> Move {
        Move::Skip
    }
}

/// What ending the flow saves: what is still waiting to be saved (every
/// choice was applied as it was made), the mark that the flow is over and,
/// with Get started, "Start with Windows" (so the switch shown on is also
/// on in Windows). Skipping keeps what was chosen too.
pub(super) fn ending(done: bool, cfg: &Config, touched: &[String]) -> (Config, Vec<String>) {
    let mut c = cfg.clone();
    c.welcome_done = true;
    let mut keys: Vec<String> = touched.to_vec();
    let extra: &[&str] = if done { &["start_with_windows", "welcome_done"] } else { &["welcome_done"] };
    for k in extra {
        if !keys.iter().any(|x| x == k) {
            keys.push(k.to_string());
        }
    }
    (c, keys)
}

/// A theme as its card draws it.
pub(super) struct Swatch {
    pub light: bool,
    pub bar: D2D1_COLOR_F,
    pub dock: D2D1_COLOR_F,
    pub accent: D2D1_COLOR_F,
}

/// The first-run flow while it is shown (it takes the whole window).
pub(super) struct Welcome {
    pub flow: Flow,
    /// The rows of the step shown.
    pub rows: Vec<Row>,
    /// Started for the first run: the window closes when it ends.
    pub close_after: bool,
    /// One per theme on the appearance list.
    pub swatches: Vec<Swatch>,
    /// Where the step's content starts and how much of it fits (scrolling).
    pub body: std::cell::Cell<(f32, f32)>,
}

impl Welcome {
    pub fn new(close_after: bool, themes: &[(String, String)]) -> Self {
        Welcome { flow: Flow::default(), rows: model::welcome_rows(0), close_after, swatches: swatches(themes), body: std::cell::Cell::new((0.0, win_h())) }
    }
}

fn swatches(themes: &[(String, String)]) -> Vec<Swatch> {
    let sys = crate::syslook::current();
    let dir = crate::config::app_dir();
    let solid = |c: crate::theme::Rgba| D2D1_COLOR_F { a: (c.a as f32 / 255.0).max(0.85), ..rgba(c) };
    themes
        .iter()
        .map(|(src, _)| {
            let th = crate::theme::Theme::load(src, &dir).resolve(&sys);
            Swatch { light: th.is_light, bar: solid(th.topbar_tint()), dock: solid(th.dock_tint()), accent: rgba(th.accent()) }
        })
        .collect()
}

fn rect(l: f32, t: f32, r: f32, b: f32) -> D2D_RECT_F {
    D2D_RECT_F { left: l, top: t, right: r, bottom: b }
}

fn alpha(c: D2D1_COLOR_F, k: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F { a: c.a * k, ..c }
}

/// What a drawing of the screen shows.
struct Scene {
    topbar: bool,
    taskbar: bool,
    position: Position,
    /// The dock's icon size (config DIPs).
    icon: f32,
    autohide: bool,
}

impl App {
    fn icon_color(&self, i: usize) -> D2D1_COLOR_F {
        match self.hc {
            Some(h) => alpha(rgba(h.text), if i % 2 == 0 { 0.9 } else { 0.55 }),
            None => {
                let (r, g, b) = ICON_COLORS[i % ICON_COLORS.len()];
                col(r, g, b, 1.0)
            }
        }
    }

    /// A screen in miniature: wallpaper, NopBar's top bar, the Windows
    /// taskbar (or not) and the dock where it sits.
    unsafe fn scene(&self, dc: &ID2D1DeviceContext, r: D2D_RECT_F, s: &Scene) -> Result<()> {
        let (w, h) = (r.right - r.left, r.bottom - r.top);
        // Bezel, then the wallpaper (a soft accent gradient).
        self.fill_round(dc, self.wash(0.10), rect(r.left - 5.0, r.top - 5.0, r.right + 5.0, r.bottom + 5.0), 10.0)?;
        match self.hc {
            Some(hc) => self.fill_round(dc, rgba(hc.window), r, 5.0)?,
            None => {
                let stops = [
                    D2D1_GRADIENT_STOP { position: 0.0, color: alpha(self.accent, 0.55) },
                    D2D1_GRADIENT_STOP { position: 1.0, color: alpha(self.accent, 0.12) },
                ];
                let coll = dc.CreateGradientStopCollection(&stops, D2D1_COLOR_SPACE_SRGB, D2D1_COLOR_SPACE_SRGB, D2D1_BUFFER_PRECISION_8BPC_UNORM, D2D1_EXTEND_MODE_CLAMP, D2D1_COLOR_INTERPOLATION_MODE_STRAIGHT)?;
                let lg = D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES { startPoint: Vector2 { X: r.left, Y: r.top }, endPoint: Vector2 { X: r.right, Y: r.bottom } };
                let gb = dc.CreateLinearGradientBrush(&lg, None, &coll)?;
                dc.FillRoundedRectangle(&D2D1_ROUNDED_RECT { rect: r, radiusX: 5.0, radiusY: 5.0 }, &gb);
            }
        }
        if self.hc.is_some() {
            let b = dc.CreateSolidColorBrush(&self.ink(), None)?;
            dc.DrawRoundedRectangle(&D2D1_ROUNDED_RECT { rect: r, radiusX: 5.0, radiusY: 5.0 }, &b, 1.0, None);
        }
        dc.PushAxisAlignedClip(&r, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
        let glass = match self.hc {
            Some(_) => self.wash(0.2),
            None if self.light => col(1.0, 1.0, 1.0, 0.62),
            None => col(0.08, 0.08, 0.08, 0.62),
        };
        let faint = self.wash(0.45);
        // NopBar's top bar: logo, menus, clock.
        let bar_h = (h * 0.075).max(7.0);
        if s.topbar {
            self.fill_round(dc, glass, rect(r.left, r.top, r.right, r.top + bar_h), 0.0)?;
            let cy = r.top + bar_h / 2.0;
            self.dot(dc, self.ink(), r.left + 8.0, cy, bar_h * 0.18)?;
            let mut x = r.left + 16.0;
            for k in [0.07, 0.05, 0.06, 0.04] {
                self.fill_round(dc, faint, rect(x, cy - 1.0, x + w * k, cy + 1.0), 1.0)?;
                x += w * k + 6.0;
            }
            self.fill_round(dc, faint, rect(r.right - 8.0 - w * 0.08, cy - 1.0, r.right - 8.0, cy + 1.0), 1.0)?;
        }
        // The Windows taskbar: Start and pinned icons in the middle, the
        // clock at the right.
        let tb_h = if s.taskbar { (h * 0.1).max(9.0) } else { 0.0 };
        if s.taskbar {
            let solid = match self.hc {
                Some(hc) => rgba(hc.window),
                None if self.light => col(0.94, 0.94, 0.94, 0.96),
                None => col(0.11, 0.11, 0.11, 0.96),
            };
            let tb = rect(r.left, r.bottom - tb_h, r.right, r.bottom);
            self.fill_round(dc, solid, tb, 0.0)?;
            self.fill_round(dc, self.wash(0.12), rect(tb.left, tb.top, tb.right, tb.top + 1.0), 0.0)?;
            let s_ = tb_h * 0.5;
            let n = 6;
            let total = n as f32 * s_ + (n - 1) as f32 * 4.0;
            let mut x = (r.left + r.right - total) / 2.0;
            let cy = tb.top + tb_h / 2.0;
            for i in 0..n {
                let c = if i == 0 { self.accent } else { alpha(self.icon_color(i), 0.85) };
                self.fill_round(dc, c, rect(x, cy - s_ / 2.0, x + s_, cy + s_ / 2.0), s_ * 0.22)?;
                x += s_ + 4.0;
            }
            self.fill_round(dc, faint, rect(r.right - 8.0 - w * 0.07, cy - 1.0, r.right - 8.0, cy + 1.0), 1.0)?;
        }
        // The dock: six icons on glass, along its edge.
        let unit = h * 0.085 * (s.icon / 52.0).clamp(0.5, 1.9);
        let gap = unit * 0.25;
        let pad = unit * 0.3;
        let n = 6;
        let long = n as f32 * unit + (n - 1) as f32 * gap + 2.0 * pad;
        let short = unit + 2.0 * pad;
        let edge = 4.0;
        let (top_lim, bottom_lim) = (r.top + if s.topbar { bar_h } else { 0.0 }, r.bottom - tb_h);
        let mid_y = (top_lim + bottom_lim) / 2.0;
        let cx = (r.left + r.right) / 2.0;
        // Hidden: tucked past its edge, a sliver left to show where it is.
        let hide = if s.autohide { short - 3.0 } else { 0.0 };
        let fade = if s.autohide { 0.6 } else { 1.0 };
        let dock = match s.position {
            Position::Bottom => rect(cx - long / 2.0, bottom_lim - edge - short + hide, cx + long / 2.0, bottom_lim - edge + hide),
            Position::Left => rect(r.left + edge - hide, mid_y - long / 2.0, r.left + edge + short - hide, mid_y + long / 2.0),
            Position::Right => rect(r.right - edge - short + hide, mid_y - long / 2.0, r.right - edge + hide, mid_y + long / 2.0),
        };
        dc.PushAxisAlignedClip(&rect(r.left, top_lim, r.right, bottom_lim), D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
        self.fill_round(dc, alpha(glass, fade), dock, pad + unit * 0.2)?;
        for i in 0..n {
            let off = pad + i as f32 * (unit + gap);
            let ic = match s.position {
                Position::Bottom => rect(dock.left + off, dock.top + pad, dock.left + off + unit, dock.top + pad + unit),
                _ => rect(dock.left + pad, dock.top + off, dock.left + pad + unit, dock.top + off + unit),
            };
            self.fill_round(dc, alpha(self.icon_color(i), fade), ic, unit * 0.24)?;
        }
        dc.PopAxisAlignedClip();
        if s.autohide {
            // Where it slides in from.
            let e = match s.position {
                Position::Bottom => rect(cx - long / 2.0, bottom_lim - 2.0, cx + long / 2.0, bottom_lim),
                Position::Left => rect(r.left, mid_y - long / 2.0, r.left + 2.0, mid_y + long / 2.0),
                Position::Right => rect(r.right - 2.0, mid_y - long / 2.0, r.right, mid_y + long / 2.0),
            };
            self.fill_round(dc, self.accent, e, 1.0)?;
        }
        dc.PopAxisAlignedClip();
        Ok(())
    }

    unsafe fn dot(&self, dc: &ID2D1DeviceContext, c: D2D1_COLOR_F, x: f32, y: f32, rad: f32) -> Result<()> {
        let b = dc.CreateSolidColorBrush(&c, None)?;
        dc.FillEllipse(&D2D1_ELLIPSE { point: Vector2 { X: x, Y: y }, radiusX: rad, radiusY: rad }, &b);
        Ok(())
    }

    /// A radio mark centered on (x, cy): a filled disc in the "on" tone
    /// with a dot of its ink when picked, a tone step when not (an outline
    /// only in high contrast).
    unsafe fn radio(&self, dc: &ID2D1DeviceContext, x: f32, cy: f32, on: bool) -> Result<()> {
        let e = D2D1_ELLIPSE { point: Vector2 { X: x, Y: cy }, radiusX: 8.0, radiusY: 8.0 };
        if let Some(h) = self.hc {
            let ring = dc.CreateSolidColorBrush(&if on { rgba(h.highlight) } else { rgba(h.text) }, None)?;
            dc.DrawEllipse(&e, &ring, 1.5, None);
            if on {
                dc.FillEllipse(&D2D1_ELLIPSE { radiusX: 4.0, radiusY: 4.0, ..e }, &ring);
            }
            return Ok(());
        }
        let disc = dc.CreateSolidColorBrush(&if on { self.on_tone() } else { self.wash(0.16) }, None)?;
        dc.FillEllipse(&e, &disc);
        if on {
            let dot = dc.CreateSolidColorBrush(&self.on_ink(), None)?;
            dc.FillEllipse(&D2D1_ELLIPSE { radiusX: 3.5, radiusY: 3.5, ..e }, &dot);
        }
        Ok(())
    }

    /// A card that can be picked: chosen (a deeper tone, its radio filled;
    /// an outline only in high contrast) or hovered.
    unsafe fn pick_card(&self, dc: &ID2D1DeviceContext, r: D2D_RECT_F, on: bool, hot: bool) -> Result<()> {
        self.card(dc, r)?;
        if on && self.hc.is_none() {
            self.fill_round(dc, self.wash(0.08), r, self.card_r())?;
        } else if hot {
            self.fill_round(dc, self.wash(0.04), r, self.card_r())?;
        }
        if on && self.hc.is_some() {
            let b = dc.CreateSolidColorBrush(&self.accent, None)?;
            dc.DrawRoundedRectangle(&D2D1_ROUNDED_RECT { rect: rect(r.left + 1.0, r.top + 1.0, r.right - 1.0, r.bottom - 1.0), radiusX: crate::theme::concentric(self.card_r(), 1.0), radiusY: crate::theme::concentric(self.card_r(), 1.0) }, &b, 2.0, None);
        }
        Ok(())
    }

    /// The whole window while the flow is shown. Returns the step's height.
    pub(super) unsafe fn paint_welcome(&self, dc: &ID2D1DeviceContext, hits: &mut Vec<(D2D_RECT_F, Hit)>, anchor: &mut Option<D2D_RECT_F>) -> Result<f32> {
        let Some(wel) = &self.welcome else { return Ok(0.0) };
        let step = wel.flow.step.min(STEPS - 1);
        let rtl = crate::i18n::rtl();
        let (left, right) = (MX, win_w() - MX);
        let m = Mirror { on: rtl, left: 0.0, right: win_w() };
        let width = right - left;

        // Header: where we are, the step's question (what it means behind
        // an "i" after it).
        self.flow.set(super::layout::Block::Start);
        let head = t("welcome.title");
        let hx = if rtl { right - self.text_w(head, super::rows::tok::BODY, true) } else { left };
        self.label(dc, head, hx, 26.0, super::rows::tok::BODY, true, if self.hc.is_some() { self.ink() } else { self.accent })?;
        let (title, sub) = TITLES[step];
        let info = if sub.is_empty() { 0.0 } else { super::rows::tok::INFO_W };
        let tw = self.text_w(t(title), super::rows::tok::LARGE_TITLE, true).min(width - info);
        dc.PushAxisAlignedClip(&rect(left, 44.0, right, 86.0), D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
        let drawn = self.label(dc, t(title), if rtl { right - tw } else { left }, 48.0, super::rows::tok::LARGE_TITLE, true, self.ink());
        dc.PopAxisAlignedClip();
        drawn?;
        if !sub.is_empty() {
            let cx = left + tw + super::rows::tok::INFO_GAP + super::rows::tok::INFO_D / 2.0;
            self.paint_info(dc, hits, super::tooltip::card_tip(sub), m.x(cx), 48.0 + super::rows::tok::LARGE_TITLE * 0.67, rect(0.0, 0.0, win_w(), win_h()))?;
        }
        let body_top = 88.0 + 22.0;
        let clip = rect(0.0, body_top - 6.0, win_w(), win_h() - BAR);
        wel.body.set((body_top, clip.bottom - body_top));

        dc.PushAxisAlignedClip(&clip, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
        let start = body_top - self.scroll;
        let end = match step {
            0 => self.paint_taskbar_step(dc, hits, start, left, right, clip, rtl)?,
            _ => self.paint_look_step(dc, hits, wel, start, left, right, clip, rtl, anchor)?,
        } + super::layout::PAGE_END;
        dc.PopAxisAlignedClip();

        // The bottom bar.
        self.fill_round(dc, self.wash(0.12), rect(0.0, win_h() - BAR, win_w(), win_h() - BAR + 1.0), 0.0)?;
        let by = win_h() - BAR / 2.0;
        let btn = |l: f32, w: f32| rect(l, by - 18.0, l + w, by + 18.0);
        let next_label = t(self.welcome_next_label());
        let nw = (self.text_w(next_label, super::rows::tok::BODY, true) + 44.0).max(120.0);
        let next = btn(right - nw, nw);
        let skip_label = t("welcome.skip");
        let sw = self.text_w(skip_label, super::rows::tok::BODY, false) + 32.0;
        let skip = btn(next.left - 8.0 - sw, sw);
        let back_label = t("welcome.back");
        let bw = (self.text_w(back_label, super::rows::tok::BODY, false) + 40.0).max(96.0);
        let back = btn(left, bw);
        if step > 0 {
            let r = m.r(back);
            self.fill_round(dc, self.ctl_fill(if self.hover == Some(Hit::WelcomeBack) { crate::theme::State::Hover } else { crate::theme::State::Rest }), r, self.ctl_r())?;
            self.centered(dc, back_label, r, super::rows::tok::BODY, false, self.ink())?;
            hits.push((r, Hit::WelcomeBack));
        }
        let r = m.r(skip);
        if self.hover == Some(Hit::WelcomeSkip) {
            self.fill_round(dc, self.wash(0.06), r, self.ctl_r())?;
        }
        self.centered(dc, skip_label, r, super::rows::tok::BODY, false, self.dim())?;
        hits.push((r, Hit::WelcomeSkip));
        let r = m.r(next);
        let mut c = self.accent;
        if self.hover == Some(Hit::WelcomeNext) && self.hc.is_none() {
            c.a *= 0.88;
        }
        self.fill_round(dc, c, r, self.ctl_r())?;
        if self.hover == Some(Hit::WelcomeNext) && self.hc.is_some() {
            let b = dc.CreateSolidColorBrush(&self.on_accent(), None)?;
            dc.DrawRoundedRectangle(&D2D1_ROUNDED_RECT { rect: rect(r.left + 2.0, r.top + 2.0, r.right - 2.0, r.bottom - 2.0), radiusX: crate::theme::concentric(self.ctl_r(), 2.0), radiusY: crate::theme::concentric(self.ctl_r(), 2.0) }, &b, 1.5, None);
        }
        self.centered(dc, next_label, r, super::rows::tok::BODY, true, self.on_accent())?;
        hits.push((r, Hit::WelcomeNext));
        // Progress dots, the current one a pill (in reading order).
        let mut x = win_w() / 2.0 - (STEPS as f32 * 8.0 + 14.0 + (STEPS - 1) as f32 * 8.0) / 2.0;
        for i in 0..STEPS {
            let w = if i == step { 22.0 } else { 8.0 };
            let c = if i == step { self.accent } else if i < step { alpha(self.accent, 0.45) } else { self.wash(0.25) };
            self.fill_round(dc, c, m.r(rect(x, by - 4.0, x + w, by + 4.0)), crate::theme::capsule(8.0))?;
            x += w + 8.0;
        }
        Ok(end - start)
    }

    /// Text centered in `r` (cut to it).
    unsafe fn centered(&self, dc: &ID2D1DeviceContext, s: &str, r: D2D_RECT_F, size: f32, bold: bool, c: D2D1_COLOR_F) -> Result<()> {
        let w = self.text_w(s, size, bold);
        let room = r.right - r.left - 12.0;
        dc.PushAxisAlignedClip(&rect(r.left + 6.0, r.top, r.right - 6.0, r.bottom), D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
        let out = self.label(dc, s, (r.left + r.right - w.min(room)) / 2.0, (r.top + r.bottom) / 2.0 - size * 0.68, size, bold, c);
        dc.PopAxisAlignedClip();
        out.map(|_| ())
    }

    /// Step 1: the taskbar hidden or kept, as two big pictures.
    #[allow(clippy::too_many_arguments)]
    unsafe fn paint_taskbar_step(&self, dc: &ID2D1DeviceContext, hits: &mut Vec<(D2D_RECT_F, Hit)>, mut y: f32, left: f32, right: f32, clip: D2D_RECT_F, rtl: bool) -> Result<f32> {
        let m = Mirror { on: rtl, left, right };
        let gap = 16.0;
        let cw = (right - left - gap) / 2.0;
        let art_h = ((cw - 28.0) * 0.56).round();
        let ch = 14.0 + art_h + 14.0 + 24.0 + 12.0;
        let chosen = if self.cfg.hide_taskbar { 0 } else { 1 };
        y = self.block_top(y, super::layout::Block::Card);
        self.note(super::layout::Kind::Picture, "welcome.taskbar", m.r(rect(left, y, right, y + ch)));
        for (k, name) in TASKBAR_CARDS.iter().enumerate() {
            let x = left + k as f32 * (cw + gap);
            let card = m.r(rect(x, y, x + cw, y + ch));
            self.pick_card(dc, card, k == chosen, self.hover == Some(Hit::WelcomeCard(k)))?;
            let scene = Scene { topbar: self.cfg.topbar.enabled, taskbar: k == 1, position: self.cfg.dock.position, icon: self.cfg.dock.icon_size, autohide: false };
            self.scene(dc, rect(card.left + 14.0, y + 14.0, card.right - 14.0, y + 14.0 + art_h), &scene)?;
            let ty = y + 14.0 + art_h + 14.0;
            let (rx, tx) = if rtl { (card.right - 22.0, card.left + 14.0) } else { (card.left + 22.0, card.left + 40.0) };
            self.radio(dc, rx, ty + 11.0, k == chosen)?;
            self.text_box(dc, t(name), tx, ty, super::rows::tok::HEADLINE, true, cw - 54.0, self.ink(), rtl)?;
            if let Some(r) = clip_to(card, clip) {
                hits.push((r, Hit::WelcomeCard(k)));
            }
        }
        Ok(y + ch)
    }

    /// Step 2: the themes as cards, then language and Start with Windows.
    #[allow(clippy::too_many_arguments)]
    unsafe fn paint_look_step(&self, dc: &ID2D1DeviceContext, hits: &mut Vec<(D2D_RECT_F, Hit)>, wel: &Welcome, mut y: f32, left: f32, right: f32, clip: D2D_RECT_F, rtl: bool, anchor: &mut Option<D2D_RECT_F>) -> Result<f32> {
        let m = Mirror { on: rtl, left, right };
        y = self.heading(dc, hits, y, left, right, "welcome.look", t("settings.section.appearance"), None, clip)?;
        y = self.block_top(y, super::layout::Block::Card);
        let gap = 12.0;
        let cols = 3usize;
        let cw = (right - left - gap * (cols as f32 - 1.0)) / cols as f32;
        let ch = 104.0;
        for (i, (src, name)) in self.themes.iter().enumerate() {
            let (cx, cy) = (left + (i % cols) as f32 * (cw + gap), y + (i / cols) as f32 * (ch + gap));
            let card = m.r(rect(cx, cy, cx + cw, cy + ch));
            let on = self.cfg.theme.source == *src;
            self.pick_card(dc, card, on, self.hover == Some(Hit::Theme(i)))?;
            if let Some(s) = wel.swatches.get(i) {
                self.theme_art(dc, rect(card.left + 12.0, cy + 12.0, card.right - 12.0, cy + 62.0), s)?;
            }
            let (rx, tx) = if rtl { (card.right - 20.0, card.left + 12.0) } else { (card.left + 20.0, card.left + 36.0) };
            self.radio(dc, rx, cy + 82.0, on)?;
            self.text_box(dc, name, tx, cy + 72.0, super::rows::tok::BODY, false, cw - 48.0, self.ink(), rtl)?;
            if let Some(r) = clip_to(card, clip) {
                hits.push((r, Hit::Theme(i)));
            }
        }
        let lines = self.themes.len().div_ceil(cols).max(1) as f32;
        let grid_h = lines * (ch + gap) - gap;
        self.note(super::layout::Kind::Picture, "welcome.themes", m.r(rect(left, y, right, y + grid_h)));
        y += grid_h;
        self.paint_rows(dc, hits, &wel.rows, y, left, right, clip, rtl, anchor)
    }

    /// A theme in miniature: its top bar and dock over a plain backdrop.
    unsafe fn theme_art(&self, dc: &ID2D1DeviceContext, r: D2D_RECT_F, s: &Swatch) -> Result<()> {
        let back = if s.light { col(0.86, 0.86, 0.86, 1.0) } else { col(0.2, 0.2, 0.2, 1.0) };
        self.fill_round(dc, back, r, 5.0)?;
        if self.hc.is_some() {
            let b = dc.CreateSolidColorBrush(&self.ink(), None)?;
            dc.DrawRoundedRectangle(&D2D1_ROUNDED_RECT { rect: r, radiusX: 5.0, radiusY: 5.0 }, &b, 1.0, None);
        }
        dc.PushAxisAlignedClip(&r, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
        self.fill_round(dc, s.bar, rect(r.left, r.top, r.right, r.top + 8.0), 0.0)?;
        let (w, cx) = (r.right - r.left, (r.left + r.right) / 2.0);
        let unit = 13.0;
        let n = 5;
        let long = n as f32 * unit + (n - 1) as f32 * 4.0 + 10.0;
        let dock = rect(cx - long.min(w - 8.0) / 2.0, r.bottom - unit - 14.0, cx + long.min(w - 8.0) / 2.0, r.bottom - 4.0);
        self.fill_round(dc, s.dock, dock, 6.0)?;
        let mut x = dock.left + 5.0;
        for i in 0..n {
            let c = if i == 0 { s.accent } else { self.icon_color(i) };
            self.fill_round(dc, c, rect(x, dock.top + 5.0, x + unit, dock.top + 5.0 + unit), 3.5)?;
            x += unit + 4.0;
        }
        dc.PopAxisAlignedClip();
        Ok(())
    }

    /// Next, or on the last step "Get started" (text keys).
    pub(super) fn welcome_next_label(&self) -> &'static str {
        let step = self.welcome.as_ref().map(|w| w.flow.step).unwrap_or(0);
        if step + 1 < STEPS { "welcome.next" } else { "welcome.get_started" }
    }

    /// Shows step `step` (its rows, from the top).
    fn show_step(&mut self, step: usize) {
        if let Some(w) = self.welcome.as_mut() {
            w.flow.step = step;
            w.rows = model::welcome_rows(step);
        }
        self.scroll = 0.0;
        self.menu = None;
        self.hover = None;
        self.focus = Some(Hit::WelcomeNext);
        self.redraw();
    }

    /// Back, Next / Get started, Skip.
    pub(super) fn welcome_move(&mut self, h: Hit) {
        let Some(w) = self.welcome.as_mut() else { return };
        let mv = match h {
            Hit::WelcomeBack => w.flow.back(),
            Hit::WelcomeNext => w.flow.next(),
            _ => w.flow.skip(),
        };
        match mv {
            Move::Stay => {}
            Move::Step(s) => self.show_step(s),
            Move::Done => self.end_welcome(true, false),
            Move::Skip => self.end_welcome(false, false),
        }
    }

    /// Step 1's cards: 0 hides the taskbar, 1 keeps it.
    pub(super) fn pick_taskbar(&mut self, k: usize) {
        let hide = k == 0;
        if self.cfg.hide_taskbar != hide {
            self.cfg.hide_taskbar = hide;
            self.touch("hide_taskbar");
            self.send_preview();
        } else {
            self.redraw();
        }
    }

    /// The flow ends (Get started or Skip): what is waiting is saved with
    /// the mark that it is over, and the flow goes once that worked (a
    /// failed save keeps it on screen with its note, tried again).
    /// `closing`: the window is going away (its own save follows).
    pub(super) fn end_welcome(&mut self, done: bool, closing: bool) {
        if self.welcome.is_none() {
            return;
        }
        // The mark that it is over (and, with Get started, Start with
        // Windows) joins what is waiting: saved like any change, with its
        // retries, offline fallback and journal.
        let (c, keys) = ending(done, &self.cfg, &self.touched);
        self.cfg = c;
        for k in keys {
            self.touch(&k);
        }
        // Closing (or signing out): the window's own save follows.
        if closing {
            if let Some(w) = self.welcome.take() {
                crate::log!("welcome: skipped on closing (step {})", w.flow.step + 1);
            }
            return;
        }
        // Not saved: the flow stays (its note says so; tried again).
        if !self.save() {
            return;
        }
        let Some(w) = self.welcome.take() else { return };
        crate::log!("welcome: {} (step {})", if done { "done" } else { "skipped" }, w.flow.step + 1);
        self.menu = None;
        self.scroll = 0.0;
        self.focus = None;
        self.hover = None;
        self.section = 0;
        // Start with Windows did not take: stay, its row says so.
        let startup_ok = !self.startup.is_some_and(|(wanted, actual)| wanted != actual);
        if w.close_after && startup_ok {
            unsafe {
                let _ = PostMessageW(Some(self.hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
            }
            return;
        }
        self.redraw();
    }

    /// Asked (by a second `--settings --welcome`) to show the flow: it
    /// starts over the open window.
    pub(super) fn show_welcome(&mut self) {
        if self.welcome.is_none() {
            self.welcome = Some(Welcome::new(false, &self.themes));
        }
        self.show_step(0);
    }

    /// Opens a page asked for with `--page` (super::page_code): the About
    /// page, and for 2 also "Report a problem", as if clicked there.
    pub(super) fn show_page(&mut self, code: usize) {
        // The first-run flow is not cut short by it.
        if self.welcome.is_some() {
            return;
        }
        self.focus = None;
        self.hover = None;
        // The clock menu's "Focus timer settings…": the timer's page.
        if code == super::PAGE_TOPBAR {
            self.open_row(super::TIMER_ROW);
            return;
        }
        let Some(i) = super::page_target(&self.sections, code) else { return };
        self.click(Hit::Section(i), 0.0);
        if code == 2 {
            self.click(Hit::ReportProblem, 0.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_and_back_walk_the_two_steps() {
        let mut f = Flow::default();
        assert_eq!(f.back(), Move::Stay, "nothing before the first step");
        assert_eq!(f.step, 0);
        assert_eq!(f.next(), Move::Step(1), "the look");
        assert_eq!(f.back(), Move::Step(0));
        assert_eq!(f.next(), Move::Step(1));
        assert_eq!(f.next(), Move::Done, "Next on the last step is Get started");
        assert_eq!(f.step, 1);
        assert_eq!(STEPS, 2, "the taskbar, the look");
        assert_eq!(f.skip(), Move::Skip);
        assert_eq!(Flow { step: 0 }.skip(), Move::Skip, "Skip from anywhere");
    }

    #[test]
    fn skip_keeps_what_was_chosen() {
        let cfg = Config { hide_taskbar: false, ..Default::default() };
        let touched = vec!["hide_taskbar".to_string()];
        let (c, keys) = ending(false, &cfg, &touched);
        assert_eq!(keys, ["hide_taskbar", "welcome_done"]);
        assert!(c.welcome_done && !c.hide_taskbar);
        // Nothing waiting: only the mark lands.
        let (c, keys) = ending(false, &cfg, &[]);
        assert_eq!(keys, ["welcome_done"]);
        let mut dock = Config::default();
        dock.dock.icon_size = 60.0;
        model::apply_keys(&c, &mut dock, &keys);
        assert!(dock.welcome_done);
        assert_eq!(dock.dock.icon_size, 60.0);
    }

    #[test]
    fn done_keeps_the_choices() {
        let cfg = Config { hide_taskbar: false, ..Default::default() };
        let touched = vec!["hide_taskbar".to_string()];
        let (c, keys) = ending(true, &cfg, &touched);
        assert!(c.welcome_done);
        for k in ["hide_taskbar", "start_with_windows", "welcome_done"] {
            assert_eq!(keys.iter().filter(|x| *x == k).count(), 1, "{k} once in {keys:?}");
        }
        let mut dock = Config::default();
        model::apply_keys(&c, &mut dock, &keys);
        assert!(!dock.hide_taskbar && dock.welcome_done && dock.start_with_windows);
        // Start with Windows switched off in the flow: off in the dock too.
        let mut off = cfg.clone();
        off.start_with_windows = false;
        let (c, keys) = ending(true, &off, &touched);
        model::apply_keys(&c, &mut dock, &keys);
        assert!(!dock.start_with_windows);
    }

    #[test]
    fn every_step_text_exists() {
        let has = |k: &str| assert!(crate::i18n::get(k).is_some(), "missing text key {k}");
        for (a, b) in TITLES {
            has(a);
            if !b.is_empty() {
                has(b);
            }
        }
        for k in TASKBAR_CARDS.iter().chain(TASKBAR_CARD_NAMES.iter()) {
            has(k);
        }
        for k in ["welcome.title", "welcome.back", "welcome.next", "welcome.skip", "welcome.get_started", "welcome.tip.off", "settings.section.appearance"] {
            has(k);
        }
    }
}
