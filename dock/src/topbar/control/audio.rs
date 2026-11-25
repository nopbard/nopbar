//! Audio devices and the apps' volumes (Core Audio, IPolicyConfig).

use super::*;

// ---- audio devices (Core Audio, IPolicyConfig) ----------------------------------------

/// Lists the output and input devices, follows changes while the panel is
/// open (IMMNotificationClient, no polling) and switches the default. Also
/// lists the apps with sound on the default output (their sessions'
/// events say when to look again) and sets their volume.
pub(super) fn audio_worker(shared: &Shared, rx: Receiver<AudioCmd>, tx: Sender<AudioCmd>) {
    use windows::Win32::Media::Audio::{IMMDeviceEnumerator, MMDeviceEnumerator};
    let e: IMMDeviceEnumerator = match unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_INPROC_SERVER) } {
        Ok(e) => e,
        Err(err) => {
            crate::log!("control: no audio devices: {err}");
            shared.update(|r| {
                r.outputs = Some(Vec::new());
                r.inputs = Some(Vec::new());
                r.apps = Some(Vec::new());
            });
            return;
        }
    };
    let changed = tx.clone();
    let client = devices::Watch::client(move || {
        let _ = changed.send(AudioCmd::Changed);
    });
    let watching = unsafe { e.RegisterEndpointNotificationCallback(&client) }.is_ok();
    let read = || {
        let (o, i) = unsafe { (devices::list(&e, Flow::Output), devices::list(&e, Flow::Input)) };
        shared.update(|r| {
            r.outputs = Some(o);
            r.inputs = Some(i);
        });
    };
    let mut icons = Icons::default();
    // Which binding of the mixer (a session handed to an older one is not
    // its).
    let mut gen = 0u64;
    let mut mixer = unsafe { Mixer::bind(&e, &tx, gen) };
    // The panel's last volume write carried out (Remote::app_seq).
    let mut applied = 0u64;
    let read_apps = |mixer: &mut Option<Mixer>, icons: &mut Icons, applied: u64| {
        let apps = mixer.as_mut().map(|m| unsafe { m.read() }).unwrap_or_default();
        let apps = icons.fill(apps, shared, &tx);
        shared.update(|r| take_read(r, apps, applied));
    };
    read();
    read_apps(&mut mixer, &mut icons, applied);
    let mut later = std::collections::VecDeque::new();
    loop {
        let cmd = match later.pop_front() {
            Some(c) => c,
            None => match rx.recv() {
                Ok(c) => c,
                Err(_) => break,
            },
        };
        if shared.quitting() {
            break;
        }
        match cmd {
            first @ (AudioCmd::Changed | AudioCmd::Apps | AudioCmd::Created(..)) => {
                // Changes come in bursts (one per role, per property, per
                // session).
                std::thread::sleep(Duration::from_millis(60));
                let mut devices = false;
                let mut batch = vec![first];
                while let Ok(c) = rx.try_recv() {
                    batch.push(c);
                }
                for c in batch {
                    match c {
                        AudioCmd::Changed => devices = true,
                        AudioCmd::Apps => {}
                        AudioCmd::Created(g, s) => {
                            if let Some(m) = mixer.as_mut().filter(|m| m.gen == g) {
                                unsafe { m.add(s.0) };
                            }
                        }
                        // Carried out before the read, which then reads it.
                        AudioCmd::AppVolume(k, p, seq) => {
                            if let Some(m) = mixer.as_mut() {
                                unsafe { m.set(k, p) };
                            }
                            applied = applied.max(seq);
                        }
                        other => later.push_back(other),
                    }
                }
                if devices {
                    read();
                    // The default output may be another device now.
                    if let Some(m) = mixer.take() {
                        unsafe { m.unbind() };
                    }
                    gen += 1;
                    mixer = unsafe { Mixer::bind(&e, &tx, gen) };
                }
                read_apps(&mut mixer, &mut icons, applied);
            }
            AudioCmd::AppVolume(k, p, seq) => {
                if let Some(m) = mixer.as_mut() {
                    unsafe { m.set(k, p) };
                }
                // Its change event (MIX_CTX) reads the apps again.
                applied = applied.max(seq);
            }
            AudioCmd::SetDefault(flow, id) => {
                // Gone since it was picked: nothing to switch.
                let list = unsafe { devices::list(&e, flow) };
                let name = list.iter().find(|d| d.id == id).map(|d| d.name.clone());
                match name {
                    None => {
                        crate::log!("control: the picked audio device is gone; not switching");
                        shared.update(|r| {
                            r.audio_fail = Some((flow, String::new()));
                            r.audio_fail_seq += 1;
                        });
                    }
                    Some(name) => {
                        if let Err(err) = unsafe { devices::set_default(&id) } {
                            crate::log!("control: cannot switch the audio device ({err})");
                            shared.update(|r| {
                                r.audio_fail = Some((flow, name));
                                r.audio_fail_seq += 1;
                            });
                        }
                    }
                }
                // What Windows really did (also when it did nothing): the
                // check follows it, the "Switching…" goes.
                read();
                shared.update(|r| {
                    if r.audio_busy.as_ref().is_some_and(|(_, b)| *b == id) {
                        r.audio_busy = None;
                    }
                });
            }
            AudioCmd::Quit => break,
        }
    }
    icons.stop();
    if let Some(m) = mixer.take() {
        unsafe { m.unbind() };
    }
    if watching {
        let _ = unsafe { e.UnregisterEndpointNotificationCallback(&client) };
    }
}

