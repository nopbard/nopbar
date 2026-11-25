//! Layout for the tile grid, media, sliders and in-panel lists.

use super::*;

/// A list the panel went into (in place of the grid).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Section {
    /// Project's display modes.
    Project,
    /// The output and input devices, and the apps' volumes.
    Sound,
    Wifi,
    Bluetooth,
    Accessibility,
}

impl Section {
    pub(super) fn has_settings_footer(self) -> bool {
        matches!(self, Self::Wifi | Self::Bluetooth | Self::Accessibility)
    }
}

pub(super) fn shifted_y(r: D2D_RECT_F, dy: f32) -> D2D_RECT_F {
    rect(r.left, r.top + dy, r.right, r.bottom + dy)
}

pub(super) fn clip_y(r: D2D_RECT_F, top: f32, bottom: f32) -> Option<D2D_RECT_F> {
    let clipped = rect(r.left, r.top.max(top), r.right, r.bottom.min(bottom));
    (clipped.top < clipped.bottom).then_some(clipped)
}

pub(super) fn reveal_offset(r: D2D_RECT_F, top: f32, bottom: f32, offset: f32, max: f32) -> f32 {
    let shift = if r.top < top || r.bottom - r.top > bottom - top {
        r.top - top
    } else {
        (r.bottom - bottom).max(0.0)
    };
    (offset + shift).clamp(0.0, max)
}

/// What fills a place on the grid.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Item {
    Connections,
    Tile(Hit),
    Brightness,
    Volume,
    Media,
    /// A removable device (its index in removable::rows()).
    Eject(usize),
}

/// The visible tiles, optional strips and the open list.
#[derive(Clone, Debug, PartialEq, Default)]
pub(super) struct Shape {
    pub(super) tiles: Vec<Hit>,
    pub(super) brightness: bool,
    pub(super) media: bool,
    pub(super) removable: usize,
    pub(super) section: Option<(Section, f32)>,
}

/// The grid's items (with their place), its height, the panel's (as tall
/// as the grid, or as the open list when that is taller) and the panel at
/// rest (rest_h: what its size is held to, panel_size).
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Layout {
    pub(super) items: Vec<(Item, D2D_RECT_F)>,
    pub(super) grid_h: f32,
    pub(super) height: f32,
    pub(super) rest: f32,
    /// What the panel's size is held to (place_fit): the grid's height, or
    /// the list's when that is taller.
    pub(super) fit: f32,
}

impl Layout {
    pub(super) fn at(&self, item: Item) -> Option<D2D_RECT_F> {
        self.items.iter().find(|x| x.0 == item).map(|x| x.1)
    }
}

pub(super) fn cell(col: usize, row: usize, cols: usize, rows: usize) -> D2D_RECT_F {
    let x = PAD + col as f32 * (UNIT + GAP);
    let y = PAD + row as f32 * (UNIT + GAP);
    let span = |n: usize| n as f32 * (UNIT + GAP) - GAP;
    rect(x, y, x + span(cols), y + span(rows))
}

pub(super) fn connection_rects(card: D2D_RECT_F, n: usize) -> impl Iterator<Item = D2D_RECT_F> {
    let top = card.top + CONN_PAD;
    let h = (card.bottom - CONN_PAD - top) / n.max(1) as f32;
    (0..n).map(move |i| rect(card.left + CONN_PAD, top + i as f32 * h, card.right - CONN_PAD, top + (i + 1) as f32 * h))
}

pub(super) fn connection_chip(row: D2D_RECT_F) -> D2D_RECT_F {
    let x = row.left + ROW_IN / 2.0;
    let y = (row.top + row.bottom - CHIP) / 2.0;
    rect(x, y, x + CHIP, y + CHIP)
}

pub(super) fn layout(s: &Shape) -> Layout {
    let mut items = vec![(Item::Connections, cell(0, 0, 2, 2))];
    let (mut row, mut col) = (0, 2);
    for &hit in &s.tiles {
        let width = if hit == Hit::Project { 2 } else { 1 };
        if col + width > COLS {
            row += 1;
            col = if row < 2 { 2 } else { 0 };
        }
        items.push((Item::Tile(hit), cell(col, row, width, 1)));
        col += width;
    }
    row = (row + 1).max(2);
    let strips = s.brightness.then_some(Item::Brightness).into_iter()
        .chain([Item::Volume])
        .chain(s.media.then_some(Item::Media))
        .chain((0..s.removable).map(Item::Eject));
    for item in strips {
        items.push((item, cell(0, row, COLS, 1)));
        row += 1;
    }
    let grid_h = cell(0, 0, COLS, row).bottom + PAD;
    let height = s.section.map_or(grid_h, |(_, h)| h);
    let rest = grid_h + if s.media { 0.0 } else { UNIT + GAP };
    Layout { items, grid_h, height, rest, fit: grid_h.max(height) }
}

