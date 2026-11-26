//! The quick settings panel under the top bar. The main view contains media,
//! a tile grid, sliders and a footer. Wi-Fi, Bluetooth, accessibility,
//! Project and Sound share the same in-panel navigation and focus.
//!
//! Slow Windows APIs run on workers attached to the lifetime of the panel.
//! The panel paints one Direct2D surface plus Composition visuals for its
//! animated parts. The last paint supplies input and UI Automation geometry.

use std::cell::RefCell;
use std::hash::{Hash, Hasher};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use windows_future::{AsyncOperationCompletedHandler, AsyncStatus, IAsyncOperation};

use windows::core::{w, Interface, Result, BOOL};
use windows::Devices::Radios::{Radio, RadioAccessStatus, RadioKind, RadioState};
use windows::Foundation::TypedEventHandler;
use windows::Networking::Connectivity::NetworkInformation;
use windows::Networking::NetworkOperators::{
    NetworkOperatorTetheringManager as Tethering, TetheringCapability, TetheringOperationStatus, TetheringOperationalState,
};
use windows::Media::Control::{
    GlobalSystemMediaTransportControlsSession as Session, GlobalSystemMediaTransportControlsSessionManager as SessionManager,
    GlobalSystemMediaTransportControlsSessionPlaybackStatus as PlaybackStatus,
};
use super::island::model::Timeline;
use windows::Storage::Streams::{DataReader, IRandomAccessStreamReference, InputStreamOptions};
use windows::UI::Composition::Desktop::DesktopWindowTarget;
use windows::UI::Composition::{
    CompositionColorBrush, CompositionColorSpace, CompositionEasingFunction, CompositionObject, CompositionPropertySet, CompositionStretch, CompositionSurfaceBrush,
    Compositor, ContainerVisual, ShapeVisual, SpriteVisual,
};
use crate::motion::SpringRun;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Direct2D::Common::{D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_PIXEL_FORMAT, D2D_RECT_F, D2D_SIZE_U};
use windows::Win32::Graphics::Direct2D::{
    ID2D1Bitmap1, ID2D1Device, ID2D1DeviceContext, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE, D2D1_BITMAP_OPTIONS_NONE, D2D1_BITMAP_PROPERTIES1,
    D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_ELLIPSE, D2D1_ROUNDED_RECT,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_WICPixelFormat32bppPBGRA, IWICImagingFactory, WICBitmapDitherTypeNone,
    WICBitmapInterpolationModeHighQualityCubic, WICBitmapPaletteTypeCustom, WICDecodeMetadataCacheOnDemand, WICRect,
};
use windows::Win32::Graphics::DirectWrite::{IDWriteTextLayout, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_WEIGHT_SEMI_BOLD};
use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_USE_HOSTBACKDROPBRUSH};
use windows::Win32::Graphics::Gdi::{ClientToScreen, GetMonitorInfoW, MonitorFromPoint, MONITORINFO, MONITOR_DEFAULTTONEAREST};
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::WinRT::Composition::ICompositorDesktopInterop;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    EnableWindow, GetCapture, GetKeyState, ReleaseCapture, SetCapture, SetFocus, VK_BACK, VK_DOWN, VK_END, VK_ESCAPE, VK_HOME, VK_LEFT,
    VK_NEXT, VK_PRIOR, VK_RETURN, VK_RIGHT, VK_SHIFT, VK_SPACE, VK_TAB, VK_UP,
};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows_numerics::{Matrix3x2, Vector2, Vector3};

use crate::a11y::{self, Act, Node, Role, Tree};
use crate::audio::devices::{self, Endpoint, Flow};
use crate::gfx::Gfx;
use crate::glyphs::Glyph;
use crate::i18n::{t, tf};
use crate::theme::{Elevation, Rgba, Theme};

mod accessibility;
mod awake;
mod bluetooth;
mod bright;
mod audio;
mod dimmer;
mod input;
mod jobs;
mod layout;
mod media;
mod motion;
mod night_light;
mod paint;
mod parts;
mod quick;
mod radio;
mod reader;
mod system_tiles;

use accessibility::*;
use audio::*;
use bluetooth::*;
use dimmer::*;
use input::TileClicks;
use jobs::*;
use layout::*;
use media::*;
use motion::*;
use quick::*;
use radio::*;
use system_tiles::*;
pub(super) use media::MediaWatch;
pub(super) use motion::{panel_extremes, readable_dim};

/// A worker changed something: redraw from the shared state.
const WM_CC_REMOTE: u32 = WM_APP + 1;
/// One-shot: a "couldn't change" line under a tile has had its time.
const NOTE_TIMER: usize = 1;
/// One-shot: a removable device's note ("Safe to remove", a refusal) has
/// had its time.
const EJECT_TIMER: usize = 2;
/// One-shot: the hovered control's tooltip has waited its time.
const TIP_TIMER: usize = 3;
/// One-shot: the time played shows its next second.
const TICK_TIMER: usize = 4;
/// One-shot: the closing panel has faded; the window goes.
const CLOSE_TIMER: usize = 5;
/// How long a tile says a switch did not go through.
const FAIL_SHOW: std::time::Duration = std::time::Duration::from_secs(3);

/// Pure: the short line a switch tile shows about its last click: while
/// Windows is still on it, "Switching…"; for FAIL_SHOW after it refused
/// (the tile is back where Windows has it), "Couldn't change". None: the
/// tile's ordinary status.
fn switch_note(busy: bool, failed: Option<std::time::Instant>, now: std::time::Instant) -> Option<&'static str> {
    if busy {
        Some("control.audio.switching")
    } else if failed.is_some_and(|t| now.saturating_duration_since(t) < FAIL_SHOW) {
        Some("control.switch_failed")
    } else {
        None
    }
}

// ---- metrics (DIP, on the 4 DIP grid: theme::metrics) ------------------------
// The panel is laid out in DIP at the default text size and drawn at
// `size` (panel_size: BASE_SIZE, grown or shrunk whole with the user's text
// size, and shrunk whole on a screen too small for it). The grid and its
// shapes: layout.rs. Corners are concentric: the panel 20, PAD 8 in, the
// cards and tiles 12, CONN_PAD 4 in, the rows 8; round things (the chips,
// the glyph buttons, the sliders) are capsules. Text sizes are the type
// ramp's (theme::ramp) and kept so whatever the size.
use crate::theme::metrics::space::{S2, S3, S4, S5};
/// The panel's size (DIP per layout DIP) at the default text size.
const BASE_SIZE: f32 = 1.0;
/// The panel at rest (the grid, with what is playing) takes at most this
/// share of its work area's height and width; a list taller than it may
/// grow it to the work area itself, never past it.
const REST_SHARE_H: f32 = 0.75;
const REST_SHARE_W: f32 = 0.5;
/// The smallest the panel is shrunk to keep within REST_SHARE (a cell 48
/// instead of 72; only a work area smaller still takes it under).
const MIN_SIZE: f32 = 2.0 / 3.0;

const COLS: usize = 4;
const UNIT: f32 = 72.0;
const GAP: f32 = 8.0;
const PAD: f32 = 8.0;
const W: f32 = PAD * 2.0 + UNIT * COLS as f32 + GAP * (COLS - 1) as f32;
const TILE_CORNER: f32 = 4.0;
const PANEL_CORNER: f32 = 8.0;
const CONN_PAD: f32 = S2;
const CHIP: f32 = 28.0;
const CHIP_GLYPH: f32 = S5;
const MARK_GLYPH: f32 = S4;
const ROW_IN: f32 = S3;
/// A tile's glyph (1 x 1, Project's, a strip's); a 1 x 1 tile's "opens
/// elsewhere" mark (EXT_IN from its top and end) and a row's.
const TILE_GLYPH: f32 = 20.0;
const EXT_MARK: f32 = 10.0;
const EXT_IN: f32 = S3;
/// A strip's content in from its start (and a slider's end), and the media
/// and removable strips' (and Project's tile's end).
const STRIP_IN: f32 = S5;
const MEDIA_IN: f32 = S4;
/// The thick sliders (CAPSULE tall, CAP_GLYPH glyph CAP_GLYPH_IN in), the
/// volume's mute button (MUTE round) and the output button beside it
/// (ROUND_L round).
const CAPSULE: f32 = 40.0;
const CAP_GLYPH: f32 = S5;
const MUTE: f32 = 32.0;
const ROUND_L: f32 = 40.0;
/// A thick slider grows this much across while dragged.
const HELD_GROW: f32 = 1.06;
/// What is playing: the media buttons (28, play / pause 32, S2 apart;
/// glyphs 16 and 20), the title's and artist's lines and the position bar
/// BAR_GAP under them.
const MEDIA_BTN: f32 = 28.0;
const PLAY_BTN: f32 = 32.0;
const MEDIA_BTN_GAP: f32 = S2;
const MEDIA_GLYPH: f32 = S5;
const PLAY_GLYPH: f32 = 20.0;
const TITLE_H: f32 = 18.0;
const ARTIST_H: f32 = S5;
const BAR_GAP: f32 = 6.0;
const TRACK_H: f32 = S2;
/// A removable device's eject button.
const EJECT_BTN: f32 = 32.0;
/// A list: its header (SUB_HEAD_H: the back button ROUND_S round and the
/// title), its cards' padding (LIST_PAD: the rows' corners are concentric
/// with the card's), headings (HEAD_H), rows (LINE_H, an app's
/// APP_ROW_H), a row's glyph and its note at its widest.
const ROUND_S: f32 = 28.0;
/// How far a focus ring reaches out of what it rings (its gap and width).
const RING_OUT: f32 = crate::theme::metrics::size::FOCUS_GAP + crate::theme::metrics::size::FOCUS_W;
/// A list's header: the back button with its focus ring's room above and
/// under it, so the ring keeps out of the panel's padding (PAD) like every
/// other control's (final audit 5: it all but touched the plate's corner).
const SUB_HEAD_H: f32 = ROUND_S + RING_OUT * 2.0;
const LIST_PAD: f32 = S2;
const HEAD_H: f32 = 24.0;
const LINE_H: f32 = crate::theme::metrics::size::ROW_M;
const APP_ROW_H: f32 = crate::theme::metrics::size::ROW_L;
const ROW_GLYPH: f32 = S5;
const NOTE_MAX: f32 = 80.0;
/// An app's row: its name's widest, its percent's room at the end; the
/// slim slider's track (TRACK_H, a capsule) and its knob (16, x 1.12
/// held; E1).
const APP_NAME_W: f32 = 84.0;
const APP_VALUE_W: f32 = 36.0;
const KNOB: f32 = crate::theme::metrics::size::KNOB;
const KNOB_HELD: f32 = crate::theme::metrics::size::KNOB_HELD;
/// Room around the panel for its shadow.
const SHADOW_M: f32 = crate::theme::metrics::SHADOW_M;
/// The type ramp (theme::ramp): names and titles Body Strong 13, rows
/// Body 13, values and headings Footnote 12, an app's percent Caption 11,
/// a list's title Headline 15.
const T_NAME: f32 = crate::theme::ramp::BODY_STRONG.size;
const T_BODY: f32 = crate::theme::ramp::BODY.size;
const T_SMALL: f32 = crate::theme::ramp::FOOTNOTE.size;
const T_CAPTION: f32 = crate::theme::ramp::CAPTION.size;
const T_HEAD: f32 = crate::theme::ramp::HEADLINE.size;

/// Which of the theme's radii a shape takes (Panel::radius).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Radius {
    /// Cards, tiles and strips (12: the panel's 20 less PAD).
    Tile,
    /// A row inside a card (concentric: the card's less its padding).
    Row,
    /// Album art (content: 8).
    Control,
}

