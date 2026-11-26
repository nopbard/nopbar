//! Shared monochrome local cards, rendered identically on screen and offscreen.
use super::{chat::paint::{contains, rect}, SlotAnchor};
use super::card_input::{Hooks, DISMISS};
use crate::desklayer::{backdrop, card as glass};
use std::{
    cell::RefCell,
    rc::Rc,
    time::{Duration, Instant},
};
use windows::{
    core::{w, Result},
    Win32::{
        Foundation::*,
        Graphics::Direct2D::{Common::D2D_RECT_F, ID2D1DeviceContext},
        UI::WindowsAndMessaging::*,
    },
};
#[derive(Clone, Debug)]
pub enum Route {
    Agenda(i64),
    Notes(String),
    BriefingNote { id: String, day: i64 },
    Inbox,
    Close,
    Tomorrow(Vec<String>),
    OpenFile(std::path::PathBuf),
    Complete(String),
    Snooze(String),
    News,
    AgentWait(crate::agents::Wait),
    AgentResult(crate::agents::Target),
    Scroll(isize),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Agent,
    Morning,
    Wrap,
    Download,
    Reminder,
    Battery,
    Bundle,
    News,
}
impl Kind {
    pub(super) fn glyph(self) -> glass::Mark {
        match self {
            Self::Morning => glass::Mark::Sun,
            Self::Wrap => glass::Mark::Moon,
            Self::Download => glass::Mark::Download,
            Self::Reminder => glass::Mark::Note,
            Self::Battery => glass::Mark::Battery,
            Self::Agent | Self::Bundle | Self::News => glass::Mark::Inbox,
        }
    }
}
#[derive(Clone, Debug)]
pub struct Line {
    pub glyph: glass::Mark,
    pub label: String,
    pub value: String,
    pub detail: String,
    pub route: Route,
}
impl Line {
    pub fn new(glyph: glass::Mark, label: impl Into<String>, route: Route) -> Self {
        Self {
            glyph,
            label: label.into(),
            value: String::new(),
            detail: String::new(),
            route,
        }
    }
    fn height(&self) -> f32 {
        if self.detail.is_empty() {
            32.
        } else {
            52.
        }
    }
}
#[derive(Clone, Debug)]
pub struct Button {
    pub label: String,
    pub route: Route,
    pub primary: bool,
}
#[derive(Clone, Debug)]
pub struct Emphasis {
    pub value: String,
    pub title: String,
    pub route: Route,
}
#[derive(Clone, Debug)]
pub struct Card {
    pub kind: Kind,
    pub title: String,
    pub emphasis: Option<Emphasis>,
    pub lines: Vec<Line>,
    pub buttons: Vec<Button>,
}
pub const WIDTH: f32 = 288.;
const TOP: f32 = 44.;
impl Card {
    pub(super) fn shown_carries(&self) -> Vec<(String, i64)> {
        self.lines.iter().filter_map(|line| match &line.route {
            Route::BriefingNote { id, day } => Some((id.clone(), *day)),
            _ => None,
        }).collect()
    }

    fn rows_top(&self) -> f32 {
        (if self.title.is_empty() { 12. } else { TOP }) + self
            .emphasis
            .as_ref()
            .map_or(0., |e| if e.title.is_empty() { 48. } else { 72. })
    }
    pub fn height(&self) -> f32 {
        self.rows_top()
            + self.lines.iter().map(Line::height).sum::<f32>()
            + 16.
            + if self.buttons.is_empty() { 0. } else { 44. }
    }
    fn hit(&self, x: f32, y: f32) -> Option<Route> {
        if let Some(e) = &self.emphasis {
            if contains(rect(16., TOP, WIDTH - 32., 64.), x, y) {
                return Some(e.route.clone());
            }
        }
        let mut top = self.rows_top();
        for row in &self.lines {
            if contains(rect(16., top, WIDTH - 32., row.height()), x, y) {
                return Some(row.route.clone());
            }
            top += row.height();
        }
        self.buttons
            .iter()
            .enumerate()
            .find(|(i, _)| contains(button_rect(self, *i), x, y))
            .map(|(_, b)| b.route.clone())
    }
}
fn button_rect(c: &Card, i: usize) -> D2D_RECT_F {
    let n = c.buttons.len().max(1) as f32;
    let width = (WIDTH - 32. - 8. * (n - 1.)) / n;
    rect(16. + i as f32 * (width + 8.), c.height() - 48., width, 32.)
}
/// Same plate, glyphs, header, typography and pills as the desktop widgets.
#[cfg(test)]
pub unsafe fn paint(
    dc: &ID2D1DeviceContext,
    gfx: &crate::gfx::Gfx,
    card: &Card,
    theme: &crate::theme::Theme,
    under: Option<&backdrop::Under>,
) -> Result<()> {
    glass::plate(dc, theme, WIDTH, card.height(), false, under)?;
    paint_content(dc, gfx, card, theme, under)
}

