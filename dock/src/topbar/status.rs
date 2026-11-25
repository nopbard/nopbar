//! What the right side of the top bar shows: clock text and status glyphs,
//! plus the background listener for network and volume changes.
//!
//! Everything here is event driven: network changes come from
//! NotifyNetworkConnectivityHintChange, volume from Core Audio callbacks,
//! battery from WM_POWERBROADCAST on the bar window. No polling.

use std::sync::{Mutex, OnceLock};

use crate::glyphs::Glyph;
use crate::idle::{IdleQueue, Jobs};

use windows::core::{Result, PCWSTR};
use windows_core::implement;
use windows::Win32::Foundation::{HANDLE, HWND, LPARAM, WPARAM};
use windows::Win32::Media::Audio::Endpoints::{
    IAudioEndpointVolume, IAudioEndpointVolumeCallback, IAudioEndpointVolumeCallback_Impl,
};
use windows::Win32::Media::Audio::{
    eConsole, eMultimedia, eRender, EDataFlow, ERole, IMMDeviceEnumerator, IMMNotificationClient,
    IMMNotificationClient_Impl, MMDeviceEnumerator, AUDIO_VOLUME_NOTIFICATION_DATA, DEVICE_STATE,
};
use windows::Win32::Foundation::PROPERTYKEY;
use windows::Win32::NetworkManagement::IpHelper::{
    GetBestInterface, GetBestInterfaceEx, GetIfEntry2, GetNetworkConnectivityHint, NotifyIpInterfaceChange,
    NotifyNetworkConnectivityHintChange, NotifyRouteChange2, MIB_IF_ROW2, MIB_IPFORWARD_ROW2, MIB_IPINTERFACE_ROW,
    MIB_NOTIFICATION_TYPE,
};
use windows::Win32::Networking::WinSock::{AF_INET6, AF_UNSPEC, NL_NETWORK_CONNECTIVITY_HINT, SOCKADDR, SOCKADDR_IN6};
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_ALL, COINIT_MULTITHREADED};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

/// Posted to the bar: wparam = Net code.
pub const WM_STATUS_NET: u32 = WM_APP + 30;
/// Posted to the bar: wparam = volume percent | muted << 8 (0xFFFF = no device).
pub const WM_STATUS_VOL: u32 = WM_APP + 31;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Net {
    Offline,
    /// Connected but no internet.
    Limited,
    Wifi,
    Wired,
    /// Internet, medium unknown (route lookup failed, e.g. odd VPNs).
    Online,
}

impl Net {
    pub fn code(self) -> usize {
        self as usize
    }
    pub fn from_code(c: usize) -> Self {
        match c {
            1 => Net::Limited,
            2 => Net::Wifi,
            3 => Net::Wired,
            4 => Net::Online,
            _ => Net::Offline,
        }
    }
}

// ---- glyphs (crate::glyphs) --------------------------------------------------

pub fn net_glyph(n: Net) -> Glyph {
    match n {
        Net::Offline => Glyph::WifiOff,
        Net::Limited => Glyph::NetLimited,
        Net::Wifi => Glyph::Wifi(4),
        Net::Wired => Glyph::Ethernet,
        Net::Online => Glyph::Globe,
    }
}

pub fn volume_glyph(percent: u8, muted: bool) -> Glyph {
    match (muted, percent) {
        (true, _) => Glyph::VolumeMute,
        (false, 0) => Glyph::Volume(0),
        (false, 1..=33) => Glyph::Volume(1),
        (false, 34..=66) => Glyph::Volume(2),
        _ => Glyph::Volume(3),
    }
}

/// What fills the drawn battery up to its level.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum BatteryFill {
    /// The bar's text color, solid (white on a dark bar); the number on it
    /// is dark.
    Normal,
    /// Red at 20 % or less (not charging); the number on it is white.
    Low,
    /// High contrast: the text color, the number on it in the window color.
    HighContrast,
}

/// Width of the battery body the level fills (DIPs): the drawn body's
/// whole width (topbar's BATTERY_BODY runs 0..this), so 100 % reaches its
/// end and 50 % its middle (design B2 review P3 #2).
pub const BATTERY_BODY: f32 = 24.0;

