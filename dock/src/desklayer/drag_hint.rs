//! A retained Composition scene on the desktop layer, shared by both drag paths.
use super::{
    backdrop,
    drag_clock::Clock,
    drag_input::{Sample, Samples},
    drag_metrics::{self, Metrics},
    drag_visual::{self, CardVisual, Face, Marks},
    edit, grid, widget, win,
    grab::{Placement, Step},
};
use crate::{gfx::Gfx, monitors::Monitor, motion};
use std::{
    cell::RefCell,
    rc::Rc,
    sync::{mpsc, OnceLock},
};
use windows::{
    core::{w, IInspectable, Interface, Result},
    Foundation::TypedEventHandler,
    Win32::{
        Foundation::*,
        Graphics::Dwm::DwmFlush,
        System::{LibraryLoader::GetModuleHandleW, WinRT::Composition::ICompositorDesktopInterop},
        UI::{
            Input::KeyboardAndMouse::{GetAsyncKeyState, GetCapture, ReleaseCapture, VK_ESCAPE},
            WindowsAndMessaging::*,
        },
    },
    UI::Composition::{
        CompositionBatchCompletedEventArgs, CompositionBatchTypes, Compositor, ContainerVisual,
        Desktop::DesktopWindowTarget, Visual,
    },
};
use windows_future::AsyncActionCompletedHandler;

/// Requests a synchronous redraw on the owner's message loop. wParam is
/// this drag's reply window, with zero used for an immediate restore.
pub const LANDED: u32 = WM_APP + 0x2c5;
const PRESENTED: u32 = WM_APP + 0x2c6;
const READY: u32 = WM_APP + 0x2c7;
pub const LIFTED: u32 = WM_APP + 0x2c8;
const HIDDEN: u32 = WM_APP + 0x2c9;
const SETTLED: u32 = WM_APP + 0x2ca;
const TICK: u32 = WM_APP + 0x2cc;
const CLOCK_STOPPED: u32 = WM_APP + 0x2cd;
const RESAMPLE_POINTER: bool = true;
const CLOCK_STALE_MS: f64 = 50.;
const ESCAPE_TIMER: usize = 1;
const PRESENT_TIMEOUT_MS: u32 = 1_000;

struct HintWindow {
    hwnd: HWND,
    target: DesktopWindowTarget,
    monitor: Monitor,
    host: Option<HWND>,
}

impl Drop for HintWindow {
    fn drop(&mut self) {
        let _ = self.target.Close();
        super::privacy::forget(self.hwnd);
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
    }
}

struct Hint {
    window: Rc<HintWindow>,
    root: ContainerVisual,
    marks: Marks,
    visual: CardVisual,
}

impl Drop for Hint {
    fn drop(&mut self) {
        win::placing(|| unsafe {
            let _ = ShowWindow(self.window.hwnd, SW_HIDE);
        });
        let _ = self.window.target.SetRoot(None::<&Visual>);
        if let Ok(children) = self.root.Children() {
            let _ = children.RemoveAll();
        }
        let _ = self.root.Close();
    }
}

struct Reply(HWND);

impl Drop for Reply {
    fn drop(&mut self) {
        unsafe {
            let _ = DestroyWindow(self.0);
        }
    }
}

struct Scene {
    hwnd: HWND,
    reply: Reply,
    gfx: Rc<Gfx>,
    hints: Vec<Hint>,
    face: Face,
    origin: RECT,
    at: RECT,
    offset: (f32, f32),
    target: Option<RECT>,
    primary_hint: Option<isize>,
    editor: bool,
    capture: HWND,
    escape_down: bool,
    following: bool,
    pending: Option<(Step, f64)>,
    metrics: Option<Metrics>,
    clock: Option<Clock>,
    last_tick: f64,
    samples: Samples,
    handoff: Option<Handoff>,
    timer: Option<usize>,
    _grid: super::grid_state::Frozen,
}

