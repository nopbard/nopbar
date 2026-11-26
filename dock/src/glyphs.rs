//! NopBar's own icon set: one line language for every status and UI glyph
//! (top bar, Control Center, menus, panels, dock buttons), replacing the
//! system icon font glyphs whose sizes and weights varied.
//!
//! Drawing rules (design direction 2026-11-04):
//! - Every glyph is drawn on a 24 x 24 grid; most ink sits in 3..21.
//! - Outlines, stroke 1.5 at 24 px, scaled with the size (1 px at 16 px,
//!   3 px at 48 px), round caps and round joins. A few marks are filled
//!   (dots, media buttons, the battery level), in the same single color.
//! - One color, the caller's text / icon color (high contrast: the system
//!   text color the caller already uses). "Faint" parts (unlit Wi-Fi bars)
//!   are that color at FAINT alpha.
//! - Pixel snapping up to 40 px a side (a 16 DIP glyph up to 200 %): the
//!   stroke is a whole number of pixels (at least 1) and the grid's center
//!   lands on a pixel center (odd stroke) or a pixel corner (even), so
//!   symmetric glyphs stay crisp at every display scale. Larger sizes keep
//!   the exact stroke and put the box on whole pixels.
//! - A translucent color is drawn opaque in a layer at its alpha: parts
//!   that meet do not show their overlap darker.
//!
//! The shapes are plain data (SVG-like path strings on the grid), so the
//! same geometry is drawn by Direct2D (`draw`) and by a small software
//! rasterizer (`raster_into`) for icons made off the UI thread (the dock's
//! trash, folder stacks and Launchpad pin), and is testable without a GPU.

use windows::core::Result;
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_BEZIER_SEGMENT, D2D1_COLOR_F, D2D1_FIGURE_BEGIN_FILLED, D2D1_FIGURE_BEGIN_HOLLOW, D2D1_FIGURE_END_CLOSED, D2D1_FIGURE_END_OPEN,
    D2D1_FILL_MODE_WINDING, D2D_SIZE_F,
};
use windows::Win32::Graphics::Direct2D::{
    ID2D1Factory, ID2D1PathGeometry, ID2D1RenderTarget, D2D1_ARC_SEGMENT, D2D1_ARC_SIZE_LARGE, D2D1_ARC_SIZE_SMALL, D2D1_CAP_STYLE_ROUND, D2D1_DASH_STYLE_SOLID,
    D2D1_LINE_JOIN_ROUND, D2D1_STROKE_STYLE_PROPERTIES, D2D1_SWEEP_DIRECTION_CLOCKWISE, D2D1_SWEEP_DIRECTION_COUNTER_CLOCKWISE,
};
use windows_numerics::{Matrix3x2, Vector2};

/// The design grid's side.
pub const GRID: f32 = 24.0;
/// Stroke width on the grid (at 24 px).
pub const STROKE: f32 = 1.5;
/// At this many px a side or fewer the stroke and center are snapped (a 16
/// DIP glyph is 19.2 px at 100 %, 38.4 px at 200 %).
pub const SNAP_MAX_PX: f32 = 40.0;
/// Alpha of faint parts (unlit bars), times the color's own alpha.
pub const FAINT: f32 = 0.32;
/// The same in high contrast: still told apart, but readable.
pub const FAINT_HC: f32 = 0.6;

/// The whole set; some are not shown anywhere yet (kept so new call sites
/// draw from the same language).
#[allow(dead_code)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Glyph {
    /// Wi-Fi with 0..=4 lit parts (dot, then the arcs inside out).
    Wifi(u8),
    WifiOff,
    /// Connected, no internet.
    NetLimited,
    Ethernet,
    Globe,
    Bluetooth,
    BluetoothOff,
    VolumeMute,
    /// Speaker with 0..=3 waves.
    Volume(u8),
    /// Battery filled 0..=4 quarters.
    Battery(u8),
    BatteryCharging,
    BatterySaver,
    Bolt,
    Moon,
    Sun,
    Cast,
    Monitor,
    Speaker,
    Headphones,
    Headset,
    Microphone,
    Prev,
    Play,
    Pause,
    Next,
    Stop,
    Music,
    /// Control Center: three sliders.
    Sliders,
    Search,
    /// Desktops: four tiles.
    Desktops,
    Keyboard,
    Mouse,
    Image,
    ChevronLeft,
    ChevronRight,
    ChevronUp,
    ChevronDown,
    Close,
    Check,
    Plus,
    Minus,
    Info,
    /// Something went wrong (the quiet states' one mark: a triangle with
    /// an exclamation mark; never a color).
    Warning,
    Gear,
    Folder,
    File,
    Document,
    Download,
    Copy,
    Save,
    Edit,
    Camera,
    Timer,
    Pin,
    Pinned,
    Restore,
    PictureInPicture,
    /// Back to the app's own window.
    BackToApp,
    /// A generic app window.
    Window,
    /// A tray with an arrow into it: windows hidden to the tray
    /// (crate::traywin).
    Tray,
    Lock,
    Restart,
    Power,
    SignOut,
    Calculator,
    Trash,
    TrashFull,
    /// The Launchpad: eight tiles, 3-3-2.
    Launchpad,
    /// NopBar's gnome mark (assets/brand/mark.svg), filled: the dock's
    /// Launchpad pin.
    Mark,
    /// Launchpad Library categories (the design kit): a terminal prompt, a
    /// game controller, an eye struck through (hidden apps).
    Terminal,
    Gamepad,
    EyeOff,
    /// A screen with the dock along its bottom (Settings).
    Dock,
    /// A screen with the bar along its top (Settings).
    TopBar,
    /// A circle half filled: light and dark, the look (Settings).
    Appearance,
    /// An arrow turning back: put the default back.
    Undo,
    /// Control Center quick tiles: airplane mode (a plane seen from above),
    /// night light (the sun setting), energy saver (a leaf), Do Not
    /// Disturb (a bell struck through), mobile hotspot (a mast sending both
    /// ways), Project (a second screen in front of the first).
    Airplane,
    NightLight,
    Leaf,
    GnomeHat,
    BellOff,
    Bell,
    Hotspot,
    Project,
    /// Opens elsewhere (a Windows settings page): a box with an arrow out.
    External,
    /// Control Center button with removable storage connected: the
    /// sliders and a small dot in the corner.
    SlidersDot,
    /// Keep Awake (a cup with steam over it).
    Cup,
    /// Safely remove: a triangle over a bar.
    Eject,
    /// Removable storage: a USB stick, an SD card, a hot-plug disk, a disc.
    UsbDrive,
    SdCard,
    Drive,
    Disc,
    /// Settings' sidebar: effects (a sparkle), the clock and calendar page
    /// (a month sheet), hot corners (a screen's four corners).
    Sparkle,
    Calendar,
    Corners,
    ChatBubble,
    FingerTap,
    Accessibility,
    FanMenu,
    MoveArrows,
    /// About this PC's facts: a processor (a chip with its pins), a
    /// graphics card (a board with its fan), memory (a module).
    Processor,
    Graphics,
    Memory,
}

impl Glyph {
    /// Every glyph (each level of the leveled ones).
    #[cfg(test)]
    pub fn all() -> Vec<Glyph> {
        use Glyph::*;
        let mut v: Vec<Glyph> = (0..=4).map(Wifi).collect();
        v.extend((0..=3).map(Volume));
        v.extend((0..=4).map(Battery));
        v.extend([
            WifiOff, NetLimited, Ethernet, Globe, Bluetooth, BluetoothOff, VolumeMute, BatteryCharging, BatterySaver, Bolt, Moon, Sun, Cast,
            Monitor, Speaker, Headphones, Headset, Microphone, Prev, Play, Pause, Next, Stop, Music, Sliders, Search,
            Desktops, Keyboard, Mouse, Image, ChevronLeft, ChevronRight, ChevronUp, ChevronDown, Close, Check, Plus, Minus, Info, Warning, Gear,
            Folder, File, Document, Download, Copy, Save, Edit, Camera, Timer, Pin, Pinned, Restore, PictureInPicture, BackToApp, Window,
            Lock, Restart, Power, SignOut, Calculator, Trash, TrashFull, Launchpad, Mark, Terminal, Gamepad, EyeOff, Dock, TopBar, Appearance, Undo, Tray, Airplane, NightLight, Leaf, GnomeHat,
            Bell, BellOff, Hotspot, Project, External, SlidersDot, Cup, Eject, UsbDrive, SdCard, Drive, Disc, Sparkle, Calendar, Corners, ChatBubble, FingerTap, Accessibility, FanMenu, MoveArrows, Processor, Graphics, Memory,
        ]);
        v
    }
}

// ---- shapes -------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Paint {
    Stroke,
    Fill,
    /// Filled and stroked (rounded corners on solid marks).
    Both,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Part {
    /// SVG path data on the grid (M L H V C A Z, absolute or relative).
    pub d: String,
    pub paint: Paint,
    pub faint: bool,
}

fn part(d: impl Into<String>, paint: Paint) -> Part {
    Part { d: d.into(), paint, faint: false }
}

fn s(d: impl Into<String>) -> Part {
    part(d, Paint::Stroke)
}

fn f(d: impl Into<String>) -> Part {
    part(d, Paint::Fill)
}

fn both(d: impl Into<String>) -> Part {
    part(d, Paint::Both)
}

fn faint(mut p: Part, yes: bool) -> Part {
    p.faint = yes;
    p
}

/// A circle as path data.
fn circle(cx: f32, cy: f32, r: f32) -> String {
    format!("M{} {}A{r} {r} 0 0 1 {} {}A{r} {r} 0 0 1 {} {}Z", cx + r, cy, cx - r, cy, cx + r, cy)
}

/// A rounded rectangle as path data.
fn rrect(l: f32, t: f32, r: f32, b: f32, rad: f32) -> String {
    let k = rad.min((r - l) / 2.0).min((b - t) / 2.0).max(0.0);
    if k == 0.0 {
        return format!("M{l} {t}H{r}V{b}H{l}Z");
    }
    format!(
        "M{} {t}H{}A{k} {k} 0 0 1 {r} {}V{}A{k} {k} 0 0 1 {} {b}H{}A{k} {k} 0 0 1 {l} {}V{}A{k} {k} 0 0 1 {} {t}Z",
        l + k,
        r - k,
        t + k,
        b - k,
        r - k,
        l + k,
        b - k,
        t + k,
        l + k
    )
}

/// An arc around (cx, cy) from angle a0 to a1 (degrees, y down: clockwise
/// when a1 > a0) as path data starting with a move.
fn arc(cx: f32, cy: f32, r: f32, a0: f32, a1: f32) -> String {
    let (s0, c0) = a0.to_radians().sin_cos();
    let (s1, c1) = a1.to_radians().sin_cos();
    let large = ((a1 - a0).abs() > 180.0) as u8;
    let sweep = (a1 > a0) as u8;
    format!("M{} {}A{r} {r} 0 {large} {sweep} {} {}", cx + r * c0, cy + r * s0, cx + r * c1, cy + r * s1)
}

/// A line from the angle `a` (degrees) at r0 out to r1 around (cx, cy).
fn ray(cx: f32, cy: f32, a: f32, r0: f32, r1: f32) -> String {
    let (sn, cs) = a.to_radians().sin_cos();
    format!("M{} {}L{} {}", cx + r0 * cs, cy + r0 * sn, cx + r1 * cs, cy + r1 * sn)
}

const SPEAKER: &str = "M11 5L6.5 9H4.5a1 1 0 0 0-1 1v4a1 1 0 0 0 1 1h2L11 19Z";
const BATTERY_NUB: &str = "M21 10.5v3";
const TRASH_BODY: &str = "M6.5 6.5V19a2 2 0 0 0 2 2h7a2 2 0 0 0 2-2V6.5";
const HEADPHONE_BAND: &str = "M3.5 16V12.5a8.5 8.5 0 0 1 17 0V16";

/// The screen Dock and TopBar are drawn on.
fn screen() -> String {
    rrect(3.0, 4.5, 21.0, 19.5, 2.0)
}

fn battery_body() -> String {
    rrect(3.0, 7.0, 19.0, 17.0, 2.5)
}