/// Our event context for the apps' volume changes.
pub(super) const MIX_CTX: windows::core::GUID = windows::core::GUID::from_u128(0x6e6f_7062_6172_4363_4d69_7865_7200_0001);
/// App icons are read this big (px; sharp at 20 DIP up to 200 %).
pub(super) const ICON_PX: i32 = 40;

/// An app's icon: `w` x `h` premultiplied BGRA.
#[derive(PartialEq, Eq)]
pub(super) struct AppIcon {
    pub(super) w: u32,
    pub(super) h: u32,
    pub(super) pixels: Vec<u8>,
}

impl std::fmt::Debug for AppIcon {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "AppIcon({}x{})", self.w, self.h)
    }
}

/// Icons by exe path (None: read and there is none).
pub(super) type IconMap = std::collections::HashMap<String, Option<Arc<AppIcon>>>;

/// Whose turn it is to read app icons: one reader in the process at a
/// time (the shell's calls can stall, and the readers of panels opened
/// and closed meanwhile must not pile up behind one), and the worker to
/// nudge when it is free.
pub(super) struct IconTurn {
    pub(super) busy: bool,
    pub(super) waiting: Option<Sender<AudioCmd>>,
}

impl IconTurn {
    /// The reader is ours (true), or `tx` is nudged when it is free.
    pub(super) fn take(&mut self, tx: &Sender<AudioCmd>) -> bool {
        if self.busy {
            self.waiting = Some(tx.clone());
            return false;
        }
        self.busy = true;
        true
    }

    /// The reader is done: the worker waiting for it, to nudge.
    pub(super) fn free(&mut self) -> Option<Sender<AudioCmd>> {
        self.busy = false;
        self.waiting.take()
    }
}

pub(super) static ICON_TURN: Mutex<IconTurn> = Mutex::new(IconTurn { busy: false, waiting: None });

/// Pure: reads the `missing` icons one at a time (`load`), keeping each
/// (`keep`), and stops between two once `quit` is set. How many it read.
pub(super) fn read_icons(missing: Vec<String>, quit: &AtomicBool, mut load: impl FnMut(&str) -> Option<Arc<AppIcon>>, mut keep: impl FnMut(String, Option<Arc<AppIcon>>)) -> usize {
    let mut n = 0;
    for exe in missing {
        if quit.load(Ordering::SeqCst) {
            break;
        }
        let icon = load(&exe);
        keep(exe, icon);
        n += 1;
    }
    n
}

/// An icon reader thread (a thread of its own: the shell wants an STA).
pub(super) struct Reader {
    /// Set to stop it after the icon it is on.
    pub(super) quit: Arc<AtomicBool>,
    /// Set by the reader once it kept its last icon and let the turn go,
    /// before it asks for a new read: the next read may start then, not
    /// only once its thread has returned (that comes later, and the ask
    /// may be answered first).
    pub(super) done: Arc<AtomicBool>,
    pub(super) thread: std::thread::JoinHandle<()>,
}

