//! App icons for Launchpad, loaded off the UI thread.
//!
//! At most one loader thread in the process: it lives while there are
//! icons to load and ends when the queue is empty (nothing runs while
//! Launchpad is closed). Icons come from search's loader (the same disk
//! cache under %LOCALAPPDATA%\NopBar\icons) and are kept in memory (pixels
//! only, one size) so a reopen soon after shows them at once; two minutes
//! after Launchpad closed they are dropped (the disk cache brings them
//! back). The GPU surfaces made from them go away with the overlay.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED};
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

use crate::icons::Icon;

pub type Loaded = Option<Arc<Icon>>;

#[derive(Default)]
struct Queue {
    jobs: VecDeque<(String, u32)>,
    running: bool,
    /// Loaded for the open overlay since it last took them.
    done: Vec<(String, u32)>,
    /// The open overlay (0 = none). Loads that finish while none is open
    /// only fill the memory cache.
    target: isize,
    /// A WM_LP_ICONS is on its way (cleared when `done` is taken).
    posted: bool,
}

static QUEUE: Mutex<Option<Queue>> = Mutex::new(None);
/// Icons by (shell item, px); None = it has none.
static MEM: Mutex<Option<HashMap<(String, u32), Loaded>>> = Mutex::new(None);
/// Most icons kept in MEM (well above any real app list).
const MAX_MEM: usize = 2048;
/// Frame sizes icon files really carry (the shell makes 96 or 192 itself,
/// and boxes a frame it lacks: icons::looks_upscaled).
const FRAMES: [u32; 5] = [32, 48, 64, 128, 256];

/// The size Launchpad asks for to draw an icon at `px` device pixels: the
/// smallest real frame at or above it (drawn down with D2D's cubic
/// filter). Not icons::SOURCE_PX (256) for every app as the dock does:
/// Launchpad keeps hundreds of icons' pixels while open (at 125 % that is
/// 128 px, 64 KiB an icon, against 256 KiB at 256).
pub fn frame_px(px: u32) -> u32 {
    FRAMES.iter().copied().find(|&f| f >= px).unwrap_or(px.max(1))
}

/// Smaller real frames tried when the shell boxed the asked one (an app
/// without a frame that large).
const FALLBACK: [u32; 2] = [48, 32];

/// `key`'s icon asked at `px`: search's loader (its disk cache), and when
/// the shell boxed it (no frame that large in the file), the largest real
/// smaller frame instead.
fn load(key: &str, px: u32) -> Option<crate::icons::Icon> {
    let icon = crate::search::load_icon(key, px)?;
    if !crate::icons::looks_upscaled(&icon) {
        return Some(icon);
    }
    FALLBACK.iter().filter(|&&f| f < px).find_map(|&f| crate::search::load_icon(key, f).filter(|i| !crate::icons::looks_upscaled(i))).or(Some(icon))
}

/// Posted to TARGET when icons are ready (wParam 0).
pub const WM_LP_ICONS: u32 = windows::Win32::UI::WindowsAndMessaging::WM_APP + 2;

/// The icon for `key` at `px` if it was loaded already (Some(None): it has
/// none).
pub fn cached(key: &str, px: u32) -> Option<Loaded> {
    MEM.lock().ok()?.as_ref()?.get(&(key.to_string(), px)).cloned()
}

/// Loads `keys` at `px` (those not loaded yet), telling `target` as they
/// come. `first`: ahead of what is queued (the page on screen).
pub fn want(keys: &[String], px: u32, target: HWND, first: bool) {
    let Ok(mut g) = QUEUE.lock() else { return };
    // Open again: a release planned at the last close is off.
    CLOSED.fetch_add(1, Ordering::SeqCst);
    let q = g.get_or_insert_with(Queue::default);
    let me = target.0 as isize;
    if q.target != me {
        // A new overlay: nothing finished for an older one is its business.
        q.target = me;
        q.done.clear();
        q.posted = false;
    }
    let mut add: Vec<(String, u32)> = Vec::new();
    for k in keys {
        if cached(k, px).is_some() {
            continue;
        }
        let job = (k.clone(), px);
        if first {
            q.jobs.retain(|j| *j != job);
        } else if q.jobs.contains(&job) {
            continue;
        }
        if !add.contains(&job) {
            add.push(job);
        }
    }
    if first {
        for j in add.into_iter().rev() {
            q.jobs.push_front(j);
        }
    } else {
        q.jobs.extend(add);
    }
    if q.running || q.jobs.is_empty() {
        return;
    }
    q.running = true;
    drop(g);
    let started = std::thread::Builder::new().name("launchpad-icons".into()).spawn(run);
    if started.is_err() {
        if let Ok(mut g) = QUEUE.lock() {
            if let Some(q) = g.as_mut() {
                q.running = false;
            }
        }
    }
}

/// What finished loading since the last call: (key, px).
pub fn take_done() -> Vec<(String, u32)> {
    QUEUE
        .lock()
        .ok()
        .and_then(|mut g| {
            g.as_mut().map(|q| {
                q.posted = false;
                std::mem::take(&mut q.done)
            })
        })
        .unwrap_or_default()
}

/// The overlay closed: drop the queue (a load in progress finishes, then
/// the thread ends). The icons stay a while for a quick reopen, then go
/// (they are a few MB at Launchpad's size, and come back from the disk
/// cache on the next open).
pub fn stop() {
    if let Ok(mut g) = QUEUE.lock() {
        if let Some(q) = g.as_mut() {
            q.target = 0;
            q.jobs.clear();
            q.done.clear();
            q.posted = false;
        }
    }
    let gen = CLOSED.fetch_add(1, Ordering::SeqCst) + 1;
    plan_release(gen, KEEP_AFTER_CLOSE);
}

