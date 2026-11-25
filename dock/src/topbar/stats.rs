//! The PC status panel, hanging from NopBar's own notification icon when
//! it is clicked (dock/trayicon.rs). It answers before it lists (design:
//! docs/design/harmony/04-pc-status.md), top to bottom:
//!
//! 1. Is the PC all right: one word (Calm, Busy, Very busy) and three dots.
//! 2. What is under pressure: four bars (CPU, memory, GPU, disk) on one
//!    0..100 scale, each with last minute's peak. Only a resource past its
//!    threshold turns bold and dark; nothing is ever colored.
//! 3. Who uses it: the three apps using the most of the resource under
//!    pressure (CPU when all is calm; power on battery), and "Quit" on
//!    the one at the top while there is something to fix.
//! 4. The rest (network, battery, uptime, NopBar's own use) folded under
//!    "Details", and a Task Manager button.
//!
//! The verdict is pure (pressure, level, verdict, lens): ten seconds'
//! average against two thresholds a resource, coming down five under where
//! it went up. Nothing new is measured for it: the numbers are the
//! sampler's (crate::sysstat, see Engine below; it runs only while the
//! panel is open, once a second, and rests while the session is locked or
//! the display off) and crate::hogs'.
//!
//! Built like the Control Center (control.rs): one borderless window with
//! the panels' material (popup::chrome), its content drawn into one D2D
//! surface, modal (a nested loop until it closes). The window is as tall
//! as the panel can get; the plate inside it grows and shrinks (the
//! culprit's row, the details) from the edge its button is on. Laid out
//! left to right (Layout, pure) and reflected at drawing time in a
//! right-to-left language. What changes is eased by the panel's own
//! tweens (Anim: lengths and curves from crate::motion), drawn frame by
//! frame only while one runs. Screen readers get every value as text, the
//! folded ones too (crate::a11y).

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use windows::core::{w, Interface, Result, BOOL};
use windows::UI::Composition::Desktop::DesktopWindowTarget;
use windows::UI::Composition::{CompositionStretch, ContainerVisual, SpriteVisual};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Direct2D::Common::{D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_PIXEL_FORMAT, D2D_RECT_F, D2D_SIZE_U};
use windows::Win32::Graphics::Direct2D::{
    ID2D1DeviceContext, D2D1_ANTIALIAS_MODE_ALIASED, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE, D2D1_BITMAP_OPTIONS_NONE, D2D1_BITMAP_PROPERTIES1, D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC, D2D1_ROUNDED_RECT,
};
use windows::Win32::Graphics::DirectWrite::{IDWriteTextLayout, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_WEIGHT_SEMI_BOLD};
use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_USE_HOSTBACKDROPBRUSH};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::{ClientToScreen, GetMonitorInfoW, MonitorFromPoint, MONITORINFO, MONITOR_DEFAULTTONEAREST};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Power::{RegisterPowerSettingNotification, UnregisterPowerSettingNotification, HPOWERNOTIFY, POWERBROADCAST_SETTING};
use windows::Win32::System::RemoteDesktop::{WTSRegisterSessionNotification, WTSUnRegisterSessionNotification, NOTIFY_FOR_THIS_SESSION};
use windows::Win32::System::SystemServices::GUID_CONSOLE_DISPLAY_STATE;
use windows::Win32::System::WinRT::Composition::ICompositorDesktopInterop;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetCapture, GetKeyState, ReleaseCapture, SetCapture, SetFocus, VK_DOWN, VK_ESCAPE, VK_RETURN, VK_SHIFT, VK_SPACE, VK_TAB, VK_UP};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows_numerics::{Matrix3x2, Vector2, Vector3};

use crate::a11y::{self, Act, Node, Role as Part, Tree};
use crate::gfx::Gfx;
use crate::glyphs::Glyph;
use crate::hogs::Hog;
use crate::i18n::{mirror, t, tf};
use crate::sysstat::{self, Sampler, Snapshot, Spark, Want};
use crate::theme::metrics::{size, space};
use crate::theme::ramp::{self, TextStyle};
use crate::theme::{Level as Ink, Rgba, Role, State, Theme};

// ---- lengths (04-pc-status.md 4; every one a token or a multiple of 4) ---------

/// The panel: 320 wide; its cards PANEL_PAD (8) in and apart.
const W: f32 = 320.0;
const PAD: f32 = size::PANEL_PAD;
const GAP: f32 = space::S3;
/// A card's rows start INSET (4) inside it and keep their content ROW_PAD
/// (8) in: the content line, 20 from the panel's edge, on both sides.
const INSET: f32 = space::S2;
const ROW_PAD: f32 = space::S3;
const CONTENT: f32 = PAD + INSET + ROW_PAD;
/// The state's line: 12 under the card's top (INSET + HEAD_GAP), a row
/// high (the Title style's 26 line in the middle of 28: everything under
/// it then lands on whole pixels at 125 and 175 % too), HEAD_GAP over the
/// bars.
const HEAD_GAP: f32 = space::S3;
const HEAD_LINE: f32 = size::ROW_M;
/// The three dots after the word.
const PIP: f32 = space::S3;
const PIP_GAP: f32 = space::S2;
/// A bar's row, and its parts: the name's column (the widest of the four
/// names, up to a multiple of 4, NAME_MIN..NAME_MAX), GAP, the bar, GAP,
/// the value's column.
const METER_H: f32 = size::ROW_M;
const NAME_MIN: f32 = 52.0;
const NAME_MAX: f32 = 96.0;
const VALUE_W: f32 = 36.0;
const TRACK_H: f32 = size::TRACK_H;
/// Last minute's peak on a bar: a 2 x 8 capsule, shown when the peak is at
/// least PEAK_MIN over the value now.
const PEAK_W: f32 = space::S1;
const PEAK_H: f32 = space::S3;
const PEAK_MIN: f32 = 4.0;
/// A bar's fill while its resource is at ease: the ink at .45 (under
/// pressure: the "on" fill, the darkest there is).
const BAR_INK: f32 = 0.45;
/// The apps: their heading, a row, the culprit's row (it holds a button).
const HEAD_H: f32 = size::HEAD_H;
const APP_H: f32 = size::ROW_M;
const CULPRIT_H: f32 = size::ROW_L;
const APPS: usize = crate::hogs::TOP_N;
const ICON: f32 = size::ICON_S;
const INFO_GLYPH: f32 = size::ICON_XS;
/// A button in a row (Quit, Try again) and the panel-wide one.
const BTN_H: f32 = size::CONTROL_H;
const BTN_PAD: f32 = size::BUTTON_PAD_X;
const WIDE_H: f32 = size::BUTTON_WIDE_H;
/// "Details": its row on the plate, its mark, a value's line in its card.
const MORE_H: f32 = size::ROW_M;
const CHEVRON: f32 = size::ICON_XS;
const DETAIL_H: f32 = size::ROW_S;
/// "Cannot measure": 16 around a 24 glyph, 8, a line, 12, a button.
const FAIL_PAD: f32 = space::S5;
const FAIL_GLYPH: f32 = size::ICON_L;
const FAIL_LINE: f32 = 20.0;
/// Shadow margin around the panel.
const SHADOW_M: f32 = crate::theme::metrics::SHADOW_M;
/// The cores in the CPU bar's tooltip, this many to a line.
const CORES_PER_LINE: usize = 4;

// ---- the verdict (pure; 04-pc-status.md 3) -------------------------------------

/// The four resources, in the order their bars are drawn.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
enum Res {
    Cpu,
    Memory,
    Gpu,
    Disk,
}

const RES: [Res; 4] = [Res::Cpu, Res::Memory, Res::Gpu, Res::Disk];

impl Res {
    fn i(self) -> usize {
        self as usize
    }
}

/// How pressed a resource (and the PC: the most pressed of them) is.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Hash)]
enum Level {
    Calm,
    Busy,
    VeryBusy,
}

/// Where a resource turns Busy and Very busy (% over PRESSURE_MS). The
/// eight numbers of the verdict, in this one place. A GPU at 100 % is a
/// game or a render doing its job, not a slow PC: it stops at Busy.
fn thresholds(r: Res) -> (f32, Option<f32>) {
    match r {
        Res::Cpu => (70.0, Some(90.0)),
        Res::Memory => (80.0, Some(92.0)),
        Res::Disk => (80.0, Some(95.0)),
        Res::Gpu => (85.0, None),
    }
}

/// A level is left this far under where it was entered (it does not
/// flicker at the edge).
const HYSTERESIS: f32 = 5.0;
/// Pressure is the average over this long (a second a sample): a spike
/// does not change the word.
const PRESSURE_MS: u64 = 10_000;
/// Fewer readings than this in that time: not known yet. (The design said
/// three; two halves the wait after opening, and the hysteresis already
/// keeps the word from flickering.)
const MIN_SAMPLES: usize = 2;
/// The apps are ranked again this often (their values change every second;
/// rows that swap places every second cannot be read).
const RANK_MS: u64 = 3_000;

/// Pure: a resource's pressure at `now`: the mean of its readings of the
/// last PRESSURE_MS. Time without a reading (the sampler resting) is not
/// counted, and old readings are gone with it. None under MIN_SAMPLES.
fn pressure(h: &Spark, now: u64) -> Option<f32> {
    let vals: Vec<f32> = h.runs(now).into_iter().flatten().filter(|(age, _)| *age <= PRESSURE_MS).map(|(_, v)| v).collect();
    (vals.len() >= MIN_SAMPLES).then(|| vals.iter().sum::<f32>() / vals.len() as f32)
}

/// Pure: resource `r`'s level at pressure `p`, having been at `prev`: up at
/// a threshold, down only HYSTERESIS under it.
fn level(r: Res, p: f32, prev: Level) -> Level {
    let (busy, very) = thresholds(r);
    let past = |at: f32, held: bool| p >= if held { at - HYSTERESIS } else { at };
    if very.is_some_and(|v| past(v, prev == Level::VeryBusy)) {
        Level::VeryBusy
    } else if past(busy, prev >= Level::Busy) {
        Level::Busy
    } else {
        Level::Calm
    }
}

/// What the panel says first.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Verdict {
    /// The PC: the most pressed resource's level.
    state: Level,
    /// Each resource's own (None: not known, left out).
    levels: [Option<Level>; 4],
    /// The resource the list answers for: the most pressed one (CPU, then
    /// memory, disk, GPU at a tie). None while all is calm.
    lead: Option<Res>,
}

impl Verdict {
    /// Under pressure (drawn bold and dark; there may be several).
    fn pressed(&self, r: Res) -> bool {
        self.levels[r.i()].is_some_and(|l| l >= Level::Busy)
    }
}

/// Pure: the verdict from the four levels; None while none is known.
fn verdict(levels: [Option<Level>; 4]) -> Option<Verdict> {
    let state = levels.iter().flatten().copied().max()?;
    let lead = [Res::Cpu, Res::Memory, Res::Disk, Res::Gpu].into_iter().find(|r| state >= Level::Busy && levels[r.i()] == Some(state));
    Some(Verdict { state, levels, lead })
}

/// What the apps are ranked by.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
enum Lens {
    Cpu,
    Memory,
    Gpu,
    /// CPU and GPU time over ten minutes (crate::hogs): on battery.
    Power,
}

impl Lens {
    /// The lens a bar's row picks (the disk has none: per-process disk use
    /// is not measured, and no false culprit is named).
    fn of(r: Res) -> Option<Lens> {
        match r {
            Res::Cpu => Some(Lens::Cpu),
            Res::Memory => Some(Lens::Memory),
            Res::Gpu => Some(Lens::Gpu),
            Res::Disk => None,
        }
    }

    fn res(self) -> Option<Res> {
        match self {
            Lens::Cpu => Some(Res::Cpu),
            Lens::Memory => Some(Res::Memory),
            Lens::Gpu => Some(Res::Gpu),
            Lens::Power => None,
        }
    }

    /// Its one word over the list.
    fn label(self) -> &'static str {
        match self {
            Lens::Cpu => t("topbar.stats.cpu"),
            Lens::Memory => t("topbar.stats.memory"),
            Lens::Gpu => t("topbar.stats.gpu"),
            Lens::Power => t("topbar.stats.power"),
        }
    }
}

/// Pure: the list's lens: what the user picked (a bar's row, until the
/// panel closes); else the lead resource's (the disk's: CPU, said by the
/// heading); else, all calm, power on battery and CPU plugged in.
fn lens(lead: Option<Res>, on_battery: bool, picked: Option<Lens>) -> Lens {
    picked.or(lead.map(|r| Lens::of(r).unwrap_or(Lens::Cpu))).unwrap_or(if on_battery { Lens::Power } else { Lens::Cpu })
}

/// Pure: the bar's row that carries the selected fill: where the
/// keyboard's cursor is (the disk's row too); else the picked lens' row;
/// else the lead resource's when the list answers for it. None while all
/// is calm and nothing was picked: a dark row in a calm panel reads as an
/// alarm.
fn selected(lead: Option<Res>, picked: Option<Lens>, cursor: Option<Res>) -> Option<Res> {
    cursor.or(picked.map(|l| l.res()).unwrap_or(lead.filter(|r| Lens::of(*r).is_some())))
}

/// Pure: (average, peak) of a resource's readings of the last minute.
fn minute(h: &Spark, now: u64) -> Option<(f32, f32)> {
    let v = h.live(now);
    (!v.is_empty()).then(|| (v.iter().sum::<f32>() / v.len() as f32, v.iter().copied().fold(0.0, f32::max)))
}

/// Before the first numbers (design 5).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Phase {
    /// The first 300 ms: the frame alone, no marks.
    Blank,
    /// No number yet: "–" where they will be.
    Waiting,
    Live,
    /// Five seconds and still none (the sampler did not start).
    Failed,
}

/// Nothing after this long: "Cannot measure".
const FAIL_MS: u64 = 5_000;

/// Pure: the phase `elapsed` ms after opening (or after "Try again");
/// `seen`: a number has come since.
fn phase(elapsed: u64, seen: bool) -> Phase {
    if seen {
        Phase::Live
    } else if elapsed < crate::motion::TIP_DELAY_MS as u64 {
        Phase::Blank
    } else if elapsed < FAIL_MS {
        Phase::Waiting
    } else {
        Phase::Failed
    }
}

// ---- what can be pressed --------------------------------------------------------

/// What can be pointed at, pressed or focused in the panel.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
enum Hit {
    /// A bar's row: picks the list's lens (CPU, memory, GPU); its tooltip.
    Meter(Res),
    /// An app's row: shows the app (when it has a window).
    App(usize),
    /// "Quit" on the first row.
    Quit,
    /// "Details".
    More,
    /// The power list's heading (its tooltip).
    Info,
    Battery,
    Own,
    Retry,
    TaskManager,
}

/// What the panel does once it is closed. An app is taken as it was when
/// pressed (its exe and its processes with their creation times), never by
/// its place in a list that may have changed since.
#[derive(Clone, Debug)]
enum After {
    TaskManager,
    Show(Hog),
    Quit(Hog),
}

/// A row of the list: the app and its value as shown.
#[derive(Clone, Debug, PartialEq)]
struct Row {
    hog: Hog,
    value: String,
}

/// The list as drawn.
#[derive(Clone, Debug, PartialEq)]
struct List {
    lens: Lens,
    rows: Vec<Row>,
    /// No rows: the one line instead ("Measuring…", "No busy apps").
    note: Option<&'static str>,
    /// The first row is the culprit: the PC is under pressure and the list
    /// answers for the resource that is (its name is drawn strong).
    culprit: bool,
    /// The first row carries "Quit": the culprit (or, on the power list,
    /// the app using the most), when it has a window to close.
    quit: bool,
}

/// Pure: the first row answers for the pressure: the PC is Busy or worse
/// and the list is ranked by the lead resource.
fn is_culprit(v: Option<&Verdict>, lens: Lens) -> bool {
    v.and_then(|v| v.lead).and_then(Lens::of) == Some(lens)
}

/// Pure: the rows shown: `fresh` (ranked now, best first) cut to `n`
/// when a ranking is `due`; between rankings the rows `shown` keep their
/// places with their new values, one that left `fresh` making room for
/// the next.
fn ranked(shown: &[String], mut fresh: Vec<Row>, due: bool, n: usize) -> Vec<Row> {
    if !due && !shown.is_empty() {
        let mut kept: Vec<Row> = shown.iter().filter_map(|k| fresh.iter().find(|r| r.hog.key == *k).cloned()).collect();
        for r in fresh {
            if kept.len() >= n {
                break;
            }
            if !kept.iter().any(|k| k.hog.key == r.hog.key) {
                kept.push(r);
            }
        }
        fresh = kept;
    }
    fresh.truncate(n);
    fresh
}

/// Whether a list refresh keeps what `h` points at: the same app (by exe)
/// at the same place, still with a window (and, for Quit, still offered).
/// Otherwise hover, press and focus on it are dropped (a press then does
/// nothing; the keyboard starts over).
fn same_target(h: Hit, old: &List, new: &List) -> bool {
    let same = |i: usize| matches!((old.rows.get(i), new.rows.get(i)), (Some(a), Some(b)) if a.hog.key == b.hog.key && b.hog.window);
    match h {
        Hit::App(i) => same(i),
        Hit::Quit => same(0) && new.quit,
        _ => true,
    }
}

/// Pure: whether `h` may be pressed while the list crosses over from
/// `old` (`crossing`: its new rows are still coming in). A row that holds
/// another app now, and a Quit that is new or on another app, wait until
/// the crossing is over: until then the last app's name still shows
/// there, and a press made meanwhile is never carried out. Everything
/// else answers as it always does.
fn settled(h: Hit, old: Option<&List>, new: &List, crossing: bool) -> bool {
    let Some(old) = old.filter(|_| crossing) else { return true };
    let same = |i: usize| matches!((old.rows.get(i), new.rows.get(i)), (Some(a), Some(b)) if a.hog.key == b.hog.key);
    match h {
        Hit::App(i) => same(i),
        Hit::Quit => same(0) && old.quit,
        _ => true,
    }
}

/// Pure: the app a press on `h` is about, as `list` has it (it must have
/// a window; Quit only where it is offered).
fn pressed_app(h: Hit, list: &List) -> Option<Hog> {
    let i = match h {
        Hit::App(i) => i,
        Hit::Quit if list.quit => 0,
        _ => return None,
    };
    list.rows.get(i).map(|r| r.hog.clone()).filter(|x| x.window)
}

// ---- numbers ----------------------------------------------------------------

fn res_label(r: Res) -> &'static str {
    t(match r {
        Res::Cpu => "topbar.stats.cpu",
        Res::Memory => "topbar.stats.memory",
        Res::Gpu => "topbar.stats.gpu",
        Res::Disk => "topbar.stats.disk",
    })
}

fn res_value(r: Res, s: &Snapshot) -> Option<f32> {
    match r {
        Res::Cpu => s.cpu,
        Res::Memory => s.memory_percent(),
        Res::Gpu => s.gpu,
        Res::Disk => s.disk,
    }
}

fn res_history(r: Res, s: &Snapshot) -> &Spark {
    match r {
        Res::Cpu => &s.history,
        Res::Memory => &s.mem_history,
        Res::Gpu => &s.gpu_history,
        Res::Disk => &s.disk_history,
    }
}

fn level_word(l: Level) -> &'static str {
    t(match l {
        Level::Calm => "topbar.stats.state.calm",
        Level::Busy => "topbar.stats.state.busy",
        Level::VeryBusy => "topbar.stats.state.very_busy",
    })
}

/// A value not known (yet, or on this machine): one en dash, wherever.
const NONE: &str = "\u{2013}";

/// "26%" (whole percent), or the dash.
fn pct_text(v: Option<f32>) -> String {
    match v {
        Some(p) => format!("{}%", p.clamp(0.0, 100.0).round() as i32),
        None => NONE.into(),
    }
}

/// One decimal with `dec` as the decimal mark (Windows' number format,
/// topbar::about::decimal_mark).
fn dec1(v: f64, dec: char) -> String {
    let s = format!("{v:.1}");
    if dec == '.' { s } else { s.replace('.', &dec.to_string()) }
}

fn gib(bytes: u64) -> f64 {
    bytes as f64 / (1u64 << 30) as f64
}

/// "12.2 / 47.6 GB".
fn gb_pair(used: u64, total: u64, dec: char) -> String {
    format!("{} / {} GB", dec1(gib(used), dec), dec1(gib(total), dec))
}

/// "84 MB" (from 1000 MB on: "1.2 GB").
fn mb_text(bytes: u64, dec: char) -> String {
    let mb = bytes as f64 / (1u64 << 20) as f64;
    if mb < 1000.0 { format!("{} MB", mb.round() as u64) } else { format!("{} GB", dec1(mb / 1024.0, dec)) }
}

/// "12.3 KB/s" / "4.5 MB/s" (sysstat::rate_text) with the decimal mark.
fn rate_text(bps: f64, dec: char) -> String {
    let s = sysstat::rate_text(bps);
    if dec == '.' { s } else { s.replace('.', &dec.to_string()) }
}

/// An app's CPU share: whole from 10 on ("18%"), one decimal under it
/// ("9.4%"; under 0.05: "<0.1%").
fn cpu_text(p: f32, dec: char) -> String {
    if p >= 9.95 {
        format!("{}%", p.round() as i32)
    } else if p < 0.05 {
        format!("<{}%", dec1(0.1, dec))
    } else {
        format!("{}%", dec1(p as f64, dec))
    }
}

/// "12%" of a share (under 1 %: "<1%").
fn share_text(share: f32) -> String {
    let p = (share * 100.0).round();
    if p < 1.0 { "<1%".into() } else { format!("{}%", p as i32) }
}

/// How long the PC has been on, its two largest units, zeros left out:
/// "35 min", "2 hr 5 min", "2 hr", "3 d 4 hr", "3 d".
fn uptime_text(ms: u64) -> String {
    let (d, h, m) = sysstat::dhm(ms);
    let (d, h, m) = (d.to_string(), h.to_string(), m.to_string());
    if d != "0" {
        if h != "0" { tf("topbar.stats.dur.dh", &[("d", &d), ("h", &h)]) } else { tf("topbar.stats.dur.d", &[("d", &d)]) }
    } else if h != "0" {
        if m != "0" { tf("topbar.stats.dur.hm", &[("h", &h), ("m", &m)]) } else { tf("topbar.stats.dur.h", &[("h", &h)]) }
    } else {
        tf("topbar.stats.dur.m", &[("m", &m)])
    }
}

/// The time left on battery, if Windows tells it.
fn battery_left(power: &sysstat::Power) -> Option<String> {
    let mins = power.secs_left? / 60;
    Some(tf("topbar.panel.battery.remaining", &[("h", &(mins / 60).to_string()), ("m", &(mins % 60).to_string())]))
}

/// "Battery 80% · About 2 hr 10 min left" (its tooltip and its text for
/// screen readers).
fn battery_line(power: &sysstat::Power) -> String {
    let p = power.battery.map(|p| format!("{p}%")).unwrap_or_else(|| NONE.into());
    match battery_left(power) {
        Some(left) => format!("{}  {p} · {left}", t("topbar.stats.battery")),
        None => format!("{}  {p}", t("topbar.stats.battery")),
    }
}

/// The cores, CORES_PER_LINE to a line ("Core 1: 5%   Core 2: 3%…"), or
/// nothing before they are read.
fn cores_tip(cores: &[f32]) -> Option<String> {
    if cores.is_empty() {
        return None;
    }
    let one = |i: usize, p: f32| tf("topbar.stats.core", &[("n", &(i + 1).to_string()), ("p", &(p.round() as i32).to_string())]);
    let lines: Vec<String> = cores.chunks(CORES_PER_LINE).enumerate().map(|(k, c)| c.iter().enumerate().map(|(j, p)| one(k * CORES_PER_LINE + j, *p)).collect::<Vec<_>>().join("   ")).collect();
    Some(lines.join("\n"))
}

