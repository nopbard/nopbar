//! Worker threads: waiting on WinRT, the one-at-a-time slots, the commands.

use super::*;


/// Waits for a WinRT operation, at most `ms`, and no longer than the panel
/// is open: the operation is cancelled on its deadline or on close. Event
/// driven (a completion handler), no polling.
pub(super) fn wait<T: windows::core::RuntimeType + 'static>(op: Result<IAsyncOperation<T>>, ms: u64, shared: &Shared, kind: Kind, what: &str) -> Option<T> {
    let op = match op {
        Ok(op) => op,
        Err(e) => {
            crate::log!("control: {what}: {e}");
            return None;
        }
    };
    let (tx, rx) = channel();
    let waker = &shared.wakers[kind as usize];
    if let Ok(mut w) = waker.lock() {
        *w = Some(tx.clone());
    }
    struct ClearWaker<'a>(&'a Mutex<Option<Sender<AsyncStatus>>>);
    impl Drop for ClearWaker<'_> {
        fn drop(&mut self) {
            if let Ok(mut w) = self.0.lock() {
                *w = None;
            }
        }
    }
    let _clear_waker = ClearWaker(waker);
    // Closed meanwhile (stop() ran before the waker was set).
    if shared.quitting() {
        cancel_op(&op);
        return None;
    }
    if let Err(e) = op.SetCompleted(&AsyncOperationCompletedHandler::new(move |_, status| {
        let _ = tx.send(status);
        Ok(())
    })) {
        crate::log!("control: {what}: {e}");
        cancel_op(&op);
        return None;
    }
    match rx.recv_timeout(Duration::from_millis(ms)) {
        Ok(AsyncStatus::Completed) => {
            let result = op.GetResults();
            let _ = op.Close();
            result.map_err(|e| crate::log!("control: {what}: {e}")).ok()
        }
        Ok(_) if shared.quitting() => {
            cancel_op(&op);
            None
        }
        Ok(s) => {
            crate::log!("control: {what}: status {}, {:?}", s.0, op.ErrorCode());
            let _ = op.Close();
            None
        }
        Err(_) => {
            crate::log!("control: {what}: no answer in {ms} ms; cancelled");
            cancel_op(&op);
            None
        }
    }
}

fn cancel_op<T: windows::core::RuntimeType + 'static>(op: &IAsyncOperation<T>) {
    let _ = op.Cancel();
    if matches!(op.Status(), Ok(AsyncStatus::Completed | AsyncStatus::Canceled | AsyncStatus::Error)) {
        let _ = op.Close();
    }
}

/// Runs a closure when dropped: event subscriptions are removed on every
/// way out of a worker.
pub(super) struct Defer(Option<Box<dyn FnOnce()>>);

impl Defer {
    pub(super) fn new(f: impl FnOnce() + 'static) -> Self {
        Defer(Some(Box::new(f)))
    }
}

impl Drop for Defer {
    fn drop(&mut self) {
        if let Some(f) = self.0.take() {
            f();
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Which {
    Wifi,
    Bt,
}

/// How long a Wi-Fi / Bluetooth switch Windows accepted may take to show
/// in the radio's state before the tile says it did not go through.
pub(super) const SWITCH_WAIT: std::time::Duration = std::time::Duration::from_secs(6);

/// One Wi-Fi / Bluetooth switch Windows accepted (SetStateAsync said
/// Allowed: queued, not done), followed until the radio's state shows it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Switch {
    /// On (true) or off.
    pub(super) target: bool,
    pub(super) deadline: std::time::Instant,
    /// SWITCH_WAIT passed without it: the tile said so. A late arrival
    /// still clears that.
    pub(super) timed_out: bool,
}

/// What a look at a switched radio means for its tile.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SwitchStep {
    /// Still on its way: "Switching…".
    Wait,
    /// The radio shows the target (in time or late): the ordinary status,
    /// no failure line.
    Done,
    /// SWITCH_WAIT is over and it does not: "Couldn't change".
    Failed,
    /// Already said it failed; nothing new.
    Quiet,
}

impl Switch {
    pub(super) fn new(target: bool, now: std::time::Instant) -> Self {
        Switch { target, deadline: now + SWITCH_WAIT, timed_out: false }
    }

