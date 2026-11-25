//! Making a device the default: IPolicyConfig, the interface Windows' own
//! Sound settings use. It is not documented, but has kept this IID and
//! vtable since Windows 7; we declare it ourselves (only
//! SetDefaultEndpoint is called, the slots before it keep the layout).
//! If it ever goes away, the caller falls back to Windows' Sound page.

#![allow(non_snake_case)]

use std::ffi::c_void;

use windows::core::{interface, IUnknown, IUnknown_Vtbl, Result, GUID, HRESULT, PCWSTR};
use windows::Win32::Foundation::PROPERTYKEY;
use windows::Win32::Media::Audio::{eCommunications, eConsole, eMultimedia, ERole};
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_ALL};

/// CLSID_PolicyConfigClient.
const CLSID_POLICY_CONFIG: GUID = GUID::from_u128(0x870af99c_171d_4f9e_af0d_e63df40c2bc9);

#[interface("f8679f50-850a-41cf-9c72-430f290290c8")]
unsafe trait IPolicyConfig: IUnknown {
    fn GetMixFormat(&self, id: PCWSTR, format: *mut *mut c_void) -> HRESULT;
    fn GetDeviceFormat(&self, id: PCWSTR, default: i32, format: *mut *mut c_void) -> HRESULT;
    fn ResetDeviceFormat(&self, id: PCWSTR) -> HRESULT;
    fn SetDeviceFormat(&self, id: PCWSTR, endpoint: *mut c_void, mix: *mut c_void) -> HRESULT;
    fn GetProcessingPeriod(&self, id: PCWSTR, default: i32, period: *mut i64, min: *mut i64) -> HRESULT;
    fn SetProcessingPeriod(&self, id: PCWSTR, period: *mut i64) -> HRESULT;
    fn GetShareMode(&self, id: PCWSTR, mode: *mut c_void) -> HRESULT;
    fn SetShareMode(&self, id: PCWSTR, mode: *mut c_void) -> HRESULT;
    fn GetPropertyValue(&self, id: PCWSTR, key: *const PROPERTYKEY, value: *mut c_void) -> HRESULT;
    fn SetPropertyValue(&self, id: PCWSTR, key: *const PROPERTYKEY, value: *mut c_void) -> HRESULT;
    fn SetDefaultEndpoint(&self, id: PCWSTR, role: ERole) -> HRESULT;
    fn SetEndpointVisibility(&self, id: PCWSTR, visible: i32) -> HRESULT;
}

/// Makes endpoint `id` the default for every role (console, multimedia,
/// communications). Err when the interface is missing or a call fails.
/// The caller's thread must have COM initialized.
pub unsafe fn set_default(id: &str) -> Result<()> {
    let policy: IPolicyConfig = CoCreateInstance(&CLSID_POLICY_CONFIG, None, CLSCTX_ALL)?;
    let wide = windows::core::HSTRING::from(id);
    for role in [eConsole, eMultimedia, eCommunications] {
        policy.SetDefaultEndpoint(PCWSTR(wide.as_ptr()), role).ok()?;
    }
    Ok(())
}