/// "Last minute: average 14% · peak 31%".
fn minute_line(h: &Spark, now: u64) -> Option<String> {
    let (avg, peak) = minute(h, now)?;
    Some(tf("topbar.stats.minute", &[("a", &(avg.round() as i32).to_string()), ("p", &(peak.round() as i32).to_string())]))
}

/// A bar's details (its tooltip, and a screen reader's help): the minute's
/// average and peak; the CPU's cores under them, memory in GB instead, the
/// disk's as active time.
fn meter_tip(r: Res, s: &Snapshot, now: u64, dec: char) -> Option<String> {
    let line = minute_line(res_history(r, s), now);
    match r {
        Res::Cpu => match (line, cores_tip(&s.cores)) {
            (Some(l), Some(c)) => Some(format!("{l}\n{c}")),
            (l, c) => l.or(c),
        },
        Res::Memory => s.memory.map(|(u, total)| gb_pair(u, total, dec)),
        Res::Gpu => line,
        Res::Disk => line.map(|l| format!("{} · {l}", t("topbar.stats.active"))),
    }
}

/// NopBar's memory, the one place that reads it ("29 MB"; the dash where
/// Windows did not give the private working set: never read as 0).
fn own_memory_text(o: &sysstat::Own, dec: char) -> String {
    o.memory.map(|m| mb_text(m, dec)).unwrap_or_else(|| NONE.into())
}

/// NopBar's line, short: "0.1% · 29 MB" (the details: own_details).
fn own_short(s: &Snapshot, dec: char) -> String {
    match s.own {
        Some(o) => {
            let cpu = o.cpu.map(|c| format!("{}%", dec1(c as f64, dec))).unwrap_or_else(|| NONE.into());
            format!("{cpu} · {}", own_memory_text(&o, dec))
        }
        None => NONE.into(),
    }
}

/// NopBar's CPU split into its parts (dock, top bar, widgets: %), one
/// line ("Dock 0.3% · Top bar 0.1% · Widgets 0.0%").
fn parts_line(split: (f32, f32, f32), dec: char) -> String {
    let p = |x: f32| format!("{}%", dec1(x as f64, dec));
    tf("topbar.stats.nopbar.parts", &[("dock", &p(split.0)), ("bar", &p(split.1)), ("widgets", &p(split.2))])
}

/// NopBar's use in full, a line each: CPU (and its split into the dock,
/// the top bar and the widgets, once two readings are in), memory, GPU
/// memory (where the adapter tells: `vram_row`), how many processes it
/// sums.
fn own_details(s: &Snapshot, split: Option<(f32, f32, f32)>, vram_row: bool, vram: Option<u64>, dec: char) -> Vec<String> {
    let none = || NONE.to_string();
    let mut v = Vec::new();
    let cpu = s.own.and_then(|o| o.cpu).map(|c| format!("{}%", dec1(c as f64, dec))).unwrap_or_else(none);
    v.push(format!("{} {cpu}", t("topbar.stats.cpu")));
    if let Some(sp) = split {
        v.push(parts_line(sp, dec));
    }
    v.push(format!("{} {}", t("topbar.stats.memory"), s.own.map(|o| own_memory_text(&o, dec)).unwrap_or_else(none)));
    if vram_row {
        v.push(format!("{} {}", t("topbar.stats.nopbar.gpu_memory"), vram.map(|b| mb_text(b, dec)).unwrap_or_else(none)));
    }
    if let Some(o) = s.own {
        v.push(tf("topbar.stats.nopbar.procs", &[("n", &o.procs.to_string())]));
    }
    v
}

/// Down and up on one line: "↓ 2.3 MB/s   ↑ 303 KB/s".
fn net_text(s: &Snapshot, dec: char) -> String {
    match s.net {
        Some((down, up)) => format!("\u{2193} {}   \u{2191} {}", rate_text(down, dec), rate_text(up, dec)),
        None => NONE.into(),
    }
}

/// The lists the snapshot and crate::hogs give, as rows for `lens`, best
/// first (more than the panel shows: see ranked).
fn rows_of(lens: Lens, s: &Snapshot, power: &[Hog], gpu: &[Hog], dec: char) -> Vec<Row> {
    match lens {
        Lens::Cpu => s.apps_cpu.iter().map(|(h, p)| Row { hog: h.clone(), value: cpu_text(*p, dec) }).collect(),
        Lens::Memory => s.apps_mem.iter().map(|(h, b)| Row { hog: h.clone(), value: mb_text(*b, dec) }).collect(),
        Lens::Gpu => gpu.iter().map(|h| Row { hog: h.clone(), value: share_text(h.share) }).collect(),
        Lens::Power => power.iter().map(|h| Row { hog: h.clone(), value: share_text(h.share) }).collect(),
    }
}

// ---- layout (pure, left to right) ------------------------------------------

/// The details' lines, top to bottom.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Detail {
    Network,
    Battery,
    Uptime,
    Own,
}

fn details_of(battery: bool) -> Vec<Detail> {
    let mut v = vec![Detail::Network];
    if battery {
        v.push(Detail::Battery);
    }
    v.extend([Detail::Uptime, Detail::Own]);
    v
}

/// Where everything is (DIP, left to right; see mirrored).
#[derive(Clone, Debug, PartialEq)]
struct Layout {
    /// The state's card, the word's line in it and the four bars' rows.
    state: D2D_RECT_F,
    head: D2D_RECT_F,
    meters: [D2D_RECT_F; 4],
    /// The apps' card, its heading and its rows.
    apps: D2D_RECT_F,
    list_head: D2D_RECT_F,
    rows: [D2D_RECT_F; APPS],
    /// "Details", its card as far as it is open (None: shut) and its lines
    /// where they are once it is.
    more: D2D_RECT_F,
    details: Option<D2D_RECT_F>,
    detail_rows: Vec<D2D_RECT_F>,
    /// "Cannot measure": its card instead of everything above (the rects
    /// above are then empty).
    failed: Option<D2D_RECT_F>,
    button: D2D_RECT_F,
    height: f32,
}

/// The panel. `quit`: how far the first app's row has grown for its
/// button (0: a row like the others, 1: CULPRIT_H); `open`: how far the
/// details are open (0..1). Both glide, so both are amounts. `failed`:
/// nothing could be measured.
fn layout(quit: f32, open: f32, battery: bool, failed: bool) -> Layout {
    let (quit, open) = (quit.clamp(0.0, 1.0), open.clamp(0.0, 1.0));
    let card = |y: f32, h: f32| rect(PAD, y, W - PAD, y + h);
    let row = |y: f32, h: f32| rect(PAD + INSET, y, W - PAD - INSET, y + h);
    let none = rect(0.0, 0.0, 0.0, 0.0);
    if failed {
        let h = FAIL_PAD + FAIL_GLYPH + space::S3 + FAIL_LINE + space::S4 + BTN_H + FAIL_PAD;
        let fail = card(PAD, h);
        let button = card(fail.bottom + GAP, WIDE_H);
        return Layout { state: none, head: none, meters: [none; 4], apps: none, list_head: none, rows: [none; APPS], more: none, details: None, detail_rows: Vec::new(), failed: Some(fail), button, height: button.bottom + PAD };
    }
    let mut y = PAD;
    let head = row(y + INSET + HEAD_GAP, HEAD_LINE);
    let first = head.bottom + HEAD_GAP;
    let meters = std::array::from_fn(|i| row(first + i as f32 * METER_H, METER_H));
    let state = card(y, first + 4.0 * METER_H + INSET - y);
    y = state.bottom + GAP;
    let list_head = row(y + INSET, HEAD_H);
    let mut at = list_head.bottom;
    let rows = std::array::from_fn(|i| {
        let h = if i == 0 { APP_H + (CULPRIT_H - APP_H) * quit } else { APP_H };
        at += h;
        row(at - h, h)
    });
    let apps = card(y, at + INSET - y);
    y = apps.bottom + GAP;
    let more = card(y, MORE_H);
    y = more.bottom;
    let lines = details_of(battery).len();
    let full = INSET + lines as f32 * DETAIL_H + INSET;
    let top = y + GAP;
    let detail_rows = (0..lines).map(|i| row(top + INSET + i as f32 * DETAIL_H, DETAIL_H)).collect();
    let details = (open > 0.0).then(|| card(top, full * open));
    y += (GAP + full) * open;
    let button = card(y + GAP, WIDE_H);
    Layout { state, head, meters, apps, list_head, rows, more, details, detail_rows, failed: None, button, height: button.bottom + PAD }
}

/// Pure: the name column's width for the widest of the four names (DIP):
/// up to a multiple of 4, NAME_MIN..NAME_MAX.
fn name_col(widest: f32) -> f32 {
    ((widest / 4.0).ceil() * 4.0).clamp(NAME_MIN, NAME_MAX)
}

/// A bar's row: its name's span, its bar, its value's span.
fn meter_parts(r: D2D_RECT_F, name_w: f32) -> ((f32, f32), D2D_RECT_F, (f32, f32)) {
    let cy = (r.top + r.bottom) / 2.0;
    let name = (r.left + ROW_PAD, r.left + ROW_PAD + name_w);
    let value = (r.right - ROW_PAD - VALUE_W, r.right - ROW_PAD);
    (name, rect(name.1 + GAP, cy - TRACK_H / 2.0, value.0 - GAP, cy + TRACK_H / 2.0), value)
}

/// The three dots at the end of the state's line, first to last.
fn pips(head: D2D_RECT_F) -> [D2D_RECT_F; 3] {
    let cy = (head.top + head.bottom) / 2.0;
    let end = head.right - ROW_PAD;
    std::array::from_fn(|i| {
        let l = end - (3 - i) as f32 * PIP - (2 - i) as f32 * PIP_GAP;
        rect(l, cy - PIP / 2.0, l + PIP, cy + PIP / 2.0)
    })
}

/// An app row's icon.
fn app_icon_rect(r: D2D_RECT_F) -> D2D_RECT_F {
    let cy = (r.top + r.bottom) / 2.0;
    rect(r.left + ROW_PAD, cy - ICON / 2.0, r.left + ROW_PAD + ICON, cy + ICON / 2.0)
}

/// A button `w` wide at the end of row `r` (Quit).
fn row_button(r: D2D_RECT_F, w: f32) -> D2D_RECT_F {
    let cy = (r.top + r.bottom) / 2.0;
    rect(r.right - ROW_PAD - w, cy - BTN_H / 2.0, r.right - ROW_PAD, cy + BTN_H / 2.0)
}

/// `r` reflected inside the panel when `on` (right to left).
fn mirrored(on: bool, r: D2D_RECT_F) -> D2D_RECT_F {
    let (l, rr) = mirror::edges(on, r.left, r.right, 0.0, W);
    rect(l, r.top, rr, r.bottom)
}

fn rgba(c: Rgba) -> D2D1_COLOR_F {
    D2D1_COLOR_F { r: c.r as f32 / 255.0, g: c.g as f32 / 255.0, b: c.b as f32 / 255.0, a: c.a as f32 / 255.0 }
}

/// `c` at `a` of its own alpha.
fn faded(c: D2D1_COLOR_F, a: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F { a: c.a * a.clamp(0.0, 1.0), ..c }
}

/// `a` on its way to `b` (`k`: 0..1).
fn mixed(a: D2D1_COLOR_F, b: D2D1_COLOR_F, k: f32) -> D2D1_COLOR_F {
    let k = k.clamp(0.0, 1.0);
    let m = |x: f32, y: f32| x + (y - x) * k;
    D2D1_COLOR_F { r: m(a.r, b.r), g: m(a.g, b.g), b: m(a.b, b.b), a: m(a.a, b.a) }
}

fn rect(l: f32, t: f32, r: f32, b: f32) -> D2D_RECT_F {
    D2D_RECT_F { left: l, top: t, right: r, bottom: b }
}

fn mid(r: D2D_RECT_F) -> f32 {
    (r.top + r.bottom) / 2.0
}

/// `r` scaled by `k` about its middle.
fn scaled_rect(r: D2D_RECT_F, k: f32) -> D2D_RECT_F {
    let (cx, cy, hw, hh) = ((r.left + r.right) / 2.0, (r.top + r.bottom) / 2.0, (r.right - r.left) / 2.0 * k, (r.bottom - r.top) / 2.0 * k);
    rect(cx - hw, cy - hh, cx + hw, cy + hh)
}

fn contains(r: &RECT, p: POINT) -> bool {
    p.x >= r.left && p.x < r.right && p.y >= r.top && p.y < r.bottom
}

/// The panel's secondary text for `theme`: Theme::text_secondary, raised
/// only as far as 4.5:1 on the panel and its cards needs whatever is under
/// a Glass panel (control::readable_dim).
fn secondary_text(theme: &Theme) -> Rgba {
    let fills = [theme.fill_of(Role::Raised, State::Rest), theme.fill_of(Role::Raised, State::Hover), theme.fill_of(Role::Raised, State::Pressed)];
    super::control::readable_dim(theme.text_secondary(), theme.text_primary().a, &super::control::panel_extremes(theme), &fills)
}

/// The "on" fill of this panel (a lit dot, a bar under pressure): the
/// theme's achromatic one (Theme::active: the darkest fill there is on
/// light, the lightest on dark; Highlight in high contrast). Never the
/// accent: pressure is said by weight, fill and shape, and a colored bar
/// would read as an alarm light.
fn on_fill(theme: &Theme) -> Rgba {
    theme.active()
}

/// A bar's colors for `theme`: (its fill at ease, its fill under
/// pressure, the peak's mark), `selected`: on a selected row. Achromatic:
/// the ink at BAR_INK, the "on" fill, the tertiary text. High contrast
/// (design 7): Highlight both, the peak in the text color; on a selected
/// row (a Highlight fill) all three in HighlightText.
fn bar_colors(theme: &Theme, selected: bool) -> (Rgba, Rgba, Rgba) {
    let s = if selected { State::Selected } else { State::Rest };
    if theme.hc.is_some() {
        let on = if selected { theme.text_of(Ink::Primary, s) } else { on_fill(theme) };
        return (on, on, theme.text_of(Ink::Tertiary, s));
    }
    let ink = theme.ink();
    (Rgba { a: (ink.a as f32 * BAR_INK).round() as u8, ..ink }, on_fill(theme), theme.text_of(Ink::Tertiary, s))
}

// ---- the keyboard (design 7) --------------------------------------------------

/// A stop of the Tab key: a group (its rows by the arrows) or one thing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Stop {
    Meters,
    Quit,
    Apps,
    More,
    Battery,
    Own,
    Retry,
    TaskManager,
}

/// What of the panel is there to stop at.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
struct Shape {
    failed: bool,
    quit: bool,
    /// App rows shown.
    apps: usize,
    /// The details are open, with a battery line.
    details: bool,
    battery: bool,
}

/// Tab's order: the bars, Quit (when offered), the apps, Details, its
/// lines that explain (once open), Task Manager. "Cannot measure": Try
/// again, Task Manager.
fn stops(s: Shape) -> Vec<Stop> {
    if s.failed {
        return vec![Stop::Retry, Stop::TaskManager];
    }
    let mut v = vec![Stop::Meters];
    if s.quit {
        v.push(Stop::Quit);
    }
    if s.apps > 0 {
        v.push(Stop::Apps);
    }
    v.push(Stop::More);
    if s.details {
        if s.battery {
            v.push(Stop::Battery);
        }
        v.push(Stop::Own);
    }
    v.push(Stop::TaskManager);
    v
}

fn stop_of(h: Hit) -> Stop {
    match h {
        Hit::Meter(_) => Stop::Meters,
        Hit::App(_) | Hit::Info => Stop::Apps,
        Hit::Quit => Stop::Quit,
        Hit::More => Stop::More,
        Hit::Battery => Stop::Battery,
        Hit::Own => Stop::Own,
        Hit::Retry => Stop::Retry,
        Hit::TaskManager => Stop::TaskManager,
    }
}

/// Where the focus lands entering `stop`: a group's selected row (`sel`:
/// the bar that carries the fill), else its first.
fn enter(stop: Stop, sel: Option<Res>) -> Hit {
    match stop {
        Stop::Meters => Hit::Meter(sel.unwrap_or(Res::Cpu)),
        Stop::Quit => Hit::Quit,
        Stop::Apps => Hit::App(0),
        Stop::More => Hit::More,
        Stop::Battery => Hit::Battery,
        Stop::Own => Hit::Own,
        Stop::Retry => Hit::Retry,
        Stop::TaskManager => Hit::TaskManager,
    }
}

/// Pure: where Tab (`back`: Shift+Tab) goes from `focus`, round; from
/// nowhere, the first stop (the last going back).
fn next_tab(s: Shape, focus: Option<Hit>, back: bool, sel: Option<Res>) -> Option<Hit> {
    let order = stops(s);
    let n = order.len() as i32;
    let i = match focus.and_then(|f| order.iter().position(|x| *x == stop_of(f))) {
        Some(i) => (i as i32 + if back { -1 } else { 1 }).rem_euclid(n.max(1)),
        None if back => n - 1,
        None => 0,
    };
    order.get(i as usize).map(|st| enter(*st, sel))
}

/// Pure: where an arrow (`up`) goes from `focus`: inside a group, its
/// next row, round; anywhere else, as Tab.
fn next_arrow(s: Shape, focus: Option<Hit>, up: bool, sel: Option<Res>) -> Option<Hit> {
    let step = |i: usize, n: usize| (i as i32 + if up { -1 } else { 1 }).rem_euclid(n.max(1) as i32) as usize;
    match focus {
        Some(Hit::Meter(r)) if !s.failed => Some(Hit::Meter(RES[step(r.i(), RES.len())])),
        Some(Hit::App(i)) if s.apps > 0 => Some(Hit::App(step(i.min(s.apps - 1), s.apps))),
        _ => next_tab(s, focus, up, sel),
    }
}

/// A11y key of a hit: an app's row and its Quit by the app (its exe), so an
/// Invoke from a screen reader never lands on another app after a refresh.
fn key_of(h: Hit, rows: &[Row]) -> String {
    let app = |i: usize| rows.get(i).map(|x| x.hog.key.as_str()).unwrap_or("?").to_string();
    match h {
        Hit::Meter(r) => format!("{r:?}").to_lowercase(),
        Hit::App(i) => format!("app:{}", app(i)),
        Hit::Quit => format!("quit:{}", app(0)),
        Hit::More => "more".into(),
        Hit::Info => "lens".into(),
        Hit::Battery => "battery".into(),
        Hit::Own => "own".into(),
        Hit::Retry => "retry".into(),
        Hit::TaskManager => "taskmgr".into(),
    }
}

/// Pure: whose tooltip shows: what the keyboard last moved to (`keyed`),
/// else what the pointer is on.
fn tip_target(hover: Option<Hit>, focus: Option<Hit>, keyed: bool) -> Option<Hit> {
    if keyed { focus } else { hover }
}

/// The tooltip showing: its text, the theme it was drawn in (a count of
/// rethemes) and where. Any of them changing draws it again.
#[derive(Clone, Debug, PartialEq)]
struct TipShown {
    text: String,
    theme: u32,
    at: (i32, i32),
}

// ---- motion (design 4.8; lengths and curves are crate::motion's) ---------------

/// What eases in the panel.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
enum Key {
    /// A bar's fill and its peak's place (0..1 of the track).
    Bar(Res),
    Peak(Res),
    /// A resource's emphasis (0 at ease, 1 under pressure): weight, ink and
    /// the bar's fill cross over.
    Hot(Res),
    /// A value coming in for the dash (0..1).
    Known(Res),
    /// A bar row's selected fill.
    Sel(Res),
    /// A row's or button's hover fill, a button's press (its shrink).
    Hover(Hit),
    Press(Hit),
    /// The state's word crossing from the last one; a dot going on.
    Word,
    Pip(u8),
    /// The list's new rows coming in, the old ones going.
    ListIn,
    ListOut,
    /// The first app row's growth for its button, the details' opening
    /// (both move things: Layout takes them), the details' ink, the mark's
    /// half turn.
    Culprit,
    Open,
    OpenInk,
    Turn,
}

impl Key {
    /// It moves or resizes something (reduced motion: never eased; it
    /// changes at once, and only fades are left).
    fn moves(self) -> bool {
        matches!(self, Key::Bar(_) | Key::Peak(_) | Key::Press(_) | Key::Culprit | Key::Open | Key::Turn)
    }
}

/// How a tween runs.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Ease {
    Curve(crate::motion::Curve),
    /// motion::SPRING_GLIDE from rest (a surface's size).
    Glide,
}

/// A spring tween is over after this long (GLIDE is within a thousandth
/// of its target by then).
const GLIDE_MS: i64 = 400;

#[derive(Clone, Copy, PartialEq, Debug)]
struct Tween {
    from: f32,
    to: f32,
    start: u64,
    ms: u64,
    ease: Ease,
}

impl Tween {
    fn done(&self, now: u64) -> bool {
        now.saturating_sub(self.start) >= self.ms
    }

    fn at(&self, now: u64) -> f32 {
        if self.done(now) {
            return self.to;
        }
        let el = now.saturating_sub(self.start) as f32;
        let k = match self.ease {
            Ease::Curve(c) => crate::motion::bezier(c, el / self.ms.max(1) as f32),
            // The spring's own time, stretched with the tween (the speed
            // setting).
            Ease::Glide => 1.0 - crate::motion::SPRING_GLIDE.left(el / 1000.0 * GLIDE_MS as f32 / self.ms.max(1) as f32),
        };
        self.from + (self.to - self.from) * k
    }
}

/// Pure: how a change of `key` designed as `ms` on `ease` plays in `mode`
/// at animation speed `speed`: (ms, ease). Full motion: as designed,
/// stretched by the speed. Reduced (design system 12.2): nothing moves or
/// resizes (0 ms: at once) and every fade is motion::REDUCED_MS, straight.
fn plan(mode: crate::motion::Mode, speed: f32, key: Key, ms: i64, ease: Ease) -> (u64, Ease) {
    if mode == crate::motion::Mode::Reduced && key.moves() {
        return (0, Ease::Curve(crate::motion::LINEAR));
    }
    let curve = match ease {
        Ease::Curve(c) => c,
        Ease::Glide => crate::motion::LINEAR,
    };
    let (len, c) = crate::motion::fade_plan(mode, ms, curve, speed);
    (len.max(0) as u64, if mode == crate::motion::Mode::Full { ease } else { Ease::Curve(c) })
}

/// The tweens running. A value with none is at rest (the caller's `rest`:
/// what the state says it is).
#[derive(Default)]
struct Anim {
    map: HashMap<Key, Tween>,
}

impl Anim {
    fn get(&self, k: Key, now: u64, rest: f32) -> f32 {
        match self.map.get(&k) {
            Some(tw) if !tw.done(now) => tw.at(now),
            _ => rest,
        }
    }

    /// `k` goes to `to` from where it is (`from`: where it rests when no
    /// tween is running), in `ms` on `ease` as planned for the motion mode.
    fn go(&mut self, k: Key, from: f32, to: f32, ms: i64, ease: Ease, now: u64) {
        let from = self.get(k, now, from);
        let (ms, ease) = plan(crate::motion::now(), crate::effects::current().speed, k, ms, ease);
        if ms == 0 || (from - to).abs() < 1e-4 {
            self.map.remove(&k);
        } else {
            self.map.insert(k, Tween { from, to, start: now, ms, ease });
        }
    }

    /// Something is still easing at `now` (frames are drawn until not).
    fn busy(&self, now: u64) -> bool {
        self.map.values().any(|tw| !tw.done(now))
    }

    fn sweep(&mut self, now: u64) {
        self.map.retain(|_, tw| !tw.done(now));
    }

    /// Every selected fill on its way ends where it is going (high
    /// contrast came on: sel_fade).
    fn snap_fills(&mut self) {
        self.map.retain(|k, _| !matches!(k, Key::Sel(_)));
    }
}