fn wifi(lit: u8, dot: bool) -> Vec<Part> {
    let mut v = Vec::new();
    if dot {
        v.push(faint(f(circle(12.0, 18.0, 1.25)), lit < 1));
    }
    for (k, r) in [4.5f32, 8.5, 12.5].into_iter().enumerate() {
        v.push(faint(s(arc(12.0, 18.0, r, -135.0, -45.0)), lit < k as u8 + 2));
    }
    v
}

fn gear() -> String {
    let (c, outer, root) = (12.0f32, 9.0f32, 6.75f32);
    let mut d = String::new();
    for i in 0..8 {
        let th = i as f32 * 45.0 - 90.0;
        for (k, (a, r)) in [(th - 15.0, root), (th - 8.5, outer), (th + 8.5, outer), (th + 15.0, root)].into_iter().enumerate() {
            let (sn, cs) = a.to_radians().sin_cos();
            let cmd = if i == 0 && k == 0 { 'M' } else { 'L' };
            d.push_str(&format!("{cmd}{} {}", c + r * cs, c + r * sn));
        }
    }
    d.push('Z');
    d
}

/// The parts of `g`, on the grid.
pub fn parts(g: Glyph) -> Vec<Part> {
    use Glyph::*;
    match g {
        Wifi(n) => wifi(n.min(4), true),
        WifiOff => {
            let mut v: Vec<Part> = wifi(0, true);
            v.push(s("M4 3.5L20 19.5"));
            v
        }
        NetLimited => {
            let mut v = wifi(0, false);
            v.push(s("M12 9.5v4.5"));
            v.push(f(circle(12.0, 17.75, 1.1)));
            v
        }
        Ethernet => vec![
            s(rrect(9.5, 3.0, 14.5, 8.0, 1.25)),
            s(rrect(3.0, 16.0, 8.0, 21.0, 1.25)),
            s(rrect(16.0, 16.0, 21.0, 21.0, 1.25)),
            s("M12 8v4M5.5 16v-4h13v4"),
        ],
        Globe => vec![s(circle(12.0, 12.0, 9.0)), s("M3 12h18"), s("M12 3a12 12 0 0 0 0 18a12 12 0 0 0 0-18Z")],
        Bluetooth => vec![s("M7.5 7.5l9 9L12 21V3l4.5 4.5-9 9")],
        BluetoothOff => vec![s("M16.5 16.5L12 21v-9l-4.5 4.5"), s("M14.25 9.75L16.5 7.5 12 3v4.5"), s("M4 4l16 16")],
        VolumeMute => vec![s(SPEAKER), s("M15.5 9.5l5 5M20.5 9.5l-5 5")],
        Volume(n) => {
            let mut v = vec![s(SPEAKER)];
            for r in [3.25f32, 6.0, 8.75].into_iter().take(n.min(3) as usize) {
                v.push(s(arc(12.0, 12.0, r, -45.0, 45.0)));
            }
            v
        }
        Battery(n) => {
            let mut v = vec![s(battery_body()), s(BATTERY_NUB)];
            let n = n.min(4);
            if n > 0 {
                v.push(f(rrect(5.0, 9.5, 5.0 + 12.0 * n as f32 / 4.0, 14.5, 1.0)));
            }
            v
        }
        BatteryCharging => vec![s(battery_body()), s(BATTERY_NUB), both("M11.75 9L9 12.4h2.6L10.75 15l3.25-3.4h-2.6Z")],
        BatterySaver => vec![s(battery_body()), s(BATTERY_NUB), s("M7.5 14.5C7.5 11.5 10 9.5 14.5 9.5C14.5 12.75 12.25 14.5 9 14.5Z"), s("M7.5 14.5l3.5-2.75")],
        Bolt => vec![both("M13.5 3L5.5 13.5H12L10.5 21l8-10.5H12Z")],
        Moon => vec![s("M12 3.9A5.4 5.4 0 0 0 20.1 12A8.1 8.1 0 1 1 12 3.9Z")],
        Sun => {
            let mut v = vec![s(circle(12.0, 12.0, 3.75))];
            for i in 0..8 {
                v.push(s(ray(12.0, 12.0, i as f32 * 45.0, 6.75, 8.75)));
            }
            v
        }
        Cast => vec![s("M3 9V7a2 2 0 0 1 2-2h14a2 2 0 0 1 2 2v10a2 2 0 0 1-2 2h-5"), s("M3 12.5a6.5 6.5 0 0 1 6.5 6.5"), s("M3 16a3 3 0 0 1 3 3"), f(circle(3.75, 18.25, 1.0))],
        Monitor => vec![s(rrect(3.0, 4.0, 21.0, 16.0, 2.0)), s("M12 16v4M8 20h8")],
        Speaker => vec![s(rrect(5.5, 3.0, 18.5, 21.0, 2.5)), s(circle(12.0, 14.5, 3.5)), f(circle(12.0, 7.5, 1.0))],
        Headphones => vec![s(HEADPHONE_BAND), s(rrect(3.5, 13.0, 8.0, 20.5, 1.75)), s(rrect(16.0, 13.0, 20.5, 20.5, 1.75))],
        Headset => vec![
            s(HEADPHONE_BAND),
            s(rrect(3.5, 12.5, 8.0, 18.0, 1.75)),
            s(rrect(16.0, 12.5, 20.5, 18.0, 1.75)),
            s("M18.25 18v.5a2.5 2.5 0 0 1-2.5 2.5H13"),
        ],
        Microphone => vec![s(rrect(9.0, 3.0, 15.0, 14.0, 3.0)), s("M18.5 11a6.5 6.5 0 0 1-13 0"), s("M12 17.5V21")],
        Prev => vec![both("M18 6.5v11l-8.5-5.5Z"), f(rrect(5.5, 6.0, 7.5, 18.0, 1.0))],
        Next => vec![both("M6 6.5v11l8.5-5.5Z"), f(rrect(16.5, 6.0, 18.5, 18.0, 1.0))],
        Play => vec![both("M8.5 5.5v13l10-6.5Z")],
        Pause => vec![f(rrect(6.5, 5.5, 10.0, 18.5, 1.25)), f(rrect(14.0, 5.5, 17.5, 18.5, 1.25))],
        Stop => vec![f(rrect(6.5, 6.5, 17.5, 17.5, 2.0))],
        Music => vec![s("M9 18V5.5l11-2V16"), s(circle(6.5, 18.0, 2.5)), s(circle(17.5, 16.0, 2.5))],
        Sliders => vec![
            s("M3.5 6H12.75M17.25 6h3.25"),
            s(circle(15.0, 6.0, 2.25)),
            s("M3.5 12H6.75M11.25 12h9.25"),
            s(circle(9.0, 12.0, 2.25)),
            s("M3.5 18H10.75M15.25 18h5.25"),
            s(circle(13.0, 18.0, 2.25)),
        ],
        Search => vec![s(circle(10.5, 10.5, 6.5)), s("M15.25 15.25L20.5 20.5")],
        Desktops => vec![
            s(rrect(4.0, 4.0, 11.0, 11.0, 1.5)),
            s(rrect(13.0, 4.0, 20.0, 11.0, 1.5)),
            s(rrect(4.0, 13.0, 11.0, 20.0, 1.5)),
            s(rrect(13.0, 13.0, 20.0, 20.0, 1.5)),
        ],
        Keyboard => {
            let mut v = vec![s(rrect(3.0, 6.0, 21.0, 18.0, 2.0)), s("M8 14.5h8")];
            for x in [7.0f32, 10.33, 13.67, 17.0] {
                v.push(f(circle(x, 10.25, 0.9)));
            }
            v
        }
        Mouse => vec![s(rrect(6.0, 3.0, 18.0, 21.0, 6.0)), s("M12 6.5v4")],
        Image => vec![s(rrect(3.0, 4.0, 21.0, 20.0, 2.0)), s(circle(9.0, 9.5, 1.75)), s("M21 15l-4.5-4.5L6 20")],
        ChevronLeft => vec![s("M14.5 6l-6 6 6 6")],
        ChevronRight => vec![s("M9.5 6l6 6-6 6")],
        ChevronUp => vec![s("M6 14.5l6-6 6 6")],
        ChevronDown => vec![s("M6 9.5l6 6 6-6")],
        Close => vec![s("M6.5 6.5l11 11M17.5 6.5l-11 11")],
        Check => vec![s("M5 12.5l4.5 4.5L19 7.5")],
        Plus => vec![s("M12 5.5v13M5.5 12h13")],
        Minus => vec![s("M5.5 12h13")],
        Info => vec![s(circle(12.0, 12.0, 9.0)), s("M12 11v5.5"), f(circle(12.0, 7.75, 1.1))],
        Warning => vec![s("M12 4.25L21 19.5H3Z"), s("M12 10v4.5"), f(circle(12.0, 16.9, 1.0))],
        Gear => vec![s(gear()), s(circle(12.0, 12.0, 3.0))],
        Folder => vec![s("M3 7a2 2 0 0 1 2-2h4l2 2.5h8a2 2 0 0 1 2 2V17a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2Z")],
        File => vec![s("M13.5 3H7a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2V8.5Z"), s("M13.5 3v5.5H19")],
        Document => vec![s("M13.5 3H7a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2V8.5Z"), s("M13.5 3v5.5H19"), s("M8.5 13h7M8.5 16.5h5")],
        Download => vec![s("M12 3.5v11"), s("M7.5 10l4.5 4.5 4.5-4.5"), s("M5 19.5h14")],
        Copy => vec![s(rrect(8.5, 8.5, 20.0, 20.0, 2.0)), s("M15.5 8.5V6a2 2 0 0 0-2-2H6a2 2 0 0 0-2 2v7.5a2 2 0 0 0 2 2h2.5")],
        Save => vec![s("M17.5 20.5h-11a2 2 0 0 1-2-2v-13a2 2 0 0 1 2-2H15.5l4 4v11a2 2 0 0 1-2 2Z"), s("M16.5 20.5v-6h-9v6"), s("M7.5 3.5v4h6")],
        Edit => vec![s("M4.5 19.5h3.5L18.5 9a2.47 2.47 0 0 0-3.5-3.5L4.5 16Z"), s("M13.5 7l3.5 3.5")],
        Camera => vec![s("M3 8.5a2 2 0 0 1 2-2h2.5L9 4.5h6l1.5 2H19a2 2 0 0 1 2 2V18a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2Z"), s(circle(12.0, 13.0, 3.5))],
        Timer => vec![s(circle(12.0, 13.5, 7.5)), s("M12 13.5V10"), s("M10 3h4M12 3v3")],
        Pin => vec![s("M9 3.5h6"), s("M10 3.5v5L7 12.5V14h10v-1.5l-3-4v-5"), s("M12 14v6.5")],
        Pinned => vec![s("M9 3.5h6"), both("M10 3.5v5L7 12.5V14h10v-1.5l-3-4v-5Z"), s("M12 14v6.5")],
        Restore => vec![s(rrect(4.5, 8.5, 15.5, 19.5, 1.5)), s("M8.5 8.5V6.5a2 2 0 0 1 2-2h7a2 2 0 0 1 2 2v7a2 2 0 0 1-2 2h-2")],
        PictureInPicture => vec![s("M21 10.5V7a2 2 0 0 0-2-2H5a2 2 0 0 0-2 2v10a2 2 0 0 0 2 2h4.5"), s(rrect(13.0, 13.0, 21.0, 19.0, 1.5))],
        BackToApp => vec![s("M10.5 5H19a2 2 0 0 1 2 2v10a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-8.5"), s("M3.5 3.5l7 7"), s("M10.5 5.5v5h-5")],
        Window => vec![s(rrect(3.0, 4.0, 21.0, 20.0, 2.0)), s("M3 8.5h18")],
        Lock => vec![s(rrect(5.0, 10.5, 19.0, 20.5, 2.0)), s("M8 10.5V7.5a4 4 0 0 1 8 0v3")],
        Restart => vec![s("M19.65 12A7.65 7.65 0 1 1 12 4.35C14.14 4.35 16.19 5.2 17.73 6.68L19.65 8.6"), s("M19.65 4.35V8.6H15.4")],
        Power => vec![s("M12 3.5V11"), s(arc(12.0, 13.0, 8.0, -55.0, 235.0))],
        SignOut => vec![s("M9.5 20.5H6a2 2 0 0 1-2-2v-13a2 2 0 0 1 2-2h3.5"), s("M15.5 16.5l4.5-4.5-4.5-4.5"), s("M20 12H9.5")],
        Calculator => {
            let mut v = vec![s(rrect(5.0, 3.0, 19.0, 21.0, 2.0)), s("M8.5 7.5h7")];
            for y in [12.5f32, 16.5] {
                for x in [8.75f32, 12.0, 15.25] {
                    v.push(f(circle(x, y, 1.0)));
                }
            }
            v
        }
        Trash => vec![s("M4 6.5h16"), s(TRASH_BODY), s("M9.5 6.5V4.5a1.5 1.5 0 0 1 1.5-1.5h2a1.5 1.5 0 0 1 1.5 1.5v2"), s("M10.5 10.5v6.5M13.5 10.5v6.5")],
        TrashFull => vec![
            s("M4.5 8.5h15"),
            s("M6.5 8.5V19a2 2 0 0 0 2 2h7a2 2 0 0 0 2-2V8.5"),
            s("M7.5 8.5L8.5 4l4.5 1-.8 3.5"),
            s("M13 8.5l2.25-4.5 3.25 1.75-1.4 2.75"),
            s("M10.5 12v5M13.5 12v5"),
        ],
        Launchpad => {
            let mut v = Vec::new();
            for (row, cols) in [(4.0f32, &[4.0f32, 10.0, 16.0][..]), (10.0, &[4.0, 10.0, 16.0][..]), (16.0, &[7.0, 13.0][..])] {
                for &x in cols {
                    v.push(f(rrect(x, row, x + 4.0, row + 4.0, 1.0)));
                }
            }
            v
        }
        // mark.svg's 1024 box fitted to the grid: 18 high, centered; the hat
        // and the beard.
        Mark => vec![f(MARK_HAT), f(MARK_BEARD)],
        Terminal => vec![s(rrect(3.0, 4.5, 21.0, 19.5, 2.5)), s("M7.5 9.5l3 2.5-3 2.5"), s("M12.5 15h4")],
        Gamepad => vec![
            s("M7.5 7.5h9a4.5 4.5 0 0 1 4.5 4.5v2a3 3 0 0 1-5.4 1.8L14.25 14h-4.5l-1.35 1.8A3 3 0 0 1 3 14v-2a4.5 4.5 0 0 1 4.5-4.5Z"),
            s("M8 9.75v3.5M6.25 11.5h3.5"),
            f(circle(15.5, 10.5, 1.0)),
            f(circle(17.5, 12.75, 1.0)),
        ],
        EyeOff => vec![
            s("M3 12C5.2 8.3 8.4 6.25 12 6.25C15.6 6.25 18.8 8.3 21 12C18.8 15.7 15.6 17.75 12 17.75C8.4 17.75 5.2 15.7 3 12Z"),
            s(circle(12.0, 12.0, 2.75)),
            s("M4.5 4.5l15 15"),
        ],
        Dock => vec![s(screen()), f(rrect(7.5, 14.5, 16.5, 17.0, 1.25))],
        TopBar => vec![s(screen()), f(rrect(5.5, 7.0, 18.5, 9.5, 1.25))],
        Appearance => vec![s(circle(12.0, 12.0, 9.0)), f("M12 3A9 9 0 0 1 12 21Z")],
        Undo => vec![s("M8.5 4.75l-4 4 4 4"), s("M4.5 8.75h9.75a5.25 5.25 0 0 1 0 10.5H9")],
        Tray => vec![s("M3.5 13.5h4.5l1.5 2.5h5l1.5-2.5h4.5"), s("M3.5 13.5V18a2 2 0 0 0 2 2h13a2 2 0 0 0 2-2v-4.5"), s("M12 3.5v8"), s("M8.75 8.25L12 11.5l3.25-3.25")],
        Airplane => vec![s("M12 3C12.8 3 13.5 3.9 13.5 5V9.5L20 13.5V15.5L13.5 13.5V17.5L15.5 19V20.5L12 19.5L8.5 20.5V19L10.5 17.5V13.5L4 15.5V13.5L10.5 9.5V5C10.5 3.9 11.2 3 12 3Z")],
        NightLight => {
            let mut v = vec![s(arc(12.0, 16.0, 4.5, -180.0, 0.0)), s("M3 16h18"), s("M7.5 19.5h9")];
            for a in [-155.0f32, -122.5, -90.0, -57.5, -25.0] {
                v.push(s(ray(12.0, 16.0, a, 7.0, 9.0)));
            }
            v
        }
        GnomeHat => vec![s("M4 19C6.3 13.2 8.5 8.1 10.4 4.5C11.5 2.5 14.3 3.5 15 6.3C13.9 5.8 13 5.9 12.7 6.7L20 19C15 20.5 9 20.5 4 19Z")],
        Leaf => vec![s("M5.5 18.5C5 10.5 10 5 19.5 4.5C19.5 13.5 14 19 5.5 18.5Z"), s("M5.5 18.5L13.5 10.5"), s("M5.5 18.5L3.5 20.5")],
        Cup => vec![s("M4.5 9.5h11V15a4.5 4.5 0 0 1-4.5 4.5H9A4.5 4.5 0 0 1 4.5 15Z"), s("M15.5 11h1.5a2.5 2.5 0 0 1 0 5h-1.5"), s("M8 3.5v3M12 3.5v3")],
        BellOff => vec![s("M6 14V10a6 6 0 0 1 12 0v4l1.5 2.5h-15Z"), s("M10 18.5a2 2 0 0 0 4 0"), s("M12 3v1"), s("M3.5 3.5l17 17")],
        Bell => vec![s("M6 14V10a6 6 0 0 1 12 0v4l1.5 2.5h-15Z"), s("M10 18.5a2 2 0 0 0 4 0"), s("M12 3v1")],
        Hotspot => vec![
            f(circle(12.0, 10.5, 1.5)),
            s(arc(12.0, 10.5, 4.25, -45.0, 45.0)),
            s(arc(12.0, 10.5, 4.25, 135.0, 225.0)),
            s(arc(12.0, 10.5, 8.0, -45.0, 45.0)),
            s(arc(12.0, 10.5, 8.0, 135.0, 225.0)),
            s("M12 13v7.5"),
        ],
        Project => vec![s("M9 12.5H5a2 2 0 0 1-2-2V6a2 2 0 0 1 2-2h8a2 2 0 0 1 2 2v3.5"), s(rrect(9.0, 9.5, 21.0, 18.0, 2.0)), s("M15 18v2.5M12.5 20.5h5")],
        SlidersDot => vec![
            s("M3.5 6H11.25"),
            s(circle(13.5, 6.0, 2.25)),
            f(circle(19.5, 4.5, 1.75)),
            s("M3.5 12H6.75M11.25 12h9.25"),
            s(circle(9.0, 12.0, 2.25)),
            s("M3.5 18H10.75M15.25 18h5.25"),
            s(circle(13.0, 18.0, 2.25)),
        ],
        Eject => vec![s("M12 4.5L19.5 13H4.5Z"), s("M4.5 18.5H19.5")],
        UsbDrive => vec![s(rrect(7.0, 9.0, 17.0, 21.0, 2.0)), s("M9 9V3.5H15V9"), s("M11 5.75v1M13 5.75v1")],
        SdCard => vec![s("M8.5 3H17A2 2 0 0 1 19 5V19A2 2 0 0 1 17 21H7A2 2 0 0 1 5 19V6.5Z"), s("M10 3.5v3M13 3.5v3M16 3.5v3")],
        Drive => vec![s(rrect(3.0, 7.0, 21.0, 17.0, 2.5)), s("M6.5 12h6"), f(circle(17.0, 12.0, 1.1))],
        Disc => vec![s(circle(12.0, 12.0, 9.0)), s(circle(12.0, 12.0, 2.5))],
        ChatBubble => vec![s("M6 4.5h12a2 2 0 0 1 2 2v9a2 2 0 0 1-2 2h-7l-5 3v-3a2 2 0 0 1-2-2v-9a2 2 0 0 1 2-2Z")],
        FingerTap => vec![s("M9 13V8a2 2 0 0 1 4 0v4l4 1a3 3 0 0 1 2 3l-1 4H10l-5-5a1.5 1.5 0 0 1 2-2l2 1"), s("M5 7L3.5 5.5M9 4V3M15.5 6L17 4.5")],
        Accessibility => vec![s(circle(12.0, 5.0, 2.0)), s("M4 9L12 11L20 9M12 11V15M12 15L8 21M12 15L16 21")],
        FanMenu => vec![s("M5 16a8 8 0 0 1 14 0M8 17l4 4 4-4"), s(circle(5., 11., 1.8)), s(circle(12., 5., 2.)), s(circle(19., 11., 1.8))],
        MoveArrows => vec![s("M12 3v18M3 12h18M9 6l3-3 3 3M9 18l3 3 3-3M6 9l-3 3 3 3M18 9l3 3-3 3")],
        Sparkle => vec![s("M11 3.5C11.55 8.4 13.1 9.95 18 10.5C13.1 11.05 11.55 12.6 11 17.5C10.45 12.6 8.9 11.05 4 10.5C8.9 9.95 10.45 8.4 11 3.5Z"), s("M18.5 15.5v5M16 18h5")],
        Calendar => vec![s(rrect(3.5, 5.0, 20.5, 20.5, 2.0)), s("M3.5 9.5h17"), s("M8 3v4M16 3v4"), f(rrect(7.0, 12.5, 10.0, 15.5, 0.75))],
        Corners => vec![s("M3.5 9V6.5a2 2 0 0 1 2-2H8"), s("M16 4.5h2.5a2 2 0 0 1 2 2V9"), s("M20.5 15v2.5a2 2 0 0 1-2 2H16"), s("M8 19.5H5.5a2 2 0 0 1-2-2V15"), f(circle(12.0, 12.0, 1.75))],
        External => vec![s("M18 13.5V18a2 2 0 0 1-2 2H6a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h4.5"), s("M14 4h6v6"), s("M20 4l-9 9")],
        Processor => vec![s(rrect(6.5, 6.5, 17.5, 17.5, 2.0)), s(rrect(9.75, 9.75, 14.25, 14.25, 0.75)), s("M9.5 3.5v3M14.5 3.5v3M9.5 17.5v3M14.5 17.5v3M3.5 9.5h3M3.5 14.5h3M17.5 9.5h3M17.5 14.5h3")],
        Graphics => vec![s(rrect(3.0, 6.0, 21.0, 16.5, 2.0)), s(circle(9.5, 11.25, 2.75)), s("M15 9.5h3M15 13h3"), s("M6 16.5v3M10.5 16.5v3")],
        Memory => vec![s(rrect(3.0, 6.5, 21.0, 15.5, 1.5)), s("M7 9.5v3M10.33 9.5v3M13.67 9.5v3M17 9.5v3"), s("M5.5 15.5v3M9 15.5v3M15 15.5v3M18.5 15.5v3")],
    }
}