/// The filled width (DIPs of the body, from its left edge) and its color;
/// any charge left shows at least a sliver. Charging shows as the bolt in
/// the body, not as a color.
pub fn battery_look(percent: u8, charging: bool, high_contrast: bool) -> (f32, BatteryFill) {
    let p = percent.min(100);
    let level = if p == 0 { 0.0 } else { (BATTERY_BODY * p as f32 / 100.0).max(2.0) };
    let fill = if high_contrast {
        BatteryFill::HighContrast
    } else if percent <= 20 && !charging {
        BatteryFill::Low
    } else {
        BatteryFill::Normal
    };
    (level, fill)
}

// ---- clock ----------------------------------------------------------------

/// Clock text: the "topbar.clock" pattern filled with the weekday, date and
/// time as the user's Windows region writes them ("Sun November 1  3:42 PM",
/// "11월 1일 (일) 오후 3:42"); `seconds`: the time with its seconds
/// ("3:42:07 PM", topbar.clock_seconds).
pub fn clock_text(now: &windows::Win32::Foundation::SYSTEMTIME, seconds: bool) -> String {
    use crate::i18n::date;
    let time = if seconds { date::time_seconds(now) } else { date::time(now) };
    crate::i18n::tf("topbar.clock", &[("weekday", &date::weekday(now)), ("date", &date::month_day(now)), ("time", &time)])
}

/// ms until the next minute starts (+ a little, so we land after it).
pub fn ms_to_next_minute(second: u16, millis: u16) -> u32 {
    let into = second as u32 * 1000 + millis as u32;
    60_000 - into.min(59_999) + 30
}

/// ms until the next second starts (+ a little, so we land after it).
pub fn ms_to_next_second(millis: u16) -> u32 {
    1000 - (millis as u32).min(999) + 15
}

/// When the bar's clock next changes: on the next second while it shows
/// seconds and someone can see it (`fast`), else on the next minute (the
/// idle cost stays one timer a minute).
pub fn ms_to_next_tick(fast: bool, second: u16, millis: u16) -> u32 {
    if fast { ms_to_next_second(millis) } else { ms_to_next_minute(second, millis) }
}

/// The bar's clock ticks every second: seconds are shown, the bar is not
/// behind a fullscreen app, and the session is not locked, asleep or dark.
pub fn ticks_every_second(seconds: bool, covered: bool, idle: bool) -> bool {
    seconds && !covered && !idle
}

/// The text whose width a clock showing `text` keeps: every ASCII digit
/// made the font's widest (`widest`), so the clock never narrows as its
/// digits change (and the cells beside it, the island too, stay put).
/// Only a change of the other letters (AM / PM, a longer weekday) moves
/// it.
pub fn steady_template(text: &str, widest: char) -> String {
    text.chars().map(|c| if c.is_ascii_digit() { widest } else { c }).collect()
}

/// The clock texts the bar's clock keeps its width for at `now` (its
/// date): the time swapped for two-digit hours before and after noon
/// (crate::popup::clock_samples), so 9:59 -> 10:00 and 12:59 -> 1:00 move
/// nothing.
pub fn clock_rooms(now: &windows::Win32::Foundation::SYSTEMTIME, seconds: bool) -> Vec<String> {
    crate::popup::clock_samples(now).iter().map(|st| clock_text(st, seconds)).collect()
}

/// Pure: of `texts` with their digits made `widest` (steady_template), the
/// widest by `width`: the room a clock showing any of them is drawn in.
pub fn steady_room(texts: &[String], widest: char, width: impl Fn(&str) -> f32) -> String {
    let mut best = (String::new(), f32::MIN);
    for t in texts {
        let room = steady_template(t, widest);
        let w = width(&room);
        if w > best.1 {
            best = (room, w);
        }
    }
    best.0
}

/// A width that only grows while its `key` (the time format, the font, the
/// scale) stays: the clock never narrows in a session, even when a date or
/// weekday comes out shorter. A new key starts over.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SteadyWidth {
    key: String,
    w: f32,
}

impl SteadyWidth {
    /// The least width to draw at under `key` (0 for a new one).
    pub fn floor(&self, key: &str) -> f32 {
        if self.key == key { self.w } else { 0.0 }
    }
    /// Keeps `w` drawn under `key`; returns the width kept.
    pub fn fit(&mut self, key: &str, w: f32) -> f32 {
        if self.key != key {
            self.key = key.to_string();
            self.w = w;
        } else {
            self.w = self.w.max(w);
        }
        self.w
    }
}

