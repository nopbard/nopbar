//! "About this PC", from the system menu's first row (panels::Action::
//! AboutPc), as Windows PC information cards (harmony B): a hero card
//! with the device drawn with the desktop wallpaper on its screen beside
//! the PC's name, its maker and model, Windows' edition and version and
//! its build; the facts as 2 x 2 tiles (processor, graphics, memory,
//! Windows' version); the disk as a wide tile with a bar of what is used;
//! and a footer with NopBar's version, a copy button and "More info…"
//! (Settings' About page).
//!
//! Built like the stats panel (stats.rs): one borderless window with the
//! popup menus' material, shadow and motion (popup::chrome, motion), its
//! content drawn into one D2D surface, modal (a nested loop until Escape,
//! a click outside or another window taking the focus); centered on the
//! bar's monitor. The facts are system.rs' (read at the bar's start);
//! what kind of PC it is form.rs'. The device art (assets/devices: a
//! light and a dark set, devices.json giving each image's size and the
//! transparent hole of its screen) is embedded and decoded, the wallpaper
//! (the bar's monitor's, placed as Windows places it, then cover-cropped
//! into the screen) and the disk's space read, on helper threads shared by
//! every opening (Shared): the window opens at once with what is ready
//! and fills in the rest as it comes. The copy button (and right-click,
//! Ctrl+C) copies the facts as text. Screen readers get every value as text and the
//! buttons (crate::a11y); right to left, the cards are reflected (the device
//! image is not).

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

use serde::Deserialize;
use windows::core::{w, Interface, Result, BOOL};
use windows::UI::Composition::Desktop::DesktopWindowTarget;
use windows::UI::Composition::{CompositionStretch, ContainerVisual, SpriteVisual};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Direct2D::Common::{D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_PIXEL_FORMAT, D2D_RECT_F, D2D_SIZE_U};
use windows::Win32::Graphics::Direct2D::{
    ID2D1DeviceContext, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE, D2D1_BITMAP_OPTIONS_NONE, D2D1_BITMAP_PROPERTIES1, D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC, D2D1_ROUNDED_RECT,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::DirectWrite::{IDWriteTextLayout, DWRITE_FONT_WEIGHT};
use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_USE_HOSTBACKDROPBRUSH};
use windows::Win32::Graphics::Gdi::{ClientToScreen, GetMonitorInfoW, HMONITOR, MONITORINFO};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::WinRT::Composition::ICompositorDesktopInterop;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetCapture, GetKeyState, ReleaseCapture, SetCapture, SetFocus, VK_C, VK_CONTROL, VK_ESCAPE, VK_INSERT, VK_RETURN, VK_SHIFT, VK_SPACE, VK_TAB};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows_numerics::{Matrix3x2, Vector2, Vector3};

use super::form::{self, Form};
use super::system::{self, SysInfo};
use crate::a11y::{self, Act, Node, Role, Tree};
use crate::gfx::Gfx;
use crate::glyphs::Glyph;
use crate::i18n::{mirror, t, tf};
use crate::icons::Icon;
use crate::theme::metrics::space;
use crate::theme::ramp::{self, TextStyle};
use crate::theme::{Rgba, State, Theme};
use crate::wallpaper::{Placement, Plan};

// ---- the device art ---------------------------------------------------------

/// One image of assets/devices/devices.json: its size (px) and its
/// screen's transparent hole ([x0, y0, x1, y1] px; None: no screen).
#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct Device {
    pub file: String,
    pub size: [f32; 2],
    pub screen: Option<[f32; 4]>,
}

#[derive(Deserialize)]
struct DevicesFile {
    devices: HashMap<String, Device>,
}

const DEVICES_JSON: &str = include_str!("../../../assets/devices/devices.json");

/// Parses devices.json's text (empty on anything malformed).
pub fn parse_devices(text: &str) -> HashMap<String, Device> {
    serde_json::from_str::<DevicesFile>(text).map(|f| f.devices).unwrap_or_default()
}

/// The device images by name ("umpc-dark").
pub fn devices() -> &'static HashMap<String, Device> {
    static D: OnceLock<HashMap<String, Device>> = OnceLock::new();
    D.get_or_init(|| parse_devices(DEVICES_JSON))
}

/// An image's name for a form and the theme ("laptop-light").
pub fn art_name(f: Form, light: bool) -> String {
    format!("{}-{}", f.art(), if light { "light" } else { "dark" })
}

/// The embedded PNG of `file` (devices.json's name for it).
pub fn art_bytes(file: &str) -> Option<&'static [u8]> {
    macro_rules! art {
        ($($name:literal),* $(,)?) => {
            match file {
                $($name => Some(include_bytes!(concat!("../../../assets/devices/", $name)) as &[u8]),)*
                _ => None,
            }
        };
    }
    art!(
        "laptop-light.png",
        "laptop-dark.png",
        "umpc-light.png",
        "umpc-dark.png",
        "handheld-light.png",
        "handheld-dark.png",
        "tablet-light.png",
        "tablet-dark.png",
        "desktop-light.png",
        "desktop-dark.png",
        "minipc-light.png",
        "minipc-dark.png",
        "allinone-light.png",
        "allinone-dark.png",
    )
}

/// `size` (w, h) shrunk to fit `max` (w, h), aspect kept; never grown.
pub fn fit_in(size: (f32, f32), max: (f32, f32)) -> (f32, f32) {
    if size.0 <= 0.0 || size.1 <= 0.0 {
        return (0.0, 0.0);
    }
    let k = (max.0 / size.0).min(max.1 / size.1).min(1.0e6);
    (size.0 * k, size.1 * k)
}

/// The device's image in the art box `area` (DIP): fitted, centered.
pub fn art_rect(d: &Device, area: D2D_RECT_F) -> D2D_RECT_F {
    let (w, h) = fit_in((d.size[0], d.size[1]), (area.right - area.left, area.bottom - area.top));
    let (cx, cy) = ((area.left + area.right) / 2.0, (area.top + area.bottom) / 2.0);
    rect(cx - w / 2.0, cy - h / 2.0, cx + w / 2.0, cy + h / 2.0)
}

/// Where the screen's hole lands when the image is drawn in `art` (DIP):
/// devices.json's px scaled with the image. None without a screen, or for
/// a hole that is empty or outside the image.
pub fn screen_rect(d: &Device, art: D2D_RECT_F) -> Option<D2D_RECT_F> {
    let [x0, y0, x1, y1] = d.screen?;
    let [w, h] = d.size;
    if w <= 0.0 || h <= 0.0 || x1 <= x0 || y1 <= y0 || x0 < 0.0 || y0 < 0.0 || x1 > w || y1 > h {
        return None;
    }
    let (kx, ky) = ((art.right - art.left) / w, (art.bottom - art.top) / h);
    Some(rect(art.left + x0 * kx, art.top + y0 * ky, art.left + x1 * kx, art.top + y1 * ky))
}

/// The part of a `img_w` x `img_h` desktop picture (the monitor's shape)
/// shown in `screen` (cover: its middle, cropped to the screen's shape:
/// wallpaper::cover_crop), source px.
pub fn cover_src(img_w: u32, img_h: u32, screen: D2D_RECT_F) -> D2D_RECT_F {
    // The shape at a fine grain (DIP x 100), so the crop keeps its ratio.
    let (sw, sh) = (((screen.right - screen.left) * 100.0).round().max(1.0) as u32, ((screen.bottom - screen.top) * 100.0).round().max(1.0) as u32);
    let c = crate::wallpaper::cover_crop(img_w.max(1), img_h.max(1), sw, sh);
    rect(c.X as f32, c.Y as f32, (c.X + c.Width) as f32, (c.Y + c.Height) as f32)
}

// ---- what it says -----------------------------------------------------------

/// A size as Windows' Explorer gives it (1024-based): one decimal from a
/// terabyte (without ".0"), whole gigabytes, whole megabytes under one.
pub fn size_text(bytes: u64, dec: char) -> String {
    const GB: f64 = 1024.0 * 1024.0 * 1024.0;
    let b = bytes as f64;
    if b >= 1024.0 * GB * 0.995 {
        let tb = (b / (1024.0 * GB) * 10.0).round() / 10.0;
        let s = if tb.fract() == 0.0 { format!("{tb:.0}") } else { format!("{tb:.1}").replace('.', &dec.to_string()) };
        format!("{s}TB")
    } else if b >= GB {
        format!("{:.0}GB", (b / GB).round())
    } else {
        format!("{:.0}MB", (b / (1024.0 * 1024.0)).round())
    }
}

/// "1.2TB 중 640GB 사용 가능".
pub fn disk_text(total: u64, free: u64, dec: char) -> String {
    tf("about_pc.disk.value", &[("total", &size_text(total, dec)), ("free", &size_text(free.min(total), dec))])
}

/// The list's rows, (label, value), the unknown left out.
pub fn rows(info: &SysInfo, disk: Option<(u64, u64)>, dec: char) -> Vec<(&'static str, String)> {
    let mut out = Vec::new();
    if !info.cpu.is_empty() {
        out.push((t("about_pc.processor"), system::cpu_short(&info.cpu)));
    }
    if !info.gpu.is_empty() {
        out.push((t("about_pc.graphics"), info.gpu.clone()));
    }
    if info.ram_kb > 0 {
        out.push((t("about_pc.memory"), format!("{}GB", system::ram_gb(info.ram_kb))));
    }
    if let Some((total, free)) = disk {
        out.push((t("about_pc.disk"), disk_text(total, free, dec)));
    }
    if !info.build.is_empty() {
        out.push((t("about_pc.build"), info.build.clone()));
    }
    out
}

/// A fact on a tile: its glyph, label and value, and whether the value is
/// a number shown big (memory).
#[derive(Clone, Debug, PartialEq)]
pub struct Fact {
    pub glyph: Glyph,
    pub label: String,
    pub value: String,
    pub big: bool,
}

/// The tiles, the unknown left out: processor, graphics, memory, and
/// Windows' version (its edition when the version is unknown).
pub fn fact_tiles(info: &SysInfo) -> Vec<Fact> {
    let fact = |glyph, label: &str, value: String, big| Fact { glyph, label: label.to_string(), value, big };
    let mut out = Vec::new();
    if !info.cpu.is_empty() {
        out.push(fact(Glyph::Processor, t("about_pc.processor"), system::cpu_short(&info.cpu), false));
    }
    if !info.gpu.is_empty() {
        out.push(fact(Glyph::Graphics, t("about_pc.graphics"), info.gpu.clone(), false));
    }
    if info.ram_kb > 0 {
        out.push(fact(Glyph::Memory, t("about_pc.memory"), format!("{}GB", system::ram_gb(info.ram_kb)), true));
    }
    let win = if info.version.is_empty() { info.windows.clone() } else { info.version.clone() };
    if !win.is_empty() {
        out.push(fact(Glyph::Window, "Windows", win, false));
    }
    out
}