// ---- path data ------------------------------------------------------------------

/// A grid point.
pub type P = (f32, f32);

/// One absolute path command.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Cmd {
    Move(P),
    Line(P),
    Cubic(P, P, P),
    /// SVG arc (circular): to `to` with radius `r`, the large arc or not,
    /// clockwise (y down) or not.
    Arc { to: P, r: f32, large: bool, sweep: bool },
    Close,
}

fn number(b: &[u8], i: &mut usize) -> Option<f32> {
    while *i < b.len() && (b[*i].is_ascii_whitespace() || b[*i] == b',') {
        *i += 1;
    }
    let start = *i;
    if *i < b.len() && (b[*i] == b'-' || b[*i] == b'+') {
        *i += 1;
    }
    let mut digits = 0;
    while *i < b.len() && b[*i].is_ascii_digit() {
        *i += 1;
        digits += 1;
    }
    if *i < b.len() && b[*i] == b'.' {
        *i += 1;
        while *i < b.len() && b[*i].is_ascii_digit() {
            *i += 1;
            digits += 1;
        }
    }
    if digits == 0 {
        *i = start;
        return None;
    }
    if *i < b.len() && (b[*i] == b'e' || b[*i] == b'E') {
        let save = *i;
        *i += 1;
        if *i < b.len() && (b[*i] == b'-' || b[*i] == b'+') {
            *i += 1;
        }
        let ds = *i;
        while *i < b.len() && b[*i].is_ascii_digit() {
            *i += 1;
        }
        if *i == ds {
            *i = save;
        }
    }
    std::str::from_utf8(&b[start..*i]).ok()?.parse().ok()
}