/// The bar clock's name for screen readers: the long date and the time,
/// with its seconds when the bar shows them.
pub fn clock_name(now: &windows::Win32::Foundation::SYSTEMTIME, seconds: bool) -> String {
    if seconds { crate::i18n::date::full_seconds(now) } else { crate::i18n::date::full(now) }
}

/// The widest of the ten digits, given their widths in order (the first
/// one wins a tie; '0' when none is known).
pub fn widest_digit(widths: &[f32]) -> char {
    let mut best = (0usize, f32::MIN);
    for (i, &w) in widths.iter().enumerate().take(10) {
        if w > best.1 {
            best = (i, w);
        }
    }
    char::from(b'0' + best.0 as u8)
}

// ---- background listener ----------------------------------------------------
//
// The listeners (network notifications, the audio endpoint callbacks) are
// registered once and live in the process; they fire on Windows' own
// threads and only queue a command. Commands run on a worker that exists
// only while there are some (crate::idle). The audio objects are COM MTA
// objects kept in AUDIO, so whichever worker runs next can use them; the
// MTA itself is held up for the process (CoIncrementMTAUsage), not by a
// thread that waits forever.

enum Cmd {
    /// Register the listeners and report the state (once).
    Init,
    /// Default output device changed: bind to the new one.
    Rebind,
    /// Mouse wheel over the volume glyph.
    Step(bool),
    /// Connectivity, an interface or a route changed: work out the icon.
    Network,
    /// Volume slider in the top bar's panel (0..1).
    Set(f32),
    /// Flip mute from whatever it is now.
    ToggleMute,
    /// After sleep, unlock or a reconnect: audio and network read again,
    /// the audio service object made again if it never came up.
    Resync,
    /// A planned retry of the audio enumerator came due.
    Retry,
}

/// How long to wait before trying to create the audio device enumerator
/// again after `failures` failed tries (audit A09: an audio service that
/// was down at start used to leave the volume blank until NopBar
/// restarted): 5 s, 10 s, 30 s, then once a minute until it works. The
/// retries are service-thread timers (no thread waits for them); a resync
/// or device change tries at once.
fn enumerator_retry_delay(failures: u32) -> std::time::Duration {
    std::time::Duration::from_secs(match failures {
        0 => 5,
        1 => 10,
        2 => 30,
        _ => 60,
    })
}

/// After sleep, unlock or a reconnect (audit A09).
pub fn resync() {
    send(Cmd::Resync);
}

/// The worker ends after this long without a command.
const STATUS_IDLE: std::time::Duration = std::time::Duration::from_secs(30);

static QUEUE: OnceLock<IdleQueue<Cmd>> = OnceLock::new();
static BAR: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(0);

/// The default output device and the listeners on it.
struct Audio {
    enumerator: Option<IMMDeviceEnumerator>,
    callback: IAudioEndpointVolumeCallback,
    client: IMMNotificationClient,
    bound: Option<IAudioEndpointVolume>,
    /// Enumerator tries that failed in a row.
    failures: u32,
    /// A retry timer is set.
    retry_planned: bool,
}

// MTA objects (the MTA is kept alive for the process): any MTA thread may
// use them.
unsafe impl Send for Audio {}

static AUDIO: Mutex<Option<Audio>> = Mutex::new(None);

fn post(msg: u32, wp: usize) {
    let h = BAR.load(std::sync::atomic::Ordering::SeqCst);
    if h != 0 {
        unsafe {
            let _ = PostMessageW(Some(HWND(h as *mut _)), msg, WPARAM(wp), LPARAM(0));
        }
    }
}

fn send(cmd: Cmd) {
    if let Some(q) = QUEUE.get() {
        let _ = q.send(cmd);
    }
}

/// Set the output volume (0..1).
pub fn set_volume(v: f32) {
    send(Cmd::Set(v));
}

pub fn toggle_mute() {
    send(Cmd::ToggleMute);
}

/// Volume up/down one step (wheel over the glyph).
pub fn volume_step(up: bool) {
    send(Cmd::Step(up));
}