impl Reader {
    pub(super) fn done(&self) -> bool {
        self.done.load(Ordering::SeqCst)
    }

    /// Joined when its thread has returned; else let go (done, it is only
    /// returning; or stalled in the shell, not waited for).
    pub(super) fn reap(self) {
        if self.thread.is_finished() {
            let _ = self.thread.join();
        }
    }
}

/// One audio worker's app icons: those read, and its reader.
#[derive(Default)]
pub(super) struct Icons {
    pub(super) got: Arc<Mutex<IconMap>>,
    pub(super) reader: Option<Reader>,
}

impl Icons {
    /// The apps with the icons read so far; the missing ones are read on
    /// the reader, which asks for a new read when done.
    pub(super) fn fill(&mut self, mut apps: Vec<(AppMix, Option<String>)>, shared: &Shared, tx: &Sender<AudioCmd>) -> Vec<AppMix> {
        let mut missing = Vec::new();
        if let Ok(c) = self.got.lock() {
            for (a, exe) in apps.iter_mut() {
                match exe.as_ref().map(|x| c.get(x)) {
                    Some(Some(icon)) => a.icon = icon.clone(),
                    Some(None) => missing.extend(exe.clone()),
                    None => {}
                }
            }
        }
        // Ours is still reading: it asks again when done.
        if self.reading() {
            missing.clear();
        }
        missing.sort();
        missing.dedup();
        if !missing.is_empty() && !shared.quitting() && ICON_TURN.lock().is_ok_and(|mut t| t.take(tx)) {
            if let Some(old) = self.reader.take() {
                old.reap();
            }
            let quit = Arc::new(AtomicBool::new(false));
            let done = Arc::new(AtomicBool::new(false));
            let (got, stop, finished, tx) = (self.got.clone(), quit.clone(), done.clone(), tx.clone());
            let started = std::thread::Builder::new().name("cc-app-icons".into()).spawn(move || {
                let com = unsafe { CoInitializeEx(None, windows::Win32::System::Com::COINIT_APARTMENTTHREADED) }.is_ok();
                let load = |exe: &str| {
                    crate::icons::load(exe, ICON_PX)
                        .filter(|i| i.width > 0 && i.height > 0)
                        .map(|i| Arc::new(AppIcon { w: i.width, h: i.height, pixels: i.pixels }))
                };
                read_icons(missing, &stop, load, |exe, icon| {
                    if let Ok(mut c) = got.lock() {
                        c.insert(exe, icon);
                    }
                });
                if com {
                    unsafe { CoUninitialize() };
                }
                // Done before the ask: the read it starts may start the
                // next reader at once.
                finished.store(true, Ordering::SeqCst);
                let waiting = ICON_TURN.lock().ok().and_then(|mut t| t.free());
                let _ = tx.send(AudioCmd::Apps);
                if let Some(w) = waiting {
                    let _ = w.send(AudioCmd::Apps);
                }
            });
            match started {
                Ok(thread) => self.reader = Some(Reader { quit, done, thread }),
                Err(_) => {
                    if let Ok(mut t) = ICON_TURN.lock() {
                        t.free();
                    }
                }
            }
        }
        apps.into_iter().map(|(a, _)| a).collect()
    }

    /// The worker ends: its reader stops after the icon it is on. Joined
    /// when already done; a shell call that stalls is not waited for (the
    /// turn keeps that to one thread, and it ends when the call does).
    pub(super) fn stop(&mut self) {
        if let Some(r) = self.reader.take() {
            r.quit.store(true, Ordering::SeqCst);
            r.reap();
        }
    }

    /// Ours is reading (it has not said it is done).
    pub(super) fn reading(&self) -> bool {
        self.reader.as_ref().is_some_and(|r| !r.done())
    }
}

/// Something about an app's session changed (its volume, its state, it
/// went): the worker reads the apps again.
#[windows::core::implement(windows::Win32::Media::Audio::IAudioSessionEvents)]
pub(super) struct SessionEvents(Sender<AudioCmd>);