    /// The radio is on / off (`on`; None: not a state it can be switched
    /// in, e.g. while Windows is still changing it), at `now`.
    pub(super) fn seen(&mut self, on: Option<bool>, now: std::time::Instant) -> SwitchStep {
        if on == Some(self.target) {
            SwitchStep::Done
        } else if self.timed_out {
            SwitchStep::Quiet
        } else if now >= self.deadline {
            self.timed_out = true;
            SwitchStep::Failed
        } else {
            SwitchStep::Wait
        }
    }
}

/// On / off for a switchable view, None otherwise.
pub(super) fn switch_state(v: RadioView) -> Option<bool> {
    match v {
        RadioView::On => Some(true),
        RadioView::Off => Some(false),
        _ => None,
    }
}

pub(super) enum RadioCmd {
    Set(Which, bool),
    /// A radio's state changed.
    Changed,
    /// The network changed: read the connected Wi-Fi name again.
    Ssid,
    WifiList,
    WifiSwitch(u64, bool),
    Quit,
}

pub(super) enum BrightCmd {
    Set(u8),
    Quit,
}

pub(super) enum QuickCmd {
    /// Turn the mobile hotspot on / off.
    Hotspot(bool),
    /// The network changed: the connection to share may be another.
    Network,
    /// A display came, went or changed mode.
    Displays,
    Quit,
}

pub(super) enum AudioCmd {
    /// A device came, went or changed, or the default moved.
    Changed,
    /// Make this device (endpoint ID) the default (every role), if it is
    /// still there.
    SetDefault(Flow, String),
    /// An app's session started, stopped, changed volume or a new one
    /// came: read the apps again.
    Apps,
    /// Set the sessions of the row with this key (mix_key) to this volume
    /// (percent): write number `.2` of the panel (Remote::app_seq).
    AppVolume(u64, u8, u64),
    /// A session made on the default output device after the mixer of
    /// generation `.0` listed them (kept: a later listing may miss it).
    Created(u64, NewSession),
    Quit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum MediaAct {
    Prev,
    Toggle,
    Next,
    /// To this position (ms on the session's timeline), for the track
    /// `gen` (Media::gen) only.
    Seek(u32, i64),
    /// Play / pause, only if session `.0` (Media::session) is still the
    /// one bound (a PiP's button: never another window's player).
    ToggleIn(u32),
}

pub(super) enum MediaCmd {
    /// The current session changed: listen to the new one.
    Session,
    /// Its title, artist or art changed.
    Props,
    /// Its playback state changed (the art is reused).
    Playback,
    /// Its timeline changed (a seek, a new position report).
    Timeline,
    Act(MediaAct),
    Quit,
}

pub(super) struct Workers {
    pub(super) radio: Sender<RadioCmd>,
    pub(super) bright: Sender<BrightCmd>,
    pub(super) media: Sender<MediaCmd>,
    pub(super) audio: Sender<AudioCmd>,
    pub(super) quick: Sender<QuickCmd>,
    pub(super) system: Sender<SystemCmd>,
    pub(super) devices: Sender<DeviceCmd>,
}

impl Workers {
    pub(super) fn idle() -> Self {
        Self {
            radio: channel().0, bright: channel().0, media: channel().0, audio: channel().0,
            quick: channel().0, system: channel().0, devices: channel().0,
        }
    }