/// Starts listening (once per process); updates go to `bar`. Calling it
/// again (bar re-created) just redirects the updates and re-sends state.
pub fn start(bar: HWND) {
    BAR.store(bar.0 as isize, std::sync::atomic::Ordering::SeqCst);
    if QUEUE.get().is_some() {
        send(Cmd::Rebind);
        send(Cmd::Network);
        return;
    }
    let q = QUEUE.get_or_init(|| IdleQueue::new("status", STATUS_IDLE, 0, |jobs: &mut Jobs<Cmd>| worker(jobs)));
    let _ = q.send(Cmd::Init);
}

fn worker(jobs: &mut Jobs<Cmd>) {
    // Uninitialize only what we initialized.
    let com = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok();
    while let Some(cmd) = jobs.next() {
        unsafe { run(cmd, jobs) };
    }
    if com {
        unsafe { CoUninitialize() };
    }
}

fn audio() -> std::sync::MutexGuard<'static, Option<Audio>> {
    AUDIO.lock().unwrap_or_else(|e| e.into_inner())
}

/// The audio device enumerator, registered for default-device changes.
unsafe fn make_enumerator(client: &IMMNotificationClient) -> Option<IMMDeviceEnumerator> {
    let e: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).ok()?;
    let _ = e.RegisterEndpointNotificationCallback(client);
    Some(e)
}

/// Asks for another enumerator try after `failures` failed ones (a timer
/// on the service thread; no thread waits for it).
fn plan_retry(a: &mut Audio) {
    if std::mem::replace(&mut a.retry_planned, true) {
        return;
    }
    let delay = enumerator_retry_delay(a.failures);
    if crate::service::after("audio retry", delay, || send(Cmd::Retry)).is_none() {
        a.retry_planned = false;
    }
}

/// Makes the enumerator if there is none yet (a failed try plans the
/// next one); binds the default device once it is there.
unsafe fn ensure_enumerator(a: &mut Audio) {
    if a.enumerator.is_some() {
        return;
    }
    a.enumerator = make_enumerator(&a.client);
    if a.enumerator.is_some() {
        crate::log!("status: audio devices available again");
        a.failures = 0;
        rebind(a);
    } else {
        a.failures = a.failures.saturating_add(1);
        plan_retry(a);
    }
}

unsafe fn rebind(a: &mut Audio) {
    if let Some(old) = a.bound.take() {
        let _ = old.UnregisterControlChangeNotify(&a.callback);
    }
    a.bound = a.enumerator.as_ref().and_then(|e| bind(e, &a.callback));
}

unsafe fn run(cmd: Cmd, jobs: &mut Jobs<Cmd>) {
    match cmd {
        Cmd::Init => {
            // These fire on thread-pool threads and only queue a command.
            // Connectivity level, interfaces coming and going, and route
            // changes (the default route moving between Wi-Fi and wired).
            let mut h1 = HANDLE::default();
            let mut h2 = HANDLE::default();
            let mut h3 = HANDLE::default();
            let _ = NotifyNetworkConnectivityHintChange(Some(hint_changed), None, false, &mut h1);
            let _ = NotifyIpInterfaceChange(AF_UNSPEC, Some(interface_changed), None, false, &mut h2);
            let _ = NotifyRouteChange2(AF_UNSPEC, Some(route_changed), std::ptr::null(), false, &mut h3);
            post(WM_STATUS_NET, read_network().code());
            let client: IMMNotificationClient = DeviceWatch.into();
            let enumerator = make_enumerator(&client);
            let callback: IAudioEndpointVolumeCallback = VolumeWatch.into();
            let bound = enumerator.as_ref().and_then(|e| bind(e, &callback));
            let mut a = Audio { enumerator, callback, client, bound, failures: 0, retry_planned: false };
            if a.enumerator.is_none() {
                crate::log!("status: audio devices unavailable; trying again later");
                plan_retry(&mut a);
            }
            *audio() = Some(a);
        }
        Cmd::Retry => {
            if let Some(a) = audio().as_mut() {
                a.retry_planned = false;
                ensure_enumerator(a);
            }
        }
        Cmd::Rebind => {
            if let Some(a) = audio().as_mut() {
                // A device change: try the enumerator at once if it is missing.
                if a.enumerator.is_none() {
                    ensure_enumerator(a);
                } else {
                    rebind(a);
                }
            }
        }
        Cmd::Resync => {
            if let Some(a) = audio().as_mut() {
                if a.enumerator.is_none() {
                    ensure_enumerator(a);
                } else {
                    rebind(a);
                }
            }
            post(WM_STATUS_NET, read_network().code());
        }
        Cmd::Step(up) => {
            if let Some(v) = bound() {
                let _ = if up { v.VolumeStepUp(std::ptr::null()) } else { v.VolumeStepDown(std::ptr::null()) };
            }
        }
        Cmd::Set(level) => {
            if let Some(v) = bound() {
                let _ = v.SetMasterVolumeLevelScalar(level.clamp(0.0, 1.0), std::ptr::null());
                let _ = v.SetMute(false, std::ptr::null());
            }
        }
        Cmd::ToggleMute => {
            if let Some(v) = bound() {
                let now = v.GetMute().map(|b| b.as_bool()).unwrap_or(false);
                let _ = v.SetMute(!now, std::ptr::null());
            }
        }
        Cmd::Network => {
            // Bursts (several routes at once) collapse into one read.
            std::thread::sleep(std::time::Duration::from_millis(150));
            let mut again = Vec::new();
            while let Some(c) = jobs.try_next() {
                if !matches!(c, Cmd::Network) {
                    again.push(c);
                }
            }
            post(WM_STATUS_NET, read_network().code());
            for c in again {
                run(c, jobs);
            }
        }
    }
}

