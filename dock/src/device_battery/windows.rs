//! Read-only PnP properties. No Bluetooth/GATT connection or pairing API.
//!
//! The watcher thread never stops for good: a watcher that stops or
//! aborts, a PC back from sleep (a loop pass far too late), the Bluetooth
//! radio turned back on, or a watcher that cannot be made (retried, a
//! little slower each time) all make it again, each said once in dock.log.
//! Every device read has a time limit (a hung Bluetooth stack never holds
//! the thread), and every 30 minutes every battery is read again directly,
//! whatever the watcher said.
use super::{
    model::{Kind, Update},
    source::{Event, Source},
};
use std::{
    collections::BTreeMap,
    sync::mpsc::{self, Receiver, Sender},
    time::{Duration, Instant},
};
use windows::{
    core::{IInspectable, Interface, Result, HSTRING},
    Devices::Enumeration::{
        DeviceInformation, DeviceInformationKind, DeviceInformationUpdate, DeviceWatcher,
    },
    Foundation::{IPropertyValue, TypedEventHandler},
};
use windows_collections::{IIterable, IMapView};

pub const BATTERY: &str = "{104EA319-6EE2-4701-BD47-8DDBF425BBE5} 2";
/// Bluetooth's device nodes.
const FILTER: &str = "System.Devices.DeviceInstanceId:~<\"BTH\"";
const CONNECTED: &str = "System.Devices.Connected";
const CONTAINER: &str = "System.Devices.ContainerId";
const STATUS: &str = "{4340A6C5-93FA-4706-972C-7B648008A5A7} 2";
const CHARGING: &str = "System.Devices.ChargingState";
const CATEGORY: &str = "System.Devices.ClassGuid";
fn restart_needed(status: Option<windows::Devices::Enumeration::DeviceWatcherStatus>) -> bool {
    use windows::Devices::Enumeration::DeviceWatcherStatus as S;
    status.is_none_or(|s| s == S::Stopped || s == S::Aborted)
}
/// How long one device read may take before it is given up.
const READ_TIMEOUT: Duration = Duration::from_secs(5);
/// Every battery read again directly this often.
const REREAD: Duration = Duration::from_secs(30 * 60);
/// A loop pass (one second at most) this late: the PC slept or the thread
/// was held; the watcher is made again.
const PAUSED: Duration = Duration::from_secs(60);
/// Why the watcher is made again (said in dock.log), or None: it goes on.
/// Pure: `status` the watcher's, `gap` since the last loop pass (wall
/// clock), `radio_on` the Bluetooth radio just came back on.
fn restart_reason(status: Option<windows::Devices::Enumeration::DeviceWatcherStatus>, gap: Duration, radio_on: bool) -> Option<String> {
    if restart_needed(status) {
        use windows::Devices::Enumeration::DeviceWatcherStatus as S;
        Some(format!("watcher {}", match status {
            None => "gone",
            Some(S::Aborted) => "aborted",
            Some(_) => "stopped",
        }))
    } else if gap >= PAUSED {
        Some(format!("back after {} s (sleep)", gap.as_secs()))
    } else if radio_on {
        Some("bluetooth radio on".into())
    } else {
        None
    }
}
/// Pure: the wait before making a watcher again after `failures` failed
/// tries in a row: 1 s, doubling, a minute at most.
fn backoff(failures: u32) -> Duration {
    Duration::from_secs(1u64 << failures.min(6)).min(Duration::from_secs(60))
}
/// The result of `op` within READ_TIMEOUT; given up (cancelled) after.
fn wait<T: windows::core::RuntimeType + 'static>(op: windows_future::IAsyncOperation<T>, what: &str) -> Option<T> {
    use windows_future::AsyncStatus;
    let deadline = Instant::now() + READ_TIMEOUT;
    loop {
        match op.Status().ok()? {
            AsyncStatus::Completed => return op.GetResults().ok(),
            AsyncStatus::Started if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            AsyncStatus::Started => {
                let _ = op.Cancel();
                crate::log!("device battery: {what} took over {} s, given up", READ_TIMEOUT.as_secs());
                return None;
            }
            _ => return None,
        }
    }
}
fn properties() -> IIterable<HSTRING> {
    vec![
        BATTERY,
        CONTAINER,
        CONNECTED,
        STATUS,
        CHARGING,
        CATEGORY,
        "System.ItemNameDisplay",
    ]
    .into_iter()
    .map(HSTRING::from)
    .collect::<Vec<_>>()
    .into()
}
fn property(map: &IMapView<HSTRING, IInspectable>, key: &str) -> Option<IPropertyValue> {
    map.Lookup(&HSTRING::from(key)).ok()?.cast().ok()
}
fn percent(p: &IPropertyValue) -> Option<u8> {
    let n = p
        .GetUInt8()
        .map(u32::from)
        .or_else(|_| p.GetUInt16().map(u32::from))
        .or_else(|_| p.GetUInt32())
        .ok()?;
    (n <= 100).then_some(n as u8)
}
fn decode(id: String, name: Option<String>, map: &IMapView<HSTRING, IInspectable>) -> Update {
    let name = name.or_else(|| {
        property(map, "System.ItemNameDisplay")
            .and_then(|v| v.GetString().ok())
            .map(|s| s.to_string())
    });
    let class = property(map, CATEGORY)
        .and_then(|v| v.GetGuid().ok())
        .map(|g| format!("{g:?}").to_lowercase())
        .unwrap_or_default();
    let kind = match class.as_str() {
        "4d36e96f-e325-11ce-bfc1-08002be10318" => Some(Kind::Mouse),
        "4d36e96b-e325-11ce-bfc1-08002be10318" => Some(Kind::Keyboard),
        "4d36e96c-e325-11ce-bfc1-08002be10318" => Some(Kind::Headphones),
        _ => name.as_deref().map(classify),
    };
    let id = property(map, CONTAINER)
        .and_then(|p| {
            p.GetGuid()
                .ok()
                .map(|g| format!("{g:?}"))
                .or_else(|| p.GetString().ok().map(|s| s.to_string()))
        })
        .and_then(|s| uuid::Uuid::parse_str(s.trim_matches(['{', '}'])).ok())
        .filter(|g| !g.is_nil())
        .map(|g| format!("container:{{{g}}}"))
        .unwrap_or(id);
    Update {
        id,
        name,
        kind,
        percent: map
            .HasKey(&HSTRING::from(BATTERY))
            .ok()
            .filter(|v| *v)
            .map(|_| property(map, BATTERY).and_then(|p| percent(&p))),
        connected: property(map, CONNECTED)
            .and_then(|v| v.GetBoolean().ok())
            .or_else(|| {
                property(map, STATUS)
                    .and_then(|v| v.GetUInt32().ok())
                    .map(|s| s & 0x02000000 == 0 && s & 8 != 0)
            }),
        // Optional driver-reported state; never infer from a rising percentage.
        charging: property(map, CHARGING).and_then(|v| v.GetBoolean().ok()),
    }
}
/// A device's kind from its name, when its class does not tell (a
/// Bluetooth LE or HID node that reports no class). Check plain words
/// first, then product names.
fn classify(name: &str) -> Kind {
    let n = name.to_lowercase();
    let any = |words: &[&str]| words.iter().any(|s| n.contains(s));
    if any(&["keyboard", "키보드"]) {
        Kind::Keyboard
    } else if any(&["mouse", "마우스", "trackball", "트랙볼"]) {
        Kind::Mouse
    } else if any(&["head", "ear", "buds", "airpods", "pods", "이어폰", "헤드", "wh-", "wf-"]) {
        Kind::Headphones
    } else if any(&["controller", "gamepad", "xbox", "dualsense", "dualshock", "8bitdo", "패드"]) {
        Kind::Gamepad
    } else if any(&["mx keys", " keys", "keychron", "k380", "k780", "k400", "k375", "k585", "k650", "k850", "k950", "magic keyboard", "mx mechanical", "craft", "pop keys"]) {
        Kind::Keyboard
    } else if any(&["mx master", "mx anywhere", "mx ergo", "mx vertical", "m720", "m590", "m650", "m350", "pebble", "lift", "pop mouse", "g pro", "g305", "g502", "g903", "viper", "deathadder", "basilisk", "orochi", "magic mouse", "magic trackpad"]) {
        Kind::Mouse
    } else {
        Kind::Other
    }
}
/// Battery-bearing physical nodes only. One emitted row per container.
#[derive(Default)]
struct Nodes(BTreeMap<String, Update>);
impl Nodes {
    fn identity(&self, raw: &str, update: &Update) -> String {
        if update.id.starts_with("container:") {
            update.id.clone()
        } else {
            self.0
                .get(raw)
                .map_or_else(|| update.id.clone(), |n| n.id.clone())
        }
    }
    fn update(&mut self, raw: String, mut u: Update) -> Option<Update> {
        if !self.0.contains_key(&raw) && !matches!(u.percent, Some(Some(0..=100))) {
            return None;
        }
        u.id = self.identity(&raw, &u);
        let id = u.id.clone();
        let sample = u.percent;
        let n = self.0.entry(raw).or_default();
        n.id = id.clone();
        if u.name.is_some() {
            n.name = u.name;
        }
        if u.kind.is_some() {
            n.kind = u.kind;
        }
        if u.percent.is_some() {
            n.percent = u.percent;
        }
        if u.charging.is_some() {
            n.charging = u.charging;
        }
        n.connected = Some(u.connected.unwrap_or(false));
        self.aggregate(&id).map(|mut combined| {
            combined.percent = sample;
            combined
        })
    }
    fn aggregate(&self, id: &str) -> Option<Update> {
        let mut candidates: Vec<_> = self.0.values().filter(|n| n.id == id).collect();
        candidates.sort_by_key(|n| {
            (
                n.connected != Some(true),
                n.percent.flatten().is_none(),
                n.percent.flatten(),
            )
        });
        let n = candidates.first()?;
        Some(Update {
            id: id.into(),
            name: n.name.clone(),
            kind: n.kind,
            percent: None,
            connected: Some(
                candidates
                    .iter()
                    .any(|n| n.connected == Some(true) && n.percent.flatten().is_some()),
            ),
            charging: n.charging,
        })
    }
    fn remove(&mut self, raw: &str) -> Option<Event> {
        let n = self.0.remove(raw)?;
        Some(
            self.aggregate(&n.id)
                .map_or(Event::Removed(n.id), Event::Update),
        )
    }
    fn raw_ids(&self, ids: &[String]) -> Vec<String> {
        self.0
            .iter()
            .filter(|(_, n)| ids.contains(&n.id) && n.percent.flatten().is_some())
            .map(|(raw, _)| raw.clone())
            .collect()
    }
}
enum Command {
    Refresh(Vec<String>),
    Node(u64, String, Update),
    Removed(u64, String),
    Enumerated(u64),
    Stopped(u64),
    /// The Bluetooth radio is on (true) or off.
    Radio(bool),
    Stop,
}
pub struct WindowsSource {
    rx: Receiver<Event>,
    tx: Sender<Command>,
}
impl WindowsSource {
    pub fn start() -> Self {
        let (events, rx) = mpsc::channel();
        let (tx, commands) = mpsc::channel();
        let callbacks = tx.clone();
        std::thread::spawn(move || {
            use windows::Win32::System::WinRT::{
                RoInitialize, RoUninitialize, RO_INIT_MULTITHREADED,
            };
            if unsafe { RoInitialize(RO_INIT_MULTITHREADED) }.is_err() {
                return;
            }
            run(events, commands, callbacks);
            unsafe { RoUninitialize() };
        });
        Self { rx, tx }
    }
}
impl Source for WindowsSource {
    fn drain(&mut self) -> Vec<Event> {
        self.rx.try_iter().collect()
    }
    fn refresh(&mut self, ids: &[String]) {
        let _ = self.tx.send(Command::Refresh(ids.to_vec()));
    }
}
impl Drop for WindowsSource {
    fn drop(&mut self) {
        let _ = self.tx.send(Command::Stop);
    }
}
// Unsubscribe even when setup/start fails partway through; late callbacks carry a generation.
struct Watching {
    watcher: DeviceWatcher,
    added: Option<i64>,
    updated: Option<i64>,
    removed: Option<i64>,
    enumerated: Option<i64>,
    stopped: Option<i64>,
}
impl Drop for Watching {
    fn drop(&mut self) {
        if let Some(t) = self.added {
            let _ = self.watcher.RemoveAdded(t);
        }
        if let Some(t) = self.updated {
            let _ = self.watcher.RemoveUpdated(t);
        }
        if let Some(t) = self.removed {
            let _ = self.watcher.RemoveRemoved(t);
        }
        if let Some(t) = self.enumerated {
            let _ = self.watcher.RemoveEnumerationCompleted(t);
        }
        if let Some(t) = self.stopped {
            let _ = self.watcher.RemoveStopped(t);
        }
        let _ = self.watcher.Stop();
    }
}
fn watch(tx: &Sender<Command>, generation: u64) -> Result<Watching> {
    let watcher = DeviceInformation::CreateWatcherWithKindAqsFilterAndAdditionalProperties(
        &HSTRING::from(FILTER),
        &properties(),
        DeviceInformationKind::Device,
    )?;
    let mut w = Watching {
        watcher,
        added: None,
        updated: None,
        removed: None,
        enumerated: None,
        stopped: None,
    };
    let t = tx.clone();
    w.added = Some(w.watcher.Added(
        &TypedEventHandler::<DeviceWatcher, DeviceInformation>::new(move |_, d| {
            if let Some(d) = d.as_ref() {
                let raw = d.Id()?.to_string();
                let u = decode(raw.clone(), Some(d.Name()?.to_string()), &d.Properties()?);
                // Non-battery profile/service Added events never enter our registry or model.
                if matches!(u.percent, Some(Some(0..=100))) {
                    let _ = t.send(Command::Node(generation, raw, u));
                }
            }
            Ok(())
        }),
    )?);
    let t = tx.clone();
    w.updated = Some(w.watcher.Updated(&TypedEventHandler::<
        DeviceWatcher,
        DeviceInformationUpdate,
    >::new(move |_, d| {
        if let Some(d) = d.as_ref() {
            let raw = d.Id()?.to_string();
            let u = decode(raw.clone(), None, &d.Properties()?);
            let _ = t.send(Command::Node(generation, raw, u));
        }
        Ok(())
    }))?);
    let t = tx.clone();
    w.removed = Some(w.watcher.Removed(&TypedEventHandler::<
        DeviceWatcher,
        DeviceInformationUpdate,
    >::new(move |_, d| {
        if let Some(d) = d.as_ref() {
            let _ = t.send(Command::Removed(generation, d.Id()?.to_string()));
        }
        Ok(())
    }))?);
    let t = tx.clone();
    w.enumerated = Some(w.watcher.EnumerationCompleted(&TypedEventHandler::<
        DeviceWatcher,
        IInspectable,
    >::new(move |_, _| {
        let _ = t.send(Command::Enumerated(generation));
        Ok(())
    }))?);
    let t = tx.clone();
    w.stopped = Some(
        w.watcher
            .Stopped(&TypedEventHandler::<DeviceWatcher, IInspectable>::new(
                move |_, _| {
                    let _ = t.send(Command::Stopped(generation));
                    Ok(())
                },
            ))?,
    );
    w.watcher.Start()?;
    Ok(w)
}
fn container_connected(id: &str) -> Option<bool> {
    let id = id.strip_prefix("container:")?;
    let props: IIterable<HSTRING> = vec![HSTRING::from(CONNECTED)].into();
    let d = wait(
        DeviceInformation::CreateFromIdAsyncWithKindAndAdditionalProperties(&HSTRING::from(id), &props, DeviceInformationKind::DeviceContainer).ok()?,
        "a device container read",
    )?;
    property(&d.Properties().ok()?, CONNECTED)?
        .GetBoolean()
        .ok()
}
fn receive(nodes: &mut Nodes, events: &Sender<Event>, raw: String, mut u: Update) {
    if !nodes.0.contains_key(&raw) && !matches!(u.percent, Some(Some(0..=100))) {
        return;
    }
    let id = nodes.identity(&raw, &u);
    let container_state = container_connected(&id);
    let connected = container_state.or(u.connected).unwrap_or(false);
    // A container result is authoritative for every profile, including a disconnect.
    if container_state.is_some() || !connected {
        for n in nodes.0.values_mut().filter(|n| n.id == id) {
            n.connected = Some(connected);
        }
    }
    u.connected = Some(connected);
    if let Some(u) = nodes.update(raw.clone(), u) {
        let _ = events.send(Event::Mapped { raw, update: u });
    }
}
/// Every battery node read again directly (the watcher aside): new and
/// changed ones go in, ones gone come out.
fn reread(nodes: &mut Nodes, events: &Sender<Event>) {
    let Ok(op) = DeviceInformation::FindAllAsyncWithKindAqsFilterAndAdditionalProperties(&HSTRING::from(FILTER), &properties(), DeviceInformationKind::Device) else { return };
    let Some(all) = wait(op, "the 30-minute battery read") else { return };
    let mut seen = std::collections::BTreeSet::new();
    for d in all {
        let (Ok(raw), Ok(p)) = (d.Id(), d.Properties()) else { continue };
        let raw = raw.to_string();
        let u = decode(raw.clone(), d.Name().ok().map(|s| s.to_string()), &p);
        if nodes.0.contains_key(&raw) || matches!(u.percent, Some(Some(0..=100))) {
            seen.insert(raw.clone());
            receive(nodes, events, raw, u);
        }
    }
    let gone: Vec<String> = nodes.0.keys().filter(|raw| !seen.contains(*raw)).cloned().collect();
    for raw in gone {
        if let Some(e) = nodes.remove(&raw) {
            let _ = events.send(e);
        }
    }
}
/// The Bluetooth radio's on/off changes, as commands (None: no radio, or
/// it cannot be read; the other restarts still hold).
struct RadioWatch(windows::Devices::Radios::Radio, i64);
impl Drop for RadioWatch {
    fn drop(&mut self) {
        let _ = self.0.RemoveStateChanged(self.1);
    }
}
fn watch_radio(tx: &Sender<Command>) -> Option<RadioWatch> {
    use windows::Devices::Radios::{Radio, RadioKind, RadioState};
    let radios = wait(Radio::GetRadiosAsync().ok()?, "the radio list")?;
    let radio = radios.into_iter().find(|r| r.Kind().ok() == Some(RadioKind::Bluetooth))?;
    let t = tx.clone();
    let token = radio
        .StateChanged(&TypedEventHandler::<Radio, IInspectable>::new(move |r, _| {
            if let Some(r) = r.as_ref() {
                let _ = t.send(Command::Radio(r.State()? == RadioState::On));
            }
            Ok(())
        }))
        .ok()?;
    Some(RadioWatch(radio, token))
}
fn run(events: Sender<Event>, commands: Receiver<Command>, callbacks: Sender<Command>) {
    let mut generation = 0;
    let mut failures: u32 = 0;
    let mut last_error = String::new();
    let radio = watch_radio(&callbacks);
    let mut radio_on = radio.as_ref().and_then(|r| r.0.State().ok()).map(|s| s == windows::Devices::Radios::RadioState::On);
    loop {
        generation += 1;
        let mut wait_for = Duration::from_secs(1);
        match watch(&callbacks, generation) {
            Ok(w) => {
                if failures > 0 {
                    crate::log!("device battery watcher: running again after {failures} failed tries");
                }
                failures = 0;
                last_error.clear();
                let mut nodes = Nodes::default();
                let mut last_pass = std::time::SystemTime::now();
                let mut last_reread = Instant::now();
                loop {
                    let mut radio_came_on = false;
                    match commands.recv_timeout(Duration::from_secs(1)) {
                        Ok(Command::Stop) | Err(mpsc::RecvTimeoutError::Disconnected) => return,
                        Ok(Command::Node(g, raw, u)) if g == generation => {
                            receive(&mut nodes, &events, raw, u)
                        }
                        Ok(Command::Removed(g, raw)) if g == generation => {
                            if let Some(e) = nodes.remove(&raw) {
                                let _ = events.send(e);
                            }
                        }
                        Ok(Command::Enumerated(g)) if g == generation => {
                            let _ = events.send(Event::Enumerated);
                        }
                        Ok(Command::Stopped(g)) if g == generation => {}
                        Ok(Command::Radio(on)) => {
                            if radio_on != Some(on) {
                                radio_came_on = on;
                                if !on {
                                    // Nothing is connected over a radio that is off.
                                    crate::log!("device battery: bluetooth radio off");
                                    let _ = events.send(Event::Reset);
                                }
                            }
                            radio_on = Some(on);
                        }
                        Ok(Command::Refresh(ids)) => {
                            for raw in nodes.raw_ids(&ids) {
                                if let Ok(op) = DeviceInformation::CreateFromIdAsyncWithKindAndAdditionalProperties(
                                    &HSTRING::from(&raw), &properties(), DeviceInformationKind::Device) {
                                    if let Some(d) = wait(op, "a battery read") {
                                        if let Ok(p) = d.Properties() {
                                            let u = decode(raw.clone(), d.Name().ok().map(|s| s.to_string()), &p);
                                            receive(&mut nodes, &events, raw, u);
                                        }
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                    let now = std::time::SystemTime::now();
                    let gap = now.duration_since(last_pass).unwrap_or_default();
                    last_pass = now;
                    if let Some(why) = restart_reason(w.watcher.Status().ok(), gap, radio_came_on) {
                        crate::log!("device battery watcher: restarted ({why})");
                        break;
                    }
                    if last_reread.elapsed() >= REREAD {
                        last_reread = Instant::now();
                        reread(&mut nodes, &events);
                    }
                }
            }
            Err(e) => {
                failures = failures.saturating_add(1);
                let text = e.to_string();
                // Said once per kind of failure, not every retry.
                if text != last_error {
                    crate::log!("device battery watcher: {text} (retrying)");
                    last_error = text;
                }
                wait_for = backoff(failures);
            }
        }
        let _ = events.send(Event::Reset);
        // Bounded retry delay; Stop is still serviced immediately while waiting.
        let deadline = Instant::now() + wait_for;
        while let Some(left) = deadline.checked_duration_since(Instant::now()) {
            match commands.recv_timeout(left) {
                Ok(Command::Stop) | Err(mpsc::RecvTimeoutError::Disconnected) => return,
                Err(mpsc::RecvTimeoutError::Timeout) => break,
                Ok(Command::Radio(on)) => radio_on = Some(on),
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    /// The names Windows gives common Bluetooth keyboards and mice.
    #[test]
    fn common_keyboards_and_mice_get_their_glyph() {
        for n in ["MX Keys", "MX Keys Mini", "MX Keys S", "Keychron K2", "Magic Keyboard", "K380 Multi-Device", "POP Keys", "블루투스 키보드"] {
            assert_eq!(classify(n), Kind::Keyboard, "{n}");
        }
        for n in ["MX Master 3S", "MX Anywhere 3", "MX Ergo", "Pebble M350", "Magic Mouse", "Razer Viper V2 Pro", "무선 마우스"] {
            assert_eq!(classify(n), Kind::Mouse, "{n}");
        }
        for n in ["Galaxy Buds2 Pro", "WH-1000XM5", "AirPods Pro"] {
            assert_eq!(classify(n), Kind::Headphones, "{n}");
        }
        for n in ["Xbox Wireless Controller", "DualSense Wireless Controller"] {
            assert_eq!(classify(n), Kind::Gamepad, "{n}");
        }
        assert_eq!(classify("Surface Pen"), Kind::Other);
    }
    use windows::Foundation::PropertyValue;
    #[test]
    fn review_02_container_profiles_merge_and_container_connection_wins() {
        crate::gfx::promo_kit::keep_com();
        unsafe {
            let _ = windows::Win32::System::WinRT::RoInitialize(
                windows::Win32::System::WinRT::RO_INIT_MULTITHREADED,
            );
        }
        let guid = windows::core::GUID::from_u128(0x12345678_1234_5678_1234_567812345678);
        let map: IMapView<HSTRING, IInspectable> = std::collections::BTreeMap::from([
            (
                HSTRING::from("System.Devices.ContainerId"),
                Some(PropertyValue::CreateGuid(guid).unwrap()),
            ),
            (
                HSTRING::from("System.Devices.Connected"),
                Some(PropertyValue::CreateBoolean(false).unwrap()),
            ),
            (
                HSTRING::from(STATUS),
                Some(PropertyValue::CreateUInt32(8).unwrap()),
            ),
            (
                HSTRING::from(BATTERY),
                Some(PropertyValue::CreateUInt8(18).unwrap()),
            ),
        ])
        .into();
        let a = decode("BTHENUM/profile".into(), Some("headphones".into()), &map);
        let b = decode("BTHLEDevice/service".into(), None, &map);
        assert_eq!(a.id, b.id, "same ContainerId must share identity");
        assert_eq!(
            a.connected,
            Some(false),
            "container false overrides started profile"
        );
    }
    /// The watcher is made again when it stops or aborts, when the PC is
    /// back from sleep (a pass a minute late) and when the Bluetooth radio
    /// comes back on; each with its reason for dock.log. A watcher that
    /// cannot be made is tried again, slower each time, a minute at most.
    #[test]
    fn a_stopped_watcher_restarts_with_its_reason() {
        use windows::Devices::Enumeration::DeviceWatcherStatus as S;
        let ok = Some(S::EnumerationCompleted);
        assert_eq!(restart_reason(ok, Duration::from_secs(1), false), None);
        assert_eq!(restart_reason(Some(S::Aborted), Duration::from_secs(1), false).as_deref(), Some("watcher aborted"));
        assert_eq!(restart_reason(Some(S::Stopped), Duration::from_secs(1), false).as_deref(), Some("watcher stopped"));
        assert_eq!(restart_reason(None, Duration::ZERO, false).as_deref(), Some("watcher gone"));
        assert_eq!(restart_reason(ok, Duration::from_secs(59), false), None);
        assert_eq!(restart_reason(ok, Duration::from_secs(3600), false).as_deref(), Some("back after 3600 s (sleep)"));
        assert_eq!(restart_reason(ok, Duration::from_secs(1), true).as_deref(), Some("bluetooth radio on"));
        assert_eq!([1, 2, 3, 6, 7, 40].map(|n| backoff(n).as_secs()), [2, 4, 8, 60, 60, 60]);
        assert_eq!(backoff(0).as_secs(), 1);
        assert_eq!(REREAD.as_secs(), 30 * 60, "every battery read again every 30 minutes");
    }
    #[test]
    fn review_10_stopped_and_aborted_watchers_are_recreated() {
        use windows::Devices::Enumeration::DeviceWatcherStatus as S;
        assert!(!restart_needed(Some(S::Started)));
        assert!(!restart_needed(Some(S::EnumerationCompleted)));
        assert!(restart_needed(Some(S::Stopped)));
        assert!(restart_needed(Some(S::Aborted)));
        assert!(restart_needed(None));
    }
    #[test]
    fn device_battery_property_decoding_is_passive_and_preserves_absent_fields() {
        crate::gfx::promo_kit::keep_com();
        unsafe {
            let _ = windows::Win32::System::WinRT::RoInitialize(
                windows::Win32::System::WinRT::RO_INIT_MULTITHREADED,
            );
        }
        let values = std::collections::BTreeMap::from([
            (
                HSTRING::from(BATTERY),
                Some(PropertyValue::CreateUInt8(18).unwrap()),
            ),
            (
                HSTRING::from(CONNECTED),
                Some(PropertyValue::CreateBoolean(true).unwrap()),
            ),
            (
                HSTRING::from(CHARGING),
                Some(PropertyValue::CreateBoolean(true).unwrap()),
            ),
        ]);
        let map: IMapView<HSTRING, IInspectable> = values.into();
        let u = decode("fake".into(), Some("이어폰".into()), &map);
        assert_eq!(u.percent, Some(Some(18)));
        assert_eq!(u.connected, Some(true));
        assert_eq!(u.charging, Some(true));
        assert_eq!(u.kind, Some(Kind::Headphones));
        let empty: IMapView<HSTRING, IInspectable> = std::collections::BTreeMap::new().into();
        let u = decode("fake".into(), None, &empty);
        assert!(u.percent.is_none() && u.connected.is_none() && u.charging.is_none());
        let invalid: IPropertyValue = PropertyValue::CreateUInt32(255).unwrap().cast().unwrap();
        assert_eq!(percent(&invalid), None);
        let zero: IPropertyValue = PropertyValue::CreateUInt8(0).unwrap().cast().unwrap();
        assert_eq!(percent(&zero), Some(0));
    }
    #[test]
    fn review_02_two_nodes_produce_one_row_one_notice_and_partial_removal() {
        use super::super::model::{Desktop, Model};
        let mut nodes = Nodes::default();
        let mut model = Model::default();
        let sample = Update {
            id: "container:{fake}".into(),
            name: Some("earphones".into()),
            percent: Some(Some(18)),
            connected: Some(true),
            ..Default::default()
        };
        model.update(nodes.update("profile".into(), sample.clone()).unwrap(), 1);
        assert!(model.acknowledge(&sample.id));
        model.update(nodes.update("service".into(), sample.clone()).unwrap(), 2);
        assert_eq!(model.rows(&Desktop::default()).len(), 1);
        assert!(!model.devices[&sample.id].needs_notice());
        assert_eq!(nodes.raw_ids(std::slice::from_ref(&sample.id)).len(), 2);
        let Event::Update(u) = nodes.remove("profile").unwrap() else {
            panic!("remaining node lost");
        };
        model.update(u, 3);
        assert!(model.devices[&sample.id].connected);
        assert!(matches!(nodes.remove("service"), Some(Event::Removed(_))));
        assert!(nodes
            .update("batteryless".into(), Update::default())
            .is_none());
    }
}

#[cfg(test)]
mod probe {
    use super::*;
    /// Read-only: what this PC's Bluetooth battery nodes say now.
    #[test]
    #[ignore = "reads this PC's devices"]
    fn probe_this_pcs_devices() {
        unsafe {
            let _ = windows::Win32::System::WinRT::RoInitialize(windows::Win32::System::WinRT::RO_INIT_MULTITHREADED);
        }
        let all = DeviceInformation::FindAllAsyncWithKindAqsFilterAndAdditionalProperties(
            &HSTRING::from(FILTER),
            &properties(),
            DeviceInformationKind::Device,
        )
        .unwrap()
        .join()
        .unwrap();
        for d in all {
            let raw = d.Id().unwrap().to_string();
            let p = d.Properties().unwrap();
            let u = decode(raw.clone(), Some(d.Name().unwrap().to_string()), &p);
            if u.percent.is_none() {
                continue;
            }
            let dev_connected = property(&p, CONNECTED).and_then(|v| v.GetBoolean().ok());
            let status = property(&p, STATUS).and_then(|v| v.GetUInt32().ok());
            if matches!(u.percent, Some(Some(_))) {
                eprintln!("PROBE {:?} raw={} pct={:?} dev_connected={:?} status={:?} container={:?} -> {:?}", u.name, raw, u.percent, dev_connected, status.map(|s| format!("{s:#x}")), container_connected(&u.id), u.connected);
            }
        }
    }
}
