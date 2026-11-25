//! Shared monochrome local cards, rendered identically on screen and offscreen.
use super::{
    chat::paint::{contains, rect},
};
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
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Morning,
    Wrap,
    Download,
}
impl Kind {
    fn glyph(self) -> glass::Mark {
        match self {
            Self::Morning => glass::Mark::Sun,
            Self::Wrap => glass::Mark::Moon,
            Self::Download => glass::Mark::Download,
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
        TOP + self
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
    glass::header_mark(dc, gfx, theme, card.kind.glyph(), &card.title, "", WIDTH)?;
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
const LIFETIME: Duration = Duration::from_secs(20);
const FADE: Duration = Duration::from_millis(160);

struct CardLifetime {
    remaining: Duration,
    last: Instant,
    hovered: bool,
    closing: Option<Instant>,
}
impl CardLifetime {
    fn new(now: Instant) -> Self {
        Self { remaining: LIFETIME, last: now, hovered: false, closing: None }
    }

    fn dismiss(&mut self, now: Instant) {
        self.closing.get_or_insert(now);
    }

    fn opacity(&mut self, now: Instant, hovered: bool, motion: bool) -> f32 {
        if !self.hovered {
            self.remaining = self.remaining.saturating_sub(now.saturating_duration_since(self.last));
        }
        self.last = now;
        self.hovered = hovered;
        if self.remaining.is_zero() {
            self.dismiss(now);
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
    hwnd: HWND,
    surface: crate::desklayer::surface::Glass,
    lifetime: CardLifetime,
    dpi: f32,
    stamp: Option<(RECT, bool)>,
    position: Option<RECT>,
    display: Option<(RECT, f32)>,
}
impl Drop for Overlay {
    fn drop(&mut self) {
        backdrop::unlisten(self.hwnd);
        crate::desklayer::privacy::forget(self.hwnd);
        unsafe { let _ = DestroyWindow(self.hwnd); }
    }
}
thread_local! {static VIEW:RefCell<Option<Overlay>>=const{RefCell::new(None)};static CLICKS:RefCell<Vec<Route>>=const{RefCell::new(Vec::new())};}
pub fn active() -> bool {
    VIEW.with(|v| v.borrow().is_some())
}
pub fn close() -> bool {
    VIEW.with(|v| v.borrow_mut().take().is_some())
}
pub fn dismiss() -> bool {
    VIEW.with(|v| {
        let mut v = v.borrow_mut();
        let Some(view) = v.as_mut() else { return false; };
        view.lifetime.dismiss(Instant::now());
        true
    })
}
pub fn interval() -> Option<u32> {
    VIEW.with(|v| v.borrow().as_ref().map(|v| if v.lifetime.closing.is_some() { 17 } else { 100 }))
}
pub fn show(gfx: &Rc<crate::gfx::Gfx>, card: Card) -> bool {
    if super::notifications_quiet() || active() {
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
            Ok(Overlay {
                card, hwnd, surface,
                lifetime: CardLifetime::new(Instant::now()),
                dpi: 1.,
                stamp: None,
                position: None,
                display: None,
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
pub fn anchor(work: RECT, dpi: f32, tip: (f32, f32), light: bool, motion: bool) -> Option<Kind> {
    if super::notifications_quiet() { close(); return None; }
    VIEW.with(|v| {
        let mut v = v.borrow_mut();
        let view = v.as_mut()?;
        let now = Instant::now();
        let first = !unsafe { IsWindowVisible(view.hwnd).as_bool() };
        if first && view.lifetime.closing.is_none() {
            view.lifetime = CardLifetime::new(now);
        }
        let hovered = unsafe {
            let mut cursor = POINT::default();
            GetCursorPos(&mut cursor).is_ok() && WindowFromPoint(cursor) == view.hwnd
        };
        let opacity = view.lifetime.opacity(now, hovered, motion);
        if opacity <= 0. {
            v.take();
            return None;
        }
        if view.display != Some((work, dpi)) {
            view.position = None;
            view.display = Some((work, dpi));
        }
        let r = place(work, dpi, tip, view.card.height(), view.position);
        view.position = Some(r);
        let (w, h) = ((r.right - r.left) as u32, (r.bottom - r.top) as u32);
        let (x, y) = (r.left, r.top);
        view.dpi = dpi;
        let result = unsafe {
            (|| -> Result<()> {
                let dirty = view.stamp != Some((r, light));
                if dirty {
                    view.surface.resize((w as i32, h as i32), dpi)?;
                    let mut theme = crate::theme::Theme::graphite();
                    theme.is_light = light;
                    let under = backdrop::under_for(view.hwnd, r);
                    view.surface.draw(|dc, gfx| {
                        glass::body(dc, &theme, WIDTH, view.card.height(), under.as_ref())?;
                        paint_content(dc, gfx, &view.card, &theme, under.as_ref())
                    })?;
                    let glass = glass::glass_for(&theme, under.as_ref());
                    crate::desklayer::shade::set(view.hwnd, glass.map_or(light, |g| g.light), glass.is_some(), false);
                    view.stamp = Some((r, light));
                }
                if dirty || !IsWindowVisible(view.hwnd).as_bool() {
                    SetWindowPos(view.hwnd, Some(HWND_TOPMOST), x, y, w as i32, h as i32, SWP_NOACTIVATE | SWP_SHOWWINDOW)?;
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
            return None;
        }
        if first && view.card.kind == Kind::Morning {
            let shown = view.card.shown_carries();
            if !shown.is_empty() {
                crate::notes::runtime::action(crate::notes::runtime::Action::BriefingShown(shown));
            }
        }
        first.then_some(view.card.kind)
    })
}
pub fn dispatch() {
    let routes = CLICKS.with(|q| std::mem::take(&mut *q.borrow_mut()));
    for route in routes {
        dismiss();
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
            Route::Close => {}
        }
    }
}
unsafe extern "system" fn proc(h: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        backdrop::WM_BACKDROP => {
            VIEW.with(|v| {
                if let Some(v) = v.borrow_mut().as_mut() {
                    v.stamp = None;
                }
            });
            super::refresh();
            LRESULT(0)
        }
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_LBUTTONUP => {
            VIEW.with(|v| {
                if let Some(v) = v.borrow().as_ref().filter(|v| v.lifetime.closing.is_none()) {
                    let x = (lp.0 as i16) as f32 / v.dpi;
                    let y = ((lp.0 >> 16) as i16) as f32 / v.dpi;
                    if let Some(route) = v.card.hit(x, y) {
                        CLICKS.with(|q| q.borrow_mut().push(route));
                    }
                }
            });
            super::refresh();
            LRESULT(0)
        }
        _ => DefWindowProcW(h, msg, wp, lp),
    }
}
#[cfg(test)]
#[path = "card_snapshots.rs"]
pub(super) mod tests;

pub fn interrupt_for_chat() -> Option<Kind> {
    VIEW.with(|v| v.borrow_mut().take().filter(|v| v.lifetime.closing.is_none()).map(|v| v.card.kind))
}

/// An interrupted briefing can be offered again after chat, even later that day.
pub fn restore_allowance(kind: Option<Kind>, cfg: &mut super::model::Config) -> bool {
    match kind {
        Some(Kind::Morning) => {
            cfg.last_greet = None;
            true
        }
        Some(Kind::Wrap) => {
            cfg.routine.wrapped = None;
            cfg.routine.dirty = true;
            false
        }
        _ => false,
    }
}
