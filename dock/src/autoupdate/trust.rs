//! What an automatic update trusts, and how it checks it
//! (docs/updates.md, "Trust"):
//! - the manifest's exact bytes must carry a valid ECDSA P-256 signature
//!   (`version.json.sig`, 128 hex digits r||s over SHA-256 of the bytes)
//!   by the key built into the app ([`UPDATE_PUBLIC_KEY`]); without one,
//!   updates are only announced;
//! - an Authenticode-signed running exe only takes files signed with the
//!   same public key (not the same display name);
//! - files are checked through handles that deny writing, renaming and
//!   deleting, held until the file is run or copied;
//! - the new exe's (or setup's) ProductVersion must be the manifest's.

use std::path::Path;

use super::sha256;

/// The update-signing public key: X || Y of a P-256 point, 128 hex digits.
///
/// NopBar's release key (made with `scripts/new-signing-key.ps1`; the
/// private key never enters the repo). A new key is pasted here the same
/// way. An empty string would mean "no key": automatic installs stay off
/// and updates are only announced.
pub const UPDATE_PUBLIC_KEY: &str = "23471bcc4e50454e367d5da0c07527fe264d2a7eba18a9efc6b30a7e08617e4db1dd6cfa4712c06d91230f3dfb6237dd69a11f954d86af10381b15e66fe195a2";

/// The largest `version.json.sig` read.
pub const MAX_SIG_BYTES: usize = 1024;