// Glyphs (crate::glyphs).
const G_WIFI: Glyph = Glyph::Wifi(4);
const G_BLUETOOTH: Glyph = Glyph::Bluetooth;
const G_MOON: Glyph = Glyph::Moon;
const G_CAST: Glyph = Glyph::Cast;
const G_SUN: Glyph = Glyph::Sun;
const G_PREV: Glyph = Glyph::Prev;
const G_NEXT: Glyph = Glyph::Next;
const G_PLAY: Glyph = Glyph::Play;
const G_PAUSE: Glyph = Glyph::Pause;
const G_MUSIC: Glyph = Glyph::Music;
const G_CHECK: Glyph = Glyph::Check;
const G_INFO: Glyph = Glyph::Info;
const G_EXTERNAL: Glyph = Glyph::External;
const G_EJECT: Glyph = Glyph::Eject;

/// Album art in the Now Playing card (DIP; corners the theme's control
/// radius, Panel::radius), and its decoded size: square, at most ART_PX px
/// a side (sharp past 200 % scale).
const ART_DIP: f32 = 48.0;
const ART_PX: u32 = 128;
/// Thumbnails bigger than this (encoded) are not read.
const ART_MAX_BYTES: u64 = 16 << 20;
/// Images with more pixels (4096 x 4096) are not decoded at all.
const ART_MAX_SRC_PIXELS: u64 = 4096 * 4096;
/// Nor strips longer than this many times their width.
const ART_MAX_ASPECT: u64 = 8;
/// Reading a thumbnail: chunk size and the whole read's deadline.
const ART_CHUNK: u32 = 64 * 1024;
const ART_READ_MS: u64 = 3000;
/// Tries per track when reading fails for a passing reason.
const ART_RETRIES: u8 = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Hit {
    WifiToggle,
    /// The Wi-Fi row: opens the network list.
    WifiName,
    BtToggle,
    /// The Bluetooth row: opens the device list.
    BtName,
    BtDevice(u64),
    BluetoothSettings,
    WifiRow(u64),
    WifiDisconnect(u64),
    WifiRefresh,
    WifiLocation,
    WifiSettings,
    ScrollList(bool),
    AccessList,
    Access(Access),
    AccessibilitySettings,
    Studio,
    Dark,
    Cast,
    Brightness,
    /// The volume slider's glyph: mutes and unmutes (a drag from it moves
    /// the volume instead).
    Mute,
    Volume,
    /// Project's tile: goes into the list of display modes.
    Project,
    /// The output button beside the volume: goes into the sound list.
    Outputs,
    /// A list's back button (and Esc): back to the grid.
    Back,
    /// A device in the sound list, by its endpoint ID's hash (device_key):
    /// focus and press stay on that device when the list changes.
    Device(Flow, u64),
    /// An app's volume slider in the sound list, by its row's key
    /// (mix_key: its process and sessions).
    App(u64),
    Prev,
    PlayPause,
    Next,
    /// Under a device list after a failed switch: Windows' Sound page.
    SoundSettings,
    /// A round toggle that is not a radio or dark mode.
    Quick(Quick),
    /// A display mode in Project's list.
    Mode(Topology),
    /// A removable device's eject button, by its connection's hash
    /// (removable_key).
    Eject(u64),
}

impl Hit {
    fn tile(self) -> bool {
        matches!(self, Self::WifiToggle | Self::WifiName | Self::BtToggle | Self::BtName
            | Self::Studio | Self::AccessList | Self::Quick(_) | Self::Project | Self::Cast | Self::Dark)
    }

    fn slider(self) -> bool {
        matches!(self, Hit::Brightness | Hit::Volume | Hit::App(_))
    }
}

/// How the panel closed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// By the user (Esc, outside click, the button again, or an action).
    Closed,
    /// Something else took over (another window, capture lost, cancel()).
    Lost,
}

/// Run once the panel is gone (the keys must reach the shell, not us).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum After {
    Cast,
    Open(&'static str),
    Studio,
    /// Switch the displays to this mode (on a thread of its own: the mode
    /// change takes a moment and may move this panel's display).
    Project(Topology),
}

// ---- quick tiles ------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Quick {
    Airplane,
    NightLight,
    Dnd,
    EnergySaver,
    Hotspot,
    /// Keeps the display on and the PC awake (awake.rs).
    Awake,
}

impl Quick {
    fn system_setting(self) -> Option<Setting> {
        match self {
            Self::Airplane => Some(Setting::Airplane),
            Self::NightLight => Some(Setting::Night),
            Self::Dnd => Some(Setting::Dnd),
            _ => None,
        }
    }
}

/// Win+P's display modes (DISPLAYCONFIG_TOPOLOGY_*).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Topology {
    /// PC screen only.
    Internal,
    /// Duplicate.
    Clone,
    Extend,
    /// Second screen only.
    External,
}

impl Topology {
    const ALL: [Topology; 4] = [Topology::Internal, Topology::Clone, Topology::Extend, Topology::External];


    /// Its SDC_TOPOLOGY_* flag (the same bit as its topology id).
    fn sdc(self) -> u32 {
        match self {
            Topology::Internal => 1,
            Topology::Clone => 2,
            Topology::Extend => 4,
            Topology::External => 8,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Topology::Internal => "control.project.internal",
            Topology::Clone => "control.project.clone",
            Topology::Extend => "control.project.extend",
            Topology::External => "control.project.external",
        }
    }
}

/// What the displays offer Project's card.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
enum Displays {
    /// Still asking Windows.
    #[default]
    Loading,
    /// One display connected (or Windows would not say): nothing to project
    /// to, no card. A display plugged in while the panel is open brings it.
    Single,
    /// Two or more connected; the current mode, if Windows said.
    Many(Option<Topology>),
}

/// Pure: Project's card's state from the number of connected displays and
/// the mode the active paths show (None: not one of Win+P's, or unknown).
fn displays_of(connected: usize, current: Option<Topology>) -> Displays {
    if connected < 2 {
        Displays::Single
    } else {
        Displays::Many(current)
    }
}

/// Pure: the mode the displays are in now, from the active paths (each
/// path's source as a number, and whether its target is the PC's own
/// screen) and whether the PC's own screen is connected at all. Not the
/// stored configuration (QDC_DATABASE_CURRENT): an app may have applied
/// another one for now. One path: PC screen only on the internal screen,
/// second screen only on another while the internal one is connected (a
/// desktop has no "PC screen": unknown). Several: one source for all is
/// Duplicate, one source each is Extend; anything mixed is unknown.
fn active_topology(active: &[(u64, bool)], internal_connected: bool) -> Option<Topology> {
    match active {
        [] => None,
        [(_, true)] => Some(Topology::Internal),
        [(_, false)] => internal_connected.then_some(Topology::External),
        _ => {
            let mut sources: Vec<u64> = active.iter().map(|a| a.0).collect();
            sources.sort_unstable();
            sources.dedup();
            match sources.len() {
                1 => Some(Topology::Clone),
                n if n == active.len() => Some(Topology::Extend),
                _ => None,
            }
        }
    }
}

/// Pure: whether picking mode `m` switches the displays: always, unless
/// the active paths show exactly that mode.
fn mode_switches(now: Displays, m: Topology) -> bool {
    now != Displays::Many(Some(m))
}

/// A hotspot Windows is still switching is read again this often, for at
/// most SETTLE_FOR, and only while the panel is open.
const SETTLE_EVERY: Duration = Duration::from_millis(500);
const SETTLE_FOR: Duration = Duration::from_secs(15);

/// Pure: a read of a hotspot being switched (`read`: hotspot_of's answer),
/// toward `target` (a switch of ours; None: Windows was switching it on its
/// own). None: not there yet, read again. Some((view, failed)): settled,
/// or out of time (`timed_out`: shown as Windows has it, and a switch of
/// ours that never showed is a failure).
fn settle(read: (RadioView, bool), target: Option<bool>, timed_out: bool) -> Option<(RadioView, bool)> {
    let (v, switching) = read;
    if v == RadioView::Missing {
        return Some((v, target.is_some()));
    }
    let reached = match target {
        Some(t) => !switching && switch_state(v) == Some(t),
        None => !switching,
    };
    if reached {
        Some((v, false))
    } else if timed_out {
        Some((v, target.is_some()))
    } else {
        None
    }
}

fn connection_rows(r: &Remote) -> Vec<Hit> {
    use Quick::*;
    let mut rows = vec![Hit::WifiToggle, Hit::BtToggle];
    rows.extend((r.radios != Some(false)).then_some(Hit::Quick(Airplane)));
    rows.extend((!matches!(r.hotspot, RadioView::Missing | RadioView::Loading)).then_some(Hit::Quick(Hotspot)));
    rows
}

fn grid_tiles(r: &Remote) -> Vec<Hit> {
    use Quick::*;
    let mut tiles = vec![Hit::Dark];
    tiles.extend((!r.system[Setting::Night as usize].state.hidden).then_some(Hit::Quick(NightLight)));
    tiles.extend([Hit::Quick(Dnd), Hit::Quick(Awake), Hit::Quick(EnergySaver), Hit::Cast]);
    tiles.extend(matches!(r.displays, Displays::Many(_)).then_some(Hit::Project));
    tiles.push(Hit::AccessList);
    tiles.extend(r.studio.is_some().then_some(Hit::Studio));
    tiles
}

fn quick_page(q: Quick, build: u32) -> Option<&'static str> {
    match q {
        Quick::Airplane => Some("ms-settings:network-airplanemode"),
        Quick::NightLight => Some("ms-settings:nightlight"),
        Quick::EnergySaver => Some("ms-settings:batterysaver"),
        Quick::Dnd => Some(if build >= 22621 { "ms-settings:notifications" } else { "ms-settings:quietmomentshome" }),
        Quick::Hotspot | Quick::Awake => None,
    }
}

fn quick_glyph(q: Quick) -> Glyph {
    match q {
        Quick::Airplane => Glyph::Airplane,
        Quick::NightLight => Glyph::NightLight,
        Quick::Dnd => Glyph::BellOff,
        Quick::EnergySaver => Glyph::Leaf,
        Quick::Hotspot => Glyph::Hotspot,
        Quick::Awake => Glyph::Cup,
    }
}

fn quick_label(q: Quick) -> &'static str {
    match q {
        Quick::Airplane => "control.airplane",
        Quick::NightLight => "control.night_light",
        Quick::Dnd => "control.dnd",
        Quick::EnergySaver => "control.energy_saver",
        Quick::Hotspot => "control.hotspot",
        Quick::Awake => "control.keep_awake",
    }
}

/// What a quick toggle shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct QuickView {
    /// The status line (a locale key; None: none).
    status: Option<&'static str>,
    /// Its disc is lit (the setting is on).
    on: bool,
    /// The state is Windows' to change: the toggle opens Settings.
    link: bool,
    /// The toggle is a switch (screen readers get a toggle).
    switch: bool,
    ready: bool,
}

/// Pure: a quick toggle's look from what the workers found.
fn quick_view(q: Quick, r: &Remote, now: std::time::Instant) -> QuickView {
    let on_off = |on: bool| Some(if on { "control.on" } else { "control.off" });
    if let Some(setting) = q.system_setting() {
        let tile = r.system[setting as usize];
        let on = tile.state.on.unwrap_or(false);
        return QuickView {
            status: switch_note(tile.busy, tile.failed, now).or(if !tile.ready {
                Some("control.loading")
            } else if tile.state.writable {
                on_off(on)
            } else {
                Some("control.in_settings")
            }),
            on,
            link: tile.ready && !tile.state.writable,
            switch: tile.ready && tile.state.writable,
            ready: tile.ready,
        };
    }
    match q {
        Quick::EnergySaver => {
            let on = r.energy.unwrap_or(false);
            QuickView { status: on_off(on), on, link: true, switch: false, ready: true }
        }
        Quick::Hotspot => {
            let on = r.hotspot.on();
            let status = switch_note(r.hotspot_busy, r.hotspot_failed, now).or(on_off(on));
            QuickView { status, on, link: false, switch: true, ready: true }
        }
        Quick::Awake => {
            let status = switch_note(false, r.awake_failed, now).or(on_off(r.awake));
            QuickView { status, on: r.awake, link: false, switch: true, ready: true }
        }
        Quick::Airplane | Quick::NightLight | Quick::Dnd => unreachable!(),
    }
}