#[derive(Clone, Copy)]
struct Handoff {
    position: RECT,
    velocity: (f32, f32),
    requested: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Watch {
    MissingCard,
    Cancel,
    Continue,
}

fn watch(alive: bool, captured: bool, escape: bool) -> Watch {
    if !alive {
        Watch::MissingCard
    } else if !captured || escape {
        Watch::Cancel
    } else {
        Watch::Continue
    }
}

fn clock_stale(last_tick: f64, now: f64) -> bool {
    now - last_tick > CLOCK_STALE_MS
}

thread_local! {
    static SCENE: RefCell<Option<Scene>> = const { RefCell::new(None) };
    static HINT_WINDOWS: RefCell<Vec<Rc<HintWindow>>> = const { RefCell::new(Vec::new()) };
}

fn dispatch_sample(card: HWND, editor: bool, sample: Sample) {
    if editor {
        edit::drag_tick(card, sample);
    } else {
        super::grab::tick(card, sample);
    }
}

unsafe extern "system" fn proc(h: HWND, m: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match m {
        WM_NCHITTEST => LRESULT(HTTRANSPARENT as isize),
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_DISPLAYCHANGE => {
            displays_changed();
            LRESULT(0)
        }
        WM_WINDOWPOSCHANGING => {
            win::pin(
                super::layer::Layer::Desktop,
                h,
                &mut *(lp.0 as *mut WINDOWPOS),
            );
            DefWindowProcW(h, m, wp, lp)
        }
        READY => {
            if let Some(editor) = SCENE.with(|s| {
                s.borrow()
                    .as_ref()
                    .filter(|s| {
                        s.reply() == h
                            && s.hwnd.0 as usize == wp.0
                            && s.handoff.is_none_or(|x| !x.requested)
                    })
                    .map(|s| s.editor)
            }) {
                let card = HWND(wp.0 as *mut _);
                if editor {
                    edit::send(card, edit::Action::Hide);
                }
                if PostMessageW(Some(card), LIFTED, WPARAM(h.0 as usize), LPARAM(0)).is_err() {
                    clear();
                }
            }
            LRESULT(0)
        }
        HIDDEN => {
            let fallback = SCENE.with(|slot| {
                let mut slot = slot.borrow_mut();
                let s = slot.as_mut().filter(|s| {
                    s.awaiting_hidden(h, wp.0) && s.pending.is_none() && s.handoff.is_none()
                })?;
                s.sample_pointer(drag_metrics::now()).map(|sample| (s.hwnd, s.editor, sample))
            });
            if let Some((card, editor, sample)) = fallback {
                dispatch_sample(card, editor, sample);
            }
            let queued = SCENE.with(|slot| {
                let mut slot = slot.borrow_mut();
                let s = slot.as_mut().filter(|s| s.awaiting_hidden(h, wp.0))?;
                s.following = true;
                let (hand, offset) = s.pending.map_or((s.at, s.offset), |(step, _)| (step.at, step.offset));
                if let Err(e) = s.face.bring_up(&s.gfx.compositor, s.origin, hand, offset) {
                    crate::log!("widgets: drag lift ({e})");
                }
                if let Some((step, sample)) = s.pending.take() {
                    if let Err(e) = s.move_to(step, sample) {
                        crate::log!("widgets: drag visual update ({e})");
                    }
                }
                s.handoff.is_some().then(|| s.start_landing())
            });
            if let Some(wait) = queued {
                cancel_timer();
                arm(wait, request_present);
            }
            LRESULT(0)
        }
        SETTLED => {
            if SCENE.with(|s| {
                s.borrow()
                    .as_ref()
                    .is_some_and(|s| s.reply() == h && s.hwnd.0 as usize == wp.0)
            }) {
                request_present();
            }
            LRESULT(0)
        }
        TICK => {
            let started = drag_metrics::now();
            let next = SCENE.with(|slot| {
                let mut slot = slot.borrow_mut();
                let s = slot.as_mut().filter(|s| s.reply() == h && s.handoff.is_none())?;
                s.clock.as_ref()?;
                s.last_tick = started;
                if let Some(metrics) = &mut s.metrics {
                    metrics.probe(started);
                }
                Some((s.hwnd, s.editor, s.sample_pointer(started)))
            });
            if let Some((card, editor, sample)) = next {
                if let Some(sample) = sample {
                    dispatch_sample(card, editor, sample);
                }
                SCENE.with(|slot| {
                    let mut slot = slot.borrow_mut();
                    if let Some(s) = slot.as_mut().filter(|s| s.reply() == h) {
                        if let Some(metrics) = &mut s.metrics {
                            metrics.tick(drag_metrics::now() - started);
                        }
                        // Keep the tick pending while it runs so the next
                        // posted message cannot continually outrun input.
                        if let Some(clock) = &s.clock {
                            clock.received();
                        }
                    }
                });
            }
            LRESULT(0)
        }
        CLOCK_STOPPED => {
            SCENE.with(|slot| {
                if let Some(s) = slot.borrow_mut().as_mut().filter(|s| s.reply() == h) {
                    s.clock = None;
                }
            });
            LRESULT(0)
        }
        WM_TIMER if wp.0 == ESCAPE_TIMER => {
            let check = SCENE.with(|slot| {
                let mut slot = slot.borrow_mut();
                let s = slot
                    .as_mut()
                    .filter(|s| s.reply() == h && s.handoff.is_none())?;
                s.reap_clock();
                let down = GetAsyncKeyState(VK_ESCAPE.0 as i32) < 0;
                let pressed = down && !s.escape_down;
                s.escape_down = down;
                let next = watch(
                    IsWindow(Some(s.hwnd)).as_bool(),
                    !s.capture.is_invalid() && s.capture == GetCapture(),
                    pressed && edit::is_home(GetForegroundWindow()),
                );
                Some((next, s.hwnd, s.editor, s.origin))
            });
            match check {
                Some((Watch::MissingCard, card, editor, _)) => {
                    if editor {
                        edit::escape_drag();
                    } else {
                        super::grab::discard(card);
                    }
                    clear();
                }
                Some((Watch::Cancel, card, editor, origin)) => {
                    if editor {
                        edit::escape_drag();
                    } else {
                        if !super::grab::cancel(card) {
                            settle(card, origin, drag_metrics::now());
                        }
                        if GetCapture() == card {
                            let _ = ReleaseCapture();
                        }
                    }
                }
                _ => {}
            }
            LRESULT(0)
        }
        PRESENTED => {
            let card = HWND(wp.0 as *mut _);
            let matches = SCENE.with(|s| {
                s.borrow().as_ref().is_some_and(|s| {
                    s.hwnd == card && s.reply() == h && s.handoff.is_some_and(|x| x.requested)
                })
            });
            if matches {
                // The owner submitted the final scene with its new geometry.
                // Keep our opaque face over it through DWM's presentation.
                finish_present();
            }
            LRESULT(0)
        }
        _ => DefWindowProcW(h, m, wp, lp),
    }
}

unsafe fn register() -> Result<(HMODULE, windows::core::PCWSTR)> {
    let inst = GetModuleHandleW(None)?;
    let class = w!("NopBar.WidgetDragHint");
    RegisterClassExW(&WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: Some(proc),
        hInstance: inst.into(),
        lpszClassName: class,
        ..Default::default()
    });
    Ok((inst, class))
}