/// The bound endpoint (called without holding AUDIO: its callbacks may run
/// while we call into it).
fn bound() -> Option<IAudioEndpointVolume> {
    audio().as_ref().and_then(|a| a.bound.clone())
}

/// Listen to the default output device and report its level now.
unsafe fn bind(e: &IMMDeviceEnumerator, cb: &IAudioEndpointVolumeCallback) -> Option<IAudioEndpointVolume> {
    let Ok(dev) = e.GetDefaultAudioEndpoint(eRender, eConsole) else {
        post(WM_STATUS_VOL, 0xFFFF);
        return None;
    };
    let vol: IAudioEndpointVolume = dev.Activate(CLSCTX_ALL, None).ok()?;
    let _ = vol.RegisterControlChangeNotify(cb);
    let level = vol.GetMasterVolumeLevelScalar().unwrap_or(0.0);
    let muted = vol.GetMute().map(|b| b.as_bool()).unwrap_or(false);
    post(WM_STATUS_VOL, pack_volume(level, muted));
    Some(vol)
}

pub fn pack_volume(level: f32, muted: bool) -> usize {
    ((level.clamp(0.0, 1.0) * 100.0).round() as usize) | ((muted as usize) << 8)
}

pub fn unpack_volume(wp: usize) -> Option<(u8, bool)> {
    if wp == 0xFFFF {
        None
    } else {
        Some(((wp & 0xFF).min(100) as u8, wp & 0x100 != 0))
    }
}

unsafe extern "system" fn hint_changed(_: *const core::ffi::c_void, _: NL_NETWORK_CONNECTIVITY_HINT) {
    send(Cmd::Network);
}

unsafe extern "system" fn interface_changed(_: *const core::ffi::c_void, _: *const MIB_IPINTERFACE_ROW, _: MIB_NOTIFICATION_TYPE) {
    send(Cmd::Network);
}

unsafe extern "system" fn route_changed(_: *const core::ffi::c_void, _: *const MIB_IPFORWARD_ROW2, _: MIB_NOTIFICATION_TYPE) {
    send(Cmd::Network);
}

/// What the network icon should show now.
unsafe fn read_network() -> Net {
    let mut hint = NL_NETWORK_CONNECTIVITY_HINT::default();
    let _ = GetNetworkConnectivityHint(&mut hint);
    // 1 none, 2 local only, 3 internet, 4 constrained internet.
    match hint.ConnectivityLevel.0 {
        3 | 4 => match route_medium() {
            Some(71) => Net::Wifi, // IF_TYPE_IEEE80211
            Some(_) => Net::Wired,
            None => Net::Online,
        },
        2 => Net::Limited,
        _ => Net::Offline,
    }
}