/// What the window shows: the facts, the rows (the copy's and screen
/// readers'; the disk's held empty until its helper answers) and the
/// layout for them (the disk's tile kept while it is read, so nothing
/// moves when it comes; gone only when it could not be read).
fn shown(info: &SysInfo, disk: Option<(u64, u64)>, disk_in: bool, dec: char) -> (SysInfo, Vec<(&'static str, String)>, Layout) {
    let mut r = rows(info, disk.or((!disk_in).then_some((0, 0))), dec);
    if disk.is_none() {
        for row in r.iter_mut().filter(|(k, _)| *k == t("about_pc.disk")) {
            row.1.clear();
        }
    }
    let lay = layout(fact_tiles(info).len(), disk.is_some() || !disk_in);
    (info.clone(), r, lay)
}

/// "NopBar 0.1.38".
fn version_line() -> String {
    format!("NopBar {}", crate::updates::CURRENT)
}

/// What Ctrl+C copies: name, maker, Windows, then "label: value" lines.
pub fn copy_text(info: &SysInfo, rows: &[(&str, String)]) -> String {
    let mut lines: Vec<String> = [info.pc.clone(), info.maker.clone(), info.os_line()].into_iter().filter(|l| !l.is_empty()).collect();
    lines.extend(rows.iter().map(|(k, v)| format!("{k}: {v}")));
    lines.push(version_line());
    lines.join("\r\n")
}

// ---- layout (harmony B: cards) ------------------------------------------------

/// The window: 392 wide, PAD in from every edge (the panel's 20 corners,
/// its cards' 12: concentric). A hero card (HERO_H) holds the device and
/// the name; under it the facts as 2 x 2 tiles (TILE_H), the disk as one
/// wide tile with its bar, and the footer row (FOOT_H): the version, the
/// copy button, "More info…". Every length is on the 4 DIP grid.
const W: f32 = 392.0;
const PAD: f32 = crate::theme::metrics::PANEL_PAD;
const GAP: f32 = space::S3;
const HERO_H: f32 = 184.0;
/// The hero's art box (in from its start, under and over its edges) and
/// the text column after it (HERO_GAP on, HERO_END from its end).
const ART_IN: f32 = space::S3;
const ART_W: f32 = 180.0;
const ART_V: f32 = space::S5;
const HERO_GAP: f32 = space::S4;
const HERO_END: f32 = 20.0;
/// The facts' tiles: two to a row, their content TILE_PAD in.
const TILE_H: f32 = 88.0;
const TILE_PAD: f32 = crate::theme::metrics::CARD_PAD;
/// A tile's label glyph and the gap after it.
const LABEL_GLYPH: f32 = space::S5;
/// The disk's bar (8 thick: mockup deviation 3) and the line under it.
const BAR_H: f32 = space::S3;
const UNDER_GAP: f32 = space::S3;
/// The footer row (the version CARD_PAD in, the buttons at its end): the
/// buttons CONTROL_H tall, BUTTON_PAD in; the copy button round.
const FOOT_H: f32 = crate::theme::metrics::size::ROW_M;
const BUTTON_H: f32 = crate::theme::metrics::size::CONTROL_H;
const BUTTON_PAD: f32 = crate::theme::metrics::size::BUTTON_PAD_X;
/// Room kept free on a short work area (half above, half below).
const ROOM: f32 = space::S5;
/// How far the wallpaper reaches under the screen's bezel (no seam).
const BLEED: f32 = 0.5;
const SHADOW_M: f32 = crate::theme::metrics::SHADOW_M;

/// The type, all from the ramp (Caption never: 12 DIP at the least, 400
/// or 600): the name Large Title, the hero's lines and the tiles' labels
/// Footnote, the tiles' values Headline (memory a Title number), the
/// disk's free space Body Strong, the button Body, the version Footnote.
const T_NAME: TextStyle = ramp::LARGE_TITLE;
const T_LINE: TextStyle = ramp::FOOTNOTE;
const T_LABEL: TextStyle = ramp::FOOTNOTE;
const T_VALUE: TextStyle = ramp::HEADLINE;
const T_BIG: TextStyle = ramp::TITLE;
const T_FREE: TextStyle = ramp::BODY_STRONG;
const T_BUTTON: TextStyle = ramp::BODY;
const T_FOOT: TextStyle = ramp::FOOTNOTE;

/// A ramp style's DirectWrite weight.
fn weight(s: TextStyle) -> DWRITE_FONT_WEIGHT {
    DWRITE_FONT_WEIGHT(s.weight as i32)
}

/// Where each block goes (DIP, left to right; reflected when right to
/// left).
#[derive(Clone, Debug, PartialEq)]
pub struct Layout {
    pub hero: D2D_RECT_F,
    /// The art's box in the hero, and the text column after it (x0, x1).
    pub art: D2D_RECT_F,
    pub text: (f32, f32),
    pub tiles: Vec<D2D_RECT_F>,
    pub disk: Option<D2D_RECT_F>,
    pub foot: D2D_RECT_F,
    pub height: f32,
}

/// Pure: the window for `n_tiles` facts (two to a row) and the disk's
/// tile or not.
pub fn layout(n_tiles: usize, disk: bool) -> Layout {
    let hero = rect(PAD, PAD, W - PAD, PAD + HERO_H);
    let art = rect(hero.left + ART_IN, hero.top + ART_V, hero.left + ART_IN + ART_W, hero.bottom - ART_V);
    let text = (art.right + HERO_GAP, hero.right - HERO_END);
    let mut y = hero.bottom + GAP;
    let half = (W - 2.0 * PAD - GAP) / 2.0;
    let mut tiles = Vec::with_capacity(n_tiles);
    for k in 0..n_tiles {
        let x = PAD + (k % 2) as f32 * (half + GAP);
        let top = y + (k / 2) as f32 * (TILE_H + GAP);
        tiles.push(rect(x, top, x + half, top + TILE_H));
    }
    y += n_tiles.div_ceil(2) as f32 * (TILE_H + GAP);
    let disk = disk.then(|| {
        let r = rect(PAD, y, W - PAD, y + TILE_H);
        y += TILE_H + GAP;
        r
    });
    let foot = rect(PAD, y, W - PAD, y + FOOT_H);
    Layout { hero, art, text, tiles, disk, foot, height: foot.bottom + PAD }
}

/// The window's rect (px, without the shadow margin) centered in the work
/// area `work`, and the factor it is shrunk by to fit a short one (1:
/// not).
pub fn place(work: RECT, s: f32, h: f32) -> (RECT, f32) {
    let room = ((work.bottom - work.top) as f32 - ROOM * s).max(1.0);
    let fit = (room / (h * s)).clamp(0.5, 1.0);
    let (pw, ph) = ((W * s * fit).ceil() as i32, (h * s * fit).ceil() as i32);
    let (cx, cy) = ((work.left + work.right) / 2, (work.top + work.bottom) / 2);
    let (l, t) = (cx - pw / 2, cy - ph / 2);
    (RECT { left: l, top: t, right: l + pw, bottom: t + ph }, fit)
}

/// Pure: the footer's buttons for "More info…" `more_w` wide (its words):
/// (copy, more), at the row's end, GAP apart, the copy button round.
pub fn buttons(foot: D2D_RECT_F, more_w: f32) -> (D2D_RECT_F, D2D_RECT_F) {
    let cy = (foot.top + foot.bottom) / 2.0;
    let mw = (more_w + 2.0 * BUTTON_PAD).ceil();
    let more = rect(foot.right - mw, cy - BUTTON_H / 2.0, foot.right, cy + BUTTON_H / 2.0);
    let copy = rect(more.left - GAP - BUTTON_H, more.top, more.left - GAP, more.bottom);
    (copy, more)
}

/// Pure: the name in at most two lines, broken at a space (Korean is not
/// broken inside a word), the two halves as even as they can be: (first,
/// second; even ones alike: the longer first). `fits`: a line fits the
/// column. A name that will not fit is cut by the caller.
pub fn name_lines(name: &str, fits: impl Fn(&str) -> bool) -> (String, Option<String>) {
    if fits(name) {
        return (name.to_string(), None);
    }
    let spaces: Vec<usize> = name.char_indices().filter(|c| c.1 == ' ').map(|c| c.0).collect();
    let halves = |i: usize| (name[..i].trim_end().to_string(), name[i + 1..].trim_start().to_string());
    let even = spaces.iter().map(|&i| halves(i)).filter(|(a, b)| !a.is_empty() && !b.is_empty() && fits(a) && fits(b)).min_by_key(|(a, b)| ((a.chars().count() as i64 - b.chars().count() as i64).abs(), std::cmp::Reverse(a.chars().count())));
    if let Some((a, b)) = even {
        return (a, Some(b));
    }
    // The longest start that fits; the rest (cut) under it.
    match spaces.iter().rev().map(|&i| halves(i)).find(|(a, _)| !a.is_empty() && fits(a)) {
        Some((a, b)) => (a, Some(b)),
        None => (name.to_string(), None),
    }
}

/// Pure: the disk's used share (0..1) for (total, free) bytes.
pub fn used_share(total: u64, free: u64) -> f32 {
    if total == 0 {
        return 0.0;
    }
    (total.saturating_sub(free.min(total)) as f64 / total as f64) as f32
}

fn rect(l: f32, t: f32, r: f32, b: f32) -> D2D_RECT_F {
    D2D_RECT_F { left: l, top: t, right: r, bottom: b }
}

fn rgba(c: crate::theme::Rgba) -> D2D1_COLOR_F {
    D2D1_COLOR_F { r: c.r as f32 / 255.0, g: c.g as f32 / 255.0, b: c.b as f32 / 255.0, a: c.a as f32 / 255.0 }
}

#[derive(Clone, Copy, PartialEq)]
enum Align {
    Start,
    End,
}

/// A text's left edge in `x0..x1` (laid out left to right, reflected when
/// right to left).
fn text_x(align: Align, x0: f32, x1: f32, w: f32, rtl: bool) -> f32 {
    let x = match align {
        Align::Start => x0,
        Align::End => x1 - w,
    };
    mirror::span(rtl, x, w, 0.0, W)
}

/// `r` as drawn (right to left: reflected across the window).
fn placed(r: D2D_RECT_F, rtl: bool) -> D2D_RECT_F {
    let (l, rr) = mirror::edges(rtl, r.left, r.right, 0.0, W);
    rect(l, r.top, rr, r.bottom)
}

// ---- what the helpers bring -------------------------------------------------

/// One kind of answer that helper threads bring for every window (the
/// art, the wallpaper, the disk): one helper at a time (a wallpaper on a
/// share that went away, or a disk that does not answer, keeps one
/// thread, not one per opening), the last answer kept for the next
/// window, and a count of the answers, so a window knows one came after it
/// opened, even from a helper an earlier window started. Windows ask
/// (open) and look (poll) on the UI thread and never wait; the helper
/// writes (finish).
#[derive(Debug)]
pub struct Shared<K, V> {
    /// The key a helper is reading now.
    running: Option<K>,
    answers: u64,
    /// The last answer: its key and what it brought (None: the helper
    /// could not start and nothing was kept).
    last: Option<(K, Option<V>)>,
}

/// What a window sees when it looks.
#[derive(Clone, Debug, PartialEq)]
pub enum Poll<V> {
    /// A helper is still reading (for this key or another): wait.
    Busy,
    /// The answer for its key, come after it asked (None: none could be
    /// had; it stays as it is).
    Done(Option<V>),
    /// Nothing runs and nothing came for its key (a helper for another
    /// monitor answered): ask again.
    Again,
}

impl<K: Clone + PartialEq, V: Clone> Shared<K, V> {
    pub const fn new() -> Self {
        Self { running: None, answers: 0, last: None }
    }

    /// A window wants `key`: what is kept for it (shown meanwhile), the
    /// count of answers it waits past, and whether to start a helper (none
    /// is running; one that is answers this window too).
    pub fn open(&mut self, key: K) -> (Option<V>, u64, bool) {
        let kept = match &self.last {
            Some((k, v)) if *k == key => v.clone(),
            _ => None,
        };
        let start = self.running.is_none();
        if start {
            self.running = Some(key);
        }
        (kept, self.answers, start)
    }

    /// The helper for `key` is done: `v`, or None when it could not start
    /// (what was kept for the key stays).
    pub fn finish(&mut self, key: K, v: Option<V>) {
        let v = v.or_else(|| match self.last.take() {
            Some((k, kept)) if k == key => kept,
            _ => None,
        });
        self.answers += 1;
        self.running = None;
        self.last = Some((key, v));
    }

    /// What a window that asked for `key` at count `since` sees now.
    pub fn poll(&self, key: &K, since: u64) -> Poll<V> {
        match &self.last {
            Some((k, v)) if self.answers > since && k == key => Poll::Done(v.clone()),
            _ if self.running.is_some() => Poll::Busy,
            _ => Poll::Again,
        }
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Asks `cell` for `key`, starting `work` on a helper when none runs:
/// what is kept for it and the count to wait past.
fn ask<K, V>(cell: &'static Mutex<Shared<K, V>>, name: &str, key: K, work: impl FnOnce() -> V + Send + 'static) -> (Option<V>, u64)
where
    K: Clone + PartialEq + Send + 'static,
    V: Clone + Send + 'static,
{
    let (kept, since, start) = lock(cell).open(key.clone());
    if start {
        let k = key.clone();
        if !helper(name, move || {
            let v = work();
            lock(cell).finish(k, Some(v));
        }) {
            lock(cell).finish(key, None);
        }
    }
    (kept, since)
}

/// The device's images (light, dark).
type Arts = (Option<Arc<Icon>>, Option<Arc<Icon>>);
/// The system volume's (total, free) bytes; None: it could not be read.
type Disk = Option<(u64, u64)>;

static ART: Mutex<Shared<Form, Arts>> = Mutex::new(Shared::new());
/// Keyed by the monitor's rect.
static WALL: Mutex<Shared<[i32; 4], Wall>> = Mutex::new(Shared::new());
static DISK: Mutex<Shared<(), Disk>> = Mutex::new(Shared::new());

/// The wallpaper for the device's screen: the bar's monitor's desktop as
/// a picture (`img`, its wallpaper placed as Windows places it on its
/// background color), or, without one, the color alone (`back`; None:
/// Windows has none written).
#[derive(Clone)]
struct Wall {
    img: Option<Arc<Icon>>,
    back: Option<[u8; 3]>,
}

/// The screen's color when there is neither a wallpaper nor a background
/// color (the art's own dark glass).
const SCREEN_OFF: [u8; 3] = [0x1C, 0x1C, 0x1E];

static ART_CACHE: Mutex<Vec<(String, Arc<Icon>)>> = Mutex::new(Vec::new());

/// A device image if it is decoded already (never decodes).
fn art_cached(name: &str) -> Option<Arc<Icon>> {
    lock(&ART_CACHE).iter().find(|(n, _)| n == name).map(|(_, i)| i.clone())
}

/// A device image, decoded once for the run.
fn art_icon(name: &str) -> Option<Arc<Icon>> {
    if let Some(hit) = art_cached(name) {
        return Some(hit);
    }
    let d = devices().get(name)?;
    let icon = Arc::new(crate::icons::decode_bytes_fit(art_bytes(&d.file)?, 1024)?);
    lock(&ART_CACHE).push((name.to_string(), icon.clone()));
    Some(icon)
}

/// A form's images when both are decoded already.
fn arts_cached(form: Form) -> Option<Arts> {
    Some((Some(art_cached(&art_name(form, true))?), Some(art_cached(&art_name(form, false))?)))
}

/// What kind of PC the window draws (P2 #3: late facts decide it again).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FormState {
    /// The facts are not in yet: no device drawn.
    Waiting,
    /// They are late (FACTS_WAIT): the default drawn meanwhile, not final.
    Guess(Form),
    /// From the facts: final.
    Decided(Form),
}

impl FormState {
    /// The form drawn now.
    pub fn form(self) -> Option<Form> {
        match self {
            FormState::Waiting => None,
            FormState::Guess(f) | FormState::Decided(f) => Some(f),
        }
    }

    /// The next state with the form the facts give (None: not in yet) and
    /// whether the wait for them is over.
    pub fn next(self, facts: Option<Form>, late: bool) -> FormState {
        match (self, facts) {
            (FormState::Decided(f), _) => FormState::Decided(f),
            (_, Some(f)) => FormState::Decided(f),
            (FormState::Waiting, None) if late => FormState::Guess(Form::default()),
            (s, None) => s,
        }
    }
}

// ---- the wallpaper as the desktop shows it ----------------------------------

/// A monitor's desktop: its wallpaper file, how Windows places it, the
/// color around (or instead of) it. `lost`: a picture is set but no file
/// of it could be found (a path that is gone, a lookup that failed), so
/// no path is not a plain color then (2026-11-11 confirm review).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Desk {
    pub(crate) path: Option<PathBuf>,
    pub(super) lost: bool,
    pub(super) placement: Option<Placement>,
    pub(super) back: Option<[u8; 3]>,
}

impl Desk {
    pub(crate) fn plain_color(&self) -> Option<[u8; 3]> {
        self.back.filter(|_| self.path.is_none() && !self.lost)
    }
}

/// What a wallpaper lookup said of the picture.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Named {
    /// No picture: the plain color.
    Nothing,
    /// This file, there.
    File(PathBuf),
    /// A picture is set but cannot be found: a path that is gone, or a
    /// lookup that failed (nothing is known of what is shown).
    Lost,
}

impl Named {
    /// Pure: a lookup's answer (`said`; None: it failed), `exists` telling
    /// whether a path is there.
    pub(super) fn of(said: Option<&str>, exists: impl Fn(&std::path::Path) -> bool) -> Named {
        match said.map(str::trim) {
            None => Named::Lost,
            Some("") => Named::Nothing,
            Some(p) if exists(std::path::Path::new(p)) => Named::File(PathBuf::from(p)),
            Some(_) => Named::Lost,
        }
    }

    /// Pure: the monitor's own answer, else Windows' one setting (`other`,
    /// asked only when needed): a file either names; no picture only when
    /// both say so; anything else is lost.
    pub(super) fn or(self, other: impl FnOnce() -> Named) -> Named {
        match self {
            Named::File(p) => Named::File(p),
            mine => match (mine, other()) {
                (_, Named::File(p)) => Named::File(p),
                (Named::Nothing, Named::Nothing) => Named::Nothing,
                _ => Named::Lost,
            },
        }
    }

    /// The desktop it makes with `placement` and `back`.
    pub(super) fn desk(self, placement: Option<Placement>, back: Option<[u8; 3]>) -> Desk {
        let lost = self == Named::Lost;
        let path = match self {
            Named::File(p) => Some(p),
            _ => None,
        };
        Desk { path, lost, placement, back }
    }
}

fn exists(p: &std::path::Path) -> bool {
    p.exists()
}

/// IDesktopWallpaper's DESKTOP_WALLPAPER_POSITION as a placement.
pub fn placement_of(pos: i32) -> Option<Placement> {
    match pos {
        0 => Some(Placement::Center),
        1 => Some(Placement::Tile),
        2 => Some(Placement::Stretch),
        3 => Some(Placement::Fit),
        4 => Some(Placement::Fill),
        5 => Some(Placement::Span),
        _ => None,
    }
}

/// A COLORREF (0x00BBGGRR) as [r, g, b].
pub fn colorref_rgb(c: u32) -> [u8; 3] {
    [(c & 0xFF) as u8, ((c >> 8) & 0xFF) as u8, ((c >> 16) & 0xFF) as u8]
}

/// A string value under HKCU ("" when missing).
fn user_string(key: &str, value: &str) -> String {
    use windows::core::HSTRING;
    use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_SZ};
    let mut buf = [0u16; 256];
    let mut size = (buf.len() * 2) as u32;
    let r = unsafe { RegGetValueW(HKEY_CURRENT_USER, &HSTRING::from(key), &HSTRING::from(value), RRF_RT_REG_SZ, None, Some(buf.as_mut_ptr().cast()), Some(&mut size)) };
    if r.is_err() {
        return String::new();
    }
    let len = (size as usize / 2).min(buf.len());
    String::from_utf16_lossy(&buf[..len]).trim_end_matches('\0').to_string()
}

/// The desktop of the monitor at `mon` (px): its own wallpaper
/// (IDesktopWallpaper, per monitor), else Windows' one setting.
pub(crate) fn desk(mon: RECT) -> Desk {
    unsafe { desk_of_monitor(mon) }.unwrap_or_else(|| {
        unasked(crate::wallpaper::current_said().as_deref(), exists).desk(
            crate::wallpaper::placement(&user_string(r"Control Panel\Desktop", "WallpaperStyle"), &user_string(r"Control Panel\Desktop", "TileWallpaper")),
            crate::wallpaper::background_rgb(),
        )
    })
}

/// Pure: the picture when the monitor itself could not be asked
/// (IDesktopWallpaper failed or does not know it): a file Windows' one
/// setting names, else lost. Never "no picture": an empty setting does not
/// tell what that monitor shows.
pub(super) fn unasked(global: Option<&str>, exists: impl Fn(&std::path::Path) -> bool) -> Named {
    Named::Lost.or(|| Named::of(global, exists))
}

/// IDesktopWallpaper's answer for the monitor at `mon` (None: it cannot
/// tell, or does not know that monitor). Needs COM.
unsafe fn desk_of_monitor(mon: RECT) -> Option<Desk> {
    use windows::core::{PCWSTR, PWSTR};
    use windows::Win32::System::Com::{CoCreateInstance, CoTaskMemFree, CLSCTX_ALL};
    use windows::Win32::UI::Shell::{DesktopWallpaper, IDesktopWallpaper};
    let dw: IDesktopWallpaper = CoCreateInstance(&DesktopWallpaper, None, CLSCTX_ALL).ok()?;
    let take = |p: PWSTR| -> String {
        let s = p.to_string().unwrap_or_default();
        CoTaskMemFree(Some(p.0 as *const _));
        s
    };
    // Some(None): the monitor is known but its picture could not be asked.
    let mut path: Option<Option<String>> = None;
    for i in 0..dw.GetMonitorDevicePathCount().ok()? {
        let Ok(id) = dw.GetMonitorDevicePathAt(i) else { continue };
        // Its rect holds the bar's monitor's middle (the same monitor,
        // whatever rounding its coordinates went through).
        let (cx, cy) = ((mon.left + mon.right) / 2, (mon.top + mon.bottom) / 2);
        let hit = dw.GetMonitorRECT(PCWSTR(id.0)).is_ok_and(|r| cx >= r.left && cx < r.right && cy >= r.top && cy < r.bottom);
        if hit {
            path = Some(dw.GetWallpaper(PCWSTR(id.0)).ok().map(take));
        }
        take(id);
        if hit {
            break;
        }
    }
    // An empty path (a slideshow between files), one that is gone or one
    // that could not be asked: Windows' current one; if that has no file
    // either, the picture is lost, not a plain color (Named::or).
    let said = path?;
    let named = Named::of(said.as_deref(), exists).or(|| Named::of(crate::wallpaper::current_said().as_deref(), exists));
    let placement = dw.GetPosition().ok().and_then(|p| placement_of(p.0));
    let back = dw.GetBackgroundColor().ok().map(|c| colorref_rgb(c.0)).or_else(crate::wallpaper::background_rgb);
    Some(named.desk(placement, back))
}