unsafe fn reply_window() -> Result<Reply> {
    // A fresh message-only window keeps delayed handoff replies out of the
    // next drag, even when it reuses the same card and cached display windows.
    let (inst, class) = register()?;
    CreateWindowExW(
        WINDOW_EX_STYLE(0), class, None, WINDOW_STYLE(0),
        0, 0, 0, 0, Some(HWND_MESSAGE), None, Some(inst.into()), None,
    ).map(Reply)
}

unsafe fn make_window(gfx: &Gfx, m: Monitor, host: Option<HWND>, metrics: Option<&mut Metrics>) -> Result<HintWindow> {
    let (inst, class) = register()?;
    let started = drag_metrics::now();
    let hwnd = CreateWindowExW(
        super::shade::EX_STYLE,
        class,
        None,
        WS_POPUP,
        m.work.left,
        m.work.top,
        m.work.right - m.work.left,
        m.work.bottom - m.work.top,
        None,
        None,
        Some(inst.into()),
        None,
    )?;
    let result = (|| -> Result<HintWindow> {
        SetLayeredWindowAttributes(hwnd, COLORREF(0), 255, LWA_ALPHA)?;
        let interop: ICompositorDesktopInterop = gfx.compositor.cast()?;
        let target = interop.CreateDesktopWindowTarget(hwnd, false)?;
        Ok(HintWindow { hwnd, target, monitor: m, host })
    })();
    if result.is_err() {
        let _ = DestroyWindow(hwnd);
    } else {
        super::privacy::register(hwnd, false);
    }
    if let Some(metrics) = metrics {
        metrics.hint_created(drag_metrics::now() - started);
    }
    result
}