/// Parses SVG path data (M L H V C A Z, absolute and relative; numbers
/// after a command repeat it, after M as L). None if it is malformed.
pub fn parse(d: &str) -> Option<Vec<Cmd>> {
    let b = d.as_bytes();
    let mut i = 0;
    let mut cmd = 0u8;
    let (mut cur, mut start): (P, P) = ((0.0, 0.0), (0.0, 0.0));
    let mut out = Vec::new();
    loop {
        while i < b.len() && (b[i].is_ascii_whitespace() || b[i] == b',') {
            i += 1;
        }
        if i >= b.len() {
            break;
        }
        if b[i].is_ascii_alphabetic() {
            cmd = b[i];
            i += 1;
            if cmd == b'Z' || cmd == b'z' {
                out.push(Cmd::Close);
                cur = start;
                continue;
            }
        } else if cmd == 0 || cmd == b'Z' || cmd == b'z' {
            return None;
        }
        let rel = cmd.is_ascii_lowercase();
        let base = if rel { cur } else { (0.0, 0.0) };
        let mut n = || number(b, &mut i);
        match cmd.to_ascii_uppercase() {
            b'M' => {
                let p = (base.0 + n()?, base.1 + n()?);
                out.push(Cmd::Move(p));
                cur = p;
                start = p;
                cmd = if rel { b'l' } else { b'L' };
            }
            b'L' => {
                let p = (base.0 + n()?, base.1 + n()?);
                out.push(Cmd::Line(p));
                cur = p;
            }
            b'H' => {
                let p = (base.0 + n()?, cur.1);
                out.push(Cmd::Line(p));
                cur = p;
            }
            b'V' => {
                let p = (cur.0, base.1 + n()?);
                out.push(Cmd::Line(p));
                cur = p;
            }
            b'C' => {
                let c1 = (base.0 + n()?, base.1 + n()?);
                let c2 = (base.0 + n()?, base.1 + n()?);
                let p = (base.0 + n()?, base.1 + n()?);
                out.push(Cmd::Cubic(c1, c2, p));
                cur = p;
            }
            b'A' => {
                let r = n()?;
                let _ry = n()?;
                let _rot = n()?;
                let large = n()? != 0.0;
                let sweep = n()? != 0.0;
                let p = (base.0 + n()?, base.1 + n()?);
                out.push(Cmd::Arc { to: p, r, large, sweep });
                cur = p;
            }
            _ => return None,
        }
    }
    (!out.is_empty()).then_some(out)
}

/// The circle an SVG arc from `p0` lies on: (center, radius, start angle,
/// signed sweep), angles in radians, y down. A radius too small for the
/// chord grows to half of it (as SVG does). None for a zero-length arc.
pub fn arc_center(p0: P, p1: P, r: f32, large: bool, sweep: bool) -> Option<(P, f32, f32, f32)> {
    let (hx, hy) = ((p0.0 - p1.0) / 2.0, (p0.1 - p1.1) / 2.0);
    let d2 = hx * hx + hy * hy;
    if d2 <= 1e-12 {
        return None;
    }
    let r = r.abs().max(d2.sqrt());
    let k = (r * r / d2 - 1.0).max(0.0).sqrt();
    let sign = if large != sweep { 1.0 } else { -1.0 };
    let (cxp, cyp) = (sign * k * hy, -sign * k * hx);
    let c = (cxp + (p0.0 + p1.0) / 2.0, cyp + (p0.1 + p1.1) / 2.0);
    let a0 = (hy - cyp).atan2(hx - cxp);
    let a1 = (-hy - cyp).atan2(-hx - cxp);
    let mut da = a1 - a0;
    if sweep && da < 0.0 {
        da += std::f32::consts::TAU;
    } else if !sweep && da > 0.0 {
        da -= std::f32::consts::TAU;
    }
    Some((c, r, a0, da))
}

/// Polylines of `cmds` (curves cut into short lines), with whether each is
/// closed.
pub fn flatten(cmds: &[Cmd]) -> Vec<(Vec<P>, bool)> {
    let mut out: Vec<(Vec<P>, bool)> = Vec::new();
    let mut cur: Vec<P> = Vec::new();
    let mut at: P = (0.0, 0.0);
    let mut start: P = (0.0, 0.0);
    let finish = |cur: &mut Vec<P>, out: &mut Vec<(Vec<P>, bool)>, closed: bool| {
        if cur.len() > 1 {
            out.push((std::mem::take(cur), closed));
        } else {
            cur.clear();
        }
    };
    for c in cmds {
        match *c {
            Cmd::Move(p) => {
                finish(&mut cur, &mut out, false);
                cur.push(p);
                at = p;
                start = p;
            }
            Cmd::Close => {
                finish(&mut cur, &mut out, true);
                at = start;
            }
            _ => {
                if cur.is_empty() {
                    cur.push(at);
                }
                match *c {
                    Cmd::Line(p) => {
                        cur.push(p);
                        at = p;
                    }
                    Cmd::Cubic(c1, c2, p) => {
                        const N: usize = 16;
                        for k in 1..=N {
                            let t = k as f32 / N as f32;
                            let u = 1.0 - t;
                            let (a, b, cc, d) = (u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t);
                            cur.push((a * at.0 + b * c1.0 + cc * c2.0 + d * p.0, a * at.1 + b * c1.1 + cc * c2.1 + d * p.1));
                        }
                        at = p;
                    }
                    Cmd::Arc { to, r, large, sweep } => {
                        match arc_center(at, to, r, large, sweep) {
                            Some((cc, rr, a0, da)) => {
                                let n = ((da.abs() / 10f32.to_radians()).ceil() as usize).max(2);
                                for k in 1..n {
                                    let a = a0 + da * k as f32 / n as f32;
                                    cur.push((cc.0 + rr * a.cos(), cc.1 + rr * a.sin()));
                                }
                                cur.push(to);
                            }
                            None => cur.push(to),
                        }
                        at = to;
                    }
                    _ => {}
                }
            }
        }
    }
    finish(&mut cur, &mut out, false);
    out
}

pub(crate) unsafe fn provider_logo(
    dc: &windows::Win32::Graphics::Direct2D::ID2D1DeviceContext,
    name: &str,
    x: f32,
    y: f32,
    size: f32,
    color: D2D1_COLOR_F,
) -> Result<()> {
    use windows::core::Interface;
    use windows::Win32::{
        Foundation::E_FAIL,
        Graphics::Direct2D::{Common::D2D_SIZE_F, ID2D1DeviceContext5},
        UI::Shell::SHCreateMemStream,
    };
    use windows_numerics::Matrix3x2;
    let color = format!(
        "#{:02x}{:02x}{:02x}",
        (color.r * 255.) as u8,
        (color.g * 255.) as u8,
        (color.b * 255.) as u8
    );
    let source = match name {
        "ChatGPT" => include_str!("../../assets/brands/openai.svg"),
        "Claude" | "Claude Code" => include_str!("../../assets/brands/claude.svg"),
        "Antigravity" => include_str!("../../assets/brands/antigravity.svg"),
        _ => return Err(windows::core::Error::from(E_FAIL)),
    };
    // Normalize the artwork bounds, not the source canvas: OpenAI carries
    // much more built-in padding than Claude. All marks share a 72% slot.
    let mut svg = source
        .replace(
            "viewBox=\"0 0 721 721\"",
            "viewBox=\"118.557 119.958 484.139 479.818\"",
        )
        .replace("viewBox=\"0 0 248 248\"", "viewBox=\"6.2 6.2 235.6 235.6\"")
        .replace("viewBox=\"22 24 92 88\"", "viewBox=\"24 28 88 80\"")
        .replace("fill=\"black\"", &format!("fill=\"{color}\""))
        .replace("fill=\"#000000\"", &format!("fill=\"{color}\""));
    for n in [721, 248, 136] {
        svg = svg
            .replace(&format!("width=\"{n}\""), "width=\"100%\"")
            .replace(&format!("height=\"{n}\""), "height=\"100%\"");
    }
    let stream = SHCreateMemStream(Some(svg.as_bytes()))
        .ok_or_else(|| windows::core::Error::from(E_FAIL))?;
    let dc5: ID2D1DeviceContext5 = dc.cast()?;
    let doc = dc5.CreateSvgDocument(
        &stream,
        D2D_SIZE_F {
            width: size,
            height: size,
        },
    )?;
    let mut transform = Matrix3x2::identity();
    dc.GetTransform(&mut transform);
    let offset = Matrix3x2 {
        M31: x,
        M32: y,
        ..Matrix3x2::identity()
    };
    dc.SetTransform(&(offset * transform));
    dc5.DrawSvgDocument(&doc);
    dc.SetTransform(&transform);
    Ok(())
}

// ---- the brand mark ---------------------------------------------------------------

/// The gnome mark's hat (with the nose under its brim) on the grid:
/// assets/brand/mark.svg's first path.
pub const MARK_HAT: &str = "M6.37 12.93 C6.09 12.8 6.26 12.41 6.47 11.94 C7.38 10.08 8.76 8.14 10.06 6.11 C11.44 3.99 13.04 3 14.68 3 C16.32 3 17.53 3.86 17.83 5.16 C18.13 6.24 17.61 6.93 17.09 6.95 C16.88 6.95 16.81 6.82 16.96 6.58 C17.31 6.02 16.45 4.99 15.58 4.99 C14.68 4.99 13.94 5.59 13.64 6.37 C13.38 7.23 13.81 8.01 14.29 8.7 C15.02 9.95 15.84 11.42 16.14 12.32 C16.27 12.63 16.32 12.8 16.1 12.93 C14.98 12.15 13.68 11.78 12.39 11.72 C12.35 12.41 12 12.73 11.35 12.73 C10.71 12.73 10.4 12.37 10.38 11.72 C8.94 11.78 7.42 12.19 6.37 12.93 Z";
/// The gnome mark's beard: mark.svg's second path.
pub const MARK_BEARD: &str = "M6.65 13.62 C6.78 15.09 8.81 16.25 11.27 16.25 C13.73 16.25 15.76 15.09 15.93 13.62 C16.45 14.05 16.58 14.87 16.32 15.91 C15.71 17.68 14.07 19.32 11.91 20.61 C11.53 20.89 11.27 21 10.88 20.78 C8.85 19.75 6.91 18.02 6.3 16.42 C5.87 15.26 6 14.14 6.65 13.62 Z";

// ---- placing ------------------------------------------------------------------------

/// Where a glyph lands in device pixels: its 24-grid's top left, px per
/// grid unit, and the stroke width.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Frame {
    pub x: f32,
    pub y: f32,
    pub k: f32,
    pub stroke: f32,
}

impl Frame {
    pub fn map(&self, p: P) -> P {
        (self.x + p.0 * self.k, self.y + p.1 * self.k)
    }
}

/// The stroke for a glyph `size_px` a side drawn `weight` times as heavy:
/// 1.5 / 24 of the size, a whole number of pixels (at least 1) at small
/// sizes.
pub fn stroke_px(size_px: f32, weight: f32) -> f32 {
    let raw = STROKE * size_px / GRID * weight;
    if size_px <= SNAP_MAX_PX {
        raw.round().max(1.0)
    } else {
        raw.max(1.0)
    }
}