unsafe fn paint_content(
    dc: &ID2D1DeviceContext,
    gfx: &crate::gfx::Gfx,
    card: &Card,
    theme: &crate::theme::Theme,
    under: Option<&backdrop::Under>,
) -> Result<()> {
    let theme = glass::theme_for(theme, under);
    if !card.title.is_empty() {
        glass::header_mark(dc, gfx, theme, card.kind.glyph(), &card.title, "", WIDTH)?;
    }
    if let Some(e) = &card.emphasis {
        glass::line(
            dc,
            gfx,
            &e.value,
            16.,
            TOP,
            glass::Line {
                tabular: true,
                ..glass::Line::new(28., 700, 36.)
            },
            glass::ink(theme, 1),
            WIDTH - 32.,
        )?;
        glass::line(
            dc,
            gfx,
            &e.title,
            16.,
            TOP + 40.,
            glass::Line::new(14., 600, 20.),
            glass::ink(theme, 1),
            WIDTH - 32.,
        )?;
    }
    let mut y = card.rows_top();
    for row in &card.lines {
        glass::mark(dc, gfx, row.glyph, 16., y + 7., 16., glass::ink(theme, 2))?;
        let numeric = glass::Line {
            tabular: true,
            ..glass::Line::new(13., 600, 30.)
        };
        let vw = glass::width(gfx, &row.value, numeric).min(124.);
        let right = WIDTH - 16.;
        let label_w = right - 40. - if row.value.is_empty() { 0. } else { vw + 8. };
        glass::line(
            dc,
            gfx,
            &row.label,
            40.,
            y,
            glass::Line::new(
                14.,
                if card.kind == Kind::Download {
                    600
                } else {
                    500
                },
                30.,
            ),
            glass::ink(theme, 1),
            label_w,
        )?;
        if !row.value.is_empty() {
            glass::line(
                dc,
                gfx,
                &row.value,
                right - vw,
                y,
                numeric,
                glass::ink(theme, 2),
                vw,
            )?;
        }
        if !row.detail.is_empty() {
            glass::line(
                dc,
                gfx,
                &row.detail,
                40.,
                y + 32.,
                glass::Line::new(
                    if card.kind == Kind::Download {
                        12.
                    } else {
                        13.
                    },
                    400,
                    16.,
                ),
                glass::ink(theme, if card.kind == Kind::Download { 3 } else { 2 }),
                right - 40.,
            )?;
        }
        y += row.height();
    }
    for (i, b) in card.buttons.iter().enumerate() {
        glass::pill(dc, gfx, theme, &b.label, button_rect(card, i), b.primary)?;
    }
    Ok(())
}
/// Keep the opening top-left, including after content grows; only push overflow in.
fn place(work: RECT, dpi: f32, tip: (f32, f32), height: f32, previous: Option<RECT>) -> RECT {
    let w = (WIDTH * dpi).ceil() as i32;
    let h = (height * dpi).ceil() as i32;
    if let Some(p) = previous {
        return super::panels::clamp(work, (w, h), (p.left, p.top));
    }
    let mx = (40. * dpi).round() as i32;
    let px = mx.min(((work.right - work.left - w) / 2).max(0));
    let py = mx.min(((work.bottom - work.top - h) / 2).max(0));
    let x = (tip.0 * dpi - w as f32 / 2.).round() as i32;
    let y = (tip.1 * dpi - h as f32 - 12. * dpi).round() as i32;
    let x = x.clamp(work.left + px, (work.right - px - w).max(work.left + px));
    let y = y.clamp(work.top + py, (work.bottom - py - h).max(work.top + py));
    RECT {
        left: x,
        top: y,
        right: x + w,
        bottom: y + h,
    }
}