/// Releases the icons `after` from now if Launchpad is still closed since
/// close `gen`; while a load is still finishing it looks again a little
/// later (so a slow last load cannot keep them for good).
fn plan_release(gen: u64, after: std::time::Duration) {
    crate::service::after("launchpad icons", after, move || match release_if_closed(gen) {
        Release::Done(true) => crate::res::trim("launchpad icons dropped"),
        Release::Done(false) | Release::Reopened => {}
        Release::Loading => plan_release(gen, RECHECK_WHILE_LOADING),
    });
}

/// While the last load is still running at release time: look again after this.
const RECHECK_WHILE_LOADING: std::time::Duration = std::time::Duration::from_secs(10);

#[derive(Debug, PartialEq)]
enum Release {
    /// Released (true if anything was there).
    Done(bool),
    /// Opened since (or open): keep.
    Reopened,
    /// Closed, but the loader is still finishing a load.
    Loading,
}

/// What a planned release for close `gen` should do now.
fn release_decision(open: bool, running: bool, gen: u64, now_gen: u64) -> Release {
    if open || now_gen != gen {
        Release::Reopened
    } else if running {
        Release::Loading
    } else {
        Release::Done(false)
    }
}

/// How long Launchpad's icons stay in memory after it closed.
const KEEP_AFTER_CLOSE: std::time::Duration = std::time::Duration::from_secs(120);
/// Bumped by every close and every open: a release planned at close `gen`
/// happens only if nothing opened since.
static CLOSED: AtomicU64 = AtomicU64::new(0);

/// Frees the icon memory if Launchpad stayed closed since close `gen`.
/// Decided and done under QUEUE (which every open takes first): icons
/// that come with a reopen are never dropped by an older close's timer.
fn release_if_closed(gen: u64) -> Release {
    let Ok(g) = QUEUE.lock() else { return Release::Reopened };
    let open = g.as_ref().is_some_and(|q| q.target != 0);
    let running = g.as_ref().is_some_and(|q| q.running);
    let d = release_decision(open, running, gen, CLOSED.load(Ordering::SeqCst));
    if d != Release::Done(false) {
        return d;
    }
    let n = MEM.lock().ok().and_then(|mut m| m.take()).map_or(0, |m| m.len());
    drop(g);
    crate::log!("launchpad: {n} icons released after Launchpad stayed closed");
    Release::Done(n > 0)
}

fn run() {
    let com = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.is_ok();
    loop {
        let job = match QUEUE.lock() {
            Ok(mut g) => {
                let q = g.get_or_insert_with(Queue::default);
                match q.jobs.pop_front() {
                    Some(j) => j,
                    None => {
                        q.running = false;
                        break;
                    }
                }
            }
            Err(_) => break,
        };
        let (key, px) = job;
        if cached(&key, px).is_none() {
            let icon = load(&key, px).map(Arc::new);
            if let Ok(mut m) = MEM.lock() {
                let m = m.get_or_insert_with(HashMap::new);
                // One size kept (a monitor at another scale replaces it).
                m.retain(|(_, p), _| *p == px);
                // Apps uninstalled over days must not stay (audit A16):
                // past the cap the cache starts over and refills on use.
                if m.len() >= MAX_MEM {
                    m.clear();
                }
                m.insert((key.clone(), px), icon);
            }
        }
        // Told only to the overlay open now, once per batch taken.
        let tell = match QUEUE.lock() {
            Ok(mut g) => {
                let q = g.get_or_insert_with(Queue::default);
                if q.target == 0 {
                    0
                } else {
                    q.done.push((key, px));
                    if q.posted {
                        0
                    } else {
                        q.posted = true;
                        q.target
                    }
                }
            }
            Err(_) => 0,
        };
        let h = tell;
        if h != 0 {
            let posted = unsafe { PostMessageW(Some(HWND(h as *mut _)), WM_LP_ICONS, WPARAM(0), LPARAM(0)) };
            if posted.is_err() {
                // Not delivered (queue full, window gone): the next icon
                // posts again instead of `posted` staying stuck (A15).
                if let Ok(mut g) = QUEUE.lock() {
                    if let Some(q) = g.as_mut().filter(|q| q.target == h) {
                        q.posted = false;
                    }
                }
            }
        }
    }
    if com {
        unsafe { CoUninitialize() };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_asked_are_real_ones_at_or_above() {
        for (px, f) in [(16, 32), (32, 32), (40, 48), (64, 64), (80, 128), (96, 128), (112, 128), (128, 128), (144, 256), (192, 256), (256, 256)] {
            assert_eq!(frame_px(px), f, "{px}");
        }
        assert_eq!(frame_px(300), 300, "past the largest frame: the size itself");
        assert_eq!(frame_px(0), 32);
    }

    #[test]
    fn release_only_for_the_same_close_and_after_loads() {
        assert_eq!(release_decision(false, false, 3, 3), Release::Done(false));
        assert_eq!(release_decision(false, true, 3, 3), Release::Loading, "a slow last load: look again later");
        assert_eq!(release_decision(false, false, 3, 4), Release::Reopened, "opened (and maybe closed) since");
        assert_eq!(release_decision(true, false, 3, 3), Release::Reopened);
        assert_eq!(release_decision(false, true, 3, 5), Release::Reopened);
    }
}
