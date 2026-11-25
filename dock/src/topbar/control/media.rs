//! Now playing (Windows.Media.Control).

use super::*;

// ---- now playing (Windows.Media.Control) -------------------------------------------

/// How a thumbnail is decoded: the whole image scaled so its shorter side
/// is `px` (never up), then the centered `px` square copied out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct ArtPlan {
    /// The scaled image.
    pub(super) w: u32,
    pub(super) h: u32,
    pub(super) px: u32,
}

impl ArtPlan {
    pub(super) fn crop(&self) -> WICRect {
        WICRect { X: ((self.w - self.px) / 2) as i32, Y: ((self.h - self.px) / 2) as i32, Width: self.px as i32, Height: self.px as i32 }
    }
}

/// None for images not worth decoding: empty, over ART_MAX_SRC_PIXELS, or
/// a strip (longer side over ART_MAX_ASPECT x the shorter).
pub(super) fn art_plan(sw: u32, sh: u32) -> Option<ArtPlan> {
    let (short, long) = (sw.min(sh) as u64, sw.max(sh) as u64);
    if short == 0 || short * long > ART_MAX_SRC_PIXELS || long > short * ART_MAX_ASPECT {
        return None;
    }
    let px = short.min(ART_PX as u64);
    let scaled = |d: u32| ((d as u64 * px + short / 2) / short).max(px) as u32;
    Some(ArtPlan { w: scaled(sw), h: scaled(sh), px: px as u32 })
}

/// Decodes encoded thumbnail bytes into small square art (needs COM; runs
/// on the media worker). Ok(None): not an image we show.
pub(super) fn decode_art(bytes: &[u8]) -> Result<Option<Art>> {
    unsafe {
        let factory: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)?;
        let stream = factory.CreateStream()?;
        stream.InitializeFromMemory(bytes)?;
        let decoder = factory.CreateDecoderFromStream(&stream, std::ptr::null(), WICDecodeMetadataCacheOnDemand)?;
        let frame = decoder.GetFrame(0)?;
        let (mut sw, mut sh) = (0u32, 0u32);
        frame.GetSize(&mut sw, &mut sh)?;
        // Bounded before any pixel is decoded.
        let Some(plan) = art_plan(sw, sh) else { return Ok(None) };
        // Scale straight from the frame (codecs that can, like JPEG, then
        // decode at the smaller size), convert the small result and copy
        // only the square out of it.
        let scaler = factory.CreateBitmapScaler()?;
        scaler.Initialize(&frame, plan.w, plan.h, WICBitmapInterpolationModeHighQualityCubic)?;
        let conv = factory.CreateFormatConverter()?;
        conv.Initialize(&scaler, &GUID_WICPixelFormat32bppPBGRA, WICBitmapDitherTypeNone, None, 0.0, WICBitmapPaletteTypeCustom)?;
        let px = plan.px;
        let mut pixels = vec![0u8; (px * px * 4) as usize];
        conv.CopyPixels(&plan.crop(), px * 4, &mut pixels)?;
        Ok(Some(Art { px, pixels }))
    }
}

/// What reading a track's thumbnail gave.
pub(super) enum Fetch {
    /// The app sent none (yet): apps often send it after the title.
    Absent,
    /// Timed out, cancelled or a read error: worth another try.
    Failed,
    /// Definitely nothing to show (too big, empty).
    Bad,
    /// The encoded image.
    Bytes(Vec<u8>),
}