#[allow(non_snake_case)]
impl windows::Win32::Media::Audio::IAudioSessionEvents_Impl for SessionEvents_Impl {
    fn OnDisplayNameChanged(&self, _: &windows::core::PCWSTR, _: *const windows::core::GUID) -> Result<()> {
        Ok(())
    }
    fn OnIconPathChanged(&self, _: &windows::core::PCWSTR, _: *const windows::core::GUID) -> Result<()> {
        Ok(())
    }
    fn OnSimpleVolumeChanged(&self, _: f32, _: BOOL, _: *const windows::core::GUID) -> Result<()> {
        // Ours (MIX_CTX) too: the read after it confirms what was written
        // (a read begun before the write kept the value shown, merge_apps).
        let _ = self.0.send(AudioCmd::Apps);
        Ok(())
    }
    fn OnChannelVolumeChanged(&self, _: u32, _: *const f32, _: u32, _: *const windows::core::GUID) -> Result<()> {
        Ok(())
    }
    fn OnGroupingParamChanged(&self, _: *const windows::core::GUID, _: *const windows::core::GUID) -> Result<()> {
        Ok(())
    }
    fn OnStateChanged(&self, _: windows::Win32::Media::Audio::AudioSessionState) -> Result<()> {
        let _ = self.0.send(AudioCmd::Apps);
        Ok(())
    }
    fn OnSessionDisconnected(&self, _: windows::Win32::Media::Audio::AudioSessionDisconnectReason) -> Result<()> {
        let _ = self.0.send(AudioCmd::Apps);
        Ok(())
    }
}

/// A session handed over by OnSessionCreated, on its way to the audio
/// worker.
pub(super) struct NewSession(windows::Win32::Media::Audio::IAudioSessionControl);

// SAFETY: audio session objects are free-threaded; this one goes from the
// callback's thread (MTA) to the audio worker, which is in the MTA too.
unsafe impl Send for NewSession {}

/// A new session on the device: handed to the worker of mixer binding
/// `.1`, which lists it and reads the apps again.
#[windows::core::implement(windows::Win32::Media::Audio::IAudioSessionNotification)]
pub(super) struct NewSessions(Sender<AudioCmd>, u64);

#[allow(non_snake_case)]
impl windows::Win32::Media::Audio::IAudioSessionNotification_Impl for NewSessions_Impl {
    fn OnSessionCreated(&self, s: windows::core::Ref<windows::Win32::Media::Audio::IAudioSessionControl>) -> Result<()> {
        // Kept, not only listed again: a new enumerator may not have it yet
        // (IAudioSessionEnumerator's contract).
        let _ = match s.cloned() {
            Some(s) => self.0.send(AudioCmd::Created(self.1, NewSession(s))),
            None => self.0.send(AudioCmd::Apps),
        };
        Ok(())
    }
}

/// AUDCLNT_S_NO_SINGLE_PROCESS: GetProcessId's success code for a session
/// that several processes share (the ID is only the first one's).
pub(super) const NO_SINGLE_PROCESS: windows::core::HRESULT = windows::core::HRESULT(0x0889_000D);

/// Pure: the one process a session belongs to, from GetProcessId's own
/// result: S_OK only. A session of several processes (NO_SINGLE_PROCESS,
/// a success), the system's (0) or a failure belongs to no single app and
/// gets no row: a row's volume must not move other apps' sound.
pub(super) fn sole_process(hr: windows::core::HRESULT, pid: u32) -> Option<u32> {
    if hr == NO_SINGLE_PROCESS {
        return None;
    }
    (hr == windows::Win32::Foundation::S_OK && pid != 0).then_some(pid)
}

/// An app's session: its process, its instance ID and its controls.
pub(super) struct AppSession {
    pub(super) pid: u32,
    pub(super) id: String,
    pub(super) ctl: windows::Win32::Media::Audio::IAudioSessionControl2,
    pub(super) vol: Option<windows::Win32::Media::Audio::ISimpleAudioVolume>,
}