impl Scene {
    fn awaiting_hidden(&self, reply: HWND, card: usize) -> bool {
        self.reply() == reply && self.hwnd.0 as usize == card
            && !self.following && self.handoff.is_none_or(|x| !x.requested)
    }

    fn sample_pointer(&mut self, at: f64) -> Option<Sample> {
        let mut point = POINT::default();
        if unsafe { GetCursorPos(&mut point) }.is_ok() {
            self.samples.push(at, point);
        }
        if RESAMPLE_POINTER { self.samples.resample(at) } else { self.samples.latest() }
    }

    fn reap_clock(&mut self) {
        if self.clock.as_ref().is_some_and(Clock::finished) {
            self.clock = None;
        }
    }

    fn attach_displays(&mut self, placement: &Placement, host: Option<HWND>) {
        HINT_WINDOWS.with(|pool| {
            let mut pool = pool.borrow_mut();
            pool.retain(|h| h.host == host && placement.mons.contains(&h.monitor) && unsafe { IsWindow(Some(h.hwnd)) }.as_bool());
        });
        for (index, &m) in placement.mons.iter().enumerate() {
            let hint = (|| -> Result<Hint> {
                let window = HINT_WINDOWS.with(|pool| {
                    let mut pool = pool.borrow_mut();
                    if let Some(h) = pool.iter().find(|h| h.monitor == m) {
                        return Ok(h.clone());
                    }
                    let window = Rc::new(unsafe { make_window(&self.gfx, m, host, self.metrics.as_mut())? });
                    pool.push(window.clone());
                    Ok::<_, windows::core::Error>(window)
                })?;
                let root = self.gfx.compositor.CreateContainerVisual()?;
                let marks = self.face.attach_marks(&self.gfx, &root, m, &placement.slots(index))?;
                let visual = self.face.attach_card(&self.gfx, &root, Some(window.hwnd), m)?;
                window.target.SetRoot(&root)?;
                // The shell or foreground windows may have changed z-order
                // since the cached hint was last visible.
                win::to_desktop(window.hwnd, host);
                win::placing(|| unsafe { let _ = ShowWindow(window.hwnd, SW_SHOWNOACTIVATE); });
                Ok(Hint { window, root, marks, visual })
            })();
            match hint {
                Ok(h) => self.hints.push(h),
                Err(e) => crate::log!(
                    "widgets: drag hint unavailable for monitor {} ({e})",
                    m.handle
                ),
            }
        }
    }

    fn reply(&self) -> HWND {
        self.reply.0
    }

    fn move_to(&mut self, step: Step, sample: f64) -> Result<()> {
        let Step { at, target, monitor: main, offset } = step;
        if self.at != at || self.offset != offset {
            let resize = self.at.right - self.at.left != at.right - at.left
                || self.at.bottom - self.at.top != at.bottom - at.top;
            self.face.move_to(&self.gfx.compositor, at, offset, resize)?;
            self.at = at;
            self.offset = offset;
        }
        let c = &self.gfx.compositor;
        if let Some(metrics) = &mut self.metrics {
            metrics.monitor(main);
            metrics.hand((at.left as f32 + offset.0, at.top as f32 + offset.1), sample);
        }
        if main != self.primary_hint {
            for h in &self.hints {
                let to = if Some(h.window.monitor.handle) == main {
                    1.
                } else {
                    0.
                };
                motion::fade_to(c, &h.marks.dots, to, drag_visual::DOT_MS, motion::EASE_OUT)?;
            }
            self.primary_hint = main;
        }
        if self.target != target {
            if let Some(r) = target {
                self.face
                    .select(&self.gfx.compositor, r, self.target.is_some())?;
            }
            if self.target.is_some() != target.is_some() {
                let to = if target.is_some() { 1. } else { 0. };
                for h in &self.hints {
                    motion::fade_to(
                        &self.gfx.compositor,
                        &h.marks.target,
                        to,
                        drag_visual::DOT_MS,
                        motion::EASE_OUT,
                    )?;
                }
            }
            self.target = target;
        }
        Ok(())
    }