    pub(super) fn start(shared: &Arc<Shared>) -> Self {
        if cfg!(test) {
            return Self::idle();
        }
        Workers {
            radio: spawn("cc-radio", &RADIO_SLOT, shared, radio_worker),
            bright: spawn("cc-bright", &BRIGHT_SLOT, shared, |s, rx, _| bright_worker(s, rx)),
            media: spawn("cc-media", &MEDIA_SLOT, shared, media_worker),
            audio: spawn("cc-audio", &AUDIO_SLOT, shared, audio_worker),
            quick: spawn("cc-quick", &QUICK_SLOT, shared, quick_worker),
            system: spawn("cc-system", &SYSTEM_SLOT, shared, system_worker),
            devices: spawn("cc-devices", &DEVICES_SLOT, shared, devices_worker),
        }
    }
    pub(super) fn quit(&self, shared: &Shared) {
        shared.stop();
        let _ = self.radio.send(RadioCmd::Quit);
        let _ = self.bright.send(BrightCmd::Quit);
        let _ = self.media.send(MediaCmd::Quit);
        let _ = self.audio.send(AudioCmd::Quit);
        let _ = self.quick.send(QuickCmd::Quit);
        let _ = self.system.send(SystemCmd::Quit);
        let _ = self.devices.send(DeviceCmd::Quit);
    }
}

/// A panel's job for one kind of worker.
pub(super) struct Job<C> {
    pub(super) shared: Arc<Shared>,
    pub(super) rx: Receiver<C>,
    pub(super) tx: Sender<C>,
}

/// At most one worker thread per kind in the process. A panel opened while
/// the last panel's worker is still finishing (a call running into its
/// deadline) queues its job on that thread instead of starting another;
/// a newer queued job replaces an older one.
pub(super) struct Slot<C> {
    pub(super) alive: bool,
    pub(super) next: Option<Job<C>>,
}

pub(super) static RADIO_SLOT: Mutex<Slot<RadioCmd>> = Mutex::new(Slot { alive: false, next: None });
pub(super) static BRIGHT_SLOT: Mutex<Slot<BrightCmd>> = Mutex::new(Slot { alive: false, next: None });
pub(super) static MEDIA_SLOT: Mutex<Slot<MediaCmd>> = Mutex::new(Slot { alive: false, next: None });
pub(super) static AUDIO_SLOT: Mutex<Slot<AudioCmd>> = Mutex::new(Slot { alive: false, next: None });
pub(super) static QUICK_SLOT: Mutex<Slot<QuickCmd>> = Mutex::new(Slot { alive: false, next: None });
static DEVICES_SLOT: Mutex<Slot<DeviceCmd>> = Mutex::new(Slot { alive: false, next: None });
static SYSTEM_SLOT: Mutex<Slot<SystemCmd>> = Mutex::new(Slot { alive: false, next: None });
/// The Live Island's own media listener (island/mod.rs), apart from the
/// panel's: both can be open at once.
pub(super) static ISLAND_MEDIA_SLOT: Mutex<Slot<MediaCmd>> = Mutex::new(Slot { alive: false, next: None });

/// Starts (or queues) a worker (COM MTA) for this panel and returns its
/// command channel. The worker gets a sender of its own too, for event
/// handlers; it ends on Quit.
pub(super) fn spawn<C: Send + 'static>(name: &str, slot: &'static Mutex<Slot<C>>, shared: &Arc<Shared>, f: fn(&Shared, Receiver<C>, Sender<C>)) -> Sender<C> {
    let (tx, rx) = channel();
    let job = Job { shared: shared.clone(), rx, tx: tx.clone() };
    let Ok(mut s) = slot.lock() else { return tx };
    if s.alive {
        s.next = Some(job);
        return tx;
    }
    s.alive = true;
    let started = std::thread::Builder::new().name(name.into()).spawn(move || {
        let mut job = Some(job);
        loop {
            let com = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok();
            while let Some(j) = job.take() {
                if !j.shared.quitting() {
                    f(&j.shared, j.rx, j.tx);
                }
                job = slot.lock().ok().and_then(|mut s| s.next.take());
            }
            // Keep ownership while COM can still be cleaning up the old job.
            if com {
                unsafe { CoUninitialize() };
            }
            let Ok(mut s) = slot.lock() else { return };
            job = s.next.take();
            if job.is_none() {
                s.alive = false;
                return;
            }
        }
    });
    if started.is_err() {
        s.alive = false;
    }
    tx
}
