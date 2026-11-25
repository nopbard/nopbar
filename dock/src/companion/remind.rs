//! Local reminders never enter the chat provider or conversation history.
use super::chat::paint::{self, rect};
use crate::notes::{
    reminders::{delivery_for, Delivery, Notice},
    reminder_delivery::Dispatcher,
    runtime::{self, Action},
};
use std::sync::Mutex;
use windows::{
    core::Result,
    Win32::Graphics::Direct2D::{Common::D2D_RECT_F, ID2D1DeviceContext},
};
enum Command { Notice(Notice), Bubble(Notice), Cancel(String) }
static QUEUE: Mutex<Vec<Command>> = Mutex::new(Vec::new());
thread_local! {
    static DISPATCHER: std::cell::RefCell<Dispatcher> = const { std::cell::RefCell::new(Dispatcher::new()) };
}
static WAKE: Mutex<(Option<crate::service::Id>, Option<std::time::Instant>)> = Mutex::new((None, None));
pub fn enqueue(id: String, text: String) {
    enqueue_notice(Notice::single(id, text));
}
pub fn enqueue_notice(notice: Notice) {
    QUEUE.lock().unwrap_or_else(|e| e.into_inner()).push(Command::Notice(notice));
    super::refresh();
}
pub fn enqueue_bubble(notice: Notice) {
    QUEUE.lock().unwrap_or_else(|e| e.into_inner()).push(Command::Bubble(notice));
    super::refresh();
}
pub fn delivery() -> Delivery {
    if super::notifications_quiet() { return Delivery::Defer; }
    let meeting = super::CALM.load(std::sync::atomic::Ordering::SeqCst) & 2 != 0;
    let system_fullscreen = || {
        crate::monitors::all()
            .iter()
            .any(|m| crate::dock::fullscreen_on_monitor(m.handle, m.rect))
    };
    super::APP.with(|a| {
        let a = a.try_borrow().ok();
        let a = a.as_ref().and_then(|a| a.as_ref());
        a.map_or_else(
            || delivery_for(false, false, false, system_fullscreen(), meeting),
            |a| {
                delivery_for(
                    a.cfg.enabled,
                    (!a.cfg.visible && a.peek_home().is_none())
                        || a.cfg.snooze_until.is_some()
                        || super::living::away()
                        || crate::dock::companion_mode() != super::visibility::Mode::Run,
                    a.ground
                        .is_some_and(|g| super::interaction::covered_stops(g.covered, &a.grab)),
                    !a.cfg.enabled && system_fullscreen(),
                    meeting,
                )
            },
        )
    })
}
pub fn cancel(id: &str) {
    QUEUE.lock().unwrap_or_else(|e| e.into_inner()).push(Command::Cancel(id.into()));
    super::refresh();
}
pub fn poll() {
    DISPATCHER.with(|state| {
    // COM/UI work may pump messages. Nested polls leave their commands queued;
    // producers never wait on a lock held across an external call.
    let Ok(mut dispatcher) = state.try_borrow_mut() else { return; };
    let commands = std::mem::take(&mut *QUEUE.lock().unwrap_or_else(|e| e.into_inner()));
    let mut cancelled = Vec::new();
    for command in commands {
        match command {
            Command::Notice(notice) => dispatcher.push(notice),
            Command::Bubble(notice) => dispatcher.push_bubble(notice, std::time::Instant::now()),
            Command::Cancel(id) => { dispatcher.cancel(&id); cancelled.push(id); }
        }
    }
    if !cancelled.is_empty() { super::chat::cancel_reminders(&cancelled); }
    let preferences = crate::notes::preferences();
    if !preferences.notes || !preferences.notes_reminders {
        arm(None);
        return;
    }
    let next = dispatcher.poll(
        std::time::Instant::now(), delivery(), crate::notes::toast::show,
        |notice| {
            let shown = super::chat::reminder(notice.clone());
            if shown {
                super::APP.with(|a| {
                    if let Ok(mut a) = a.try_borrow_mut() {
                        if let Some(a) = a.as_mut() {
                            if a.peek_home().is_some() { a.peek.notify(a.start.elapsed().as_secs_f64()); }
                        }
                    }
                });
            }
            shown
        },
        |notice| runtime::action(Action::Delivered(notice)),
    );
    arm(next);
    });
}
fn arm(delay: Option<std::time::Duration>) {
    let mut wake = WAKE.lock().unwrap_or_else(|e| e.into_inner());
    let next = delay.map(|d| std::time::Instant::now() + d);
    let unchanged = match (wake.1, next) {
        (None, None) => true,
        (Some(a), Some(b)) => a.saturating_duration_since(b).max(b.saturating_duration_since(a)) < std::time::Duration::from_millis(1),
        _ => false,
    };
    if unchanged { return; }
    wake.1 = next;
    if let Some(id) = wake.0 { crate::service::poke(id); }
    else if delay.is_some() { wake.0 = crate::service::add(Box::new(RetryWatch)); }
}
struct RetryWatch;
impl crate::service::Watch for RetryWatch {
    fn name(&self) -> &'static str { "reminder-delivery" }
    fn handles(&self) -> Vec<windows::Win32::Foundation::HANDLE> { Vec::new() }
    fn wake(&mut self, _: crate::service::Wake) -> crate::service::Next {
        let mut wake = WAKE.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(at) = wake.1 {
            let left = at.saturating_duration_since(std::time::Instant::now());
            if !left.is_zero() { return crate::service::Next::Sleep(left); }
        }
        *wake = (None, None);
        drop(wake);
        super::refresh();
        crate::service::Next::Done
    }
}

