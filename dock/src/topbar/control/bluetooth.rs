//! Paired Bluetooth devices and the audio filters that can reconnect them.

use super::*;
use std::collections::{HashMap, HashSet};
use windows::core::{GUID, HSTRING, PCWSTR};
use windows::Devices::{
    Bluetooth::{BluetoothDevice, BluetoothLEDevice},
    Enumeration::{
        DeviceInformation, DeviceInformationKind, DeviceInformationUpdate, DeviceWatcher,
    },
};
use windows::Foundation::IPropertyValue;
use windows::Media::Devices::MediaDevice;
use windows::Win32::{
    Devices::FunctionDiscovery::PKEY_Device_ContainerId,
    Media::{
        Audio::{
            eAll, IDeviceTopology, IMMDevice, IMMDeviceEnumerator, IPart, MMDeviceEnumerator,
            DEVICE_STATE, DEVICE_STATEMASK_ALL, DEVICE_STATE_ACTIVE, DEVICE_STATE_UNPLUGGED,
        },
        KernelStreaming::{
            IKsControl, KSPROPSETID_BtAudio, KSIDENTIFIER, KSIDENTIFIER_0, KSIDENTIFIER_0_0,
            KSPROPERTY_ONESHOT_DISCONNECT, KSPROPERTY_ONESHOT_RECONNECT,
            KSPROPERTY_TYPE_BASICSUPPORT, KSPROPERTY_TYPE_GET,
        },
    },
    System::Com::{
        CoTaskMemFree,
        StructuredStorage::PropVariantToGUID,
        STGM_READ,
    },
};
use windows_collections::{IIterable, IMapView};

const CONNECTED: &str = "System.Devices.Aep.IsConnected";
const CONTAINER: &str = "System.Devices.Aep.ContainerId";
const CATEGORY: &str = "System.Devices.Aep.Category";
const MAJOR: &str = "System.Devices.Aep.Bluetooth.Cod.Major";
const MINOR: &str = "System.Devices.Aep.Bluetooth.Cod.Minor";
const APPEARANCE: &str = "System.Devices.Aep.Bluetooth.Le.Appearance";
const NAME: &str = "System.ItemNameDisplay";
const BATTERY: &str = "{104EA319-6EE2-4701-BD47-8DDBF425BBE5} 2";
const STUDIO: &str = "{6EDC630D-C2E3-43B7-B2D1-20525A1AF120} 4";
const BATTERY_EVERY: Duration = Duration::from_secs(45);
const CHECK: Duration = Duration::from_millis(250);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum BtKind {
    #[default]
    Other,
    Keyboard,
    Mouse,
    Gamepad,
    Audio,
}