#[cfg(test)]
pub(super) fn rest_h(s: &Shape) -> f32 {
    layout(s).rest
}

/// Pure: a strip's thick slider (CAPSULE tall, around its middle) from
/// STRIP_IN to STRIP_IN from the end, or (`button`) to a round button's
/// width and a gap before it; and that button (ROUND_L).
pub(super) fn strip_slider(strip: D2D_RECT_F, button: bool) -> (D2D_RECT_F, Option<D2D_RECT_F>) {
    let cy = (strip.top + strip.bottom) / 2.0;
    let end = strip.right - STRIP_IN;
    let (right, b) = if button { (end - ROUND_L - GAP, Some(rect(end - ROUND_L, cy - ROUND_L / 2.0, end, cy + ROUND_L / 2.0))) } else { (end, None) };
    (rect(strip.left + STRIP_IN, cy - CAPSULE / 2.0, right, cy + CAPSULE / 2.0), b)
}

/// Pure: the volume slider's mute button: a circle MUTE round, inset
/// (CAPSULE - MUTE) / 2 at the slider's start, around its glyph.
pub(super) fn mute_in(slider: D2D_RECT_F) -> D2D_RECT_F {
    let i = (CAPSULE - MUTE) / 2.0;
    rect(slider.left + i, slider.top + i, slider.left + i + MUTE, slider.top + i + MUTE)
}

/// Pointer endpoints, with half the slider height held at either end.
pub(super) fn track_span(slider: D2D_RECT_F) -> (f32, f32) {
    let inset = (slider.bottom - slider.top) / 2.0;
    (slider.left + inset, slider.right - inset)
}

pub(super) fn slider_value(x: f32, x0: f32, x1: f32) -> f32 {
    if x1 <= x0 {
        return 0.0;
    }
    ((x - x0) / (x1 - x0)).clamp(0.0, 1.0)
}

pub(super) fn cap_value(x: f32, slider: D2D_RECT_F) -> f32 {
    let (start, end) = track_span(slider);
    slider_value(x, start, end)
}

/// Where the parts of the Now Playing strip go.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct MediaPlan {
    pub(super) art: D2D_RECT_F,
    /// The text column: its left edge, width, the title's and the
    /// artist's line tops.
    pub(super) text_x: f32,
    pub(super) text_w: f32,
    pub(super) title_top: f32,
    pub(super) artist_top: f32,
    pub(super) buttons: [(Hit, D2D_RECT_F); 3],
    /// The position bar (TRACK_H tall; None: no timeline).
    pub(super) bar: Option<D2D_RECT_F>,
}

/// Art and metadata at the start, transport controls at the end.
pub(super) fn media_plan(strip: D2D_RECT_F, bar: bool, rtl: bool) -> MediaPlan {
    let cy = (strip.top + strip.bottom) / 2.0;
    let ax = strip.left + MEDIA_IN;
    let art = rect(ax, cy - ART_DIP / 2.0, ax + ART_DIP, cy + ART_DIP / 2.0);
    let sizes = [MEDIA_BTN, PLAY_BTN, MEDIA_BTN];
    let row = sizes.iter().sum::<f32>() + MEDIA_BTN_GAP * 2.0;
    let mut x = strip.right - MEDIA_IN - row;
    let order = if rtl { [Hit::Next, Hit::PlayPause, Hit::Prev] } else { [Hit::Prev, Hit::PlayPause, Hit::Next] };
    let buttons = std::array::from_fn(|k| {
        let d = sizes[k];
        let r = rect(x, cy - d / 2.0, x + d, cy + d / 2.0);
        x += d + MEDIA_BTN_GAP;
        (order[k], r)
    });
    let text_x = art.right + MEDIA_IN;
    let text_w = (strip.right - MEDIA_IN - row - MEDIA_IN - text_x).max(0.0);
    let block = TITLE_H + ARTIST_H + if bar { BAR_GAP + TRACK_H } else { 0.0 };
    let title_top = cy - block / 2.0;
    let artist_top = title_top + TITLE_H;
    let bar = bar.then(|| {
        let y = artist_top + ARTIST_H + BAR_GAP;
        rect(text_x, y, text_x + text_w, y + TRACK_H)
    });
    MediaPlan { art, text_x, text_w, title_top, artist_top, buttons, bar }
}