fn hex_bytes<const N: usize>(s: &str) -> Option<[u8; N]> {
    let s: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    if s.len() != N * 2 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let mut out = [0u8; N];
    for (i, o) in out.iter_mut().enumerate() {
        *o = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(out)
}

/// The built-in key, if one was pasted in.
pub fn embedded_key() -> Option<[u8; 64]> {
    hex_bytes::<64>(UPDATE_PUBLIC_KEY)
}

/// A BCrypt key handle, destroyed on drop.
struct Key(windows::Win32::Security::Cryptography::BCRYPT_KEY_HANDLE);

impl Drop for Key {
    fn drop(&mut self) {
        unsafe {
            let _ = windows::Win32::Security::Cryptography::BCryptDestroyKey(self.0);
        }
    }
}

fn import_public(xy: &[u8; 64]) -> Result<Key, String> {
    use windows::Win32::Security::Cryptography::{BCryptImportKeyPair, BCRYPT_ECCPUBLIC_BLOB, BCRYPT_ECDSA_P256_ALG_HANDLE, BCRYPT_ECDSA_PUBLIC_P256_MAGIC, BCRYPT_KEY_HANDLE};
    let mut blob = Vec::with_capacity(72);
    blob.extend_from_slice(&BCRYPT_ECDSA_PUBLIC_P256_MAGIC.to_le_bytes());
    blob.extend_from_slice(&32u32.to_le_bytes());
    blob.extend_from_slice(xy);
    let mut h = BCRYPT_KEY_HANDLE::default();
    let r = unsafe { BCryptImportKeyPair(BCRYPT_ECDSA_P256_ALG_HANDLE, None, BCRYPT_ECCPUBLIC_BLOB, &mut h, &blob, 0) };
    if r.is_err() {
        return Err(format!("not a P-256 public key ({:#x})", r.0));
    }
    Ok(Key(h))
}

/// `manifest` (its exact bytes) is signed by `key_hex` (X || Y, hex) with
/// `sig_text` (r || s, hex).
pub fn verify_with(manifest: &[u8], sig_text: &str, key: &[u8; 64]) -> Result<(), String> {
    use windows::Win32::Security::Cryptography::{BCryptVerifySignature, BCRYPT_FLAGS};
    let sig = hex_bytes::<64>(sig_text).ok_or("the signature is not 128 hex digits")?;
    let key = import_public(key)?;
    let hash = sha256::of(manifest)?;
    let r = unsafe { BCryptVerifySignature(key.0, None, &hash, &sig, BCRYPT_FLAGS(0)) };
    if r.is_err() {
        return Err("the manifest signature does not verify".into());
    }
    Ok(())
}

/// [`verify_with`] the built-in key; Err when there is none.
pub fn verify(manifest: &[u8], sig_text: &str) -> Result<(), String> {
    let key = embedded_key().ok_or("no update-signing key is built in")?;
    verify_with(manifest, sig_text, &key)
}

// ---------------------------------------------------------------- Authenticode publisher

/// Who signed a file: Ok(None) = no signature at all, Ok(Some) = a valid
/// signature (SHA-256 of the signing certificate's public key), Err = a
/// signature that does not verify (or the check failed).
pub fn publisher(path: &Path) -> Result<Option<[u8; 32]>, String> {
    use windows::core::HSTRING;
    use windows::Win32::Foundation::HWND;
    use windows::Win32::Security::WinTrust::*;
    const TRUST_E_NOSIGNATURE: i32 = 0x800B0100u32 as i32;
    let file = HSTRING::from(path.as_os_str());
    let mut fi = WINTRUST_FILE_INFO { cbStruct: std::mem::size_of::<WINTRUST_FILE_INFO>() as u32, pcwszFilePath: windows::core::PCWSTR(file.as_ptr()), ..Default::default() };
    let mut data = WINTRUST_DATA {
        cbStruct: std::mem::size_of::<WINTRUST_DATA>() as u32,
        dwUIChoice: WTD_UI_NONE,
        fdwRevocationChecks: WTD_REVOKE_NONE,
        dwUnionChoice: WTD_CHOICE_FILE,
        dwStateAction: WTD_STATEACTION_VERIFY,
        dwProvFlags: WTD_CACHE_ONLY_URL_RETRIEVAL,
        ..Default::default()
    };
    data.Anonymous.pFile = &mut fi;
    let mut action = WINTRUST_ACTION_GENERIC_VERIFY_V2;
    let r = unsafe { WinVerifyTrust(HWND(-1isize as *mut _), &mut action, (&mut data as *mut WINTRUST_DATA).cast()) };
    let out = if r == 0 {
        unsafe {
            let prov = WTHelperProvDataFromStateData(data.hWVTStateData);
            let sgnr = if prov.is_null() { std::ptr::null_mut() } else { WTHelperGetProvSignerFromChain(prov, 0, false, 0) };
            if sgnr.is_null() || (*sgnr).pasCertChain.is_null() || (*sgnr).csCertChain == 0 || (*(*sgnr).pasCertChain).pCert.is_null() {
                Err("signed, but the signer's certificate cannot be read".to_string())
            } else {
                let cert = &*(*(*sgnr).pasCertChain).pCert;
                let info = &*cert.pCertInfo;
                let key = &info.SubjectPublicKeyInfo.PublicKey;
                let bytes = std::slice::from_raw_parts(key.pbData, key.cbData as usize);
                sha256::of(bytes).map(Some)
            }
        }
    } else if r == TRUST_E_NOSIGNATURE {
        Ok(None)
    } else {
        Err(format!("{}: the signature does not verify ({:#x})", path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(), r as u32))
    };
    data.dwStateAction = WTD_STATEACTION_CLOSE;
    unsafe {
        let _ = WinVerifyTrust(HWND(-1isize as *mut _), &mut action, (&mut data as *mut WINTRUST_DATA).cast());
    }
    out
}

/// A signed running exe only takes a file signed with the same key; an
/// unsigned one (dev, early releases) takes any, the manifest signature
/// being the check; a running exe whose signature does not verify takes
/// nothing.
pub fn publisher_ok(mine: &Result<Option<[u8; 32]>, String>, theirs: &Result<Option<[u8; 32]>, String>) -> Result<(), String> {
    match (mine, theirs) {
        (Err(e), _) => Err(format!("this exe's own signature does not verify ({e}); not updating")),
        (Ok(None), _) => Ok(()),
        (Ok(Some(m)), Ok(Some(t))) if m == t => Ok(()),
        (Ok(Some(_)), Ok(Some(_))) => Err("the new file is signed with another key".into()),
        (Ok(Some(_)), Ok(None)) => Err("the new file is not signed (this exe is)".into()),
        (Ok(Some(_)), Err(e)) => Err(e.clone()),
    }
}

/// `file` is signed like this exe ([`publisher_ok`]).
pub fn same_publisher(file: &Path) -> Result<(), String> {
    let me = std::env::current_exe().map_err(|e| e.to_string())?;
    let (mine, theirs) = (publisher(&me), publisher(file));
    publisher_ok(&mine, &theirs)?;
    if matches!(mine, Ok(None)) {
        crate::log!("autoupdate: this build is not signed; {} is trusted through the signed manifest only", file.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default());
    }
    Ok(())
}

// ---------------------------------------------------------------- locked files

/// A file opened for reading that nobody can write, rename or delete while
/// this is held (share mode: read only), and that is a plain file, not a
/// link. Its bytes are read through this very handle.
pub struct Locked {
    pub file: std::fs::File,
}

impl Locked {
    pub fn open(path: &Path) -> Result<Locked, String> {
        use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
        const FILE_SHARE_READ: u32 = 1;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        let file = std::fs::OpenOptions::new().read(true).share_mode(FILE_SHARE_READ).custom_flags(FILE_FLAG_OPEN_REPARSE_POINT).open(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let meta = file.metadata().map_err(|e| e.to_string())?;
        if !meta.is_file() || meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(format!("{}: not a plain file", path.display()));
        }
        Ok(Locked { file })
    }

    /// All of it (at most `max` bytes).
    pub fn read_all(&mut self, max: u64) -> Result<Vec<u8>, String> {
        use std::io::{Read, Seek};
        self.file.rewind().map_err(|e| e.to_string())?;
        let mut buf = Vec::new();
        (&self.file).take(max + 1).read_to_end(&mut buf).map_err(|e| e.to_string())?;
        if buf.len() as u64 > max {
            return Err(format!("larger than {max} bytes"));
        }
        Ok(buf)
    }

    pub fn sha256(&mut self) -> Result<String, String> {
        use std::io::{Read, Seek};
        self.file.rewind().map_err(|e| e.to_string())?;
        let mut h = sha256::Sha256::new()?;
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let n = (&self.file).read(&mut buf).map_err(|e| e.to_string())?;
            if n == 0 {
                break;
            }
            h.update(&buf[..n])?;
        }
        Ok(sha256::hex(&h.finish()?))
    }
}