/// Reads the thumbnail into memory: every step bounded and cancelled on
/// close (the same wait() as every WinRT call here), at most ART_MAX_BYTES.
pub(super) fn read_art(thumb: Option<IRandomAccessStreamReference>, shared: &Shared) -> Fetch {
    let Some(thumb) = thumb else { return Fetch::Absent };
    let Some(stream) = wait(thumb.OpenReadAsync(), 2000, shared, Kind::Media, "album art") else { return Fetch::Failed };
    let size = stream.Size().unwrap_or(0);
    if size > ART_MAX_BYTES {
        return Fetch::Bad;
    }
    let reader = match stream.GetInputStreamAt(0).and_then(|i| DataReader::CreateDataReader(&i)) {
        Ok(r) => r,
        Err(e) => {
            crate::log!("control: album art: {e}");
            return Fetch::Failed;
        }
    };
    let _ = reader.SetInputStreamOptions(InputStreamOptions::Partial);
    let deadline = Instant::now() + Duration::from_millis(ART_READ_MS);
    let mut bytes = Vec::with_capacity(size as usize);
    loop {
        if shared.quitting() {
            return Fetch::Failed;
        }
        let want = if size > 0 { (size - bytes.len() as u64).min(ART_CHUNK as u64) as u32 } else { ART_CHUNK };
        if want == 0 {
            break;
        }
        let left = deadline.saturating_duration_since(Instant::now()).as_millis() as u64;
        if left == 0 {
            crate::log!("control: album art: not read in {ART_READ_MS} ms");
            return Fetch::Failed;
        }
        let Some(n) = wait(reader.LoadAsync(want), left, shared, Kind::Media, "album art read") else { return Fetch::Failed };
        if n == 0 {
            break;
        }
        let at = bytes.len();
        bytes.resize(at + n as usize, 0);
        if let Err(e) = reader.ReadBytes(&mut bytes[at..]) {
            crate::log!("control: album art: {e}");
            return Fetch::Failed;
        }
        if bytes.len() as u64 > ART_MAX_BYTES {
            return Fetch::Bad;
        }
    }
    if bytes.is_empty() {
        Fetch::Bad
    } else {
        Fetch::Bytes(bytes)
    }
}

/// (app, title, artist, album, track number).
pub(super) type ArtKey = (String, String, String, String, i32);

/// The art of the current track, so playback-state events do not read or
/// decode it again. Lives on the worker (gone when the panel closes) and is
/// cleared when the session changes.
#[derive(Default)]
pub(super) struct ArtCache {
    pub(super) key: Option<ArtKey>,
    /// Hash of the bytes `art` was decoded from (or failed to decode from).
    pub(super) hash: Option<u64>,
    pub(super) art: Option<Arc<Art>>,
    /// Nothing more to try for this track until its properties change.
    pub(super) settled: bool,
    /// Passing failures for this track; retried until ART_RETRIES.
    pub(super) failures: u8,
}

impl ArtCache {
    /// The art for `key`. `fresh`: the media properties changed (the art
    /// may have too): read it again, but decode only if the bytes differ.
    /// A playback-state event (`fresh` false) reuses what is settled.
    pub(super) fn resolve(&mut self, key: ArtKey, fresh: bool, fetch: impl FnOnce() -> Fetch, decode: impl FnOnce(&[u8]) -> Option<Art>) -> Option<Arc<Art>> {
        if self.key.as_ref() != Some(&key) {
            *self = ArtCache { key: Some(key), ..ArtCache::default() };
        } else if self.settled && !(fresh && self.failures < ART_RETRIES) {
            return self.art.clone();
        }
        match fetch() {
            Fetch::Absent => {
                self.art = None;
                self.hash = None;
                self.settled = false;
            }
            Fetch::Failed => {
                // Keep what is shown; give up after a few tries.
                self.failures += 1;
                self.settled = self.failures >= ART_RETRIES;
            }
            Fetch::Bad => {
                self.art = None;
                self.hash = None;
                self.settled = true;
                self.failures = 0;
            }
            Fetch::Bytes(b) => {
                let mut h = std::collections::hash_map::DefaultHasher::new();
                b.hash(&mut h);
                let h = h.finish();
                if self.hash != Some(h) {
                    self.art = decode(&b).map(Arc::new);
                    self.hash = Some(h);
                }
                self.settled = true;
                self.failures = 0;
            }
        }
        self.art.clone()
    }
}