/// Pure: a removable device's strip: its glyph's middle, the text's start
/// and widest, and the eject button (EJECT_BTN round, MEDIA_IN from the
/// end).
pub(super) fn eject_columns(strip: D2D_RECT_F) -> (f32, f32, f32, D2D_RECT_F) {
    let cy = (strip.top + strip.bottom) / 2.0;
    let gx = strip.left + STRIP_IN + TILE_GLYPH / 2.0;
    let x = strip.left + STRIP_IN + TILE_GLYPH + MEDIA_IN;
    let b = rect(strip.right - MEDIA_IN - EJECT_BTN, cy - EJECT_BTN / 2.0, strip.right - MEDIA_IN, cy + EJECT_BTN / 2.0);
    (gx, x, (b.left - MEDIA_IN - x).max(0.0), b)
}

/// Pure: two lines (a name TITLE_H and a value ARTIST_H tall) around `cy`:
/// their tops.
pub(super) fn two_lines(cy: f32) -> (f32, f32) {
    let top = cy - (TITLE_H + ARTIST_H) / 2.0;
    (top, top + TITLE_H)
}

// ---- the lists (Project's modes, the sound devices and apps) ---------------------

/// A line in a list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Line {
    /// A heading (a locale key).
    Head(&'static str),
    Mode(Topology),
    Device(Flow, u64),
    /// Windows would not switch to a device of this direction.
    Failed(Flow),
    SoundSettings,
    App(u64),
    Control(Hit),
    Status(&'static str),
}

/// Pure: the cards of list `s` for what is known now: Project's four
/// modes; or the output devices, the input devices under their heading, a
/// failed switch with the way to Windows' Sound page, and (a card of their
/// own) the apps' volumes while more than one plays.
pub(super) fn section_cards(s: Section, r: &Remote) -> Vec<Vec<Line>> {
    match s {
        Section::Project => vec![Topology::ALL.map(Line::Mode).to_vec()],
        Section::Wifi => {
            use super::super::panels::WifiError;
            let off = !r.radio_busy[0]
                && matches!(r.wifi, RadioView::Off | RadioView::Locked(false) | RadioView::Denied(false));
            let lines = if off {
                vec![Line::Status("control.off")]
            } else if r.wifi == RadioView::Missing {
                vec![Line::Status("control.wifi_unavailable")]
            } else {
                match r.wifi_networks.as_ref() {
                    Some(Ok(networks)) if !networks.is_empty() => networks.iter().map(|n| Line::Control(Hit::WifiRow(wifi_key(n)))).collect(),
                    Some(Ok(_)) => vec![Line::Status("topbar.panel.network.none")],
                    Some(Err(WifiError::Denied)) => vec![Line::Status("topbar.panel.network.location_needed"), Line::Control(Hit::WifiLocation)],
                    Some(Err(WifiError::Service)) => vec![Line::Status("control.wifi_unavailable")],
                    Some(Err(WifiError::Slow)) => vec![Line::Status("control.list_failed"), Line::Control(Hit::WifiRefresh)],
                    None | Some(Err(WifiError::Pending)) => vec![Line::Status("control.loading")],
                }
            };
            vec![lines, vec![Line::Control(Hit::WifiSettings)]]
        }
        Section::Bluetooth => {
            let lines = r.bt_devices.iter().map(|d| Line::Control(Hit::BtDevice(d.key))).collect::<Vec<_>>();
            let settings = vec![Line::Control(Hit::BluetoothSettings)];
            if lines.is_empty() { vec![settings] } else { vec![lines, settings] }
        }
        Section::Accessibility => {
            let lines = Access::ALL.iter().map(|&a| Line::Control(Hit::Access(a))).collect();
            vec![lines, vec![Line::Control(Hit::AccessibilitySettings)]]
        }
        Section::Sound => {
            let mut v = Vec::new();
            let outputs = device_list(r, Flow::Output);
            let mut failed = false;
            if !outputs.is_empty() || fail_for(r, Flow::Output).is_some() {
                v.push(Line::Head("control.output"));
                v.extend(outputs.iter().map(|e| Line::Device(Flow::Output, device_key(&e.id))));
                if fail_for(r, Flow::Output).is_some() {
                    v.push(Line::Failed(Flow::Output));
                    failed = true;
                }
            }
            let inputs = device_list(r, Flow::Input);
            if !inputs.is_empty() || fail_for(r, Flow::Input).is_some() {
                v.push(Line::Head("control.input"));
                v.extend(inputs.iter().map(|e| Line::Device(Flow::Input, device_key(&e.id))));
                if fail_for(r, Flow::Input).is_some() {
                    v.push(Line::Failed(Flow::Input));
                    failed = true;
                }
            }
            if failed {
                v.push(Line::SoundSettings);
            }
            let mut cards = vec![v];
            let apps = mixer_apps(r.apps.as_deref());
            if !apps.is_empty() {
                let mut a = vec![Line::Head("control.app_volume")];
                a.extend(apps.iter().map(|x| Line::App(x.key)));
                cards.push(a);
            }
            cards.retain(|c| !c.is_empty());
            cards
        }
    }
}

/// Pure: a heading's node key for screen readers (stable, one per heading).
pub(super) fn head_node(key: &'static str) -> &'static str {
    match key {
        "control.output" => "text:head_output",
        "control.input" => "text:head_input",
        "control.app_volume" => "text:head_app_volume",
        _ => "text:head",
    }
}

/// A device's name for screen readers, with its direction ("Output:
/// Headset").
pub(super) fn device_name(flow: Flow, name: &str) -> String {
    let dir = t(if flow == Flow::Output { "control.output" } else { "control.input" });
    tf("control.device_flow", &[("flow", dir), ("name", name)])
}

pub(super) fn line_h(l: Line) -> f32 {
    match l {
        Line::Head(_) => HEAD_H,
        Line::App(_) => APP_ROW_H,
        Line::Control(Hit::BtDevice(_) | Hit::WifiRow(_)) => 44.0,
        _ => LINE_H,
    }
}

/// Pure: the list's header (the back button and the title) and each
/// card's place with its lines' tops, from the panel's top.
pub(super) fn sub_plan(cards: &[Vec<Line>]) -> (D2D_RECT_F, Vec<(D2D_RECT_F, Vec<f32>)>) {
    let head = rect(PAD, PAD, W - PAD, PAD + SUB_HEAD_H);
    let mut y = head.bottom + GAP;
    let mut out = Vec::with_capacity(cards.len());
    for lines in cards {
        let top = y;
        let mut ly = top + LIST_PAD;
        let tops = lines
            .iter()
            .map(|l| {
                let at = ly;
                ly += line_h(*l);
                at
            })
            .collect();
        let card = rect(PAD, top, W - PAD, ly + LIST_PAD);
        y = card.bottom + GAP;
        out.push((card, tops));
    }
    (head, out)
}

/// Pure: the list's height (as a panel: PAD under its last card).
pub(super) fn sub_h(cards: &[Vec<Line>]) -> f32 {
    let (head, list) = sub_plan(cards);
    list.last().map_or(head.bottom, |c| c.0.bottom) + PAD
}

/// Pure: the back button in the list's header `head` (ROUND_S, its
/// focus ring's reach in, around its middle): the ring stays out of the
/// panel's padding.
pub(super) fn back_in(head: D2D_RECT_F) -> D2D_RECT_F {
    let cy = (head.top + head.bottom) / 2.0;
    let x = head.left + RING_OUT;
    rect(x, cy - ROUND_S / 2.0, x + ROUND_S, cy + ROUND_S / 2.0)
}

/// An app's row's columns (DIP, left to right): the name's left and
/// widest, the slider's track ends, the percent's end.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct AppColumns {
    pub(super) name: f32,
    pub(super) name_w: f32,
    pub(super) track: (f32, f32),
    pub(super) value_end: f32,
}

/// Pure: an app's row `lr`: ROW_IN in, the name (APP_NAME_W), ROW_IN, the
/// track, ROW_IN, the percent (APP_VALUE_W) ROW_IN from the end.
pub(super) fn app_columns(lr: D2D_RECT_F) -> AppColumns {
    let name = lr.left + ROW_IN;
    let x0 = name + APP_NAME_W + ROW_IN;
    let value_end = lr.right - ROW_IN;
    let x1 = (value_end - APP_VALUE_W - ROW_IN).max(x0);
    AppColumns { name, name_w: APP_NAME_W, track: (x0, x1), value_end }
}

// ---- the rest -------------------------------------------------------------------

/// Pure: ms to the next tick of a track's time played: None while paused,
/// without a bar, or at its end (it no longer moves there; the next
/// timeline or playback event starts the ticks again).
pub(super) fn tick_in(tl: &Timeline, playing: bool, now: std::time::Instant) -> Option<u32> {
    if !playing || !tl.shown() {
        return None;
    }
    let pos = tl.position(now, true);
    (pos < tl.end_ms).then(|| crate::topbar::island::model::ms_to_next_second(pos - tl.start_ms, tl.rate))
}

#[cfg(test)]
thread_local! {
    /// Made-up removable devices for the design audit's pictures.
    pub(super) static FAKE_ROWS: std::cell::RefCell<Option<Vec<crate::removable::model::Shown>>> = const { std::cell::RefCell::new(None) };
}

/// The removable devices now (crate::removable's; the pictures' made-up
/// ones in tests).
pub(super) fn removable_now() -> Vec<crate::removable::model::Shown> {
    #[cfg(test)]
    if let Some(v) = FAKE_ROWS.with(|f| f.borrow().clone()) {
        return v;
    }
    crate::removable::rows()
}

/// The removable storage strips now (0: none).
pub(super) fn removable_rows() -> usize {
    let rows = removable_now();
    if crate::removable::model::section_visible(&rows) { rows.len() } else { 0 }
}

/// A removable device's key for Hit::Eject: its connection (a press or
/// focus on a device unplugged and back is not carried over to the new
/// connection).
pub(super) fn removable_key(d: &crate::removable::model::Device) -> u64 {
    device_key(&format!("{}|{}", d.id, d.gen))
}

/// A removable device's glyph.
pub(super) fn removable_glyph(k: crate::removable::model::Kind) -> Glyph {
    use crate::removable::model::Kind;
    match k {
        Kind::Usb => Glyph::UsbDrive,
        Kind::Card => Glyph::SdCard,
        Kind::Disc => Glyph::Disc,
    }
}

/// What a removable device's second line says: its letters and labels, or
/// the eject's note.
pub(super) fn removable_note(row: &crate::removable::model::Shown) -> String {
    use crate::removable::model::{refusal_words, Note};
    match &row.note {
        Note::Ready => row.device.detail(),
        Note::Ejecting => t("control.eject.ejecting").to_string(),
        Note::Safe => t("control.eject.safe").to_string(),
        Note::Refused(r) => match refusal_words(r) {
            (key, Some(name)) => tf(key, &[("name", name)]),
            (key, None) => t(key).to_string(),
        },
    }
}

/// The failed switch to report under direction `f`'s devices: the
/// device's name ("" when it went away before the switch).
pub(super) fn fail_for(r: &Remote, f: Flow) -> Option<&str> {
    r.audio_fail.as_ref().filter(|(fl, _)| *fl == f).map(|(_, n)| n.as_str())
}

/// The devices of one direction, all of them (a long list makes the panel
/// taller; place() shrinks it to fit the screen).
pub(super) fn device_list(r: &Remote, f: Flow) -> &[Endpoint] {
    match f {
        Flow::Output => r.outputs.as_deref(),
        Flow::Input => r.inputs.as_deref(),
    }
    .unwrap_or(&[])
}

/// A device's stable key for Hit::Device.
pub(super) fn device_key(id: &str) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    id.hash(&mut h);
    h.finish()
}

