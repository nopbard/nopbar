//! Local calendar, session state and the purposeful animation gate.
use super::{rhythm, App, State};
use std::time::Instant;
use windows::Win32::System::SystemInformation::GetLocalTime;

impl App {
    pub(super) fn living(&mut self, base: State, reaction: bool) -> super::purpose::Output {
        let blocked = super::chat::is_open()
            || self.wake.active() || self.rest.is_some() || self.bubble.open()
            || super::convenience::menu_open() || self.trip.is_some() || self.grab.busy()
            || reaction
            || matches!(base, State::Notify | State::Wait | State::Done | State::Error | State::Peek);
        self.purpose.frame_at(super::purpose::uptime(), !blocked)
    }
}

pub(super) fn local_calendar() -> rhythm::Calendar {
    let t = unsafe { GetLocalTime() };
    rhythm::Calendar {
        date: t.wYear as u32 * 10000 + t.wMonth as u32 * 100 + t.wDay as u32,
        minute: t.wHour * 60 + t.wMinute,
    }
}
static SESSION_AWAY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
pub fn away() -> bool {
    SESSION_AWAY.load(std::sync::atomic::Ordering::SeqCst)
}
pub fn session_away(away: bool) {
    super::downloads::presence_changed(away, Instant::now());
    SESSION_AWAY.store(away, std::sync::atomic::Ordering::SeqCst);
    let seconds = super::purpose::uptime();
    super::APP.with(|a| {
        if let Some(a) = a.borrow_mut().as_mut() {
            a.rhythm.away(away);
            if a.purpose.away(away, seconds) { super::news::returned(); }
            a.purpose_due=Instant::now();
        }
    });
    super::refresh();
}