/// The About panel's desktop picture: 640 px at most (a screen of 200
/// DIP at 300 % is 600).
pub const PANEL_PICTURE: u32 = 640;

/// The panel's picture size for a `mw` x `mh` monitor (canvas_within).
#[cfg(test)]
pub fn canvas_size(mw: u32, mh: u32) -> (u32, u32) {
    canvas_within(mw, mh, PANEL_PICTURE)
}

/// The picture the top bar's clear style reads its ink from
/// (look::measure) is larger: the darkest and brightest points under the
/// text count, and a thin dark band shrunk into the bright around it
/// would read as neither (2026-11-11 confirm review). 1280: a picture
/// pixel is 1.5 screen pixels on a 1920 x 1080 monitor, 3 on 4K (the
/// size of clear's halo, which covers what is finer).
pub const BAR_PICTURE: u32 = 1280;

/// The picture's size for a `mw` x `mh` monitor: its shape, `max` px at
/// most, never grown.
pub fn canvas_within(mw: u32, mh: u32, max: u32) -> (u32, u32) {
    let (mw, mh) = (mw.max(1) as f32, mh.max(1) as f32);
    let k = (max.max(1) as f32 / mw.max(mh)).min(1.0);
    (((mw * k).round() as u32).max(1), ((mh * k).round() as u32).max(1))
}

/// One axis of an image placed on the desktop picture (wallpaper::plan):
/// the part of the source decoded (`src`, `src_len` px), its length
/// decoded (`len`), where it starts on the picture (`at`), and `whole`:
/// the image entire, repeated from `at` (a tile shorter than the picture).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Span {
    pub src: u32,
    pub src_len: u32,
    pub len: u32,
    pub at: i32,
    pub whole: bool,
}

/// The axis of a source `s` px long, drawn `d` long at `at` on a picture
/// `c` long: only the part on the picture is decoded (a centered image
/// larger than the monitor stays the picture's size); None when none of
/// it is on it.
pub fn axis(s: u32, d: u32, at: i32, c: u32, tile: bool) -> Option<Span> {
    if s == 0 || d == 0 || c == 0 {
        return None;
    }
    if tile && d < c {
        return Some(Span { src: 0, src_len: s, len: d, at, whole: true });
    }
    let (at64, d64) = (at as i64, d as i64);
    let (v0, v1) = (at64.max(0), (at64 + d64).min(c as i64));
    if v1 <= v0 {
        return None;
    }
    let k = s as f64 / d as f64;
    let s0 = (((v0 - at64) as f64 * k).floor() as u32).min(s - 1);
    let s1 = (((v1 - at64) as f64 * k).ceil() as u32).clamp(s0 + 1, s);
    Some(Span { src: s0, src_len: s1 - s0, len: (v1 - v0) as u32, at: v0 as i32, whole: false })
}

/// Span: one image covers the box around every display (`virt`, as fill
/// covers one), and each shows its own part. The part of a `sw` x `sh`
/// image the monitor at `mon` shows (source px; [x, y, w, h]).
pub fn span_src(sw: u32, sh: u32, virt: RECT, mon: RECT) -> [i32; 4] {
    let (vw, vh) = ((virt.right - virt.left).max(1) as u32, (virt.bottom - virt.top).max(1) as u32);
    let c = crate::wallpaper::cover_crop(sw.max(1), sh.max(1), vw, vh);
    let k = c.Width as f64 / vw as f64;
    let px = |v: i32| (v as f64 * k).round() as i32;
    let x = (c.X + px(mon.left - virt.left)).clamp(0, sw as i32 - 1);
    let y = (c.Y + px(mon.top - virt.top)).clamp(0, sh as i32 - 1);
    let w = px(mon.right - mon.left).clamp(1, sw as i32 - x);
    let h = px(mon.bottom - mon.top).clamp(1, sh as i32 - y);
    [x, y, w, h]
}

/// The box around every display (px).
fn virtual_screen() -> RECT {
    unsafe {
        let (x, y) = (GetSystemMetrics(SM_XVIRTUALSCREEN), GetSystemMetrics(SM_YVIRTUALSCREEN));
        RECT { left: x, top: y, right: x + GetSystemMetrics(SM_CXVIRTUALSCREEN), bottom: y + GetSystemMetrics(SM_CYVIRTUALSCREEN) }
    }
}

/// Where the decoded part goes along an axis: once, or (a whole tile)
/// repeated to the picture's end.
pub fn steps(a: Span, c: u32) -> Vec<i32> {
    if !a.whole {
        return vec![a.at];
    }
    (0..).map(|i| a.at + i * a.len as i32).take_while(|&p| p < c as i32).collect()
}

/// `img` (premultiplied BGRA, `iw` x `ih`) over `canvas` (opaque BGRA, `w`
/// x `h`) at (`x`, `y`), cropped to it.
#[allow(clippy::too_many_arguments)]
fn blit(canvas: &mut [u8], w: u32, h: u32, img: &[u8], iw: u32, ih: u32, x: i32, y: i32) {
    for row in 0..ih as i32 {
        let cy = y + row;
        if cy < 0 || cy >= h as i32 {
            continue;
        }
        for col in 0..iw as i32 {
            let cx = x + col;
            if cx < 0 || cx >= w as i32 {
                continue;
            }
            let s = ((row as u32 * iw + col as u32) * 4) as usize;
            let d = ((cy as u32 * w + cx as u32) * 4) as usize;
            let a = img[s + 3] as u32;
            for c in 0..3 {
                canvas[d + c] = (img[s + c] as u32 + canvas[d + c] as u32 * (255 - a) / 255).min(255) as u8;
            }
            canvas[d + 3] = 255;
        }
    }
}

/// The desktop of the monitor at `mon` as a picture: its wallpaper as
/// Windows places it (fill, fit, stretch, center, tile, or its part of
/// one spanned across `virt`, the box around every display) on its
/// background color, `max` px at most (canvas_within). None without a
/// wallpaper, or one that cannot be read or placed (the color alone stands
/// for it). Needs COM.
pub(crate) fn compose(d: &Desk, mon: RECT, virt: RECT, max: u32) -> Option<Icon> {
    use windows::core::HSTRING;
    use windows::Win32::Foundation::GENERIC_READ;
    use windows::Win32::Graphics::Imaging::{
        CLSID_WICImagingFactory, GUID_WICPixelFormat32bppPBGRA, IWICImagingFactory, WICBitmapDitherTypeNone, WICBitmapInterpolationModeFant, WICBitmapPaletteTypeCustom, WICDecodeMetadataCacheOnDemand, WICRect,
    };
    use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};
    let path = d.path.as_ref()?;
    let (mw, mh) = ((mon.right - mon.left).max(1) as u32, (mon.bottom - mon.top).max(1) as u32);
    let (cw, ch) = canvas_within(mw, mh, max);
    unsafe {
        let factory: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).ok()?;
        let decoder = factory.CreateDecoderFromFilename(&HSTRING::from(path.as_os_str()), None, GENERIC_READ, WICDecodeMetadataCacheOnDemand).ok()?;
        let frame = decoder.GetFrame(0).ok()?;
        let (mut sw, mut sh) = (0u32, 0u32);
        frame.GetSize(&mut sw, &mut sh).ok()?;
        // The part `src` of the image (source px) scaled to `dw` x `dh`.
        let decode = |src: WICRect, dw: u32, dh: u32| -> Option<Vec<u8>> {
            let clipper = factory.CreateBitmapClipper().ok()?;
            clipper.Initialize(&frame, &src).ok()?;
            let scaler = factory.CreateBitmapScaler().ok()?;
            scaler.Initialize(&clipper, dw, dh, WICBitmapInterpolationModeFant).ok()?;
            let conv = factory.CreateFormatConverter().ok()?;
            conv.Initialize(&scaler, &GUID_WICPixelFormat32bppPBGRA, WICBitmapDitherTypeNone, None, 0.0, WICBitmapPaletteTypeCustom).ok()?;
            let mut px = vec![0u8; (dw * dh * 4) as usize];
            conv.CopyPixels(std::ptr::null(), dw * 4, &mut px).ok()?;
            Some(px)
        };
        let pixels = match crate::wallpaper::plan(d.placement, sw, sh, mw, mh, cw, ch) {
            Plan::Cover(crop) => decode(crop, cw, ch)?,
            Plan::Place { w, h, x, y, tile } => {
                let [r, g, b] = d.back.unwrap_or([0, 0, 0]);
                let mut canvas = [b, g, r, 255].repeat((cw * ch) as usize);
                if let (Some(ax), Some(ay)) = (axis(sw, w, x, cw, tile), axis(sh, h, y, ch, tile)) {
                    let src = WICRect { X: ax.src as i32, Y: ay.src as i32, Width: ax.src_len as i32, Height: ay.src_len as i32 };
                    let img = decode(src, ax.len, ay.len)?;
                    for ty in steps(ay, ch) {
                        for tx in steps(ax, cw) {
                            blit(&mut canvas, cw, ch, &img, ax.len, ay.len, tx, ty);
                        }
                    }
                }
                canvas
            }
            Plan::Solid if d.placement == Some(Placement::Span) => {
                let [x, y, w, h] = span_src(sw, sh, virt, mon);
                decode(WICRect { X: x, Y: y, Width: w, Height: h }, cw, ch)?
            }
            Plan::Solid => return None,
        };
        Some(Icon { width: cw, height: ch, pixels })
    }
}

/// The wallpaper for the screen of the monitor at `mon`, kept while its
/// file, placement and color, the monitor and the displays' box are the
/// same.
fn wall_for(mon: RECT) -> Wall {
    type Kept = (Desk, Option<std::time::SystemTime>, [RECT; 2], Wall);
    static KEPT: Mutex<Option<Kept>> = Mutex::new(None);
    let d = desk(mon);
    let rects = [mon, virtual_screen()];
    let modified = d.path.as_ref().and_then(|p| std::fs::metadata(p).and_then(|m| m.modified()).ok());
    if let Some(w) = lock(&KEPT).as_ref().filter(|k| k.0 == d && k.1 == modified && k.2 == rects).map(|k| k.3.clone()) {
        return w;
    }
    let wall = Wall { img: compose(&d, mon, rects[1], PANEL_PICTURE).map(Arc::new), back: d.back };
    // A file that could not be read or found is tried again next time.
    if wall.img.is_some() || (d.path.is_none() && !d.lost) {
        *lock(&KEPT) = Some((d, modified, rects, wall.clone()));
    }
    wall
}

/// What the desktop of a monitor shows, as read from its file.
pub(super) enum Desktop {
    /// No picture: the plain color.
    Plain([u8; 3]),
    /// Its picture placed as Windows places it (at most BAR_PICTURE px).
    Picture(Icon),
    /// A picture that could not be found, read or placed (nothing is
    /// known of what is shown).
    Unreadable,
}

/// The desktop of the monitor at `mon` (px) as Windows shows it, from
/// the wallpaper file (never the screen), a picture spanned across the
/// displays cut from `virt` (the box around them). The bar's clear style
/// reads its ink from it (look::measure). Needs COM; a helper thread's.
pub(super) fn desktop_picture(mon: RECT, virt: RECT) -> Desktop {
    desktop_of(&desk(mon), |d| compose(d, mon, virt, BAR_PICTURE))
}

/// Pure: what desktop `d` shows, its picture made by `compose`: the plain
/// color only when no picture is set at all; a picture set but lost, or
/// one that cannot be read, is Unreadable (look::judged: solid, tried
/// again).
pub(super) fn desktop_of(d: &Desk, compose: impl FnOnce(&Desk) -> Option<Icon>) -> Desktop {
    match (&d.path, d.lost, d.back) {
        (None, false, Some(c)) => Desktop::Plain(c),
        (None, ..) => Desktop::Unreadable,
        (Some(_), ..) => compose(d).map_or(Desktop::Unreadable, Desktop::Picture),
    }
}

/// The box around every display (px).
pub(crate) fn displays_box() -> RECT {
    virtual_screen()
}

/// Runs `f` on a named helper with COM (WIC needs it).
fn helper(name: &str, f: impl FnOnce() + Send + 'static) -> bool {
    std::thread::Builder::new()
        .name(name.into())
        .spawn(move || {
            use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_MULTITHREADED};
            let com = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok();
            f();
            if com {
                unsafe { CoUninitialize() };
            }
        })
        .is_ok()
}

// ---- the keyboard and screen readers ----------------------------------------

/// The footer's two buttons.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Btn {
    /// Copies the facts as text (what right-click and Ctrl+C did, now in
    /// sight).
    Copy,
    /// "More info…": Windows' About page (the dialog's default button).
    More,
}

/// What a key does. Enter presses the focused button, or "More info…" (the
/// default one) when none has the focus; Space presses the focused one
/// only; Tab and Shift+Tab move the focus between the two (wrapping: there
/// is nothing else to go to); Ctrl+C copies. A screen reader's SetFocus is
/// Tab's to that button (asked).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Key {
    Close,
    Press(Btn),
    /// The focus one way (1) or the other (-1).
    Focus(i32),
    /// The focus onto that button.
    FocusOn(Btn),
    Copy,
    Nothing,
}

pub fn key(vk: u16, ctrl: bool, shift: bool, focus: Option<Btn>) -> Key {
    if vk == VK_ESCAPE.0 {
        Key::Close
    } else if vk == VK_RETURN.0 {
        Key::Press(focus.unwrap_or(Btn::More))
    } else if vk == VK_SPACE.0 {
        focus.map_or(Key::Nothing, Key::Press)
    } else if vk == VK_TAB.0 {
        Key::Focus(if shift { -1 } else { 1 })
    } else if ctrl && (vk == VK_C.0 || vk == VK_INSERT.0) {
        Key::Copy
    } else {
        Key::Nothing
    }
}

/// Pure: the focus after Tab (`dir` 1) or Shift+Tab (-1) from `cur`: copy,
/// then "More info…", round again (none yet: the first one that way).
pub fn next_focus(cur: Option<Btn>, dir: i32) -> Btn {
    match (cur, dir > 0) {
        (None, true) | (Some(Btn::More), true) => Btn::Copy,
        (Some(Btn::Copy), true) => Btn::More,
        (None, false) | (Some(Btn::Copy), false) => Btn::More,
        (Some(Btn::More), false) => Btn::Copy,
    }
}

/// A button's node key in the tree.
fn node_of(b: Btn) -> &'static str {
    match b {
        Btn::Copy => "copy",
        Btn::More => "more",
    }
}

/// What a screen reader's request on `node` does.
pub fn asked(node: &str, act: Act) -> Key {
    let b = match node {
        "more" => Btn::More,
        "copy" => Btn::Copy,
        _ => return Key::Nothing,
    };
    match act {
        Act::Invoke => Key::Press(b),
        Act::Focus => Key::FocusOn(b),
        _ => Key::Nothing,
    }
}

/// The node the tree says has the keyboard: the focused button, else none
/// (the window itself).
pub fn tree_focus(focus: Option<Btn>) -> Option<String> {
    focus.map(|b| node_of(b).to_string())
}

/// A raised button's fill for its state (fill_tile: transparent in high
/// contrast, where it is outlined).
pub fn button_fill(theme: &Theme, hover: bool, pressed: bool) -> Rgba {
    theme.fill_tile(if pressed {
        State::Pressed
    } else if hover {
        State::Hover
    } else {
        State::Rest
    })
}

// ---- the window -------------------------------------------------------------

/// How long the window waits for the facts before it draws the default
/// device meanwhile (they come a few seconds after the bar's start).
const FACTS_WAIT: std::time::Duration = std::time::Duration::from_secs(5);
/// Timers: the helpers' news (until all of it is in), the "Copied" tip,
/// the copy button's name (TIP_DELAY_MS under the pointer).
const T_POLL: usize = 1;
const T_TIP: usize = 2;
const T_HOVER: usize = 3;
const POLL_MS: u32 = 50;
const TIP_MS: u32 = 1200;