/// Pure: whether a failed device switch (`fail`: its number,
/// Remote::audio_fail_seq; None: none) takes the panel, showing `open`,
/// into the sound list now. Each failure once (`shown`: the last one shown
/// there, kept up to date): from the grid, or when the list is open as it
/// comes; one shown and left stays left (the 2026-11-11 review: an
/// unrelated update took the user back in). One that comes while another
/// list is open waits for the grid.
pub(super) fn fail_leads_in(fail: Option<u64>, open: Option<Section>, shown: &mut u64) -> bool {
    let Some(n) = fail else { return false };
    if *shown == n {
        return false;
    }
    match open {
        Some(Section::Sound) => {
            *shown = n;
            false
        }
        None => {
            *shown = n;
            true
        }
        Some(_) => false,
    }
}

/// Pure: a removable device's eject takes input (it can be asked: ready,
/// or refused and tried again); on its way or done it does not, but is
/// still read (Panel::inert).
pub(super) fn eject_ready(note: &crate::removable::model::Note) -> bool {
    use crate::removable::model::Note;
    matches!(note, Note::Ready | Note::Refused(_))
}

/// Records control `h` at `r`: with the controls that take input, or
/// (`input` false) with those only read, after the input ones so far.
pub(super) fn add_control(hits: &mut Vec<(D2D_RECT_F, Hit)>, inert: &mut Vec<(usize, D2D_RECT_F, Hit)>, r: D2D_RECT_F, h: Hit, input: bool) {
    if input {
        hits.push((r, h));
    } else {
        inert.push((hits.len(), r, h));
    }
}