/// Opens `path` locked and checks it is exactly `want` (64 hex).
pub fn locked_with_hash(path: &Path, want: &str) -> Result<Locked, String> {
    let mut l = Locked::open(path)?;
    let got = l.sha256()?;
    if !got.eq_ignore_ascii_case(want.trim()) {
        return Err(format!("{} is not the checked file", path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()));
    }
    Ok(l)
}

// ---------------------------------------------------------------- versions, URLs, elevation

/// The ProductVersion string of an exe's version resource.
pub fn product_version(path: &Path) -> Option<String> {
    let v = unsafe { crate::launchers::version_string(&path.to_string_lossy(), "ProductVersion") };
    (!v.is_empty()).then_some(v)
}

/// The exe says it is `manifest_version` (same semver), newer than `current`.
pub fn version_ok(found: Option<&str>, manifest_version: &str, current: &str) -> Result<(), String> {
    let found = found.ok_or("the new file has no ProductVersion")?;
    let (f, m) = (crate::updates::Version::parse(found), crate::updates::Version::parse(manifest_version));
    if f.is_none() || f != m {
        return Err(format!("the new file is version {found:?}, the manifest says {manifest_version}"));
    }
    if !crate::updates::is_newer(found, current) {
        return Err(format!("the new file ({found}) is not newer than {current}"));
    }
    Ok(())
}

