//! The legacy desktop shell verb. All registry operations use an injected root.
use std::{io::Write, path::Path};
use windows::{
    core::{Result, HSTRING, PCWSTR},
    Win32::{Foundation::ERROR_FILE_NOT_FOUND, System::Registry::*},
};

const VERB: &str = r"DesktopBackground\Shell\NopBar.Widgets";
const COMMAND: &str = r"DesktopBackground\Shell\NopBar.Widgets\command";
const REQUEST: &[u8] = b"{\"type\":\"WidgetsAdd\"}\n";

struct Key(HKEY);
impl Drop for Key {
    fn drop(&mut self) {
        unsafe {
            let _ = RegCloseKey(self.0);
        }
    }
}
fn create(root: HKEY, path: &str) -> Result<Key> {
    let mut key = HKEY::default();
    unsafe {
        RegCreateKeyExW(
            root,
            &HSTRING::from(path),
            None,
            PCWSTR::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_ALL_ACCESS,
            None,
            &mut key,
            None,
        )
        .ok()?;
    }
    Ok(Key(key))
}
fn read(root: HKEY, path: &str, name: &str) -> Result<Option<String>> {
    let (path, name) = (HSTRING::from(path), HSTRING::from(name));
    let mut bytes = 0;
    unsafe {
        let status = RegGetValueW(
            root,
            &path,
            &name,
            RRF_RT_REG_SZ,
            None,
            None,
            Some(&mut bytes),
        );
        if status == ERROR_FILE_NOT_FOUND {
            return Ok(None);
        }
        status.ok()?;
        let mut value = vec![0u16; bytes as usize / 2];
        RegGetValueW(
            root,
            &path,
            &name,
            RRF_RT_REG_SZ,
            None,
            Some(value.as_mut_ptr().cast()),
            Some(&mut bytes),
        )
        .ok()?;
        Ok(Some(
            String::from_utf16_lossy(&value)
                .trim_end_matches('\0')
                .to_string(),
        ))
    }
}
fn desired(exe: &Path, label: &str) -> [(&'static str, &'static str, String); 3] {
    let exe = exe.to_string_lossy();
    [
        (VERB, "MUIVerb", label.to_string()),
        (VERB, "Icon", format!("\"{exe}\",0")),
        (COMMAND, "", format!("\"{exe}\" --widgets-add")),
    ]
}
fn sync(root: HKEY, exe: &Path, label: &str) -> Result<usize> {
    let mut changed = 0;
    for (path, name, value) in desired(exe, label) {
        if read(root, path, name)?.as_deref() == Some(&value) {
            continue;
        }
        let key = create(root, path)?;
        let wide: Vec<u16> = value.encode_utf16().chain(Some(0)).collect();
        unsafe {
            RegSetValueExW(
                key.0,
                &HSTRING::from(name),
                None,
                REG_SZ,
                Some(std::slice::from_raw_parts(
                    wide.as_ptr().cast(),
                    wide.len() * 2,
                )),
            )
            .ok()?;
        }
        changed += 1;
    }
    Ok(changed)
}
fn remove(root: HKEY) -> Result<()> {
    let status = unsafe { RegDeleteTreeW(root, &HSTRING::from(VERB)) };
    if status == ERROR_FILE_NOT_FOUND {
        Ok(())
    } else {
        status.ok()
    }
}
fn send_to(pipe: Option<impl Write>) -> bool {
    pipe.is_some_and(|mut pipe| pipe.write_all(REQUEST).and_then(|_| pipe.flush()).is_ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    struct TestRoot {
        key: Key,
        path: String,
    }
    impl TestRoot {
        fn new() -> Self {
            static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
            let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let path = format!(
                r"Software\NopBarTests\Widgets-{}-{n}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            );
            assert!(path.starts_with(r"Software\NopBarTests\"));
            Self {
                key: create(HKEY_CURRENT_USER, &path).unwrap(),
                path,
            }
        }
    }
    impl Drop for TestRoot {
        fn drop(&mut self) {
            unsafe {
                let _ = RegDeleteTreeW(HKEY_CURRENT_USER, &HSTRING::from(&self.path));
            }
        }
    }
    #[test]
    fn review_13_registration_updates_only_changed_values_under_temporary_root() {
        let root = TestRoot::new();
        let exe = Path::new(r"C:\한글 경로\NopBar\nopbar.exe");
        assert_eq!(sync(root.key.0, exe, "위젯 추가").unwrap(), 3);
        for (path, name, value) in desired(exe, "위젯 추가") {
            assert_eq!(read(root.key.0, path, name).unwrap(), Some(value));
        }
        assert_eq!(sync(root.key.0, exe, "위젯 추가").unwrap(), 0);
        assert_eq!(sync(root.key.0, exe, "Add widgets").unwrap(), 1);
        assert_eq!(
            sync(
                root.key.0,
                Path::new(r"C:\new path\nopbar.exe"),
                "Add widgets"
            )
            .unwrap(),
            2
        );
        assert_eq!(
            read(root.key.0, COMMAND, "").unwrap(),
            Some(r#""C:\new path\nopbar.exe" --widgets-add"#.into())
        );
    }
    #[test]
    fn review_13_removal_is_idempotent_and_preserves_other_verbs() {
        let root = TestRoot::new();
        sync(root.key.0, Path::new(r"C:\NopBar\nopbar.exe"), "위젯 추가").unwrap();
        let neighbor = create(root.key.0, r"DesktopBackground\Shell\OtherApp").unwrap();
        assert!(read(root.key.0, VERB, "MUIVerb").unwrap().is_some());
        remove(root.key.0).unwrap();
        assert_eq!(read(root.key.0, VERB, "MUIVerb").unwrap(), None);
        remove(root.key.0).unwrap();
        let mut reopened = HKEY::default();
        unsafe {
            RegOpenKeyExW(
                root.key.0,
                &HSTRING::from(r"DesktopBackground\Shell\OtherApp"),
                None,
                KEY_READ,
                &mut reopened,
            )
            .ok()
            .unwrap();
        }
        drop(Key(reopened));
        drop(neighbor);
    }
    #[test]
    fn review_13_absent_or_broken_pipe_never_starts_an_app() {
        assert!(!send_to(None::<Vec<u8>>));
        let mut bytes = Vec::new();
        assert!(send_to(Some(&mut bytes)));
        assert_eq!(bytes, REQUEST);
        struct Broken;
        impl Write for Broken {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                Err(std::io::ErrorKind::BrokenPipe.into())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        assert!(!send_to(Some(Broken)));
    }
}

/// A one-shot client of the existing pipe. Never launches or initializes a dock.
pub fn forward_existing() {
    let (tx, rx) = std::sync::mpsc::channel();
    let _ = std::thread::Builder::new()
        .name("widgets-add".into())
        .spawn(move || {
            let pipe = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(crate::worker::ipc::pipe_name())
                .ok();
            let _ = tx.send(send_to(pipe));
        });
    let _ = rx.recv_timeout(std::time::Duration::from_millis(250));
}

// Product roots cannot be accessed by a test binary, even via startup/settings.
#[cfg(not(test))]
fn classes(create_missing: bool) -> Result<Option<Key>> {
    let mut key = HKEY::default();
    let status = unsafe {
        RegOpenKeyExW(
            HKEY_CURRENT_USER,
            &HSTRING::from(r"Software\Classes"),
            None,
            KEY_ALL_ACCESS,
            &mut key,
        )
    };
    if status == ERROR_FILE_NOT_FOUND {
        return if create_missing {
            create(HKEY_CURRENT_USER, r"Software\Classes").map(Some)
        } else {
            Ok(None)
        };
    }
    status.ok()?;
    Ok(Some(Key(key)))
}
pub fn sync_current() {
    #[cfg(not(test))]
    {
        let result = (|| -> Result<()> {
            let exe = std::env::current_exe()?;
            if let Some(root) = classes(true)? {
                sync(root.0, &exe, crate::i18n::t("widgets.add"))?;
            }
            Ok(())
        })();
        if let Err(e) = result {
            crate::log!("widget shell registration: {e}");
        }
    }
}
pub fn remove_current() -> bool {
    #[cfg(not(test))]
    {
        classes(false)
            .and_then(|root| root.map_or(Ok(()), |root| remove(root.0)))
            .is_ok()
    }
    #[cfg(test)]
    {
        true
    }
}