/// Pure: Project's card's second line: the mode the displays are in
/// (None when Windows would not say).
fn project_status(d: Displays) -> Option<&'static str> {
    match d {
        Displays::Many(Some(t)) => Some(t.label()),
        _ => None,
    }
}

/// Pure: the hotspot tile from the tethering state, and whether Windows is
/// still switching it.
fn hotspot_of(state: Option<TetheringOperationalState>) -> (RadioView, bool) {
    match state {
        Some(TetheringOperationalState::On) => (RadioView::On, false),
        Some(TetheringOperationalState::Off) => (RadioView::Off, false),
        Some(TetheringOperationalState::InTransition) => (RadioView::Off, true),
        _ => (RadioView::Missing, false),
    }
}

pub struct Request<'a> {
    pub gfx: &'a Rc<Gfx>,
    pub theme: &'a Theme,
    /// The control button, screen px: the panel hangs under it, right-aligned.
    pub button: RECT,
    pub scale: f32,
    /// Output volume (percent, muted), as the bar shows it.
    pub volume: Option<(u8, bool)>,
}

// ---- what the workers report ---------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
enum RadioView {
    /// Still asking Windows.
    #[default]
    Loading,
    /// No such radio.
    Missing,
    On,
    Off,
    /// Switched off by the hardware (or a state Windows will not change);
    /// true = on.
    Locked(bool),
    /// Windows refused us access to this radio; true = on.
    Denied(bool),
}

impl RadioView {
    fn on(self) -> bool {
        matches!(self, RadioView::On | RadioView::Locked(true) | RadioView::Denied(true))
    }
}

/// Pure: why a radio cannot be switched here, in words (None: it can).
fn radio_reason(v: RadioView) -> Option<&'static str> {
    match v {
        RadioView::Missing => Some("control.radio.missing"),
        RadioView::Denied(_) => Some("control.radio.denied"),
        RadioView::Locked(_) => Some("control.radio.locked"),
        RadioView::Loading | RadioView::On | RadioView::Off => None,
    }
}

/// Pure: the Windows settings page that can do what the tile cannot
/// (access for apps, the adapter's own page).
fn radio_fix(wifi: bool, v: RadioView) -> Option<&'static str> {
    match v {
        RadioView::Denied(_) => Some("ms-settings:privacy-radios"),
        RadioView::Missing | RadioView::Locked(_) => Some(if wifi { "ms-settings:network-wifi" } else { "ms-settings:bluetooth" }),
        RadioView::Loading | RadioView::On | RadioView::Off => None,
    }
}

/// What a radio tile shows: its state, and whether Windows let us switch it.
fn view_of(state: Option<RadioState>, denied: bool) -> RadioView {
    match state {
        Some(RadioState::On) if denied => RadioView::Denied(true),
        _ if denied => RadioView::Denied(false),
        Some(RadioState::On) => RadioView::On,
        Some(RadioState::Off) => RadioView::Off,
        _ => RadioView::Locked(false),
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
enum Ssid {
    /// Not read yet, or Windows would not say (location permission).
    #[default]
    Unknown,
    None,
    Some(String),
}

fn radio_summary(r: &Remote, wifi: bool) -> (&str, usize) {
    if wifi {
        if let (true, Ssid::Some(name)) = (r.wifi.on(), &r.ssid) {
            return (name, 0);
        }
    } else if r.bt.on() {
        let mut connected = r.bt_devices.iter().filter(|device| device.connected);
        if let Some(first) = connected.next() {
            return (&first.name, connected.count());
        }
    }
    ("", 0)
}

fn wifi_network(r: &Remote, key: u64) -> Option<&super::panels::Wifi> {
    r.wifi_networks.as_ref()?.as_ref().ok()?.iter().find(|n| wifi_key(n) == key)
}

fn bluetooth_device(r: &Remote, key: u64) -> Option<&BtDevice> {
    r.bt_devices.iter().find(|d| d.key == key)
}

#[derive(Clone, Debug, PartialEq, Default)]
struct Media {
    title: String,
    artist: String,
    /// The session's app (AppUserModelID or exe name).
    app: String,
    playing: bool,
    prev: bool,
    next: bool,
    toggle: bool,
    /// None = no art (or not decodable): a placeholder tile.
    art: Option<Arc<Art>>,
    /// Where in the track it is (the Live Island's position bar).
    timeline: Option<Timeline>,
    /// This session and track (a hash of the worker's session count and
    /// the track's identity): a seek for another one is dropped.
    gen: u32,
    /// The worker's count of sessions bound so far (which session this is;
    /// a PiP's play / pause goes to its own session only).
    session: u32,
    /// Seeks the app refused (or did not answer) so far.
    refused: u32,
}

/// Decoded album art: `px` x `px`, premultiplied BGRA.
#[derive(PartialEq, Eq)]
pub(super) struct Art {
    pub(super) px: u32,
    pub(super) pixels: Vec<u8>,
}

impl std::fmt::Debug for Art {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Art({0}x{0})", self.px)
    }
}

#[derive(Clone, Debug, Default)]
struct Remote {
    system: [SystemTile; 3],
    wifi: RadioView,
    bt: RadioView,
    ssid: Ssid,
    /// None = still asking; Some(None) = nothing on the panel's display
    /// can be dimmed (no WMI panel, no monitor answering DDC/CI): the
    /// slider stays hidden.
    brightness: Option<Option<u8>>,
    /// None = no media session: the row stays hidden.
    media: Option<Media>,
    /// Dark mode as the dark-mode worker found it after request `seq`:
    /// (seq, dark). Taken by the panel.
    dark: Option<(u64, bool)>,
    /// Active output / input devices (None = still asking).
    outputs: Option<Vec<Endpoint>>,
    inputs: Option<Vec<Endpoint>>,
    /// A device switch asked of Windows and not answered yet: (direction,
    /// endpoint ID). Its row says "Switching…"; the check stays where
    /// Windows has it.
    audio_busy: Option<(Flow, String)>,
    /// Windows would not switch to this device (its name): said under the
    /// list, with a way to Windows' Sound page. The check stays on the
    /// device still in use.
    audio_fail: Option<(Flow, String)>,
    /// Counts the failures (audio_fail): each one leads into the sound
    /// list once (layout::fail_leads_in).
    audio_fail_seq: u64,
    /// A Wi-Fi / Bluetooth switch asked of Windows and not answered yet.
    radio_busy: [bool; 2],
    /// When Windows last refused (or ignored) a Wi-Fi / Bluetooth switch.
    radio_failed: [Option<std::time::Instant>; 2],
    /// Any radio at all (Wi-Fi, Bluetooth, mobile broadband...): None =
    /// still asking. None of them: no airplane mode tile.
    radios: Option<bool>,
    /// Current power-saving status when available.
    energy: Option<bool>,
    /// Mobile hotspot: Loading, Missing (no connection Windows can share:
    /// no tile), On or Off.
    hotspot: RadioView,
    /// A hotspot switch asked of Windows and not answered yet, and when
    /// Windows last refused one.
    hotspot_busy: bool,
    hotspot_failed: Option<std::time::Instant>,
    /// The displays, for Project's card.
    displays: Displays,
    studio: Option<String>,
    // Battery presence selects the Windows energy settings page.
    battery: Option<(u8, bool)>,
    bt_devices: Vec<BtDevice>,
    bt_audio_ready: bool,
    bt_busy: std::collections::HashSet<u64>,
    bt_failed: std::collections::HashMap<u64, Instant>,
    wifi_networks: Option<std::result::Result<Vec<super::panels::Wifi>, super::panels::WifiError>>,
    wifi_busy: Option<(u64, bool)>,
    wifi_failed: Option<(u64, Instant)>,
    access: [AccessView; 6],
    /// Keep Awake is on, and when Windows last refused to switch it.
    awake: bool,
    awake_failed: Option<std::time::Instant>,
    /// The apps with sound on the default output device (None = still
    /// asking), for the per-app volume rows.
    apps: Option<Vec<AppMix>>,
    /// The apps' volume writes the panel asked for: a count, and each
    /// row's last one (its key -> the count then). A read the worker began
    /// before it carried out a write does not undo it (merge_apps).
    app_seq: u64,
    app_wrote: std::collections::HashMap<u64, u64>,
}

/// One app with an audio session on the default output device: all its
/// process's sessions as one row.
#[derive(Clone, Debug, PartialEq)]
struct AppMix {
    /// The row's key (mix_key): its process and its sessions, so another
    /// process given the same ID later is another row.
    key: u64,
    name: String,
    /// The loudest of its sessions' volumes (percent) and whether all of
    /// them are muted.
    level: u8,
    muted: bool,
    /// Playing now (a session in the active state).
    active: bool,
    /// Its icon (premultiplied BGRA, at most ICON_PX a side), when read.
    icon: Option<Arc<AppIcon>>,
}

/// Pure: a mixer row's key for Hit::App: its process and the first of its
/// sessions' instance IDs (unique to each session: a new process given an
/// old one's ID has sessions of its own, so its row, focus, drag and
/// writes are never the old one's).
fn mix_key(pid: u32, instances: &[&str]) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    pid.hash(&mut h);
    instances.iter().min().hash(&mut h);
    h.finish()
}

/// Pure: sessions (process, instance ID) as mixer rows, one per process
/// in the order found: each row's key, its process and its sessions (by
/// index).
fn mix_rows(sessions: &[(u32, String)]) -> Vec<(u64, u32, Vec<usize>)> {
    let mut pids: Vec<(u32, Vec<usize>)> = Vec::new();
    for (i, (pid, _)) in sessions.iter().enumerate() {
        match pids.iter_mut().find(|p| p.0 == *pid) {
            Some(p) => p.1.push(i),
            None => pids.push((*pid, vec![i])),
        }
    }
    pids.into_iter()
        .map(|(pid, at)| {
            let ids: Vec<&str> = at.iter().map(|&i| sessions[i].1.as_str()).collect();
            (mix_key(pid, &ids), pid, at)
        })
        .collect()
}

/// Pure: a fresh read of the apps, except that a row with a write the
/// worker had not carried out yet when it read (`wrote` after `applied`)
/// keeps the value shown now (`now`): the write lands after the read, and
/// its own change event (MIX_CTX) reads the apps again.
fn merge_apps(mut fresh: Vec<AppMix>, now: Option<&[AppMix]>, wrote: &std::collections::HashMap<u64, u64>, applied: u64) -> Vec<AppMix> {
    for a in &mut fresh {
        if wrote.get(&a.key).is_some_and(|&s| s > applied) {
            if let Some(n) = now.and_then(|l| l.iter().find(|n| n.key == a.key)) {
                a.level = n.level;
                a.muted = n.muted;
            }
        }
    }
    fresh
}

/// Pure: the worker's read of the apps (begun once write `applied` was
/// carried out) into `r`: merged (merge_apps), and the writes carried out
/// before it forgotten (no read can undo them now).
fn take_read(r: &mut Remote, fresh: Vec<AppMix>, applied: u64) {
    let merged = merge_apps(fresh, r.apps.as_deref(), &r.app_wrote, applied);
    r.apps = Some(merged);
    r.app_wrote.retain(|_, s| *s > applied);
}

/// Pure: keeps only the entries of the rows listed now (`listed` keys).
fn prune_rows<V>(by_row: &mut std::collections::HashMap<u64, V>, listed: &[u64]) {
    by_row.retain(|k, _| listed.contains(k));
}

