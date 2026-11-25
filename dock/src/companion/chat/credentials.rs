//! Generic credentials are per Windows user. Large rotating token sets are
//! chunked into a fresh generation; a small manifest switches atomically last.
//! No credential content appears in errors, Debug, config, or logs.
use serde::{Deserialize, Serialize};
use windows::{
    core::HSTRING,
    Win32::{Foundation::ERROR_NOT_FOUND, Security::Credentials::*},
};
pub type Result<T> = std::result::Result<T, &'static str>;
const PREFIX: &str = "NopBar/Companion/SIWC/";
pub trait Vault {
    fn read(&self, key: &str) -> Result<Option<Vec<u8>>>;
    fn write(&self, key: &str, data: &[u8]) -> Result<()>;
    fn delete(&self, key: &str) -> Result<()>;
}
pub struct WindowsVault;
impl Vault for WindowsVault {
    fn read(&self, key: &str) -> Result<Option<Vec<u8>>> {
        unsafe {
            let mut p = std::ptr::null_mut();
            if let Err(e) = CredReadW(
                &HSTRING::from(format!("{PREFIX}{key}")),
                CRED_TYPE_GENERIC,
                None,
                &mut p,
            ) {
                return if e.code() == ERROR_NOT_FOUND.to_hresult() {
                    Ok(None)
                } else {
                    Err(crate::i18n::t("chat.error.credentials_read"))
                };
            }
            let bytes =
                std::slice::from_raw_parts((*p).CredentialBlob, (*p).CredentialBlobSize as usize)
                    .to_vec();
            CredFree(p.cast());
            Ok(Some(bytes))
        }
    }
    fn write(&self, key: &str, data: &[u8]) -> Result<()> {
        unsafe {
            let mut name = format!("{PREFIX}{key}")
                .encode_utf16()
                .chain(Some(0))
                .collect::<Vec<_>>();
            let c = CREDENTIALW {
                Type: CRED_TYPE_GENERIC,
                TargetName: windows::core::PWSTR(name.as_mut_ptr()),
                CredentialBlobSize: data.len() as u32,
                CredentialBlob: data.as_ptr() as *mut u8,
                Persist: CRED_PERSIST_LOCAL_MACHINE,
                ..Default::default()
            };
            CredWriteW(&c, 0).map_err(|_| crate::i18n::t("chat.error.credentials_save"))
        }
    }
    fn delete(&self, key: &str) -> Result<()> {
        unsafe {
            match CredDeleteW(
                &HSTRING::from(format!("{PREFIX}{key}")),
                CRED_TYPE_GENERIC,
                None,
            ) {
                Ok(()) => Ok(()),
                Err(e) if e.code() == ERROR_NOT_FOUND.to_hresult() => Ok(()),
                Err(_) => Err(crate::i18n::t("chat.error.credentials_delete")),
            }
        }
    }
}
#[derive(Serialize, Deserialize)]
struct Manifest {
    generation: String,
    chunks: usize,
}
fn manifest(v: &impl Vault, key: &str) -> Result<Option<Manifest>> {
    v.read(key)?
        .map(|b| serde_json::from_slice(&b).map_err(|_| crate::i18n::t("chat.error.credentials_reconnect")))
        .transpose()
}
pub fn load<T: serde::de::DeserializeOwned>(v: &impl Vault, key: &str) -> Result<Option<T>> {
    let Some(m) = manifest(v, key)? else {
        return Ok(None);
    };
    if m.chunks > 64 {
        return Err(crate::i18n::t("chat.error.credentials_invalid"));
    }
    let mut all = Vec::new();
    for i in 0..m.chunks {
        all.extend(
            v.read(&format!("{key}/{}/{i}", m.generation))?
                .ok_or(crate::i18n::t("chat.error.credentials_invalid"))?,
        );
    }
    serde_json::from_slice(&all)
        .map(Some)
        .map_err(|_| crate::i18n::t("chat.error.credentials_invalid"))
}
pub fn save<T: Serialize>(v: &impl Vault, key: &str, value: &T) -> Result<()> {
    let old = manifest(v, key)?;
    let bytes = serde_json::to_vec(value).map_err(|_| crate::i18n::t("chat.error.credentials_save"))?;
    let m = Manifest {
        generation: uuid::Uuid::new_v4().to_string(),
        chunks: bytes.len().div_ceil(2400),
    };
    if m.chunks > 64 {
        return Err(crate::i18n::t("chat.error.credentials_large"));
    }
    for (i, chunk) in bytes.chunks(2400).enumerate() {
        if let Err(e) = v.write(&format!("{key}/{}/{i}", m.generation), chunk) {
            for j in 0..i {
                let _ = v.delete(&format!("{key}/{}/{j}", m.generation));
            }
            return Err(e);
        }
    }
    let data = serde_json::to_vec(&m).map_err(|_| crate::i18n::t("chat.error.credentials_save"))?;
    if let Err(e) = v.write(key, &data) {
        for i in 0..m.chunks {
            let _ = v.delete(&format!("{key}/{}/{i}", m.generation));
        }
        return Err(e);
    }
    if let Some(old) = old {
        for i in 0..old.chunks {
            let _ = v.delete(&format!("{key}/{}/{i}", old.generation));
        }
    }
    Ok(())
}
#[cfg(test)]
pub fn delete(v: &impl Vault, key: &str) -> Result<()> {
    if let Some(m) = manifest(v, key)? {
        v.delete(key)?;
        for i in 0..m.chunks {
            v.delete(&format!("{key}/{}/{i}", m.generation))?;
        }
    }
    Ok(())
}
pub fn purge() -> Result<()> {
    unsafe {
        let mut count = 0;
        let mut entries = std::ptr::null_mut();
        if let Err(e) = CredEnumerateW(
            &HSTRING::from(format!("{PREFIX}*")),
            None,
            &mut count,
            &mut entries,
        ) {
            return if e.code() == ERROR_NOT_FOUND.to_hresult() {
                Ok(())
            } else {
                Err(crate::i18n::t("chat.error.credentials_delete"))
            };
        }
        let mut ok = true;
        for i in 0..count as usize {
            let c = *entries.add(i);
            ok &= CredDeleteW(
                windows::core::PCWSTR((*c).TargetName.0),
                CRED_TYPE_GENERIC,
                None,
            )
            .is_ok();
        }
        CredFree(entries.cast());
        if ok {
            Ok(())
        } else {
            Err(crate::i18n::t("chat.error.credentials_clear"))
        }
    }
}
/// Cross-process serialization covers read/refresh/replace and sign-out.
pub struct Lock(windows::Win32::Foundation::HANDLE);
impl Lock {
    pub fn take() -> Result<Self> {
        Self::named("siwc")
    }
    pub fn named(name: &str) -> Result<Self> {
        unsafe {
            use windows::Win32::System::Threading::*;
            let h = CreateMutexW(
                None,
                false,
                &HSTRING::from(format!(
                    "Local\\{}.{}",
                    crate::worker::ipc::pipe_name().replace('\\', "_"),
                    name
                )),
            )
            .map_err(|_| crate::i18n::t("chat.error.connection_lock"))?;
            let r = WaitForSingleObject(h, 30_000);
            if r.0 == 0 || r.0 == 0x80 {
                Ok(Self(h))
            } else {
                let _ = windows::Win32::Foundation::CloseHandle(h);
                Err(crate::i18n::t("chat.error.connection_busy"))
            }
        }
    }
}
impl Drop for Lock {
    fn drop(&mut self) {
        unsafe {
            let _ = windows::Win32::System::Threading::ReleaseMutex(self.0);
            let _ = windows::Win32::Foundation::CloseHandle(self.0);
        }
    }
}
#[cfg(test)]
pub mod tests {
    use super::*;
    use std::{cell::RefCell, collections::HashMap};
    #[derive(Default)]
    pub struct Fake(pub RefCell<HashMap<String, Vec<u8>>>);
    impl Vault for Fake {
        fn read(&self, k: &str) -> Result<Option<Vec<u8>>> {
            Ok(self.0.borrow().get(k).cloned())
        }
        fn write(&self, k: &str, v: &[u8]) -> Result<()> {
            self.0.borrow_mut().insert(k.into(), v.into());
            Ok(())
        }
        fn delete(&self, k: &str) -> Result<()> {
            self.0.borrow_mut().remove(k);
            Ok(())
        }
    }
    #[test]
    fn oversized_tokens_roundtrip_rotate_and_delete() {
        let v = Fake::default();
        let token = "secret".repeat(3000);
        save(&v, "account", &token).unwrap();
        assert_eq!(load::<String>(&v, "account").unwrap(), Some(token));
        save(&v, "account", &"replacement").unwrap();
        assert_eq!(v.0.borrow().len(), 2);
        delete(&v, "account").unwrap();
        assert!(v.0.borrow().is_empty());
    }
}