/// `c` as an app's session (None: the system sounds, expired, or not one
/// app's).
pub(super) unsafe fn app_session(c: &windows::Win32::Media::Audio::IAudioSessionControl2) -> Option<AppSession> {
    use windows::Win32::Media::Audio::AudioSessionStateExpired;
    if c.IsSystemSoundsSession() == windows::Win32::Foundation::S_OK || c.GetState().is_ok_and(|s| s == AudioSessionStateExpired) {
        return None;
    }
    // The raw result: the wrapper turns NO_SINGLE_PROCESS (a success) into
    // Ok(the first process).
    let mut pid = 0u32;
    let hr = (Interface::vtable(c).GetProcessId)(Interface::as_raw(c), &mut pid);
    let pid = sole_process(hr, pid)?;
    let p = c.GetSessionInstanceIdentifier().ok()?;
    let id = p.to_string().unwrap_or_default();
    windows::Win32::System::Com::CoTaskMemFree(Some(p.0 as *const _));
    if id.is_empty() {
        return None;
    }
    Some(AppSession { pid, id, ctl: c.clone(), vol: c.cast().ok() })
}

/// The default output device's sessions, followed while the panel is
/// open (registrations dropped by unbind).
pub(super) struct Mixer {
    /// Which binding this is (AudioCmd::Created).
    pub(super) gen: u64,
    pub(super) mgr: windows::Win32::Media::Audio::IAudioSessionManager2,
    pub(super) notify: windows::Win32::Media::Audio::IAudioSessionNotification,
    pub(super) events: windows::Win32::Media::Audio::IAudioSessionEvents,
    /// The sessions listened to now.
    pub(super) watched: Vec<windows::Win32::Media::Audio::IAudioSessionControl2>,
    /// Sessions handed over by OnSessionCreated: listed with the
    /// enumerator's until they expire.
    pub(super) added: Vec<windows::Win32::Media::Audio::IAudioSessionControl2>,
    /// Display names and exes by row (mix_key), for the rows listed now.
    pub(super) names: std::collections::HashMap<u64, (String, Option<String>)>,
}

impl Mixer {
    pub(super) unsafe fn bind(e: &windows::Win32::Media::Audio::IMMDeviceEnumerator, tx: &Sender<AudioCmd>, gen: u64) -> Option<Mixer> {
        use windows::Win32::Media::Audio::{eConsole, eRender, IAudioSessionManager2};
        let dev = e.GetDefaultAudioEndpoint(eRender, eConsole).ok()?;
        let mgr: IAudioSessionManager2 = dev.Activate(windows::Win32::System::Com::CLSCTX_ALL, None).ok()?;
        let notify: windows::Win32::Media::Audio::IAudioSessionNotification = NewSessions(tx.clone(), gen).into();
        // Notifications start once the sessions were listed.
        let _ = mgr.GetSessionEnumerator();
        let _ = mgr.RegisterSessionNotification(&notify);
        Some(Mixer { gen, mgr, notify, events: SessionEvents(tx.clone()).into(), watched: Vec::new(), added: Vec::new(), names: Default::default() })
    }

    pub(super) unsafe fn unwatch(&mut self) {
        for s in self.watched.drain(..) {
            let _ = s.UnregisterAudioSessionNotification(&self.events);
        }
    }

    pub(super) unsafe fn unbind(mut self) {
        self.unwatch();
        let _ = self.mgr.UnregisterSessionNotification(&self.notify);
    }

    /// Keeps a session handed over by OnSessionCreated.
    pub(super) unsafe fn add(&mut self, c: windows::Win32::Media::Audio::IAudioSessionControl) {
        if let Ok(c) = c.cast::<windows::Win32::Media::Audio::IAudioSessionControl2>() {
            if !self.added.iter().any(|a| a.as_raw() == c.as_raw()) {
                self.added.push(c);
            }
        }
    }