struct Panel {
    hwnd: HWND,
    gfx: Rc<Gfx>,
    theme: Theme,
    scale: f32,
    fit: f32,
    _target: DesktopWindowTarget,
    outer: ContainerVisual,
    content: SpriteVisual,
    rect: RECT,
    work: RECT,
    monitor: RECT,
    rtl: bool,
    dec: char,
    height: f32,
    opened: std::time::Instant,
    slate: bool,
    form: FormState,
    light: Option<Arc<Icon>>,
    dark: Option<Arc<Icon>>,
    /// Waiting for the art of this form (since this answer count).
    art_wait: Option<(Form, u64)>,
    wall: Option<Arc<Icon>>,
    back: Option<[u8; 3]>,
    wall_wait: Option<u64>,
    disk: Disk,
    /// The disk is known (read now, or kept from before): its row shows.
    disk_in: bool,
    disk_wait: Option<u64>,
    hover: Option<Btn>,
    /// The button the press began on (it acts only if let go over it).
    pressed: Option<Btn>,
    /// The keyboard focus is on a button (Tab, a screen reader): Space
    /// presses it, the tree says so.
    focus: Option<Btn>,
    /// Its ring shows: the keyboard moved the focus (crate::input), a
    /// press of the pointer hides it.
    ring: bool,
    done: bool,
    more: bool,
    /// The buttons as drawn: (copy, more).
    buttons: (D2D_RECT_F, D2D_RECT_F),
    /// "Copied" shows; the copy button's name shows; when a tooltip last
    /// closed (TIP_WARM_MS).
    tip: bool,
    hover_tip: bool,
    tip_closed: Option<std::time::Instant>,
    /// The facts were read when last drawn.
    facts_in: bool,
}

thread_local! {
    static PANEL: RefCell<Option<Panel>> = const { RefCell::new(None) };
}

fn with_panel<R>(f: impl FnOnce(&mut Panel) -> R) -> Option<R> {
    PANEL.with(|c| c.try_borrow_mut().ok().and_then(|mut g| g.as_mut().map(f)))
}

pub fn is_open() -> bool {
    PANEL.with(|c| c.try_borrow().map(|g| g.is_some()).unwrap_or(true))
}

/// Close it (its bar is going away or moved). Safe from inside its loop.
pub fn cancel() {
    if let Some(h) = with_panel(|p| {
        p.done = true;
        p.hwnd
    }) {
        unsafe {
            let _ = PostMessageW(Some(h), WM_NULL, WPARAM(0), LPARAM(0));
        }
    }
}

/// The theme was re-resolved: an open window takes it.
pub(super) fn retheme(theme: &Theme) {
    with_panel(|p| {
        if p.theme != *theme {
            p.theme = theme.clone();
            if let Err(e) = p.build_chrome() {
                crate::log!("about: retheme failed: {e}");
            }
            p.redraw();
        }
    });
}

/// The facts, or what is known before they are read (the name).
fn facts() -> SysInfo {
    system::info().cloned().unwrap_or_default()
}

/// Windows' decimal mark (the user's number format), read once.
pub(super) fn decimal_mark() -> char {
    static MARK: OnceLock<char> = OnceLock::new();
    *MARK.get_or_init(|| unsafe {
        use windows::Win32::Globalization::{GetLocaleInfoEx, LOCALE_SDECIMAL};
        let mut buf = [0u16; 8];
        let n = GetLocaleInfoEx(windows::core::PCWSTR::null(), LOCALE_SDECIMAL, Some(&mut buf));
        if n > 1 {
            String::from_utf16_lossy(&buf[..(n - 1) as usize]).chars().next().unwrap_or('.')
        } else {
            '.'
        }
    })
}

/// Shows it centered on monitor `hmon` (at `scale`) until it closes. Must
/// not be called inside a RefCell borrow of the caller (the loop
/// dispatches).
pub(crate) fn show(gfx: &Rc<Gfx>, theme: &Theme, hmon: isize, scale: f32) {
    if is_open() || super::stats::is_open() || super::control::is_open() || crate::popup::is_tracking() {
        return;
    }
    unsafe {
        let hinst = GetModuleHandleW(None).unwrap_or_default();
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wndproc),
            hInstance: hinst.into(),
            lpszClassName: w!("NopBarAbout"),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            ..Default::default()
        };
        RegisterClassExW(&wc);
        let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        if !GetMonitorInfoW(HMONITOR(hmon as *mut _), &mut mi).as_bool() {
            return;
        }
        let panel = match Panel::new(gfx, theme, mi.rcWork, mi.rcMonitor, scale) {
            Ok(p) => p,
            Err(e) => {
                crate::log!("about: cannot open: {e}");
                return;
            }
        };
        let hwnd = panel.hwnd;
        PANEL.with(|c| *c.borrow_mut() = Some(panel));
        // At once, with what is ready (P2 #1: the UI thread never waits on
        // the helpers); the rest comes on T_POLL.
        with_panel(|p| {
            p.start();
            p.resize();
            p.redraw();
            if p.pending() {
                SetTimer(Some(p.hwnd), T_POLL, POLL_MS, None);
            }
        });
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        let _ = SetForegroundWindow(hwnd);
        let _ = SetFocus(Some(hwnd));
        SetCapture(hwnd);
        a11y::menu_event(hwnd, true);
        if GetForegroundWindow() != hwnd || GetCapture() != hwnd {
            crate::log!("about: could not take foreground/capture; not opening");
        } else {
            let mut msg = MSG::default();
            loop {
                if with_panel(|p| p.done).unwrap_or(true) {
                    break;
                }
                let r = GetMessageW(&mut msg, None, 0, 0);
                if r.0 == 0 || r.0 == -1 {
                    PostQuitMessage(msg.wParam.0 as i32);
                    break;
                }
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        let _ = ReleaseCapture();
        super::desktops::hide_tip();
        let mut more = false;
        if let Some(p) = PANEL.with(|c| c.borrow_mut().take()) {
            more = p.more;
            let _ = KillTimer(Some(p.hwnd), T_POLL);
            let _ = KillTimer(Some(p.hwnd), T_TIP);
            let _ = KillTimer(Some(p.hwnd), T_HOVER);
            a11y::menu_event(p.hwnd, false);
            let (hwnd, c, outer) = (p.hwnd, p.gfx.compositor.clone(), p.outer.clone());
            crate::motion::retire(hwnd, &c, &outer, Box::new(p));
        }
        if more {
            crate::launch::open_soon(crate::launch::Target::new("ms-settings:about", "About", crate::launch::Source::TopBar));
        }
    }
}

impl Panel {
    unsafe fn new(gfx: &Rc<Gfx>, theme: &Theme, work: RECT, monitor: RECT, s: f32) -> Result<Self> {
        let h = shown(&facts(), None, false, '.').2.height;
        let (rect, fit) = place(work, s, h);
        let m_px = (SHADOW_M * s).ceil() as i32;
        let hwnd = CreateWindowExW(
            WS_EX_NOREDIRECTIONBITMAP | WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
            w!("NopBarAbout"),
            &windows::core::HSTRING::from(t("topbar.system.about")),
            WS_POPUP,
            rect.left - m_px,
            rect.top - m_px,
            rect.right - rect.left + m_px * 2,
            rect.bottom - rect.top + m_px * 2,
            None,
            None,
            Some(GetModuleHandleW(None)?.into()),
            None,
        )?;
        let built = (|| -> Result<Self> {
            let enable = BOOL(1);
            let _ = DwmSetWindowAttribute(hwnd, DWMWA_USE_HOSTBACKDROPBRUSH, &enable as *const _ as *const _, 4);
            let c = gfx.compositor.clone();
            let interop: ICompositorDesktopInterop = c.cast()?;
            let target = interop.CreateDesktopWindowTarget(hwnd, true)?;
            let outer = c.CreateContainerVisual()?;
            outer.SetScale(Vector3 { X: s * fit, Y: s * fit, Z: 1.0 })?;
            outer.SetOffset(Vector3 { X: m_px as f32, Y: m_px as f32, Z: 0.0 })?;
            target.SetRoot(&outer)?;
            let content = c.CreateSpriteVisual()?;
            content.SetSize(Vector2 { X: W, Y: h })?;
            let (root, border) = crate::popup::chrome(gfx, theme, &outer, W, h, s * fit, theme.radii().panel)?;
            root.Children()?.InsertBelow(&content, &border)?;
            // It grows from its middle and fades in.
            let _ = crate::motion::open(&c, &outer, (W, h), (W / 2.0, h / 2.0), crate::motion::Kind::Panel, s * fit);
            Ok(Panel {
                hwnd,
                gfx: gfx.clone(),
                theme: theme.clone(),
                scale: s,
                fit,
                _target: target,
                outer,
                content,
                rect,
                work,
                monitor,
                rtl: crate::i18n::rtl(),
                dec: decimal_mark(),
                height: h,
                opened: std::time::Instant::now(),
                slate: form::slate_now(),
                form: FormState::Waiting,
                light: None,
                dark: None,
                art_wait: None,
                wall: None,
                back: None,
                wall_wait: None,
                disk: None,
                disk_in: false,
                disk_wait: None,
                hover: None,
                pressed: None,
                focus: None,
                ring: false,
                done: false,
                more: false,
                buttons: (rect_zero(), rect_zero()),
                tip: false,
                hover_tip: false,
                tip_closed: None,
                facts_in: false,
            })
        })();
        if built.is_err() {
            let _ = DestroyWindow(hwnd);
        }
        built
    }

    fn ds(&self) -> f32 {
        self.scale * self.fit
    }

    /// The monitor as the wallpaper's key.
    fn mon_key(&self) -> [i32; 4] {
        let m = self.monitor;
        [m.left, m.top, m.right, m.bottom]
    }

    /// Asks for the wallpaper and the disk (each shown at once when kept
    /// from before), and the art if the facts are in.
    fn start(&mut self) {
        self.ask_wall();
        self.ask_disk();
        self.poll();
    }

    fn ask_wall(&mut self) {
        let mon = self.monitor;
        let (kept, since) = ask(&WALL, "about-wall", self.mon_key(), move || wall_for(mon));
        if let Some(w) = kept {
            self.wall = w.img;
            self.back = w.back;
        }
        self.wall_wait = Some(since);
    }

    fn ask_disk(&mut self) {
        let (kept, since) = ask(&DISK, "about-disk", (), system::system_disk);
        if let Some(d) = kept {
            self.disk = d;
            self.disk_in = true;
        }
        self.disk_wait = Some(since);
    }

    /// The art of `form`: at once when decoded already, else from a helper.
    fn ask_art(&mut self, form: Form) {
        if let Some((light, dark)) = arts_cached(form) {
            (self.light, self.dark, self.art_wait) = (light, dark, None);
            return;
        }
        let (kept, since) = ask(&ART, "about-art", form, move || (art_icon(&art_name(form, true)), art_icon(&art_name(form, false))));
        (self.light, self.dark) = kept.unwrap_or((None, None));
        self.art_wait = Some((form, since));
    }

    /// Takes what came since it last looked (never waits); true when
    /// something changed. The form is decided again when the facts come
    /// after the wait for them (P2 #3); every answer reaches this window,
    /// whichever window's helper brought it (P2 #2, #4).
    fn poll(&mut self) -> bool {
        let mut changed = false;
        let decided = system::info().map(|i| form::decide(&form::Inputs { slate_now: self.slate, ..i.form.clone() }));
        let next = self.form.next(decided, self.opened.elapsed() >= FACTS_WAIT);
        if next != self.form {
            let was = self.form.form();
            self.form = next;
            if let Some(f) = next.form().filter(|f| Some(*f) != was) {
                self.ask_art(f);
            }
            changed = true;
        }
        if let Some((form, since)) = self.art_wait {
            let got = lock(&ART).poll(&form, since);
            match got {
                Poll::Done(arts) => {
                    (self.light, self.dark) = arts.unwrap_or((None, None));
                    self.art_wait = None;
                    changed = true;
                }
                Poll::Again => self.ask_art(form),
                Poll::Busy => {}
            }
        }
        if let Some(since) = self.wall_wait {
            let got = lock(&WALL).poll(&self.mon_key(), since);
            match got {
                Poll::Done(w) => {
                    if let Some(w) = w {
                        self.wall = w.img;
                        self.back = w.back;
                    }
                    self.wall_wait = None;
                    changed = true;
                }
                Poll::Again => self.ask_wall(),
                Poll::Busy => {}
            }
        }
        if let Some(since) = self.disk_wait {
            let got = lock(&DISK).poll(&(), since);
            match got {
                Poll::Done(d) => {
                    self.disk = d.flatten();
                    self.disk_in = true;
                    self.disk_wait = None;
                    changed = true;
                }
                Poll::Again => self.ask_disk(),
                Poll::Busy => {}
            }
        }
        // The facts may come after the window (at the bar's start).
        let facts_in = system::info().is_some();
        if facts_in != self.facts_in {
            self.facts_in = facts_in;
            changed = true;
        }
        changed
    }

    /// Something is still to come.
    fn pending(&self) -> bool {
        !self.facts_in || !matches!(self.form, FormState::Decided(_)) || self.art_wait.is_some() || self.wall_wait.is_some() || self.disk_wait.is_some()
    }