pub(super) fn place_slot(slot: SlotAnchor, work: RECT, height: f32) -> RECT {
    let SlotAnchor { rect, dpi } = slot;
    let w = (WIDTH * dpi).ceil() as i32;
    let h = (height * dpi).ceil() as i32;
    let gap = (8. * dpi).round() as i32;
    let center = rect.left + (rect.right - rect.left) / 2;
    let below = rect.bottom + gap;
    let above = rect.top - gap;
    let room_below = work.bottom - below;
    let top = if h <= room_below || room_below >= above - work.top { below } else { above - h };
    super::panels::clamp(work, (w, h), (center - w / 2, top))
}

#[derive(Clone, Copy)]
pub(super) enum Anchor {
    Body { work: RECT, dpi: f32, tip: (f32, f32) },
    Slot { slot: SlotAnchor, work: RECT },
}

impl Anchor {
    pub(super) fn slot(self) -> Option<SlotAnchor> {
        match self { Self::Slot { slot, .. } => Some(slot), Self::Body { .. } => None }
    }

    fn display(self) -> (RECT, f32) {
        match self {
            Self::Body { work, dpi, .. } | Self::Slot { work, slot: SlotAnchor { dpi, .. } } => (work, dpi),
        }
    }

    fn visible_rows(self) -> usize {
        let (work, dpi) = self.display();
        let room = if let Some(slot) = self.slot() {
            (work.bottom - slot.rect.bottom).max(slot.rect.top - work.top) as f32 / dpi - 8.
        } else { (work.bottom - work.top) as f32 / dpi };
        ((room - TOP - 16.) / 32.).max(1.) as usize
    }
}

const ARRIVAL: Duration = Duration::from_millis(240);
fn entry_offset(elapsed: Duration) -> f32 {
    if elapsed >= ARRIVAL { return 0.; }
    let t = elapsed.as_secs_f32() / ARRIVAL.as_secs_f32();
    -8. * (1. - t).powi(2) * (t * std::f32::consts::TAU).cos()
}
const LIFETIME: Duration = Duration::from_secs(20);
const FADE: Duration = Duration::from_millis(160);

struct CardLifetime {
    remaining: Duration,
    last: Instant,
    hovered: bool,
    engaged: bool,
    closing: Option<Instant>,
}
impl CardLifetime {
    fn new(now: Instant) -> Self {
        Self { remaining: LIFETIME, last: now, hovered: false, engaged: false, closing: None }
    }

    fn dismiss(&mut self, now: Instant) {
        self.closing.get_or_insert(now);
    }

    fn opacity(&mut self, now: Instant, hovered: bool, motion: bool) -> f32 {
        if self.closing.is_none() {
            if !self.hovered {
                self.remaining = self.remaining.saturating_sub(now.saturating_duration_since(self.last));
            }
            self.last = now;
            self.hovered = hovered;
            self.engaged |= hovered || self.remaining.is_zero();
            if self.remaining.is_zero() {
                self.dismiss(now);
            }
        }
        match self.closing {
            Some(at) if motion => 1. - (now.saturating_duration_since(at).as_secs_f32() / FADE.as_secs_f32()).min(1.),
            Some(_) => 0.,
            None => 1.,
        }
    }
}