/// Pure: the apps the sound section lists: the ones playing now, and only
/// when more than one is (one app playing needs no mixer: the volume
/// slider is it). In the order the worker found them.
fn mixer_apps(apps: Option<&[AppMix]>) -> Vec<&AppMix> {
    let playing: Vec<&AppMix> = apps.unwrap_or(&[]).iter().filter(|a| a.active).collect();
    if playing.len() > 1 { playing } else { Vec::new() }
}

/// Which worker a WinRT wait belongs to (each has its own waker; WMI's
/// calls are bounded by WMI timeouts instead).
#[derive(Clone, Copy)]
enum Kind {
    Radio = 0,
    Media = 1,
    Quick = 2,
    Devices = 3,
}

/// Shared between the panel and its workers.
struct Shared {
    remote: Mutex<Remote>,
    /// The panel window (0 once it is gone: nothing more is posted).
    hwnd: AtomicIsize,
    /// What is posted to it when a value changed.
    msg: u32,
    /// The panel closed: workers stop between steps.
    quit: AtomicBool,
    airplane_refresh: AtomicBool,
    bt_list_open: AtomicBool,
    /// The display the panel is on (HMONITOR; 0: not known), for the
    /// brightness worker.
    monitor: AtomicIsize,
    /// Wakes a worker waiting on a WinRT operation when the panel closes.
    wakers: [Mutex<Option<Sender<AsyncStatus>>>; 4],
}

impl Shared {
    fn new() -> Self {
        Self::posting(WM_CC_REMOTE)
    }
    fn posting(msg: u32) -> Self {
        Shared {
            remote: Mutex::new(Remote::default()),
            hwnd: AtomicIsize::new(0),
            msg,
            quit: AtomicBool::new(false),
            airplane_refresh: AtomicBool::new(false),
            bt_list_open: AtomicBool::new(false),
            monitor: AtomicIsize::new(0),
            wakers: std::array::from_fn(|_| Mutex::new(None)),
        }
    }
    fn update(&self, f: impl FnOnce(&mut Remote)) {
        if let Ok(mut r) = self.remote.lock() {
            f(&mut r);
        }
        let h = self.hwnd.load(Ordering::SeqCst);
        if h != 0 {
            unsafe {
                let _ = PostMessageW(Some(HWND(h as *mut _)), self.msg, WPARAM(0), LPARAM(0));
            }
        }
    }
    /// Change without telling the panel (it changed it and redraws itself).
    fn set_quiet(&self, f: impl FnOnce(&mut Remote)) {
        if let Ok(mut r) = self.remote.lock() {
            f(&mut r);
        }
    }
    fn snapshot(&self) -> Remote {
        self.remote.lock().map(|r| r.clone()).unwrap_or_default()
    }
    /// The dark-mode worker's answer, read and cleared under one lock (a
    /// newer answer written in between is never wiped).
    fn take_dark(&self) -> Option<(u64, bool)> {
        self.remote.lock().ok().and_then(|mut r| r.dark.take())
    }
    fn quitting(&self) -> bool {
        self.quit.load(Ordering::SeqCst)
    }
    /// The panel is gone: stop posting, stop the workers' waits.
    fn stop(&self) {
        self.hwnd.store(0, Ordering::SeqCst);
        self.quit.store(true, Ordering::SeqCst);
        // No art kept while closed (a queued worker job may hold `self`).
        if let Ok(mut r) = self.remote.lock() {
            r.media = None;
            r.apps = None;
            r.app_wrote.clear();
        }
        for w in &self.wakers {
            if let Some(tx) = w.lock().ok().and_then(|g| g.clone()) {
                let _ = tx.send(AsyncStatus::Canceled);
            }
        }
    }
}
/// The panel's visuals. `motion` (the window's root, px) carries the open
/// and close animations; `outer` (scaled to DIP, offset by the shadow
/// margin) holds `stage`; `stage` holds the chrome (popup::chrome: the
/// shadows, then the clipped root with the material). Inside the root,
/// over the material, the two views: `main` (the grid's drawn surface and
/// its parts over it) and `sub` (a list's), crossing when the panel goes
/// into a list and back.
struct Stage {
    motion: ContainerVisual,
    outer: ContainerVisual,
    stage: ContainerVisual,
    main: ContainerVisual,
    content_main: SpriteVisual,
    parts_main: ContainerVisual,
    sub: ContainerVisual,
    content_sub: SpriteVisual,
    parts_sub: ContainerVisual,
    /// The chrome built last: its clipped root and border.
    root: Option<ContainerVisual>,
    border: Option<ShapeVisual>,
    /// The plate's height ("h", DIP): the chrome follows it
    /// (popup::chrome_live), a spring on it moves the panel's free edge.
    h: CompositionPropertySet,
}

impl Stage {
    fn new(c: &Compositor) -> Result<Self> {
        let motion = c.CreateContainerVisual()?;
        let outer = c.CreateContainerVisual()?;
        let stage = c.CreateContainerVisual()?;
        motion.Children()?.InsertAtTop(&outer)?;
        outer.Children()?.InsertAtTop(&stage)?;
        let view = |c: &Compositor| -> Result<(ContainerVisual, SpriteVisual, ContainerVisual)> {
            let v = c.CreateContainerVisual()?;
            let content = c.CreateSpriteVisual()?;
            let parts = c.CreateContainerVisual()?;
            v.Children()?.InsertAtTop(&content)?;
            v.Children()?.InsertAtTop(&parts)?;
            Ok((v, content, parts))
        };
        let (main, content_main, parts_main) = view(c)?;
        let (sub, content_sub, parts_sub) = view(c)?;
        sub.SetOpacity(0.0)?;
        let h = c.CreatePropertySet()?;
        Ok(Stage { motion, outer, stage, main, content_main, parts_main, sub, content_sub, parts_sub, root: None, border: None, h })
    }
}

/// A part drawn as a visual of its own over the surface (it animates).
enum Spec {
    Disc(DiscSpec),
    Pill(PillSpec),
    Art(D2D_RECT_F, Option<Arc<Art>>),
    Time(TimeSpec),
}

/// A round or square button (a 1 x 1 tile, a chip, a glyph button, the
/// output button): its shape in one color (or, high contrast, its outline)
/// with a glyph in another, both Composition color brushes over white
/// masks, so a change of state cross-fades in the compositor and a press
/// scales it.
#[derive(Clone, Copy, Debug, PartialEq)]
struct DiscSpec {
    hit: Hit,
    /// Its square, and its corners (half its side: a circle).
    r: D2D_RECT_F,
    rad: f32,
    glyph: Glyph,
    gsize: f32,
    fill: Rgba,
    /// High contrast: an outline this wide in `fill`'s color instead of a
    /// filled shape.
    ring: Option<f32>,
    ink: Rgba,
    look: Look,
    /// Its scale while pressed (motion::press_scale).
    press: f32,
    /// Not usable now: the glyph at DIM.
    dim: bool,
    /// The "opens elsewhere" mark at its top end.
    mark: bool,
    /// Flat v2's card light on a raised tile (Theme::card_light): the top
    /// edge's white and the drop's black, as alphas (0: none: another
    /// face, on, pressed for the light, not Flat v2, high contrast).
    lit: (u8, u8),
}

/// How faint a glyph that does nothing now is, and the "opens elsewhere"
/// mark beside the ink (6.2: disabled .38; the third text level against
/// the first).
const DIM: f32 = 0.38;
const MARK_ALPHA: f32 = 0.5;

/// A thick slider: what its surface shows, and whether it is held.
#[derive(Clone, Debug, PartialEq)]
struct PillSpec {
    hit: Hit,
    r: D2D_RECT_F,
    held: bool,
    look: PillLook,
}

#[derive(Clone, Debug, PartialEq)]
struct PillLook {
    w: f32,
    v: f32,
    value: String,
    glyph: Glyph,
    /// The value shows (hovered or held).
    show_value: bool,
    enabled: bool,
    muted: bool,
    /// The volume's mute button: hovered, pressed.
    mute: Option<(bool, bool)>,
    tones: Tones,
    /// High contrast: (text, highlight, highlight text).
    hc: Option<(Rgba, Rgba, Rgba)>,
    /// Right to left: the insides mirrored (layout::pill_geo).
    rtl: bool,
}

/// The position bar.
#[derive(Clone, Debug, PartialEq)]
struct TimeSpec {
    r: D2D_RECT_F,
    ratio: f32,
    tones: Tones,
    hc: Option<(Rgba, Rgba)>,
}

struct DiscPart {
    spec: DiscSpec,
    vis: SpriteVisual,
    glyph_v: SpriteVisual,
    mark_v: Option<SpriteVisual>,
    /// The face (the fill through its shape), Flat v2's drop under it and
    /// its top edge's light over it (DiscSpec::lit): children of `vis`,
    /// so a press scales them all.
    face_v: SpriteVisual,
    drop_v: SpriteVisual,
    light_v: SpriteVisual,
    drop: CompositionColorBrush,
    light: CompositionColorBrush,
    dmask: windows::UI::Composition::CompositionMaskBrush,
    lmask: windows::UI::Composition::CompositionMaskBrush,
    fill: CompositionColorBrush,
    ink: CompositionColorBrush,
    mask: windows::UI::Composition::CompositionMaskBrush,
    gmask: windows::UI::Composition::CompositionMaskBrush,
}

struct PillPart {
    spec: PillSpec,
    vis: SpriteVisual,
}

struct ArtPart {
    vis: ContainerVisual,
    sprites: [SpriteVisual; 2],
    /// Which sprite shows the art now.
    front: usize,
    art: Option<Arc<Art>>,
}

struct TimePart {
    spec: TimeSpec,
    vis: SpriteVisual,
}

/// The white masks the buttons are cut from (by size in px), made once.
#[derive(Clone, Copy, Debug, PartialEq)]
enum MaskKey {
    /// A square `px` a side with corners (1/100 px), filled, or outlined
    /// this wide (1/100 px; 0: filled).
    Shape(u32, u32, u32),
    Glyph(Glyph, u32, u32),
    /// The "opens elsewhere" mark at a tile's top end.
    Mark(u32),
    /// Flat v2's card light: the band along the top edge of a square `px`
    /// a side with corners (1/100 px), one DIP (1/100 px) down.
    Light(u32, u32, u32),
}