/// Pure: how a bar row's selected fill changes (`is`: it takes it): its
/// length and curve, or None: at once. The keyboard: at once. High
/// contrast: at once too (design system 12.3), with the text it flips
/// (row): a fill between Window and Highlight under text already turned
/// HighlightText (or not yet) loses the system's contrast.
fn sel_fade(how: How, is: bool, hc: bool) -> Option<(i64, Ease)> {
    match how {
        _ if hc => None,
        How::Keys => None,
        How::Pointer => Some(if is { (FADE_IN_MS, OUT) } else { (FADE_OUT_MS, IN) }),
        How::Auto => Some((CROSS_MS, OUT)),
    }
}

/// Pure: how much of the selected fill a row draws for `sel` (0..1, as
/// eased). In high contrast all or nothing, on the same side of the half
/// as its text's state (row): the pair is always the system's.
fn sel_fill(hc: bool, sel: f32) -> f32 {
    if hc {
        if sel > 0.5 { 1.0 } else { 0.0 }
    } else {
        sel
    }
}

/// Pure: a row's state (its text's color in high contrast) for its
/// selected fill `sel` and hover fill `hover` as drawn (0..1).
fn row_state(sel: f32, hover: f32, cursor: bool, pressed: bool) -> State {
    if pressed {
        State::Pressed
    } else if cursor {
        State::Cursor
    } else if sel > 0.5 {
        State::Selected
    } else if hover > 0.5 {
        State::Hover
    } else {
        State::Rest
    }
}

const OUT: Ease = Ease::Curve(crate::motion::EASE_OUT);
const IN: Ease = Ease::Curve(crate::motion::EASE_IN);
use crate::motion::{CROSS_MS, FADE_IN_MS, FADE_OUT_MS, SNAP_MAX_MS};

/// The frames' timer (only while a tween runs) and the one that ends the
/// wait for the first numbers (phase).
const FRAME_TIMER: usize = 2;
const FRAME_MS: u32 = 16;
const WAIT_TIMER: usize = 3;

/// placement::POPUP_GAP_DIP under the button (over it, `up`, for a bar at the
/// bottom), aligned with its outer edge (right; left in a right-to-left
/// language, where the cell sits on the bar's left side), inside the work
/// area (shrunk to the room past the gap if the screen is short). Returns
/// the rect (px) and the shrink.
fn place(button: RECT, work: RECT, scale: f32, h: f32, rtl: bool, up: bool) -> (RECT, f32) {
    let gap = crate::placement::popup_gap(scale);
    let room_px = crate::placement::hang_room(button, gap, up, work);
    let room = room_px as f32;
    let full = (h * scale).ceil();
    let fit = if full > room { room / full } else { 1.0 };
    let ds = scale * fit;
    let w_px = (W * ds).ceil() as i32;
    let h_px = ((h * ds).ceil() as i32).min(room_px);
    let x = if rtl { button.left } else { button.right - w_px }.min(work.right - w_px).max(work.left);
    let y = crate::placement::hang_y(button, h_px, gap, up, work);
    (RECT { left: x, top: y, right: x + w_px, bottom: y + h_px }, fit)
}

/// Where a text `w` wide goes in `x0..x1`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Align {
    Start,
    End,
    Mid,
}

/// Its left edge, laid out left to right, then reflected when `rtl`.
fn text_x(align: Align, x0: f32, x1: f32, w: f32, rtl: bool) -> f32 {
    let x = match align {
        Align::Start => x0,
        Align::End => x1 - w,
        Align::Mid => (x0 + x1 - w) / 2.0,
    };
    mirror::span(rtl, x, w, 0.0, W)
}

/// A bar as shown: its value (None: not known), last minute's peak when it
/// is worth a mark, and whether its resource is under pressure.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
struct Meter {
    value: Option<f32>,
    peak: Option<f32>,
    hot: bool,
}

/// What the panel shows, worked out from its readings and what was
/// picked: drawn as it is, and compared with the last one to ease what
/// changed.
#[derive(Clone, Debug, PartialEq)]
struct View {
    phase: Phase,
    verdict: Option<Verdict>,
    meters: [Meter; 4],
    /// The bar's row with the selected fill.
    sel: Option<Res>,
    list: List,
}

impl View {
    fn blank() -> View {
        View { phase: Phase::Blank, verdict: None, meters: [Meter::default(); 4], sel: None, list: List { lens: Lens::Cpu, rows: Vec::new(), note: None, culprit: false, quit: false } }
    }

    /// Dots lit: one calm, two busy, three very busy; none while unknown.
    fn lit(&self) -> usize {
        self.verdict.map_or(0, |v| v.state as usize + 1)
    }
}

/// Pure: what of the list changed between two views: (its heading, each
/// row). A row whose app stayed keeps still while its value changes.
fn list_changes(old: &List, new: &List) -> (bool, [bool; APPS]) {
    let head = old.lens != new.lens;
    let key = |l: &List, i: usize| l.rows.get(i).map(|r| r.hog.key.clone());
    let rows = std::array::from_fn(|i| head || key(old, i) != key(new, i) || (i == 0 && (old.note != new.note || old.quit != new.quit || old.culprit != new.culprit)));
    (head, rows)
}

/// What made the view change (how fast a selection follows: design 4.8).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum How {
    /// A reading, the clock: crossed over.
    Auto,
    /// The pointer: 90 in, 120 out.
    Pointer,
    /// The keyboard: at once.
    Keys,
}

type Laid = (IDWriteTextLayout, f32, f32);

struct Panel {
    hwnd: HWND,
    gfx: Rc<Gfx>,
    theme: Theme,
    /// Secondary text for the theme (secondary_text).
    dim: Rgba,
    scale: f32,
    fit: f32,
    _target: DesktopWindowTarget,
    outer: ContainerVisual,
    content: SpriteVisual,
    /// The room the panel has (px): itself at its tallest. Its window
    /// covers this (and the shadow's margin) and never moves.
    room: RECT,
    /// It hangs over its button (a bar at the bottom): it grows upward.
    up: bool,
    /// The panel as shown now (px): `shown_h` tall at the button's edge of
    /// the room.
    rect: RECT,
    shown_h: f32,
    /// What motion::open added to the plate's offset (it scales about its
    /// button's point, and takes the shift that leaves at rest back there):
    /// kept in every offset set afterwards.
    shift: (f32, f32),
    rtl: bool,
    /// Windows' decimal mark.
    dec: char,
    battery: bool,
    /// On battery: the list is by power while all is calm.
    on_battery: bool,
    /// NopBar's GPU memory shows (the adapter told at opening), and its
    /// last reading (bytes).
    vram_row: bool,
    vram: Option<u64>,
    /// crate::hogs' lists (power, GPU) and whether it has measured.
    power: Vec<Hog>,
    gpu: Vec<Hog>,
    measured: bool,
    snap: Snapshot,
    /// NopBar's CPU time and its top bar's and widgets' at the last reading
    /// (desklayer::load), and the split of its CPU they give.
    parts: Option<(u64, u64, u64)>,
    split: Option<(f32, f32, f32)>,
    /// Readings taken since it opened (the last one's seq).
    samples: u32,
    last_seq: Option<u64>,
    /// When it opened, or "Try again" was pressed (clock_ms), and whether a
    /// number has come since (phase).
    opened: u64,
    seen: bool,
    /// Each resource's level at the last reading (the next one's hysteresis).
    levels: [Option<Level>; 4],
    /// The lens a bar's row picked (until the panel closes).
    picked: Option<Lens>,
    /// The details are open, and were as it opened (show tells a change).
    details: bool,
    details_at_open: bool,
    /// The name column for this language (name_col).
    name_w: f32,
    view: View,
    /// When the apps were last ranked (clock_ms).
    ranked_at: u64,
    /// What the list and the word cross over from, and which rows do.
    old_list: Option<List>,
    swap: (bool, [bool; APPS]),
    old_word: Option<Option<Level>>,
    anim: Anim,
    ticking: bool,
    hover: Option<Hit>,
    pressed: Option<Hit>,
    /// Keyboard focus (Tab, arrows); None: Enter opens Task Manager.
    focus: Option<Hit>,
    /// The keyboard moved the focus since the pointer last moved to
    /// something else: its ring or cursor shows, the tooltip follows it.
    keyed: bool,
    done: bool,
    /// Closed by a press outside it (its button's included: see
    /// closed_by_press): which one, where.
    outside: Option<ClosingPress>,
    after: Option<After>,
    /// What can be pointed at (DIP, as drawn).
    hits: Vec<(D2D_RECT_F, Hit)>,
    /// The tooltip showing.
    tip: Option<TipShown>,
    /// Rethemes so far (TipShown::theme).
    theme_gen: u32,
}

/// A mouse button, as its message names it (swapped buttons applied).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Button {
    Left,
    Right,
    Middle,
}

/// The press outside that closed the panel: its button, and where it was
/// (screen px, from its own message).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ClosingPress {
    pub button: Button,
    pub at: POINT,
}

thread_local! {
    static PANEL: RefCell<Option<Panel>> = const { RefCell::new(None) };
    /// When, and by what, a press outside last closed the panel.
    static PRESS_CLOSED: std::cell::Cell<Option<(std::time::Instant, ClosingPress)>> = const { std::cell::Cell::new(None) };
}

/// The press outside that closed the panel less than `ms` ago, if one
/// did. That press is eaten, but a button that opens it on release (the
/// notification icon) may still report that click: it must not open the
/// panel again right away, so a second click on the button closes it.
pub fn closed_by_press(ms: u32) -> Option<ClosingPress> {
    PRESS_CLOSED.with(|c| c.get()).filter(|(at, _)| at.elapsed().as_millis() < ms as u128).map(|(_, p)| p)
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

/// New numbers from the sampler.
fn sampled() {
    with_panel(|p| {
        p.take(snapshot());
        p.take_lists();
        p.refresh(How::Auto, true);
        p.refresh_tip();
        p.publish();
    });
}

/// The theme was re-resolved (light/dark, accent, high contrast): an open
/// panel takes it (chrome and content drawn again, and a tooltip showing).
pub(super) fn retheme(theme: &Theme) {
    with_panel(|p| {
        if p.theme != *theme {
            p.theme = theme.clone();
            p.dim = secondary_text(theme);
            p.theme_gen = p.theme_gen.wrapping_add(1);
            if theme.hc.is_some() {
                // A selection fading as high contrast comes on ends where
                // it is going (design system 12.3: fills change at once).
                p.anim.snap_fills();
            }
            if let Err(e) = p.build_chrome() {
                crate::log!("stats: retheme failed: {e}");
            }
            p.redraw(sysstat::clock_ms());
            p.refresh_tip();
        }
    });
}

/// Shows the panel under `button` (screen px; over it, `up`, for a bar at
/// the bottom) until it closes, its details open or shut as `details` says
/// (the setting). Returns the details' state if the user changed it, for
/// the caller to save. Must not be called inside a RefCell borrow of the
/// caller (the loop dispatches).
pub(crate) fn show(gfx: &Rc<Gfx>, theme: &Theme, button: RECT, scale: f32, up: bool, details: bool) -> Option<bool> {
    if is_open() || super::control::is_open() || crate::popup::is_tracking() {
        return None;
    }
    let mut changed = None;
    unsafe {
        let hinst = GetModuleHandleW(None).unwrap_or_default();
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wndproc),
            hInstance: hinst.into(),
            lpszClassName: w!("NopBarStats"),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            ..Default::default()
        };
        RegisterClassExW(&wc);
        let panel = match Panel::new(gfx, theme, button, scale, up, details) {
            Ok(p) => p,
            Err(e) => {
                crate::log!("stats: cannot open: {e}");
                return None;
            }
        };
        let hwnd = panel.hwnd;
        PANEL.with(|c| *c.borrow_mut() = Some(panel));
        // The sampler reads the panel's numbers from now on (its own,
        // started for the panel and ended when it closes).
        set_sampling(true);
        with_panel(|p| {
            p.take(snapshot());
            p.take_lists();
            p.refresh(How::Auto, true);
            // Nothing eases in as it opens: the panel itself does.
            p.settle();
        });
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        let _ = SetForegroundWindow(hwnd);
        let _ = SetFocus(Some(hwnd));
        SetCapture(hwnd);
        let _ = SetTimer(Some(hwnd), WAIT_TIMER, crate::motion::TIP_DELAY_MS as u32, None);
        a11y::menu_event(hwnd, true);
        if GetForegroundWindow() != hwnd || GetCapture() != hwnd {
            crate::log!("stats: could not take foreground/capture; not opening");
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
        let _ = KillTimer(Some(hwnd), FRAME_TIMER);
        let _ = KillTimer(Some(hwnd), WAIT_TIMER);
        super::desktops::hide_tip();
        let mut after = None;
        if let Some(mut p) = PANEL.with(|c| c.borrow_mut().take()) {
            after = p.after.take();
            changed = (p.details != p.details_at_open).then_some(p.details);
            if let Some(press) = p.outside {
                PRESS_CLOSED.with(|c| c.set(Some((std::time::Instant::now(), press))));
            }
            a11y::menu_event(p.hwnd, false);
            // A quick fade, then the window goes (crate::motion).
            let (hwnd, c, outer) = (p.hwnd, p.gfx.compositor.clone(), p.outer.clone());
            crate::motion::retire(hwnd, &c, &outer, Box::new(p));
        }
        set_sampling(false);
        match after {
            // On the launch helper (its own thread, with COM): a slow
            // shell never holds up the dock and the bar.
            Some(After::TaskManager) => crate::launch::open_soon(task_manager()),
            // The app pressed, re-checked window by window just before.
            Some(After::Show(h)) => crate::hogs::show(&h),
            Some(After::Quit(h)) => crate::hogs::quit(&h),
            None => {}
        }
    }
    changed
}

/// Pure: what the panel's Task Manager button opens (handed to
/// launch::open_soon, never run on the UI thread).
fn task_manager() -> crate::launch::Target {
    crate::launch::Target::new("taskmgr.exe", t("topbar.panel.power.task_manager"), crate::launch::Source::TopBar)
}

// ---- the sampler (one while the panel is open) ---------------------------------

/// The sampler's message: a new snapshot is ready.
const WM_SAMPLE: u32 = WM_APP + 60;

/// Pure: what the sampler reads: everything the panel shows, nothing at
/// all while the session is locked or the display is off (`resting`).
fn want(resting: bool) -> Want {
    if resting {
        Want::default()
    } else {
        Want { run: true, full: true }
    }
}

/// The panel's sampler: a hidden window for its messages and the session
/// lock and display state notifications, started as the panel opens and
/// ended as it closes.
struct Engine {
    hwnd: HWND,
    sampler: Sampler,
    locked: bool,
    display_off: bool,
    power: Option<HPOWERNOTIFY>,
    wts: bool,
}

thread_local! {
    static ENGINE: RefCell<Option<Engine>> = const { RefCell::new(None) };
}

fn with_engine<R>(f: impl FnOnce(&mut Engine) -> R) -> Option<R> {
    ENGINE.with(|e| e.try_borrow_mut().ok().and_then(|mut g| g.as_mut().map(f)))
}

impl Engine {
    unsafe fn new() -> Result<Engine> {
        let hinst = GetModuleHandleW(None)?;
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(engine_proc),
            hInstance: hinst.into(),
            lpszClassName: w!("NopBarStatsSampler"),
            ..Default::default()
        };
        RegisterClassExW(&wc);
        // A plain hidden popup (not message-only): session and power
        // notifications are delivered to it.
        let hwnd = CreateWindowExW(WS_EX_TOOLWINDOW, w!("NopBarStatsSampler"), w!("Stats"), WS_POPUP, 0, 0, 0, 0, None, None, Some(hinst.into()), None)?;
        let power = RegisterPowerSettingNotification(windows::Win32::Foundation::HANDLE(hwnd.0), &GUID_CONSOLE_DISPLAY_STATE, REGISTER_NOTIFICATION_FLAGS(0)).ok();
        let wts = WTSRegisterSessionNotification(hwnd, NOTIFY_FOR_THIS_SESSION).is_ok();
        Ok(Engine { hwnd, sampler: Sampler::new(hwnd, WM_SAMPLE), locked: false, display_off: false, power, wts })
    }

    fn close(self) {
        unsafe {
            if let Some(p) = self.power {
                let _ = UnregisterPowerSettingNotification(p);
            }
            if self.wts {
                let _ = WTSUnRegisterSessionNotification(self.hwnd);
            }
            let _ = DestroyWindow(self.hwnd);
        }
        // The sampler (dropped here) stops its thread.
    }

    /// Tells the sampler what is wanted now.
    fn retune(&mut self) {
        let w = want(self.locked || self.display_off);
        self.sampler.set(w);
    }
}

/// The panel opened (starts its sampler) or closed (ends it).
fn set_sampling(open: bool) {
    if !open {
        if let Some(e) = ENGINE.with(|e| e.try_borrow_mut().ok().and_then(|mut g| g.take())) {
            e.close();
        }
        return;
    }
    let exists = ENGINE.with(|e| e.try_borrow().map(|g| g.is_some()).unwrap_or(true));
    if !exists {
        match unsafe { Engine::new() } {
            Ok(e) => ENGINE.with(|g| *g.borrow_mut() = Some(e)),
            Err(e) => {
                crate::log!("stats: cannot start the sampler: {e}");
                return;
            }
        }
    }
    with_engine(|e| e.retune());
}

/// The latest readings.
fn snapshot() -> Snapshot {
    with_engine(|e| e.sampler.snapshot()).unwrap_or_default()
}