    fn start_landing(&self) -> u32 {
        let Some(handoff) = self.handoff else { return PRESENT_TIMEOUT_MS };
        if !self.following {
            return PRESENT_TIMEOUT_MS;
        }
        match self.animate_landing(handoff) {
            Ok(()) => drag_visual::drop_ms() + 120,
            Err(e) => {
                crate::log!("widgets: drag settle ({e})");
                drag_visual::drop_ms()
            }
        }
    }

    fn animate_landing(&self, handoff: Handoff) -> Result<()> {
        let c = &self.gfx.compositor;
        let scale = self.hints.iter()
            .find(|h| Some(h.window.monitor.handle) == self.primary_hint)
            .map(|h| h.window.monitor.scale());
        let batch = c.CreateScopedBatch(CompositionBatchTypes::Animation)?;
        let start = (|| -> Result<()> {
            self.face.land(c, self.at, self.offset, handoff.position, handoff.velocity, scale)?;
            for h in &self.hints {
                motion::fade_to(c, &h.marks.dots, 0., drag_visual::DOT_MS, motion::EASE_IN)?;
                motion::fade_to(c, &h.marks.target, 0., drag_visual::DOT_MS, motion::EASE_IN)?;
            }
            Ok(())
        })();
        let (card, reply) = (self.hwnd.0 as usize, self.reply().0 as usize);
        let registered = batch.Completed(&TypedEventHandler::<
            IInspectable,
            CompositionBatchCompletedEventArgs,
        >::new(move |_, _| {
            unsafe {
                let _ = PostMessageW(
                    Some(HWND(reply as *mut _)),
                    SETTLED,
                    WPARAM(card),
                    LPARAM(0),
                );
            }
            Ok(())
        }));
        batch.End()?;
        start?;
        registered?;
        Ok(())
    }
}

/// The caller hides the original only after the face at its current place
/// has been submitted. If construction fails, leave the original alone.
pub(super) fn begin(hwnd: HWND, origin: RECT, size: widget::Size, editor: bool, started: f64, placement: &Placement) -> bool {
    SCENE.with(|slot| {
        let mut slot = slot.borrow_mut();
        if slot.is_some() {
            return false;
        }
        let Some(preview) = edit::preview_of(hwnd) else {
            return false;
        };
        let Some(gfx) = super::surface::gfx() else {
            return false;
        };
        let theme = edit::look();
        let dpi = super::surface::scale(hwnd);
        let made = (|| -> Result<Scene> {
            let metrics = Metrics::new(started, origin, &placement.mons);
            let under = backdrop::for_drag(hwnd, origin);
            let face = Face::new(&gfx, &theme, &preview, size, origin, dpi, under.as_ref())?;
            let mut scene = Scene {
                hwnd,
                reply: unsafe { reply_window()? },
                gfx,
                hints: Vec::new(),
                face,
                origin,
                at: origin,
                offset: (0., 0.),
                target: None,
                primary_hint: None,
                editor,
                following: false,
                pending: None,
                metrics: Some(metrics),
                clock: None,
                last_tick: started,
                samples: Samples::default(),
                handoff: None,
                timer: None,
                capture: unsafe { GetCapture() },
                escape_down: unsafe { GetAsyncKeyState(VK_ESCAPE.0 as i32) < 0 },
                _grid: super::grid_state::Frozen::new(),
            };
            let host = win::shell_host();
            scene.attach_displays(placement, host);
            let main = grid::monitor_of(origin, &placement.mons).map(|i| placement.mons[i]);
            if !scene.hints.iter().any(|h| Some(h.window.monitor) == main) {
                return Err(windows::core::Error::from_hresult(E_FAIL));
            }
            // Keep the original below the retained hints during the handoff.
            win::to_desktop(hwnd, host);
            Ok(scene)
        })();
        match made {
            Ok(mut scene) => {
                let monitor = grid::monitor_of(origin, &placement.mons).map(|i| placement.mons[i].handle);
                scene.last_tick = drag_metrics::now();
                scene.clock = scene.metrics.as_ref().and_then(|metrics| {
                    Clock::start(scene.reply(), TICK, CLOCK_STOPPED, monitor, metrics.displays())
                });
                after_commit(&scene.gfx.compositor, scene.reply(), READY, hwnd);
                unsafe {
                    SetTimer(Some(scene.reply()), ESCAPE_TIMER, 30, None);
                }
                *slot = Some(scene);
                true
            }
            Err(e) => {
                crate::log!("widgets: drag visual unavailable ({e})");
                false
            }
        }
    })
}