pub fn bounds(b: D2D_RECT_F) -> [D2D_RECT_F; 2] {
    let x = (b.left + b.right) / 2. - 98.;
    [
        rect(x, b.bottom + 6., 92., 24.),
        rect(x + 96., b.bottom + 6., 100., 24.),
    ]
}
pub fn place(items: &mut [paint::Item], work: D2D_RECT_F, edge: Option<super::peek::Edge>) {
    let Some(i) = items.first_mut() else {
        return;
    };
    if edge.is_none() || edge == Some(super::peek::Edge::Bottom) {
        i.bounds.top -= 30.;
        i.bounds.bottom -= 30.;
    }
    let dy =
        (work.top + 4. - i.bounds.top).max(0.) - (i.bounds.bottom + 30. - work.bottom + 4.).max(0.);
    let rs = bounds(i.bounds);
    let dx = (work.left + 4. - rs[0].left).max(0.) - (rs[1].right - work.right + 4.).max(0.);
    i.bounds.top += dy;
    i.bounds.bottom += dy;
    i.bounds.left += dx;
    i.bounds.right += dx;
}
pub unsafe fn buttons(
    dc: &ID2D1DeviceContext,
    gfx: &crate::gfx::Gfx,
    b: D2D_RECT_F,
    elapsed: f64,
    theme: &crate::theme::Theme,
) -> Result<()> {
    if !(0.06..59.9).contains(&elapsed) {
        return Ok(());
    }
    use crate::desklayer::card as glass;
    let [complete, snooze] = bounds(b);
    glass::pill(dc, gfx, theme, crate::i18n::t("remind.complete"), complete, true)?;
    glass::pill_on_backdrop(dc, gfx, theme, crate::i18n::t("remind.snooze"), snooze)?;
    Ok(())
}
pub fn click(id: &str, b: D2D_RECT_F, x: f32, y: f32) -> bool {
    let rs = bounds(b);
    let a = if !id.is_empty() && paint::contains(rs[0], x, y) {
        Action::Complete(id.into())
    } else if !id.is_empty() && paint::contains(rs[1], x, y) {
        Action::Snooze(id.into())
    } else if paint::contains(b, x, y) {
        Action::Reveal(id.into())
    } else {
        return false;
    };
    runtime::action(a);
    true
}