unsafe extern "system" fn engine_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_SAMPLE => {
            sampled();
            LRESULT(0)
        }
        WM_WTSSESSION_CHANGE => {
            let lock = match wp.0 as u32 {
                WTS_SESSION_LOCK => Some(true),
                WTS_SESSION_UNLOCK => Some(false),
                _ => None,
            };
            if let Some(l) = lock {
                with_engine(|e| {
                    e.locked = l;
                    e.retune();
                });
            }
            LRESULT(0)
        }
        WM_POWERBROADCAST if wp.0 as u32 == PBT_POWERSETTINGCHANGE && lp.0 != 0 => {
            let s = &*(lp.0 as *const POWERBROADCAST_SETTING);
            if s.PowerSetting == GUID_CONSOLE_DISPLAY_STATE && s.DataLength >= 4 {
                // 0 off, 1 on, 2 dimmed (still on).
                let off = u32::from_le_bytes(std::ptr::read_unaligned(s.Data.as_ptr() as *const [u8; 4])) == 0;
                with_engine(|e| {
                    e.display_off = off;
                    e.retune();
                });
            }
            LRESULT(1)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

impl Panel {
    unsafe fn new(gfx: &Rc<Gfx>, theme: &Theme, button: RECT, s: f32, up: bool, details: bool) -> Result<Self> {
        let power = sysstat::read_power();
        let battery = power.battery.is_some();
        let on_battery = crate::hogs::shown(battery && !power.on_ac);
        let vram_row = gfx.video_memory().is_some();
        // Its window is as tall as it can get (the culprit's row grown,
        // the details open): what changes afterwards is the plate inside.
        let tallest = layout(1.0, 1.0, battery, false).height;
        let h = layout(0.0, if details { 1.0 } else { 0.0 }, battery, false).height;
        let rtl = crate::i18n::rtl();
        // The button's own monitor (its middle: a bar at the bottom ends on
        // the next display's edge).
        let at = POINT { x: (button.left + button.right) / 2, y: (button.top + button.bottom) / 2 };
        let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        let _ = GetMonitorInfoW(MonitorFromPoint(at, MONITOR_DEFAULTTONEAREST), &mut mi);
        let (room, fit) = place(button, mi.rcWork, s, tallest, rtl, up);
        let m_px = (SHADOW_M * s).ceil() as i32;
        let hwnd = CreateWindowExW(
            WS_EX_NOREDIRECTIONBITMAP | WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
            w!("NopBarStats"),
            &windows::core::HSTRING::from(t("topbar.stats.title")),
            WS_POPUP,
            room.left - m_px,
            room.top - m_px,
            room.right - room.left + m_px * 2,
            room.bottom - room.top + m_px * 2,
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
            let ds = s * fit;
            outer.SetScale(Vector3 { X: ds, Y: ds, Z: 1.0 })?;
            target.SetRoot(&outer)?;
            let content = c.CreateSpriteVisual()?;
            let mut p = Panel {
                hwnd,
                gfx: gfx.clone(),
                theme: theme.clone(),
                dim: secondary_text(theme),
                scale: s,
                fit,
                _target: target,
                outer,
                content,
                room,
                up,
                rect: room,
                shown_h: 0.0,
                shift: (0.0, 0.0),
                rtl,
                dec: super::about::decimal_mark(),
                battery,
                on_battery,
                vram_row,
                vram: None,
                power: Vec::new(),
                gpu: Vec::new(),
                measured: false,
                snap: Snapshot::default(),
                parts: None,
                split: None,
                samples: 0,
                last_seq: None,
                opened: sysstat::clock_ms(),
                seen: false,
                levels: [None; 4],
                picked: None,
                details,
                details_at_open: details,
                name_w: NAME_MIN,
                view: View::blank(),
                ranked_at: 0,
                old_list: None,
                swap: (false, [false; APPS]),
                old_word: None,
                anim: Anim::default(),
                ticking: false,
                hover: None,
                pressed: None,
                focus: None,
                keyed: false,
                done: false,
                outside: None,
                after: None,
                hits: Vec::new(),
                tip: None,
                theme_gen: 0,
            };
            p.name_w = p.measure_names();
            p.resize(h)?;
            // It grows from its button (down from a bar at the top, up from
            // one at the bottom) and fades in.
            let from = (((button.left + button.right) / 2 - p.rect.left) as f32 / ds, ((button.top + button.bottom) / 2 - p.rect.top) as f32 / ds);
            let before = p.outer.Offset()?;
            let _ = crate::motion::open(&c, &p.outer, (W, h), from, crate::motion::Kind::Panel, ds);
            let after = p.outer.Offset()?;
            p.shift = (after.X - before.X, after.Y - before.Y);
            Ok(p)
        })();
        if built.is_err() {
            let _ = DestroyWindow(hwnd);
        }
        built
    }

    fn ds(&self) -> f32 {
        self.scale * self.fit
    }

    /// The name column for the four names as this theme draws them.
    fn measure_names(&self) -> f32 {
        name_col(RES.iter().filter_map(|r| self.styled(res_label(*r), ramp::BODY_STRONG)).map(|l| l.1).fold(0.0, f32::max))
    }

    /// The plate `h` DIP tall at the button's edge of the room: the visual
    /// tree's place, the chrome for that size, the rect as shown.
    fn resize(&mut self, h: f32) -> Result<()> {
        if (h - self.shown_h).abs() < 0.01 {
            return Ok(());
        }
        self.shown_h = h;
        let h_px = ((h * self.ds()).ceil() as i32).min(self.room.bottom - self.room.top);
        self.rect = if self.up { RECT { top: self.room.bottom - h_px, ..self.room } } else { RECT { bottom: self.room.top + h_px, ..self.room } };
        let m_px = (SHADOW_M * self.scale).ceil();
        self.outer.SetOffset(Vector3 { X: m_px + self.shift.0, Y: m_px + (self.rect.top - self.room.top) as f32 + self.shift.1, Z: 0.0 })?;
        self.content.SetSize(Vector2 { X: W, Y: h })?;
        self.build_chrome()
    }

    /// (Re)builds the material, shadow and border for the theme and the
    /// plate's size, with the content inside.
    fn build_chrome(&mut self) -> Result<()> {
        // A visual has one parent: out of the old chrome first.
        if let Ok(parent) = self.content.Parent() {
            let _ = parent.Children().and_then(|c| c.Remove(&self.content));
        }
        self.outer.Children()?.RemoveAll()?;
        let (root, border) = crate::popup::chrome(&self.gfx, &self.theme, &self.outer, W, self.shown_h, self.ds(), self.theme.radii().panel)?;
        root.Children()?.InsertBelow(&self.content, &border)?;
        Ok(())
    }

    /// A snapshot in (a new reading counts by its seq): NopBar's split,
    /// and each resource's level (the verdict's hysteresis lives here).
    fn take(&mut self, s: Snapshot) {
        if self.last_seq != Some(s.seq) {
            self.last_seq = Some(s.seq);
            self.samples = self.samples.saturating_add(1);
            // NopBar's CPU split by what its parts took since the last one.
            let (bar, widgets) = crate::desklayer::load::parts_time();
            let now = (sysstat::own_cpu_time(), bar, widgets);
            if let (Some(p), Some(cpu)) = (self.parts, s.own.and_then(|o| o.cpu)) {
                let d = |a: u64, b: u64| a.saturating_sub(b);
                self.split = Some(crate::desklayer::load::split(cpu, d(now.0, p.0), d(now.1, p.1), d(now.2, p.2)));
            }
            self.parts = Some(now);
            self.judge(&s);
        }
        self.snap = s;
    }

    /// Each resource's level from `s` (a resource with no value now is left
    /// out), and whether any number has come.
    fn judge(&mut self, s: &Snapshot) {
        for r in RES {
            let was = self.levels[r.i()].unwrap_or(Level::Calm);
            self.levels[r.i()] = res_value(r, s).and(pressure(res_history(r, s), s.at)).map(|p| level(r, p, was));
            self.seen |= res_value(r, s).is_some();
        }
    }

    /// crate::hogs' lists and NopBar's GPU memory, with the new reading.
    fn take_lists(&mut self) {
        if self.vram_row {
            self.vram = self.gfx.video_memory();
        }
        if self.on_battery {
            self.power = crate::hogs::list();
        }
        self.gpu = crate::hogs::list_gpu();
        self.measured = crate::hogs::measured();
    }

    /// The bar's row the keyboard's cursor is on.
    fn cursor(&self) -> Option<Res> {
        match self.focus {
            Some(Hit::Meter(r)) if self.keyed => Some(r),
            _ => None,
        }
    }

    /// The view now. `due`: the apps are ranked again (else the rows shown
    /// keep their places).
    fn compute(&self, now: u64, due: bool) -> View {
        let ph = phase(now.saturating_sub(self.opened), self.seen);
        let live = ph == Phase::Live;
        let v = if live { verdict(self.levels) } else { None };
        let meters = std::array::from_fn(|i| {
            let r = RES[i];
            let value = res_value(r, &self.snap).filter(|_| live);
            let peak = minute(res_history(r, &self.snap), self.snap.at).map(|m| m.1).filter(|p| value.is_some_and(|v| p - v >= PEAK_MIN));
            Meter { value, peak, hot: value.is_some() && v.is_some_and(|v| v.pressed(r)) }
        });
        let lead = v.and_then(|v| v.lead);
        let lens = lens(lead, self.on_battery, self.picked);
        let fresh = if live { rows_of(lens, &self.snap, &self.power, &self.gpu, self.dec) } else { Vec::new() };
        let ranked_now = due || matches!(lens, Lens::Gpu | Lens::Power);
        let shown: Vec<String> = if self.view.list.lens == lens { self.view.list.rows.iter().map(|r| r.hog.key.clone()).collect() } else { Vec::new() };
        let rows = ranked(&shown, fresh, ranked_now, APPS);
        let note = (rows.is_empty() && ph != Phase::Blank).then(|| {
            let measured = match lens {
                Lens::Cpu | Lens::Memory => self.samples >= 3,
                Lens::Gpu | Lens::Power => self.measured,
            };
            if live && measured { t("topbar.stats.hogs.none") } else { t("topbar.stats.measuring") }
        });
        let culprit = !rows.is_empty() && is_culprit(v.as_ref(), lens);
        let quit = rows.first().is_some_and(|r| r.hog.window) && (culprit || lens == Lens::Power);
        View { phase: ph, verdict: v, meters, sel: selected(lead, self.picked, self.cursor()).filter(|_| live), list: List { lens, rows, note, culprit, quit } }
    }

    /// The view worked out again, what changed eased from where it was,
    /// and drawn. `sample`: a new reading came (the apps may be ranked
    /// again).
    fn refresh(&mut self, how: How, sample: bool) {
        let now = sysstat::clock_ms();
        let due = sample && (self.view.list.rows.is_empty() || now.saturating_sub(self.ranked_at) >= RANK_MS);
        let new = self.compute(now, due);
        if due && !new.list.rows.is_empty() {
            self.ranked_at = now;
        }
        let old = std::mem::replace(&mut self.view, new);
        // What pointed at a row that is another app's now lets go.
        let (o, n) = (&old.list, &self.view.list);
        let keep = |h: Option<Hit>| h.filter(|h| same_target(*h, o, n));
        (self.focus, self.pressed, self.hover) = (keep(self.focus), keep(self.pressed), keep(self.hover));
        self.retarget(&old, how, now);
        self.sync(now);
    }

    /// Starts the tweens for what changed from `old` to the view.
    fn retarget(&mut self, old: &View, how: How, now: u64) {
        let new = self.view.clone();
        let on = |b: bool| if b { 1.0 } else { 0.0 };
        for r in RES {
            let (o, n) = (old.meters[r.i()], new.meters[r.i()]);
            let frac = |v: Option<f32>| v.unwrap_or(0.0).clamp(0.0, 100.0) / 100.0;
            self.anim.go(Key::Bar(r), frac(o.value), frac(n.value), CROSS_MS, OUT, now);
            match (o.peak, n.peak) {
                (Some(a), Some(b)) => self.anim.go(Key::Peak(r), frac(Some(a)), frac(Some(b)), CROSS_MS, OUT, now),
                _ => {
                    self.anim.map.remove(&Key::Peak(r));
                }
            }
            self.anim.go(Key::Hot(r), on(o.hot), on(n.hot), CROSS_MS, OUT, now);
            match (o.value.is_some(), n.value.is_some()) {
                // The dash crosses over to the value; a value lost goes at once.
                (false, true) if old.phase != Phase::Blank => self.anim.go(Key::Known(r), 0.0, 1.0, CROSS_MS, OUT, now),
                (a, b) if a != b => {
                    self.anim.map.remove(&Key::Known(r));
                }
                _ => {}
            }
            let (was, is) = (old.sel == Some(r), new.sel == Some(r));
            if was != is {
                match sel_fade(how, is, self.theme.hc.is_some()) {
                    Some((ms, ease)) => self.anim.go(Key::Sel(r), on(was), on(is), ms, ease, now),
                    None => {
                        self.anim.map.remove(&Key::Sel(r));
                    }
                }
            }
        }
        let state = |v: &View| v.verdict.map(|x| x.state);
        if state(old) != state(&new) && old.phase != Phase::Blank {
            self.old_word = Some(state(old));
            self.anim.go(Key::Word, 0.0, 1.0, CROSS_MS, OUT, now);
        }
        for i in 0..3u8 {
            self.anim.go(Key::Pip(i), on(old.lit() > i as usize), on(new.lit() > i as usize), CROSS_MS, OUT, now);
        }
        let changes = list_changes(&old.list, &new.list);
        if (changes.0 || changes.1.iter().any(|c| *c)) && old.phase != Phase::Blank {
            self.old_list = Some(old.list.clone());
            self.swap = changes;
            self.anim.go(Key::ListIn, 0.0, 1.0, CROSS_MS, OUT, now);
            self.anim.go(Key::ListOut, 1.0, 0.0, FADE_OUT_MS, IN, now);
        }
        self.anim.go(Key::Culprit, on(old.list.quit), on(new.list.quit), GLIDE_MS, Ease::Glide, now);
    }

    /// Every tween ended where it is going (the panel as it opens, a
    /// picture of it).
    fn settle(&mut self) {
        self.anim.map.clear();
        (self.old_list, self.old_word) = (None, None);
        self.sync(sysstat::clock_ms());
    }

    /// The layout at `now`: the first app row's growth and the details'
    /// opening where their glides are.
    fn layout_at(&self, now: u64) -> Layout {
        let on = |b: bool| if b { 1.0 } else { 0.0 };
        layout(self.anim.get(Key::Culprit, now, on(self.view.list.quit)), self.anim.get(Key::Open, now, on(self.details)), self.battery, self.view.phase == Phase::Failed)
    }

    /// The plate at its height for `now`, the content drawn, and frames
    /// kept coming while something eases.
    fn sync(&mut self, now: u64) {
        if let Err(e) = self.resize(self.layout_at(now).height) {
            crate::log!("stats: resize failed: {e}");
        }
        self.redraw(now);
        let busy = self.anim.busy(now);
        if !busy {
            self.anim.sweep(now);
            (self.old_list, self.old_word) = (None, None);
        }
        if busy != self.ticking {
            self.ticking = busy;
            unsafe {
                if busy {
                    let _ = SetTimer(Some(self.hwnd), FRAME_TIMER, FRAME_MS, None);
                } else {
                    let _ = KillTimer(Some(self.hwnd), FRAME_TIMER);
                }
            }
        }
    }

    // ---- drawing ------------------------------------------------------------

    /// Text of `level` on something in `s` (the theme's; secondary raised
    /// to read on a Glass panel: `dim`).
    fn ink(&self, level: Ink, s: State) -> D2D1_COLOR_F {
        if level == Ink::Secondary && self.theme.hc.is_none() {
            return rgba(self.dim);
        }
        rgba(self.theme.text_of(level, s))
    }

    /// `r` (laid out left to right) where it is drawn.
    fn m(&self, r: D2D_RECT_F) -> D2D_RECT_F {
        mirrored(self.rtl, r)
    }

    /// Text in a ramp style (its optical size and tracking), in DIP.
    fn styled(&self, text: &str, style: TextStyle) -> Option<Laid> {
        self.gfx.text_style(text, &self.theme.font.family, style, 1.0).ok()
    }

    /// Body numbers that keep their width as they change (tabular figures).
    /// In a right-to-left language a value stays in one piece, read left
    /// to right ("9.8 GB", never "GB 9.8"): it is set apart from the line's
    /// direction (a left-to-right embedding).
    fn num(&self, text: &str, strong: bool) -> Option<Laid> {
        self.figures(text, strong, true)
    }

    /// Tabular text; `apart`: a value with its unit (see num). Words (how
    /// long the PC has been on) follow the line's direction instead.
    fn figures(&self, text: &str, strong: bool, apart: bool) -> Option<Laid> {
        let weight = if strong { DWRITE_FONT_WEIGHT_SEMI_BOLD } else { DWRITE_FONT_WEIGHT_NORMAL };
        let held;
        let text = if self.rtl && apart {
            held = format!("\u{202A}{text}\u{202C}");
            &held
        } else {
            text
        };
        self.gfx.text_layout_tabular(text, &self.theme.font.family, ramp::BODY.size, weight).ok()
    }

    /// Text cut with "…" to fit `maxw` DIP, and whether it was cut.
    fn fit(&self, text: &str, style: TextStyle, maxw: f32) -> Option<(Laid, bool)> {
        let l = self.styled(text, style)?;
        if l.1 <= maxw {
            return Some((l, false));
        }
        let chars: Vec<char> = text.chars().collect();
        let cut = |n: usize| format!("{}…", chars[..n].iter().collect::<String>().trim_end());
        let (mut lo, mut hi) = (0usize, chars.len());
        while lo < hi {
            let mid = (lo + hi).div_ceil(2);
            if self.styled(&cut(mid), style).is_some_and(|l| l.1 <= maxw) {
                lo = mid;
            } else {
                hi = mid - 1;
            }
        }
        self.styled(&cut(lo), style).map(|l| (l, true))
    }

    /// A text in `x0..x1` (left to right; reflected when right to left),
    /// its middle on `cy`. Returns its width.
    #[allow(clippy::too_many_arguments)]
    unsafe fn text_in(&self, dc: &ID2D1DeviceContext, l: &Laid, x0: f32, x1: f32, cy: f32, align: Align, c: D2D1_COLOR_F) -> Result<f32> {
        if c.a > 0.0 {
            let x = text_x(align, x0, x1, l.1, self.rtl);
            let b = dc.CreateSolidColorBrush(&c, None)?;
            crate::gfx::draw_text(dc, Vector2 { X: x, Y: cy - l.2 / 2.0 }, &l.0, &b, D2D1_DRAW_TEXT_OPTIONS_NONE);
        }
        Ok(l.1)
    }

    /// A name or a number in `x0..x1` crossing from at ease (regular, the
    /// secondary ink) to under pressure (strong, the primary) as `hot`
    /// goes 0..1; `tabular`: a number. On a row with a fill of its own
    /// (selected, the keyboard's cursor, pressed) the ink is the primary
    /// at either weight: the secondary does not read on that fill.
    #[allow(clippy::too_many_arguments)]
    unsafe fn label(&self, dc: &ID2D1DeviceContext, text: &str, x0: f32, x1: f32, cy: f32, align: Align, hot: f32, s: State, tabular: bool, alpha: f32) -> Result<()> {
        for (strong, a) in [(false, 1.0 - hot), (true, hot)] {
            if a * alpha <= 0.004 {
                continue;
            }
            let l = if tabular { self.num(text, strong) } else { self.fit(text, if strong { ramp::BODY_STRONG } else { ramp::BODY }, x1 - x0).map(|l| l.0) };
            if let Some(l) = l {
                let level = if strong || matches!(s, State::Selected | State::Cursor | State::Pressed) { Ink::Primary } else { Ink::Secondary };
                self.text_in(dc, &l, x0, x1, cy, align, faded(self.ink(level, s), a * alpha))?;
            }
        }
        Ok(())
    }

    /// A glyph centered on `cx` (left to right) and `cy`.
    unsafe fn glyph(&self, dc: &ID2D1DeviceContext, g: Glyph, cx: f32, cy: f32, size: f32, c: D2D1_COLOR_F) -> Result<()> {
        crate::gfx::draw_glyph(dc, g, mirror::x(self.rtl, cx, 0.0, W), cy, size, c)
    }

    unsafe fn fill_rr(&self, dc: &ID2D1DeviceContext, c: D2D1_COLOR_F, r: D2D_RECT_F, rad: f32) -> Result<()> {
        if c.a > 0.0 && r.right > r.left && r.bottom > r.top {
            let rad = rad.min((r.right - r.left) / 2.0).min((r.bottom - r.top) / 2.0);
            let b = dc.CreateSolidColorBrush(&c, None)?;
            dc.FillRoundedRectangle(&D2D1_ROUNDED_RECT { rect: r, radiusX: rad, radiusY: rad }, &b);
        }
        Ok(())
    }

    /// A stroke `width` wide inside `r`'s edge.
    unsafe fn stroke_rr(&self, dc: &ID2D1DeviceContext, c: D2D1_COLOR_F, r: D2D_RECT_F, rad: f32, width: f32) -> Result<()> {
        if c.a > 0.0 {
            let b = dc.CreateSolidColorBrush(&c, None)?;
            let h = width / 2.0;
            dc.DrawRoundedRectangle(&D2D1_ROUNDED_RECT { rect: rect(r.left + h, r.top + h, r.right - h, r.bottom - h), radiusX: (rad - h).max(0.0), radiusY: (rad - h).max(0.0) }, &b, width, None);
        }
        Ok(())
    }

    /// The theme's outline for `role` in `s` inside `r` (high contrast
    /// only: theme::outline_of), at `alpha`.
    unsafe fn outline(&self, dc: &ID2D1DeviceContext, role: Role, s: State, r: D2D_RECT_F, rad: f32, alpha: f32) -> Result<()> {
        match self.theme.outline_of(role, s) {
            Some((c, w)) => self.stroke_rr(dc, faded(rgba(c), alpha), r, rad, w),
            None => Ok(()),
        }
    }

    /// A card that cannot be pressed (it never changes): the card's rest
    /// fill; an outline in high contrast. `r` as drawn.
    unsafe fn card(&self, dc: &ID2D1DeviceContext, r: D2D_RECT_F, alpha: f32) -> Result<()> {
        let rad = self.theme.radii().card;
        crate::gfx::parts::card(dc, r, rad, &self.theme, State::Rest, self.theme.fill_of(Role::Raised, State::Rest), 1.0, alpha)?;
        self.outline(dc, Role::Raised, State::Rest, r, rad, alpha)
    }

    /// It answers the pointer (a fill under it, a press): a bar's row that
    /// picks a lens, an app with a window, the buttons. The rest only
    /// explain.
    /// A row that holds another app while the list crosses over, and its
    /// Quit, answer only once the crossing is over (settled).
    fn reacts(&self, h: Hit) -> bool {
        let live = match h {
            Hit::Meter(r) => self.pickable(r),
            Hit::App(i) => self.view.list.rows.get(i).is_some_and(|r| r.hog.window),
            Hit::Quit => self.view.list.quit,
            Hit::More | Hit::Retry | Hit::TaskManager => true,
            Hit::Info | Hit::Battery | Hit::Own => false,
        };
        live && settled(h, self.old_list.as_ref(), &self.view.list, self.crossing(sysstat::clock_ms()))
    }

    /// The list is crossing over at `now`: its new rows are still coming in.
    fn crossing(&self, now: u64) -> bool {
        self.old_list.is_some() && self.anim.get(Key::ListIn, now, 1.0) < 1.0
    }

    /// A bar's row picks a lens: it has one, and its value is known.
    fn pickable(&self, r: Res) -> bool {
        Lens::of(r).is_some() && self.view.meters[r.i()].value.is_some()
    }

    /// How far the pointer's fill is on `h` at `now` (0..1).
    fn hovered(&self, h: Hit, now: u64) -> f32 {
        if !self.reacts(h) || self.pressed == Some(h) {
            return 0.0;
        }
        self.anim.get(Key::Hover(h), now, if self.hover == Some(h) { 1.0 } else { 0.0 })
    }

    /// A row that is part of its surface (design system 6.2): no fill at
    /// rest; the row fills under the pointer, pressed, selected (`sel`:
    /// 0..1) or under the keyboard's cursor. `r` as drawn. Returns its
    /// state (its text's color in high contrast).
    #[allow(clippy::too_many_arguments)]
    unsafe fn row(&self, dc: &ID2D1DeviceContext, hit: Hit, r: D2D_RECT_F, rad: f32, sel: f32, cursor: bool, now: u64) -> Result<State> {
        let pressed = self.pressed == Some(hit) && self.reacts(hit);
        let hover = self.hovered(hit, now);
        let sel = sel_fill(self.theme.hc.is_some(), sel);
        let fill = |s: State| rgba(self.theme.fill_of(Role::Row, s));
        let s = row_state(sel, hover, cursor, pressed);
        if pressed {
            self.fill_rr(dc, fill(State::Pressed), r, rad)?;
        } else {
            let on = if cursor { 1.0 } else { sel };
            self.fill_rr(dc, faded(fill(if cursor { State::Cursor } else { State::Selected }), on), r, rad)?;
            self.fill_rr(dc, faded(fill(State::Hover), hover * (1.0 - on)), r, rad)?;
        }
        self.outline(dc, Role::Row, s, r, rad, 1.0)?;
        Ok(s)
    }

    /// Something raised (a button; design system 6.2): the card's fill at
    /// rest, lighter under the pointer, darker and a little smaller
    /// pressed; `contact`: it sits on a card, and the contact shadow gives
    /// it its edge (a white button on a white card). The keyboard's ring
    /// when it has the focus. `r` as drawn; returns it as drawn now and
    /// its state.
    unsafe fn raised(&self, dc: &ID2D1DeviceContext, hit: Hit, r: D2D_RECT_F, rad: f32, contact: bool, now: u64) -> Result<(D2D_RECT_F, State)> {
        let pressed = self.pressed == Some(hit);
        let hover = self.hovered(hit, now);
        let press = self.anim.get(Key::Press(hit), now, if pressed { 1.0 } else { 0.0 });
        let r = scaled_rect(r, 1.0 - (1.0 - crate::motion::press_scale(r.right - r.left, r.bottom - r.top)) * press);
        let s = if pressed {
            State::Pressed
        } else if hover > 0.5 {
            State::Hover
        } else {
            State::Rest
        };
        let fill = |s: State| rgba(self.theme.fill_of(Role::Raised, s));
        if contact {
            let (c, k) = (self.theme.contact(), crate::theme::CONTACT_W);
            if c.a > 0 {
                let b = dc.CreateSolidColorBrush(&rgba(c), None)?;
                dc.DrawRoundedRectangle(&D2D1_ROUNDED_RECT { rect: rect(r.left - k / 2.0, r.top - k / 2.0, r.right + k / 2.0, r.bottom + k / 2.0), radiusX: rad + k / 2.0, radiusY: rad + k / 2.0 }, &b, k, None);
            }
        }
        // Flat v2's card light round it (gfx::parts::card_under / card_over).
        crate::gfx::parts::card_under(dc, r, rad, &self.theme, s, 1.0, 1.0)?;
        self.fill_rr(dc, if pressed { fill(State::Pressed) } else { mixed(fill(State::Rest), fill(State::Hover), hover) }, r, rad)?;
        crate::gfx::parts::card_over(dc, r, rad, &self.theme, s, 1.0, 1.0)?;
        self.outline(dc, Role::Raised, s, r, rad, 1.0)?;
        self.ring(dc, hit, r, rad)?;
        Ok((r, s))
    }

    /// The keyboard's focus ring around `r` when `hit` has the focus and
    /// the keyboard put it there.
    unsafe fn ring(&self, dc: &ID2D1DeviceContext, hit: Hit, r: D2D_RECT_F, rad: f32) -> Result<()> {
        if self.keyed && self.focus == Some(hit) {
            crate::gfx::focus_ring(dc, r, rad, &self.theme, 1.0)?;
        }
        Ok(())
    }

    /// An app icon (premultiplied BGRA, `side` px square) in `r` (as
    /// drawn).
    unsafe fn icon(&self, dc: &ID2D1DeviceContext, side: u32, px: &[u8], r: D2D_RECT_F, alpha: f32) -> Result<()> {
        if px.len() != (side * side * 4) as usize || side == 0 {
            return Ok(());
        }
        let props = D2D1_BITMAP_PROPERTIES1 {
            pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
            dpiX: 96.0,
            dpiY: 96.0,
            bitmapOptions: D2D1_BITMAP_OPTIONS_NONE,
            colorContext: std::mem::ManuallyDrop::new(None),
        };
        let bmp = dc.CreateBitmap(D2D_SIZE_U { width: side, height: side }, Some(px.as_ptr() as _), side * 4, &props)?;
        dc.DrawBitmap(&bmp, Some(&r), alpha, D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC, None, None);
        Ok(())
    }

    fn redraw(&mut self, now: u64) {
        let s = self.ds();
        let (pw, ph) = ((W * s).ceil() as u32, (self.shown_h * s).ceil().max(1.0) as u32);
        let mut hits = Vec::new();
        let me = &*self;
        let surface = self.gfx.draw(pw, ph, |dc, ox, oy| unsafe {
            dc.SetAntialiasMode(D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
            dc.SetTransform(&Matrix3x2 { M11: s, M12: 0.0, M21: 0.0, M22: s, M31: ox, M32: oy });
            let r = me.paint(dc, &mut hits, now);
            dc.SetTransform(&Matrix3x2::identity());
            r
        });
        self.hits = hits;
        match surface {
            Ok(surface) => {
                if let Ok(brush) = self.gfx.compositor.CreateSurfaceBrushWithSurface(&surface) {
                    let _ = brush.SetStretch(CompositionStretch::Fill);
                    let _ = self.content.SetBrush(&brush);
                }
            }
            Err(e) => crate::log!("stats: draw failed: {e}"),
        }
    }

    /// The state's word (None: the dash, while it is not known).
    unsafe fn word(&self, dc: &ID2D1DeviceContext, head: D2D_RECT_F, state: Option<Level>, alpha: f32) -> Result<()> {
        let (text, level) = match state {
            Some(l) => (level_word(l), Ink::Primary),
            None => (NONE, Ink::Tertiary),
        };
        let room = pips(head)[0].left - GAP - (head.left + ROW_PAD);
        if let Some((l, _)) = self.fit(text, ramp::TITLE, room) {
            self.text_in(dc, &l, head.left + ROW_PAD, head.right - ROW_PAD, mid(head), Align::Start, faded(self.ink(level, State::Rest), alpha))?;
        }
        Ok(())
    }

    /// Is the PC all right, and what is under pressure: the word, the
    /// three dots, the four bars.
    unsafe fn paint_state(&self, dc: &ID2D1DeviceContext, hits: &mut Vec<(D2D_RECT_F, Hit)>, lay: &Layout, now: u64) -> Result<()> {
        let v = &self.view;
        self.card(dc, self.m(lay.state), 1.0)?;
        if v.phase != Phase::Blank {
            let a = self.anim.get(Key::Word, now, 1.0);
            if let Some(old) = self.old_word.filter(|_| a < 1.0) {
                self.word(dc, lay.head, old, 1.0 - a)?;
            }
            self.word(dc, lay.head, v.verdict.map(|x| x.state), a)?;
        }
        // The dots: a well each; lit, the "on" fill over it.
        for (i, p) in pips(lay.head).into_iter().enumerate() {
            let on = self.anim.get(Key::Pip(i as u8), now, if v.lit() > i { 1.0 } else { 0.0 });
            let r = self.m(p);
            if on < 1.0 {
                crate::gfx::well(dc, r, PIP / 2.0, &self.theme, 1.0)?;
            }
            self.fill_rr(dc, faded(rgba(on_fill(&self.theme)), on), r, PIP / 2.0)?;
        }
        for (res, r) in RES.into_iter().zip(lay.meters) {
            self.paint_meter(dc, hits, res, r, now)?;
        }
        Ok(())
    }

    /// A bar's row: its name, its bar with last minute's peak, its value.
    unsafe fn paint_meter(&self, dc: &ID2D1DeviceContext, hits: &mut Vec<(D2D_RECT_F, Hit)>, res: Res, r: D2D_RECT_F, now: u64) -> Result<()> {
        let v = &self.view;
        let m = v.meters[res.i()];
        let hit = Hit::Meter(res);
        let on = |b: bool| if b { 1.0 } else { 0.0 };
        let sel = self.anim.get(Key::Sel(res), now, on(v.sel == Some(res)));
        let cursor = self.cursor() == Some(res);
        hits.push((self.m(r), hit));
        let s = self.row(dc, hit, self.m(r), self.theme.radii().control, sel, cursor, now)?;
        let hot = self.anim.get(Key::Hot(res), now, on(m.hot));
        let cy = mid(r);
        let (name, bar, value) = meter_parts(r, self.name_w);
        self.label(dc, res_label(res), name.0, name.1, cy, Align::Start, hot, s, false, 1.0)?;
        // The bar: a well, its fill from the start, the peak's mark.
        let filled = matches!(s, State::Selected | State::Cursor | State::Pressed);
        let (ease, pressed, peak_c) = bar_colors(&self.theme, filled);
        let track = self.m(bar);
        match self.theme.hc.filter(|_| filled) {
            // On a Highlight fill the well's outline is the row's text color.
            Some(_) => self.stroke_rr(dc, rgba(peak_c), track, TRACK_H / 2.0, 1.0)?,
            None => crate::gfx::well(dc, track, TRACK_H / 2.0, &self.theme, 1.0)?,
        }
        let span = bar.right - bar.left;
        let at = self.anim.get(Key::Bar(res), now, m.value.unwrap_or(0.0).clamp(0.0, 100.0) / 100.0);
        if at > 0.0 {
            // Never thinner than its own height: a sliver still reads as a bar.
            let w = (span * at).max(TRACK_H);
            self.fill_rr(dc, mixed(rgba(ease), rgba(pressed), hot), self.m(rect(bar.left, bar.top, bar.left + w, bar.bottom)), TRACK_H / 2.0)?;
        }
        if let Some(p) = m.peak {
            let x = bar.left + span * self.anim.get(Key::Peak(res), now, p.clamp(0.0, 100.0) / 100.0);
            self.fill_rr(dc, rgba(peak_c), self.m(rect(x - PEAK_W / 2.0, cy - PEAK_H / 2.0, x + PEAK_W / 2.0, cy + PEAK_H / 2.0)), PEAK_W / 2.0)?;
        }
        // The value; the dash while it is not known (crossing over when it comes).
        let known = self.anim.get(Key::Known(res), now, on(m.value.is_some()));
        if known < 1.0 && v.phase != Phase::Blank {
            if let Some(l) = self.num(NONE, false) {
                self.text_in(dc, &l, value.0, value.1, cy, Align::End, faded(self.ink(Ink::Tertiary, s), 1.0 - known))?;
            }
        }
        if m.value.is_some() {
            self.label(dc, &pct_text(m.value), value.0, value.1, cy, Align::End, hot, s, true, known)?;
        }
        Ok(())
    }

    /// Who uses it: the heading (what the list is ranked by) and its rows;
    /// while it changes, the last list going out under the new one.
    unsafe fn paint_apps(&self, dc: &ID2D1DeviceContext, hits: &mut Vec<(D2D_RECT_F, Hit)>, lay: &Layout, now: u64) -> Result<()> {
        self.card(dc, self.m(lay.apps), 1.0)?;
        let (ain, aout) = (self.anim.get(Key::ListIn, now, 1.0), self.anim.get(Key::ListOut, now, 0.0));
        let (head, rows) = self.swap;
        if let Some(old) = self.old_list.as_ref().filter(|_| aout > 0.0) {
            self.paint_list(dc, None, old, lay, [head, rows[0], rows[1], rows[2]].map(|c| if c { aout } else { 0.0 }), now)?;
        }
        let fresh = self.crossing(now);
        self.paint_list(dc, Some(hits), &self.view.list, lay, [head, rows[0], rows[1], rows[2]].map(|c| if c && fresh { ain } else { 1.0 }), now)
    }

    /// A list: `alpha` for its heading and each of its rows; `hits`: it is
    /// the live one (it answers the pointer and the keyboard).
    unsafe fn paint_list(&self, dc: &ID2D1DeviceContext, mut hits: Option<&mut Vec<(D2D_RECT_F, Hit)>>, list: &List, lay: &Layout, alpha: [f32; 4], now: u64) -> Result<()> {
        let h = lay.list_head;
        if alpha[0] > 0.0 {
            let c = faded(self.ink(Ink::Tertiary, State::Rest), alpha[0]);
            let room = h.right - h.left - 2.0 * ROW_PAD - INFO_GLYPH - space::S2;
            if let Some((l, _)) = self.fit(list.lens.label(), ramp::FOOTNOTE, room) {
                let w = self.text_in(dc, &l, h.left + ROW_PAD, h.right - ROW_PAD, mid(h), Align::Start, c)?;
                if list.lens == Lens::Power {
                    // The info mark: what the power list's numbers are.
                    let gx = h.left + ROW_PAD + w + space::S2;
                    self.glyph(dc, Glyph::Info, gx + INFO_GLYPH / 2.0, mid(h), INFO_GLYPH, c)?;
                    let hr = self.m(rect(h.left + ROW_PAD - space::S2, h.top, gx + INFO_GLYPH + space::S2, h.bottom));
                    if let Some(hits) = hits.as_deref_mut() {
                        hits.push((hr, Hit::Info));
                    }
                }
            }
        }
        if let Some(note) = list.note {
            let r = lay.rows[0];
            if let Some(l) = self.styled(note, ramp::BODY) {
                self.text_in(dc, &l, r.left + ROW_PAD, r.right - ROW_PAD, r.top + APP_H / 2.0, Align::Start, faded(self.ink(Ink::Tertiary, State::Rest), alpha[1]))?;
            }
            return Ok(());
        }
        for (i, (row, r)) in list.rows.iter().zip(lay.rows).enumerate() {
            let a = alpha[i + 1];
            if a <= 0.0 {
                continue;
            }
            let cy = mid(r);
            let hit = Hit::App(i);
            let s = match hits.as_deref_mut() {
                Some(hits) => {
                    hits.push((self.m(r), hit));
                    self.row(dc, hit, self.m(r), self.theme.radii().control, 0.0, self.keyed && self.focus == Some(hit), now)?
                }
                None => State::Rest,
            };
            // Its icon; NopBar's own mark; a plain window where none is read.
            let ir = app_icon_rect(r);
            let own = row.hog.procs.iter().any(|p| p.0 == std::process::id());
            match row.hog.icon.as_deref() {
                Some(icon) if !own => self.icon(dc, icon.0, &icon.1, self.m(ir), a)?,
                // NopBar itself: its gnome mark in the brand's two colors
                // (one color, the ink, in high contrast).
                _ if own => {
                    let cx = mirror::x(self.rtl, (ir.left + ir.right) / 2.0, 0.0, W);
                    crate::gfx::draw_mark(dc, &self.theme, (cx, cy), ICON, faded(self.ink(Ink::Secondary, s), a), a)?
                }
                _ => self.glyph(dc, Glyph::Window, (ir.left + ir.right) / 2.0, cy, ICON, faded(self.ink(Ink::Secondary, s), a))?,
            }
            let mut end = r.right - ROW_PAD;
            if i == 0 && list.quit {
                // "Quit": on the culprit's row only.
                if let Some(l) = self.styled(t("topbar.stats.hogs.quit"), ramp::BODY) {
                    let br = row_button(r, l.1 + 2.0 * BTN_PAD);
                    // Live once it answers: while it crosses in for another
                    // app (or newly), it fades in with its row and cannot be
                    // pressed yet (settled).
                    if let Some(hits) = hits.as_deref_mut() {
                        hits.push((self.m(br), Hit::Quit));
                    }
                    let text = match hits.as_deref_mut().filter(|_| self.reacts(Hit::Quit)) {
                        Some(_) => {
                            let (_, bs) = self.raised(dc, Hit::Quit, self.m(br), self.theme.radii().control, true, now)?;
                            self.ink(Ink::Primary, bs)
                        }
                        None => {
                            crate::gfx::parts::card(dc, self.m(br), self.theme.radii().control, &self.theme, State::Rest, self.theme.fill_of(Role::Raised, State::Rest), 1.0, a)?;
                            self.ink(Ink::Primary, State::Rest)
                        }
                    };
                    self.text_in(dc, &l, br.left, br.right, cy, Align::Mid, faded(text, a))?;
                    end = br.left - GAP;
                }
            }
            let vw = match self.num(&row.value, false) {
                Some(l) => self.text_in(dc, &l, ir.right + GAP, end, cy, Align::End, faded(self.ink(Ink::Secondary, s), a))?,
                None => 0.0,
            };
            let (n0, n1) = (ir.right + GAP, end - vw - GAP);
            let strong = i == 0 && list.culprit;
            if let Some((l, _)) = self.fit(&row.hog.name, if strong { ramp::BODY_STRONG } else { ramp::BODY }, (n1 - n0).max(10.0)) {
                self.text_in(dc, &l, n0, n1, cy, Align::Start, faded(self.ink(Ink::Primary, s), a))?;
            }
        }
        Ok(())
    }

    /// The room an app's name has in its row (as paint_list gives it).
    fn name_room(&self, i: usize) -> Option<f32> {
        let list = &self.view.list;
        let row = list.rows.get(i)?;
        let r = layout(if list.quit { 1.0 } else { 0.0 }, 0.0, self.battery, false).rows[i];
        let mut end = r.right - ROW_PAD;
        if i == 0 && list.quit {
            end -= self.styled(t("topbar.stats.hogs.quit"), ramp::BODY)?.1 + 2.0 * BTN_PAD + GAP;
        }
        Some(end - self.num(&row.value, false)?.1 - GAP - (app_icon_rect(r).right + GAP))
    }

    /// "Details": its row on the plate (the mark turns as it opens) and,
    /// as far as it is open, the card with the rest of the numbers.
    unsafe fn paint_more(&self, dc: &ID2D1DeviceContext, hits: &mut Vec<(D2D_RECT_F, Hit)>, lay: &Layout, now: u64) -> Result<()> {
        let on = |b: bool| if b { 1.0 } else { 0.0 };
        let r = lay.more;
        let rad = self.theme.radii().card;
        hits.push((self.m(r), Hit::More));
        let s = self.row(dc, Hit::More, self.m(r), rad, 0.0, false, now)?;
        self.ring(dc, Hit::More, self.m(r), rad)?;
        if let Some((l, _)) = self.fit(t("topbar.stats.more"), ramp::BODY, r.right - r.left - 2.0 * size::CARD_PAD - CHEVRON - GAP) {
            self.text_in(dc, &l, CONTENT, W - CONTENT, mid(r), Align::Start, self.ink(Ink::Primary, s))?;
        }
        // The mark: down when shut, half a turn round when open.
        let (cx, cy) = (mirror::x(self.rtl, W - CONTENT - CHEVRON / 2.0, 0.0, W), mid(r));
        let turn = self.anim.get(Key::Turn, now, on(self.details)) * std::f32::consts::PI;
        let (sn, cs) = turn.sin_cos();
        let mut old = Matrix3x2::identity();
        dc.GetTransform(&mut old);
        // A turn about the mark's middle, then the surface's own transform.
        let spin = Matrix3x2 { M11: cs, M12: sn, M21: -sn, M22: cs, M31: cx - cx * cs + cy * sn, M32: cy - cx * sn - cy * cs };
        dc.SetTransform(&times(spin, old));
        let drawn = crate::gfx::draw_glyph(dc, Glyph::ChevronDown, cx, cy, CHEVRON, self.ink(Ink::Tertiary, s));
        dc.SetTransform(&old);
        drawn?;
        let Some(card) = lay.details else { return Ok(()) };
        let ink = self.anim.get(Key::OpenInk, now, on(self.details));
        // Cut to the card as far as it is open (its lines stay where they
        // will be).
        dc.PushAxisAlignedClip(&card, D2D1_ANTIALIAS_MODE_ALIASED);
        let full = rect(card.left, card.top, card.right, lay.detail_rows.last().map_or(card.bottom, |l| l.bottom + INSET));
        let drawn = (|| -> Result<()> {
            self.card(dc, self.m(full), ink)?;
            for (d, r) in details_of(self.battery).into_iter().zip(&lay.detail_rows) {
                let (name, value, hit) = match d {
                    Detail::Network => (t("a11y.topbar.network"), net_text(&self.snap, self.dec), None),
                    Detail::Battery => (t("topbar.stats.battery"), self.snap.power.battery.map(|p| format!("{p}%")).unwrap_or_else(|| NONE.into()), Some(Hit::Battery)),
                    Detail::Uptime => (t("topbar.stats.uptime"), uptime_text(self.snap.uptime_ms), None),
                    Detail::Own => ("NopBar", own_short(&self.snap, self.dec), Some(Hit::Own)),
                };
                let (x0, x1, cy) = (r.left + ROW_PAD, r.right - ROW_PAD, mid(*r));
                let vw = match self.figures(&value, false, d != Detail::Uptime) {
                    Some(l) => self.text_in(dc, &l, x0, x1, cy, Align::End, faded(self.ink(Ink::Primary, State::Rest), ink))?,
                    None => 0.0,
                };
                if let Some((l, _)) = self.fit(name, ramp::BODY, (x1 - x0 - vw - GAP).max(10.0)) {
                    self.text_in(dc, &l, x0, x1, cy, Align::Start, faded(self.ink(Ink::Secondary, State::Rest), ink))?;
                }
                if let Some(h) = hit.filter(|_| self.details) {
                    hits.push((self.m(*r), h));
                    self.ring(dc, h, self.m(*r), self.theme.radii().control)?;
                }
            }
            Ok(())
        })();
        dc.PopAxisAlignedClip();
        drawn
    }

    /// "Cannot measure": the warning, one line, "Try again".
    unsafe fn paint_failed(&self, dc: &ID2D1DeviceContext, hits: &mut Vec<(D2D_RECT_F, Hit)>, card: D2D_RECT_F, now: u64) -> Result<()> {
        self.card(dc, self.m(card), 1.0)?;
        let cx = (card.left + card.right) / 2.0;
        let mut y = card.top + FAIL_PAD;
        self.glyph(dc, Glyph::Warning, cx, y + FAIL_GLYPH / 2.0, FAIL_GLYPH, self.ink(Ink::Primary, State::Rest))?;
        y += FAIL_GLYPH + space::S3;
        if let Some((l, _)) = self.fit(t("topbar.stats.failed"), ramp::BODY, card.right - card.left - 2.0 * size::CARD_PAD) {
            self.text_in(dc, &l, card.left, card.right, y + FAIL_LINE / 2.0, Align::Mid, self.ink(Ink::Primary, State::Rest))?;
        }
        y += FAIL_LINE + space::S4;
        if let Some(l) = self.styled(t("topbar.stats.retry"), ramp::BODY) {
            let w = l.1 + 2.0 * BTN_PAD;
            let br = rect(cx - w / 2.0, y, cx + w / 2.0, y + BTN_H);
            hits.push((self.m(br), Hit::Retry));
            let (_, s) = self.raised(dc, Hit::Retry, self.m(br), self.theme.radii().control, true, now)?;
            self.text_in(dc, &l, br.left, br.right, mid(br), Align::Mid, self.ink(Ink::Primary, s))?;
        }
        Ok(())
    }

    /// Task Manager: as wide as the cards, its name in the middle.
    unsafe fn paint_button(&self, dc: &ID2D1DeviceContext, hits: &mut Vec<(D2D_RECT_F, Hit)>, r: D2D_RECT_F, now: u64) -> Result<()> {
        hits.push((self.m(r), Hit::TaskManager));
        let (_, s) = self.raised(dc, Hit::TaskManager, self.m(r), self.theme.radii().card, false, now)?;
        if let Some((l, _)) = self.fit(t("topbar.panel.power.task_manager"), ramp::BODY, r.right - r.left - 2.0 * size::CARD_PAD) {
            self.text_in(dc, &l, r.left, r.right, mid(r), Align::Mid, self.ink(Ink::Primary, s))?;
        }
        Ok(())
    }

    unsafe fn paint(&self, dc: &ID2D1DeviceContext, hits: &mut Vec<(D2D_RECT_F, Hit)>, now: u64) -> Result<()> {
        let lay = self.layout_at(now);
        match lay.failed {
            Some(card) => self.paint_failed(dc, hits, card, now)?,
            None => {
                self.paint_state(dc, hits, &lay, now)?;
                self.paint_apps(dc, hits, &lay, now)?;
                self.paint_more(dc, hits, &lay, now)?;
            }
        }
        self.paint_button(dc, hits, lay.button, now)
    }

    // ---- words ----------------------------------------------------------------

    /// The details a hover shows (they are in the screen readers' text
    /// too): a bar's minute (the cores, memory in GB), an app's whole name
    /// when it was cut, what the power list's numbers are, the battery's
    /// time left, NopBar's own use in full.
    fn tip_text(&self, h: Hit) -> Option<String> {
        let s = &self.snap;
        match h {
            Hit::Meter(r) => self.view.meters[r.i()].value.and_then(|_| meter_tip(r, s, s.at, self.dec)),
            Hit::App(i) => {
                let row = self.view.list.rows.get(i)?;
                let style = if i == 0 && self.view.list.culprit { ramp::BODY_STRONG } else { ramp::BODY };
                (self.styled(&row.hog.name, style)?.1 > self.name_room(i)?).then(|| row.hog.name.clone())
            }
            Hit::Info => Some(t("topbar.stats.hogs.tip").to_string()),
            Hit::Battery => Some(battery_line(&s.power)),
            Hit::Own => Some(own_details(s, self.split, self.vram_row, self.vram, self.dec).join("\n")),
            _ => None,
        }
    }

    /// What there is to stop at (the keyboard, the screen readers).
    fn shape(&self) -> Shape {
        Shape { failed: self.view.phase == Phase::Failed, quit: self.view.list.quit, apps: self.view.list.rows.len(), details: self.details, battery: self.battery }
    }

    /// The tree for screen readers (only while one reads this window).
    fn publish(&self) {
        if a11y::wanted(self.hwnd) {
            a11y::publish(self.hwnd, self.tree());
        }
    }

    /// As read (design 7): the state, the four bars, the list's heading
    /// and rows, Quit, Details, its lines (as text even while it is shut),
    /// Task Manager.
    fn tree(&self) -> Tree {
        let (ds, origin) = (self.ds(), (self.rect.left, self.rect.top));
        let on = |b: bool| if b { 1.0 } else { 0.0 };
        let lay = layout(on(self.view.list.quit), on(self.details), self.battery, self.view.phase == Phase::Failed);
        let px = |r: D2D_RECT_F| {
            let r = mirrored(self.rtl, r);
            a11y::px_rect(origin, ds, r.left, r.top, r.right - r.left, r.bottom - r.top)
        };
        let at = |h: Hit| self.hits.iter().find(|(_, x)| *x == h).map(|(r, _)| a11y::px_rect(origin, ds, r.left, r.top, r.right - r.left, r.bottom - r.top)).unwrap_or_default();
        let (v, s) = (&self.view, &self.snap);
        let key = |h: Hit| key_of(h, &v.list.rows);
        let mut nodes: Vec<Node> = Vec::new();
        if let Some(card) = lay.failed {
            nodes.push(Node::new("state", Part::Text, format!("{}: {}", t("topbar.stats.title"), t("topbar.stats.failed")), px(card)));
            nodes.push(Node::new(key(Hit::Retry), Part::Button, t("topbar.stats.retry"), at(Hit::Retry)).invoke());
        } else {
            let word = v.verdict.map_or(NONE, |x| level_word(x.state));
            nodes.push(Node::new("state", Part::Text, format!("{}: {word}", t("topbar.stats.title")), px(lay.head)));
            for r in RES {
                let m = v.meters[r.i()];
                let mut name = format!("{} {}", res_label(r), pct_text(m.value));
                if m.hot {
                    name = format!("{name}, {}", t("topbar.stats.pressed"));
                }
                let help = m.value.and_then(|_| meter_tip(r, s, s.at, self.dec)).unwrap_or_default();
                let node = Node::new(key(Hit::Meter(r)), if self.pickable(r) { Part::Button } else { Part::Group }, name, px(lay.meters[r.i()])).help(help);
                nodes.push(if self.pickable(r) { node.invoke() } else { node });
            }
            let power = v.list.lens == Lens::Power;
            nodes.push(Node::new(key(Hit::Info), Part::Group, v.list.lens.label(), px(lay.list_head)).help(if power { t("topbar.stats.hogs.tip") } else { "" }));
            if let Some(note) = v.list.note {
                nodes.push(Node::new("app:none", Part::Text, note, px(lay.rows[0])));
            }
            for (i, row) in v.list.rows.iter().enumerate() {
                let name = if power { tf("topbar.stats.hogs.row", &[("app", &row.hog.name), ("p", &row.value)]) } else { format!("{}  {}", row.hog.name, row.value) };
                let node = Node::new(key(Hit::App(i)), if row.hog.window { Part::Button } else { Part::Text }, name, px(lay.rows[i]));
                nodes.push(if row.hog.window { node.invoke() } else { node });
            }
            if let Some(row) = v.list.rows.first().filter(|_| v.list.quit) {
                nodes.push(Node::new(key(Hit::Quit), Part::Button, format!("{} — {}", t("topbar.stats.hogs.quit"), row.hog.name), at(Hit::Quit)).invoke());
            }
            let fold = t(if self.details { "topbar.stats.expanded" } else { "topbar.stats.collapsed" });
            nodes.push(Node::new(key(Hit::More), Part::Button, format!("{}, {fold}", t("topbar.stats.more")), px(lay.more)).invoke().expand(self.details));
            for (i, d) in details_of(self.battery).into_iter().enumerate() {
                // Shut: its lines are still read, where "Details" is.
                let r = px(if self.details { lay.detail_rows[i] } else { lay.more });
                nodes.push(match d {
                    Detail::Network => Node::new("network", Part::Text, format!("{}  {}", t("a11y.topbar.network"), net_text(s, self.dec)), r),
                    Detail::Battery => Node::new(key(Hit::Battery), Part::Group, battery_line(&s.power), r),
                    Detail::Uptime => Node::new("uptime", Part::Text, format!("{}  {}", t("topbar.stats.uptime"), uptime_text(s.uptime_ms)), r),
                    Detail::Own => Node::new(key(Hit::Own), Part::Group, format!("NopBar  {}", own_short(s, self.dec)), r).help(own_details(s, self.split, self.vram_row, self.vram, self.dec).join("\n")),
                });
            }
        }
        nodes.push(Node::new(key(Hit::TaskManager), Part::Button, t("topbar.panel.power.task_manager"), at(Hit::TaskManager)).invoke().accel("Ctrl+Shift+Esc"));
        let focus = self.focus.map(key).unwrap_or_else(|| "taskmgr".into());
        Tree { role: Part::Group, name: t("topbar.stats.title").into(), nodes, focus: Some(focus), focusable: true, active: true, focus_in: None }
    }

    /// What a screen reader's key names (key_of), if the keyboard can stop
    /// at it now.
    fn hit_of(&self, key: &str) -> Option<Hit> {
        let shape = self.shape();
        let all = RES.into_iter().map(Hit::Meter).chain((0..shape.apps).map(Hit::App)).chain([Hit::Quit, Hit::More, Hit::Battery, Hit::Own, Hit::Retry, Hit::TaskManager]);
        let order = stops(shape);
        all.filter(|h| order.contains(&stop_of(*h))).find(|h| key_of(*h, &self.view.list.rows) == key)
    }

    // ---- input ----------------------------------------------------------------

    /// Panel DIP of a client point.
    fn dip(&self, lp: LPARAM) -> (f32, f32) {
        let m = (SHADOW_M * self.scale).ceil();
        let x = (lp.0 & 0xFFFF) as i16 as f32;
        let y = ((lp.0 >> 16) & 0xFFFF) as i16 as f32;
        ((x - m) / self.ds(), (y - m - (self.rect.top - self.room.top) as f32) / self.ds())
    }

    fn hit(&self, x: f32, y: f32) -> Option<Hit> {
        // The last drawn on top (Quit over its row).
        self.hits.iter().rev().find(|(r, _)| x >= r.left && x < r.right && y >= r.top && y < r.bottom).map(|(_, h)| *h)
    }

    /// Does what `h` stands for. What leaves the panel (Task Manager, an
    /// app shown or asked to quit) happens after it closes (it holds the
    /// mouse and the keyboard until then); an app is taken as it is now.
    fn press(&mut self, h: Hit, how: How) {
        if !self.reacts(h) {
            return;
        }
        self.after = match h {
            Hit::TaskManager => Some(After::TaskManager),
            Hit::App(_) => pressed_app(h, &self.view.list).map(After::Show),
            Hit::Quit => pressed_app(h, &self.view.list).map(After::Quit),
            Hit::Meter(r) => {
                self.picked = Lens::of(r);
                return self.refresh(how, false);
            }
            Hit::More => return self.fold(!self.details),
            Hit::Retry => return self.retry(),
            Hit::Info | Hit::Battery | Hit::Own => None,
        };
        self.done = self.after.is_some();
    }

    /// Opens or shuts the details: the plate glides, the card's ink
    /// crosses in (out faster), the mark turns.
    fn fold(&mut self, open: bool) {
        let now = sysstat::clock_ms();
        let on = |b: bool| if b { 1.0 } else { 0.0 };
        let was = std::mem::replace(&mut self.details, open);
        self.anim.go(Key::Open, on(was), on(open), GLIDE_MS, Ease::Glide, now);
        self.anim.go(Key::OpenInk, on(was), on(open), if open { CROSS_MS } else { FADE_OUT_MS }, if open { OUT } else { IN }, now);
        self.anim.go(Key::Turn, on(was), on(open), SNAP_MAX_MS, OUT, now);
        // Its lines are no stop while it is shut.
        if !open && matches!(self.focus, Some(Hit::Battery | Hit::Own)) {
            self.focus = Some(Hit::More);
        }
        self.sync(now);
        self.refresh_tip();
        self.publish();
    }

    /// "Try again": the sampler started over, and the wait with it.
    fn retry(&mut self) {
        set_sampling(false);
        set_sampling(true);
        (self.opened, self.seen, self.samples, self.last_seq, self.levels) = (sysstat::clock_ms(), false, 0, None, [None; 4]);
        self.snap = Snapshot::default();
        if matches!(self.focus, Some(Hit::Retry)) {
            self.focus = None;
        }
        unsafe {
            let _ = SetTimer(Some(self.hwnd), WAIT_TIMER, crate::motion::TIP_DELAY_MS as u32, None);
        }
        self.refresh(How::Auto, false);
        self.publish();
    }

    /// The wait's timer: the dashes after 300 ms, "Cannot measure" after
    /// five seconds; over once a number came.
    fn waited(&mut self) {
        self.refresh(How::Auto, false);
        let left = match self.view.phase {
            Phase::Blank | Phase::Waiting => Some(FAIL_MS.saturating_sub(sysstat::clock_ms().saturating_sub(self.opened)) + 20),
            Phase::Live | Phase::Failed => None,
        };
        unsafe {
            match left {
                Some(ms) => {
                    let _ = SetTimer(Some(self.hwnd), WAIT_TIMER, ms as u32, None);
                }
                None => {
                    let _ = KillTimer(Some(self.hwnd), WAIT_TIMER);
                }
            }
        }
        self.publish();
    }

    /// The keyboard focus to `h` (Tab, the arrows, a screen reader's
    /// SetFocus): its ring or cursor (`show`: it shows from now on;
    /// crate::input), its tooltip, the tree. On a bar's row the selection
    /// follows it, and the list with it (the disk's row takes the fill;
    /// the list stays).
    fn focus_on(&mut self, h: Option<Hit>, show: bool) {
        self.focus = h;
        self.keyed |= show;
        if let Some(Hit::Meter(r)) = h {
            if self.pickable(r) {
                self.picked = Lens::of(r);
            }
        }
        self.refresh(How::Keys, false);
        self.refresh_tip();
        self.publish();
    }

    /// The pointer is on `h` now: the fill under it (in over 90 ms, out
    /// over 120), and the tooltip of what explains. The keyboard's cursor
    /// lets go: the last input wins.
    fn point(&mut self, h: Option<Hit>) {
        if h == self.hover {
            return;
        }
        let now = sysstat::clock_ms();
        let was = std::mem::replace(&mut self.hover, h);
        if let Some(o) = was.filter(|o| self.reacts(*o)) {
            self.anim.go(Key::Hover(o), 1.0, 0.0, FADE_OUT_MS, IN, now);
        }
        if let Some(n) = h.filter(|n| self.reacts(*n)) {
            self.anim.go(Key::Hover(n), 0.0, 1.0, FADE_IN_MS, OUT, now);
        }
        if std::mem::replace(&mut self.keyed, false) {
            self.refresh(How::Pointer, false);
        } else {
            self.sync(now);
        }
        self.refresh_tip();
    }

    /// A key: Esc closes, Enter and Space press what has the focus, Tab
    /// and the arrows move it. The ring shows once the keyboard moves the
    /// focus (crate::input::navigates), not on any other key.
    fn on_key(&mut self, vk: u16) {
        let show = crate::input::navigates(vk);
        if vk == VK_ESCAPE.0 {
            self.done = true;
        } else if vk == VK_RETURN.0 || vk == VK_SPACE.0 {
            if show && !self.keyed && self.focus.is_some() {
                self.keyed = true;
                self.refresh(How::Keys, false);
            }
            self.press(self.focus.unwrap_or(Hit::TaskManager), How::Keys);
        } else if vk == VK_TAB.0 {
            let next = next_tab(self.shape(), self.focus, unsafe { GetKeyState(VK_SHIFT.0 as i32) } < 0, self.view.sel);
            self.focus_on(next, show);
        } else if vk == VK_DOWN.0 || vk == VK_UP.0 {
            let next = next_arrow(self.shape(), self.focus, vk == VK_UP.0, self.view.sel);
            self.focus_on(next, show);
        }
    }

    /// A button went down inside the panel at (`x`, `y`) DIP: the
    /// keyboard's ring and cursor go (the last input wins), the left one
    /// holds what it is on.
    fn on_down(&mut self, left: bool, x: f32, y: f32) {
        if std::mem::replace(&mut self.keyed, false) {
            self.refresh(How::Pointer, false);
            self.refresh_tip();
        }
        if left {
            let h = self.hit(x, y);
            self.hold(h);
        }
    }

    /// The left button went down on `h` (the fill at once; a button
    /// shrinks on the snap) or came up.
    fn hold(&mut self, h: Option<Hit>) {
        let now = sysstat::clock_ms();
        let held = h.filter(|h| self.reacts(*h));
        let was = std::mem::replace(&mut self.pressed, held);
        for (hit, to) in [(was, 0.0), (self.pressed, 1.0)] {
            if let Some(hit) = hit.filter(|x| matches!(x, Hit::Quit | Hit::Retry | Hit::TaskManager)) {
                self.anim.go(Key::Press(hit), 1.0 - to, to, SNAP_MAX_MS, OUT, now);
            }
        }
        if was != self.pressed {
            self.sync(now);
        }
    }

    /// The tooltip for what the pointer is on, or the keyboard moved to,
    /// with today's numbers (drawn again only when its text, the theme or
    /// its place changed).
    fn refresh_tip(&mut self) {
        let want = tip_target(self.hover, self.focus, self.keyed).and_then(|h| self.tip_text(h).map(|text| (h, text)));
        match want {
            Some((h, text)) => {
                let Some((r, _)) = self.hits.iter().find(|(_, x)| *x == h) else { return };
                let ds = self.ds();
                let at = POINT { x: self.rect.left + ((r.left + r.right) / 2.0 * ds) as i32, y: self.rect.top + (r.bottom * ds) as i32 + 4 };
                let shown = TipShown { text, theme: self.theme_gen, at: (at.x, at.y) };
                if self.tip.as_ref() == Some(&shown) {
                    return;
                }
                let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
                unsafe {
                    let _ = GetMonitorInfoW(MonitorFromPoint(at, MONITOR_DEFAULTTONEAREST), &mut mi);
                }
                super::desktops::show_tip_at(&self.gfx, &self.theme, self.scale, &shown.text, at, false, mi.rcMonitor);
                self.tip = Some(shown);
            }
            None => {
                if self.tip.take().is_some() {
                    super::desktops::hide_tip();
                }
            }
        }
    }
}

/// `a` then `b` (a point goes through `a` first).
fn times(a: Matrix3x2, b: Matrix3x2) -> Matrix3x2 {
    Matrix3x2 {
        M11: a.M11 * b.M11 + a.M12 * b.M21,
        M12: a.M11 * b.M12 + a.M12 * b.M22,
        M21: a.M21 * b.M11 + a.M22 * b.M21,
        M22: a.M21 * b.M12 + a.M22 * b.M22,
        M31: a.M31 * b.M11 + a.M32 * b.M21 + b.M31,
        M32: a.M31 * b.M12 + a.M32 * b.M22 + b.M32,
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        // A closed panel fading out lets the pointer through.
        WM_NCHITTEST if crate::motion::retired(hwnd) => LRESULT(HTTRANSPARENT as isize),
        WM_MOUSEMOVE => {
            with_panel(|p| {
                let (x, y) = p.dip(lp);
                let h = p.hit(x, y);
                p.point(h);
            });
            LRESULT(0)
        }
        WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN => {
            with_panel(|p| {
                let mut pt = POINT { x: (lp.0 & 0xFFFF) as i16 as i32, y: ((lp.0 >> 16) & 0xFFFF) as i16 as i32 };
                let _ = ClientToScreen(hwnd, &mut pt);
                if !contains(&p.rect, pt) {
                    // Outside (the icon included): close,
                    // click eaten. Which button, and where, for the tray
                    // icon's toggle (trayicon::toggle).
                    let button = match msg {
                        WM_LBUTTONDOWN => Button::Left,
                        WM_RBUTTONDOWN => Button::Right,
                        _ => Button::Middle,
                    };
                    p.done = true;
                    p.outside = Some(ClosingPress { button, at: pt });
                    return;
                }
                let (x, y) = p.dip(lp);
                p.on_down(msg == WM_LBUTTONDOWN, x, y);
            });
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            with_panel(|p| {
                let (x, y) = p.dip(lp);
                if let Some(h) = p.pressed {
                    let on_it = p.hit(x, y) == Some(h);
                    p.hold(None);
                    if on_it {
                        p.press(h, How::Pointer);
                    }
                }
            });
            LRESULT(0)
        }
        WM_KEYDOWN | WM_SYSKEYDOWN => {
            let vk = wp.0 as u16;
            with_panel(|p| p.on_key(vk));
            LRESULT(0)
        }
        WM_TIMER if wp.0 == FRAME_TIMER => {
            // A frame of what is easing (the timer ends with the last one).
            with_panel(|p| p.sync(sysstat::clock_ms()));
            LRESULT(0)
        }
        WM_TIMER if wp.0 == WAIT_TIMER => {
            with_panel(|p| p.waited());
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
                match (key.as_str(), act) {
                    // A button by its key (an app's by the app: never another
                    // one's after a refresh).
                    (k, Act::Invoke) => {
                        with_panel(|p| {
                            if let Some(h) = p.hit_of(k) {
                                p.press(h, How::Keys);
                            }
                        });
                    }
                    // Where a screen reader puts the focus is where Tab goes
                    // on from (the text alone cannot take it: a11y). The
                    // ring as it was, or with Windows' keyboard cues on.
                    (k, Act::Focus) => {
                        with_panel(|p| {
                            if let Some(h) = p.hit_of(k) {
                                let show = crate::input::reader_focus(p.keyed);
                                p.focus_on(Some(h), show);
                            }
                        });
                    }
                    (_, Act::Refresh) => {
                        with_panel(|p| p.publish());
                    }
                    _ => {}
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
#[path = "stats_harmony.rs"]
mod harmony;
#[cfg(test)]
pub(crate) use harmony::font_compare;

/// The panel's picture for the promo film (tests only).
#[cfg(test)]
#[path = "stats_promo.rs"]
mod promo;

#[cfg(test)]
mod tests {
    use super::*;

    /// The part of this file that ships (the tests' own text aside).
    fn body() -> &'static str {
        include_str!("stats.rs").split("#[cfg(test)]\nmod tests").next().unwrap_or("").split("#[cfg(test)]\r\nmod tests").next().unwrap()
    }

    /// The Task Manager button goes through the launch helper (its own
    /// thread): the panel never calls the shell on the UI thread.
    #[test]
    fn task_manager_opens_off_the_ui_thread() {
        let t = task_manager();
        assert_eq!((t.file.as_str(), t.verb.as_str(), t.args.as_str(), &t.source), ("taskmgr.exe", "open", "", &crate::launch::Source::TopBar));
        let src = include_str!("stats.rs");
        for call in [concat!("ShellExecute", "W("), concat!("ShellExecute", "ExW(")] {
            assert!(!src.contains(call), "no direct shell call in the panel: {call}");
        }
        assert!(src.contains(concat!("Some(After::TaskManager) => crate::launch::", "open_soon(task_manager())")));
    }

    #[test]
    fn the_hogs_tip_is_a_few_words() {
        // Owner, 2026-11-08: no sentence about CPU and GPU time.
        {
            let _l = crate::i18n::test_lock("ko");
            assert_eq!(t("topbar.stats.hogs.tip"), "최근 10분 추정치");
        }
        let _l = crate::i18n::test_lock("en");
        assert_eq!(t("topbar.stats.hogs.tip"), "Estimate for the last 10 minutes");
    }

    /// The state reads in a word (three at most), in every language.
    #[test]
    fn the_verdict_is_a_word() {
        {
            let _l = crate::i18n::test_lock("ko");
            assert_eq!([Level::Calm, Level::Busy, Level::VeryBusy].map(level_word), ["여유", "바쁨", "매우 바쁨"]);
            assert_eq!([Lens::Cpu, Lens::Memory, Lens::Gpu, Lens::Power].map(Lens::label), ["CPU", "메모리", "GPU", "전력"]);
        }
        let _l = crate::i18n::test_lock("en");
        assert_eq!([Level::Calm, Level::Busy, Level::VeryBusy].map(level_word), ["Calm", "Busy", "Very busy"]);
    }

    #[test]
    fn the_panel_takes_the_tokens() {
        // Every length on the 4 DIP grid (whole pixels at 100 to 200 %).
        for v in [W, PAD, GAP, INSET, ROW_PAD, CONTENT, HEAD_GAP, PIP, PIP_GAP, METER_H, NAME_MIN, NAME_MAX, VALUE_W, TRACK_H, PEAK_H, HEAD_H, APP_H, CULPRIT_H, ICON, INFO_GLYPH, BTN_H, BTN_PAD, WIDE_H, MORE_H, CHEVRON, DETAIL_H, FAIL_PAD, FAIL_GLYPH, FAIL_LINE] {
            assert!((v / 4.0).fract() == 0.0, "{v}");
        }
        // The two lines every panel keeps (design system 3.3): the cards'
        // edge at 8, the content at 20, on both sides.
        assert_eq!((PAD, CONTENT), (size::PANEL_PAD, 20.0));
        // A row is 28; 32 only where it holds a 28 button; a value's line 24.
        assert_eq!((METER_H, APP_H, CULPRIT_H, BTN_H, DETAIL_H, WIDE_H), (28.0, 28.0, 32.0, 28.0, 24.0, 32.0));
        let src = body();
        // No sparkline, no hairline, no tile, no pad of the old panel; and
        // no color, weight or press scale of its own.
        for gone in ["spark_", "hairline", "TILE_", "LEGACY_PAD", "SEP_H", "soft_shadow(", "DWRITE_FONT_WEIGHT_BOLD", ".wash(", "card_states", "fill_tile", "0.94", "high_contrast"] {
            assert!(!src.contains(gone), "{gone}");
        }
        // Fills, outlines and text come from the theme's one place; the
        // ring and the wells from the shared parts.
        for used in ["fill_of(Role::Row", "fill_of(Role::Raised", "outline_of(", "text_of(", "crate::gfx::focus_ring(", "crate::gfx::well(", "crate::motion::press_scale("] {
            assert!(src.contains(used), "{used}");
        }
        // Fades' lengths and curves are crate::motion's.
        assert!(src.contains("use crate::motion::{CROSS_MS, FADE_IN_MS, FADE_OUT_MS, SNAP_MAX_MS};") && src.contains("crate::motion::fade_plan("));
        assert!(HEAD_LINE >= ramp::TITLE.line);
    }

    #[test]
    fn secondary_text_reads_on_the_panel_and_its_cards() {
        use crate::syslook::SysLook;
        use crate::theme::{contrast, over};
        // Against the panel as text sees it (Theme::plate_seen: its top
        // light and grain at their bounds, every fallback level) over the
        // greys and the full colors, every finish (glass v3 review 3).
        use crate::theme::Finish;
        for light in [false, true] {
            for finish in [Finish::Flat, Finish::Glass] {
                let mut raw = Theme::graphite();
                raw.finish = finish;
                let t = raw.resolve_fx(&SysLook { light, ..Default::default() }, true);
                let dim = secondary_text(&t);
                for (panel, need) in t.plate_seen_all(crate::theme::Material::Panel) {
                    let card = over(t.fill_of(Role::Raised, State::Rest), panel);
                    for under in [panel, card, over(t.fill_of(Role::Raised, State::Hover), panel), over(t.fill_of(Role::Raised, State::Pressed), panel)] {
                        let c = contrast(over(dim, under), under);
                        assert!(c >= need, "light {light} {finish:?}: {c}");
                    }
                    // On a selected row (a fill over the card) names and
                    // values are drawn in the primary ink.
                    let row = over(t.fill_of(Role::Row, State::Selected), card);
                    assert!(contrast(over(t.text_of(Ink::Primary, State::Selected), row), row) >= need, "light {light} {finish:?}");
                }
            }
        }
    }

    /// A resource's history: `vals` a second apart, the last at `now`.
    fn hist(now: u64, vals: &[Option<f32>]) -> Spark {
        let mut h = Spark::new(sysstat::SPARK_SPAN_MS);
        let n = vals.len() as u64;
        for (i, v) in vals.iter().enumerate() {
            h.push(now - (n - 1 - i as u64) * 1_000, *v);
        }
        h
    }

    #[test]
    fn pressure_is_ten_seconds_average_of_what_was_read() {
        let now = 100_000;
        // Twenty readings: only the last ten seconds count (eleven of them).
        let mut v: Vec<Option<f32>> = vec![Some(0.0); 9];
        v.extend(vec![Some(88.0); 11]);
        assert_eq!(pressure(&hist(now, &v), now), Some(88.0));
        // A spike in a calm stretch does not carry the word.
        let mut spike = vec![Some(10.0); 10];
        spike.push(Some(100.0));
        assert!(pressure(&hist(now, &spike), now).unwrap() < 20.0);
        // Too few readings: not known (never a guess from one).
        assert_eq!(pressure(&hist(now, &[Some(95.0)]), now), None);
        assert!(pressure(&hist(now, &[Some(95.0), Some(97.0)]), now).is_some());
        assert_eq!(pressure(&Spark::new(sysstat::SPARK_SPAN_MS), now), None);
        // Unread ticks are not counted as zero.
        assert_eq!(pressure(&hist(now, &[Some(80.0), None, None, Some(90.0)]), now), Some(85.0));
        // The sampler rested (locked, display off): what was read before
        // is too old to speak for now.
        let mut h = hist(now - 30_000, &[Some(95.0); 10]);
        assert_eq!(pressure(&h, now), None);
        h.push(now - 1_000, Some(20.0));
        h.push(now, Some(30.0));
        assert_eq!(pressure(&h, now), Some(25.0));
    }

    #[test]
    fn levels_go_up_at_a_threshold_and_down_five_under_it() {
        use Level::*;
        // CPU: busy from 70, very busy from 90.
        assert_eq!([69.9, 70.0, 89.9, 90.0, 100.0].map(|p| level(Res::Cpu, p, Calm)), [Calm, Busy, Busy, VeryBusy, VeryBusy]);
        // Coming down: busy holds until under 65, very busy until under 85.
        assert_eq!([69.0, 65.0, 64.9].map(|p| level(Res::Cpu, p, Busy)), [Busy, Busy, Calm]);
        assert_eq!([89.0, 85.0, 84.9, 64.9].map(|p| level(Res::Cpu, p, VeryBusy)), [VeryBusy, VeryBusy, Busy, Calm]);
        // At the edge it does not go back and forth.
        let mut l = Calm;
        let seen: Vec<Level> = [69.0, 71.0, 69.0, 71.0, 66.0, 64.0, 69.0].into_iter().map(|p| {
            l = level(Res::Cpu, p, l);
            l
        }).collect();
        assert_eq!(seen, [Calm, Busy, Busy, Busy, Busy, Calm, Calm]);
        // Each resource its own numbers.
        assert_eq!((level(Res::Memory, 79.0, Calm), level(Res::Memory, 80.0, Calm), level(Res::Memory, 92.0, Calm)), (Calm, Busy, VeryBusy));
        assert_eq!((level(Res::Disk, 80.0, Calm), level(Res::Disk, 94.0, Calm), level(Res::Disk, 95.0, Calm)), (Busy, Busy, VeryBusy));
        // A GPU at 100 % is doing its job: busy, never very busy.
        assert_eq!((level(Res::Gpu, 84.0, Calm), level(Res::Gpu, 85.0, Calm), level(Res::Gpu, 100.0, Busy)), (Calm, Busy, Busy));
        assert_eq!(level(Res::Gpu, 80.0, Busy), Busy);
        // The eight numbers live in one place.
        assert_eq!(RES.map(thresholds), [(70.0, Some(90.0)), (80.0, Some(92.0)), (85.0, None), (80.0, Some(95.0))]);
    }

    #[test]
    fn the_verdict_is_the_most_pressed_resource() {
        use Level::*;
        let all = |cpu, mem, gpu, disk| verdict([cpu, mem, gpu, disk]);
        // Nothing known: no verdict (the dash).
        assert_eq!(all(None, None, None, None), None);
        // All calm: calm, nothing leads, nothing pressed.
        let v = all(Some(Calm), Some(Calm), Some(Calm), Some(Calm)).unwrap();
        assert_eq!((v.state, v.lead), (Calm, None));
        assert!(RES.iter().all(|r| !v.pressed(*r)));
        // The unknown are left out, not counted as calm or busy.
        let v = all(None, Some(Busy), None, None).unwrap();
        assert_eq!((v.state, v.lead), (Busy, Some(Res::Memory)));
        // Several pressed: all drawn bold; the highest leads.
        let v = all(Some(Busy), Some(VeryBusy), Some(Busy), Some(Calm)).unwrap();
        assert_eq!((v.state, v.lead), (VeryBusy, Some(Res::Memory)));
        assert_eq!(RES.map(|r| v.pressed(r)), [true, true, true, false]);
        // A tie: CPU, then memory, then the disk, then the GPU.
        assert_eq!(all(Some(Busy), Some(Busy), Some(Busy), Some(Busy)).unwrap().lead, Some(Res::Cpu));
        assert_eq!(all(Some(Calm), Some(Busy), Some(Busy), Some(Busy)).unwrap().lead, Some(Res::Memory));
        assert_eq!(all(Some(Calm), Some(Calm), Some(Busy), Some(Busy)).unwrap().lead, Some(Res::Disk));
        assert_eq!(all(Some(Calm), Some(Calm), Some(Busy), Some(Calm)).unwrap().lead, Some(Res::Gpu));
    }

    #[test]
    fn the_list_answers_for_what_is_pressed() {
        // The lead resource's lens.
        assert_eq!(lens(Some(Res::Memory), false, None), Lens::Memory);
        assert_eq!(lens(Some(Res::Gpu), true, None), Lens::Gpu, "pressure before power");
        // The disk has no list of its own: CPU, and no row is selected for
        // it (no false culprit).
        assert_eq!(lens(Some(Res::Disk), false, None), Lens::Cpu);
        assert_eq!(selected(Some(Res::Disk), None, None), None);
        assert!(!is_culprit(verdict([None, None, None, Some(Level::Busy)]).as_ref(), Lens::Cpu));
        // All calm: CPU plugged in, power on battery; no row selected (a
        // dark row in a calm panel reads as an alarm).
        assert_eq!((lens(None, false, None), lens(None, true, None)), (Lens::Cpu, Lens::Power));
        assert_eq!(selected(None, None, None), None);
        // Picked by the user: that one, whatever is pressed, with its fill.
        assert_eq!(lens(Some(Res::Memory), true, Some(Lens::Gpu)), Lens::Gpu);
        assert_eq!(selected(Some(Res::Memory), Some(Lens::Gpu), None), Some(Res::Gpu));
        assert_eq!(selected(None, Some(Lens::Cpu), None), Some(Res::Cpu));
        // The keyboard's cursor carries the fill, on the disk's row too.
        assert_eq!(selected(Some(Res::Memory), Some(Lens::Memory), Some(Res::Disk)), Some(Res::Disk));
        assert_eq!(selected(Some(Res::Memory), None, None), Some(Res::Memory));
        // The culprit: only while the list is ranked by the lead resource.
        let v = verdict([Some(Level::Calm), Some(Level::Busy), None, None]);
        assert!(is_culprit(v.as_ref(), Lens::Memory) && !is_culprit(v.as_ref(), Lens::Cpu) && !is_culprit(None, Lens::Cpu));
        assert!(!is_culprit(verdict([Some(Level::Calm); 4]).as_ref(), Lens::Cpu), "calm: nobody to blame");
        assert_eq!(RES.map(Lens::of), [Some(Lens::Cpu), Some(Lens::Memory), Some(Lens::Gpu), None]);
        assert_eq!(Lens::Power.res(), None);
    }

    fn app(key: &str, window: bool) -> Hog {
        Hog { key: key.into(), name: key.into(), share: 0.1, icon: None, procs: vec![(7, 1)], window }
    }

    fn row(key: &str, value: &str) -> Row {
        Row { hog: app(key, true), value: value.into() }
    }

    fn keys(rows: &[Row]) -> Vec<&str> {
        rows.iter().map(|r| r.hog.key.as_str()).collect()
    }

    #[test]
    fn rows_keep_their_places_between_rankings() {
        let shown: Vec<String> = ["a", "b", "c"].map(String::from).to_vec();
        // A second later b leads: not due, so the rows stay where they
        // are, with their new values.
        let fresh = vec![row("b", "40%"), row("a", "30%"), row("d", "20%"), row("c", "10%")];
        let kept = ranked(&shown, fresh.clone(), false, 3);
        assert_eq!(keys(&kept), ["a", "b", "c"]);
        assert_eq!(kept[0].value, "30%", "values still change every second");
        // Due (three seconds on): ranked again.
        assert_eq!(keys(&ranked(&shown, fresh.clone(), true, 3)), ["b", "a", "d"]);
        // A row that left the list makes room for the next, in rank order.
        let gone = vec![row("b", "40%"), row("d", "20%"), row("c", "10%")];
        assert_eq!(keys(&ranked(&shown, gone, false, 3)), ["b", "c", "d"]);
        // Nothing shown yet (or another lens): the ranking as it is.
        assert_eq!(keys(&ranked(&[], fresh, false, 3)), ["b", "a", "d"]);
        assert!(ranked(&shown, Vec::new(), false, 3).is_empty());
        assert_eq!(RANK_MS, 3_000);
    }

    fn list(lens: Lens, rows: Vec<Row>, culprit: bool, quit: bool) -> List {
        List { lens, rows, note: None, culprit, quit }
    }

    /// The app pressed is the app acted on: a refresh that puts another
    /// app in the row drops the target.
    #[test]
    fn a_press_stays_with_the_app_it_was_made_on() {
        let old = list(Lens::Memory, vec![row("a", "9 GB"), row("b", "3 GB")], true, true);
        let same = list(Lens::Memory, vec![row("a", "8 GB"), row("b", "3 GB")], true, true);
        let swapped = list(Lens::Memory, vec![row("b", "9 GB"), row("a", "3 GB")], true, true);
        assert!(same_target(Hit::Quit, &old, &same) && same_target(Hit::App(1), &old, &same), "the value changed: the same app");
        assert!(!same_target(Hit::Quit, &old, &swapped) && !same_target(Hit::App(0), &old, &swapped) && !same_target(Hit::App(1), &old, &swapped));
        // Still the same app, but Quit is no longer offered (the pressure
        // passed): the press is dropped.
        let calm = list(Lens::Memory, vec![row("a", "8 GB"), row("b", "3 GB")], false, false);
        assert!(!same_target(Hit::Quit, &old, &calm) && same_target(Hit::App(0), &old, &calm));
        // Gone, or its window is: dropped.
        assert!(!same_target(Hit::App(1), &old, &list(Lens::Memory, vec![row("a", "8 GB")], true, true)));
        let closed = list(Lens::Memory, vec![Row { hog: app("a", false), value: "8 GB".into() }], true, false);
        assert!(!same_target(Hit::App(0), &old, &closed));
        // Everything else is not a row's.
        for h in [Hit::TaskManager, Hit::More, Hit::Meter(Res::Cpu), Hit::Battery] {
            assert!(same_target(h, &old, &list(Lens::Cpu, Vec::new(), false, false)), "{h:?}");
        }
        // The key a screen reader invokes names the app, not the row.
        assert_eq!((key_of(Hit::Quit, &old.rows), key_of(Hit::App(1), &old.rows)), ("quit:a".into(), "app:b".into()));
        // What is done after closing takes the app as it was when pressed.
        let src = body();
        assert!(src.contains("Some(After::Show(h)) => crate::hogs::show(&h)") && src.contains("Some(After::Quit(h)) => crate::hogs::quit(&h)"));
    }

    /// A ranking puts B where A was: while the rows cross over (A's name
    /// still showing on the first frames), that row and its Quit cannot be
    /// pressed, and a press begun meanwhile is never carried out, not even
    /// once the crossing is over. Then a press takes the app shown.
    #[test]
    fn a_row_crossing_to_another_app_cannot_be_pressed_until_it_is_over() {
        let old = list(Lens::Memory, vec![row("a", "9 GB"), row("b", "3 GB")], true, true);
        let new = list(Lens::Memory, vec![row("b", "9 GB"), row("a", "3 GB")], true, true);
        // The new list's hits are made on the first frame: what they answer.
        for h in [Hit::Quit, Hit::App(0), Hit::App(1)] {
            assert!(!settled(h, Some(&old), &new, true), "crossing: {h:?}");
            assert!(settled(h, Some(&old), &new, false), "over: {h:?}");
            assert!(settled(h, None, &new, true), "nothing to cross from: {h:?}");
        }
        // A row whose app stayed (its value changed) answers throughout;
        // the one that changed waits.
        let kept = list(Lens::Memory, vec![row("a", "8 GB"), row("c", "1 GB")], true, true);
        assert!(settled(Hit::App(0), Some(&old), &kept, true) && settled(Hit::Quit, Some(&old), &kept, true));
        assert!(!settled(Hit::App(1), Some(&old), &kept, true));
        // A Quit that just came (the pressure did) fades in before it answers.
        let calm = list(Lens::Memory, vec![row("a", "8 GB"), row("b", "3 GB")], false, false);
        assert!(!settled(Hit::Quit, Some(&calm), &old, true) && settled(Hit::App(0), Some(&calm), &old, true));
        // Everything else answers as always.
        for h in [Hit::TaskManager, Hit::More, Hit::Retry, Hit::Meter(Res::Cpu)] {
            assert!(settled(h, Some(&old), &new, true), "{h:?}");
        }
        // Once over, the press is about the app shown in that place now.
        assert_eq!(pressed_app(Hit::Quit, &new).map(|h| h.key), Some("b".into()));
        assert_eq!(pressed_app(Hit::App(1), &new).map(|h| h.key), Some("a".into()));
        assert!(pressed_app(Hit::Quit, &calm).is_none(), "Quit not offered");
        let closed = list(Lens::Memory, vec![Row { hog: app("a", false), value: "8 GB".into() }], true, true);
        assert!(pressed_app(Hit::App(0), &closed).is_none() && pressed_app(Hit::Quit, &closed).is_none(), "no window");
        // Down → up → After: the button down is held only on what answers
        // (so an up after the crossing has nothing to carry out), the press
        // checks again, and what answers goes through settled. The new
        // Quit is drawn as a button that can be pressed only then.
        let src = body();
        assert!(src.contains("live && settled(h, self.old_list.as_ref(), &self.view.list, self.crossing(sysstat::clock_ms()))"));
        assert!(src.contains("let held = h.filter(|h| self.reacts(*h));"));
        assert!(src.contains("if let Some(h) = p.pressed {"));
        let press = src.split("fn press(&mut self, h: Hit, how: How) {").nth(1).unwrap();
        assert!(press.trim_start().starts_with("if !self.reacts(h) {"));
        assert!(press.contains("Hit::Quit => pressed_app(h, &self.view.list).map(After::Quit)"));
        assert!(src.contains("hits.as_deref_mut().filter(|_| self.reacts(Hit::Quit))"));
    }

    #[test]
    fn what_changed_in_the_list_crosses_over_and_the_rest_keeps_still() {
        let a = list(Lens::Cpu, vec![row("a", "4%"), row("b", "3%"), row("c", "1%")], false, false);
        // Values alone: nothing crosses.
        let b = list(Lens::Cpu, vec![row("a", "9%"), row("b", "3%"), row("c", "2%")], false, false);
        assert_eq!(list_changes(&a, &b), (false, [false; 3]));
        // Two rows swap: those two.
        let c = list(Lens::Cpu, vec![row("a", "9%"), row("c", "3%"), row("b", "2%")], false, false);
        assert_eq!(list_changes(&a, &c), (false, [false, true, true]));
        // Another lens: the heading and every row.
        let d = list(Lens::Memory, vec![row("a", "1 GB"), row("b", "1 GB"), row("c", "1 GB")], false, false);
        assert_eq!(list_changes(&a, &d), (true, [true; 3]));
        // The first row becomes the culprit (its weight, its button).
        let e = list(Lens::Cpu, a.rows.clone(), true, true);
        assert_eq!(list_changes(&a, &e), (false, [true, false, false]));
    }

    fn overlaps(a: &D2D_RECT_F, b: &D2D_RECT_F) -> bool {
        a.left < b.right && b.left < a.right && a.top < b.bottom && b.top < a.bottom
    }

    #[test]
    fn the_panel_is_as_tall_as_what_it_holds() {
        // Calm 380; the culprit's row 4 taller; the details 88 more (112
        // with a battery line).
        assert_eq!(layout(0.0, 0.0, false, false).height, 380.0);
        assert_eq!(layout(1.0, 0.0, false, false).height, 384.0);
        assert_eq!(layout(0.0, 1.0, false, false).height, 380.0 + 88.0);
        assert_eq!(layout(1.0, 1.0, true, false).height, 384.0 + 112.0);
        // A battery changes nothing while the details are shut.
        assert_eq!(layout(0.0, 0.0, true, false).height, 380.0);
        // The cards: the state 164, the apps 116 (120 with the culprit).
        let l = layout(0.0, 0.0, false, false);
        assert_eq!((l.state.bottom - l.state.top, l.apps.bottom - l.apps.top), (164.0, 116.0));
        let q = layout(1.0, 0.0, false, false);
        assert_eq!((q.apps.bottom - q.apps.top, q.rows[0].bottom - q.rows[0].top, q.rows[1].bottom - q.rows[1].top), (120.0, CULPRIT_H, APP_H));
        // Halfway through a glide everything below has moved half as far.
        let h = layout(0.5, 0.5, false, false);
        assert_eq!(h.height, 380.0 + 2.0 + 44.0);
        assert_eq!(h.details.map(|d| d.bottom - d.top), Some(40.0));
        assert!(l.details.is_none(), "shut: no card, and no gap for it");
        // "Cannot measure": its card and the button, nothing else.
        let f = layout(1.0, 1.0, true, true);
        let card = f.failed.unwrap();
        assert_eq!((card.top, card.bottom - card.top, f.button.top - card.bottom, f.height), (PAD, 124.0, GAP, 180.0));
    }

    #[test]
    fn everything_stands_on_the_two_lines_and_nothing_overlaps() {
        for (quit, open, battery) in [(0.0, 0.0, false), (1.0, 0.0, false), (0.0, 1.0, false), (1.0, 1.0, true), (0.0, 1.0, true)] {
            let l = layout(quit, open, battery, false);
            // The cards, "Details" and the button on the card line (8).
            let mut cards = vec![l.state, l.apps, l.more, l.button];
            cards.extend(l.details);
            for c in &cards {
                assert_eq!((c.left, c.right), (PAD, W - PAD), "{c:?}");
            }
            // Top to bottom, GAP apart, PAD from the plate's ends.
            let mut order = vec![l.state, l.apps, l.more];
            order.extend(l.details);
            order.push(l.button);
            assert_eq!(order[0].top, PAD);
            for w in order.windows(2) {
                assert_eq!(w[1].top - w[0].bottom, GAP, "{w:?}");
            }
            assert_eq!(l.button.bottom + PAD, l.height);
            // Rows inside their cards, INSET in; their content on the
            // content line (20) at both ends.
            let mut rows: Vec<D2D_RECT_F> = l.meters.to_vec();
            rows.push(l.head);
            rows.push(l.list_head);
            rows.extend(l.rows);
            rows.extend(l.detail_rows.iter().copied());
            for r in &rows {
                assert_eq!((r.left + ROW_PAD, r.right - ROW_PAD), (CONTENT, W - CONTENT), "{r:?}");
            }
            let all: Vec<D2D_RECT_F> = l.meters.iter().chain([&l.head, &l.list_head]).chain(&l.rows).chain(&l.detail_rows).copied().collect();
            for (i, a) in all.iter().enumerate() {
                for b in &all[i + 1..] {
                    assert!(!overlaps(a, b), "{a:?} {b:?}");
                }
            }
            for m in &l.meters {
                assert!(m.top >= l.state.top + INSET && m.bottom <= l.state.bottom - INSET);
            }
            for r in &l.rows {
                assert!(r.top >= l.apps.top + INSET && r.bottom <= l.apps.bottom - INSET);
            }
            if let Some(d) = l.details {
                assert_eq!(l.detail_rows.len(), if battery { 4 } else { 3 });
                assert!(l.detail_rows.iter().all(|r| r.top >= d.top + INSET && r.bottom <= d.bottom - INSET));
            }
            // "Details" starts its text on the content line too.
            assert_eq!(l.more.left + size::CARD_PAD, CONTENT);
            // Whole pixels at every scale.
            for s in [1.0f32, 1.25, 1.5, 1.75, 2.0] {
                for r in cards.iter().chain(&rows) {
                    for v in [r.left, r.top, r.right, r.bottom] {
                        assert!(((v * s) - (v * s).round()).abs() < 1e-3, "{v} at {s}");
                    }
                }
            }
        }
        // A bar's row: name, bar, value, apart; the value's end on the
        // content line; the three dots end there too.
        let l = layout(0.0, 0.0, false, false);
        let (name, bar, value) = meter_parts(l.meters[0], NAME_MIN);
        assert_eq!((name.0, value.1, value.1 - value.0), (CONTENT, W - CONTENT, VALUE_W));
        assert_eq!((bar.left - name.1, value.0 - bar.right, bar.bottom - bar.top), (GAP, GAP, TRACK_H));
        assert!(bar.right - bar.left > 150.0, "{bar:?}");
        let dots = pips(l.head);
        assert_eq!((dots[2].right, dots[1].left - dots[0].right, dots[0].right - dots[0].left), (W - CONTENT, PIP_GAP, PIP));
        // The name column: the widest name up to a multiple of 4, in bounds.
        assert_eq!([10.0, 52.0, 53.0, 70.5, 200.0].map(name_col), [52.0, 52.0, 56.0, 72.0, 96.0]);
        // An app's row: icon on the content line; Quit ends on it.
        let ir = app_icon_rect(l.rows[0]);
        assert_eq!((ir.left, ir.right - ir.left), (CONTENT, ICON));
        let b = row_button(layout(1.0, 0.0, false, false).rows[0], 60.0);
        assert_eq!((b.right, b.bottom - b.top, b.right - b.left), (W - CONTENT, BTN_H, 60.0));
    }

    #[test]
    fn right_to_left_mirrors_the_layout() {
        let l = layout(0.0, 0.0, false, false);
        let ir = mirrored(true, app_icon_rect(l.rows[0]));
        assert_eq!((ir.left, ir.right), (W - CONTENT - ICON, W - CONTENT), "a row starts at the right edge");
        assert_eq!(mirrored(false, l.state), l.state);
        assert_eq!(mirrored(true, l.state), l.state, "the cards span the plate either way");
        // A bar fills from the right: its start is the mirrored end.
        let (_, bar, _) = meter_parts(l.meters[0], NAME_MIN);
        let fill = mirrored(true, rect(bar.left, bar.top, bar.left + 40.0, bar.bottom));
        assert_eq!((fill.right, fill.right - fill.left), (W - bar.left, 40.0));
        // The dots lead from the other end; text too.
        assert_eq!(mirrored(true, pips(l.head)[2]).left, CONTENT);
        assert_eq!(text_x(Align::Start, CONTENT, W - CONTENT, 40.0, true), W - CONTENT - 40.0);
        assert_eq!(text_x(Align::End, CONTENT, W - CONTENT, 40.0, true), CONTENT);
        assert_eq!(text_x(Align::Mid, CONTENT, W - CONTENT, 40.0, true), text_x(Align::Mid, CONTENT, W - CONTENT, 40.0, false));
    }

    #[test]
    fn tab_goes_by_groups_and_arrows_inside_them() {
        let calm = Shape { apps: 3, ..Default::default() };
        assert_eq!(stops(calm), [Stop::Meters, Stop::Apps, Stop::More, Stop::TaskManager]);
        let busy = Shape { quit: true, apps: 3, details: true, battery: true, ..Default::default() };
        assert_eq!(stops(busy), [Stop::Meters, Stop::Quit, Stop::Apps, Stop::More, Stop::Battery, Stop::Own, Stop::TaskManager]);
        assert_eq!(stops(Shape { details: true, ..Default::default() }), [Stop::Meters, Stop::More, Stop::Own, Stop::TaskManager], "no rows, no battery");
        assert_eq!(stops(Shape { failed: true, quit: true, apps: 3, details: true, battery: true }), [Stop::Retry, Stop::TaskManager]);
        // Tab: from nowhere to the bars (the selected one), then on, round.
        assert_eq!(next_tab(busy, None, false, None), Some(Hit::Meter(Res::Cpu)));
        assert_eq!(next_tab(busy, None, false, Some(Res::Memory)), Some(Hit::Meter(Res::Memory)), "the group is entered at its selection");
        assert_eq!(next_tab(busy, Some(Hit::Meter(Res::Gpu)), false, None), Some(Hit::Quit));
        assert_eq!(next_tab(busy, Some(Hit::Quit), false, None), Some(Hit::App(0)));
        assert_eq!(next_tab(busy, Some(Hit::App(2)), false, None), Some(Hit::More));
        assert_eq!(next_tab(busy, Some(Hit::More), false, None), Some(Hit::Battery));
        assert_eq!(next_tab(busy, Some(Hit::Own), false, None), Some(Hit::TaskManager));
        assert_eq!(next_tab(busy, Some(Hit::TaskManager), false, None), Some(Hit::Meter(Res::Cpu)), "round");
        assert_eq!(next_tab(busy, None, true, None), Some(Hit::TaskManager));
        assert_eq!(next_tab(busy, Some(Hit::App(1)), true, None), Some(Hit::Quit));
        assert_eq!(next_tab(calm, Some(Hit::More), false, None), Some(Hit::TaskManager), "shut: its lines are no stop");
        // The arrows: the bars' rows (the disk's too), the apps' rows, round.
        assert_eq!(next_arrow(busy, Some(Hit::Meter(Res::Cpu)), false, None), Some(Hit::Meter(Res::Memory)));
        assert_eq!(next_arrow(busy, Some(Hit::Meter(Res::Gpu)), false, None), Some(Hit::Meter(Res::Disk)));
        assert_eq!(next_arrow(busy, Some(Hit::Meter(Res::Disk)), false, None), Some(Hit::Meter(Res::Cpu)));
        assert_eq!(next_arrow(busy, Some(Hit::Meter(Res::Cpu)), true, None), Some(Hit::Meter(Res::Disk)));
        assert_eq!(next_arrow(busy, Some(Hit::App(2)), false, None), Some(Hit::App(0)));
        assert_eq!(next_arrow(busy, Some(Hit::App(0)), true, None), Some(Hit::App(2)));
        // Anywhere else an arrow is Tab.
        assert_eq!(next_arrow(busy, Some(Hit::More), false, None), Some(Hit::Battery));
        assert_eq!(next_arrow(busy, None, false, None), Some(Hit::Meter(Res::Cpu)));
        assert_eq!(next_arrow(Shape { failed: true, ..Default::default() }, Some(Hit::Retry), false, None), Some(Hit::TaskManager));
        // The tooltip follows the keyboard until the pointer moves on.
        let (cpu, own) = (Some(Hit::Meter(Res::Cpu)), Some(Hit::Own));
        assert_eq!(tip_target(cpu, own, true), own, "the keyboard moved last");
        assert_eq!(tip_target(cpu, own, false), cpu, "the pointer moved last");
        assert_eq!(tip_target(None, own, false), None);
        // A screen reader's focus and invoke are handled (not dropped).
        let src = body();
        assert!(src.contains("(k, Act::Focus) =>") && src.contains("(k, Act::Invoke) =>"));
        assert_eq!(RES.map(|r| key_of(Hit::Meter(r), &[])), ["cpu", "memory", "gpu", "disk"]);
    }

    #[test]
    fn the_wait_shows_nothing_then_dashes_then_says_it_cannot() {
        assert_eq!([0, 299].map(|ms| phase(ms, false)), [Phase::Blank; 2], "nothing for 300 ms");
        assert_eq!([300, 4_999].map(|ms| phase(ms, false)), [Phase::Waiting; 2]);
        assert_eq!(phase(5_000, false), Phase::Failed);
        // A number came: live, however late, and for good.
        assert_eq!([0, 300, 60_000].map(|ms| phase(ms, true)), [Phase::Live; 3]);
        assert_eq!(pct_text(None), "\u{2013}", "one en dash wherever a value is not known");
        assert_eq!((pct_text(Some(26.4)), pct_text(Some(140.0))), ("26%".into(), "100%".into()));
    }

    #[test]
    fn reduced_motion_only_fades() {
        use crate::motion::{Mode, LINEAR, REDUCED_MS};
        // Nothing moves or resizes: a bar's fill, the peak, the plate's
        // height (the culprit's row, the details), the mark's turn, a
        // button's press all change at once.
        for k in [Key::Bar(Res::Cpu), Key::Peak(Res::Gpu), Key::Culprit, Key::Open, Key::Turn, Key::Press(Hit::Quit)] {
            assert!(k.moves(), "{k:?}");
            assert_eq!(plan(Mode::Reduced, 1.0, k, GLIDE_MS, Ease::Glide).0, 0, "{k:?}");
            assert_eq!(plan(Mode::Reduced, 1.0, k, CROSS_MS, OUT).0, 0, "{k:?}");
        }
        // Everything else is a 60 ms straight fade, whatever the speed.
        for k in [Key::Hot(Res::Cpu), Key::Known(Res::Disk), Key::Sel(Res::Memory), Key::Hover(Hit::More), Key::Word, Key::Pip(0), Key::ListIn, Key::ListOut, Key::OpenInk] {
            assert!(!k.moves(), "{k:?}");
            assert_eq!(plan(Mode::Reduced, 4.0, k, CROSS_MS, OUT), (REDUCED_MS as u64, Ease::Curve(LINEAR)), "{k:?}");
        }
        assert_eq!(plan(Mode::Reduced, 1.0, Key::Hover(Hit::More), FADE_IN_MS, OUT).0, 60);
        // Full motion: as designed.
        assert_eq!(plan(Mode::Full, 1.0, Key::Bar(Res::Cpu), CROSS_MS, OUT), (180, OUT));
        assert_eq!(plan(Mode::Full, 1.0, Key::Open, GLIDE_MS, Ease::Glide), (GLIDE_MS as u64, Ease::Glide));
        assert_eq!((CROSS_MS, FADE_IN_MS, FADE_OUT_MS), (180, 90, 120));
    }

    #[test]
    fn a_tween_runs_from_where_the_last_one_was() {
        let tw = Tween { from: 0.2, to: 0.8, start: 1_000, ms: 180, ease: OUT };
        assert_eq!((tw.at(1_000), tw.at(1_180), tw.at(9_999)), (0.2, 0.8, 0.8));
        assert!(tw.at(1_060) > 0.5, "decelerating: most of the way early");
        assert!(!tw.done(1_179) && tw.done(1_180));
        // The glide: a spring from rest, settled by its end.
        let g = Tween { from: 0.0, to: 1.0, start: 0, ms: GLIDE_MS as u64, ease: Ease::Glide };
        assert!(g.at(100) > 0.3 && g.at(100) < 0.95, "{}", g.at(100));
        assert!((g.at(GLIDE_MS as u64 - 1) - 1.0).abs() < 0.01);
        // At rest a value is what the state says.
        let mut a = Anim::default();
        assert_eq!(a.get(Key::Word, 5, 0.25), 0.25);
        assert!(!a.busy(5));
        a.map.insert(Key::Bar(Res::Cpu), tw);
        assert!(a.busy(1_100));
        assert!((a.get(Key::Bar(Res::Cpu), 1_180, 0.8) - 0.8).abs() < 1e-6, "done: at rest");
        a.sweep(2_000);
        assert!(a.map.is_empty() && !a.busy(2_000));
    }

    #[test]
    fn uptime_leaves_out_zero_units() {
        let ms = |d: u64, h: u64, m: u64| ((d * 24 + h) * 60 + m) * 60_000;
        {
            let _l = crate::i18n::test_lock("ko");
            assert_eq!(uptime_text(ms(0, 0, 35)), "35분");
            assert_eq!(uptime_text(ms(0, 2, 5)), "2시간 5분");
            assert_eq!(uptime_text(ms(0, 2, 0)), "2시간");
            assert_eq!(uptime_text(ms(3, 4, 59)), "3일 4시간", "minutes go once there are days");
            assert_eq!(uptime_text(ms(3, 0, 10)), "3일");
            assert_eq!(uptime_text(0), "0분");
        }
        let _l = crate::i18n::test_lock("en");
        assert_eq!(uptime_text(ms(0, 0, 35)), "35 min");
        assert_eq!(uptime_text(ms(0, 2, 5)), "2 hr 5 min");
        assert_eq!(uptime_text(ms(3, 4, 0)), "3 d 4 hr");
        assert_eq!(uptime_text(ms(1, 0, 0)), "1 d");
    }

    #[test]
    fn numbers_take_the_decimal_mark() {
        assert_eq!(dec1(2.75, ','), "2,8");
        assert_eq!(dec1(2.75, '.'), "2.8");
        assert_eq!(gb_pair(13 << 29, 47 << 30, '.'), "6.5 / 47.0 GB");
        assert_eq!(gb_pair(13 << 29, 47 << 30, ','), "6,5 / 47,0 GB");
        assert_eq!(mb_text(84 << 20, ','), "84 MB");
        assert_eq!(mb_text(1536 << 20, ','), "1,5 GB");
        assert_eq!(mb_text(999 << 20, '.'), "999 MB");
        assert_eq!(rate_text(2048.0, ','), "2,0 KB/s");
        assert_eq!(rate_text(3.0 * 1024.0 * 1024.0, '.'), "3.0 MB/s");
        // An app's CPU: whole from 10 on, a decimal under it.
        assert_eq!([71.4, 9.96, 9.94, 2.84, 0.01].map(|p| cpu_text(p, ',')), ["71%", "10%", "9,9%", "2,8%", "<0,1%"]);
        assert_eq!(share_text(0.123), "12%");
        assert_eq!(share_text(0.004), "<1%");
        assert_eq!(share_text(1.0), "100%");
    }

    /// The lists as rows: each lens its own value.
    #[test]
    fn each_lens_shows_its_own_measure() {
        let s = Snapshot { apps_cpu: vec![(app("a.exe", true), 18.2), (app("b.exe", false), 9.4)], apps_mem: vec![(app("b.exe", false), 9 << 30)], ..Default::default() };
        let (power, gpu) = ([Hog { share: 0.46, ..app("p.exe", true) }], [Hog { share: 0.61, ..app("g.exe", true) }]);
        let shown = |l: Lens| rows_of(l, &s, &power, &gpu, '.').into_iter().map(|r| (r.hog.key, r.value)).collect::<Vec<_>>();
        assert_eq!(shown(Lens::Cpu), [("a.exe".into(), "18%".into()), ("b.exe".into(), "9.4%".into())]);
        assert_eq!(shown(Lens::Memory), [("b.exe".into(), "9.0 GB".into())]);
        assert_eq!(shown(Lens::Gpu), [("g.exe".into(), "61%".into())]);
        assert_eq!(shown(Lens::Power), [("p.exe".into(), "46%".into())]);
    }

    #[test]
    fn details_go_to_tooltips_and_screen_readers() {
        let _l = crate::i18n::test_lock("en");
        // Four cores to a line.
        let tip = cores_tip(&[5.0, 3.0, 8.0, 2.0, 50.4]).unwrap();
        let lines: Vec<&str> = tip.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].starts_with("Core 1: 5%") && lines[0].contains("Core 4: 2%"), "{tip}");
        assert_eq!(lines[1], "Core 5: 50%");
        assert!(cores_tip(&[]).is_none());
        // A bar's minute: average and peak; the CPU's cores under it,
        // memory in GB instead, the disk's said to be active time.
        let now = 100_000;
        let mut s = Snapshot { history: hist(now, &[Some(10.0), Some(20.0), Some(30.0)]), gpu_history: hist(now, &[Some(8.0), Some(18.0)]), disk_history: hist(now, &[Some(4.0), Some(9.0)]), memory: Some((13 << 30, 32 << 30)), cores: vec![5.0, 3.0], at: now, ..Default::default() };
        assert_eq!(minute(&s.history, now), Some((20.0, 30.0)));
        assert_eq!(meter_tip(Res::Cpu, &s, now, '.').unwrap(), "Last minute: average 20% · peak 30%\nCore 1: 5%   Core 2: 3%");
        assert_eq!(meter_tip(Res::Memory, &s, now, '.').unwrap(), "13.0 / 32.0 GB");
        assert_eq!(meter_tip(Res::Gpu, &s, now, '.').unwrap(), "Last minute: average 13% · peak 18%");
        assert_eq!(meter_tip(Res::Disk, &s, now, '.').unwrap(), "Active time · Last minute: average 7% · peak 9%");
        assert_eq!(meter_tip(Res::Gpu, &Snapshot::default(), now, '.'), None);
        // NopBar: short in its line; in full in its tooltip (no GPU memory
        // where the adapter does not tell).
        assert_eq!(own_short(&s, '.'), "\u{2013}");
        assert_eq!(own_details(&s, None, true, None, '.'), ["CPU \u{2013}", "Memory \u{2013}", "GPU memory \u{2013}"]);
        s.own = Some(sysstat::Own { cpu: Some(0.437), memory: Some(29 << 20), procs: 2 });
        assert_eq!(own_short(&s, ','), "0,4% · 29 MB");
        assert_eq!(own_details(&s, None, true, Some(30 << 20), '.'), ["CPU 0.4%", "Memory 29 MB", "GPU memory 30 MB", "Processes: 2"]);
        assert_eq!(own_details(&s, None, false, None, '.'), ["CPU 0.4%", "Memory 29 MB", "Processes: 2"]);
        // Two readings in: the split into the dock, the top bar and the widgets.
        let d = own_details(&s, Some((0.3, 0.1, 0.0)), false, None, '.');
        assert_eq!(d[1], "Dock 0.3% · Top bar 0.1% · Widgets 0.0%");
        // The battery: its time left, when told.
        let mut p = sysstat::Power { battery: Some(80), on_ac: false, secs_left: Some(2 * 3600 + 10 * 60), saver: false };
        assert_eq!(battery_line(&p), "Battery  80% · About 2 hr 10 min left");
        p.secs_left = None;
        assert_eq!(battery_line(&p), "Battery  80%");
        assert_eq!(net_text(&Snapshot { net: Some((2048.0, 512.0)), ..Default::default() }, '.'), "\u{2193} 2.0 KB/s   \u{2191} 0.5 KB/s");
        assert_eq!(net_text(&Snapshot::default(), '.'), "\u{2013}");
        // The lines, in order; the battery's only with a battery.
        assert_eq!(details_of(false), [Detail::Network, Detail::Uptime, Detail::Own]);
        assert_eq!(details_of(true), [Detail::Network, Detail::Battery, Detail::Uptime, Detail::Own]);
    }

    /// A tooltip is drawn again when the theme changes, not only its text.
    #[test]
    fn a_retheme_draws_the_tooltip_again() {
        let shown = TipShown { text: "1.0 / 2.0 GB".into(), theme: 0, at: (10, 20) };
        assert_eq!(shown, TipShown { ..shown.clone() }, "the same: kept");
        assert_ne!(shown, TipShown { theme: 1, ..shown.clone() }, "retheme: drawn again");
        assert_ne!(shown, TipShown { at: (10, 30), ..shown.clone() });
    }

    /// High contrast: the bars in Highlight, the peak in the text color;
    /// on a selected row everything in HighlightText. Otherwise no color.
    #[test]
    fn bars_are_achromatic_and_follow_high_contrast() {
        use crate::syslook::{HighContrast, SysLook};
        let (white, black, cyan) = (Rgba { r: 255, g: 255, b: 255, a: 255 }, Rgba { r: 0, g: 0, b: 0, a: 255 }, Rgba { r: 0, g: 255, b: 255, a: 255 });
        for light in [false, true] {
            // Windows' accent (a blue here) and a chosen one never reach it.
            let mut raw = Theme::graphite();
            raw.accent = "#FFD60A".into();
            let t = raw.resolve_fx(&SysLook { light, ..Default::default() }, false);
            let (ease, hot, peak) = bar_colors(&t, false);
            for c in [ease, hot, peak] {
                assert!(c.r == c.g && c.g == c.b, "no color: {c:?}");
            }
            assert!(ease.a < hot.a, "at ease it is fainter than under pressure");
            assert_eq!(hot, on_fill(&t));
            assert_eq!(ease.a, (t.ink().a as f32 * BAR_INK).round() as u8);
        }
        let hc = Theme::graphite().resolve(&SysLook { high_contrast: Some(HighContrast { window: black, text: white, highlight: cyan, highlight_text: black, gray_text: crate::syslook::GRAY_TEXT }), ..Default::default() });
        assert_eq!(bar_colors(&hc, false), (cyan, cyan, white));
        assert_eq!(bar_colors(&hc, true), (black, black, black));
    }

    /// High contrast: a bar row's selected fill and its text flip together
    /// at once (design system 12.3), by the pointer, by a reading and as
    /// high contrast comes on mid-fade; never a gray between Window and
    /// Highlight under text that has already turned (or not yet).
    #[test]
    fn high_contrast_selection_flips_fill_and_text_together() {
        use crate::syslook::{HighContrast, SysLook};
        let (white, black) = (Rgba { r: 255, g: 255, b: 255, a: 255 }, Rgba { r: 0, g: 0, b: 0, a: 255 });
        // The review's colors: Window black, WindowText white, Highlight
        // white, HighlightText black.
        let hc = Theme::graphite().resolve(&SysLook { high_contrast: Some(HighContrast { window: black, text: white, highlight: white, highlight_text: black, gray_text: crate::syslook::GRAY_TEXT }), ..Default::default() });
        // No fade in high contrast, whatever moved it; outside it, as before.
        for how in [How::Pointer, How::Auto, How::Keys] {
            for is in [false, true] {
                assert_eq!(sel_fade(how, is, true), None, "{how:?} {is}");
            }
        }
        assert_eq!(sel_fade(How::Pointer, true, false), Some((FADE_IN_MS, OUT)));
        assert_eq!(sel_fade(How::Pointer, false, false), Some((FADE_OUT_MS, IN)));
        assert_eq!(sel_fade(How::Auto, true, false), Some((CROSS_MS, OUT)));
        assert_eq!(sel_fade(How::Keys, true, false), None);
        // Whatever a tween left (one running as high contrast came on), the
        // row draws the system's pair: Window under WindowText, or
        // Highlight under HighlightText.
        let window = hc.hc.unwrap().window;
        for i in 0..=100 {
            let sel = i as f32 / 100.0;
            let a = sel_fill(true, sel);
            let s = row_state(a, 0.0, false, false);
            let fill = hc.fill_of(Role::Row, State::Selected);
            let bg = if a >= 1.0 { fill } else if a <= 0.0 { window } else { panic!("a fill between: {a} at {sel}") };
            let pair = (bg, hc.text_of(Ink::Primary, s));
            assert!(pair == (white, black) || pair == (black, white), "{sel}: {pair:?}");
            assert_eq!(a == 1.0, sel > 0.5, "{sel}: flips at the half, with its text");
        }
        // Outside high contrast the fill still fades.
        assert_eq!(sel_fill(false, 0.49), 0.49);
        // High contrast coming on: a selection on its way ends where it is
        // going; nothing else is cut short.
        let mut anim = Anim::default();
        let tw = |from: f32, to: f32| Tween { from, to, start: 0, ms: 180, ease: OUT };
        anim.map.insert(Key::Sel(Res::Memory), tw(0.0, 1.0));
        anim.map.insert(Key::Sel(Res::Cpu), tw(1.0, 0.0));
        anim.map.insert(Key::ListIn, tw(0.0, 1.0));
        assert!(anim.get(Key::Sel(Res::Memory), 60, 1.0) < 1.0);
        anim.snap_fills();
        assert_eq!((anim.get(Key::Sel(Res::Memory), 60, 1.0), anim.get(Key::Sel(Res::Cpu), 60, 0.0)), (1.0, 0.0));
        assert!(anim.get(Key::ListIn, 60, 1.0) < 1.0);
        let src = body();
        assert!(src.contains("p.anim.snap_fills();") && src.contains("match sel_fade(how, is, self.theme.hc.is_some()) {") && src.contains("let sel = sel_fill(self.theme.hc.is_some(), sel);"));
    }

    #[test]
    fn a_press_outside_is_remembered_for_the_toggle() {
        assert_eq!(closed_by_press(500), None, "never closed");
        let middle = ClosingPress { button: Button::Middle, at: POINT { x: -5, y: 1050 } };
        PRESS_CLOSED.with(|c| c.set(Some((std::time::Instant::now(), middle))));
        assert_eq!(closed_by_press(500), Some(middle), "its button and place kept");
        PRESS_CLOSED.with(|c| c.set(Some((std::time::Instant::now() - std::time::Duration::from_secs(2), middle))));
        assert_eq!(closed_by_press(500), None, "long ago");
    }
}

#[cfg(test)]
mod rtl_place_tests {
    use super::*;

    #[test]
    fn the_panel_hangs_from_the_cells_outer_edge() {
        let work = RECT { left: 0, top: 30, right: 2000, bottom: 1200 };
        let button = RECT { left: 100, top: 0, right: 140, bottom: 30 };
        let (ltr, _) = place(button, work, 1.0, 100.0, false, false);
        let (rtl, _) = place(button, work, 1.0, 100.0, true, false);
        assert_eq!(rtl.left, 100, "RTL: starts at the cell's left edge");
        assert_eq!(ltr.left, 0, "LTR at the left edge: kept on screen");
        assert_eq!(ltr.top, 30 + crate::placement::popup_gap(1.0), "the popup gap under the button");
    }

    #[test]
    fn the_panel_hangs_the_popup_gap_off_the_bar_at_every_scale() {
        // The one rule for everything off the bar (placement::POPUP_GAP_DIP,
        // 6 DIP): under a bar on top, over one at the bottom.
        for (s, gap) in [(1.0f32, 6), (1.25, 8), (1.5, 9), (2.0, 12)] {
            let h = (28.0 * s).round() as i32;
            let cell = RECT { left: 2400, top: 0, right: 2440, bottom: h };
            let work = RECT { left: 0, top: h, right: 2560, bottom: 1440 };
            let (p, fit) = place(cell, work, s, 300.0, false, false);
            assert_eq!((p.top - cell.bottom, fit), (gap, 1.0), "top bar at {s}");
            let cell_b = RECT { top: 1440 - h, bottom: 1440, ..cell };
            let work_b = RECT { left: 0, top: 0, right: 2560, bottom: 1440 - h };
            let (p, _) = place(cell_b, work_b, s, 300.0, false, true);
            assert_eq!(cell_b.top - p.bottom, gap, "bottom bar at {s}");
        }
    }

    #[test]
    fn from_windows_tray_the_panel_keeps_the_gap_and_the_screen() {
        // A 1.5x monitor with a 48 px taskbar at the bottom; our icon near
        // the clock. The gap is POPUP_GAP_DIP at that scale (9 px).
        let work = RECT { left: 0, top: 0, right: 2880, bottom: 1752 };
        let icon = RECT { left: 2600, top: 1760, right: 2636, bottom: 1796 };
        let gap = crate::placement::popup_gap(1.5);
        assert_eq!(gap, 9);
        let (p, fit) = place(icon, work, 1.5, 400.0, false, true);
        assert_eq!((p.bottom, p.right, fit), (1752 - gap, 2636, 1.0), "over the taskbar's edge, the icon's right edge");
        assert_eq!(p.right - p.left, (W * 1.5) as i32, "as wide as the panel at that scale");
        // The icon at the very right edge: the panel stays on screen.
        let edge = RECT { left: 2860, top: 1760, right: 2900, bottom: 1796 };
        assert_eq!(place(edge, work, 1.5, 400.0, false, true).0.right, 2880);
        // Right to left (the tray on the left): from the icon's left edge,
        // and kept on screen at the left end too.
        let rtl_icon = RECT { left: 240, top: 1760, right: 276, bottom: 1796 };
        assert_eq!(place(rtl_icon, work, 1.5, 400.0, true, true).0.left, 240);
        let far_left = RECT { left: -10, top: 1760, right: 20, bottom: 1796 };
        assert_eq!(place(far_left, work, 1.5, 400.0, true, true).0.left, 0);
        // A monitor left of the primary: negative coordinates are fine.
        let left_work = RECT { left: -1920, top: 0, right: 0, bottom: 1032 };
        let li = RECT { left: -60, top: 1040, right: -36, bottom: 1072 };
        let (p, _) = place(li, left_work, 1.0, 400.0, false, true);
        assert_eq!((p.right, p.bottom), (-36, 1032 - 6));
        assert!(p.left >= -1920);
        // A taskbar at the top: under it.
        let top_work = RECT { left: 0, top: 48, right: 1920, bottom: 1080 };
        let ti = RECT { left: 1700, top: 8, right: 1724, bottom: 40 };
        assert_eq!(place(ti, top_work, 1.0, 400.0, false, false).0.top, 48 + 6);
    }

    #[test]
    fn over_a_bottom_bar_the_panel_opens_upward() {
        let work = RECT { left: 0, top: 0, right: 2000, bottom: 1170 };
        let button = RECT { left: 1800, top: 1170, right: 1840, bottom: 1200 };
        let (p, fit) = place(button, work, 1.0, 100.0, false, true);
        assert_eq!((p.bottom, p.top, p.right, fit), (1164, 1064, 1840, 1.0));
        let (p, _) = place(button, work, 1.0, 100.0, true, true);
        assert_eq!((p.bottom, p.right), (1164, 2000), "RTL: kept on screen");
        // Short screen: shrunk, never past the work area's bottom.
        let (p, fit) = place(button, RECT { bottom: 60, ..work }, 1.0, 100.0, false, true);
        assert!(fit < 1.0 && p.top >= 0 && p.bottom <= 60, "{p:?}");
        // Short, with the bar's own edge inside the work area: shrunk to
        // the room over the gap, which it keeps.
        let (p, fit) = place(button, RECT { bottom: 1200, ..work }, 1.0, 1300.0, false, true);
        assert!(fit < 1.0 && p.top >= 0, "{p:?}");
        assert_eq!(p.bottom, 1170 - 6);
        // The tallest panel (with the power list) fits a 1080p work area.
        let (_, fit) = place(RECT { left: 1800, top: 0, right: 1840, bottom: 30 }, RECT { left: 0, top: 30, right: 1920, bottom: 1040 }, 1.0, layout(1.0, 1.0, true, false).height, false, false);
        assert_eq!(fit, 1.0);
    }
}