struct Overlay {
    card: Card,
    visible_card: Card,
    hwnd: HWND,
    surface: crate::desklayer::surface::Glass,
    lifetime: CardLifetime,
    dpi: f32,
    stamp: Option<(RECT, bool)>,
    position: Option<RECT>,
    display: Option<(RECT, f32)>,
    slot: Option<SlotAnchor>,
    opened: Option<Instant>,
    opened_tick: Option<u32>,
    read_ids: Vec<u64>,
    scroll: usize,
    wheel_remainder: i32,
    rows: usize,
    hooks: Option<Hooks>,
}
impl Overlay {
    fn record_engagement(&mut self) {
        if self.lifetime.engaged && !self.read_ids.is_empty() {
            READ.with(|read| read.borrow_mut().append(&mut self.read_ids));
            super::refresh();
        }
    }
}
impl Drop for Overlay {
    fn drop(&mut self) {
        self.hooks.take();
        backdrop::unlisten(self.hwnd);
        crate::desklayer::privacy::forget(self.hwnd);
        unsafe { let _ = DestroyWindow(self.hwnd); }
    }
}
thread_local! {
    static VIEW: RefCell<Option<Overlay>> = const { RefCell::new(None) };
    static CLICKS: RefCell<Vec<Route>> = const { RefCell::new(Vec::new()) };
    static READ: RefCell<Vec<u64>> = const { RefCell::new(Vec::new()) };
}

pub(super) fn track_read(ids: Vec<u64>) {
    VIEW.with(|view| {
        if let Some(view) = view.borrow_mut().as_mut() {
            view.read_ids = ids;
            view.record_engagement();
        }
    });
}

pub(super) fn take_read() -> Vec<u64> {
    READ.with(|read| std::mem::take(&mut *read.borrow_mut()))
}
pub fn active() -> bool {
    VIEW.with(|v| v.try_borrow().map_or(true, |v| v.is_some()))
}

pub(super) fn current_slot() -> Option<SlotAnchor> {
    VIEW.with(|v| v.try_borrow().ok()?.as_ref()?.slot)
}

pub(super) struct Interrupted {
    pub card: Card,
    pub read_ids: Vec<u64>,
}

pub(super) fn interrupt_for_chat() -> Option<Interrupted> {
    VIEW.with(|v| {
        let mut v = v.borrow_mut();
        if v.as_ref().is_some_and(|view| view.slot.is_some()) { return None; }
        let mut view = v.take()?;
        (view.opened.is_some() && view.lifetime.closing.is_none()
            && matches!(view.card.kind, Kind::Morning | Kind::Wrap)).then(|| Interrupted {
                card: view.card.clone(),
                read_ids: std::mem::take(&mut view.read_ids),
            })
    })
}

pub(super) fn is_history() -> bool {
    VIEW.with(|v| v.try_borrow().ok().is_some_and(|v| v.as_ref().is_some_and(|v| v.card.kind == Kind::News)))
}
pub(super) fn resolve_waits(alive: impl Fn(&crate::agents::Wait) -> bool) {
    VIEW.with(|v| {
        let mut v = v.borrow_mut();
        let Some(view) = v.as_mut() else { return };
        let count = view.card.lines.len();
        retain_waits(&mut view.card, &alive);
        if view.card.lines.len() == count {
            return;
        }
        if view.card.kind != Kind::News && view.card.lines.is_empty()
            && view.card.emphasis.is_none() && view.card.buttons.is_empty()
        {
            v.take();
        } else {
            view.scroll = view.scroll.min(view.card.lines.len().saturating_sub(page_size(&view.card, view.rows)));
            view.visible_card = viewport(&view.card, view.scroll, view.rows);
            view.stamp = None;
        }
    });
}

pub(super) fn retain_waits(card: &mut Card, alive: impl Fn(&crate::agents::Wait) -> bool) {
    card.lines.retain(|row| !matches!(&row.route, Route::AgentWait(wait) if !alive(wait)));
}
pub fn close() -> bool {
    VIEW.with(|v| v.borrow_mut().take().is_some())
}
fn dismiss() -> bool {
    VIEW.with(|v| {
        let mut v = v.borrow_mut();
        let Some(view) = v.as_mut() else { return false; };
        view.lifetime.dismiss(Instant::now());
        true
    })
}
pub(super) fn tapped_body() -> bool {
    VIEW.with(|cell| {
        let Ok(mut view) = cell.try_borrow_mut() else { return true; };
        let Some(view) = view.as_mut() else { return false; };
        let now = unsafe { windows::Win32::System::SystemInformation::GetTickCount() };
        if view.opened_tick.is_some_and(|opened| dismissal_ready(opened, now)) {
            view.lifetime.dismiss(Instant::now());
        }
        true
    })
}