pub(super) fn update(hwnd: HWND, step: Step, sample: f64) {
    SCENE.with(|s| {
        if let Some(s) = s
            .borrow_mut()
            .as_mut()
            .filter(|s| s.hwnd == hwnd && s.handoff.is_none())
        {
            if let Some(clock) = &s.clock {
                clock.monitor(step.monitor);
            }
            if !s.following {
                s.pending = Some((step, sample));
            } else if let Err(e) = s.move_to(step, sample) {
                crate::log!("widgets: drag visual update ({e})");
            }
        }
    });
}

/// Store the pointer, returning it for direct tracking if the clock stalls.
pub(super) fn sample(hwnd: HWND, started: f64, point: POINT) -> Option<Sample> {
    SCENE.with(|slot| {
        let mut slot = slot.borrow_mut();
        let s = slot.as_mut().filter(|s| s.hwnd == hwnd && s.handoff.is_none())?;
        s.samples.push(started, point);
        if let Some(metrics) = &mut s.metrics {
            metrics.input(started, drag_metrics::now());
        }
        s.reap_clock();
        if s.clock.is_none() || clock_stale(s.last_tick, started) {
            s.samples.latest()
        } else {
            None
        }
    })
}

pub(super) fn release_point(hwnd: HWND) -> Option<POINT> {
    let mut point = POINT::default();
    if unsafe { GetCursorPos(&mut point) }.is_ok() {
        return Some(point);
    }
    SCENE.with(|slot| {
        let slot = slot.borrow();
        let sample = slot.as_ref().filter(|s| s.hwnd == hwnd)?.samples.latest()?;
        Some(POINT { x: sample.point.0 as i32, y: sample.point.1 as i32 })
    })
}

/// Start one finite Composition settle. The owner's normal placement
/// path keeps/persists the geometry and the scene stays until its reply.
pub(super) fn settle(hwnd: HWND, to: RECT, released: f64) {
    let wait = SCENE.with(|s| {
        s.borrow_mut().as_mut().filter(|s| s.hwnd == hwnd).map(|s| {
            if s.handoff.is_some() {
                return None;
            }
            s.clock = None;
            s.handoff = Some(Handoff {
                position: to,
                velocity: s.samples.velocity(released),
                requested: false,
            });
            unsafe {
                let _ = KillTimer(Some(s.reply()), ESCAPE_TIMER);
            }
            let wait = s.start_landing();
            if let Some(metrics) = s.metrics.take() {
                metrics.finish();
            }
            Some(wait)
        })
    });
    match wait {
        Some(Some(wait)) => {
            cancel_timer();
            arm(wait, request_present);
        }
        Some(None) => {}
        None => { tell(hwnd, to, HWND::default()); }
    }
}

fn arm(ms: u32, task: fn()) {
    let timer = motion::after(ms, Box::new(task));
    if let Some(timer) = timer {
        SCENE.with(|s| {
            if let Some(scene) = s.borrow_mut().as_mut() {
                scene.timer = Some(timer);
            }
        });
    }
}

fn tell(hwnd: HWND, to: RECT, reply: HWND) -> bool {
    if !unsafe { IsWindow(Some(hwnd)) }.as_bool() {
        return false;
    }
    edit::send(hwnd, edit::Action::Place(to));
    unsafe { PostMessageW(Some(hwnd), LANDED, WPARAM(reply.0 as usize), LPARAM(0)).is_ok() }
}

fn request_present() {
    let next = SCENE.with(|s| {
        let mut s = s.borrow_mut();
        let s = s.as_mut()?;
        let reply = s.reply();
        let h = s.handoff.as_mut()?;
        if h.requested {
            return None;
        }
        h.requested = true;
        Some((s.hwnd, h.position, reply))
    });
    if let Some((hwnd, to, reply)) = next {
        cancel_timer();
        if tell(hwnd, to, reply) {
            arm(PRESENT_TIMEOUT_MS, clear);
        } else {
            clear();
        }
    }
}

#[derive(Clone, Copy)]
struct FlushReply {
    window: usize,
    message: u32,
    card: usize,
}