/// A glyph `size_px` a side centered on (cx, cy) (device px), snapped (see
/// the module notes).
pub fn frame(cx: f32, cy: f32, size_px: f32, weight: f32) -> Frame {
    let k = size_px / GRID;
    let stroke = stroke_px(size_px, weight);
    let whole = (stroke - stroke.round()).abs() < 1e-4;
    let (x, y) = if whole {
        // The grid's center on a pixel center (odd stroke) or corner (even).
        let odd = (stroke.round() as i64) % 2 == 1;
        let snap = |c: f32| if odd { (c - 0.5).round() + 0.5 } else { c.round() };
        (snap(cx) - 12.0 * k, snap(cy) - 12.0 * k)
    } else {
        ((cx - 12.0 * k).round(), (cy - 12.0 * k).round())
    };
    Frame { x, y, k, stroke }
}

// ---- Direct2D ---------------------------------------------------------------------

/// The grid box per unit of `size` in `draw`: `size` is an em like the
/// icon font's, so a glyph's ink (3..21 of the grid) is about 0.9 em like
/// the font glyphs it replaces and stays inside a `size` square.
pub const EM: f32 = 1.2;

/// Draws `g` at `size` (an em, see EM) centered on (cx, cy) in `dc`'s
/// current units (its transform and DPI are respected; snapping happens in
/// device pixels when the transform is a plain scale and offset).
pub unsafe fn draw(dc: &ID2D1RenderTarget, g: Glyph, cx: f32, cy: f32, size: f32, color: D2D1_COLOR_F) -> Result<()> {
    draw_weighted(dc, g, cx, cy, size, color, 1.0)
}

/// `draw` with the stroke `weight` times the standard.
pub unsafe fn draw_weighted(dc: &ID2D1RenderTarget, g: Glyph, cx: f32, cy: f32, size: f32, color: D2D1_COLOR_F, weight: f32) -> Result<()> {
    if size <= 0.0 || color.a <= 0.0 {
        return Ok(());
    }
    let mut m = Matrix3x2::default();
    dc.GetTransform(&mut m);
    let (mut dpx, mut dpy) = (0.0f32, 0.0f32);
    dc.GetDpi(&mut dpx, &mut dpy);
    match placement(&m, dpx, cx, cy, size, weight) {
        Place::Device(fr) => draw_device(dc, g, &fr, color),
        Place::Local(fr) => render(dc, g, &fr, color),
    }
}

/// Where `draw` puts a glyph.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Place {
    /// In device pixels (snapped): the transform is a plain scale (maybe
    /// mirrored) and offset.
    Device(Frame),
    /// In the context's own units (rotated or skewed: not snapped).
    Local(Frame),
}

/// `draw`'s placement for transform `m` at `dpi` (0 = 96): pure.
pub fn placement(m: &Matrix3x2, dpi: f32, cx: f32, cy: f32, size: f32, weight: f32) -> Place {
    let size = size * EM;
    let d = if dpi > 0.0 { dpi / 96.0 } else { 1.0 };
    let axis = m.M12 == 0.0 && m.M21 == 0.0 && m.M11 != 0.0 && (m.M11.abs() - m.M22.abs()).abs() < 1e-4;
    if axis {
        // Device pixels: snap there, draw unmirrored.
        let s = m.M11.abs() * d;
        let (dcx, dcy) = ((m.M11 * cx + m.M31) * d, (m.M22 * cy + m.M32) * d);
        Place::Device(frame(dcx, dcy, size * s, weight))
    } else {
        let k = size / GRID;
        Place::Local(Frame { x: cx - 12.0 * k, y: cy - 12.0 * k, k, stroke: STROKE * k * weight })
    }
}

/// Draws `g` at a frame in device pixels.
pub unsafe fn draw_device(dc: &ID2D1RenderTarget, g: Glyph, fr: &Frame, color: D2D1_COLOR_F) -> Result<()> {
    if color.a <= 0.0 {
        return Ok(());
    }
    let mut m = Matrix3x2::default();
    dc.GetTransform(&mut m);
    let (mut dpx, mut dpy) = (0.0f32, 0.0f32);
    dc.GetDpi(&mut dpx, &mut dpy);
    let d = if dpx > 0.0 { dpx / 96.0 } else { 1.0 };
    dc.SetTransform(&Matrix3x2 { M11: 1.0 / d, M12: 0.0, M21: 0.0, M22: 1.0 / d, M31: 0.0, M32: 0.0 });
    let r = render(dc, g, fr, color);
    dc.SetTransform(&m);
    r
}

/// The ink of `g` placed at `fr`: (left, top, right, bottom) in the
/// frame's pixels, strokes (with their round caps) included.
pub fn ink_bounds(g: Glyph, fr: &Frame) -> (f32, f32, f32, f32) {
    let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for p in parts(g) {
        let Some(cmds) = parse(&p.d) else { continue };
        let pad = if p.paint == Paint::Fill { 0.0 } else { fr.stroke / 2.0 };
        for (pts, _) in flatten(&cmds) {
            for q in pts {
                let (x, y) = fr.map(q);
                x0 = x0.min(x - pad);
                y0 = y0.min(y - pad);
                x1 = x1.max(x + pad);
                y1 = y1.max(y + pad);
            }
        }
    }
    (x0, y0, x1, y1)
}

/// A glyph at `em` px in a `side` px square surface: the frame (snapped,
/// about the square's center) and the whole pixels to pad the square with
/// on every side so the ink is never cut (`Gfx::glyph`).
pub fn padded(g: Glyph, side: f32, em: f32) -> (Frame, f32) {
    let fr = frame(side / 2.0, side / 2.0, em * EM, 1.0);
    let (x0, y0, x1, y1) = ink_bounds(g, &fr);
    let over = [-x0, -y0, x1 - side, y1 - side].into_iter().fold(0.0f32, f32::max);
    let pad = (over - 1e-3).max(0.0).ceil();
    (Frame { x: fr.x + pad, y: fr.y + pad, ..fr }, pad)
}

/// A glyph at about `em` px centered in a fixed `side` px square at
/// (ox, oy) (whole pixels): snapped, and a little smaller if the snapped
/// ink would cross the square's edge (top bar cells, island leads).
pub fn fitted(g: Glyph, ox: f32, oy: f32, side: f32, em: f32) -> Frame {
    let mut size = em * EM;
    let mut fr = frame(ox + side / 2.0, oy + side / 2.0, size, 1.0);
    for _ in 0..12 {
        let (x0, y0, x1, y1) = ink_bounds(g, &fr);
        if x0 >= ox - 1e-3 && y0 >= oy - 1e-3 && x1 <= ox + side + 1e-3 && y1 <= oy + side + 1e-3 {
            break;
        }
        size *= 0.97;
        fr = frame(ox + side / 2.0, oy + side / 2.0, size, 1.0);
    }
    fr
}

unsafe fn render(dc: &ID2D1RenderTarget, g: Glyph, fr: &Frame, color: D2D1_COLOR_F) -> Result<()> {
    use windows::core::Interface;
    use windows::Win32::Graphics::Direct2D::{ID2D1DeviceContext, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE, D2D1_LAYER_OPTIONS1_NONE, D2D1_LAYER_PARAMETERS1};
    if color.a < 0.999 {
        if let Ok(ctx) = dc.cast::<ID2D1DeviceContext>() {
            // Opaque in a layer at the color's alpha (over the ink only).
            let (x0, y0, x1, y1) = ink_bounds(g, fr);
            let params = D2D1_LAYER_PARAMETERS1 {
                contentBounds: windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F { left: x0 - 1.0, top: y0 - 1.0, right: x1 + 1.0, bottom: y1 + 1.0 },
                geometricMask: std::mem::ManuallyDrop::new(None),
                maskAntialiasMode: D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
                maskTransform: Matrix3x2::identity(),
                opacity: color.a,
                opacityBrush: std::mem::ManuallyDrop::new(None),
                layerOptions: D2D1_LAYER_OPTIONS1_NONE,
            };
            ctx.PushLayer(&params, None);
            let r = render_parts(dc, g, fr, D2D1_COLOR_F { a: 1.0, ..color });
            ctx.PopLayer();
            return r;
        }
    }
    render_parts(dc, g, fr, color)
}

/// `cmds` placed at `fr` as a Direct2D path (filled figures when `filled`).
unsafe fn path_geometry(factory: &ID2D1Factory, cmds: Vec<Cmd>, fr: &Frame, filled: bool) -> Result<ID2D1PathGeometry> {
    let v = |p: P| {
        let (x, y) = fr.map(p);
        Vector2 { X: x, Y: y }
    };
    let geom = factory.CreatePathGeometry()?;
    let sink = geom.Open()?;
    sink.SetFillMode(D2D1_FILL_MODE_WINDING);
    let begin = if filled { D2D1_FIGURE_BEGIN_FILLED } else { D2D1_FIGURE_BEGIN_HOLLOW };
    let (mut open, mut at, mut start): (bool, P, P) = (false, (0.0, 0.0), (0.0, 0.0));
    for c in cmds {
        if !open && !matches!(c, Cmd::Move(_) | Cmd::Close) {
            sink.BeginFigure(v(at), begin);
            open = true;
        }
        match c {
            Cmd::Move(q) => {
                if open {
                    sink.EndFigure(D2D1_FIGURE_END_OPEN);
                }
                sink.BeginFigure(v(q), begin);
                open = true;
                at = q;
                start = q;
            }
            Cmd::Line(q) => {
                sink.AddLine(v(q));
                at = q;
            }
            Cmd::Cubic(a, b, q) => {
                sink.AddBezier(&D2D1_BEZIER_SEGMENT { point1: v(a), point2: v(b), point3: v(q) });
                at = q;
            }
            Cmd::Arc { to, r, large, sweep } => {
                sink.AddArc(&D2D1_ARC_SEGMENT {
                    point: v(to),
                    size: D2D_SIZE_F { width: r * fr.k, height: r * fr.k },
                    rotationAngle: 0.0,
                    sweepDirection: if sweep { D2D1_SWEEP_DIRECTION_CLOCKWISE } else { D2D1_SWEEP_DIRECTION_COUNTER_CLOCKWISE },
                    arcSize: if large { D2D1_ARC_SIZE_LARGE } else { D2D1_ARC_SIZE_SMALL },
                });
                at = to;
            }
            Cmd::Close => {
                if open {
                    sink.EndFigure(D2D1_FIGURE_END_CLOSED);
                    open = false;
                }
                at = start;
            }
        }
    }
    if open {
        sink.EndFigure(D2D1_FIGURE_END_OPEN);
    }
    sink.Close()?;
    Ok(geom)
}

unsafe fn render_parts(dc: &ID2D1RenderTarget, g: Glyph, fr: &Frame, color: D2D1_COLOR_F) -> Result<()> {
    let factory = dc.GetFactory()?;
    let props = D2D1_STROKE_STYLE_PROPERTIES {
        startCap: D2D1_CAP_STYLE_ROUND,
        endCap: D2D1_CAP_STYLE_ROUND,
        dashCap: D2D1_CAP_STYLE_ROUND,
        lineJoin: D2D1_LINE_JOIN_ROUND,
        miterLimit: 10.0,
        dashStyle: D2D1_DASH_STYLE_SOLID,
        dashOffset: 0.0,
    };
    let style = factory.CreateStrokeStyle(&props, None)?;
    let solid = dc.CreateSolidColorBrush(&color, None)?;
    let parts = parts(g);
    // Only glyphs with faint parts ask the system (registry) about it.
    let faint = if parts.iter().any(|p| p.faint) && crate::syslook::current().high_contrast.is_some() { FAINT_HC } else { FAINT };
    let dim = dc.CreateSolidColorBrush(&D2D1_COLOR_F { a: color.a * faint, ..color }, None)?;
    for p in parts {
        let Some(cmds) = parse(&p.d) else { continue };
        let filled = p.paint != Paint::Stroke;
        let geom = path_geometry(&factory, cmds, fr, filled)?;
        let brush = if p.faint { &dim } else { &solid };
        if filled {
            dc.FillGeometry(&geom, brush, None);
        }
        if p.paint != Paint::Fill {
            dc.DrawGeometry(&geom, brush, fr.stroke, &style);
        }
    }
    Ok(())
}

// ---- software ---------------------------------------------------------------------