    /// The facts, the rows and the layout as drawn now.
    fn now(&self) -> (SysInfo, Vec<(&'static str, String)>, Layout) {
        shown(&facts(), self.disk, self.disk_in, self.dec)
    }

    /// Sized again for what it shows now (facts that came after it opened,
    /// a disk that could not be read), still in the middle.
    fn resize(&mut self) {
        let h = self.now().2.height;
        if h == self.height {
            return;
        }
        self.height = h;
        let (rect, fit) = place(self.work, self.scale, h);
        self.rect = rect;
        self.fit = fit;
        let m_px = (SHADOW_M * self.scale).ceil() as i32;
        unsafe {
            let _ = SetWindowPos(
                self.hwnd,
                None,
                rect.left - m_px,
                rect.top - m_px,
                rect.right - rect.left + m_px * 2,
                rect.bottom - rect.top + m_px * 2,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }
        let ds = self.ds();
        let _ = self.outer.SetScale(Vector3 { X: ds, Y: ds, Z: 1.0 });
        let _ = self.content.SetSize(Vector2 { X: W, Y: h });
        if let Err(e) = self.build_chrome() {
            crate::log!("about: resize failed: {e}");
        }
    }

    fn build_chrome(&mut self) -> Result<()> {
        if let Ok(parent) = self.content.Parent() {
            let _ = parent.Children().and_then(|c| c.Remove(&self.content));
        }
        self.outer.Children()?.RemoveAll()?;
        let (root, border) = crate::popup::chrome(&self.gfx, &self.theme, &self.outer, W, self.height, self.ds(), self.theme.radii().panel)?;
        root.Children()?.InsertBelow(&self.content, &border)?;
        Ok(())
    }

    fn hc(&self) -> Option<crate::syslook::HighContrast> {
        self.theme.hc.filter(|_| self.theme.high_contrast)
    }
    /// Text on the panel's material (translucent levels, so it keeps its
    /// contrast over the wallpaper showing through; the secondary and
    /// tertiary raised only as far as they must: readable_level).
    fn primary(&self) -> D2D1_COLOR_F {
        rgba(self.theme.text_primary())
    }
    fn secondary(&self) -> D2D1_COLOR_F {
        rgba(readable_level(&self.theme, self.theme.text_secondary()))
    }
    fn tertiary(&self) -> D2D1_COLOR_F {
        rgba(readable_level(&self.theme, self.theme.text_tertiary()))
    }

    /// `text` in ramp style `st` (`tabular`: figures all one width).
    fn text(&self, text: &str, st: TextStyle, tabular: bool) -> Option<(IDWriteTextLayout, f32, f32)> {
        let family = &self.theme.font.family;
        if tabular {
            self.gfx.text_layout_tabular(text, family, st.size, weight(st)).ok()
        } else {
            self.gfx.text_layout(text, family, st.size, weight(st)).ok()
        }
    }

    /// Text cut with "…" to fit `maxw` DIP.
    fn fit_text(&self, text: &str, st: TextStyle, tabular: bool, maxw: f32) -> Option<(IDWriteTextLayout, f32, f32)> {
        let l = self.text(text, st, tabular)?;
        if l.1 <= maxw {
            return Some(l);
        }
        let chars: Vec<char> = text.chars().collect();
        let cut = |n: usize| format!("{}…", chars[..n].iter().collect::<String>().trim_end());
        let (mut lo, mut hi) = (0usize, chars.len());
        while lo < hi {
            let mid = (lo + hi).div_ceil(2);
            if self.text(&cut(mid), st, tabular).is_some_and(|l| l.1 <= maxw) {
                lo = mid;
            } else {
                hi = mid - 1;
            }
        }
        self.text(&cut(lo), st, tabular)
    }

    /// A line of `text` cut to `x0..x1`, in the band `(top, bottom)`.
    #[allow(clippy::too_many_arguments)]
    unsafe fn line(&self, dc: &ID2D1DeviceContext, text: &str, st: TextStyle, tabular: bool, x0: f32, x1: f32, band: (f32, f32), align: Align, c: D2D1_COLOR_F) -> Result<()> {
        if let Some(l) = self.fit_text(text, st, tabular, (x1 - x0).max(0.0)) {
            self.draw_text(dc, &l, x0, x1, band, align, c)?;
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    unsafe fn draw_text(&self, dc: &ID2D1DeviceContext, l: &(IDWriteTextLayout, f32, f32), x0: f32, x1: f32, (top, bottom): (f32, f32), align: Align, c: D2D1_COLOR_F) -> Result<()> {
        let x = text_x(align, x0, x1, l.1, self.rtl);
        let b = dc.CreateSolidColorBrush(&c, None)?;
        crate::gfx::draw_text(dc, Vector2 { X: x, Y: (top + bottom) / 2.0 - l.2 / 2.0 }, &l.0, &b, D2D1_DRAW_TEXT_OPTIONS_NONE);
        Ok(())
    }

    unsafe fn bitmap(&self, dc: &ID2D1DeviceContext, img: &Icon, dst: D2D_RECT_F, src: Option<D2D_RECT_F>) -> Result<()> {
        if img.width == 0 || img.height == 0 || img.pixels.len() != (img.width * img.height * 4) as usize {
            return Ok(());
        }
        let props = D2D1_BITMAP_PROPERTIES1 {
            pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
            dpiX: 96.0,
            dpiY: 96.0,
            bitmapOptions: D2D1_BITMAP_OPTIONS_NONE,
            colorContext: std::mem::ManuallyDrop::new(None),
        };
        let bmp = dc.CreateBitmap(D2D_SIZE_U { width: img.width, height: img.height }, Some(img.pixels.as_ptr() as _), img.width * 4, &props)?;
        dc.DrawBitmap(&bmp, Some(&dst), 1.0, D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC, src.as_ref().map(|r| r as *const _), None);
        Ok(())
    }

    unsafe fn fill(&self, dc: &ID2D1DeviceContext, r: D2D_RECT_F, rad: f32, c: D2D1_COLOR_F) -> Result<()> {
        if c.a > 0.0 {
            dc.FillRoundedRectangle(&D2D1_ROUNDED_RECT { rect: r, radiusX: rad, radiusY: rad }, &dc.CreateSolidColorBrush(&c, None)?);
        }
        Ok(())
    }

    /// An outline `w` wide inside `r` (its corners concentric).
    unsafe fn outline(&self, dc: &ID2D1DeviceContext, r: D2D_RECT_F, rad: f32, c: D2D1_COLOR_F, w: f32) -> Result<()> {
        let h = w / 2.0;
        let rr = D2D1_ROUNDED_RECT { rect: rect(r.left + h, r.top + h, r.right - h, r.bottom - h), radiusX: (rad - h).max(0.0), radiusY: (rad - h).max(0.0) };
        dc.DrawRoundedRectangle(&rr, &dc.CreateSolidColorBrush(&c, None)?, w, None);
        Ok(())
    }

    /// A card that holds facts (2.4: the card's rest fill; high contrast an
    /// outline of WindowText).
    unsafe fn card(&self, dc: &ID2D1DeviceContext, r: D2D_RECT_F) -> Result<()> {
        let rad = self.theme.radii().card;
        match self.hc() {
            Some(hc) => self.outline(dc, r, rad, rgba(hc.text), 1.0),
            None => {
                // (Flat v2's card light round it: gfx::parts::card.)
                crate::gfx::parts::card(dc, r, rad, &self.theme, State::Rest, self.theme.fill_tile(State::Rest), 1.0, 1.0)
            }
        }
    }

    /// The hero card: a faint fall of light from its top (white .88 to .52
    /// light, .10 to .03 dark: the one card with a gradient, mockup
    /// deviation 4); an outline in high contrast.
    unsafe fn hero(&self, dc: &ID2D1DeviceContext, r: D2D_RECT_F) -> Result<()> {
        use windows::Win32::Graphics::Direct2D::Common::D2D1_GRADIENT_STOP;
        use windows::Win32::Graphics::Direct2D::{D2D1_EXTEND_MODE_CLAMP, D2D1_GAMMA_2_2, D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES};
        if self.hc().is_some() {
            return self.card(dc, r);
        }
        let (a, b) = hero_light(self.theme.is_light);
        let white = |k: f32| D2D1_COLOR_F { r: 1.0, g: 1.0, b: 1.0, a: k };
        let stops = [D2D1_GRADIENT_STOP { position: 0.0, color: white(a) }, D2D1_GRADIENT_STOP { position: 1.0, color: white(b) }];
        let rt: windows::Win32::Graphics::Direct2D::ID2D1RenderTarget = dc.cast()?;
        let coll = rt.CreateGradientStopCollection(&stops, D2D1_GAMMA_2_2, D2D1_EXTEND_MODE_CLAMP)?;
        let props = D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES { startPoint: Vector2 { X: 0.0, Y: r.top }, endPoint: Vector2 { X: 0.0, Y: r.bottom } };
        let brush = rt.CreateLinearGradientBrush(&props, None, &coll)?;
        let rad = self.theme.radii().card;
        crate::gfx::parts::card_under(dc, r, rad, &self.theme, State::Rest, 1.0, 1.0)?;
        dc.FillRoundedRectangle(&D2D1_ROUNDED_RECT { rect: r, radiusX: rad, radiusY: rad }, &brush);
        crate::gfx::parts::card_over(dc, r, rad, &self.theme, State::Rest, 1.0, 1.0)
    }

    /// The device in its box: a soft floor shadow under it, the wallpaper
    /// in its screen, the image over it (nothing until its image is in: no
    /// screen floating without its device). The image is never reflected.
    unsafe fn paint_art(&self, dc: &ID2D1DeviceContext, area: D2D_RECT_F) -> Result<()> {
        use windows::Win32::Graphics::Direct2D::Common::D2D1_GRADIENT_STOP;
        use windows::Win32::Graphics::Direct2D::{D2D1_ELLIPSE, D2D1_EXTEND_MODE_CLAMP, D2D1_GAMMA_2_2, D2D1_RADIAL_GRADIENT_BRUSH_PROPERTIES};
        let Some(form) = self.form.form() else { return Ok(()) };
        let light = self.theme.is_light;
        let Some(d) = devices().get(&art_name(form, light)) else { return Ok(()) };
        let Some(img) = (if light { self.light.as_deref() } else { self.dark.as_deref() }) else { return Ok(()) };
        let art = art_rect(d, placed(area, self.rtl));
        if self.hc().is_none() {
            // The floor: an ellipse of shade fading out, under its foot.
            let (cx, cy) = ((art.left + art.right) / 2.0, art.bottom - 2.0);
            let (rx, ry) = ((art.right - art.left) * 0.55, 7.0);
            let k = if light { 0.32 } else { 0.60 };
            let stops = [D2D1_GRADIENT_STOP { position: 0.0, color: D2D1_COLOR_F { r: 0.0, g: 0.0, b: 0.0, a: k } }, D2D1_GRADIENT_STOP { position: 1.0, color: D2D1_COLOR_F { r: 0.0, g: 0.0, b: 0.0, a: 0.0 } }];
            let rt: windows::Win32::Graphics::Direct2D::ID2D1RenderTarget = dc.cast()?;
            let coll = rt.CreateGradientStopCollection(&stops, D2D1_GAMMA_2_2, D2D1_EXTEND_MODE_CLAMP)?;
            let props = D2D1_RADIAL_GRADIENT_BRUSH_PROPERTIES { center: Vector2 { X: cx, Y: cy }, gradientOriginOffset: Vector2 { X: 0.0, Y: 0.0 }, radiusX: rx, radiusY: ry };
            let brush = rt.CreateRadialGradientBrush(&props, None, &coll)?;
            dc.FillEllipse(&D2D1_ELLIPSE { point: Vector2 { X: cx, Y: cy }, radiusX: rx, radiusY: ry }, &brush);
        }
        if let Some(sr) = screen_rect(d, art) {
            // Half a DIP under the bezel: no seam at the hole's edge.
            let under = rect(sr.left - BLEED, sr.top - BLEED, sr.right + BLEED, sr.bottom + BLEED);
            match self.wall.as_deref() {
                Some(wall) => self.bitmap(dc, wall, under, Some(cover_src(wall.width, wall.height, sr)))?,
                None => {
                    let [r, g, b] = self.back.unwrap_or(SCREEN_OFF);
                    let c = rgba(Rgba { r, g, b, a: 255 });
                    dc.FillRectangle(&under, &dc.CreateSolidColorBrush(&c, None)?);
                }
            }
        }
        self.bitmap(dc, img, art, None)
    }

    /// The hero's words: the name (Large Title, at most two lines broken at
    /// a space), the maker, then Windows' edition and version and its
    /// build: one block around the card's middle.
    unsafe fn paint_hero_text(&self, dc: &ID2D1DeviceContext, info: &SysInfo, lay: &Layout) -> Result<()> {
        let (x0, x1) = lay.text;
        let w = x1 - x0;
        let (l1, l2) = name_lines(&info.pc, |s| self.text(s, T_NAME, false).is_some_and(|l| l.1 <= w));
        let name_h = T_NAME.line * if l2.is_some() { 2.0 } else { 1.0 };
        let os = info.os_line();
        let build = if info.build.is_empty() { String::new() } else { format!("{} {}", t("about_pc.build"), info.build) };
        let lines = |v: &[&str]| v.iter().filter(|s| !s.is_empty()).count() as f32;
        let maker_h = lines(&[&info.maker]) * T_LINE.line;
        let os_h = lines(&[&os, &build]) * T_LINE.line;
        let block = name_h + maker_h + if os_h > 0.0 { HERO_GAP + os_h } else { 0.0 };
        let mut y = ((lay.hero.top + lay.hero.bottom) / 2.0 - block / 2.0).round();
        let (primary, secondary) = (self.primary(), self.secondary());
        for l in [Some(l1), l2].into_iter().flatten() {
            self.line(dc, &l, T_NAME, false, x0, x1, (y, y + T_NAME.line), Align::Start, primary)?;
            y += T_NAME.line;
        }
        if !info.maker.is_empty() {
            self.line(dc, &info.maker, T_LINE, false, x0, x1, (y, y + T_LINE.line), Align::Start, secondary)?;
            y += T_LINE.line;
        }
        if os_h > 0.0 {
            y += HERO_GAP;
            for (s, tab) in [(os.as_str(), false), (build.as_str(), true)] {
                if !s.is_empty() {
                    self.line(dc, s, T_LINE, tab, x0, x1, (y, y + T_LINE.line), Align::Start, secondary)?;
                    y += T_LINE.line;
                }
            }
        }
        Ok(())
    }

    /// A fact's tile: its glyph and label at the top, its value at the
    /// bottom (two lines at most: a long graphics name wraps).
    unsafe fn paint_tile(&self, dc: &ID2D1DeviceContext, f: &Fact, r: D2D_RECT_F) -> Result<()> {
        self.card(dc, placed(r, self.rtl))?;
        let (x0, x1) = (r.left + TILE_PAD, r.right - TILE_PAD);
        let top = r.top + TILE_PAD;
        self.label(dc, f.glyph, &f.label, x0, x1, top)?;
        let st = if f.big { T_BIG } else { T_VALUE };
        let bottom = r.bottom - TILE_PAD;
        let (l1, l2) = name_lines(&f.value, |s| self.text(s, st, f.big).is_some_and(|l| l.1 <= x1 - x0));
        match l2 {
            Some(l2) => {
                self.line(dc, &l1, st, f.big, x0, x1, (bottom - 2.0 * st.line, bottom - st.line), Align::Start, self.primary())?;
                self.line(dc, &l2, st, f.big, x0, x1, (bottom - st.line, bottom), Align::Start, self.primary())
            }
            None => {
                // One line: on the big values' baseline (a tile's "25H2"
                // beside the memory's "16GB" stood 3 px off: states-issues 7).
                let band = (bottom - st.line, bottom);
                let Some(lay) = self.fit_text(&l1, st, f.big, (x1 - x0).max(0.0)) else { return Ok(()) };
                let band = match (f.big, self.text("0", T_BIG, true)) {
                    (false, Some(big)) => value_band(bottom, (T_BIG.line, big.2, crate::gfx::first_baseline(&big.0, big.2)), (lay.2, crate::gfx::first_baseline(&lay.0, lay.2))),
                    _ => band,
                };
                self.draw_text(dc, &lay, x0, x1, band, Align::Start, self.primary())
            }
        }
    }

    /// A tile's label line: its glyph (tertiary), the label (secondary).
    unsafe fn label(&self, dc: &ID2D1DeviceContext, g: Glyph, text: &str, x0: f32, x1: f32, top: f32) -> Result<()> {
        let band = (top, top + T_LABEL.line);
        let gx = x0 + LABEL_GLYPH / 2.0;
        let (gl, _) = mirror::edges(self.rtl, gx, gx, 0.0, W);
        crate::gfx::draw_glyph(dc, g, gl, (band.0 + band.1) / 2.0, LABEL_GLYPH, self.tertiary())?;
        self.line(dc, text, T_LABEL, false, x0 + LABEL_GLYPH + GAP, x1, band, Align::Start, self.secondary())
    }

    /// The disk's tile: its label, the bar of what is used (8, a groove
    /// with its inner shade; the "on" fill: the accent when one was
    /// chosen), the free space under it and the whole at its end.
    unsafe fn paint_disk(&self, dc: &ID2D1DeviceContext, r: D2D_RECT_F) -> Result<()> {
        self.card(dc, placed(r, self.rtl))?;
        let (x0, x1) = (r.left + TILE_PAD, r.right - TILE_PAD);
        self.label(dc, Glyph::Drive, t("about_pc.disk"), x0, x1, r.top + TILE_PAD)?;
        let bottom = r.bottom - TILE_PAD;
        let under = (bottom - T_FREE.line, bottom);
        let bar = rect(x0, under.0 - UNDER_GAP - BAR_H, x1, under.0 - UNDER_GAP);
        let rad = BAR_H / 2.0;
        let Some((total, free)) = self.disk else {
            // Still read: the groove only, nothing that moves when it comes.
            return self.groove(dc, placed(bar, self.rtl), rad);
        };
        self.groove(dc, placed(bar, self.rtl), rad)?;
        let used = used_share(total, free);
        let fw = ((bar.right - bar.left) * used).max(if used > 0.0 { BAR_H } else { 0.0 });
        let fill_r = placed(rect(bar.left, bar.top, bar.left + fw, bar.bottom), self.rtl);
        let c = self.hc().map(|h| h.highlight).unwrap_or_else(|| self.theme.accent());
        self.fill(dc, fill_r, rad, rgba(c))?;
        let free_s = tf("about_pc.disk.free", &[("free", &size_text(free.min(total), self.dec))]);
        let total_s = size_text(total, self.dec);
        let total_w = self.text(&total_s, T_FOOT, true).map_or(0.0, |l| l.1);
        self.line(dc, &free_s, T_FREE, true, x0, (x1 - total_w - GAP).max(x0), under, Align::Start, self.primary())?;
        self.line(dc, &total_s, T_FOOT, true, x0, x1, under, Align::End, self.tertiary())
    }

    /// A groove (2.4: the track's fill with an inner shade along its top;
    /// high contrast an outline of WindowText).
    unsafe fn groove(&self, dc: &ID2D1DeviceContext, r: D2D_RECT_F, rad: f32) -> Result<()> {
        if let Some(hc) = self.hc() {
            return self.outline(dc, r, rad, rgba(hc.text), 1.0);
        }
        self.fill(dc, r, rad, rgba(self.theme.fill_track()))?;
        let shade = if self.theme.is_light { 0.10 } else { 0.34 };
        dc.PushAxisAlignedClip(&r, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
        let edge = rect(r.left, r.top + 0.5, r.right, r.bottom + 4.0);
        self.outline(dc, edge, rad + 1.0, D2D1_COLOR_F { r: 0.0, g: 0.0, b: 0.0, a: shade }, 1.0)?;
        dc.PopAxisAlignedClip();
        Ok(())
    }

    /// A footer button (raised: the card's fills; high contrast an outline
    /// of WindowText, Highlight 2 under the pointer, Highlight pressed),
    /// its ring when the keyboard is on it, and its glyph or words.
    unsafe fn paint_button(&self, dc: &ID2D1DeviceContext, b: Btn, r: D2D_RECT_F) -> Result<()> {
        let r = placed(r, self.rtl);
        let rad = match b {
            Btn::Copy => crate::theme::capsule(BUTTON_H),
            Btn::More => self.theme.radii().control,
        };
        let (hover, pressed) = (self.hover == Some(b), self.pressed == Some(b) && self.hover == Some(b));
        // Its ink.
        let ink = match self.hc() {
            Some(hc) if pressed => {
                self.fill(dc, r, rad, rgba(hc.highlight))?;
                rgba(hc.highlight_text)
            }
            Some(hc) => {
                let (c, w) = if hover { (hc.highlight, 2.0) } else { (hc.text, 1.0) };
                self.outline(dc, r, rad, rgba(c), w)?;
                rgba(hc.text)
            }
            None => {
                let st = crate::wash::raised_state(hover, pressed);
                let fill = button_fill(&self.theme, hover, pressed);
                crate::gfx::parts::card(dc, r, rad, &self.theme, st, fill, 1.0, 1.0)?;
                self.primary()
            }
        };
        if self.ring && self.focus == Some(b) {
            crate::gfx::parts::focus_ring(dc, r, rad, &self.theme, 1.0)?;
        }
        match b {
            Btn::Copy => crate::gfx::draw_glyph(dc, Glyph::Copy, (r.left + r.right) / 2.0, (r.top + r.bottom) / 2.0, LABEL_GLYPH, ink),
            Btn::More => {
                if let Some(l) = self.text(t("about_pc.more"), T_BUTTON, false) {
                    let x = (r.left + r.right) / 2.0 - l.1 / 2.0;
                    let brush = dc.CreateSolidColorBrush(&ink, None)?;
                    crate::gfx::draw_text(dc, Vector2 { X: x, Y: (r.top + r.bottom) / 2.0 - l.2 / 2.0 }, &l.0, &brush, D2D1_DRAW_TEXT_OPTIONS_NONE);
                }
                Ok(())
            }
        }
    }

    /// The footer's buttons where they are drawn (left to right).
    fn button_rects(&self, lay: &Layout) -> (D2D_RECT_F, D2D_RECT_F) {
        let more_w = self.text(t("about_pc.more"), T_BUTTON, false).map_or(64.0, |l| l.1);
        buttons(lay.foot, more_w)
    }

    unsafe fn paint(&self, dc: &ID2D1DeviceContext, out: &mut (D2D_RECT_F, D2D_RECT_F)) -> Result<()> {
        let (info, _, lay) = self.now();
        self.hero(dc, lay.hero)?;
        self.paint_art(dc, lay.art)?;
        self.paint_hero_text(dc, &info, &lay)?;
        for (f, r) in fact_tiles(&info).iter().zip(&lay.tiles) {
            self.paint_tile(dc, f, *r)?;
        }
        if let Some(r) = lay.disk {
            self.paint_disk(dc, r)?;
        }
        let (copy, more) = self.button_rects(&lay);
        self.line(dc, &version_line(), T_FOOT, false, lay.foot.left + TILE_PAD, copy.left - GAP, (lay.foot.top, lay.foot.bottom), Align::Start, self.tertiary())?;
        self.paint_button(dc, Btn::Copy, copy)?;
        self.paint_button(dc, Btn::More, more)?;
        *out = (placed(copy, self.rtl), placed(more, self.rtl));
        Ok(())
    }

    fn redraw(&mut self) {
        let s = self.ds();
        let (pw, ph) = ((W * s).ceil() as u32, (self.height * s).ceil() as u32);
        let mut buttons = self.buttons;
        let me = &*self;
        let surface = self.gfx.draw(pw, ph, |dc, ox, oy| unsafe {
            dc.SetAntialiasMode(D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
            dc.SetTransform(&Matrix3x2 { M11: s, M12: 0.0, M21: 0.0, M22: s, M31: ox, M32: oy });
            let r = me.paint(dc, &mut buttons);
            dc.SetTransform(&Matrix3x2::identity());
            r
        });
        self.buttons = buttons;
        match surface {
            Ok(surface) => {
                if let Ok(brush) = self.gfx.compositor.CreateSurfaceBrushWithSurface(&surface) {
                    let _ = brush.SetStretch(CompositionStretch::Fill);
                    let _ = self.content.SetBrush(&brush);
                }
            }
            Err(e) => crate::log!("about: draw failed: {e}"),
        }
        self.publish();
    }

    fn publish(&self) {
        if a11y::wanted(self.hwnd) {
            a11y::publish(self.hwnd, self.tree());
        }
    }

    /// Every line as text where it is drawn, then the two buttons.
    fn tree(&self) -> Tree {
        let (info, rows, lay) = self.now();
        let (ds, origin) = (self.ds(), (self.rect.left, self.rect.top));
        let px = |r: D2D_RECT_F| {
            let r = placed(r, self.rtl);
            a11y::px_rect(origin, ds, r.left, r.top, r.right - r.left, r.bottom - r.top)
        };
        let text = rect(lay.text.0, lay.hero.top, lay.text.1, lay.hero.bottom);
        let mut nodes = Vec::new();
        let mut put = |key: String, name: String, r: RECT| {
            if !name.is_empty() {
                nodes.push(Node::new(key, Role::Text, name, r));
            }
        };
        put("pc".into(), info.pc.clone(), px(text));
        put("maker".into(), info.maker.clone(), px(text));
        put("os".into(), info.os_line(), px(text));
        // Each fact as "label: value", where its tile (or the hero, for the
        // build) is.
        let facts = fact_tiles(&info);
        for (i, (k, v)) in rows.iter().enumerate() {
            if v.is_empty() {
                continue;
            }
            let at = if *k == t("about_pc.disk") {
                lay.disk
            } else {
                facts.iter().position(|f| f.label == *k).and_then(|j| lay.tiles.get(j).copied())
            };
            put(format!("row:{i}"), format!("{k}: {v}"), px(at.unwrap_or(text)));
        }
        put("version".into(), version_line(), px(lay.foot));
        let (copy, more) = self.buttons;
        let raw = |r: D2D_RECT_F| a11y::px_rect(origin, ds, r.left, r.top, r.right - r.left, r.bottom - r.top);
        nodes.push(Node::new("copy", Role::Button, t("about_pc.copy"), raw(copy)).invoke());
        nodes.push(Node::new("more", Role::Button, t("about_pc.more"), raw(more)).invoke());
        Tree { role: Role::Group, name: t("topbar.system.about").into(), nodes, focus: tree_focus(self.focus), focusable: true, active: true, focus_in: None }
    }

    fn dip(&self, lp: LPARAM) -> (f32, f32) {
        let m = (SHADOW_M * self.scale).ceil();
        let x = (lp.0 & 0xFFFF) as i16 as f32;
        let y = ((lp.0 >> 16) & 0xFFFF) as i16 as f32;
        ((x - m) / self.ds(), (y - m) / self.ds())
    }

    /// The button under (x, y) (as drawn).
    fn button_at(&self, (x, y): (f32, f32)) -> Option<Btn> {
        let inside = |b: &D2D_RECT_F| x >= b.left && x < b.right && y >= b.top && y < b.bottom;
        let (copy, more) = self.buttons;
        if inside(&copy) {
            Some(Btn::Copy)
        } else if inside(&more) {
            Some(Btn::More)
        } else {
            None
        }
    }

    /// The pointer moved onto `b` (or off every button): redrawn, and the
    /// copy button's name waits its time (300 ms; at once soon after
    /// another tooltip).
    fn hover_to(&mut self, b: Option<Btn>) {
        if b == self.hover {
            return;
        }
        self.hover = b;
        if std::mem::take(&mut self.hover_tip) {
            super::desktops::hide_tip();
            self.tip_closed = Some(std::time::Instant::now());
        }
        unsafe {
            let _ = KillTimer(Some(self.hwnd), T_HOVER);
            if b == Some(Btn::Copy) && !self.tip {
                if crate::motion::tip_warm(self.tip_closed.map(|t| t.elapsed())) {
                    self.show_hover_tip();
                } else {
                    SetTimer(Some(self.hwnd), T_HOVER, crate::motion::TIP_DELAY_MS as u32, None);
                }
            }
        }
        self.redraw();
    }

    /// The copy button's name under it.
    fn show_hover_tip(&mut self) {
        if self.hover != Some(Btn::Copy) || self.pressed.is_some() || self.tip {
            return;
        }
        super::desktops::show_tip_at(&self.gfx, &self.theme, self.scale, t("about_pc.copy"), self.under(self.buttons.0), false, self.monitor);
        self.hover_tip = true;
    }

    /// The point under a button for a tip (screen px, EDGE_GAP under it).
    fn under(&self, b: D2D_RECT_F) -> POINT {
        let ds = self.ds();
        POINT { x: self.rect.left + ((b.left + b.right) / 2.0 * ds) as i32, y: self.rect.top + ((b.bottom + crate::theme::metrics::size::EDGE_GAP) * ds) as i32 }
    }

    fn press(&mut self, b: Btn) {
        match b {
            Btn::More => {
                self.more = true;
                self.done = true;
            }
            Btn::Copy => self.copy(self.under(self.buttons.0)),
        }
    }

    /// The keyboard focus onto `b` (Tab, a screen reader's SetFocus), its
    /// ring shown from now on with `show` (crate::input), the tree told
    /// (redraw publishes it).
    fn take_focus(&mut self, b: Btn, show: bool) {
        if self.focus != Some(b) || (show && !self.ring) {
            self.focus = Some(b);
            self.ring |= show;
            self.redraw();
        }
    }

    /// Key `vk`: what it asks (key()), the ring shown once the keyboard
    /// moves the focus (crate::input::navigates).
    fn on_key(&mut self, vk: u16, ctrl: bool, shift: bool) {
        let k = key(vk, ctrl, shift, self.focus);
        if crate::input::navigates(vk) && !self.ring && self.focus.is_some() {
            self.ring = true;
            self.redraw();
        }
        self.act(k, true);
    }

    /// A button went down inside the panel: the ring goes (the last input
    /// wins).
    fn on_down(&mut self) {
        if std::mem::replace(&mut self.ring, false) && self.focus.is_some() {
            self.redraw();
        }
    }

    /// Does what a key (`by_key`) or a screen reader's request asks.
    fn act(&mut self, k: Key, by_key: bool) {
        match k {
            Key::Close => self.done = true,
            Key::Press(b) => self.press(b),
            Key::Focus(dir) => {
                let show = by_key && crate::input::navigates(VK_TAB.0);
                self.take_focus(next_focus(self.focus, dir), show)
            }
            Key::FocusOn(b) => {
                // A screen reader: the ring as it was, or with Windows'
                // keyboard cues always on.
                let show = crate::input::reader_focus(self.ring);
                self.take_focus(b, show)
            }
            Key::Copy => self.copy(self.under(self.buttons.0)),
            Key::Nothing => {}
        }
    }

    /// The facts as text on the clipboard, and a word that says so.
    fn copy(&mut self, at: POINT) {
        let (info, rows, _) = self.now();
        let rows: Vec<_> = rows.into_iter().filter(|(_, v)| !v.is_empty()).collect();
        let text = copy_text(&info, &rows);
        if unsafe { crate::dock::copy_text(self.hwnd, &text) } {
            self.hover_tip = false;
            super::desktops::show_tip_at(&self.gfx, &self.theme, self.scale, t("about_pc.copied"), at, false, self.monitor);
            self.tip = true;
            unsafe {
                let _ = KillTimer(Some(self.hwnd), T_HOVER);
                SetTimer(Some(self.hwnd), T_TIP, TIP_MS, None);
            }
        }
    }
}

fn rect_zero() -> D2D_RECT_F {
    rect(0.0, 0.0, 0.0, 0.0)
}

fn contains(r: &RECT, p: POINT) -> bool {
    p.x >= r.left && p.x < r.right && p.y >= r.top && p.y < r.bottom
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_NCHITTEST if crate::motion::retired(hwnd) => LRESULT(HTTRANSPARENT as isize),
        WM_MOUSEMOVE => {
            with_panel(|p| {
                let at = p.button_at(p.dip(lp));
                p.hover_to(at);
            });
            LRESULT(0)
        }
        WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN => {
            with_panel(|p| {
                let mut pt = POINT { x: (lp.0 & 0xFFFF) as i16 as i32, y: ((lp.0 >> 16) & 0xFFFF) as i16 as i32 };
                let _ = ClientToScreen(hwnd, &mut pt);
                if !contains(&p.rect, pt) {
                    // Outside: close, the click eaten.
                    p.done = true;
                    return;
                }
                p.on_down();
                match msg {
                    WM_LBUTTONDOWN => {
                        if let Some(b) = p.button_at(p.dip(lp)) {
                            p.pressed = Some(b);
                            p.focus = None;
                            if std::mem::take(&mut p.hover_tip) {
                                super::desktops::hide_tip();
                                p.tip_closed = Some(std::time::Instant::now());
                            }
                            p.redraw();
                        }
                    }
                    WM_RBUTTONDOWN => p.copy(POINT { x: pt.x, y: pt.y + (space::S5 * p.scale) as i32 }),
                    _ => {}
                }
            });
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            with_panel(|p| {
                // Only the button the press began on, let go over it.
                if let Some(b) = p.pressed.take() {
                    if p.button_at(p.dip(lp)) == Some(b) {
                        p.press(b);
                    }
                    if !p.done {
                        p.redraw();
                    }
                }
            });
            LRESULT(0)
        }
        WM_KEYDOWN | WM_SYSKEYDOWN => {
            let vk = wp.0 as u16;
            let ctrl = GetKeyState(VK_CONTROL.0 as i32) < 0;
            let shift = GetKeyState(VK_SHIFT.0 as i32) < 0;
            with_panel(|p| p.on_key(vk, ctrl, shift));
            LRESULT(0)
        }
        WM_TIMER if wp.0 == T_POLL => {
            with_panel(|p| {
                if p.poll() {
                    p.resize();
                    p.redraw();
                }
                if !p.pending() {
                    let _ = KillTimer(Some(hwnd), T_POLL);
                }
            });
            LRESULT(0)
        }
        WM_TIMER if wp.0 == T_HOVER => {
            let _ = KillTimer(Some(hwnd), T_HOVER);
            with_panel(|p| p.show_hover_tip());
            LRESULT(0)
        }
        WM_TIMER if wp.0 == T_TIP => {
            let _ = KillTimer(Some(hwnd), T_TIP);
            with_panel(|p| {
                if std::mem::take(&mut p.tip) {
                    super::desktops::hide_tip();
                }
            });
            LRESULT(0)
        }
        WM_CLOSE => {
            with_panel(|p| p.done = true);
            LRESULT(0)
        }
        WM_ACTIVATE if (wp.0 & 0xFFFF) as u32 == WA_INACTIVE => {
            with_panel(|p| p.done = true);
            LRESULT(0)
        }
        WM_CAPTURECHANGED => {
            let now = GetCapture();
            with_panel(|p| {
                if now != p.hwnd {
                    p.done = true;
                }
            });
            LRESULT(0)
        }
        WM_GETOBJECT => {
            let answer = a11y::on_getobject(hwnd, wp, lp, || {
                with_panel(|p| p.publish());
            });
            answer.unwrap_or_else(|| DefWindowProcW(hwnd, msg, wp, lp))
        }
        m if m == a11y::wm_action() => {
            for (key, act) in a11y::take(hwnd) {
                if act == Act::Refresh {
                    with_panel(|p| p.publish());
                } else {
                    let k = asked(&key, act);
                    with_panel(|p| p.act(k, false));
                }
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            a11y::forget(hwnd);
            DefWindowProcW(hwnd, msg, wp, lp)
        }
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

/// Pictures for the design audit (tests only).
#[cfg(test)]
#[path = "about_harmony.rs"]
mod harmony;

/// The hero card's fall of light: white at these alphas, top to bottom
/// (mockup deviation 4).
fn hero_light(light: bool) -> (f32, f32) {
    if light {
        (0.88, 0.52)
    } else {
        (0.10, 0.03)
    }
}

/// What text on this panel lies on (over the plate): the cards, the
/// hero's light at both ends.
fn text_fills(theme: &Theme) -> [Rgba; 3] {
    let white = |k: f32| Rgba { r: 255, g: 255, b: 255, a: (k * 255.0).round() as u8 };
    let (top, bottom) = hero_light(theme.is_light);
    [theme.fill_tile(State::Rest), white(top), white(bottom)]
}

/// Pure: the band a one-line value of height `h` (its first baseline `b`
/// from its top) is centered in so its baseline is a big value's: a big
/// value (its line `big.0`, its height `big.1`, baseline `big.2`) is
/// centered in the band of its line ending at `bottom`.
fn value_band(bottom: f32, big: (f32, f32, f32), (h, b): (f32, f32)) -> (f32, f32) {
    let big_top = bottom - big.0 + (big.0 - big.1) / 2.0;
    let top = big_top + big.2 - b;
    (top, top + h)
}

/// Pure: a translucent text level `c` (secondary, tertiary) that keeps
/// its contrast on the panel as text sees it over any backdrop and on
/// everything it lies on (text_fills): raised toward the primary's alpha
/// only as far as that needs (control::readable_dim, glass v3).
fn readable_level(theme: &Theme, c: Rgba) -> Rgba {
    super::control::readable_dim(c, theme.text_primary().a, &super::control::panel_extremes(theme), &text_fills(theme))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn devices_json_has_every_image_embedded() {
        let d = devices();
        assert_eq!(d.len(), 14, "7 forms x light and dark");
        for (name, dev) in d {
            assert_eq!(dev.file, format!("{name}.png"));
            let png = art_bytes(&dev.file).unwrap_or_else(|| panic!("{name}: not embedded"));
            // The PNG's own size (IHDR) is what devices.json says.
            assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n", "{name}");
            let be = |i: usize| u32::from_be_bytes([png[i], png[i + 1], png[i + 2], png[i + 3]]) as f32;
            assert_eq!([be(16), be(20)], dev.size, "{name}");
            // Its screen is inside it (the mini PC has none).
            match dev.screen {
                Some(_) => assert!(screen_rect(dev, rect(0.0, 0.0, dev.size[0], dev.size[1])).is_some(), "{name}"),
                None => assert!(name.starts_with("minipc"), "{name}"),
            }
        }
        assert_eq!(art_bytes("nope.png"), None);
    }

    #[test]
    fn devices_json_parses_and_rejects() {
        let d = parse_devices(r#"{"devices":{"a":{"file":"a.png","size":[400,300],"screen":[10,20,390,200]},"b":{"file":"b.png","size":[400,200],"screen":null}}}"#);
        assert_eq!(d["a"], Device { file: "a.png".into(), size: [400.0, 300.0], screen: Some([10.0, 20.0, 390.0, 200.0]) });
        assert_eq!(d["b"].screen, None);
        assert!(parse_devices("not json").is_empty());
        assert!(parse_devices(r#"{"devices":{"a":{"file":"a.png"}}}"#).is_empty(), "no size");
    }

    #[test]
    fn the_art_fits_its_box_and_the_screen_follows() {
        let area = rect(50.0, 24.0, 250.0, 174.0);
        // The UMPC (400 x 384): as tall as the box, centered.
        let umpc = &devices()["umpc-light"];
        let a = art_rect(umpc, area);
        assert!((a.bottom - a.top - 150.0).abs() < 1e-3);
        assert!((a.right - a.left - 156.25).abs() < 1e-3);
        assert!(((a.left + a.right) / 2.0 - 150.0).abs() < 1e-3);
        // The handheld (400 x 199): as wide as the box.
        let hh = art_rect(&devices()["handheld-dark"], area);
        assert!((hh.right - hh.left - 200.0).abs() < 1e-3);
        assert!(((hh.top + hh.bottom) / 2.0 - 99.0).abs() < 1e-3);
        // The screen scales with the image: 49..349 of 400 px.
        let s = screen_rect(umpc, a).unwrap();
        let k = 156.25 / 400.0;
        assert!((s.left - (a.left + 49.0 * k)).abs() < 1e-3);
        assert!((s.right - (a.left + 349.2 * k)).abs() < 1e-3);
        assert!((s.top - (a.top + 25.7 * k)).abs() < 1e-3);
        // None without a screen, or with a broken one.
        assert_eq!(screen_rect(&devices()["minipc-light"], area), None);
        let bad = Device { file: "x".into(), size: [400.0, 300.0], screen: Some([10.0, 10.0, 500.0, 200.0]) };
        assert_eq!(screen_rect(&bad, area), None);
        let empty = Device { screen: Some([10.0, 10.0, 10.0, 200.0]), ..bad };
        assert_eq!(screen_rect(&empty, area), None);
        // Never grown, never a division by nothing.
        assert_eq!(fit_in((100.0, 50.0), (200.0, 150.0)), (200.0, 100.0));
        assert_eq!(fit_in((0.0, 50.0), (200.0, 150.0)), (0.0, 0.0));
    }

    #[test]
    fn the_wallpaper_covers_the_screen_cropped_in_the_middle() {
        // A 16:9 wallpaper in a 16:10 screen: the sides cut.
        let screen = rect(0.0, 0.0, 160.0, 100.0);
        let c = cover_src(1920, 1080, screen);
        assert_eq!((c.top, c.bottom), (0.0, 1080.0));
        assert!(((c.right - c.left) - 1728.0).abs() <= 1.0);
        assert!((c.left - 96.0).abs() <= 1.0, "centered: {c:?}");
        // A tall (portrait) wallpaper in a wide screen: top and bottom cut.
        let p = cover_src(1080, 1920, screen);
        assert_eq!((p.left, p.right), (0.0, 1080.0));
        assert!(((p.bottom - p.top) - 675.0).abs() <= 1.0);
        assert!((p.top - 622.0).abs() <= 1.0);
        // The same shape: all of it.
        assert_eq!(cover_src(1600, 1000, screen), rect(0.0, 0.0, 1600.0, 1000.0));
        // The real screen of the UMPC art at the size drawn.
        let umpc = &devices()["umpc-dark"];
        let s = screen_rect(umpc, art_rect(umpc, rect(0.0, 0.0, 200.0, 150.0))).unwrap();
        let c = cover_src(2560, 1600, s);
        let (cw, ch) = (c.right - c.left, c.bottom - c.top);
        assert!((cw / ch - (s.right - s.left) / (s.bottom - s.top)).abs() < 0.01);
    }

    #[test]
    fn disk_space_reads_like_explorer() {
        let _l = crate::i18n::test_lock("ko");
        const GB: u64 = 1024 * 1024 * 1024;
        assert_eq!(size_text(640 * GB, '.'), "640GB");
        assert_eq!(size_text(1229 * GB, '.'), "1.2TB");
        assert_eq!(size_text(1229 * GB, ','), "1,2TB", "the decimal mark");
        assert_eq!(size_text(2048 * GB, '.'), "2TB", "no .0");
        // A 1 TB drive is 931 GB to Windows.
        assert_eq!(size_text(1_000_204_886_016, '.'), "932GB");
        assert_eq!(size_text(1020 * GB, '.'), "1TB", "just under a TB rounds up");
        assert_eq!(size_text(512 * 1024 * 1024, '.'), "512MB");
        assert_eq!(disk_text(1229 * GB, 640 * GB, '.'), "1.2TB 중 640GB 사용 가능");
        assert_eq!(disk_text(100 * GB, 200 * GB, '.'), "100GB 중 100GB 사용 가능", "never more free than there is");
        let _e = { drop(_l); crate::i18n::test_lock("en") };
        assert_eq!(disk_text(1229 * GB, 640 * GB, '.'), "640GB available of 1.2TB");
    }

    fn pc() -> SysInfo {
        SysInfo {
            pc: "EXAMPLE-PC".into(),
            maker: "Example Works X14".into(),
            windows: "Windows 11 Home".into(),
            version: "25H2".into(),
            build: "26200.1000".into(),
            cpu: "Example Core X8 w/ Example Graphics".into(),
            gpu: "Example Graphics".into(),
            ram_kb: 32 * 1024 * 1024,
            form: Default::default(),
        }
    }

    #[test]
    fn the_list_says_what_is_known_in_order() {
        let _l = crate::i18n::test_lock("ko");
        const GB: u64 = 1024 * 1024 * 1024;
        let r = rows(&pc(), Some((1229 * GB, 640 * GB)), '.');
        assert_eq!(
            r,
            [
                ("프로세서", "Example Core X8".to_string()),
                ("그래픽", "Example Graphics".to_string()),
                ("메모리", "32GB".to_string()),
                ("디스크", "1.2TB 중 640GB 사용 가능".to_string()),
                ("빌드", "26200.1000".to_string()),
            ]
        );
        // Unknown: left out.
        let some = SysInfo { cpu: String::new(), gpu: String::new(), ..pc() };
        assert_eq!(rows(&some, None, '.').iter().map(|r| r.0).collect::<Vec<_>>(), ["메모리", "빌드"]);
        assert!(rows(&SysInfo::default(), None, '.').is_empty());
        // The copy: every line, labels with their values.
        let text = copy_text(&pc(), &r);
        assert_eq!(
            text.split("\r\n").collect::<Vec<_>>(),
            [
                "EXAMPLE-PC",
                "Example Works X14",
                "Windows 11 Home · 25H2",
                "프로세서: Example Core X8",
                "그래픽: Example Graphics",
                "메모리: 32GB",
                "디스크: 1.2TB 중 640GB 사용 가능",
                "빌드: 26200.1000",
                &format!("NopBar {}", crate::updates::CURRENT),
            ]
        );
    }

    #[test]
    fn the_window_is_cards_top_to_bottom() {
        // Four facts and the disk: the hero, two rows of tiles, the disk,
        // the footer; 392 x 524 (the mockup's).
        let l = layout(4, true);
        assert_eq!(l.hero, rect(PAD, PAD, W - PAD, PAD + HERO_H));
        assert_eq!(l.tiles.len(), 4);
        assert_eq!(l.tiles[0], rect(8.0, 200.0, 192.0, 288.0));
        assert_eq!(l.tiles[1], rect(200.0, 200.0, 384.0, 288.0));
        assert_eq!(l.tiles[2].top, 296.0);
        assert_eq!(l.disk, Some(rect(8.0, 392.0, 384.0, 480.0)));
        assert_eq!(l.foot, rect(8.0, 488.0, 384.0, 516.0));
        assert_eq!((W, l.height), (392.0, 524.0));
        // The art's box in the hero, the text column after it.
        assert_eq!(l.art, rect(PAD + ART_IN, PAD + ART_V, PAD + ART_IN + ART_W, PAD + HERO_H - ART_V));
        assert_eq!(l.text, (l.art.right + HERO_GAP, W - PAD - HERO_END));
        assert!(l.text.1 - l.text.0 >= 150.0);
        // Three facts: the odd one alone on its row; no disk: shorter.
        let s = layout(3, false);
        assert_eq!(s.tiles[2].left, PAD);
        assert_eq!(s.disk, None);
        assert_eq!(s.foot.top, s.tiles[2].bottom + GAP);
        assert!(s.height < l.height);
        // Nothing known: the hero and the footer.
        let n = layout(0, false);
        assert_eq!(n.foot.top, n.hero.bottom + GAP);
    }

    #[test]
    fn it_opens_in_the_middle_and_fits_a_short_screen() {
        let work = RECT { left: 0, top: 40, right: 2560, bottom: 1600 };
        let h = layout(4, true).height;
        let (r, fit) = place(work, 2.0, h);
        assert_eq!(fit, 1.0);
        assert_eq!(r.right - r.left, 784);
        assert_eq!(r.bottom - r.top, (h * 2.0).ceil() as i32);
        assert_eq!((r.left + r.right) / 2, 1280);
        assert!(((r.top + r.bottom) / 2 - 820).abs() <= 1);
        // A 1280 x 720 work area at 200 %: shrunk to fit with a margin.
        let short = RECT { left: 0, top: 0, right: 1280, bottom: 720 };
        let (r, fit) = place(short, 2.0, h);
        assert!(fit < 1.0);
        assert!(r.top >= 0 && r.bottom <= 720, "{r:?}");
        // Another monitor's coordinates (left of the main one).
        let left = RECT { left: -1920, top: 0, right: 0, bottom: 1040 };
        let (r, _) = place(left, 1.0, h);
        assert_eq!((r.left + r.right) / 2, -960);
    }

    #[test]
    fn the_footer_s_buttons_sit_at_its_end() {
        let foot = layout(4, true).foot;
        let (copy, more) = buttons(foot, 40.0);
        assert_eq!(more.right, foot.right);
        assert_eq!(more.right - more.left, 40.0 + 2.0 * BUTTON_PAD);
        assert_eq!(more.left - copy.right, GAP);
        assert_eq!((copy.right - copy.left, copy.bottom - copy.top), (BUTTON_H, BUTTON_H), "round");
        assert_eq!((copy.top + copy.bottom) / 2.0, (foot.top + foot.bottom) / 2.0);
        // The disk's bar: what is used.
        const GB: u64 = 1024 * 1024 * 1024;
        assert!((used_share(512 * GB, 212 * GB) - 300.0 / 512.0).abs() < 1e-6);
        assert_eq!(used_share(0, 0), 0.0);
        assert_eq!(used_share(100, 500), 0.0, "never less than empty");
    }

    #[test]
    fn the_name_breaks_at_a_space_into_two_even_lines() {
        // A "column" that holds 8 characters.
        let fits = |s: &str| s.chars().count() <= 8;
        assert_eq!(name_lines("하모니 노트북", fits), ("하모니 노트북".to_string(), None));
        assert_eq!(name_lines("하모니 작업실 노트북", fits), ("하모니 작업실".to_string(), Some("노트북".to_string())));
        assert_eq!(name_lines("AAAA BBBB CCCC", fits), ("AAAA".to_string(), Some("BBBB CCCC".to_string())));
        // Never inside a word: no space that fits, one line (cut by the
        // caller).
        assert_eq!(name_lines("EXAMPLE-WORKSTATION", fits).1, None);
        // The longest start that fits, the rest (cut) under it.
        assert_eq!(name_lines("AB CDEFGHIJKLMN", fits), ("AB".to_string(), Some("CDEFGHIJKLMN".to_string())));
    }

    /// What this PC's window would say (read only).
    /// `cargo test -p dock probe_about -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn probe_about() {
        let _l = crate::i18n::test_lock("ko");
        system::warm();
        let until = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while system::info().is_none() && std::time::Instant::now() < until {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        let info = system::info().cloned().unwrap_or_default();
        let form = form::decide(&form::Inputs { slate_now: form::slate_now(), ..info.form.clone() });
        println!("form {form:?}, art {}", art_name(form, true));
        println!("{}", copy_text(&info, &rows(&info, system::system_disk(), '.')));
    }

    // ---- the review's P2s (docs/reviews/2026-11-07-about-pc-review.md) ----

    #[test]
    fn p2_1_what_is_kept_shows_at_once_and_the_rest_comes_on_poll() {
        // Opening never waits: it gets what is kept now, and looks later.
        let mut s: Shared<(), u32> = Shared::new();
        let (kept, since, start) = s.open(());
        assert_eq!((kept, start), (None, true), "nothing kept yet: a helper starts");
        assert_eq!(s.poll(&(), since), Poll::Busy, "not in yet: drawn without it, looked at again");
        s.finish((), Some(7));
        assert_eq!(s.poll(&(), since), Poll::Done(Some(7)));
        // The next window has it at once, and a fresh read follows.
        let (kept, since, start) = s.open(());
        assert_eq!((kept, start), (Some(7), true));
        assert_eq!(s.poll(&(), since), Poll::Busy, "the kept one is shown meanwhile, not final");
    }

    #[test]
    fn p2_2_a_window_opened_while_the_wallpaper_is_read_gets_its_answer() {
        let mut s: Shared<[i32; 4], &str> = Shared::new();
        let mon = [0, 0, 2560, 1600];
        // The first window starts the helper and closes.
        let (_, _, start) = s.open(mon);
        assert!(start);
        // The second, opened while it runs: busy, not "no wallpaper".
        let (kept, since, start) = s.open(mon);
        assert_eq!((kept, start), (None, false), "one helper at a time");
        assert_eq!(s.poll(&mon, since), Poll::Busy);
        // The first window's helper answers: the second takes it.
        s.finish(mon, Some("wall"));
        assert_eq!(s.poll(&mon, since), Poll::Done(Some("wall")));
        // "No wallpaper" is an answer of its own (Done with the color only),
        // and a helper that could not start keeps what was there.
        let (_, since, _) = s.open(mon);
        s.finish(mon, None);
        assert_eq!(s.poll(&mon, since), Poll::Done(Some("wall")));
    }

    #[test]
    fn p2_3_late_facts_decide_the_form_again() {
        let s = FormState::Waiting;
        assert_eq!(s.next(None, false), FormState::Waiting, "not drawn while the facts may still come");
        // Past the wait: the default, provisional.
        let guess = s.next(None, true);
        assert_eq!(guess, FormState::Guess(Form::Laptop));
        assert_eq!(guess.form(), Some(Form::Laptop));
        assert_eq!(guess.next(None, true), guess, "still a guess while they are out");
        // They come: decided from them, and the art asked for that form.
        let decided = guess.next(Some(Form::Desktop), true);
        assert_eq!(decided, FormState::Decided(Form::Desktop));
        assert_ne!(decided.form(), guess.form(), "a new form: its art is asked for");
        // Final: never decided again.
        assert_eq!(decided.next(Some(Form::Tablet), true), decided);
        assert_eq!(decided.next(None, true), decided);
        // In time: straight to the facts' form.
        assert_eq!(FormState::Waiting.next(Some(Form::Umpc), false), FormState::Decided(Form::Umpc));
        assert_eq!(FormState::Waiting.form(), None);
    }

    #[test]
    fn p2_4_art_and_disk_come_apart_one_helper_each() {
        let mut art: Shared<Form, u32> = Shared::new();
        let mut disk: Shared<(), Option<(u64, u64)>> = Shared::new();
        let (_, art_since, a) = art.open(Form::Umpc);
        let (_, disk_since, d) = disk.open(());
        assert!(a && d);
        // The art in, the disk still read: the art is drawn now.
        art.finish(Form::Umpc, Some(1));
        assert_eq!(art.poll(&Form::Umpc, art_since), Poll::Done(Some(1)));
        assert_eq!(disk.poll(&(), disk_since), Poll::Busy);
        // A stuck disk: reopening many times never starts a second read.
        let mut waits = Vec::new();
        for _ in 0..10 {
            let (kept, since, start) = disk.open(());
            assert!(!start && kept.is_none());
            waits.push(since);
        }
        // It answers late: every window, the current one too, gets it;
        // unreadable is an answer (its row left out), not a wait.
        disk.finish((), Some(None));
        assert!(waits.iter().all(|&s| disk.poll(&(), s) == Poll::Done(Some(None))));
        assert_eq!(disk.poll(&(), disk_since), Poll::Done(Some(None)));
        // Art for another form than the one being read: asked again once
        // the reader is done.
        let (_, s1, _) = art.open(Form::Laptop);
        let (_, s2, start) = art.open(Form::Desktop);
        assert!(!start);
        art.finish(Form::Laptop, Some(2));
        assert_eq!(art.poll(&Form::Laptop, s1), Poll::Done(Some(2)));
        assert_eq!(art.poll(&Form::Desktop, s2), Poll::Again);
    }

    #[test]
    fn p2_5_the_desktop_is_placed_as_windows_places_it() {
        // IDesktopWallpaper's positions and color.
        assert_eq!(placement_of(0), Some(Placement::Center));
        assert_eq!(placement_of(1), Some(Placement::Tile));
        assert_eq!(placement_of(2), Some(Placement::Stretch));
        assert_eq!(placement_of(3), Some(Placement::Fit));
        assert_eq!(placement_of(4), Some(Placement::Fill));
        assert_eq!(placement_of(5), Some(Placement::Span));
        assert_eq!(placement_of(9), None);
        assert_eq!(colorref_rgb(0x00_30_20_10), [0x10, 0x20, 0x30]);
        // The picture has the monitor's shape, 640 at most.
        assert_eq!(canvas_size(1920, 1080), (640, 360));
        assert_eq!(canvas_size(1080, 1920), (360, 640));
        assert_eq!(canvas_size(320, 200), (320, 200), "never grown");
        let (mw, mh) = (1920, 1080);
        let (cw, ch) = canvas_size(mw, mh);
        let place = |p, sw, sh| match crate::wallpaper::plan(Some(p), sw, sh, mw, mh, cw, ch) {
            Plan::Place { w, h, x, y, tile } => (axis(sw, w, x, cw, tile), axis(sh, h, y, ch, tile), w, h),
            other => panic!("{other:?}"),
        };
        // Center, a small image: all of it, a third of its size, centered.
        let (ax, ay, w, h) = place(Placement::Center, 800, 600);
        let (ax, ay) = (ax.unwrap(), ay.unwrap());
        assert_eq!((ax.src, ax.src_len, ax.len, ay.src, ay.src_len, ay.len), (0, 800, w, 0, 600, h));
        assert_eq!((ax.at, ay.at), ((cw as i32 - w as i32) / 2, (ch as i32 - h as i32) / 2));
        assert_eq!(steps(ax, cw), [ax.at], "once");
        // Center, a larger image than the monitor: only its middle,
        // decoded at the picture's size, not the image's.
        let (ax, ay, _, _) = place(Placement::Center, 3840, 2160);
        let (ax, ay) = (ax.unwrap(), ay.unwrap());
        assert_eq!((ax.at, ax.len, ay.at, ay.len), (0, cw, 0, ch));
        assert_eq!((ax.src, ax.src_len), (960, 1920));
        assert_eq!((ay.src, ay.src_len), (540, 1080));
        // Tile: the whole tile, repeated across.
        let (ax, ay, w, _) = place(Placement::Tile, 96, 96);
        let (ax, ay) = (ax.unwrap(), ay.unwrap());
        assert!(ax.whole && ay.whole);
        assert_eq!(ax.len, w);
        let xs = steps(ax, cw);
        assert_eq!(xs[0], 0);
        assert!(xs.windows(2).all(|p| p[1] - p[0] == w as i32));
        assert!(*xs.last().unwrap() < cw as i32 && xs.last().unwrap() + w as i32 >= cw as i32, "to the end");
        // Fit: all of it, letterboxed in the middle.
        let (ax, ay, w, h) = place(Placement::Fit, 1000, 1000);
        let (ax, ay) = (ax.unwrap(), ay.unwrap());
        assert_eq!((w, h), (ch, ch));
        assert_eq!((ax.src_len, ay.src_len), (1000, 1000));
        assert_eq!(ax.at, (cw - ch) as i32 / 2);
        // Off the picture: nothing.
        assert_eq!(axis(100, 50, 700, 640, false), None);
        assert_eq!(axis(100, 50, -60, 640, false), None);
        // Fill is the cover crop.
        assert!(matches!(crate::wallpaper::plan(Some(Placement::Fill), 1600, 1000, mw, mh, cw, ch), Plan::Cover(_)));
        // Span: each monitor its part of the one image over both.
        let r = |l, t, rr, b| RECT { left: l, top: t, right: rr, bottom: b };
        let virt = r(0, 0, 3840, 1080);
        assert_eq!(span_src(3840, 1080, virt, r(0, 0, 1920, 1080)), [0, 0, 1920, 1080]);
        assert_eq!(span_src(3840, 1080, virt, r(1920, 0, 3840, 1080)), [1920, 0, 1920, 1080]);
        // A 16:9 image over a 32:9 box: covered (top and bottom cut), the
        // right half for the right monitor.
        assert_eq!(span_src(1920, 1080, virt, r(1920, 0, 3840, 1080)), [960, 270, 960, 540]);
        // A monitor left of the main one (negative coordinates).
        assert_eq!(span_src(3840, 1080, r(-1920, 0, 1920, 1080), r(-1920, 0, 0, 1080)), [0, 0, 1920, 1080]);
        // Each monitor its own: an answer for monitor A is not B's.
        let mut s: Shared<[i32; 4], u32> = Shared::new();
        let (a, b) = ([0, 0, 1920, 1080], [1920, 0, 3840, 1080]);
        let (_, sa, _) = s.open(a);
        let (_, sb, _) = s.open(b);
        s.finish(a, Some(1));
        assert_eq!(s.poll(&a, sa), Poll::Done(Some(1)));
        assert_eq!(s.poll(&b, sb), Poll::Again);
        assert_eq!(s.open(b).0, None, "A's picture is not kept for B");
        // The kept picture is told apart by its placement too.
        let desk = |placement| Desk { path: Some("C:/w.jpg".into()), lost: false, placement, back: Some([0, 0, 0]) };
        assert_ne!(desk(Some(Placement::Fill)), desk(Some(Placement::Center)));
    }

    #[test]
    fn p2_6_the_tree_follows_the_real_focus() {
        // Opened: the focus is the window's; the tree says no button has it.
        assert_eq!(tree_focus(None), None);
        assert_eq!(key(VK_SPACE.0, false, false, None), Key::Nothing, "Space needs the focus");
        // Tab: copy, then "More info…", round again; Shift+Tab the other way.
        assert_eq!(key(VK_TAB.0, false, false, None), Key::Focus(1));
        assert_eq!(key(VK_TAB.0, false, true, None), Key::Focus(-1));
        assert_eq!(next_focus(None, 1), Btn::Copy);
        assert_eq!(next_focus(Some(Btn::Copy), 1), Btn::More);
        assert_eq!(next_focus(Some(Btn::More), 1), Btn::Copy);
        assert_eq!(next_focus(None, -1), Btn::More);
        assert_eq!(next_focus(Some(Btn::More), -1), Btn::Copy);
        // A screen reader's SetFocus on a button is Tab's to it.
        assert_eq!(asked("more", Act::Focus), Key::FocusOn(Btn::More));
        assert_eq!(asked("copy", Act::Focus), Key::FocusOn(Btn::Copy));
        // Focused: the tree says so, and Space presses it.
        assert_eq!(tree_focus(Some(Btn::Copy)), Some("copy".to_string()));
        assert_eq!(key(VK_SPACE.0, false, false, Some(Btn::Copy)), Key::Press(Btn::Copy));
        // Enter presses the focused one, or "More info…" (the default).
        assert_eq!(key(VK_RETURN.0, false, false, None), Key::Press(Btn::More));
        assert_eq!(key(VK_RETURN.0, false, false, Some(Btn::Copy)), Key::Press(Btn::Copy));
        assert_eq!(asked("more", Act::Invoke), Key::Press(Btn::More));
        assert_eq!(asked("copy", Act::Invoke), Key::Press(Btn::Copy));
        assert_eq!(asked("pc", Act::Focus), Key::Nothing, "text is not focusable");
        assert_eq!(key(VK_ESCAPE.0, false, false, Some(Btn::More)), Key::Close);
        assert_eq!(key(VK_C.0, true, false, None), Key::Copy);
        assert_eq!(key(VK_C.0, false, false, None), Key::Nothing);
    }

    // ---- the B1 design tokens -----------------------------------------------

    #[test]
    fn every_length_is_on_the_grid() {
        let on = |v: f32, step: f32| (v / step).fract() == 0.0;
        for (name, v) in [
            ("W", W),
            ("PAD", PAD),
            ("GAP", GAP),
            ("HERO_H", HERO_H),
            ("ART_IN", ART_IN),
            ("ART_W", ART_W),
            ("ART_V", ART_V),
            ("HERO_GAP", HERO_GAP),
            ("HERO_END", HERO_END),
            ("TILE_H", TILE_H),
            ("TILE_PAD", TILE_PAD),
            ("LABEL_GLYPH", LABEL_GLYPH),
            ("BAR_H", BAR_H),
            ("UNDER_GAP", UNDER_GAP),
            ("FOOT_H", FOOT_H),
            ("BUTTON_H", BUTTON_H),
            ("BUTTON_PAD", BUTTON_PAD),
            ("ROOM", ROOM),
            ("SHADOW_M", SHADOW_M),
        ] {
            assert!(on(v, 4.0), "{name} = {v} is off the 4 DIP grid");
        }
        // Every card lands on the grid; the corners nest (20 - 8 = 12).
        for (n, disk) in [(4, true), (3, false), (0, false), (2, true)] {
            let l = layout(n, disk);
            let mut rs = vec![l.hero, l.art, l.foot];
            rs.extend(l.tiles.iter().copied());
            rs.extend(l.disk);
            for r in rs {
                assert!([r.left, r.top, r.right, r.bottom].iter().all(|&e| on(e, 4.0)), "{r:?}");
            }
            assert!(on(l.height, 4.0));
        }
        // Glass's corners nest (Flat's are crisp: panel 10, card 8).
        let mut raw = Theme::graphite();
        raw.finish = crate::theme::Finish::Glass;
        let t = raw.resolve_fx(&crate::syslook::SysLook { transparency: true, ..Default::default() }, true);
        assert_eq!(crate::theme::concentric(t.radii().panel, PAD), t.radii().card);
        // The footer row is a row of the kit (28), its buttons the control
        // height.
        assert_eq!((FOOT_H, BUTTON_H), (28.0, 28.0));
    }

    #[test]
    fn the_type_is_the_ramps() {
        for (name, st) in [("name", T_NAME), ("line", T_LINE), ("label", T_LABEL), ("value", T_VALUE), ("big", T_BIG), ("free", T_FREE), ("button", T_BUTTON), ("foot", T_FOOT)] {
            assert!(ramp::RAMP.contains(&st), "{name}: {st:?} is not a ramp style");
            assert!(st.size >= 12.0, "{name}: under 12 DIP");
            assert!(st.weight == ramp::REGULAR || st.weight == ramp::SEMIBOLD, "{name}: weight {}", st.weight);
            assert!(weight(st).0 <= 600, "{name}: never Bold");
        }
        // The name Large Title, values Headline, memory a Title number.
        assert_eq!((T_NAME.size, T_NAME.weight), (26.0, ramp::SEMIBOLD));
        assert_eq!((T_VALUE.size, T_BIG.size, T_FREE.size), (15.0, 20.0, 13.0));
        assert_eq!((T_LABEL.size, T_FOOT.size), (12.0, 12.0));
    }

    #[test]
    fn the_facts_go_on_tiles_the_unknown_left_out() {
        let _l = crate::i18n::test_lock("ko");
        let f = fact_tiles(&pc());
        assert_eq!(f.iter().map(|x| x.label.as_str()).collect::<Vec<_>>(), ["프로세서", "그래픽", "메모리", "Windows"]);
        assert_eq!(f.iter().map(|x| x.glyph).collect::<Vec<_>>(), [Glyph::Processor, Glyph::Graphics, Glyph::Memory, Glyph::Window]);
        assert_eq!((f[2].value.as_str(), f[2].big), ("32GB", true));
        assert_eq!(f[3].value, "25H2");
        let some = SysInfo { cpu: String::new(), version: String::new(), ..pc() };
        let f = fact_tiles(&some);
        assert_eq!(f.iter().map(|x| x.value.as_str()).collect::<Vec<_>>(), ["Example Graphics", "32GB", "Windows 11 Home"]);
        assert!(fact_tiles(&SysInfo::default()).is_empty());
        // The disk's tile stays while it is read, goes when it cannot be.
        assert!(shown(&pc(), None, false, '.').2.disk.is_some());
        assert!(shown(&pc(), None, true, '.').2.disk.is_none());
        assert!(shown(&pc(), Some((1, 1)), true, '.').2.disk.is_some());
        assert_eq!(shown(&pc(), None, false, '.').2.tiles.len(), 4);
    }

    #[test]
    fn the_button_is_filled_like_a_tile() {
        use crate::syslook::{HighContrast, SysLook};
        let black = Rgba { r: 0, g: 0, b: 0, a: 255 };
        let white = Rgba { r: 255, g: 255, b: 255, a: 255 };
        let hc = HighContrast { window: black, text: white, highlight: white, highlight_text: black, gray_text: crate::syslook::GRAY_TEXT };
        for sys in [SysLook { light: true, ..Default::default() }, SysLook { light: false, ..Default::default() }, SysLook { high_contrast: Some(hc), ..Default::default() }] {
            let t = Theme::graphite().resolve(&sys);
            assert_eq!(button_fill(&t, false, false), t.fill_tile(State::Rest));
            assert_eq!(button_fill(&t, true, false), t.fill_tile(State::Hover));
            assert_eq!(button_fill(&t, true, true), t.fill_tile(State::Pressed));
            assert_eq!(button_fill(&t, false, true), t.fill_tile(State::Pressed), "pressed wins");
            if !t.high_contrast {
                assert_ne!(button_fill(&t, true, false), button_fill(&t, false, false), "hover shows");
            }
        }
    }

    #[test]
    fn text_reads_on_the_panel_its_cards_and_the_hero() {
        // Glass v3: against the panel as text sees it (Theme::plate_seen:
        // its top light and grain at their bounds, every fallback level)
        // over the greys and the full colors, every finish.
        use crate::syslook::SysLook;
        use crate::theme::{contrast, over, Finish, Material};
        for light in [false, true] {
            for finish in [Finish::Flat, Finish::Glass] {
                let mut raw = Theme::graphite();
                raw.finish = finish;
                let t = raw.resolve_fx(&SysLook { light, ..Default::default() }, true);
                let (sec, ter) = (readable_level(&t, t.text_secondary()), readable_level(&t, t.text_tertiary()));
                assert!(sec.a >= t.text_secondary().a && sec.a <= t.text_primary().a.max(t.text_secondary().a));
                for (wp, need) in crate::theme::backdrops() {
                    for panel in t.plate_seen(Material::Panel, wp) {
                        let mut unders = vec![panel];
                        unders.extend(text_fills(&t).iter().map(|f| over(*f, panel)));
                        for under in unders {
                            for (what, c) in [("primary", t.text_primary()), ("secondary", sec), ("tertiary", ter)] {
                                let k = contrast(over(c, under), under);
                                assert!(k >= need, "{what}, light {light} {finish:?} wp {wp:?}: {k}");
                            }
                        }
                        // The footer's buttons: the primary on every state.
                        for (h, p) in [(false, false), (true, false), (true, true)] {
                            let under = over(button_fill(&t, h, p), panel);
                            let k = contrast(over(t.text_primary(), under), under);
                            assert!(k >= need, "button, light {light} {finish:?} wp {wp:?}: {k}");
                        }
                    }
                }
            }
        }
    }

    /// states-issues 7: a one-line value beside a big one stands on its
    /// baseline.
    #[test]
    fn a_value_stands_on_the_big_values_baseline() {
        // A big value 28 tall (baseline 22) in a 28 line ending at 100;
        // a value 20 tall (baseline 16).
        let (top, bottom) = value_band(100.0, (28.0, 28.0, 22.0), (20.0, 16.0));
        assert_eq!(bottom - top, 20.0);
        // Centered in that band, its baseline is the big one's.
        assert_eq!(top + 16.0, 72.0 + 22.0);
    }

    #[test]
    fn right_to_left_reflects_the_cards() {
        // A text at the end of 40..100 (20 of text): at 80 left to right,
        // at W - 100 right to left.
        assert_eq!(text_x(Align::End, 40.0, 100.0, 20.0, false), 80.0);
        assert_eq!(text_x(Align::End, 40.0, 100.0, 20.0, true), W - 100.0);
        assert_eq!(text_x(Align::Start, 110.0, 250.0, 50.0, true), W - 160.0);
        // A card reflected across the window.
        let l = layout(4, true);
        assert_eq!(placed(l.tiles[0], true), rect(W - 192.0, 200.0, W - 8.0, 288.0));
        assert_eq!(placed(l.tiles[0], false), l.tiles[0]);
    }
}