pub fn interval() -> Option<u32> {
    VIEW.with(|v| v.borrow().as_ref().map(|v| if v.lifetime.closing.is_some()
        || v.slot.is_some() && v.opened.is_some_and(|at| at.elapsed() < ARRIVAL) { 17 } else { 100 }))
}
pub(super) fn show_at(gfx: &Rc<crate::gfx::Gfx>, card: Card, anchor: Anchor) -> bool {
    if active() {
        return false;
    }
    let result = unsafe {
        (|| -> Result<Overlay> {
            let inst = windows::Win32::System::LibraryLoader::GetModuleHandleW(None)?;
            let class = w!("NopBarAssistantCard");
            RegisterClassExW(&WNDCLASSEXW { cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32, lpfnWndProc: Some(proc), hInstance: inst.into(), lpszClassName: class, ..Default::default() });
            let hwnd = CreateWindowExW(WS_EX_NOREDIRECTIONBITMAP | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE, class, None, WS_POPUP, -30000, -30000, 1, 1, None, None, Some(inst.into()), None)?;
            let surface = match crate::desklayer::surface::Glass::new(Some(hwnd), gfx.clone(), (1, 1), 1.) {
                Ok(s) => s,
                Err(e) => { let _ = DestroyWindow(hwnd); return Err(e); }
            };
            let rows = anchor.visible_rows();
            let visible_card = viewport(&card, 0, rows);
            Ok(Overlay {
                card, visible_card, hwnd, surface,
                lifetime: CardLifetime::new(Instant::now()),
                dpi: 1.,
                stamp: None,
                position: None,
                display: None,
                slot: anchor.slot(),
                opened: None,
                opened_tick: None,
                read_ids: Vec::new(),
                scroll: 0,
                wheel_remainder: 0,
                rows,
                hooks: None,
            })
        })()
    };
    match result {
        Ok(view) => {
            backdrop::listen(view.hwnd);
            crate::desklayer::privacy::register(view.hwnd, false);
            crate::desklayer::shade::attach(view.hwnd);
            VIEW.with(|v| *v.borrow_mut() = Some(view));
            true
        }
        Err(e) => {
            crate::log!("assistant card: {e}");
            false
        }
    }
}

fn viewport(card: &Card, scroll: usize, rows: usize) -> Card {
    let mut visible = card.clone();
    if card.kind == Kind::News {
        let rows = page_size(card, rows);
        visible.lines = card.lines.iter().skip(scroll).take(rows).cloned().collect();
        for (has_page, label, direction) in [(scroll > 0, "↑", -1), (scroll + rows < card.lines.len(), "↓", 1)] {
            if has_page {
                visible.buttons.push(Button { label: label.into(), route: Route::Scroll(direction * rows as isize), primary: false });
            }
        }
    }
    visible
}

fn page_size(card: &Card, rows: usize) -> usize {
    if card.lines.len() > rows { rows.saturating_sub(2).max(1) } else { rows }
}

fn scroll(view: &mut Overlay, delta: isize) {
    let last = view.card.lines.len().saturating_sub(page_size(&view.card, view.rows));
    let offset = view.scroll.saturating_add_signed(delta).min(last);
    if view.scroll != offset {
        view.scroll = offset;
        view.visible_card = viewport(&view.card, view.scroll, view.rows);
        view.stamp = None;
    }
}

fn wheel_steps(remainder: &mut i32, delta: i16) -> isize {
    let total = *remainder + i32::from(delta);
    *remainder = total % 120;
    (total / 120) as isize
}