/// Draws a button's shape `key` in color `c`, `px` a side from (`ox`, `oy`)
/// px, for a button `d` DIP across at `ds` px per DIP (the transform is
/// left as the identity).
#[allow(clippy::too_many_arguments)]
unsafe fn paint_shape(dc: &ID2D1DeviceContext, ox: f32, oy: f32, key: MaskKey, d: f32, ds: f32, px: u32, c: D2D1_COLOR_F) -> Result<()> {
    dc.SetAntialiasMode(D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
    let b = dc.CreateSolidColorBrush(&c, None)?;
    match key {
        MaskKey::Shape(_, rad, w) => {
            let (rad, w, side) = (rad as f32 / 100.0, w as f32 / 100.0, px as f32);
            if w <= 0.0 {
                dc.FillRoundedRectangle(&D2D1_ROUNDED_RECT { rect: rect(ox, oy, ox + side, oy + side), radiusX: rad, radiusY: rad }, &b);
            } else {
                let h = w / 2.0;
                let r = (rad - h).max(0.0);
                dc.DrawRoundedRectangle(&D2D1_ROUNDED_RECT { rect: rect(ox + h, oy + h, ox + side - h, oy + side - h), radiusX: r, radiusY: r }, &b, w, None);
            }
        }
        MaskKey::Glyph(g, _, gs) => {
            dc.SetTransform(&Matrix3x2 { M11: ds, M12: 0.0, M21: 0.0, M22: ds, M31: ox, M32: oy });
            let r = crate::glyphs::draw(dc, g, d / 2.0, d / 2.0, gs as f32 / 100.0, c);
            dc.SetTransform(&Matrix3x2::identity());
            r?;
        }
        MaskKey::Light(_, rad, k) => {
            use windows::Win32::Graphics::Direct2D::D2D1_COMBINE_MODE_EXCLUDE;
            let (rad, k, side) = (rad as f32 / 100.0, k as f32 / 100.0, px as f32);
            let f = dc.GetFactory()?;
            let face = f.CreateRoundedRectangleGeometry(&D2D1_ROUNDED_RECT { rect: rect(ox, oy, ox + side, oy + side), radiusX: rad, radiusY: rad })?;
            let below = f.CreateRoundedRectangleGeometry(&D2D1_ROUNDED_RECT { rect: rect(ox, oy + k, ox + side, oy + side + k), radiusX: rad, radiusY: rad })?;
            let band = f.CreatePathGeometry()?;
            let sink = band.Open()?;
            face.CombineWithGeometry(&below, D2D1_COMBINE_MODE_EXCLUDE, None, 0.25, &sink)?;
            sink.Close()?;
            dc.FillGeometry(&band, &b, None);
        }
        MaskKey::Mark(_) => {
            dc.SetTransform(&Matrix3x2 { M11: ds, M12: 0.0, M21: 0.0, M22: ds, M31: ox, M32: oy });
            let r = crate::glyphs::draw(dc, G_EXTERNAL, d - EXT_IN - EXT_MARK / 2.0, EXT_IN + EXT_MARK / 2.0, EXT_MARK, c);
            dc.SetTransform(&Matrix3x2::identity());
            r?;
        }
    }
    Ok(())
}

/// One view's parts (the grid's, or a list's: `sub`), and the masks they
/// share.
#[derive(Default)]
struct Parts {
    sub: bool,
    discs: Vec<DiscPart>,
    pills: Vec<PillPart>,
    art: Option<ArtPart>,
    time: Option<TimePart>,
    masks: Vec<(MaskKey, CompositionSurfaceBrush)>,
}

struct Panel {
    hwnd: HWND,
    gfx: Rc<Gfx>,
    theme: Theme,
    scale: f32,
    _target: DesktopWindowTarget,
    v: Stage,
    /// The panel on screen (px), without the shadow margin.
    rect: RECT,
    /// The fitted viewport height in layout units.
    height: f32,
    /// The plate's height on its way (GLIDE, Stage::h) and the timer that
    /// shrinks the window once a shorter plate has arrived.
    h_run: SpringRun,
    shrink: Option<usize>,
    /// Where the window is against the panel (WinFit: taller than the
    /// panel while a shorter plate glides, until `shrink` fires).
    fit: WinFit,
    /// The control button and its monitor's work area (screen px).
    button: RECT,
    work: RECT,
    /// DIP per layout DIP (panel_size): the panel's size on this monitor.
    size: f32,
    shared: Arc<Shared>,
    workers: Workers,
    _power: PowerWatch,
    volume: Option<(u8, bool)>,
    dark: bool,
    /// Dark mode requests made / the last one the worker answered.
    dark_seq: u64,
    dark_done: u64,
    /// When Windows last kept dark mode as it was after our switch.
    dark_failed: Option<std::time::Instant>,
    hits: Vec<(D2D_RECT_F, Hit)>,
    /// What the last paint said in words, for screen readers: text lines
    /// that belong to no control (an error, the track), each after the
    /// controls drawn before it (hits.len() then), with its key; and the
    /// status line drawn with a control (its value).
    texts: RefCell<Vec<(usize, D2D_RECT_F, &'static str, String)>>,
    /// Controls drawn that take no input now, read all the same (disabled,
    /// with their status: an eject on its way or done), each after the
    /// first `.0` hits (layout::reading_order).
    inert: RefCell<Vec<(usize, D2D_RECT_F, Hit)>>,
    tile_clicks: TileClicks,
    /// The audio failure (Remote::audio_fail_seq) last shown in the sound
    /// list (layout::fail_leads_in).
    fail_shown: u64,
    values: RefCell<Vec<(Hit, String)>>,
    /// What each control's tooltip says (the 1 x 1 toggles' names, ...).
    tips: RefCell<Vec<(Hit, String)>>,
    /// Sliders: (which, track) DIP.
    sliders: Vec<(Hit, D2D_RECT_F)>,
    hover: Option<Hit>,
    pressed: Option<Hit>,
    /// Where the last press went down (DIP): a drag from the mute glyph
    /// becomes a volume drag.
    press_at: (f32, f32),
    focus: Option<Hit>,
    /// The keyboard was used: show the focus ring.
    keyboard: bool,
    sliding: Option<Hit>,
    /// An app's volume being dragged (its key, percent): the echoes of
    /// the worker do not pull it back.
    app_drag: Option<(u64, u8)>,
    /// The list the panel went into (None: the grid).
    open: Option<Section>,
    scroll: f32,
    scroll_max: f32,
    /// The tooltip: the control it is for (waiting or shown), whether it
    /// shows, and when the last one closed (TIP_WARM_MS).
    tip_for: Option<Hit>,
    tip_shown: bool,
    tip_closed: Option<std::time::Instant>,
    done: Option<Outcome>,
    after: Option<After>,
    /// The app icons uploaded to the GPU, made again only when they or the
    /// D2D device change; they go with the panel. (The art is a part: a
    /// surface of its own, ArtPart.)
    icon_bmps: RefCell<Vec<(Arc<AppIcon>, ID2D1Device, ID2D1Bitmap1)>>,
    /// Right-to-left language: the panel is laid out as usual and drawn
    /// through a mirroring transform (Wi-Fi on the right); text, glyphs in
    /// parts and album art are drawn unmirrored at their mirrored places,
    /// and pointer x is mirrored back (see dip).
    rtl: bool,
    /// The parts the last paint asked for, and the visuals showing them
    /// (the grid's and a list's).
    plan: RefCell<Vec<Spec>>,
    parts: RefCell<Parts>,
    sub_parts: RefCell<Parts>,
    /// The time played ticks (TICK_TIMER is set).
    ticking: bool,
}

thread_local! {
    static CC: RefCell<Option<Panel>> = const { RefCell::new(None) };
    /// Closed panels still fading out: their window, target and visuals,
    /// until CLOSE_TIMER.
    static CLOSING: RefCell<Vec<(HWND, DesktopWindowTarget, ContainerVisual)>> = const { RefCell::new(Vec::new()) };
}

fn with_cc<R>(f: impl FnOnce(&mut Panel) -> R) -> Option<R> {
    CC.with(|c| c.try_borrow_mut().ok().and_then(|mut g| g.as_mut().map(f)))
}

/// with_cc for a message to window `hwnd`: only the open panel's own (a
/// closed one still fading gets nothing).
fn with_cc_at<R>(hwnd: HWND, f: impl FnOnce(&mut Panel) -> R) -> Option<R> {
    CC.with(|c| c.try_borrow_mut().ok().and_then(|mut g| g.as_mut().filter(|p| p.hwnd == hwnd).map(f)))
}

/// The panel is open right now.
pub fn is_open() -> bool {
    CC.with(|c| c.try_borrow().map(|g| g.is_some()).unwrap_or(true))
}

/// Close the panel (its bar is going away or moved). Safe from inside the
/// nested loop's handlers.
pub fn cancel() {
    let h = with_cc(|p| {
        p.done.get_or_insert(Outcome::Lost);
        p.hwnd
    });
    if let Some(h) = h {
        unsafe {
            let _ = PostMessageW(Some(h), WM_NULL, WPARAM(0), LPARAM(0));
        }
    }
}

/// The output volume changed (from the bar's listener).
pub fn set_volume(v: Option<(u8, bool)>) {
    with_cc(|p| {
        // While dragging, the slider follows the pointer, not the echoes.
        p.volume = match (p.sliding == Some(Hit::Volume), p.volume, v) {
            (true, Some((level, _)), Some((_, muted))) => Some((level, muted)),
            _ => v,
        };
        p.redraw();
    });
}

/// The network changed: the Wi-Fi name may have too.
pub fn net_changed() {
    with_cc(|p| {
        let _ = p.workers.radio.send(RadioCmd::Ssid);
        let _ = p.workers.quick.send(QuickCmd::Network);
    });
}

/// The theme was re-resolved (light/dark, accent, high contrast, motion).
pub fn retheme(theme: &Theme) {
    with_cc(|p| {
        // A switch of ours still on its way reports itself.
        if p.dark_seq == p.dark_done {
            p.dark = read_dark();
        }
        if p.theme != *theme {
            p.theme = theme.clone();
            // A new text size sizes the panel anew (resize builds the
            // chrome and redraws).
            p.resize(true);
            return;
        }
        p.redraw();
    });
}

/// Removable storage came, went, or an eject answered: the cards follow
/// (they appear with the first device, go with the last).
pub fn removable_changed() {
    with_cc(|p| p.sync());
}

/// Quitting: Keep Awake's power request goes.
pub fn release_awake() {
    awake::release();
}

/// WM_POWERBROADCAST (`event`): back from sleep, Keep Awake's request
/// (which Windows ended for the sleep) is made again; if it cannot be,
/// the tile shows it off.
pub fn on_power(event: usize) {
    awake::on_power(event);
    // An open panel's tile follows.
    with_cc(|p| {
        let on = awake::on();
        let (battery, energy) = read_power();
        let before = p.shared.snapshot();
        if before.awake != on || before.battery != battery || before.energy != energy {
            p.shared.set_quiet(|r| { r.awake = on; r.battery = battery; r.energy = energy; });
            p.redraw();
        }
    });
}

/// Shows the panel and returns how it closed. Must not be called while the
/// caller holds its own RefCell borrow (the nested loop dispatches to it).
pub fn show(req: Request) -> Outcome {
    if is_open() || crate::popup::is_tracking() {
        return Outcome::Closed;
    }
    unsafe {
        register_class();
        let panel = match Panel::new(&req) {
            Ok(p) => p,
            Err(e) => {
                crate::log!("control: cannot open: {e}");
                return Outcome::Lost;
            }
        };
        let hwnd = panel.hwnd;
        CC.with(|c| *c.borrow_mut() = Some(panel));
        with_cc(|p| {
            p.redraw();
            p.play_open();
        });
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        let _ = SetForegroundWindow(hwnd);
        let _ = SetFocus(Some(hwnd));
        SetCapture(hwnd);
        a11y::menu_event(hwnd, true);
        let mut out = Outcome::Lost;
        if GetForegroundWindow() != hwnd || GetCapture() != hwnd {
            crate::log!("control: could not take foreground/capture; not opening");
        } else {
            let mut msg = MSG::default();
            loop {
                if with_cc(|p| p.done.is_some()).unwrap_or(true) {
                    break;
                }
                let r = GetMessageW(&mut msg, None, 0, 0);
                if r.0 == 0 || r.0 == -1 {
                    // WM_QUIT: close and let the outer loop see it.
                    PostQuitMessage(msg.wParam.0 as i32);
                    with_cc(|p| p.done = Some(Outcome::Lost));
                    break;
                }
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        let _ = ReleaseCapture();
        let mut after = None;
        let mut studio = None;
        if let Some(p) = CC.with(|c| c.borrow_mut().take()) {
            out = p.done.unwrap_or(out);
            after = p.after;
            studio = p.shared.snapshot().studio;
            a11y::menu_event(p.hwnd, false);
            p.close();
        }
        match (after, studio) {
            (Some(After::Cast), _) if !cfg!(test) => crate::keys::cast(),
            (Some(After::Studio), Some(camera)) if !cfg!(test) => open_studio(camera),
            (Some(After::Open(target)), _) if !cfg!(test) => {
                crate::launch::open_soon(crate::launch::Target::new(target, target, crate::launch::Source::TopBar));
            }
            (Some(After::Project(tn)), _) if !cfg!(test) => {
                let _ = std::thread::Builder::new().name("cc-display-mode".into())
                    .spawn(move || set_topology(tn))
                    .inspect_err(|e| crate::log!("control: no thread for the display mode ({e})"));
            }
            _ => {}
        }
        out
    }
}

unsafe fn register_class() {
    let hinst = GetModuleHandleW(None).unwrap_or_default();
    let wc = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: Some(wndproc),
        hInstance: hinst.into(),
        lpszClassName: w!("NopBarControlCenter"),
        hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
        ..Default::default()
    };
    // Fails harmlessly once registered.
    RegisterClassExW(&wc);
}

// ---- Composition helpers (every length at the chosen speed, crate::effects)

fn span(ms: i64) -> windows::Foundation::TimeSpan {
    windows::Foundation::TimeSpan { Duration: crate::effects::ms(ms.max(1)) * 10_000 }
}

fn curve(c: &Compositor, e: Curve) -> Result<CompositionEasingFunction> {
    c.CreateCubicBezierEasingFunction(Vector2 { X: e.0 .0, Y: e.0 .1 }, Vector2 { X: e.1 .0, Y: e.1 .1 })?.cast()
}

fn hs(s: &str) -> windows::core::HSTRING {
    windows::core::HSTRING::from(s)
}

/// A property sprung to `to` (written as an expression's vector, e.g.
/// "Vector3(1, 1, 1)"), from wherever it is: the spring's course sampled
/// into key frames (Spring::frames), linear between them.
fn spring_to(c: &Compositor, o: &CompositionObject, prop: &str, to: &str, s: Spring, v3: bool) -> Result<()> {
    let lin = c.CreateLinearEasingFunction()?;
    let frames = s.frames(SPRING_FRAMES);
    let key = |p: f32| hs(&format!("this.StartingValue + ({to} - this.StartingValue) * {p}"));
    if v3 {
        let a = c.CreateVector3KeyFrameAnimation()?;
        for (f, p) in frames {
            a.InsertExpressionKeyFrameWithEasingFunction(f, &key(p), &lin)?;
        }
        a.SetDuration(span(s.max_ms))?;
        o.StartAnimation(&hs(prop), &a)
    } else {
        let a = c.CreateVector2KeyFrameAnimation()?;
        for (f, p) in frames {
            a.InsertExpressionKeyFrameWithEasingFunction(f, &key(p), &lin)?;
        }
        a.SetDuration(span(s.max_ms))?;
        o.StartAnimation(&hs(prop), &a)
    }
}

/// A color brush moved to `to` over a fade of `ms` on `e`, as the motion
/// settings play it (0: at once).
fn tint(c: &Compositor, b: &CompositionColorBrush, to: Rgba, ms: i64, e: Curve) -> Result<()> {
    let (ms, e) = crate::motion::fade_now(ms, e);
    if ms <= 0 {
        let _ = b.StopAnimation(&hs("Color"));
        return b.SetColor(ui_color(to));
    }
    let a = c.CreateColorKeyFrameAnimation()?;
    a.SetInterpolationColorSpace(CompositionColorSpace::Rgb)?;
    a.InsertKeyFrameWithEasingFunction(1.0, ui_color(to), &curve(c, e)?)?;
    a.SetDuration(span_real(ms))?;
    b.StartAnimation(&hs("Color"), &a)
}

fn obj(v: &impl Interface) -> Result<CompositionObject> {
    v.cast()
}

/// A key-frame animation's length as it plays (no speed applied again:
/// motion::fade_now already did).
fn span_real(ms: i64) -> windows::Foundation::TimeSpan {
    windows::Foundation::TimeSpan { Duration: ms.max(1) * 10_000 }
}

/// A scalar property faded to `to` over a fade of `ms` on `e`, as the
/// motion settings play it (motion::fade_now: reduced motion 60 ms,
/// straight; 0: at once).
fn fade_scalar(c: &Compositor, o: &CompositionObject, prop: &str, to: f32, ms: i64, e: Curve) -> Result<()> {
    let (ms, e) = crate::motion::fade_now(ms, e);
    if ms <= 0 {
        let _ = o.StopAnimation(&hs(prop));
        let a = c.CreateScalarKeyFrameAnimation()?;
        a.InsertKeyFrame(1.0, to)?;
        a.SetDuration(span_real(1))?;
        return o.StartAnimation(&hs(prop), &a);
    }
    let a = c.CreateScalarKeyFrameAnimation()?;
    a.InsertKeyFrameWithEasingFunction(1.0, to, &curve(c, e)?)?;
    a.SetDuration(span_real(ms))?;
    o.StartAnimation(&hs(prop), &a)
}

impl Panel {
    unsafe fn new(req: &Request) -> Result<Self> {
        let shared = Arc::new(Shared::new());
        let (battery, energy) = read_power();
        shared.set_quiet(|r| {
            r.battery = battery;
            r.energy = energy;
            r.awake = awake::on();
        });
        // Under the button (over it at the bottom), right-aligned with it,
        // inside the work area. The monitor is the button's own (its
        // middle: a bar at the bottom ends on the next display's edge).
        let at = POINT { x: (req.button.left + req.button.right) / 2, y: (req.button.top + req.button.bottom) / 2 };
        let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        let mon = MonitorFromPoint(at, MONITOR_DEFAULTTONEAREST);
        let _ = GetMonitorInfoW(mon, &mut mi);
        // Its brightness: the level it had last time shows at once (the
        // worker corrects it, or hides the slider).
        shared.monitor.store(mon.0 as isize, Ordering::SeqCst);
        if let Some(level) = bright::CACHE.lock().ok().and_then(|c| c.shown(&bright::key_of(mon.0 as isize))) {
            shared.set_quiet(|r| r.brightness = Some(Some(level)));
        }
        // What every machine starts with (the rest comes with the workers'
        // answers).
        let first = layout(&shape_of(&shared.snapshot(), None, removable_rows()));
        Self::make(req, shared, first, mi.rcWork, read_dark(), Workers::start)
    }

    /// The (hidden) panel for what `shared` holds so far, laid out as
    /// `first`, in the work area `work` (screen px); `dark`: Windows' dark
    /// mode now; its workers from `workers` (Workers::start).
    unsafe fn make(req: &Request, shared: Arc<Shared>, first: Layout, work: RECT, dark: bool, workers: fn(&Arc<Shared>) -> Workers) -> Result<Self> {
        let s = req.scale;
        let (rect, size) = place_fit(req.button, work, s, crate::fonts::scale(), first.rest, first.fit, first.height, crate::i18n::rtl(), super::at_bottom());
        let height = viewport_height(first.height, rect, s * size);
        let m_px = (SHADOW_M * s).ceil() as i32;
        let hwnd = CreateWindowExW(
            WS_EX_NOREDIRECTIONBITMAP | WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
            w!("NopBarControlCenter"),
            &windows::core::HSTRING::from(t("control.title")),
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
            let c = req.gfx.compositor.clone();
            let interop: ICompositorDesktopInterop = c.cast()?;
            let target = interop.CreateDesktopWindowTarget(hwnd, true)?;
            let v = Stage::new(&c)?;
            v.outer.SetScale(Vector3 { X: s * size, Y: s * size, Z: 1.0 })?;
            v.outer.SetOffset(Vector3 { X: m_px as f32, Y: m_px as f32, Z: 0.0 })?;
            target.SetRoot(&v.motion)?;
            shared.hwnd.store(hwnd.0 as isize, Ordering::SeqCst);
            let mut p = Panel {
                hwnd,
                gfx: req.gfx.clone(),
                theme: req.theme.clone(),
                scale: s,
                _target: target,
                v,
                rect,
                height,
                button: req.button,
                work,
                size,
                workers: workers(&shared),
                _power: PowerWatch::new(hwnd),
                shared,
                volume: req.volume,
                dark,
                dark_seq: 0,
                dark_done: 0,
                dark_failed: None,
                hits: Vec::new(),
                texts: RefCell::new(Vec::new()),
                inert: RefCell::new(Vec::new()),
                tile_clicks: TileClicks::default(),
                fail_shown: 0,
                values: RefCell::new(Vec::new()),
                tips: RefCell::new(Vec::new()),
                sliders: Vec::new(),
                hover: None,
                pressed: None,
                press_at: (0.0, 0.0),
                focus: None,
                keyboard: false,
                sliding: None,
                app_drag: None,
                open: None,
                scroll: 0.0,
                scroll_max: first.height - height,
                tip_for: None,
                tip_shown: false,
                tip_closed: None,
                done: None,
                after: None,
                icon_bmps: RefCell::new(Vec::new()),
                rtl: crate::i18n::rtl(),
                plan: RefCell::new(Vec::new()),
                parts: RefCell::new(Parts::default()),
                sub_parts: RefCell::new(Parts { sub: true, ..Default::default() }),
                ticking: false,
                h_run: SpringRun::still(crate::motion::SPRING_GLIDE, height, std::time::Instant::now()),
                shrink: None,
                fit: WinFit::at(rect),
            };
            if let Err(e) = p.build_chrome() {
                p.workers.quit(&p.shared);
                return Err(e);
            }
            Ok(p)
        })();
        if built.is_err() {
            let _ = DestroyWindow(hwnd);
        }
        built
    }

    /// px per layout DIP as drawn: the monitor scale times the panel's size.
    fn ds(&self) -> f32 {
        self.scale * self.size
    }

    /// Full motion (crate::motion: Windows' animation effects and NopBar's
    /// small animations) and high contrast off: buttons press, sliders
    /// grow. Otherwise only fades.
    fn moving(&self) -> bool {
        crate::motion::full() && self.hc().is_none()
    }

    /// The window's shadow margin (px).
    fn margin(&self) -> i32 {
        (SHADOW_M * self.scale).ceil() as i32
    }

    /// Opening: from the bar's button, a spring of scale (GLIDE) and a fade
    /// (reduced motion: the fade alone, 60 ms straight).
    fn play_open(&self) {
        let r = (|| -> Result<()> {
            let c = &self.gfx.compositor;
            let motion = crate::motion::full();
            let bottom = super::at_bottom();
            let m = self.margin();
            let v = &self.v.motion;
            // The button's middle, and the panel's edge on the bar's side,
            // in the window.
            let cx = (self.button.left + self.button.right) as f32 / 2.0 - (self.rect.left - m) as f32;
            let cy = if bottom { (self.rect.bottom - self.rect.top + m) as f32 } else { m as f32 };
            v.SetCenterPoint(Vector3 { X: cx, Y: cy, Z: 0.0 })?;
            let s0 = open_from(motion);
            v.SetOpacity(0.0)?;
            v.SetScale(Vector3 { X: s0, Y: s0, Z: 1.0 })?;
            v.SetOffset(Vector3::default())?;
            let o = obj(v)?;
            fade_scalar(c, &o, "Opacity", 1.0, OPEN_FADE_MS, EASE_OUT)?;
            if motion {
                spring_to(c, &o, "Scale", "Vector3(1, 1, 1)", SPRING_C, true)?;
            } else {
                v.SetScale(Vector3 { X: 1.0, Y: 1.0, Z: 1.0 })?;
            }
            Ok(())
        })();
        if let Err(e) = r {
            crate::log!("control: no opening animation: {e}");
            let _ = self.v.motion.SetOpacity(1.0);
        }
    }

    /// (Re)builds the material and shadow for the window's size and theme,
    /// with the two views inside.
    fn build_chrome(&mut self) -> Result<()> {
        // A visual has one parent: take the views out of the old chrome
        // before it goes, or they cannot join the new one.
        for v in [&self.v.main, &self.v.sub] {
            if let Ok(parent) = v.Parent() {
                let _ = parent.Children().and_then(|c| c.Remove(v));
            }
        }
        self.v.stage.Children()?.RemoveAll()?;
        // The plate at its height now, at rest (Stage::h).
        let _ = self.v.h.StopAnimation(&windows::core::HSTRING::from("h"));
        self.h_run = SpringRun::still(crate::motion::SPRING_GLIDE, self.height, std::time::Instant::now());
        let (root, border) = crate::popup::chrome_live(&self.gfx, &self.theme, &self.v.stage, W, self.height, self.ds(), PANEL_CORNER, &self.v.h)?;
        root.Children()?.InsertBelow(&self.v.main, &border)?;
        root.Children()?.InsertBelow(&self.v.sub, &border)?;
        self.v.root = Some(root);
        self.v.border = Some(border);
        self.rest_stage();
        Ok(())
    }

    /// The views as tall as the panel.
    fn rest_stage(&self) {
        let v = &self.v;
        let size = Vector2 { X: W, Y: self.height };
        for c in [&v.main, &v.sub] {
            let _ = c.SetSize(size);
        }
        for c in [&v.content_main, &v.content_sub] {
            let _ = c.SetSize(size);
        }
    }

    /// Closing: a quick fade (the kit's: no shrink); the window goes when
    /// it is over (CLOSE_TIMER), the workers at once.
    fn close(mut self) {
        self.hide_tip();
        self.workers.quit(&self.shared);
        unsafe {
            for id in [NOTE_TIMER, EJECT_TIMER, TIP_TIMER, TICK_TIMER] {
                let _ = KillTimer(Some(self.hwnd), id);
            }
        }
        let (ms, _) = crate::motion::fade_now(CLOSE_MS, EASE_EXIT);
        let played = (|| -> Result<()> {
            let c = &self.gfx.compositor;
            let o = obj(&self.v.motion)?;
            fade_scalar(c, &o, "Opacity", 0.0, CLOSE_MS, EASE_EXIT)
        })();
        let hwnd = self.hwnd;
        let kept = played.is_ok()
            && unsafe {
                // Fading: no more input.
                let _ = EnableWindow(hwnd, false);
                SetTimer(Some(hwnd), CLOSE_TIMER, ms as u32 + 40, None) != 0
            };
        if kept {
            let Panel { _target, v, .. } = self;
            CLOSING.with(|c| c.borrow_mut().push((hwnd, _target, v.motion)));
        } else {
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
        }
    }

    fn show_display(r: &Remote) -> bool {
        matches!(r.brightness, Some(Some(_)))
    }

    /// The layout for what is known now.
    fn layout_now(&self, r: &Remote) -> Layout {
        layout(&shape_of(r, self.open, removable_rows()))
    }

    /// Values arrived: take a confirmed dark mode, leave a list with
    /// nothing left in it, grow or shrink the panel for what came or went
    /// (moved up or shrunk to stay in the work area), redraw.
    fn sync(&mut self) {
        let answer = self.shared.take_dark();
        let r = self.shared.snapshot();
        if let Some((seq, dark)) = answer {
            self.dark_done = self.dark_done.max(seq);
            // Older answers are overtaken by a newer click.
            if seq == self.dark_seq {
                if dark != self.dark {
                    self.dark_failed = Some(std::time::Instant::now());
                }
                self.dark = dark;
            }
        }
        // Repaint when the next transient status expires.
        let now = Instant::now();
        let next_note = [self.dark_failed, r.hotspot_failed, r.awake_failed, r.wifi_failed.map(|(_, at)| at)]
            .into_iter().chain(r.radio_failed)
            .chain(r.system.iter().map(|s| s.failed))
            .chain(r.access.iter().map(|s| s.failed))
            .chain(r.bt_failed.values().copied().map(Some))
            .flatten().map(|at| at + FAIL_SHOW).filter(|&until| until > now).min();
        if let Some(until) = next_note {
            unsafe { SetTimer(Some(self.hwnd), NOTE_TIMER, until.duration_since(now).as_millis() as u32 + 16, None); }
        }
        // A removable device's note goes on its own.
        if let Some(d) = crate::removable::next_change() {
            unsafe {
                SetTimer(Some(self.hwnd), EJECT_TIMER, (d.as_millis() as u32).saturating_add(50), None);
            }
        }
        // A failed device switch is said in the sound list (gone back into
        // if it was left before it came), not by leaving for Windows'
        // settings. Once: left after it was shown, it stays left.
        let fail = r.audio_fail.is_some().then_some(r.audio_fail_seq);
        if fail_leads_in(fail, self.open, &mut self.fail_shown) {
            return self.set_section(Some(Section::Sound));
        }
        // Nothing left to show in the open list (the second display went,
        // every device is gone and nothing to report).
        if let Some(s) = self.open {
            let gone = match s {
                Section::Project => !matches!(r.displays, Displays::Many(_)),
                Section::Sound => section_cards(s, &r).is_empty(),
                _ => false,
            };
            if gone {
                return self.set_section(None);
            }
        }
        // The app being dragged keeps its value; any other echo is shown.
        if self.sliding.is_none_or(|h| !matches!(h, Hit::App(_))) {
            self.app_drag = None;
        }
        self.resize(false);
    }

    /// Fits the window to the layout now (or at least rebuilds the
    /// chrome, `force`) and redraws.
    fn resize(&mut self, force: bool) {
        let r = self.shared.snapshot();
        let lay = self.layout_now(&r);
        // The size follows the panel at rest too (lay.rest): the same
        // height with other removable devices may still want another size.
        let (rect, size) = place_fit(self.button, self.work, self.scale, crate::fonts::scale(), lay.rest, lay.fit, lay.height, self.rtl, super::at_bottom());
        let h = viewport_height(lay.height, rect, self.scale * size);
        self.scroll_max = lay.height - h;
        self.scroll = self.scroll.min(self.scroll_max);
        // Another height only, under a bar at the top: it glides (7.4: a
        // panel's height on GLIDE, its edge at the bar fixed; the window
        // grows before the plate does and shrinks after it). Nothing
        // changed: a shrink on its way is left to come (WinFit::keep).
        let (fit, step) = resize_step(&mut self.fit, force, crate::motion::full(), super::at_bottom(), (self.height, self.rect, self.size), (h, rect, size));
        match fit {
            Fit::Glide => self.glide_to(h, rect, step),
            Fit::Now => {
                self.height = h;
                self.rect = rect;
                if size != self.size {
                    // The parts were drawn for the old size: made again.
                    self.drop_parts();
                    self.size = size;
                }
                self.window_step(step, 0);
                let ds = self.ds();
                let _ = self.v.outer.SetScale(Vector3 { X: ds, Y: ds, Z: 1.0 });
                if let Err(e) = self.build_chrome() {
                    crate::log!("control: resize failed: {e}");
                }
            }
            Fit::Keep => {
                self.window_step(step, 0);
                // A wait whose timer is gone (it could not reach the
                // panel) shrinks now rather than never.
                if self.fit.waiting && self.shrink.is_none() {
                    self.shrink_arrived();
                }
            }
        }
        self.draw(&r, &lay);
    }

    /// The plate glides to `h` DIP tall (`rect` on screen): its top where
    /// it is, the window as large as the larger of the two meanwhile
    /// (`step`: WinFit::glide).
    fn glide_to(&mut self, h: f32, rect: RECT, step: WinStep) {
        self.height = h;
        self.rect = rect;
        // A shrink waiting stops; the window grows now if it must.
        self.window_step(WinStep { wait: false, ..step }, 0);
        let ms = match crate::motion::spring_prop(&self.gfx.compositor, &self.v.h, "h", &self.h_run, h) {
            Ok(run) => {
                self.h_run = run;
                (1..=250u32).map(|i| i * 8).find(|ms| self.h_run.settled(std::time::Instant::now() + std::time::Duration::from_millis(*ms as u64), 0.25)).unwrap_or(2000)
            }
            Err(e) => {
                crate::log!("control: no height glide: {e}");
                let _ = self.v.h.InsertScalar(&windows::core::HSTRING::from("h"), h);
                self.h_run = SpringRun::still(crate::motion::SPRING_GLIDE, h, std::time::Instant::now());
                0
            }
        };
        // The window shrinks to the panel once the plate has arrived.
        self.window_step(WinStep { cancel: false, to: None, wait: step.wait }, ms);
    }

    /// Does what WinFit asked of the window: stops the shrink waiting,
    /// moves the window, sets the shrink's timer (`ms` from now: the
    /// plate's glide). A timer that cannot be set shrinks at once.
    fn window_step(&mut self, step: WinStep, ms: u32) {
        if step.cancel {
            if let Some(id) = self.shrink.take() {
                crate::motion::cancel(id);
            }
        }
        if let Some(r) = step.to {
            self.move_window(r);
        }
        if step.wait {
            let hwnd = self.hwnd;
            self.shrink = crate::motion::after(
                ms + 32,
                Box::new(move || {
                    with_cc_at(hwnd, |p| p.shrink_arrived());
                }),
            );
            if self.shrink.is_none() {
                self.shrink_arrived();
            }
        }
    }

    /// The shrink's timer: the plate arrived, the window takes its size.
    fn shrink_arrived(&mut self) {
        self.shrink = None;
        if let Some(r) = self.fit.arrived(self.rect) {
            self.move_window(r);
        }
    }

    /// The window at `r` (the panel's rectangle, without the shadow
    /// margin). Only through WinFit's steps (Panel::fit knows where it is).
    fn move_window(&self, r: RECT) {
        let m = self.margin();
        unsafe {
            let _ = SetWindowPos(self.hwnd, None, r.left - m, r.top - m, r.right - r.left + m * 2, r.bottom - r.top + m * 2, SWP_NOZORDER | SWP_NOACTIVATE);
        }
    }

    /// Goes into list `want` (None: back to the grid): the panel stays
    /// where it is (taller only for a list that needs it) and the views
    /// cross in place, the one leaving on FADE_OUT_MS, the one coming on
    /// CROSS_MS (reduced motion: 60 ms straight). The focus goes to the
    /// list's back button, or back to what went into the list.
    fn set_section(&mut self, want: Option<Section>) {
        if want == self.open {
            return;
        }
        let from = self.open;
        self.open = want;
        self.tile_clicks = TileClicks::default();
        if from == Some(Section::Bluetooth) || want == Some(Section::Bluetooth) {
            self.shared.set_quiet(|r| r.bt_audio_ready = false);
            self.shared.bt_list_open.store(want == Some(Section::Bluetooth), Ordering::Release);
            let _ = self.workers.devices.send(DeviceCmd::ListChanged);
        }
        self.scroll = 0.0;
        if want == Some(Section::Wifi) {
            let _ = self.workers.radio.send(RadioCmd::WifiList);
        }
        self.hide_tip();
        self.hover = None;
        self.pressed = None;
        self.sliding = None;
        self.app_drag = None;
        self.focus = match want {
            Some(_) => Some(Hit::Back),
            None => from.map(section_card),
        };
        self.resize(false);
        if self.reveal_focus() {
            self.redraw();
        }
        let (coming, going) = if want.is_some() { (&self.v.sub, &self.v.main) } else { (&self.v.main, &self.v.sub) };
        let c = &self.gfx.compositor;
        let r = (|| -> Result<()> {
            fade_scalar(c, &obj(coming)?, "Opacity", 1.0, crate::motion::CROSS_MS, EASE_OUT)?;
            fade_scalar(c, &obj(going)?, "Opacity", 0.0, crate::motion::FADE_OUT_MS, EASE_EXIT)
        })();
        if let Err(e) = r {
            crate::log!("control: views: {e}");
            let _ = coming.SetOpacity(1.0);
            let _ = going.SetOpacity(0.0);
        }
    }

    /// Takes every part away (they are made again by the next redraw).
    fn drop_parts(&self) {
        for host in [&self.v.parts_main, &self.v.parts_sub] {
            let _ = host.Children().and_then(|c| c.RemoveAll());
        }
        *self.parts.borrow_mut() = Parts::default();
        *self.sub_parts.borrow_mut() = Parts { sub: true, ..Default::default() };
    }

    // ---- colors ----

    fn hc(&self) -> Option<crate::syslook::HighContrast> {
        self.theme.hc.filter(|_| self.theme.high_contrast)
    }
    fn tones(&self) -> Tones {
        tones_of(&self.theme)
    }

    // ---- drawing ----

    fn redraw(&mut self) {
        self.resize(false);
    }

    /// Draws the fitted view from the same state used for sizing. The
    /// other view keeps its last picture while
    /// it fades.
    fn draw(&mut self, remote: &Remote, layout: &Layout) {
        let s = self.ds();
        let (pw, ph) = ((W * s).ceil() as u32, (self.height * s).ceil() as u32);
        let mut hits = Vec::new();
        let mut sliders = Vec::new();
        let me = &*self;
        let surface = self.gfx.draw(pw, ph, |dc, ox, oy| unsafe {
            dc.SetAntialiasMode(D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
            dc.SetTransform(&if me.rtl { mirrored(s, ox, oy) } else { Matrix3x2 { M11: s, M12: 0.0, M21: 0.0, M22: s, M31: ox, M32: oy } });
            let r = me.paint(dc, &mut hits, &mut sliders, remote, layout);
            dc.SetTransform(&Matrix3x2::identity());
            r
        });
        if self.open.is_none() {
            let (left, top) = (self.rect.left as f32, self.rect.top as f32);
            let inert = self.inert.borrow();
            let positions = hits.iter().copied().chain(inert.iter().map(|&(_, rc, hit)| (rc, hit)))
                .filter(|&(_, hit)| hit.tile())
                .map(|(rc, hit)| {
                    let (l, r) = crate::i18n::mirror::edges(self.rtl, rc.left, rc.right, 0.0, W);
                    (hit, rect(left + l * s, top + (rc.top + self.scroll) * s, left + r * s, top + (rc.bottom + self.scroll) * s))
                });
            let now = Instant::now();
            self.tile_clicks.update(positions, now);
            if self.pressed.is_some_and(|hit| self.tile_clicks.blocked(hit, now)) {
                self.pressed = None;
            }
        }
        self.hits = hits;
        self.sliders = sliders;
        let (view, content) = if self.open.is_some() { (&self.v.sub, &self.v.content_sub) } else { (&self.v.main, &self.v.content_main) };
        match surface {
            Ok(surface) => {
                if let Ok(brush) = self.gfx.compositor.CreateSurfaceBrushWithSurface(&surface) {
                    let _ = brush.SetStretch(CompositionStretch::Fill);
                    let _ = content.SetBrush(&brush);
                    // As tall as it was drawn (the view leaving keeps its
                    // own picture's size while the plate glides).
                    let size = Vector2 { X: W, Y: self.height };
                    let _ = content.SetSize(size);
                    let _ = view.SetSize(size);
                }
            }
            Err(e) => crate::log!("control: draw failed: {e}"),
        }
        let plan = std::mem::take(&mut *self.plan.borrow_mut());
        self.sync_parts(plan, self.open.is_some());
        self.sync_tick();
        // The focused control went (a device left, the list was left): the
        // focus goes to what went into the list.
        let shown = |h| self.hits.iter().any(|x| x.1 == h);
        let home = if self.open.is_some() && self.focus.is_some_and(|h| !shown(h)) {
            Some(Hit::Back)
        } else {
            focus_home(self.focus, None, shown)
        };
        if home != self.focus {
            self.focus = home;
            if self.keyboard {
                return self.draw(remote, layout);
            }
        }
        self.publish();
    }

    /// While a track with a timeline plays: a tick when its position bar
    /// moves a second on (and none otherwise, nor at its end: the next
    /// timeline or playback event starts it again, through sync).
    fn sync_tick(&mut self) {
        let r = self.shared.snapshot();
        let now = std::time::Instant::now();
        let next = r.media.as_ref().filter(|_| self.open.is_none())
            .and_then(|m| m.timeline.as_ref().and_then(|tl| tick_in(tl, m.playing, now)));
        unsafe {
            match next {
                Some(ms) => {
                    SetTimer(Some(self.hwnd), TICK_TIMER, ms.max(16), None);
                    self.ticking = true;
                }
                None if self.ticking => {
                    let _ = KillTimer(Some(self.hwnd), TICK_TIMER);
                    self.ticking = false;
                }
                None => {}
            }
        }
    }

    /// The time played moved on: only the position bar is drawn again.
    fn tick(&mut self) {
        let r = self.shared.snapshot();
        let lay = self.layout_now(&r);
        if let (Some(m), Some(strip)) = (&r.media, lay.at(Item::Media).filter(|_| self.open.is_none())) {
            if let Some(spec) = self.time_spec(m, shifted_y(strip, -self.scroll)) {
                self.sync_parts_only(vec![Spec::Time(spec)]);
            }
        }
        self.sync_tick();
    }

    // ---- the tooltip (300 ms, then at once within TIP_WARM_MS) ----

    /// The pointer moved to `h`: the tooltip shown goes, and `h`'s (if it
    /// has one) waits its time, or shows at once soon after another.
    fn tip_follow(&mut self, h: Option<Hit>) {
        if self.tip_for == h {
            return;
        }
        self.hide_tip();
        let Some(h) = h.filter(|h| self.tips.borrow().iter().any(|t| t.0 == *h)) else { return };
        self.tip_for = Some(h);
        let warm = crate::motion::tip_warm(self.tip_closed.map(|t| t.elapsed()));
        if warm {
            self.show_tip();
        } else {
            unsafe {
                SetTimer(Some(self.hwnd), TIP_TIMER, crate::motion::TIP_DELAY_MS as u32, None);
            }
        }
    }

    /// The tooltip of the control waited for, centered under it (EDGE_GAP).
    fn show_tip(&mut self) {
        let Some(h) = self.tip_for else { return };
        if self.pressed.is_some() || self.sliding.is_some() {
            return;
        }
        let Some(text) = self.tips.borrow().iter().find(|t| t.0 == h).map(|t| t.1.clone()) else { return };
        let Some(&(rc, _)) = self.hits.iter().find(|x| x.1 == h) else { return };
        let ds = self.ds();
        let (l, r) = crate::i18n::mirror::edges(self.rtl, rc.left, rc.right, 0.0, W);
        let at = POINT {
            x: self.rect.left + ((l + r) / 2.0 * ds).round() as i32,
            y: self.rect.top + ((rc.bottom + crate::theme::metrics::size::EDGE_GAP) * ds).round() as i32,
        };
        super::desktops::show_tip_at(&self.gfx, &self.theme, self.scale, &text, at, false, self.work);
        self.tip_shown = true;
    }

    fn hide_tip(&mut self) {
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TIP_TIMER);
        }
        self.tip_for = None;
        if std::mem::take(&mut self.tip_shown) {
            super::desktops::hide_tip();
            self.tip_closed = Some(std::time::Instant::now());
        }
    }
}

/// Pure: the grid's shape for what the workers found (`open`: the list the
/// panel is in), with `removable` devices.
fn shape_of(r: &Remote, open: Option<Section>, removable: usize) -> Shape {
    Shape {
        tiles: grid_tiles(r),
        brightness: Panel::show_display(r),
        media: r.media.is_some(),
        removable,
        section: open.map(|s| {
            let h = sub_h(&section_cards(s, r));
            (s, if s.has_settings_footer() { h.max(400.0) } else { h })
        }),
    }
}

struct TileView {
    glyph: Glyph,
    label: &'static str,
    note: Option<&'static str>,
    on: bool,
    enabled: bool,
    disclosure: bool,
    external: bool,
}

/// A closed panel's fade is over: its window goes.
fn closed(hwnd: HWND) {
    CLOSING.with(|c| c.borrow_mut().retain(|e| e.0 != hwnd));
    unsafe {
        let _ = DestroyWindow(hwnd);
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_TIMER if wp.0 == CLOSE_TIMER => {
            let _ = KillTimer(Some(hwnd), CLOSE_TIMER);
            closed(hwnd);
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            with_cc_at(hwnd, |p| p.on_move(lp));
            LRESULT(0)
        }
        WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN => {
            with_cc_at(hwnd, |p| p.on_button(hwnd, lp, true, msg == WM_LBUTTONDOWN));
            LRESULT(0)
        }
        WM_LBUTTONUP | WM_RBUTTONUP => {
            with_cc_at(hwnd, |p| p.on_button(hwnd, lp, false, msg == WM_LBUTTONUP));
            LRESULT(0)
        }
        WM_MOUSEWHEEL => {
            with_cc_at(hwnd, |p| p.on_wheel(((wp.0 >> 16) as i16) > 0));
            LRESULT(0)
        }
        WM_KEYDOWN | WM_SYSKEYDOWN => {
            if GetMessageExtraInfo().0 as usize != crate::keys::OWN_INPUT {
                with_cc_at(hwnd, |p| p.on_key(wp.0 as u16));
            }
            LRESULT(0)
        }
        WM_CC_REMOTE => {
            with_cc_at(hwnd, |p| p.sync());
            LRESULT(0)
        }
        WM_POWERBROADCAST if wp.0 == PBT_POWERSETTINGCHANGE as usize => {
            with_cc_at(hwnd, |p| {
                PowerWatch::update(&p.shared, lp);
                p.redraw();
            });
            LRESULT(1)
        }
        WM_SETTINGCHANGE => {
            with_cc_at(hwnd, |p| { let _ = p.workers.system.send(SystemCmd::AccessRefresh); });
            DefWindowProcW(hwnd, msg, wp, lp)
        }
        WM_DISPLAYCHANGE => {
            // A display came or went: Project's card follows.
            with_cc_at(hwnd, |p| {
                let _ = p.workers.quick.send(QuickCmd::Displays);
            });
            LRESULT(0)
        }
        WM_GETOBJECT => {
            let answer = a11y::on_getobject(hwnd, wp, lp, || {
                with_cc_at(hwnd, |p| p.publish());
            });
            answer.unwrap_or_else(|| DefWindowProcW(hwnd, msg, wp, lp))
        }
        m if m == a11y::wm_action() => {
            for (key, act) in a11y::take(hwnd) {
                with_cc_at(hwnd, |p| p.a11y_act(&key, act));
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            a11y::forget(hwnd);
            DefWindowProcW(hwnd, msg, wp, lp)
        }
        WM_TIMER if wp.0 == NOTE_TIMER => {
            let _ = KillTimer(Some(hwnd), NOTE_TIMER);
            with_cc_at(hwnd, |p| p.sync());
            LRESULT(0)
        }
        WM_TIMER if wp.0 == EJECT_TIMER => {
            // A note ran out: a card may go (and the panel shrink with it).
            let _ = KillTimer(Some(hwnd), EJECT_TIMER);
            with_cc_at(hwnd, |p| p.sync());
            LRESULT(0)
        }
        WM_TIMER if wp.0 == TIP_TIMER => {
            let _ = KillTimer(Some(hwnd), TIP_TIMER);
            with_cc_at(hwnd, |p| p.show_tip());
            LRESULT(0)
        }
        WM_TIMER if wp.0 == TICK_TIMER => {
            let _ = KillTimer(Some(hwnd), TICK_TIMER);
            with_cc_at(hwnd, |p| {
                p.ticking = false;
                p.tick();
            });
            LRESULT(0)
        }
        WM_CLOSE => {
            with_cc_at(hwnd, |p| p.finish(Outcome::Closed, None));
            LRESULT(0)
        }
        WM_ACTIVATE if (wp.0 & 0xFFFF) as u32 == WA_INACTIVE => {
            with_cc_at(hwnd, |p| {
                p.done.get_or_insert(Outcome::Lost);
            });
            LRESULT(0)
        }
        WM_CAPTURECHANGED => {
            let now = GetCapture();
            with_cc_at(hwnd, |p| {
                if now != p.hwnd {
                    p.done.get_or_insert(Outcome::Lost);
                }
            });
            LRESULT(0)
        }
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}


#[cfg(test)]
mod tests;

/// Pictures for the design audit (tests only).
#[cfg(test)]
#[path = "control_harmony.rs"]
mod harmony;
#[cfg(test)]
pub(super) use harmony::paint_site_control;