fn seg_dist2(p: P, a: P, b: P) -> f32 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let l2 = dx * dx + dy * dy;
    let t = if l2 > 0.0 { (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / l2).clamp(0.0, 1.0) } else { 0.0 };
    let (qx, qy) = (a.0 + t * dx - p.0, a.1 + t * dy - p.1);
    qx * qx + qy * qy
}

/// Nonzero winding of the closed polygons at p.
fn winding(p: P, polys: &[Vec<P>]) -> i32 {
    let mut w = 0;
    for poly in polys {
        let n = poly.len();
        for i in 0..n {
            let (a, b) = (poly[i], poly[(i + 1) % n]);
            if a.1 <= p.1 {
                if b.1 > p.1 && (b.0 - a.0) * (p.1 - a.1) - (p.0 - a.0) * (b.1 - a.1) > 0.0 {
                    w += 1;
                }
            } else if b.1 <= p.1 && (b.0 - a.0) * (p.1 - a.1) - (p.0 - a.0) * (b.1 - a.1) < 0.0 {
                w -= 1;
            }
        }
    }
    w
}

/// Composites `g` into premultiplied BGRA `pixels` (`n` x `n`) at `fr` in
/// `rgba` (straight, 0..1): what `draw` draws, in software (4 x 4
/// samples where an edge crosses a pixel).
pub fn raster_into(pixels: &mut [u8], n: u32, g: Glyph, fr: &Frame, rgba: [f32; 4]) {
    let n = n as usize;
    if pixels.len() < n * n * 4 {
        return;
    }
    let hw = fr.stroke / 2.0;
    for p in parts(g) {
        let Some(cmds) = parse(&p.d) else { continue };
        let lines: Vec<(Vec<P>, bool)> = flatten(&cmds).into_iter().map(|(pts, closed)| (pts.into_iter().map(|q| fr.map(q)).collect(), closed)).collect();
        let stroke = p.paint != Paint::Fill;
        let fill = p.paint != Paint::Stroke;
        let mut segs: Vec<(P, P)> = Vec::new();
        for (pts, closed) in &lines {
            for w in pts.windows(2) {
                segs.push((w[0], w[1]));
            }
            if *closed || fill {
                if let (Some(&a), Some(&b)) = (pts.last(), pts.first()) {
                    segs.push((a, b));
                }
            }
        }
        let polys: Vec<Vec<P>> = if fill { lines.iter().map(|(pts, _)| pts.clone()).collect() } else { Vec::new() };
        if segs.is_empty() {
            continue;
        }
        let pad = if stroke { hw } else { 0.0 } + 1.0;
        let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
        for (a, b) in &segs {
            x0 = x0.min(a.0.min(b.0));
            y0 = y0.min(a.1.min(b.1));
            x1 = x1.max(a.0.max(b.0));
            y1 = y1.max(a.1.max(b.1));
        }
        let lo = |v: f32| (v - pad).floor().max(0.0) as usize;
        let hi = |v: f32| ((v + pad).ceil().max(0.0) as usize).min(n);
        let alpha = rgba[3] * if p.faint { FAINT } else { 1.0 };
        let inside = |q: P| -> bool {
            let d2 = segs.iter().map(|(a, b)| seg_dist2(q, *a, *b)).fold(f32::MAX, f32::min);
            (stroke && d2 <= hw * hw) || (fill && winding(q, &polys) != 0)
        };
        for y in lo(y0)..hi(y1) {
            for x in lo(x0)..hi(x1) {
                let c = (x as f32 + 0.5, y as f32 + 0.5);
                let d = segs.iter().map(|(a, b)| seg_dist2(c, *a, *b)).fold(f32::MAX, f32::min).sqrt();
                // Far from every edge: the center decides for the pixel.
                let edge = if stroke { (d - hw).abs() } else { d };
                let cover = if edge > 0.75 && (!stroke || !fill || d > hw + 0.75) {
                    if inside(c) {
                        1.0
                    } else {
                        0.0
                    }
                } else {
                    let mut hits = 0;
                    for sy in 0..4 {
                        for sx in 0..4 {
                            if inside((x as f32 + (sx as f32 + 0.5) / 4.0, y as f32 + (sy as f32 + 0.5) / 4.0)) {
                                hits += 1;
                            }
                        }
                    }
                    hits as f32 / 16.0
                };
                if cover <= 0.0 {
                    continue;
                }
                let a = alpha * cover;
                let i = (y * n + x) * 4;
                for (k, ch) in [rgba[2], rgba[1], rgba[0]].into_iter().enumerate() {
                    let v = ch * 255.0 * a + pixels[i + k] as f32 * (1.0 - a);
                    pixels[i + k] = v.round().clamp(0.0, 255.0) as u8;
                }
                let v = 255.0 * a + pixels[i + 3] as f32 * (1.0 - a);
                pixels[i + 3] = v.round().clamp(0.0, 255.0) as u8;
            }
        }
    }
}

// ---- dock tiles ---------------------------------------------------------------------

/// Signed distance from (x, y) to a rounded rectangle (negative inside).
pub fn rounded_sd(x: f32, y: f32, l: f32, t: f32, r: f32, b: f32, rad: f32) -> f32 {
    let (cx, cy) = ((l + r) / 2.0, (t + b) / 2.0);
    let (hx, hy) = ((r - l) / 2.0 - rad, (b - t) / 2.0 - rad);
    let (qx, qy) = ((x - cx).abs() - hx, (y - cy).abs() - hy);
    let outside = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt();
    outside + qx.max(qy).min(0.0) - rad
}

/// Colors of a drawn dock tile.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TileLook {
    /// The tile (straight RGBA 0..1).
    pub back: [f32; 4],
    /// The glyph on it.
    pub ink: [f32; 4],
    /// A 1 px outline in this color (high contrast).
    pub outline: Option<[f32; 4]>,
    /// The light theme's tile (the drawn pictures' palette).
    pub light: bool,
    /// Flat's matte pictures (else Glass's lit ones: crate::tile_art).
    pub flat: bool,
}

impl TileLook {
    /// Neutral grey tiles like the design direction's dock: graphite with
    /// light ink on a dark dock, near-white with slate ink on a light one;
    /// high contrast: the window color, its text color and an outline.
    pub fn new(light: bool, high_contrast: Option<([u8; 3], [u8; 3])>) -> Self {
        let c = |v: [u8; 3]| [v[0] as f32 / 255.0, v[1] as f32 / 255.0, v[2] as f32 / 255.0, 1.0];
        match high_contrast {
            Some((window, text)) => TileLook { back: c(window), ink: c(text), outline: Some(c(text)), light, flat: false },
            // Greys only (the design kit): near-black ink on a light tile,
            // the kit's tile grey with light ink in dark mode.
            None if light => TileLook { back: c([0xFB, 0xFB, 0xFB]), ink: c([0x27, 0x27, 0x27]), outline: None, light, flat: false },
            None => TileLook { back: c([0x34, 0x34, 0x34]), ink: c([0xE6, 0xE6, 0xE6]), outline: None, light, flat: false },
        }
    }

    /// The same look in Flat's pictures (`flat`) or Glass's.
    pub fn finish(self, flat: bool) -> Self {
        TileLook { flat, ..self }
    }
}

/// Share of the tile the glyph's 24-grid box takes (the Launchpad's tiles
/// a little more: they are its whole picture).
pub const TILE_GLYPH: f32 = 0.62;
const TILE_LAUNCHPAD: f32 = 0.7;
/// The gnome mark as large as on the brand icon (icon.svg: about 57 %).
const TILE_MARK: f32 = 0.76;

/// At this many px a side or fewer a tile is the line glyph on a flat
/// plate (the drawn pictures blur into each other there).
pub const LINE_TILE_MAX_PX: u32 = 20;

/// A full-bleed dock tile `px` a side for `g` (premultiplied BGRA), cut by
/// the dock to its squircle like app icons: the drawn picture on its lit
/// plate (crate::tile_art, design C); the line glyph on a flat tile at
/// LINE_TILE_MAX_PX or less, in high contrast (the system colors, an
/// outline) and if Direct2D fails.
pub fn tile(px: u32, g: Glyph, look: TileLook) -> crate::icons::Icon {
    let n = px.clamp(8, 1024);
    if look.outline.is_none() && n > LINE_TILE_MAX_PX {
        if let Some(i) = crate::tile_art::draw(n, g, look.light, look.flat) {
            return i;
        }
    }
    line_tile(n, g, look)
}

/// How `tile` draws at a size: pictures of one kind at two sizes differ
/// only in scale, so one can be shrunk to the other; the next kind down is
/// drawn apart (a picture shrunk from a larger kind keeps details the
/// smaller one leaves out).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TileKind {
    /// The line glyph on a flat tile (LINE_TILE_MAX_PX or less, high contrast).
    Line,
    /// The picture without its fine details (crate::tile_art::LOD_MAX_PX or less).
    Simple,
    /// The whole picture.
    Full,
}

/// The kind of tile `tile` draws `px` a side for `look`.
pub fn tile_kind(px: u32, look: &TileLook) -> TileKind {
    let n = px.clamp(8, 1024);
    if look.outline.is_some() || n <= LINE_TILE_MAX_PX {
        TileKind::Line
    } else if n <= crate::tile_art::LOD_MAX_PX {
        TileKind::Simple
    } else {
        TileKind::Full
    }
}