/// What the reader's tree is made of, in drawing order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Read {
    /// A control that takes input (an index into the hits).
    Hit(usize),
    /// A text line (into the texts).
    Text(usize),
    /// A control drawn that takes no input now (into the inert ones).
    Inert(usize),
}

/// Pure: drawing_order with the inert controls (each after the first
/// `inert_at[k]` hits, after the texts there).
pub(super) fn reading_order(hits: usize, text_at: &[usize], inert_at: &[usize]) -> Vec<Read> {
    let at: Vec<usize> = text_at.iter().chain(inert_at).copied().collect();
    drawing_order(hits, &at)
        .into_iter()
        .map(|(extra, i)| match (extra, i.checked_sub(text_at.len())) {
            (false, _) => Read::Hit(i),
            (true, None) => Read::Text(i),
            (true, Some(k)) => Read::Inert(k),
        })
        .collect()
}

/// Pure: controls (`hits` of them) and text lines (each drawn after the
/// first `text_at[k]` controls) in drawing order, as (is a text, index).
pub(super) fn drawing_order(hits: usize, text_at: &[usize]) -> Vec<(bool, usize)> {
    let mut out = Vec::with_capacity(hits + text_at.len());
    for i in 0..=hits {
        out.extend(text_at.iter().enumerate().filter(|(_, &a)| a == i).map(|(k, _)| (true, k)));
        if i < hits {
            out.push((false, i));
        }
    }
    // A line recorded past the last control still comes last.
    out.extend(text_at.iter().enumerate().filter(|(_, &a)| a > hits).map(|(k, _)| (true, k)));
    out
}