/// `session`: the worker's count of sessions bound so far (part of gen).
pub(super) fn read_media(s: &Session, shared: &Shared, cache: &mut ArtCache, fresh: bool, session: u64) -> Option<Media> {
    let (playing, prev, next, toggle, timeline) = {
        let info = s.GetPlaybackInfo().ok()?;
        let status = info.PlaybackStatus().unwrap_or(PlaybackStatus::Closed);
        if status == PlaybackStatus::Closed {
            return None;
        }
        let controls = info.Controls().ok();
        let can = |f: &dyn Fn(&windows::Media::Control::GlobalSystemMediaTransportControlsSessionPlaybackControls) -> Result<bool>| {
            controls.as_ref().and_then(|c| f(c).ok()).unwrap_or(false)
        };
        (
            status == PlaybackStatus::Playing,
            can(&|c| c.IsPreviousEnabled()),
            can(&|c| c.IsNextEnabled()),
            can(&|c| c.IsPlayPauseToggleEnabled()) || can(&|c| c.IsPlayEnabled()) || can(&|c| c.IsPauseEnabled()),
            read_timeline(s, &info),
        )
    };
    let props = wait(s.TryGetMediaPropertiesAsync(), 2000, shared, Kind::Media, "media properties");
    let text = |f: &dyn Fn(&windows::Media::Control::GlobalSystemMediaTransportControlsSessionMediaProperties) -> Result<windows::core::HSTRING>| {
        props.as_ref().and_then(|p| f(p).ok()).map(|h| h.to_string_lossy()).unwrap_or_default()
    };
    let title = text(&|p| p.Title());
    let mut artist = text(&|p| p.Artist());
    if artist.is_empty() {
        artist = text(&|p| p.AlbumArtist());
    }
    let app = s.SourceAppUserModelId().map(|h| h.to_string_lossy()).unwrap_or_default();
    let track = props.as_ref().and_then(|p| p.TrackNumber().ok()).unwrap_or(0);
    let key = (app.clone(), title.clone(), artist.clone(), text(&|p| p.AlbumTitle()), track);
    let gen = track_gen(session, &key);
    let art = cache.resolve(
        key,
        fresh,
        || read_art(props.as_ref().and_then(|p| p.Thumbnail().ok()), shared),
        |b| match decode_art(b) {
            Ok(a) => a,
            Err(e) => {
                crate::log!("control: album art: {e}");
                None
            }
        },
    );
    Some(Media {
        title,
        artist,
        app,
        playing,
        prev,
        next,
        toggle,
        art,
        timeline,
        gen,
        session: session as u32,
        refused: 0,
    })
}

/// A session and track's generation: the session count and the track's
/// identity (app, title, artist, album, number) hashed to 32 bits.
pub(super) fn track_gen(session: u64, key: &(String, String, String, String, i32)) -> u32 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    session.hash(&mut h);
    key.hash(&mut h);
    let v = h.finish();
    (v ^ (v >> 32)) as u32
}

/// 100 ns ticks (TimeSpan, DateTime) in ms.
pub(super) const TICKS_PER_MS: i64 = 10_000;
/// A report older than this is taken as made now (a clock that was set,
/// an app that never stamps its reports).
pub(super) const TIMELINE_MAX_AGE_MS: i64 = 6 * 3_600_000;

