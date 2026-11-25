//! Compatibility fallback: Windows exposes Focus start only through a
//! Limited Access Feature. QuietHours' COM contract is undocumented;
//! keep it isolated and return errors instead of editing CloudStore.
//! IID/vtable reference: focus-time/focus-time-app, windows-dnd/quiethours.idl.
#![allow(non_snake_case)]
use windows::core::{interface, IUnknown, IUnknown_Vtbl, GUID, HRESULT, PCWSTR, PWSTR, HSTRING, Result};
use windows::Win32::System::Com::{CoCreateInstance, CoTaskMemFree, CLSCTX_LOCAL_SERVER};

pub const OFF: &str = "Microsoft.QuietHoursProfile.Unrestricted";
pub const ON: &str = "Microsoft.QuietHoursProfile.PriorityOnly";

#[interface("6bff4732-81ec-4ffb-ae67-b6c1bc29631f")]
unsafe trait IQuietHoursSettings: IUnknown {
    fn GetUserSelectedProfile(&self, profile: *mut PWSTR) -> HRESULT;
    fn SetUserSelectedProfile(&self, profile: PCWSTR) -> HRESULT;
}

pub struct Settings(IQuietHoursSettings);
impl Settings {
    /// The caller initializes COM and keeps this interface on that thread.
    pub fn new() -> Result<Self> { unsafe {
        CoCreateInstance(&GUID::from_u128(0xf53321fa_34f8_4b7f_b9a3_361877cb94cf), None, CLSCTX_LOCAL_SERVER).map(Self)
    } }
    pub fn get(&self) -> Result<String> { unsafe {
        let mut value = PWSTR::null();
        self.0.GetUserSelectedProfile(&mut value).ok()?;
        if value.is_null() { return Err(windows::Win32::Foundation::E_FAIL.into()); }
        let result = value.to_string();
        CoTaskMemFree(Some(value.0.cast()));
        result.map_err(|_| windows::core::Error::from(windows::Win32::Foundation::E_INVALIDARG))
    } }
    pub fn set(&self, value: &str) -> Result<()> { unsafe {
        self.0.SetUserSelectedProfile(PCWSTR(HSTRING::from(value).as_ptr())).ok()
    } }
}