/// Interface type of the route to the internet: IPv4 first, then IPv6
/// (IPv6-only networks). None if neither has a route.
unsafe fn route_medium() -> Option<u32> {
    let mut index = 0u32;
    // 8.8.8.8 (same bytes in either order).
    let mut found = GetBestInterface(0x0808_0808, &mut index) == 0;
    if !found {
        let mut a = SOCKADDR_IN6 { sin6_family: AF_INET6, ..Default::default() };
        // 2001:4860:4860::8888
        a.sin6_addr.u.Byte = [0x20, 0x01, 0x48, 0x60, 0x48, 0x60, 0, 0, 0, 0, 0, 0, 0, 0, 0x88, 0x88];
        found = GetBestInterfaceEx(&a as *const SOCKADDR_IN6 as *const SOCKADDR, &mut index) == 0;
    }
    if !found {
        return None;
    }
    let mut row = MIB_IF_ROW2 { InterfaceIndex: index, ..Default::default() };
    GetIfEntry2(&mut row).is_ok().then_some(row.Type)
}

/// The bar is gone: stop posting to its old window.
pub fn detach() {
    BAR.store(0, std::sync::atomic::Ordering::SeqCst);
}

#[implement(IAudioEndpointVolumeCallback)]
struct VolumeWatch;

impl IAudioEndpointVolumeCallback_Impl for VolumeWatch_Impl {
    fn OnNotify(&self, data: *mut AUDIO_VOLUME_NOTIFICATION_DATA) -> Result<()> {
        if let Some(d) = unsafe { data.as_ref() } {
            post(WM_STATUS_VOL, pack_volume(d.fMasterVolume, d.bMuted.as_bool()));
        }
        Ok(())
    }
}

#[implement(IMMNotificationClient)]
struct DeviceWatch;

impl IMMNotificationClient_Impl for DeviceWatch_Impl {
    fn OnDeviceStateChanged(&self, _: &PCWSTR, _: DEVICE_STATE) -> Result<()> {
        Ok(())
    }
    fn OnDeviceAdded(&self, _: &PCWSTR) -> Result<()> {
        Ok(())
    }
    fn OnDeviceRemoved(&self, _: &PCWSTR) -> Result<()> {
        Ok(())
    }
    fn OnDefaultDeviceChanged(&self, flow: EDataFlow, role: ERole, _: &PCWSTR) -> Result<()> {
        // Called on an audio thread; rebind on ours.
        if flow == eRender && (role == eConsole || role == eMultimedia) {
            send(Cmd::Rebind);
        }
        Ok(())
    }
    fn OnPropertyValueChanged(&self, _: &PCWSTR, _: &PROPERTYKEY) -> Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_enumerator_is_retried_on_a_bounded_schedule() {
        let secs: Vec<u64> = (0..6).map(|n| enumerator_retry_delay(n).as_secs()).collect();
        assert_eq!(secs, [5, 10, 30, 60, 60, 60]);
        assert_eq!(enumerator_retry_delay(u32::MAX).as_secs(), 60);
    }

    #[test]
    fn clock_follows_the_pattern() {
        use crate::i18n::date;
        let st = windows::Win32::Foundation::SYSTEMTIME { wYear: 2026, wMonth: 11, wDay: 1, wHour: 15, wMinute: 42, ..Default::default() };
        // Date parts are formatted in the UI language (with the user's own
        // region formats when the region speaks it).
        let parts = || (date::weekday(&st), date::month_day(&st), date::time(&st));
        let l = crate::i18n::test_lock("ko");
        let (wd, d, tm) = parts();
        assert!(!wd.is_empty() && !d.is_empty() && !tm.is_empty(), "{wd:?} {d:?} {tm:?}");
        assert!(!date::year_month(&st).is_empty());
        assert_eq!(clock_text(&st, false), format!("{d} ({wd}) {tm}"));
        drop(l);
        let _l = crate::i18n::test_lock("en");
        let (wd, d, tm) = parts();
        assert_eq!(clock_text(&st, false), format!("{wd} {d}  {tm}"));
        // English UI never shows Hangul dates, whatever the region.
        assert!(!clock_text(&st, false).chars().any(|c| (0xAC00..=0xD7A3).contains(&(c as u32))), "{}", clock_text(&st, false));
    }

