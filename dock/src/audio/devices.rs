//! Output and input devices for the Control Center's chooser: the active
//! endpoints with their names and kinds, which one is the default, making
//! one the default, and a listener for devices coming, going or changing.

use windows::core::{implement, Result, PCWSTR, PWSTR};
use windows::Win32::Foundation::PROPERTYKEY;
use windows::Win32::Media::Audio::{
    eCapture, eConsole, eRender, EDataFlow, ERole, IMMDeviceEnumerator, IMMNotificationClient, IMMNotificationClient_Impl,
    PKEY_AudioEndpoint_FormFactor, DEVICE_STATE, DEVICE_STATE_ACTIVE,
};
use windows::Win32::System::Com::StructuredStorage::PropVariantToUInt32;
use windows::Win32::System::Com::{CoTaskMemFree, STGM_READ};

/// PKEY_Device_FriendlyName ("Speakers (Realtek(R) Audio)").
const PKEY_FRIENDLY_NAME: PROPERTYKEY = PROPERTYKEY { fmtid: windows::core::GUID::from_u128(0xa45c254e_df1c_4efd_8020_67d146a850e0), pid: 14 };
/// PKEY_Device_EnumeratorName (the bus: "USB", "HDAUDIO", "BTHENUM"...).
const PKEY_ENUMERATOR: PROPERTYKEY = PROPERTYKEY { fmtid: windows::core::GUID::from_u128(0xa45c254e_df1c_4efd_8020_67d146a850e0), pid: 24 };

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Flow {
    Output,
    Input,
}

impl Flow {
    fn data_flow(self) -> EDataFlow {
        match self {
            Flow::Output => eRender,
            Flow::Input => eCapture,
        }
    }
}

/// What a device looks like (its icon).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Speakers,
    Headphones,
    Headset,
    /// HDMI / DisplayPort (a monitor or TV).
    Display,
    Bluetooth,
    Microphone,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Endpoint {
    pub id: String,
    pub name: String,
    pub kind: Kind,
    pub default: bool,
}

/// The kind from the endpoint's form factor (EndpointFormFactor) and bus.
pub fn kind_of(flow: Flow, form: Option<u32>, enumerator: Option<&str>) -> Kind {
    if enumerator.is_some_and(|e| e.to_ascii_uppercase().starts_with("BTH")) {
        return Kind::Bluetooth;
    }
    match form {
        Some(1) => Kind::Speakers,
        Some(3) => Kind::Headphones,
        // Headset, handset.
        Some(5 | 6) => Kind::Headset,
        Some(4) => Kind::Microphone,
        // DigitalAudioDisplayDevice.
        Some(9) => Kind::Display,
        _ if flow == Flow::Input => Kind::Microphone,
        _ => Kind::Speakers,
    }
}

/// The glyph for a kind (crate::glyphs).
pub fn glyph(k: Kind) -> crate::glyphs::Glyph {
    use crate::glyphs::Glyph;
    match k {
        Kind::Speakers => Glyph::Speaker,
        Kind::Headphones => Glyph::Headphones,
        Kind::Headset => Glyph::Headset,
        Kind::Display => Glyph::Monitor,
        Kind::Bluetooth => Glyph::Bluetooth,
        Kind::Microphone => Glyph::Microphone,
    }
}

/// A steady order (picking one must not move the list): by name, ignoring
/// case, then by id.
pub fn sort(list: &mut [Endpoint]) {
    list.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()).then_with(|| a.id.cmp(&b.id)));
}

/// Marks `id` as the default. (The Control Center no longer moves the
/// check before Windows confirms a switch; kept for the list tests.)
#[cfg(test)]
pub fn mark_default(list: &mut [Endpoint], id: &str) {
    for e in list {
        e.default = e.id == id;
    }
}

unsafe fn take_pwstr(p: PWSTR) -> String {
    let s = p.to_string().unwrap_or_default();
    CoTaskMemFree(Some(p.0 as *const _));
    s
}

/// The default device's id for `flow`.
pub unsafe fn default_id(e: &IMMDeviceEnumerator, flow: Flow) -> Option<String> {
    let dev = e.GetDefaultAudioEndpoint(flow.data_flow(), eConsole).ok()?;
    Some(take_pwstr(dev.GetId().ok()?))
}

