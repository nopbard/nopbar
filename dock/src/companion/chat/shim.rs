use std::{
    io,
    path::{Path, PathBuf},
};
pub fn target() -> Result<PathBuf, String> {
    Ok(
        PathBuf::from(std::env::var_os("LOCALAPPDATA").ok_or(crate::i18n::t("chat.error.local_directory"))?)
            .join("NopBar/mcp/nopbar-mcp.exe"),
    )
}
fn deploy(
    source: &Path,
    target: &Path,
    protocol: &str,
    replace: impl FnOnce(&Path, &Path) -> io::Result<()>,
) -> io::Result<()> {
    let marker = target.with_extension("protocol");
    if target.is_file() && std::fs::read_to_string(&marker).is_ok_and(|v| v == protocol) {
        return Ok(());
    }
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let temp = target.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
    let result = std::fs::copy(source, &temp).and_then(|_| replace(&temp, target));
    if let Err(e) = result {
        let _ = std::fs::remove_file(&temp);
        // A running previous protocol remains usable; retry on the next enable.
        return if target.is_file() { Ok(()) } else { Err(e) };
    }
    std::fs::write(marker, protocol)
}
pub fn install() -> Result<PathBuf, String> {
    let source = std::env::current_exe()
        .map_err(|_| crate::i18n::t("chat.error.install_directory"))?
        .with_file_name("nopbar-mcp.exe");
    let target = target()?;
    deploy(&source, &target, nopbar_mcp::PROTOCOL, |a, b| {
        std::fs::rename(a, b)
    })
    .map_err(|_| crate::i18n::t("chat.error.mcp_binary"))?;
    Ok(target)
}
pub fn maintain() {
    // An enabled installation retries a previously locked protocol upgrade on
    // the next app start. This touches only our shim, never CLI configuration.
    std::thread::spawn(|| {
        if let Ok(_lock) = super::credentials::Lock::named("mcp-config") {
            let _ = install();
        }
    });
}
fn remove_with(
    path: &Path,
    delete: impl FnOnce(&Path) -> io::Result<()>,
    schedule: impl FnOnce(&Path) -> io::Result<()>,
) -> io::Result<()> {
    match delete(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(_) => schedule(path),
    }
}
pub fn remove() -> Result<(), String> {
    let path = target()?;
    remove_with(
        &path,
        |p| std::fs::remove_file(p),
        |p| unsafe {
            windows::Win32::Storage::FileSystem::MoveFileExW(
                &windows::core::HSTRING::from(p.to_string_lossy().as_ref()),
                None,
                windows::Win32::Storage::FileSystem::MOVEFILE_DELAY_UNTIL_REBOOT,
            )
            .map_err(|e| io::Error::other(e.to_string()))
        },
    )
    .map_err(|_| crate::i18n::t("chat.error.mcp_cleanup"))?;
    let _ = std::fs::remove_file(path.with_extension("protocol"));
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn copies_only_on_protocol_change_and_keeps_locked_shim() {
        let root = std::env::temp_dir().join(format!("nopbar-shim-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let source = root.join("source.exe");
        let target = root.join("shim.exe");
        std::fs::write(&source, b"v1").unwrap();
        deploy(&source, &target, "p1", |a, b| std::fs::rename(a, b)).unwrap();
        std::fs::write(&source, b"v2").unwrap();
        deploy(&source, &target, "p1", |_, _| {
            panic!("same protocol never replaces")
        })
        .unwrap();
        deploy(&source, &target, "p2", |_, _| {
            Err(io::ErrorKind::PermissionDenied.into())
        })
        .unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"v1");
        assert_eq!(
            std::fs::read_to_string(target.with_extension("protocol")).unwrap(),
            "p1"
        );
        deploy(&source, &target, "p2", |a, b| std::fs::rename(a, b)).unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"v2");
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 3);
        let mut scheduled = false;
        remove_with(
            &target,
            |_| Err(io::ErrorKind::PermissionDenied.into()),
            |_| {
                scheduled = true;
                Ok(())
            },
        )
        .unwrap();
        assert!(scheduled);
        for file in [source, target.clone(), target.with_extension("protocol")] {
            std::fs::remove_file(file).unwrap();
        }
        std::fs::remove_dir(root).unwrap();
    }
}