/// The session's timeline (None: it gives none, EndTime 0). Its position
/// was reported at LastUpdatedTime (wall clock): that is turned into a
/// monotonic instant once, here, so the card can move it on by itself.
fn read_timeline(s: &Session, info: &windows::Media::Control::GlobalSystemMediaTransportControlsSessionPlaybackInfo) -> Option<Timeline> {
    let tp = s.GetTimelineProperties().ok()?;
    let ms = |t: windows::Foundation::TimeSpan| t.Duration / TICKS_PER_MS;
    let (start_ms, end_ms, pos_ms) = (ms(tp.StartTime().ok()?), ms(tp.EndTime().ok()?), ms(tp.Position().ok()?));
    let updated = tp.LastUpdatedTime().map(|d| d.UniversalTime).unwrap_or(0);
    // FILETIME and DateTime count the same 100 ns from 1601 (UTC).
    let now_ft = unsafe { windows::Win32::System::SystemInformation::GetSystemTimeAsFileTime() };
    let now_ft = ((now_ft.dwHighDateTime as i64) << 32) | now_ft.dwLowDateTime as i64;
    let age = if updated > 0 { (now_ft - updated) / TICKS_PER_MS } else { 0 };
    let age = if (0..=TIMELINE_MAX_AGE_MS).contains(&age) { age } else { 0 };
    let at = Instant::now().checked_sub(Duration::from_millis(age as u64)).unwrap_or_else(Instant::now);
    let rate = info.PlaybackRate().ok().and_then(|r| r.Value().ok()).filter(|r| r.is_finite() && *r > 0.0).unwrap_or(1.0);
    let seekable = info.Controls().and_then(|c| c.IsPlaybackPositionEnabled()).unwrap_or(false);
    // Where a seek may go (both 0 when the app does not say).
    let seek_min_ms = tp.MinSeekTime().map(ms).unwrap_or(0);
    let seek_max_ms = tp.MaxSeekTime().map(ms).unwrap_or(0);
    let tl = Timeline { start_ms, end_ms, pos_ms, at, rate, seekable, seek_min_ms, seek_max_ms };
    tl.shown().then_some(tl)
}

/// The timeline read again alone (TimelinePropertiesChanged): kept, and
/// the panel told, only when it is not the course already known (apps
/// that report every second wake nothing).
pub(super) fn refresh_timeline(s: &Session, shared: &Shared) {
    let tl = {
        let Ok(info) = s.GetPlaybackInfo() else { return };
        read_timeline(s, &info)
    };
    let news = match shared.remote.lock() {
        Ok(r) => r.media.as_ref().is_some_and(|m| !crate::topbar::island::model::same_course(m.timeline.as_ref(), tl.as_ref(), m.playing)),
        Err(_) => false,
    };
    if news && !shared.quitting() {
        shared.update(|r| {
            if let Some(m) = r.media.as_mut() {
                m.timeline = tl;
            }
        });
    }
}

/// The session we listen to, with its subscriptions (removed on drop).
pub(super) struct Bound {
    pub(super) session: Session,
    pub(super) _subs: Vec<Defer>,
}

pub(super) fn bind(mgr: &SessionManager, tx: &Sender<MediaCmd>) -> Option<Bound> {
    let s = mgr.GetCurrentSession().ok()?;
    let mut subs = Vec::new();
    let t1 = tx.clone();
    if let Ok(a) = s.MediaPropertiesChanged(&TypedEventHandler::new(move |_, _| {
        let _ = t1.send(MediaCmd::Props);
        Ok(())
    })) {
        let s2 = s.clone();
        subs.push(Defer::new(move || {
            let _ = s2.RemoveMediaPropertiesChanged(a);
        }));
    }
    let t2 = tx.clone();
    if let Ok(p) = s.PlaybackInfoChanged(&TypedEventHandler::new(move |_, _| {
        let _ = t2.send(MediaCmd::Playback);
        Ok(())
    })) {
        let s2 = s.clone();
        subs.push(Defer::new(move || {
            let _ = s2.RemovePlaybackInfoChanged(p);
        }));
    }
    let t3 = tx.clone();
    if let Ok(p) = s.TimelinePropertiesChanged(&TypedEventHandler::new(move |_, _| {
        let _ = t3.send(MediaCmd::Timeline);
        Ok(())
    })) {
        let s2 = s.clone();
        subs.push(Defer::new(move || {
            let _ = s2.RemoveTimelinePropertiesChanged(p);
        }));
    }
    Some(Bound { session: s, _subs: subs })
}

pub(super) fn media_worker(shared: &Shared, rx: Receiver<MediaCmd>, tx: Sender<MediaCmd>) {
    run_media_worker(shared, rx, tx, SessionManager::RequestAsync());
}