    /// The apps' sessions now: the enumerator's and those handed over, once
    /// each (by instance ID). Handed-over ones that expired go.
    pub(super) unsafe fn sessions(&mut self) -> Vec<AppSession> {
        use windows::Win32::Media::Audio::{AudioSessionStateExpired, IAudioSessionControl2};
        let mut found: Vec<IAudioSessionControl2> = Vec::new();
        if let Ok(en) = self.mgr.GetSessionEnumerator() {
            for i in 0..en.GetCount().unwrap_or(0) {
                if let Ok(c) = en.GetSession(i).and_then(|c| c.cast::<IAudioSessionControl2>()) {
                    found.push(c);
                }
            }
        }
        self.added.retain(|c| !c.GetState().is_ok_and(|s| s == AudioSessionStateExpired));
        found.extend(self.added.iter().cloned());
        let mut out: Vec<AppSession> = Vec::new();
        for c in &found {
            if let Some(s) = app_session(c) {
                if !out.iter().any(|o| o.id == s.id) {
                    out.push(s);
                }
            }
        }
        out
    }

    /// The apps now (one per process, in the order found), with each one's
    /// exe for its icon; every session listened to from here on.
    pub(super) unsafe fn read(&mut self) -> Vec<(AppMix, Option<String>)> {
        use windows::Win32::Media::Audio::AudioSessionStateActive;
        self.unwatch();
        let list = self.sessions();
        for s in &list {
            if s.ctl.RegisterAudioSessionNotification(&self.events).is_ok() {
                self.watched.push(s.ctl.clone());
            }
        }
        let ids: Vec<(u32, String)> = list.iter().map(|s| (s.pid, s.id.clone())).collect();
        let mut apps: Vec<(AppMix, Option<String>)> = Vec::new();
        for (key, pid, at) in mix_rows(&ids) {
            let mut a = AppMix { key, name: String::new(), level: 0, muted: true, active: false, icon: None };
            for &i in &at {
                let s = &list[i];
                let vol = s.vol.as_ref();
                a.level = a.level.max(vol.and_then(|v| v.GetMasterVolume().ok()).map(crate::audio::session::percent).unwrap_or(0));
                a.muted &= vol.is_some_and(|v| v.GetMute().is_ok_and(|m| m.as_bool()));
                a.active |= s.ctl.GetState().is_ok_and(|st| st == AudioSessionStateActive);
            }
            let (name, exe) = self.names.entry(key).or_insert_with(|| app_name(pid)).clone();
            a.name = name;
            apps.push((a, exe));
        }
        // The names of rows gone (their process or its sessions ended) go
        // too: a new process given the same ID is named afresh.
        let listed: Vec<u64> = apps.iter().map(|(a, _)| a.key).collect();
        prune_rows(&mut self.names, &listed);
        apps
    }

    /// Sets row `key`'s sessions to `p` percent (turning it up unmutes):
    /// only the sessions that row was read from (none once they ended,
    /// even if another process has its ID now).
    pub(super) unsafe fn set(&mut self, key: u64, p: u8) {
        let list = self.sessions();
        let ids: Vec<(u32, String)> = list.iter().map(|s| (s.pid, s.id.clone())).collect();
        let Some((_, _, at)) = mix_rows(&ids).into_iter().find(|r| r.0 == key) else { return };
        for i in at {
            if let Some(v) = &list[i].vol {
                let _ = v.SetMasterVolume(p.min(100) as f32 / 100.0, &MIX_CTX);
                if p > 0 {
                    let _ = v.SetMute(false, &MIX_CTX);
                }
            }
        }
    }
}

/// An app's name and exe from its process (the exe's description, else
/// its file name).
pub(super) fn app_name(pid: u32) -> (String, Option<String>) {
    use windows::Win32::System::Threading::{OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION};
    let exe = unsafe {
        OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok().and_then(|h| {
            let mut buf = [0u16; 1024];
            let mut len = buf.len() as u32;
            let got = QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, windows::core::PWSTR(buf.as_mut_ptr()), &mut len).is_ok();
            let _ = windows::Win32::Foundation::CloseHandle(h);
            got.then(|| String::from_utf16_lossy(&buf[..len as usize]).to_lowercase())
        })
    };
    let w = crate::apps::AppWindow { hwnd: HWND::default(), exe: exe.clone(), aumid: None, pid, started: 0 };
    (crate::apps::display_name(&w), exe)
}