pub(super) fn anchor_at(anchor: Anchor, light: bool, motion: bool) -> bool {
    VIEW.with(|v| {
        let mut v = v.borrow_mut();
        let Some(view) = v.as_mut() else { return false; };
        let now = Instant::now();
        let first = view.opened.is_none();
        if first && view.lifetime.closing.is_none() {
            view.lifetime = CardLifetime::new(now);
        }
        let hovered = unsafe {
            let mut cursor = POINT::default();
            GetCursorPos(&mut cursor).is_ok() && WindowFromPoint(cursor) == view.hwnd
        };
        let opacity = view.lifetime.opacity(now, hovered, motion);
        view.record_engagement();
        if opacity <= 0. {
            v.take();
            return false;
        }
        let (work, dpi) = anchor.display();
        if view.display != Some((work, dpi)) {
            view.position = None;
            view.display = Some((work, dpi));
        }
        if anchor.visible_rows() != view.rows {
            view.rows = anchor.visible_rows();
            view.scroll = view.scroll.min(view.card.lines.len().saturating_sub(page_size(&view.card, view.rows)));
            view.visible_card = viewport(&view.card, view.scroll, view.rows);
            view.stamp = None;
        }
        let height = view.visible_card.height();
        let r = match anchor {
            Anchor::Body { tip, .. } => place(work, dpi, tip, height, view.position),
            Anchor::Slot { slot, .. } => place_slot(slot, work, height),
        };
        let offset = if motion && anchor.slot().is_some() {
            (entry_offset(now.saturating_duration_since(view.opened.unwrap_or(now))) * dpi).round() as i32
        } else { 0 };
        let moved = super::panels::clamp(work, (r.right - r.left, r.bottom - r.top), (r.left, r.top + offset));
        let (w, h) = ((r.right - r.left) as u32, (r.bottom - r.top) as u32);
        view.slot = anchor.slot();
        let result = unsafe {
            (|| -> Result<()> {
                let dirty = view.stamp != Some((r, light));
                if dirty {
                    view.surface.resize((w as i32, h as i32), dpi)?;
                    let mut theme = crate::dock::companion_theme().unwrap_or_else(crate::theme::Theme::graphite);
                    theme.is_light = light;
                    let under = backdrop::under_for(view.hwnd, r);
                    view.surface.draw(|dc, gfx| {
                        glass::body(dc, &theme, WIDTH, height, under.as_ref())?;
                        paint_content(dc, gfx, &view.visible_card, &theme, under.as_ref())
                    })?;
                    let glass = glass::glass_for(&theme, under.as_ref());
                    crate::desklayer::shade::set(view.hwnd, glass.map_or(light, |g| g.light), glass.is_some(), false);
                    view.stamp = Some((r, light));
                }
                if first || view.position != Some(moved) {
                    SetWindowPos(view.hwnd, Some(HWND_TOPMOST), moved.left, moved.top, w as i32, h as i32, SWP_NOACTIVATE | SWP_SHOWWINDOW)?;
                }
                if dirty || view.position != Some(moved) {
                    crate::desklayer::shade::sync(view.hwnd);
                }
                if view.lifetime.closing.is_some() {
                    view.surface.root.SetOpacity(opacity)?;
                    crate::desklayer::shade::opacity(view.hwnd, (opacity * 255.).round() as u8);
                }
                Ok(())
            })()
        };
        if let Err(e) = result {
            crate::log!("assistant card: {e}");
            v.take();
            return false;
        }
        view.dpi = dpi;
        view.position = Some(moved);
        if first {
            view.opened = Some(if motion { now } else { now - ARRIVAL });
            view.opened_tick = Some(unsafe { windows::Win32::System::SystemInformation::GetTickCount() });
            if !cfg!(test) { view.hooks = Hooks::start(view.hwnd); }
        }
        first
    })
}
pub fn dispatch() {
    let routes = CLICKS.with(|q| std::mem::take(&mut *q.borrow_mut()));
    for route in routes {
        if !matches!(route, Route::Scroll(_) | Route::AgentWait(_) | Route::AgentResult(_)) { dismiss(); }
        match route {
            Route::Agenda(at) => crate::topbar::companion_agenda(at),
            Route::Notes(id) | Route::BriefingNote { id, .. } => {
                crate::notes::runtime::action(crate::notes::runtime::Action::Reveal(id))
            }
            Route::Inbox => {
                crate::notes::runtime::action(crate::notes::runtime::Action::Reveal(String::new()))
            }
            Route::Tomorrow(ids) => {
                crate::notes::runtime::action(crate::notes::runtime::Action::Tomorrow(ids))
            }
            Route::OpenFile(path) => crate::launch::open_soon(crate::launch::Target::new(
                &path.to_string_lossy(),
                &path.file_name().unwrap_or_default().to_string_lossy(),
                crate::launch::Source::Stack,
            )),
            Route::Complete(id) => crate::notes::runtime::action(crate::notes::runtime::Action::Complete(id)),
            Route::Snooze(id) => crate::notes::runtime::action(crate::notes::runtime::Action::Snooze(id)),
            Route::News => super::news::open(),
            Route::AgentWait(wait) => {
                crate::agents::focus(&wait.target);
                dismiss();
            }
            Route::AgentResult(target) => {
                crate::agents::focus(&target);
                dismiss();
            }
            Route::Scroll(delta) => VIEW.with(|v| {
                if let Some(view) = v.borrow_mut().as_mut() { scroll(view, delta); }
            }),
            Route::Close => {}
        }
    }
}
unsafe extern "system" fn proc(h: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        backdrop::WM_BACKDROP => {
            VIEW.with(|v| {
                if let Ok(mut v) = v.try_borrow_mut() {
                    if let Some(v) = v.as_mut() {
                    v.stamp = None;
                    }
                }
            });
            super::refresh();
            LRESULT(0)
        }
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_POINTERACTIVATE => LRESULT(PA_NOACTIVATE as isize),
        DISMISS => {
            VIEW.with(|v| {
                if let Ok(mut v) = v.try_borrow_mut() {
                    if let Some(view) = v.as_mut().filter(|view| view.hwnd == h && view.hooks.as_ref().is_some_and(|hook| hook.token == lp.0 as usize)) {
                        if view.opened_tick.is_some_and(|opened| dismissal_ready(opened, wp.0 as u32)) {
                            view.lifetime.dismiss(Instant::now());
                            super::refresh();
                        }
                    }
                }
            });
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            VIEW.with(|v| {
                if let Ok(mut v) = v.try_borrow_mut() {
                    if let Some(view) = v.as_mut().filter(|view| view.hwnd == h && view.lifetime.closing.is_none()) {
                        view.lifetime.engaged = true;
                        view.record_engagement();
                    }
                }
            });
            LRESULT(0)
        }
        WM_MOUSEWHEEL => {
            VIEW.with(|v| {
                let Ok(mut v) = v.try_borrow_mut() else { return; };
                if let Some(v) = v.as_mut().filter(|v| v.hwnd == h && v.card.kind == Kind::News) {
                    let steps = wheel_steps(&mut v.wheel_remainder, (wp.0 >> 16) as i16);
                    scroll(v, -steps * 3);
                }
            });
            super::refresh();
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            VIEW.with(|v| {
                let Ok(mut v) = v.try_borrow_mut() else { return; };
                if let Some(v) = v.as_mut().filter(|v| v.hwnd == h && v.lifetime.closing.is_none()) {
                    let x = (lp.0 as i16) as f32 / v.dpi;
                    let y = ((lp.0 >> 16) as i16) as f32 / v.dpi;
                    if let Some(route) = v.visible_card.hit(x, y) {
                        v.lifetime.engaged = true;
                        v.record_engagement();
                        let button = v.visible_card.buttons.iter().enumerate()
                            .any(|(i, _)| contains(button_rect(&v.visible_card, i), x, y));
                        if !matches!(route, Route::Close) || button {
                            CLICKS.with(|q| q.borrow_mut().push(route));
                        }
                    }
                }
            });
            super::refresh();
            LRESULT(0)
        }
        _ => DefWindowProcW(h, msg, wp, lp),
    }
}

fn dismissal_ready(opened: u32, input: u32) -> bool {
    (1500..=i32::MAX as u32).contains(&input.wrapping_sub(opened))
}
#[cfg(test)]
#[path = "card_snapshots.rs"]
pub(super) mod tests;