/// For dock.log: `host/…/file`, never the query or fragment (signed CDN
/// links carry tokens there).
pub fn redact_url(url: &str) -> String {
    let rest = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
    let rest = rest.split(['?', '#']).next().unwrap_or("");
    let host = rest.split('/').next().unwrap_or("");
    let host = host.rsplit('@').next().unwrap_or(host);
    match rest.rsplit_once('/') {
        Some((dir, file)) if dir != host && !file.is_empty() => format!("{host}/…/{file}"),
        Some((_, file)) if !file.is_empty() => format!("{host}/{file}"),
        _ => host.to_string(),
    }
}

/// This process runs elevated (an administrator's full token). Automatic
/// updates then stay off: their state and files are writable by the
/// unelevated user.
pub fn elevated() -> bool {
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::Security::{GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY};
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    unsafe {
        let mut token = HANDLE::default();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).is_err() {
            return true;
        }
        let mut e = TOKEN_ELEVATION::default();
        let mut len = 0u32;
        let ok = GetTokenInformation(token, TokenElevation, Some(&mut e as *mut _ as *mut _), std::mem::size_of::<TOKEN_ELEVATION>() as u32, &mut len).is_ok();
        let _ = CloseHandle(token);
        !ok || e.TokenIsElevated != 0
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use windows::Win32::Security::Cryptography::*;

    /// A throwaway P-256 key pair made for the test run only.
    pub struct TestKey(BCRYPT_KEY_HANDLE);

    impl TestKey {
        pub fn new() -> TestKey {
            let mut h = BCRYPT_KEY_HANDLE::default();
            unsafe {
                assert!(BCryptGenerateKeyPair(BCRYPT_ECDSA_P256_ALG_HANDLE, &mut h, 256, 0).is_ok());
                assert!(BCryptFinalizeKeyPair(h, 0).is_ok());
            }
            TestKey(h)
        }

        /// X || Y as hex (what new-signing-key.ps1 prints).
        pub fn public_hex(&self) -> String {
            let mut buf = [0u8; 72];
            let mut n = 0u32;
            unsafe { assert!(BCryptExportKey(self.0, None, BCRYPT_ECCPUBLIC_BLOB, Some(&mut buf), &mut n, 0).is_ok()) };
            assert_eq!(n, 72);
            sha256::hex(&buf[8..])
        }

        /// r || s as hex (what sign-manifest.ps1 writes).
        pub fn sign(&self, data: &[u8]) -> String {
            let hash = sha256::of(data).unwrap();
            let mut sig = [0u8; 64];
            let mut n = 0u32;
            unsafe { assert!(BCryptSignHash(self.0, None, &hash, Some(&mut sig), &mut n, BCRYPT_FLAGS(0)).is_ok()) };
            assert_eq!(n, 64);
            sha256::hex(&sig)
        }
    }

    impl Drop for TestKey {
        fn drop(&mut self) {
            unsafe {
                let _ = BCryptDestroyKey(self.0);
            }
        }
    }

    #[test]
    fn manifest_signatures_verify_only_for_the_exact_bytes_and_key() {
        let k = TestKey::new();
        let key = hex_bytes::<64>(&k.public_hex()).unwrap();
        let m = br#"{"version":"0.2.0","url":"https://nopbard.com/nopbar/download"}"#;
        let sig = k.sign(m);
        assert_eq!(verify_with(m, &sig, &key), Ok(()));
        assert_eq!(verify_with(m, &format!("  {}\r\n", sig.to_uppercase()), &key), Ok(()), "case and line ends do not matter");
        // One byte changed (even whitespace): refused.
        let mut t = m.to_vec();
        t.push(b'\n');
        assert!(verify_with(&t, &sig, &key).is_err());
        let t = br#"{"version":"9.2.0","url":"https://nopbard.com/nopbar/download"}"#;
        assert!(verify_with(t, &sig, &key).is_err());
        // Another key: refused.
        let other = TestKey::new();
        assert!(verify_with(m, &other.sign(m), &key).is_err());
        // Malformed signature or key.
        assert!(verify_with(m, "abc", &key).is_err());
        assert!(verify_with(m, &"0".repeat(128), &key).is_err());
        assert!(verify_with(m, &sig, &[0u8; 64]).is_err(), "not a point on the curve");
    }

    #[test]
    fn the_built_in_key_is_the_placeholder_or_a_real_point() {
        if UPDATE_PUBLIC_KEY.is_empty() {
            // The placeholder: automatic installs stay off.
            assert_eq!(embedded_key(), None);
        } else {
            let key = embedded_key().expect("UPDATE_PUBLIC_KEY must be 128 hex digits (new-signing-key.ps1 prints it)");
            assert!(import_public(&key).is_ok(), "UPDATE_PUBLIC_KEY is not a P-256 point");
        }
        assert!(verify(b"{}", &"0".repeat(128)).is_err());
    }

    #[test]
    fn publishers_compare_by_key() {
        let a = Ok(Some([1u8; 32]));
        let b = Ok(Some([2u8; 32]));
        let none: Result<Option<[u8; 32]>, String> = Ok(None);
        let bad: Result<Option<[u8; 32]>, String> = Err("tampered".into());
        assert!(publisher_ok(&a, &a).is_ok());
        assert!(publisher_ok(&a, &b).is_err(), "same name, other key");
        assert!(publisher_ok(&a, &none).is_err());
        assert!(publisher_ok(&a, &bad).is_err());
        assert!(publisher_ok(&none, &none).is_ok(), "unsigned build: the manifest signature decides");
        assert!(publisher_ok(&none, &b).is_ok());
        assert!(publisher_ok(&bad, &a).is_err(), "a broken own signature is not 'unsigned'");
    }

    #[test]
    fn versions_must_match_the_manifest() {
        assert!(version_ok(Some("0.2.0"), "0.2.0", "0.1.0").is_ok());
        assert!(version_ok(Some("0.2.0"), "v0.2.0+b1", "0.1.0").is_ok());
        assert!(version_ok(Some("0.1.5"), "0.2.0", "0.1.0").is_err(), "an older build under a newer number");
        assert!(version_ok(Some("0.2.0"), "0.2.0", "0.2.0").is_err());
        assert!(version_ok(Some("0.2.0.0"), "0.2.0", "0.1.0").is_err(), "not the semver string");
        assert!(version_ok(None, "0.2.0", "0.1.0").is_err());
    }

    #[test]
    fn urls_are_logged_without_queries() {
        assert_eq!(redact_url("https://github.com/o/r/releases/download/v1/NopBar-1-setup.exe?token=SECRET#x"), "github.com/…/NopBar-1-setup.exe");
        assert_eq!(redact_url("https://nopbard.com/version.json?sig=abc"), "nopbard.com/version.json");
        assert_eq!(redact_url("https://user:pw@a.com/x/y.zip"), "a.com/…/y.zip");
        assert_eq!(redact_url("https://a.com?t=1"), "a.com");
        assert!(!redact_url("https://a.com/p?q=SECRET").contains("SECRET"));
    }

    #[test]
    fn locked_files_cannot_be_swapped() {
        let s = crate::autoupdate::testutil::Scratch::new("trust-lock");
        let p = s.0.join("setup.exe");
        std::fs::write(&p, b"the checked bytes").unwrap();
        let want = sha256::hex(&sha256::of(b"the checked bytes").unwrap());
        let mut l = locked_with_hash(&p, &want).unwrap();
        // While held: no write, no rename, no delete.
        assert!(std::fs::OpenOptions::new().write(true).open(&p).is_err());
        assert!(std::fs::rename(&p, s.0.join("moved.exe")).is_err());
        assert!(std::fs::remove_file(&p).is_err());
        // Others may still read (the loader does).
        assert_eq!(std::fs::read(&p).unwrap(), b"the checked bytes");
        assert_eq!(l.read_all(100).unwrap(), b"the checked bytes");
        assert!(l.read_all(3).is_err());
        drop(l);
        std::fs::write(&p, b"swapped").unwrap();
        assert!(locked_with_hash(&p, &want).is_err());
        assert!(Locked::open(&s.0).is_err(), "a folder is not a plain file");
    }
}
