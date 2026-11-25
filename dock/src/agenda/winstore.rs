//! Windows' own calendar store (Windows.ApplicationModel.Appointments):
//! the calendars other apps keep in Windows. It opens for an unpackaged
//! app too (a desktop process is not held to the appointments capability),
//! read only. Few apps write to it any more (the old Mail and Calendar app
//! did; the new Outlook does not), so its switch is off by default.
//!
//! Runs on the agenda worker thread (COM is set up there); every wait is
//! bounded by a completion handler with a deadline, never a busy loop.

use std::sync::mpsc::channel;
use std::time::Duration;

use windows::core::Interface;
use windows::ApplicationModel::Appointments::{AppointmentManager, AppointmentStoreAccessType, FindAppointmentsOptions};
use windows::Foundation::{DateTime, TimeSpan};
use windows_future::{AsyncActionCompletedHandler, AsyncOperationCompletedHandler, AsyncStatus, IAsyncInfo, IAsyncOperation};

use super::ics::Event;
use super::Why;

/// Seconds between 1601-01-01 (WinRT's DateTime) and 1970-01-01.
const EPOCH_1601: i64 = 11_644_473_600;
/// The most appointments read at once.
const MAX_COUNT: u32 = 500;

fn to_datetime(utc: i64) -> DateTime {
    DateTime { UniversalTime: (utc + EPOCH_1601) * 10_000_000 }
}

fn from_datetime(d: DateTime) -> i64 {
    d.UniversalTime.div_euclid(10_000_000) - EPOCH_1601
}

/// Waits at most `ms` for `op` (cancelled on the deadline).
fn wait<T: windows::core::RuntimeType + 'static>(op: windows::core::Result<IAsyncOperation<T>>, ms: u64) -> Option<T> {
    let op = op.ok()?;
    let (tx, rx) = channel();
    op.SetCompleted(&AsyncOperationCompletedHandler::new(move |_, status| {
        let _ = tx.send(status);
        Ok(())
    }))
    .ok()?;
    match rx.recv_timeout(Duration::from_millis(ms)) {
        Ok(AsyncStatus::Completed) => op.GetResults().ok(),
        _ => {
            if let Ok(info) = op.cast::<IAsyncInfo>() {
                let _ = info.Cancel();
            }
            None
        }
    }
}

/// The appointments overlapping [from, to) (UTC seconds), as events.
pub fn fetch(from: i64, to: i64) -> Result<Vec<Event>, Why> {
    let Some(store) = wait(AppointmentManager::RequestStoreAsync(AppointmentStoreAccessType::AllCalendarsReadOnly), 10_000) else {
        crate::log!("agenda: the Windows calendar store did not open");
        return Err(Why::Store);
    };
    let opts = FindAppointmentsOptions::new().map_err(|_| Why::Store)?;
    let _ = opts.SetMaxCount(MAX_COUNT);
    let span = TimeSpan { Duration: (to - from).max(0) * 10_000_000 };
    let Some(list) = wait(store.FindAppointmentsAsyncWithOptions(to_datetime(from), span, &opts), 10_000) else {
        crate::log!("agenda: the Windows calendar store did not answer");
        return Err(Why::Store);
    };
    let mut out = Vec::new();
    for a in list {
        let (Ok(start), Ok(dur)) = (a.StartTime(), a.Duration()) else { continue };
        if a.IsCanceledMeeting().unwrap_or(false) {
            continue;
        }
        let start = from_datetime(start);
        let end = start + dur.Duration.div_euclid(10_000_000).max(0);
        let all_day = a.AllDay().unwrap_or(false);
        let url = a.Uri().ok().and_then(|u| u.AbsoluteUri().ok()).map(|s| s.to_string()).filter(|u| crate::updates::is_https_url(u)).unwrap_or_default();
        let mut ev = Event {
            start,
            end,
            all_day,
            title: a.Subject().map(|s| s.to_string()).unwrap_or_default(),
            location: a.Location().map(|s| s.to_string()).unwrap_or_default(),
            url,
        };
        if all_day {
            // Stored as local midnights: the days themselves.
            let first = super::local_day(start);
            let last = super::local_day((end - 1).max(start));
            ev.start = first * 86_400;
            ev.end = (last + 1) * 86_400;
        }
        out.push(ev);
    }
    out.sort_by(|a, b| (a.start, &a.title).cmp(&(b.start, &b.title)));
    crate::log!("agenda: Windows calendar: {} events", out.len());
    Ok(out)
}

/// Opens Windows' calendar app at `start` (UTC seconds), on its own short
/// thread; the calendar protocol if that does not work.
pub fn show(start: i64) {
    let ok = match AppointmentManager::ShowTimeFrameAsync(to_datetime(start), TimeSpan { Duration: 3600 * 10_000_000 }) {
        Ok(op) => {
            let (tx, rx) = channel();
            let set = op.SetCompleted(&AsyncActionCompletedHandler::new(move |_, status| {
                let _ = tx.send(status);
                Ok(())
            }));
            set.is_ok() && matches!(rx.recv_timeout(Duration::from_secs(5)), Ok(AsyncStatus::Completed))
        }
        Err(_) => false,
    };
    if !ok {
        let _ = crate::launch::environment::launch("open", "outlookcal:", "", "");
    }
}