    #[test]
    fn seconds_are_added_in_the_regions_own_format() {
        use crate::i18n::date;
        let st = windows::Win32::Foundation::SYSTEMTIME { wYear: 2026, wMonth: 11, wDay: 1, wHour: 15, wMinute: 42, wSecond: 7, ..Default::default() };
        for lang in ["en", "ko", "de"] {
            let _l = crate::i18n::test_lock(lang);
            let (short, long) = (date::time(&st), date::time_seconds(&st));
            // The same time with ":07" more, wherever the region puts it
            // (12 h keeps its AM / PM, 24 h stays 24 h).
            assert!(long.contains("07") && !short.contains("07"), "{lang}: {short:?} {long:?}");
            assert!(long.contains("42") && short.contains("42"), "{lang}: {short:?} {long:?}");
            assert!(long.chars().count() > short.chars().count(), "{lang}: {short:?} {long:?}");
            let without: String = short.chars().filter(|c| !c.is_ascii_digit()).collect();
            for word in without.split(|c: char| c.is_ascii_punctuation() || c.is_whitespace()).filter(|w| !w.is_empty()) {
                assert!(long.contains(word), "{lang}: {word:?} kept in {long:?}");
            }
            // On the bar: the pattern with the long time; off: as before.
            assert!(clock_text(&st, true).contains(&long), "{lang}");
            assert!(clock_text(&st, false).contains(&short) && !clock_text(&st, false).contains(&long), "{lang}");
        }
    }

    #[test]
    fn seconds_tick_on_the_second_and_minutes_on_the_minute() {
        assert_eq!(ms_to_next_second(0), 1015);
        assert_eq!(ms_to_next_second(999), 16);
        assert_eq!(ms_to_next_second(2000), 16, "never negative");
        assert_eq!(ms_to_next_tick(true, 30, 250), 765, "the next second");
        assert_eq!(ms_to_next_tick(false, 30, 250), 29_780, "the next minute");
        assert_eq!(ms_to_next_tick(false, 59, 999), ms_to_next_minute(59, 999));
        // Fast only with seconds on, the bar uncovered and someone there.
        assert!(ticks_every_second(true, false, false));
        assert!(!ticks_every_second(false, false, false), "seconds off: a minute timer");
        assert!(!ticks_every_second(true, true, false), "behind a fullscreen app");
        assert!(!ticks_every_second(true, false, true), "locked, asleep or the display off");
    }

    #[test]
    fn the_clock_keeps_its_width_as_digits_change() {
        assert_eq!(steady_template("Sun Nov 1  9:41:07 PM", '0'), "Sun Nov 0  0:00:00 PM");
        assert_eq!(steady_template("오후 3:42:07", '8'), "오후 8:88:88");
        // Every second of a minute gives the same template.
        let t = |s: u32| steady_template(&format!("20:41:{s:02}"), '0');
        assert!((0..60).all(|s| t(s) == t(0)));
        assert_eq!(widest_digit(&[5.0, 4.0, 5.0, 5.0, 5.5, 5.0, 5.0, 5.0, 5.0, 5.0]), '4');
        assert_eq!(widest_digit(&[5.0; 10]), '0', "tabular figures: any, the first");
        assert_eq!(widest_digit(&[]), '0');
    }

    #[test]
    fn the_hour_gaining_a_digit_moves_nothing() {
        use windows::Win32::Foundation::SYSTEMTIME;
        let at = |h: u16, m: u16, s: u16| SYSTEMTIME { wYear: 2026, wMonth: 11, wDay: 5, wHour: h, wMinute: m, wSecond: s, ..Default::default() };
        // Width by characters: every digit as wide as '0', the rest 1.
        let chars = |t: &str| t.chars().count() as f32;
        let _l = crate::i18n::test_lock("en");
        for (seconds, before, after) in [(true, at(21, 59, 59), at(22, 0, 0)), (false, at(12, 59, 0), at(13, 0, 0)), (true, at(9, 59, 59), at(10, 0, 0))] {
            let room_of = |st: &SYSTEMTIME| {
                let mut texts = clock_rooms(st, seconds);
                texts.push(clock_text(st, seconds));
                steady_room(&texts, '0', chars)
            };
            let (a, b) = (room_of(&before), room_of(&after));
            // 9:59:59 PM -> 10:00:00 PM, 12:59 PM -> 1:00 PM: the same room,
            // as wide as the widest of both.
            assert_eq!(a, b, "{seconds}");
            for st in [&before, &after] {
                assert!(chars(&a) >= chars(&clock_text(st, seconds)), "{a:?} {:?}", clock_text(st, seconds));
            }
            assert!(a.contains("00:00"), "{a:?}: a two-digit hour");
        }
        // The widest wins, whichever it is.
        let texts = ["9:41 AM".to_string(), "10:41 AM".to_string(), "10:41 PM".to_string()];
        assert_eq!(steady_room(&texts, '0', chars), "00:00 AM", "the first of the widest");
    }