/// The active devices for `flow`, sorted, the default marked.
pub unsafe fn list(e: &IMMDeviceEnumerator, flow: Flow) -> Vec<Endpoint> {
    let def = default_id(e, flow);
    let mut out = Vec::new();
    let Ok(coll) = e.EnumAudioEndpoints(flow.data_flow(), DEVICE_STATE_ACTIVE) else { return out };
    for i in 0..coll.GetCount().unwrap_or(0) {
        let Ok(dev) = coll.Item(i) else { continue };
        let Ok(id) = dev.GetId().map(|p| take_pwstr(p)) else { continue };
        let Ok(store) = dev.OpenPropertyStore(STGM_READ) else { continue };
        let text = |k: &PROPERTYKEY| store.GetValue(k).ok().map(|v| v.to_string()).filter(|s| !s.is_empty());
        let name = text(&PKEY_FRIENDLY_NAME).unwrap_or_else(|| crate::i18n::t("control.unknown_device").to_string());
        let form = store.GetValue(&PKEY_AudioEndpoint_FormFactor).ok().and_then(|v| PropVariantToUInt32(&v).ok());
        let bus = text(&PKEY_ENUMERATOR);
        out.push(Endpoint { default: def.as_deref() == Some(id.as_str()), id, name, kind: kind_of(flow, form, bus.as_deref()) });
    }
    sort(&mut out);
    out
}

/// Makes `id` the default device (all roles). Err if Windows would not.
pub unsafe fn set_default(id: &str) -> Result<()> {
    super::policy::set_default(id)
}

/// Calls `f` when a device comes, goes, changes state or name, or the
/// default changes (Core Audio's thread: `f` should only nudge).
#[implement(IMMNotificationClient)]
pub struct Watch {
    f: Box<dyn Fn() + Send + Sync>,
}

impl Watch {
    pub fn client(f: impl Fn() + Send + Sync + 'static) -> IMMNotificationClient {
        Watch { f: Box::new(f) }.into()
    }
}

impl IMMNotificationClient_Impl for Watch_Impl {
    fn OnDeviceStateChanged(&self, _: &PCWSTR, _: DEVICE_STATE) -> Result<()> {
        (self.f)();
        Ok(())
    }
    fn OnDeviceAdded(&self, _: &PCWSTR) -> Result<()> {
        (self.f)();
        Ok(())
    }
    fn OnDeviceRemoved(&self, _: &PCWSTR) -> Result<()> {
        (self.f)();
        Ok(())
    }
    fn OnDefaultDeviceChanged(&self, _: EDataFlow, role: ERole, _: &PCWSTR) -> Result<()> {
        // Once per change (it comes for every role).
        if role == eConsole {
            (self.f)();
        }
        Ok(())
    }
    fn OnPropertyValueChanged(&self, _: &PCWSTR, key: &PROPERTYKEY) -> Result<()> {
        if *key == PKEY_FRIENDLY_NAME {
            (self.f)();
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ep(id: &str, name: &str) -> Endpoint {
        Endpoint { id: id.into(), name: name.into(), kind: Kind::Speakers, default: false }
    }

    #[test]
    fn device_list_sorting() {
        let mut v = vec![ep("3", "speakers (USB)"), ep("1", "Headphones"), ep("2", "Speakers (Realtek)"), ep("0", "headphones"), ep("4", "LG TV")];
        sort(&mut v);
        let order: Vec<&str> = v.iter().map(|e| e.id.as_str()).collect();
        // By name ignoring case; equal names by id.
        assert_eq!(order, vec!["0", "1", "4", "2", "3"]);
        // Picking another default does not move anything.
        mark_default(&mut v, "2");
        let before: Vec<String> = v.iter().map(|e| e.id.clone()).collect();
        sort(&mut v);
        assert_eq!(v.iter().map(|e| e.id.clone()).collect::<Vec<_>>(), before);
        assert_eq!(v.iter().filter(|e| e.default).map(|e| e.id.as_str()).collect::<Vec<_>>(), vec!["2"]);
        mark_default(&mut v, "missing");
        assert!(v.iter().all(|e| !e.default));
    }

    #[test]
    fn kinds_by_form_factor_and_bus() {
        assert_eq!(kind_of(Flow::Output, Some(1), Some("HDAUDIO")), Kind::Speakers);
        assert_eq!(kind_of(Flow::Output, Some(3), Some("USB")), Kind::Headphones);
        assert_eq!(kind_of(Flow::Output, Some(5), None), Kind::Headset);
        assert_eq!(kind_of(Flow::Output, Some(9), Some("HDAUDIO")), Kind::Display);
        assert_eq!(kind_of(Flow::Output, Some(3), Some("BTHENUM")), Kind::Bluetooth);
        assert_eq!(kind_of(Flow::Input, Some(5), Some("BthHFEnum")), Kind::Bluetooth);
        assert_eq!(kind_of(Flow::Output, None, None), Kind::Speakers);
        assert_eq!(kind_of(Flow::Input, Some(10), None), Kind::Microphone);
        assert_eq!(kind_of(Flow::Input, Some(4), Some("USB")), Kind::Microphone);
        // Every kind has its own icon.
        let all = [Kind::Speakers, Kind::Headphones, Kind::Headset, Kind::Display, Kind::Bluetooth, Kind::Microphone];
        for (i, a) in all.iter().enumerate() {
            for b in &all[i + 1..] {
                assert_ne!(glyph(*a), glyph(*b));
            }
        }
    }
}