/// A control's key in the screen reader tree (stable while it is shown:
/// devices by their endpoint ID's hash).
pub(super) fn hit_key(h: Hit) -> String {
    format!("{h:?}")
}

/// The list a control lies in (None: on the grid).
pub(super) fn section_of(h: Hit) -> Option<Section> {
    match h {
        Hit::Mode(_) => Some(Section::Project),
        Hit::WifiRow(_) | Hit::WifiDisconnect(_) | Hit::WifiRefresh | Hit::WifiLocation | Hit::WifiSettings => Some(Section::Wifi),
        Hit::BtDevice(_) | Hit::BluetoothSettings => Some(Section::Bluetooth),
        Hit::Access(_) | Hit::AccessibilitySettings => Some(Section::Accessibility),
        Hit::Device(..) | Hit::App(_) | Hit::SoundSettings => Some(Section::Sound),
        _ => None,
    }
}

/// The control on the grid that goes into list `s`.
pub(super) fn section_card(s: Section) -> Hit {
    match s {
        Section::Project => Hit::Project,
        Section::Sound => Hit::Outputs,
        Section::Wifi => Hit::WifiName,
        Section::Bluetooth => Hit::BtName,
        Section::Accessibility => Hit::AccessList,
    }
}

/// Pure: where the keyboard focus goes when the control it is on is no
/// longer `shown`: from a list's control (or its back button) to the
/// control that went into the list (`from`: the list left); anything else
/// keeps it.
pub(super) fn focus_home(focus: Option<Hit>, from: Option<Section>, shown: impl Fn(Hit) -> bool) -> Option<Hit> {
    match focus {
        Some(h) if !shown(h) => section_of(h).or(from.filter(|_| h == Hit::Back)).map(section_card).or(Some(h)),
        f => f,
    }
}

/// Pure: the focus screen readers are told of: the focused control only
/// while it is `published`.
pub(super) fn reader_focus(focus: Option<Hit>, published: &[Hit]) -> Option<String> {
    focus.filter(|h| published.contains(h)).map(hit_key)
}

/// The Windows build number (0 if unknown).
pub(super) fn windows_build() -> u32 {
    crate::taskbar::windows_build().parse().unwrap_or(0)
}

/// Next focusable control from `cur` in direction `dir` (wrapping).
pub(super) fn step_focus(order: &[Hit], cur: Option<Hit>, dir: i32) -> Option<Hit> {
    if order.is_empty() {
        return None;
    }
    let n = order.len() as i32;
    let i = match cur.and_then(|c| order.iter().position(|&h| h == c)) {
        Some(i) => (i as i32 + dir).rem_euclid(n),
        None if dir > 0 => 0,
        None => n - 1,
    };
    Some(order[i as usize])
}

/// Pure: a slider's new value (0..1) from `old` for a key: right (left,
/// right to left) and up a percent, left and down back, Page Up / Page
/// Down ten, Home / End the ends. None: not a slider key.
pub(super) fn slider_key(vk: u16, old: f32, rtl: bool) -> Option<f32> {
    let step = |d: f32| Some((old + d).clamp(0.0, 1.0));
    match vk {
        v if v == VK_HOME.0 => Some(0.0),
        v if v == VK_END.0 => Some(1.0),
        v if v == VK_PRIOR.0 => step(0.10),
        v if v == VK_NEXT.0 => step(-0.10),
        v if v == VK_UP.0 => step(0.01),
        v if v == VK_DOWN.0 => step(-0.01),
        v if v == VK_RIGHT.0 || v == VK_LEFT.0 => step(0.01 * crate::i18n::mirror::arrow(rtl, v == VK_RIGHT.0) as f32),
        _ => None,
    }
}

/// The wheel moves a slider this much a notch (the kit's 2 %).
pub(super) const WHEEL_STEP: f32 = 0.02;