    #[test]
    fn the_clock_never_narrows_within_a_format() {
        let mut w = SteadyWidth::default();
        assert_eq!(w.floor("en|h:mm:ss tt|Segoe UI|1300"), 0.0);
        assert_eq!(w.fit("en|h:mm:ss tt|Segoe UI|1300", 120.0), 120.0);
        assert_eq!(w.fit("en|h:mm:ss tt|Segoe UI|1300", 110.0), 120.0, "a shorter weekday: kept");
        assert_eq!(w.floor("en|h:mm:ss tt|Segoe UI|1300"), 120.0);
        assert_eq!(w.fit("en|h:mm:ss tt|Segoe UI|1300", 126.0), 126.0);
        // Another format, font or scale: measured afresh.
        assert_eq!(w.floor("en|HH:mm|Segoe UI|1300"), 0.0);
        assert_eq!(w.fit("en|HH:mm|Segoe UI|1300", 90.0), 90.0);
        assert_eq!(w.fit("en|HH:mm|Segoe UI|1950", 140.0), 140.0);
    }

    #[test]
    fn the_clocks_name_says_what_it_shows() {
        let st = windows::Win32::Foundation::SYSTEMTIME { wYear: 2026, wMonth: 11, wDay: 5, wHour: 21, wMinute: 4, wSecond: 9, ..Default::default() };
        let _l = crate::i18n::test_lock("en");
        let (on, off) = (clock_name(&st, true), clock_name(&st, false));
        assert!(on.contains("9:04:09") && on.contains("2026"), "{on}");
        assert!(off.contains("9:04") && !off.contains("9:04:09"), "{off}");
        assert!(clock_text(&st, true).contains("9:04:09"));
    }

    #[test]
    fn next_minute() {
        assert_eq!(ms_to_next_minute(0, 0), 60_030);
        assert_eq!(ms_to_next_minute(59, 999), 31);
        assert_eq!(ms_to_next_minute(30, 0), 30_030);
    }

    #[test]
    fn volume_packing() {
        assert_eq!(unpack_volume(pack_volume(0.42, false)), Some((42, false)));
        assert_eq!(unpack_volume(pack_volume(1.0, true)), Some((100, true)));
        assert_eq!(unpack_volume(0xFFFF), None);
    }

    #[test]
    fn battery_looks() {
        use BatteryFill::*;
        assert_eq!(battery_look(0, false, false), (0.0, Low));
        assert_eq!(battery_look(20, false, false).1, Low);
        assert_eq!(battery_look(20, true, false).1, Normal, "charging is the bolt, not red");
        assert_eq!(battery_look(21, false, false).1, Normal);
        assert_eq!(battery_look(100, true, false), (BATTERY_BODY, Normal));
        assert_eq!(battery_look(50, false, false).0, BATTERY_BODY / 2.0);
        assert_eq!(battery_look(1, false, false).0, 2.0, "a sliver stays visible");
        assert_eq!(battery_look(50, false, true).1, HighContrast);
        assert_eq!(battery_look(255, false, false).0, BATTERY_BODY);
    }

    #[test]
    fn glyph_steps() {
        assert_eq!(volume_glyph(0, false), Glyph::Volume(0));
        assert_eq!(volume_glyph(80, true), Glyph::VolumeMute);
        assert_eq!(volume_glyph(20, false), Glyph::Volume(1));
        assert_eq!(volume_glyph(50, false), Glyph::Volume(2));
        assert_eq!(volume_glyph(100, false), Glyph::Volume(3));
        assert_eq!(net_glyph(Net::Wifi), Glyph::Wifi(4));
        assert_ne!(net_glyph(Net::Offline), net_glyph(Net::Limited));
        for c in 0..5 {
            assert_eq!(Net::from_code(Net::from_code(c).code()), Net::from_code(c));
        }
    }
}