impl BtKind {
    pub(super) fn glyph(self) -> Glyph {
        match self {
            Self::Audio => Glyph::Headphones,
            Self::Keyboard => Glyph::Keyboard,
            Self::Mouse => Glyph::Mouse,
            Self::Gamepad => Glyph::Gamepad,
            Self::Other => Glyph::Bluetooth,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct BtDevice {
    pub(super) key: u64,
    pub(super) name: String,
    pub(super) kind: BtKind,
    pub(super) connected: bool,
    pub(super) battery: Option<u8>,
    pub(super) can_switch: bool,
}

#[derive(Clone, Default)]
struct EndpointView {
    name: String,
    container: Option<GUID>,
    connected: bool,
    kind: BtKind,
}

pub(super) enum DeviceCmd {
    Added(DeviceInformation),
    Updated(DeviceInformationUpdate),
    Removed(DeviceInformationUpdate),
    Studio,
    AudioChanged,
    ListChanged,
    Switch(u64, String, bool),
    Quit,
}

type Properties = IMapView<HSTRING, windows::core::IInspectable>;

fn value(properties: &Properties, name: &str) -> Option<IPropertyValue> {
    properties.Lookup(&HSTRING::from(name)).ok()?.cast().ok()
}

fn numbers(properties: &Properties, name: &str) -> Option<u32> {
    let value = value(properties, name)?;
    value
        .GetUInt32()
        .ok()
        .or_else(|| value.GetUInt16().ok().map(u32::from))
        .or_else(|| value.GetUInt8().ok().map(u32::from))
}

fn properties(names: &[&str]) -> IIterable<HSTRING> {
    names
        .iter()
        .copied()
        .map(HSTRING::from)
        .collect::<Vec<_>>()
        .into()
}

fn device_kind(major: Option<u32>, minor: Option<u32>, appearance: Option<u32>) -> BtKind {
    // Classic peripheral minor uses two keyboard/pointing bits and four subclass bits.
    let classic = match (major, minor.unwrap_or_default()) {
        (Some(4), _) => BtKind::Audio,
        (Some(5), minor) if matches!(minor & 0x0f, 1 | 2) => BtKind::Gamepad,
        (Some(5), minor) if minor & 0x30 == 0x20 => BtKind::Mouse,
        (Some(5), minor) if minor & 0x10 != 0 => BtKind::Keyboard,
        _ => BtKind::Other,
    };
    let le = match appearance {
        Some(0x03c1) => BtKind::Keyboard,
        Some(0x03c2) => BtKind::Mouse,
        Some(0x03c3 | 0x03c4) => BtKind::Gamepad,
        Some(0x0840..=0x08bf | 0x0940..=0x097f) => BtKind::Audio,
        _ => BtKind::Other,
    };
    classic.max(le)
}

impl EndpointView {
    fn update(&mut self, p: &Properties) {
        if let Some(name) = value(p, NAME)
            .and_then(|v| v.GetString().ok())
            .filter(|n| !n.is_empty())
        {
            self.name = name.to_string();
        }
        if let Some(v) = value(p, CONNECTED).and_then(|v| v.GetBoolean().ok()) {
            self.connected = v;
        }
        if let Some(v) = value(p, CONTAINER)
            .and_then(|v| v.GetGuid().ok())
            .filter(|v| *v != GUID::zeroed())
        {
            self.container = Some(v);
        }
        let mut categories = windows::core::Array::<HSTRING>::new();
        let _ = value(p, CATEGORY).and_then(|v| v.GetStringArray(&mut categories).ok());
        let audio = categories.iter().any(|s| {
            let s = s.to_string();
            s.starts_with("Audio") || s.starts_with("Communication.Headset")
        });
        let kind = if audio {
            BtKind::Audio
        } else {
            device_kind(numbers(p, MAJOR), numbers(p, MINOR), numbers(p, APPEARANCE))
        };
        self.kind = self.kind.max(kind);
    }
}

fn watch(aqs: HSTRING, tx: &Sender<DeviceCmd>) -> Option<Defer> {
    let p = properties(&[
        CONNECTED,
        "System.Devices.Aep.IsPaired",
        CONTAINER,
        CATEGORY,
        MAJOR,
        MINOR,
        APPEARANCE,
        NAME,
    ]);
    let w = DeviceInformation::CreateWatcherWithKindAqsFilterAndAdditionalProperties(
        &aqs,
        &p,
        DeviceInformationKind::AssociationEndpoint,
    )
    .ok()?;
    let a = tx.clone();
    let added = w
        .Added(&TypedEventHandler::<DeviceWatcher, DeviceInformation>::new(
            move |_, info| {
                if let Some(info) = info.as_ref() {
                    let _ = a.send(DeviceCmd::Added(info.clone()));
                }
                Ok(())
            },
        ))
        .ok()?;
    let u = tx.clone();
    let updated = w
        .Updated(
            &TypedEventHandler::<DeviceWatcher, DeviceInformationUpdate>::new(move |_, info| {
                if let Some(info) = info.as_ref() {
                    let _ = u.send(DeviceCmd::Updated(info.clone()));
                }
                Ok(())
            }),
        )
        .ok()?;
    let r = tx.clone();
    let removed = w
        .Removed(
            &TypedEventHandler::<DeviceWatcher, DeviceInformationUpdate>::new(move |_, info| {
                if let Some(info) = info.as_ref() {
                    let _ = r.send(DeviceCmd::Removed(info.clone()));
                }
                Ok(())
            }),
        )
        .ok()?;
    w.Start().ok()?;
    Some(Defer::new(move || {
        let _ = w.Stop();
        let _ = w.RemoveAdded(added);
        let _ = w.RemoveUpdated(updated);
        let _ = w.RemoveRemoved(removed);
    }))
}

fn studio_watch(selector: &HSTRING, tx: &Sender<DeviceCmd>) -> Option<Defer> {
    let w = DeviceInformation::CreateWatcherAqsFilterAndAdditionalProperties(
        selector,
        &properties(&[STUDIO]),
    )
    .ok()?;
    let a = tx.clone();
    let added = w
        .Added(&TypedEventHandler::<DeviceWatcher, DeviceInformation>::new(
            move |_, _| {
                let _ = a.send(DeviceCmd::Studio);
                Ok(())
            },
        ))
        .ok()?;
    let r = tx.clone();
    let removed = w
        .Removed(
            &TypedEventHandler::<DeviceWatcher, DeviceInformationUpdate>::new(move |_, _| {
                let _ = r.send(DeviceCmd::Studio);
                Ok(())
            }),
        )
        .ok()?;
    w.Start().ok()?;
    Some(Defer::new(move || {
        let _ = w.Stop();
        let _ = w.RemoveAdded(added);
        let _ = w.RemoveRemoved(removed);
    }))
}

fn studio_camera(shared: &Shared, selector: &HSTRING) -> Option<String> {
    let p = properties(&[STUDIO]);
    let devices = wait(
        DeviceInformation::FindAllAsyncAqsFilterAndAdditionalProperties(selector, &p),
        2000,
        shared,
        Kind::Devices,
        "Studio Effects cameras",
    )?;
    devices.into_iter().find_map(|device| {
        let p = device.Properties().ok()?;
        value(&p, STUDIO)?
            .GetBoolean()
            .ok()?
            .then(|| device.Id().ok().map(|s| s.to_string()))
            .flatten()
    })
}

fn battery(shared: &Shared, container: GUID) -> Option<u8> {
    let aqs = HSTRING::from(format!("System.Devices.ContainerId:=\"{{{container:?}}}\""));
    let p = properties(&[BATTERY]);
    let devices = wait(
        DeviceInformation::FindAllAsyncWithKindAqsFilterAndAdditionalProperties(
            &aqs,
            &p,
            DeviceInformationKind::Device,
        ),
        700,
        shared,
        Kind::Devices,
        "Bluetooth battery",
    )?;
    devices.into_iter().find_map(|d| {
        value(&d.Properties().ok()?, BATTERY)?
            .GetUInt8()
            .ok()
            .filter(|n| *n <= 100)
    })
}

unsafe fn endpoint_container(endpoint: &IMMDevice) -> Option<GUID> {
    let store = endpoint.OpenPropertyStore(STGM_READ).ok()?;
    let property = store.GetValue(&PKEY_Device_ContainerId).ok()?;
    PropVariantToGUID(&property).ok()
}

unsafe fn filter_ids(endpoint: &IMMDevice) -> Option<Vec<String>> {
    let topology = endpoint
        .Activate::<IDeviceTopology>(CLSCTX_INPROC_SERVER, None)
        .ok()?;
    let mut ids = Vec::new();
    for i in 0..topology.GetConnectorCount().ok()? {
        let part = topology
            .GetConnector(i)
            .and_then(|c| c.GetConnectedTo())
            .and_then(|c| c.cast::<IPart>());
        if let Ok(id) = part
            .and_then(|p| p.GetTopologyObject())
            .and_then(|t| t.GetDeviceId())
        {
            let text = id.to_string().ok();
            CoTaskMemFree(Some(id.0.cast()));
            if let Some(text) = text {
                ids.push(text);
            }
        }
    }
    Some(ids)
}

#[derive(Default)]
struct AudioFilters {
    ids: HashSet<String>,
    connected: Option<bool>,
}

fn audio_property(property: u32, flags: u32) -> KSIDENTIFIER {
    KSIDENTIFIER {
        Anonymous: KSIDENTIFIER_0 {
            Anonymous: KSIDENTIFIER_0_0 {
                Set: KSPROPSETID_BtAudio,
                Id: property,
                Flags: flags,
            },
        },
    }
}

unsafe fn open_filter(enumerator: &IMMDeviceEnumerator, id: &str) -> windows::core::Result<IKsControl> {
    enumerator
        .GetDevice(PCWSTR(HSTRING::from(id).as_ptr()))?
        .Activate(CLSCTX_INPROC_SERVER, None)
}

fn reconnect_supported(reply: Option<(u32, u32)>) -> bool {
    reply.is_some_and(|(size, access)| {
        (size == 0 || size == size_of::<u32>() as u32) && access & KSPROPERTY_TYPE_GET != 0
    })
}

unsafe fn probe_reconnect(enumerator: &IMMDeviceEnumerator, id: &str) -> Option<(u32, u32)> {
    if cfg!(test) {
        return None;
    }
    let filter = open_filter(enumerator, id).ok()?;
    let property = audio_property(KSPROPERTY_ONESHOT_RECONNECT.0 as u32, KSPROPERTY_TYPE_BASICSUPPORT);
    let mut access = 0u32;
    let mut returned = 0;
    // The binding converts all successful HRESULTs to (). Keep the exact
    // code for the first capability probe on each filter.
    let hr = (Interface::vtable(&filter).KsProperty)(
        Interface::as_raw(&filter),
        &property,
        size_of::<KSIDENTIFIER>() as u32,
        (&mut access as *mut u32).cast(),
        size_of::<u32>() as u32,
        &mut returned,
    );
    static LOGGED: std::sync::OnceLock<Mutex<HashSet<String>>> = std::sync::OnceLock::new();
    if LOGGED.get_or_init(Mutex::default).lock().is_ok_and(|mut ids| !ids.contains(id) && ids.insert(id.to_owned())) {
        crate::log!("bt: reconnect capability filter={id:?} hr={:#010x} access={access:#010x} returned={returned}", hr.0 as u32);
    }
    hr.is_ok().then_some((returned, access))
}

fn supported_filter_ids(ids: Vec<String>, probe: impl Fn(&str) -> Option<(u32, u32)>) -> Vec<String> {
    ids.into_iter()
        .filter(|id| reconnect_supported(probe(id)))
        .collect()
}

/// Only expose audio controls for containers with a Bluetooth KS filter.
unsafe fn filters(
    enumerator: &IMMDeviceEnumerator,
    containers: &HashSet<GUID>,
) -> HashMap<GUID, AudioFilters> {
    let mut all: HashMap<GUID, AudioFilters> = HashMap::new();
    if let Ok(endpoints) = enumerator.EnumAudioEndpoints(eAll, DEVICE_STATE(DEVICE_STATEMASK_ALL)) {
        for i in 0..endpoints.GetCount().unwrap_or(0) {
            let Ok(endpoint) = endpoints.Item(i) else {
                continue;
            };
            if let Some(container) = endpoint_container(&endpoint).filter(|id| containers.contains(id)) {
                let ids = supported_filter_ids(filter_ids(&endpoint).unwrap_or_default(), |id| {
                    probe_reconnect(enumerator, id)
                });
                if !ids.is_empty() {
                    let audio = all.entry(container).or_default();
                    audio.ids.extend(ids);
                    match endpoint.GetState() {
                        Ok(DEVICE_STATE_ACTIVE) => audio.connected = Some(true),
                        Ok(DEVICE_STATE_UNPLUGGED) => {
                            audio.connected.get_or_insert(false);
                        }
                        _ => {}
                    }
                }
            }
        }
    }
    all
}

unsafe fn switch(enumerator: &IMMDeviceEnumerator, ids: &HashSet<String>, on: bool) -> bool {
    if cfg!(test) {
        return false;
    }
    let id = if on {
        KSPROPERTY_ONESHOT_RECONNECT
    } else {
        KSPROPERTY_ONESHOT_DISCONNECT
    };
    let property = audio_property(id.0 as u32, KSPROPERTY_TYPE_GET);
    let mut accepted = false;
    for id in ids {
        if let Ok(filter) = open_filter(enumerator, id) {
            let mut returned = 0;
            accepted |= filter
                .KsProperty(
                    &property,
                    std::mem::size_of_val(&property) as u32,
                    std::ptr::null_mut(),
                    0,
                    &mut returned,
                )
                .is_ok();
        }
    }
    accepted
}

pub(super) fn log_switch(name: &str, on: bool, accepted: bool) {
    let operation = if on { "connect" } else { "disconnect" };
    let result = if accepted { "accepted" } else { "failed" };
    crate::log!("bt: {} {operation} -> {result}", name.replace(['\r', '\n'], " "));
}

fn merged(endpoints: &HashMap<String, EndpointView>) -> Vec<(u64, EndpointView)> {
    let mut merged: HashMap<u64, EndpointView> = HashMap::new();
    for (id, v) in endpoints {
        let key = v
            .container
            .map_or_else(|| device_key(id), |guid| device_key(&format!("{guid:?}")));
        let row = merged.entry(key).or_default();
        if !v.name.is_empty()
            && (row.name.is_empty()
                || v.connected && !row.connected
                || v.connected == row.connected && v.name < row.name)
        {
            row.name.clone_from(&v.name);
        }
        row.connected |= v.connected;
        row.kind = row.kind.max(v.kind);
        row.container = v.container;
    }
    let mut rows: Vec<_> = merged
        .into_iter()
        .filter(|(_, v)| !v.name.is_empty())
        .collect();
    rows.sort_by(|(a, x), (b, y)| {
        (!x.connected, x.name.to_lowercase(), a).cmp(&(!y.connected, y.name.to_lowercase(), b))
    });
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reconnect_capability_selects_new_and_legacy_filter_paths() {
        let supported = [
            r"{2}.\\?\apxenum#audio\speaker",
            r"{2}.\\?\bthenum#audio",
            r"{2}.\\?\bthhfenum#audio",
        ];
        let rejected = [
            (r"{2}.\\?\usb#audio", None),
            (r"{2}.\\?\bthenum#no-reconnect", Some((4, KSPROPERTY_TYPE_BASICSUPPORT))),
            (r"{2}.\\?\apxenum#short-reply", Some((2, KSPROPERTY_TYPE_GET))),
            (r"{2}.\\?\bthenum#empty-reply", Some((0, 0))),
        ];
        let paths = supported
            .into_iter()
            .chain(rejected.map(|(id, _)| id))
            .map(str::to_owned)
            .collect();
        let selected = supported_filter_ids(paths, |id| {
            rejected.iter().find(|(path, _)| *path == id).map_or_else(
                || Some((if id == supported[2] { 0 } else { 4 }, 0x201)),
                |(_, reply)| *reply,
            )
        });
        assert_eq!(selected, supported);
    }

    #[test]
    fn class_of_device_and_le_appearance_distinguish_device_types() {
        for (minor, expected) in [
            (0x20, BtKind::Mouse), (0x10, BtKind::Keyboard), (2, BtKind::Gamepad), (3, BtKind::Other),
        ] {
            assert_eq!(device_kind(Some(5), Some(minor), None), expected);
        }
        for (appearance, expected) in [
            (0x03c1, BtKind::Keyboard),
            (0x03c2, BtKind::Mouse),
            (0x03c4, BtKind::Gamepad),
            (0x0941, BtKind::Audio),
            (0x03c0, BtKind::Other),
        ] {
            assert_eq!(device_kind(None, None, Some(appearance)), expected);
        }
        assert_eq!(device_kind(Some(4), Some(6), None), BtKind::Audio);
    }

    #[test]
    fn a_connected_nameless_endpoint_keeps_the_paired_devices_name() {
        let container = Some(GUID::from_u128(1));
        let endpoints = HashMap::from([
            (
                "classic".into(),
                EndpointView {
                    name: "Headphones".into(),
                    container,
                    kind: BtKind::Audio,
                    ..Default::default()
                },
            ),
            (
                "le".into(),
                EndpointView {
                    container,
                    connected: true,
                    ..Default::default()
                },
            ),
        ]);
        let rows = merged(&endpoints);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].1.name, "Headphones");
        assert!(rows[0].1.connected);
        assert_eq!(rows[0].1.kind, BtKind::Audio);
    }
}

pub(super) fn devices_worker(shared: &Shared, rx: Receiver<DeviceCmd>, tx: Sender<DeviceCmd>) {
    if cfg!(test) {
        return;
    }
    let _bt = [
        BluetoothDevice::GetDeviceSelectorFromPairingState(true),
        BluetoothLEDevice::GetDeviceSelectorFromPairingState(true),
    ]
    .into_iter()
    .filter_map(|s| watch(s.ok()?, &tx))
    .collect::<Vec<_>>();
    let selector = MediaDevice::GetVideoCaptureSelector().ok();
    let _studio = selector.as_ref().and_then(|s| studio_watch(s, &tx));
    let enumerator: Option<IMMDeviceEnumerator> =
        unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_INPROC_SERVER) }.ok();
    let _audio_watch = enumerator.clone().and_then(|e| {
        let tx = tx.clone();
        let client = devices::Watch::client(move || {
            let _ = tx.send(DeviceCmd::AudioChanged);
        });
        unsafe { e.RegisterEndpointNotificationCallback(&client) }.ok()?;
        Some(Defer::new(move || unsafe {
            let _ = e.UnregisterEndpointNotificationCallback(&client);
        }))
    });
    let mut filters_by_device = HashMap::<GUID, AudioFilters>::new();
    let mut endpoints = HashMap::<String, EndpointView>::new();
    let mut batteries = HashMap::<GUID, (Instant, Option<u8>)>::new();
    let mut requests = HashMap::<u64, (bool, Instant)>::new();
    let mut refresh_filters = true;
    let mut next_filters = Instant::now();
    let mut last_filters = None::<Instant>;
    let mut refresh_studio = true;
    while !shared.quitting() {
        let cmd = rx.recv_timeout(CHECK);
        if shared.quitting() {
            break;
        }
        match cmd {
            Ok(DeviceCmd::Quit) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            Ok(DeviceCmd::Added(info)) => {
                if let (Ok(id), Ok(name), Ok(p)) = (info.Id(), info.Name(), info.Properties()) {
                    let mut v = EndpointView {
                        name: name.to_string(),
                        ..Default::default()
                    };
                    v.update(&p);
                    endpoints.insert(id.to_string(), v);
                    refresh_filters = true;
                }
            }
            Ok(DeviceCmd::Updated(info)) => {
                if let (Ok(id), Ok(p)) = (info.Id(), info.Properties()) {
                    if let Some(v) = endpoints.get_mut(&id.to_string()) {
                        let before = v.connected;
                        v.update(&p);
                        if before != v.connected {
                            if let Some(container) = v.container {
                                batteries.remove(&container);
                            }
                            refresh_filters = true;
                        }
                    }
                }
            }
            Ok(DeviceCmd::Removed(info)) => {
                if let Ok(id) = info.Id() {
                    endpoints.remove(&id.to_string());
                }
                refresh_filters = true;
            }
            Ok(DeviceCmd::Studio) => refresh_studio = true,
            Ok(DeviceCmd::AudioChanged) => refresh_filters = true,
            Ok(DeviceCmd::ListChanged) => {
                refresh_filters = true;
                next_filters = Instant::now();
            }
            Ok(DeviceCmd::Switch(key, name, on)) => {
                let container = merged(&endpoints)
                    .into_iter()
                    .find(|(k, _)| *k == key)
                    .and_then(|(_, v)| v.container);
                let result = container
                    .and_then(|c| filters_by_device.get(&c))
                    .zip(enumerator.as_ref())
                    .is_some_and(|(audio, e)| unsafe { switch(e, &audio.ids, on) });
                log_switch(&name, on, result);
                if result {
                    requests.insert(key, (on, Instant::now() + Duration::from_secs(20)));
                    refresh_filters = true;
                } else {
                    shared.update(|r| {
                        r.bt_busy.remove(&key);
                        r.bt_failed.insert(key, Instant::now());
                    });
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
        }
        let now = Instant::now();
        let rows = merged(&endpoints);
        let list_open = shared.bt_list_open.load(Ordering::Acquire);
        let dirty_due = refresh_filters && last_filters.is_none_or(|at| now.duration_since(at) >= CHECK);
        if list_open && (dirty_due || now >= next_filters) {
            let containers: HashSet<_> = rows.iter().filter_map(|(_, v)| v.container).collect();
            filters_by_device = enumerator
                .as_ref()
                .filter(|_| !containers.is_empty())
                .map(|e| unsafe { filters(e, &containers) })
                .unwrap_or_default();
            next_filters = now
                + if requests.is_empty() {
                    Duration::from_secs(5)
                } else {
                    Duration::from_millis(500)
                };
            refresh_filters = false;
            last_filters = Some(now);
        }
        let devices = rows
            .iter()
            .map(|(key, v)| {
                let audio = v.container.filter(|_| list_open).and_then(|c| filters_by_device.get(&c));
                let can_switch = audio.is_some();
                let connected = audio.and_then(|a| a.connected).unwrap_or(v.connected);
                BtDevice {
                    key: *key,
                    name: v.name.clone(),
                    kind: if can_switch { BtKind::Audio } else { v.kind },
                    connected,
                    battery: v
                        .container
                        .filter(|_| connected)
                        .and_then(|c| batteries.get(&c))
                        .and_then(|(_, battery)| *battery),
                    can_switch,
                }
            })
            .collect::<Vec<_>>();
        let battery_due = rows
            .iter()
            .zip(&devices)
            .filter(|(_, device)| device.connected)
            .filter_map(|((_, view), _)| view.container)
            .find(|c| {
                batteries.get(c).is_none_or(|(time, _)| now.saturating_duration_since(*time) >= BATTERY_EVERY)
            });
        let mut finished = Vec::new();
        requests.retain(|key, (target, until)| {
            let reached = devices
                .iter()
                .find(|d| d.key == *key)
                .is_some_and(|d| d.connected == *target);
            if reached || now >= *until {
                finished.push((*key, !reached));
                false
            } else {
                true
            }
        });
        let audio_ready = list_open && last_filters.is_some();
        let changed = shared.remote.lock().is_ok_and(|r| r.bt_devices != devices || r.bt_audio_ready != audio_ready);
        if changed || !finished.is_empty() {
            shared.update(|r| {
                r.bt_devices = devices;
                r.bt_audio_ready = audio_ready;
                for (key, failed) in finished {
                    r.bt_busy.remove(&key);
                    if failed {
                        r.bt_failed.insert(key, now);
                    }
                }
                r.bt_failed
                    .retain(|_, at| now.saturating_duration_since(*at) < FAIL_SHOW);
            });
        }
        if refresh_studio {
            let camera = selector.as_ref().and_then(|s| studio_camera(shared, s));
            shared.update(|r| r.studio = camera);
            refresh_studio = false;
        }
        // Give commands a chance to run between individual battery reads.
        if let Some(c) = battery_due {
            if !shared.quitting() {
                batteries.insert(c, (now, battery(shared, c)));
            }
        }
    }
}