/// Pure: DIP per layout DIP for a panel `height` tall, `rest` at rest
/// (Layout::rest), on a work area `work_w` x `work_h` DIP, at text size
/// `text` (crate::fonts::scale): BASE_SIZE grown or shrunk with the text
/// size; shrunk so the panel at rest keeps within REST_SHARE of the work
/// area, but not under MIN_SIZE; and, whatever is open, shrunk to fit the
/// work area itself (place fits it to the room past the bar's gap). In DIP
/// it is the same on every monitor of the same size in DIP, whatever their
/// scale.
pub(super) fn panel_size(work_w: f32, work_h: f32, text: f32, rest: f32, height: f32) -> f32 {
    let (work_w, work_h) = (work_w.max(1.0), work_h.max(1.0));
    let want = BASE_SIZE * text.clamp(0.5, 2.0);
    let share = (work_h * REST_SHARE_H / rest.max(1.0)).min(work_w * REST_SHARE_W / W);
    let least = MIN_SIZE.min(want);
    let size = want.min(share).max(least);
    size.min(work_h / height.max(1.0)).min(work_w / W)
}

/// Pure: whether the window is fitted again (`force`, or the panel now:
/// height, rect, size; against the one wanted: height, rect and size from
/// place).
pub(super) fn refits(force: bool, now: (f32, RECT, f32), want: (f32, RECT, f32)) -> bool {
    force || now != want
}

/// Pure: the panel only changes its height, its top where it is (a bar
/// at the top: not `up`), with full motion: it glides there (popup::
/// chrome_live) instead of being fitted at once.
pub(super) fn glides(full: bool, up: bool, now: (f32, RECT, f32), want: (f32, RECT, f32)) -> bool {
    full && !up && now.0 != want.0 && now.2 == want.2 && (now.1.left, now.1.top, now.1.right) == (want.1.left, want.1.top, want.1.right)
}

/// The window's own rectangle against the panel's (screen px). The panel
/// (Panel::rect) is where the plate is going; the window is where it is
/// (what move_window last set). While a plate glides shorter the window
/// keeps the taller size and shrinks once the plate has arrived
/// (`waiting`, Panel::shrink: the timer that does it). 2026-11-11 final
/// review 2: an update at the same height (a sound device's value)
/// cancelled that timer and fitted nothing, the window left taller than
/// its plate, its empty lower part over the windows under it.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(super) struct WinFit {
    /// Where the window is.
    pub win: RECT,
    /// A shrink to the panel waits for a shorter plate to arrive.
    pub waiting: bool,
}

/// What the window does for a step of WinFit.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(super) struct WinStep {
    /// The shrink waiting (Panel::shrink) is stopped.
    pub cancel: bool,
    /// The window is moved and sized there now.
    pub to: Option<RECT>,
    /// A shrink to the panel waits for the plate's glide to end.
    pub wait: bool,
}

/// How resize fits the panel (resize_step).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Fit {
    /// Its height glides (glides).
    Glide,
    /// Fitted at once (refits).
    Now,
    /// Nothing changed: a glide on its way goes on.
    Keep,
}

fn cover(a: RECT, b: RECT) -> RECT {
    RECT { left: a.left.min(b.left), top: a.top.min(b.top), right: a.right.max(b.right), bottom: a.bottom.max(b.bottom) }
}

impl WinFit {
    /// The window as it was made, at `rect`.
    pub fn at(rect: RECT) -> WinFit {
        WinFit { win: rect, waiting: false }
    }

    /// The plate glides to a panel at `want`: the window covers both
    /// meanwhile (grown at once if `want` reaches past it: the plate is
    /// never cut) and shrinks to `want` after (a new wait: the one
    /// before is for a place no longer wanted).
    pub fn glide(&mut self, want: RECT) -> WinStep {
        let cancel = std::mem::take(&mut self.waiting);
        let both = cover(self.win, want);
        let to = (both != self.win).then_some(both);
        self.win = both;
        self.waiting = both != want;
        WinStep { cancel, to, wait: self.waiting }
    }

    /// Fitted to `want` at once (no glide): any wait is over.
    pub fn now(&mut self, want: RECT) -> WinStep {
        let cancel = std::mem::take(&mut self.waiting);
        self.win = want;
        WinStep { cancel, to: Some(want), wait: false }
    }

    /// Nothing about the panel changed (`want` as it was): a shrink that
    /// waits keeps waiting; a window that is not where the panel is (with
    /// nothing to bring it there) is fitted.
    pub fn keep(&mut self, want: RECT) -> WinStep {
        if self.waiting || self.win == want {
            return WinStep::default();
        }
        self.win = want;
        WinStep { to: Some(want), ..Default::default() }
    }

    /// The plate arrived (the wait's timer): the window shrinks to `want`
    /// (None: nothing waited).
    pub fn arrived(&mut self, want: RECT) -> Option<RECT> {
        if !std::mem::take(&mut self.waiting) {
            return None;
        }
        let to = (self.win != want).then_some(want);
        self.win = want;
        to
    }
}