/// The line glyph centered on a flat rounded square (`look`'s colors).
pub fn line_tile(px: u32, g: Glyph, look: TileLook) -> crate::icons::Icon {
    let n = px.clamp(8, 1024);
    let s = n as f32;
    let rad = s * 0.225;
    let mut pixels = vec![0u8; (n * n * 4) as usize];
    const SS: usize = 4;
    for y in 0..n {
        for x in 0..n {
            let (mut cover, mut ring) = (0u32, 0u32);
            for sy in 0..SS {
                for sx in 0..SS {
                    let fx = x as f32 + (sx as f32 + 0.5) / SS as f32;
                    let fy = y as f32 + (sy as f32 + 0.5) / SS as f32;
                    let d = rounded_sd(fx, fy, 0.0, 0.0, s, s, rad);
                    if d <= 0.0 {
                        cover += 1;
                        if d > -(s / 48.0).max(1.0) {
                            ring += 1;
                        }
                    }
                }
            }
            if cover == 0 {
                continue;
            }
            let a = cover as f32 / (SS * SS) as f32;
            let mut col = look.back;
            if let Some(o) = look.outline {
                let t = ring as f32 / cover as f32;
                for k in 0..3 {
                    col[k] = col[k] * (1.0 - t) + o[k] * t;
                }
            }
            let i = ((y * n + x) * 4) as usize;
            let alpha = a * col[3];
            pixels[i] = (col[2] * 255.0 * alpha).round() as u8;
            pixels[i + 1] = (col[1] * 255.0 * alpha).round() as u8;
            pixels[i + 2] = (col[0] * 255.0 * alpha).round() as u8;
            pixels[i + 3] = (255.0 * alpha).round() as u8;
        }
    }
    let side = s * match g {
        Glyph::Launchpad => TILE_LAUNCHPAD,
        Glyph::Mark => TILE_MARK,
        _ => TILE_GLYPH,
    };
    // Thicker than a status glyph of the same side: the dock reads from afar.
    let fr = frame(s / 2.0, s / 2.0, side, 1.2);
    raster_into(&mut pixels, n, g, &fr, look.ink);
    crate::icons::Icon { width: n, height: n, pixels }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bounds(g: Glyph) -> (f32, f32, f32, f32) {
        let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
        for p in parts(g) {
            let cmds = parse(&p.d).unwrap_or_else(|| panic!("{g:?}: bad path {}", p.d));
            for (pts, _) in flatten(&cmds) {
                for (x, y) in pts {
                    x0 = x0.min(x);
                    y0 = y0.min(y);
                    x1 = x1.max(x);
                    y1 = y1.max(y);
                }
            }
        }
        (x0, y0, x1, y1)
    }


    #[test]
    fn brand_site_copies_match_the_canonical_assets() {
        let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let site = repo.join("site");
        if !site.exists() {
            return;
        }
        let brand = repo.join("assets/brand");
        for (canonical, copy) in [
            ("icon-small.svg", "nopbar/img/icon-small.svg"),
            ("icon-256.png", "nopbar/img/icon-256.png"),
            ("nopbar.ico", "favicon.ico"),
        ] {
            let expected = std::fs::read(brand.join(canonical)).unwrap();
            let actual = std::fs::read(site.join(copy)).unwrap_or_else(|error| panic!("{copy}: {error}"));
            assert!(actual == expected, "{copy} must match the canonical asset");
        }
    }

    #[test]
    fn the_mark_is_a_hat_over_a_beard() {
        // Two parts, the hat wholly above the beard (a gap between them).
        let ys = |d: &str| flatten(&parse(d).unwrap()).into_iter().flat_map(|(pts, _)| pts).fold((f32::MAX, f32::MIN), |(lo, hi), (_, y)| (lo.min(y), hi.max(y)));
        assert_eq!(parts(Glyph::Mark), vec![f(MARK_HAT), f(MARK_BEARD)]);
        let (hat, beard) = (ys(MARK_HAT), ys(MARK_BEARD));
        assert!(hat.1 < beard.0, "{hat:?} {beard:?}");
    }

    #[test]
    fn every_glyph_has_paths_on_the_grid() {
        let all = Glyph::all();
        assert!(all.len() > 70);
        for g in all {
            let ps = parts(g);
            assert!(!ps.is_empty(), "{g:?}");
            for p in &ps {
                let cmds = parse(&p.d).unwrap_or_else(|| panic!("{g:?}: bad path {}", p.d));
                assert!(matches!(cmds[0], Cmd::Move(_)), "{g:?} starts with a move");
                assert!(!flatten(&cmds).is_empty(), "{g:?}: {}", p.d);
            }
            // Ink inside the grid, stroke included (a little margin).
            let (x0, y0, x1, y1) = bounds(g);
            assert!(x0 >= 1.5 && y0 >= 1.5 && x1 <= 22.5 && y1 <= 22.5, "{g:?}: {:?}", (x0, y0, x1, y1));
            // Not a speck.
            assert!(x1 - x0 >= 5.0 || y1 - y0 >= 5.0, "{g:?}");
        }
    }

    #[test]
    fn glyphs_look_balanced() {
        // Symmetric ones are centered on the grid.
        for g in [Glyph::Close, Glyph::Plus, Glyph::Info, Glyph::Globe, Glyph::Sun, Glyph::Gear, Glyph::Bluetooth, Glyph::ChevronUp, Glyph::Lock, Glyph::Dock, Glyph::TopBar, Glyph::Appearance, Glyph::Airplane, Glyph::NightLight, Glyph::Hotspot, Glyph::BellOff, Glyph::Eject, Glyph::UsbDrive, Glyph::SdCard, Glyph::Drive, Glyph::Disc, Glyph::Processor, Glyph::Graphics, Glyph::Memory] {
            let (x0, _, x1, _) = bounds(g);
            assert!(((x0 + x1) / 2.0 - 12.0).abs() < 0.3, "{g:?} centered: {x0} {x1}");
        }
        let (_, y0, _, y1) = bounds(Glyph::Wifi(4));
        assert!(((y0 + y1) / 2.0 - 12.0).abs() < 1.0, "wifi sits mid-box: {y0} {y1}");
    }

    #[test]
    fn leveled_glyphs_differ() {
        let lit = |g: Glyph| parts(g).iter().filter(|p| !p.faint).count();
        assert_eq!((0..=4).map(|n| lit(Glyph::Wifi(n))).collect::<Vec<_>>(), vec![0, 1, 2, 3, 4]);
        assert_eq!(parts(Glyph::Wifi(9)), parts(Glyph::Wifi(4)));
        assert_eq!((0..=3).map(|n| parts(Glyph::Volume(n)).len()).collect::<Vec<_>>(), vec![1, 2, 3, 4]);
        let (_, _, w0, _) = bounds(Glyph::Battery(1));
        assert!(parts(Glyph::Battery(0)).len() == 2 && parts(Glyph::Battery(4)).len() == 3 && w0 > 19.0);
        assert_ne!(parts(Glyph::Trash), parts(Glyph::TrashFull));
        assert_ne!(parts(Glyph::Pin), parts(Glyph::Pinned));
        assert_ne!(parts(Glyph::ChevronLeft), parts(Glyph::ChevronRight));
    }

    #[test]
    fn settings_glyphs() {
        // Dock and TopBar: the same screen, the mark at the bottom or the
        // top, the same distance in from the frame.
        let mark = |g: Glyph| bounds_of(&parts(g)[1]);
        let (screen, dock, bar) = (bounds_of(&parts(Glyph::Dock)[0]), mark(Glyph::Dock), mark(Glyph::TopBar));
        assert_eq!(parts(Glyph::Dock)[0], parts(Glyph::TopBar)[0]);
        assert!(dock.1 > 12.0 && bar.3 < 12.0, "dock low, bar high");
        assert!(((screen.3 - dock.3) - (bar.1 - screen.1)).abs() < 1e-3, "{screen:?} {dock:?} {bar:?}");
        // Appearance: the filled half is the right one, out to the ring.
        let half = bounds_of(&parts(Glyph::Appearance)[1]);
        assert!((half.0 - 12.0).abs() < 1e-3 && (half.2 - 21.0).abs() < 0.1 && (half.1 - 3.0).abs() < 1e-3 && (half.3 - 21.0).abs() < 1e-3, "{half:?}");
        // Undo: the head points to the left and the whole sits mid-box.
        let (x0, y0, x1, y1) = bounds(Glyph::Undo);
        assert!(bounds_of(&parts(Glyph::Undo)[0]).0 == x0, "head on the left");
        assert!(((x0 + x1) / 2.0 - 12.0).abs() < 0.3 && ((y0 + y1) / 2.0 - 12.0).abs() < 0.3, "{:?}", (x0, y0, x1, y1));
    }

    fn bounds_of(p: &Part) -> (f32, f32, f32, f32) {
        let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
        for (pts, _) in flatten(&parse(&p.d).unwrap()) {
            for (x, y) in pts {
                (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
            }
        }
        (x0, y0, x1, y1)
    }

    #[test]
    fn path_parsing() {
        assert_eq!(parse("M1 2L3 4Z"), Some(vec![Cmd::Move((1.0, 2.0)), Cmd::Line((3.0, 4.0)), Cmd::Close]));
        // Relative, implicit repeats, compact numbers.
        assert_eq!(
            parse("m1 1 2 0h-1v.5-.5"),
            Some(vec![Cmd::Move((1.0, 1.0)), Cmd::Line((3.0, 1.0)), Cmd::Line((2.0, 1.0)), Cmd::Line((2.0, 1.5)), Cmd::Line((2.0, 1.0))])
        );
        assert_eq!(parse("M0 0a2 2 0 0 1 2 2"), Some(vec![Cmd::Move((0.0, 0.0)), Cmd::Arc { to: (2.0, 2.0), r: 2.0, large: false, sweep: true }]));
        assert_eq!(parse("M0 0c1 1 2 2 3 3"), Some(vec![Cmd::Move((0.0, 0.0)), Cmd::Cubic((1.0, 1.0), (2.0, 2.0), (3.0, 3.0))]));
        assert_eq!(parse("1 2"), None);
        assert_eq!(parse("M1"), None);
        assert_eq!(parse("Q1 2 3 4"), None);
        assert_eq!(parse(""), None);
    }

    #[test]
    fn arcs_find_their_circle() {
        // Clockwise half circle from left to right goes over the top.
        let (c, r, a0, da) = arc_center((8.0, 7.5), (16.0, 7.5), 4.0, false, true).unwrap();
        assert!((c.0 - 12.0).abs() < 1e-4 && (c.1 - 7.5).abs() < 1e-4 && (r - 4.0).abs() < 1e-4);
        let mid = a0 + da / 2.0;
        assert!((c.1 + r * mid.sin() - 3.5).abs() < 1e-3, "over the top");
        // Quarter arc, small vs large.
        let (c, _, _, da) = arc_center((10.0, 0.0), (0.0, 10.0), 10.0, false, true).unwrap();
        assert!((c.0).abs() < 1e-3 && (c.1).abs() < 1e-3 && (da - std::f32::consts::FRAC_PI_2).abs() < 1e-3);
        let (c, _, _, da) = arc_center((10.0, 0.0), (0.0, 10.0), 10.0, true, true).unwrap();
        assert!((c.0 - 10.0).abs() < 1e-3 && (c.1 - 10.0).abs() < 1e-3 && (da - 1.5 * std::f32::consts::PI).abs() < 1e-3);
        // Too small a radius grows to the chord.
        let (_, r, _, _) = arc_center((0.0, 0.0), (10.0, 0.0), 1.0, false, true).unwrap();
        assert!((r - 5.0).abs() < 1e-4);
        assert!(arc_center((1.0, 1.0), (1.0, 1.0), 3.0, false, true).is_none());
        // A full circle path closes on itself.
        let pts = &flatten(&parse(&circle(12.0, 12.0, 5.0)).unwrap())[0].0;
        assert!(pts.iter().all(|p| ((p.0 - 12.0).hypot(p.1 - 12.0) - 5.0).abs() < 1e-3));
    }

    #[test]
    fn an_em_holds_the_ink() {
        // The ink (stroke included) of every glyph drawn at an em stays in
        // the em's square: call sites size surfaces by it.
        for g in Glyph::all() {
            let (x0, y0, x1, y1) = bounds(g);
            let half = STROKE / 2.0;
            let to_em = |v: f32| (v - 12.0) * EM / GRID + 0.5;
            assert!(to_em(x0 - half) >= -0.01 && to_em(x1 + half) <= 1.01, "{g:?} x");
            assert!(to_em(y0 - half) >= -0.01 && to_em(y1 + half) <= 1.01, "{g:?} y");
        }
    }

    fn inside(b: (f32, f32, f32, f32), l: f32, t: f32, r: f32, bt: f32) -> bool {
        b.0 >= l - 1e-3 && b.1 >= t - 1e-3 && b.2 <= r + 1e-3 && b.3 <= bt + 1e-3
    }

    #[test]
    fn glyph_surfaces_hold_the_snapped_ink() {
        // Review of 2772b9e, 4: the preview's Move button (Monitor at 12
        // DIP, 100 %) lost 0.4 px of its right stroke on a 12 px surface.
        let (fr, pad) = padded(Glyph::Monitor, 12.0, 12.0);
        assert!(pad >= 1.0 && inside(ink_bounds(Glyph::Monitor, &fr), 0.0, 0.0, 12.0 + 2.0 * pad, 12.0 + 2.0 * pad));
        for g in Glyph::all() {
            for em in [10.0f32, 12.0, 14.0, 16.0] {
                for scale in [1.0f32, 1.25, 1.5, 1.75, 2.0] {
                    // Gfx::glyph: an em-sized square, padded.
                    let side = (em * scale).round();
                    let (fr, pad) = padded(g, side, side);
                    let full = side + 2.0 * pad;
                    assert!(pad <= 2.0, "{g:?} {em} {scale}: pad {pad}");
                    assert!(inside(ink_bounds(g, &fr), 0.0, 0.0, full, full), "{g:?} {em} {scale}");
                    // ... which is where draw() puts it on that surface.
                    let Place::Device(d) = placement(&Matrix3x2::identity(), 96.0, pad + side / 2.0, pad + side / 2.0, side, 1.0) else { panic!() };
                    assert!((d.x - fr.x).abs() < 1e-3 && (d.y - fr.y).abs() < 1e-3 && d.k == fr.k && d.stroke == fr.stroke, "{g:?} {em} {scale}");
                    // Fixed boxes (top bar cell 16, island lead 16): fitted,
                    // barely smaller than asked.
                    let bx = (16.0 * scale).round();
                    let f = fitted(g, 3.0, 5.0, bx, em * scale);
                    assert!(inside(ink_bounds(g, &f), 3.0, 5.0, 3.0 + bx, 5.0 + bx), "{g:?} {em} {scale} fitted");
                    assert!(f.k >= em * scale * EM / GRID * 0.9, "{g:?} {em} {scale}: shrunk to {}", f.k);
                }
            }
        }
    }

    #[test]
    fn draw_places_in_device_pixels() {
        // A mirrored panel at 125 % with a fractional offset, 120 DPI.
        let m = Matrix3x2 { M11: -1.25, M12: 0.0, M21: 0.0, M22: 1.25, M31: 200.3, M32: 10.7 };
        let Place::Device(f) = placement(&m, 120.0, 40.0, 20.0, 12.0, 1.0) else { panic!("axis aligned") };
        let d = 120.0 / 96.0;
        let size = 12.0 * EM * 1.25 * d;
        assert!((f.k - size / GRID).abs() < 1e-4);
        assert_eq!(f.stroke, stroke_px(size, 1.0));
        let c = f.map((12.0, 12.0));
        let want = ((-1.25 * 40.0 + 200.3) * d, (1.25 * 20.0 + 10.7) * d);
        assert!((c.0 - want.0).abs() <= 0.5 + 1e-3 && (c.1 - want.1).abs() <= 0.5 + 1e-3, "{c:?} near {want:?}");
        if f.stroke == f.stroke.round() && f.stroke as i64 % 2 == 1 {
            assert_eq!(((c.0 * 2.0).round() as i64).rem_euclid(2), 1, "on a pixel center");
        }
        // Rotated: not snapped, in the panel's units.
        let r = Matrix3x2 { M11: 0.0, M12: 1.0, M21: -1.0, M22: 0.0, M31: 0.0, M32: 0.0 };
        assert!(matches!(placement(&r, 96.0, 10.0, 10.0, 12.0, 1.0), Place::Local(_)));
    }

    /// `g` drawn by Direct2D (WARP) in `color` on an `n` px transparent
    /// target: its BGRA pixels.
    fn d2d_pixels(g: Glyph, n: u32, size: f32, color: D2D1_COLOR_F) -> Vec<u8> {
        use windows::core::Interface;
        use windows::Win32::Foundation::HMODULE;
        use windows::Win32::Graphics::Direct2D::Common::{D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_PIXEL_FORMAT, D2D_SIZE_U};
        use windows::Win32::Graphics::Direct2D::{
            D2D1CreateFactory, ID2D1Factory1, D2D1_BITMAP_OPTIONS_CANNOT_DRAW, D2D1_BITMAP_OPTIONS_CPU_READ, D2D1_BITMAP_OPTIONS_TARGET, D2D1_BITMAP_PROPERTIES1,
            D2D1_DEVICE_CONTEXT_OPTIONS_NONE, D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_MAP_OPTIONS_READ,
        };
        use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_WARP;
        use windows::Win32::Graphics::Direct3D11::{D3D11CreateDevice, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION};
        use windows::Win32::Graphics::Dxgi::{Common::DXGI_FORMAT_B8G8R8A8_UNORM, IDXGIDevice};
        unsafe {
            let mut dev = None;
            D3D11CreateDevice(None, D3D_DRIVER_TYPE_WARP, HMODULE::default(), D3D11_CREATE_DEVICE_BGRA_SUPPORT, None, D3D11_SDK_VERSION, Some(&mut dev), None, None).unwrap();
            let dxgi: IDXGIDevice = dev.unwrap().cast().unwrap();
            let f: ID2D1Factory1 = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None).unwrap();
            let dc = f.CreateDevice(&dxgi).unwrap().CreateDeviceContext(D2D1_DEVICE_CONTEXT_OPTIONS_NONE).unwrap();
            let pf = D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED };
            let size_u = D2D_SIZE_U { width: n, height: n };
            let props = |o| D2D1_BITMAP_PROPERTIES1 { pixelFormat: pf, dpiX: 96.0, dpiY: 96.0, bitmapOptions: o, ..Default::default() };
            let target = dc.CreateBitmap(size_u, None, 0, &props(D2D1_BITMAP_OPTIONS_TARGET)).unwrap();
            dc.SetTarget(&target);
            dc.BeginDraw();
            dc.Clear(Some(&D2D1_COLOR_F { r: 0.0, g: 0.0, b: 0.0, a: 0.0 }));
            let half = n as f32 / 2.0;
            draw(&dc.cast().unwrap(), g, half, half, size, color).unwrap();
            dc.EndDraw(None, None).unwrap();
            let read = dc.CreateBitmap(size_u, None, 0, &props(D2D1_BITMAP_OPTIONS_CPU_READ | D2D1_BITMAP_OPTIONS_CANNOT_DRAW)).unwrap();
            read.CopyFromBitmap(None, &target, None).unwrap();
            let m = read.Map(D2D1_MAP_OPTIONS_READ).unwrap();
            let mut out = Vec::with_capacity((n * n * 4) as usize);
            for y in 0..n {
                out.extend_from_slice(std::slice::from_raw_parts(m.bits.add((y * m.pitch) as usize), (n * 4) as usize));
            }
            read.Unmap().unwrap();
            out
        }
    }

    #[test]
    fn translucent_glyphs_show_no_overlaps() {
        // The lock's shackle meets its body: drawn at half alpha, no pixel is
        // more opaque than half (one layer), and opaque it is solid.
        let max_a = |px: &[u8]| px.chunks(4).map(|p| p[3]).max().unwrap_or(0);
        let white = D2D1_COLOR_F { r: 1.0, g: 1.0, b: 1.0, a: 1.0 };
        for g in [Glyph::Lock, Glyph::Wifi(4), Glyph::Calendar] {
            let half = d2d_pixels(g, 48, 24.0, D2D1_COLOR_F { a: 0.5, ..white });
            let a = max_a(&half);
            assert!((120..=129).contains(&a), "{g:?}: {a}");
            assert_eq!(max_a(&d2d_pixels(g, 48, 24.0, white)), 255, "{g:?}");
        }
    }

    #[test]
    fn stroke_scales_and_snaps() {
        assert_eq!(stroke_px(16.0, 1.0), 1.0);
        assert_eq!(stroke_px(20.0, 1.0), 1.0);
        assert_eq!(stroke_px(12.0, 1.0), 1.0, "never thinner than a pixel");
        assert_eq!(stroke_px(24.0, 1.0), 2.0, "1.5 snapped");
        assert_eq!(stroke_px(32.0, 1.0), 2.0);
        assert_eq!(stroke_px(48.0, 1.0), 3.0);
        assert_eq!(stroke_px(44.0, 1.0), 2.75, "past the limit: exact");
        assert_eq!(stroke_px(16.0, 2.0), 2.0);
        // A 16 DIP glyph (em x 1.2) at every display scale up to 200 %: a
        // whole number of pixels, so it never blurs.
        for scale in [1.0, 1.25, 1.5, 1.75, 2.0] {
            let px = 16.0 * EM * scale;
            let st = stroke_px(px, 1.0);
            assert!(px <= SNAP_MAX_PX && st == st.round() && st >= 1.0, "{scale}: {px} px, {st}");
        }
    }

    #[test]
    fn frames_land_on_pixels() {
        // Odd whole stroke: the grid center on a pixel center.
        let f = frame(10.2, 7.9, 16.0, 1.0);
        let c = f.map((12.0, 12.0));
        assert_eq!((c.0, c.1, f.stroke), (10.5, 7.5, 1.0));
        assert!((f.k - 16.0 / 24.0).abs() < 1e-6);
        // Even whole stroke: on a pixel corner.
        let f = frame(10.2, 7.6, 32.0, 1.0);
        assert_eq!(f.map((12.0, 12.0)), (10.0, 8.0));
        // Fractional stroke (past the snap limit): the box's corner on
        // whole pixels.
        let f = frame(12.3, 12.3, 44.0, 1.0);
        assert_eq!((f.x, f.y, f.stroke), (-10.0, -10.0, 2.75));
    }

    fn ink(px: &[u8]) -> u32 {
        px.chunks_exact(4).map(|p| p[3] as u32).sum()
    }

    #[test]
    fn software_raster_draws_every_glyph_inside_its_box() {
        let n = 32u32;
        for g in Glyph::all() {
            let mut px = vec![0u8; (n * n * 4) as usize];
            let fr = frame(16.0, 16.0, 24.0, 1.0);
            raster_into(&mut px, n, g, &fr, [1.0, 1.0, 1.0, 1.0]);
            assert!(ink(&px) > 255 * 6, "{g:?} has ink");
            // Nothing outside the 24 px box (plus the stroke).
            for y in 0..n {
                for x in 0..n {
                    if !(3..29).contains(&x) || !(3..29).contains(&y) {
                        assert_eq!(px[((y * n + x) * 4 + 3) as usize], 0, "{g:?} at {x},{y}");
                    }
                }
            }
            // Premultiplied: no channel above alpha.
            assert!(px.chunks_exact(4).all(|p| p[0] <= p[3] && p[1] <= p[3] && p[2] <= p[3]), "{g:?}");
        }
    }

    #[test]
    fn software_raster_shapes() {
        let n = 24u32;
        let fr = frame(12.0, 12.0, 24.0, 1.0);
        let draw = |g: Glyph| {
            let mut px = vec![0u8; (n * n * 4) as usize];
            raster_into(&mut px, n, g, &fr, [1.0, 1.0, 1.0, 1.0]);
            px
        };
        let a = |px: &[u8], x: u32, y: u32| px[((y * n + x) * 4 + 3) as usize];
        // Filled marks are solid inside, outlines hollow.
        let pause = draw(Glyph::Pause);
        assert_eq!(a(&pause, 8, 12), 255);
        assert_eq!(a(&pause, 12, 12), 0);
        let search = draw(Glyph::Search);
        assert_eq!(a(&search, 10, 10), 0, "the lens is hollow");
        // Fuller levels have more ink; faint parts some.
        assert!(ink(&draw(Glyph::Wifi(4))) > ink(&draw(Glyph::Wifi(1))));
        assert!(ink(&draw(Glyph::Wifi(0))) > 0);
        assert!(ink(&draw(Glyph::Battery(4))) > ink(&draw(Glyph::Battery(0))));
        // The X is mirror-symmetric.
        let x = draw(Glyph::Close);
        for y in 0..n {
            for xx in 0..n {
                let (l, r) = (a(&x, xx, y) as i32, a(&x, n - 1 - xx, y) as i32);
                assert!((l - r).abs() <= 2, "{xx},{y}: {l} vs {r}");
            }
        }
    }

    #[test]
    fn dock_tiles() {
        for light in [false, true] {
            let look = TileLook::new(light, None);
            let t = tile(64, Glyph::Trash, look);
            assert_eq!((t.width, t.height, t.pixels.len()), (64, 64, 64 * 64 * 4));
            assert!(crate::icons::analyze(&t).coverage >= 0.9, "full-bleed for the dock's squircle");
            assert_eq!(t.pixels[3], 0, "rounded corner: clear");
            assert_ne!(t.pixels, tile(64, Glyph::TrashFull, look).pixels);
        }
        // Dark tile with light ink, light tile with dark ink.
        let (d, l) = (TileLook::new(false, None), TileLook::new(true, None));
        assert!(d.back[0] < 0.3 && d.ink[0] > 0.7 && l.back[0] > 0.9 && l.ink[0] < 0.5);
        let hc = TileLook::new(false, Some(([0, 0, 0], [255, 255, 0])));
        assert_eq!((hc.back, hc.ink, hc.outline), ([0.0, 0.0, 0.0, 1.0], [1.0, 1.0, 0.0, 1.0], Some([1.0, 1.0, 0.0, 1.0])));
        // The line tile is neutral: no channel far from the others.
        let t = line_tile(48, Glyph::Folder, d);
        assert!(t.pixels.chunks_exact(4).all(|p| (p[0] as i32 - p[2] as i32).abs() <= 8));
        // The drawn folder is pale blue (design C), its plate grey.
        let t = tile(48, Glyph::Folder, d);
        let at = |x: u32, y: u32| &t.pixels[((y * 48 + x) * 4) as usize..][..4];
        assert!(at(24, 30)[0] as i32 > at(24, 30)[2] as i32 + 20, "{:?}", at(24, 30));
        assert!((at(24, 3)[0] as i32 - at(24, 3)[2] as i32).abs() <= 8, "{:?}", at(24, 3));
    }
}