fn run_media_worker(
    shared: &Shared,
    rx: Receiver<MediaCmd>,
    tx: Sender<MediaCmd>,
    manager: Result<IAsyncOperation<SessionManager>>,
) {
    let Some(mgr) = wait(manager, 5000, shared, Kind::Media, "media sessions") else {
        shared.update(|r| r.media = None);
        // Nothing to listen to: wait for the panel to close.
        while !shared.quitting() {
            match rx.recv() {
                Ok(MediaCmd::Quit) | Err(_) => break,
                Ok(_) => {}
            }
        }
        return;
    };
    let mut _changed = None;
    let tx2 = tx.clone();
    if let Ok(t) = mgr.CurrentSessionChanged(&TypedEventHandler::new(move |_, _| {
        let _ = tx2.send(MediaCmd::Session);
        Ok(())
    })) {
        let m = mgr.clone();
        _changed = Some(Defer::new(move || {
            let _ = m.RemoveCurrentSessionChanged(t);
        }));
    }
    let mut cache = ArtCache::default();
    // Sessions bound so far, and seeks refused (kept across reads).
    let mut session = 0u64;
    let mut refused = 0u32;
    let read = |b: &Option<Bound>, cache: &mut ArtCache, fresh: bool, session: u64, refused: u32| {
        let m = b.as_ref().and_then(|b| read_media(&b.session, shared, cache, fresh, session)).map(|m| Media { refused, ..m });
        if !shared.quitting() {
            shared.update(|r| r.media = m);
        }
    };
    let mut bound = bind(&mgr, &tx);
    read(&bound, &mut cache, true, session, refused);
    while let Ok(cmd) = rx.recv() {
        if shared.quitting() {
            break;
        }
        match cmd {
            MediaCmd::Quit => break,
            MediaCmd::Session => {
                // The old session's subscriptions go with it.
                drop(bound.take());
                bound = bind(&mgr, &tx);
                session += 1;
                // Another session's art is never reused.
                cache = ArtCache::default();
                read(&bound, &mut cache, true, session, refused);
            }
            MediaCmd::Props => read(&bound, &mut cache, true, session, refused),
            MediaCmd::Playback => read(&bound, &mut cache, false, session, refused),
            MediaCmd::Timeline => {
                if let Some(b) = &bound {
                    refresh_timeline(&b.session, shared);
                }
            }
            MediaCmd::Act(a) => {
                if let Some(b) = &bound {
                    let s = &b.session;
                    let op = match a {
                        MediaAct::Prev => s.TrySkipPreviousAsync(),
                        MediaAct::Toggle => s.TryTogglePlayPauseAsync(),
                        MediaAct::ToggleIn(want) => {
                            if want != session as u32 {
                                continue;
                            }
                            s.TryTogglePlayPauseAsync()
                        }
                        MediaAct::Next => s.TrySkipNextAsync(),
                        MediaAct::Seek(gen, ms) => {
                            // Only for the track it was meant for, and only
                            // where the app takes a seek now.
                            let to = shared.remote.lock().ok().and_then(|r| {
                                let m = r.media.as_ref().filter(|m| m.gen == gen)?;
                                m.timeline.as_ref()?.clamp_seek(ms)
                            });
                            let Some(to) = to else { continue };
                            s.TryChangePlaybackPositionAsync(to.saturating_mul(TICKS_PER_MS))
                        }
                    };
                    let done = wait(op, 3000, shared, Kind::Media, "media command");
                    if matches!(a, MediaAct::Seek(..)) {
                        if done != Some(true) {
                            // The card lets go of its hold at once.
                            refused = refused.wrapping_add(1);
                            if !shared.quitting() {
                                shared.update(|r| {
                                    if let Some(m) = r.media.as_mut() {
                                        m.refused = refused;
                                    }
                                });
                            }
                        }
                        // Apps that take a seek without a timeline event.
                        refresh_timeline(s, shared);
                    }
                }
            }
        }
    }
    // `bound` and `_changed` unsubscribe as they drop.
}