/// Pure: how resize fits the panel from `now` (height, rect, size) to
/// `want` (`force`: at once; `full`: full motion; `up`: a bar at the
/// bottom), and what the window does for it.
pub(super) fn resize_step(fit: &mut WinFit, force: bool, full: bool, up: bool, now: (f32, RECT, f32), want: (f32, RECT, f32)) -> (Fit, WinStep) {
    if !force && glides(full, up, now, want) {
        (Fit::Glide, fit.glide(want.1))
    } else if refits(force, now, want) {
        (Fit::Now, fit.now(want.1))
    } else {
        (Fit::Keep, fit.keep(want.1))
    }
}

/// Where the panel goes (screen px) and its size (DIP per layout DIP,
/// panel_size, for a monitor at `scale` and text size `text`, a panel
/// `height` tall and `rest` at rest): placement::POPUP_GAP_DIP under the
/// button (over it, `up`, for a bar at the bottom: it grows upward), at
/// the work area's right end the same gap in (right to left: its left
/// end; placement::edge_x, where the calendar opens too), sized to the
/// room left past the gap (placement::hang_room) so it never moves over
/// the gap.
/// (The panel itself places with place_fit: a list keeps the grid's size.)
#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub(super) fn place(button: RECT, work: RECT, scale: f32, text: f32, rest: f32, height: f32, rtl: bool, up: bool) -> (RECT, f32) {
    place_fit(button, work, scale, text, rest, height, height, rtl, up)
}

/// place() for a panel `height` tall whose size is held as for one `fit`
/// tall (Layout::fit: a list shorter than the grid keeps the grid's size,
/// so going into it changes the height alone).
#[allow(clippy::too_many_arguments)]
pub(super) fn place_fit(button: RECT, work: RECT, scale: f32, text: f32, rest: f32, fit: f32, height: f32, rtl: bool, up: bool) -> (RECT, f32) {
    let dips = |n: i32| n.max(1) as f32 / scale.max(0.1);
    let gap = crate::placement::popup_gap(scale);
    let room = crate::placement::hang_room(button, gap, up, work);
    // Sized against the work area as ever, then held to the room past the
    // gap (a full-height panel would otherwise move up over it).
    let fit = fit.max(height);
    let work_w = dips(work.right - work.left);
    let work_h = dips(work.bottom - work.top);
    let readable = MIN_SIZE.min(BASE_SIZE * text.clamp(0.5, 2.0)).min(work_w / W);
    let size = panel_size(work_w, work_h, text, rest, fit)
        .min(dips(room) / fit.max(1.0))
        .max(readable);
    let ds = scale * size;
    let w_px = ((W * ds).ceil() as i32).min(work.right - work.left);
    let h_px = ((height * ds).ceil() as i32).min(room);
    let x = crate::placement::edge_x(work, w_px, gap, rtl);
    let y = crate::placement::hang_y(button, h_px, gap, up, work);
    (RECT { left: x, top: y, right: x + w_px, bottom: y + h_px }, size)
}

/// Height available to the content on a fitted window, in layout units.
pub(super) fn viewport_height(height: f32, window: RECT, scale: f32) -> f32 {
    height.min((window.bottom - window.top) as f32 / scale)
}

/// The panel's transform right to left: `s` DIP scale, the surface's
/// origin at (ox, oy), x reflected across the panel's width W.
pub(super) fn mirrored(s: f32, ox: f32, oy: f32) -> Matrix3x2 {
    Matrix3x2 { M11: -s, M12: 0.0, M21: 0.0, M22: s, M31: ox + W * s, M32: oy }
}

/// `m` without the reflection `mirrored` adds (identity on a plain one).
pub(super) fn unmirror(m: Matrix3x2) -> Matrix3x2 {
    if m.M11 < 0.0 { Matrix3x2 { M11: -m.M11, M31: m.M31 + m.M11 * W, ..m } } else { m }
}

pub(super) fn rgba(c: crate::theme::Rgba) -> D2D1_COLOR_F {
    D2D1_COLOR_F { r: c.r as f32 / 255.0, g: c.g as f32 / 255.0, b: c.b as f32 / 255.0, a: c.a as f32 / 255.0 }
}

pub(super) fn ui_color(c: Rgba) -> windows::UI::Color {
    windows::UI::Color { A: c.a, R: c.r, G: c.g, B: c.b }
}

pub(super) fn rect(l: f32, tn: f32, r: f32, b: f32) -> D2D_RECT_F {
    D2D_RECT_F { left: l, top: tn, right: r, bottom: b }
}

pub(super) fn inside(r: &D2D_RECT_F, x: f32, y: f32) -> bool {
    x >= r.left && x < r.right && y >= r.top && y < r.bottom
}

pub(super) fn contains(r: &RECT, p: POINT) -> bool {
    p.x >= r.left && p.x < r.right && p.y >= r.top && p.y < r.bottom
}