fn flush_on_worker(reply: FlushReply) {
    static WORKER: OnceLock<Option<mpsc::SyncSender<FlushReply>>> = OnceLock::new();
    let worker = WORKER.get_or_init(|| {
        let (tx, rx) = mpsc::sync_channel::<FlushReply>(8);
        std::thread::Builder::new()
            .name("widget-dwm-flush".into())
            .spawn(move || {
                while let Ok(next) = rx.recv() {
                    let h = HWND(next.window as *mut _);
                    unsafe {
                        if !IsWindow(Some(h)).as_bool() {
                            continue;
                        }
                        let _ = DwmFlush();
                        let _ = PostMessageW(Some(h), next.message, WPARAM(next.card), LPARAM(0));
                    }
                }
            })
            .ok()
            .map(|_| tx)
    });
    // The handoff watchdog can restore the card if the worker is stuck in DWM.
    if worker.as_ref().is_none_or(|w| w.try_send(reply).is_err()) {
        crate::log!("widgets: compositor flush worker unavailable");
    }
}

fn after_commit(c: &Compositor, reply: HWND, message: u32, card: HWND) {
    let reply = FlushReply {
        window: reply.0 as usize,
        message,
        card: card.0 as usize,
    };
    let queued = c.RequestCommitAsync().and_then(|operation| {
        operation.SetCompleted(&AsyncActionCompletedHandler::new(move |_, _| {
            flush_on_worker(reply);
            Ok(())
        }))
    });
    if queued.is_err() {
        flush_on_worker(reply);
    }
}

/// Owner-thread reply, after the card's own blank or final frame is committed.
pub fn hidden(hwnd: HWND, reply: WPARAM, c: &Compositor) {
    after_commit(c, HWND(reply.0 as *mut _), HIDDEN, hwnd);
}

pub fn presented(hwnd: HWND, reply: WPARAM, c: &Compositor) {
    if reply.0 != 0 {
        after_commit(c, HWND(reply.0 as *mut _), PRESENTED, hwnd);
    }
}

fn finish_present() {
    if motion::full() {
        clear();
        return;
    }
    SCENE.with(|s| {
        if let Some(scene) = s.borrow().as_ref() {
            for hint in &scene.hints {
                let _ = motion::fade_to(
                    &scene.gfx.compositor,
                    &hint.visual.root,
                    0.,
                    motion::REDUCED_MS,
                    motion::LINEAR,
                );
            }
        }
    });
    cancel_timer();
    arm(motion::REDUCED_MS as u32 + 16, clear);
}

fn clear() {
    if let Some(mut s) = SCENE.with(|s| s.borrow_mut().take()) {
        s.clock = None;
        if let Some(metrics) = s.metrics.take() {
            metrics.finish();
        }
        if let Some(timer) = s.timer.take() {
            motion::cancel(timer);
        }
    }
}

fn cancel_timer() {
    let timer = SCENE.with(|s| s.borrow_mut().as_mut().and_then(|s| s.timer.take()));
    if let Some(timer) = timer {
        motion::cancel(timer);
    }
}

/// Complete a pending handoff now, for display or geometry changes.
pub fn hide() {
    cancel_timer();
    let next = SCENE.with(|s| {
        s.borrow()
            .as_ref()
            .and_then(|s| s.handoff.map(|h| h.requested))
    });
    match next {
        Some(false) => request_present(),
        Some(true) => arm(PRESENT_TIMEOUT_MS, clear),
        None => clear(),
    }
}

pub(super) fn displays_changed() {
    // An active scene still owns the windows it is using through its final
    // presentation. Idle windows can be destroyed on their owning thread now.
    let stale = HINT_WINDOWS.with(|pool| {
        pool.try_borrow_mut().ok().map(|mut pool| std::mem::take(&mut *pool))
    });
    drop(stale);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_tick_switches_to_direct_input_after_fifty_milliseconds() {
        assert!(!clock_stale(100., 150.));
        assert!(clock_stale(100., 150.1));
        assert!(!clock_stale(150.1, 151.));
    }

    #[test]
    fn watchdog_releases_orphaned_drags_even_without_capture_messages() {
        assert_eq!(watch(false, true, false), Watch::MissingCard);
        assert_eq!(watch(false, false, true), Watch::MissingCard);
        assert_eq!(watch(true, false, false), Watch::Cancel);
        assert_eq!(watch(true, true, true), Watch::Cancel);
        assert_eq!(watch(true, true, false), Watch::Continue);
    }

}