/// What is playing, for the Live Island (island/mod.rs).
#[derive(Clone, Debug, PartialEq, Default)]
pub(in crate::topbar) struct NowPlaying {
    pub(in crate::topbar) title: String,
    pub(in crate::topbar) artist: String,
    pub(in crate::topbar) app: String,
    pub(in crate::topbar) playing: bool,
    pub(in crate::topbar) prev: bool,
    pub(in crate::topbar) next: bool,
    pub(in crate::topbar) toggle: bool,
    pub(in crate::topbar) art: Option<Arc<Art>>,
    pub(in crate::topbar) timeline: Option<Timeline>,
    pub(in crate::topbar) gen: u32,
    pub(in crate::topbar) session: u32,
    pub(in crate::topbar) seek_refused: u32,
}

/// The panel's media listener (the same worker, session events and art
/// cache), run for the Live Island while it shows what is playing: it
/// posts `msg` to `hwnd` on every change and lives until dropped. Event
/// driven like the panel's: the thread sleeps in its channel between
/// Windows' session events.
pub(in crate::topbar) struct MediaWatch {
    pub(super) shared: Arc<Shared>,
    pub(super) tx: Sender<MediaCmd>,
}

impl MediaWatch {
    pub(in crate::topbar) fn start(hwnd: HWND, msg: u32) -> Self {
        let shared = Arc::new(Shared::posting(msg));
        shared.hwnd.store(hwnd.0 as isize, Ordering::SeqCst);
        let tx = spawn("island-media", &ISLAND_MEDIA_SLOT, &shared, media_worker);
        MediaWatch { shared, tx }
    }

    pub(in crate::topbar) fn now(&self) -> Option<NowPlaying> {
        let m = self.shared.snapshot().media?;
        Some(NowPlaying { title: m.title, artist: m.artist, app: m.app, playing: m.playing, prev: m.prev, next: m.next, toggle: m.toggle, art: m.art, timeline: m.timeline, gen: m.gen, session: m.session, seek_refused: m.refused })
    }

    pub(in crate::topbar) fn prev(&self) {
        let _ = self.tx.send(MediaCmd::Act(MediaAct::Prev));
    }
    pub(in crate::topbar) fn toggle(&self) {
        let _ = self.tx.send(MediaCmd::Act(MediaAct::Toggle));
    }
    /// Play / pause if session `session` (NowPlaying::session) is still
    /// the current one.
    pub(in crate::topbar) fn toggle_in(&self, session: u32) {
        let _ = self.tx.send(MediaCmd::Act(MediaAct::ToggleIn(session)));
    }
    pub(in crate::topbar) fn next(&self) {
        let _ = self.tx.send(MediaCmd::Act(MediaAct::Next));
    }
    /// To position `ms` on the session's timeline, if track `gen` still
    /// plays.
    pub(in crate::topbar) fn seek(&self, gen: u32, ms: i64) {
        let _ = self.tx.send(MediaCmd::Act(MediaAct::Seek(gen, ms)));
    }
}

impl Drop for MediaWatch {
    fn drop(&mut self) {
        self.shared.stop();
        let _ = self.tx.send(MediaCmd::Quit);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stopping_media_worker_wakes_pending_session_request() {
        let shared = Arc::new(Shared::new());
        let (tx, rx) = channel();
        let (release, hold) = channel::<()>();
        let (done, finished) = channel();
        let worker_shared = shared.clone();
        let worker_tx = tx.clone();
        let worker = std::thread::spawn(move || {
            let manager = IAsyncOperation::spawn(move || {
                let _ = hold.recv();
                Err(windows::core::Error::empty())
            });
            run_media_worker(&worker_shared, rx, worker_tx, Ok(manager));
            done.send(()).unwrap();
        });
        let deadline = Instant::now() + Duration::from_secs(2);
        while shared.wakers[Kind::Media as usize].lock().unwrap().is_none() {
            assert!(Instant::now() < deadline, "media worker did not start waiting");
            std::thread::sleep(Duration::from_millis(1));
        }
        shared.stop();
        let _ = tx.send(MediaCmd::Quit);
        let stopped = finished.recv_timeout(Duration::from_secs(1));
        drop(release);
        stopped.expect("media worker did not stop while its request was pending");
        worker.join().unwrap();
    }
}
