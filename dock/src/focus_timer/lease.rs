//! Write-ahead ownership record. All disk and COM work belongs to the DND worker.
use super::quiet_hours::{OFF, ON};
use std::{io::{self, Write}, path::Path};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Lease {
    pub started_ms: i64,
    pub deadline_ms: i64,
    pub previous: String,
    pub profile: String,
}
impl Lease {
    pub fn new(now: i64, deadline: i64) -> Self {
        Self { started_ms: now, deadline_ms: deadline, previous: OFF.into(), profile: ON.into() }
    }
    pub fn load(path: &Path) -> io::Result<Option<Self>> {
        match std::fs::read(path) {
            Ok(bytes) => {
                let lease: Self = serde_json::from_slice(&bytes)
                    .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
                // Never apply arbitrary profile strings from a corrupt journal.
                if lease.previous != OFF || lease.profile != ON || lease.deadline_ms < lease.started_ms {
                    return Err(io::Error::new(io::ErrorKind::InvalidData, "invalid DND lease"));
                }
                Ok(Some(lease))
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }
    pub fn save(&self, path: &Path) -> io::Result<()> {
        if let Some(parent) = path.parent() { std::fs::create_dir_all(parent)?; }
        let tmp = path.with_extension("json.tmp");
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(&serde_json::to_vec(self)?)?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(tmp, path)
    }
    pub fn restore<E>(&self, current: &str, mut set: impl FnMut(&str) -> Result<(), E>) -> Result<(), E> {
        if current == self.profile { set(&self.previous)?; }
        Ok(())
    }
    pub fn expired(&self, now: i64) -> bool { now >= self.deadline_ms }
    pub fn restore_and_clear(&self, path: &Path, current: &str,
        set: impl FnMut(&str) -> Result<(), String>) -> Result<(), String> {
        self.restore(current, set)?;
        clear(path).map_err(|e| e.to_string())
    }
}
pub fn acquire(owned: &mut Option<Lease>, path: &Path, now: i64, deadline: i64,
    mut set: impl FnMut(&str) -> Result<(), String>) -> Result<(), String> {
    let record = Lease::new(now, deadline);
    record.save(path).map_err(|e| e.to_string())?;
    *owned = Some(record);
    set(ON)
}
pub fn clear(path: &Path) -> io::Result<()> {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn record_precedes_os_write_and_failed_restore_survives_restart() {
        let dir = std::env::temp_dir().join(format!("nopbar-lease-{}", uuid::Uuid::new_v4()));
        let path = dir.join("lease.json"); let mut owned = None;
        acquire(&mut owned, &path, 10, 100, |s| {
            assert_eq!(s, ON);
            assert!(Lease::load(&path).unwrap().is_some(), "journal must exist before changing Windows");
            Ok(())
        }).unwrap();
        let restarted = Lease::load(&path).unwrap().unwrap();
        assert!(restarted.restore_and_clear(&path, ON, |_| Err("COM unavailable".into())).is_err());
        assert!(Lease::load(&path).unwrap().is_some());
        restarted.restore_and_clear(&path, ON, |s| {assert_eq!(s, OFF); Ok(())}).unwrap();
        assert!(Lease::load(&path).unwrap().is_none());
        restarted.save(&path).unwrap();
        restarted.restore_and_clear(&path, "Microsoft.QuietHoursProfile.AlarmsOnly", |_| panic!("user change")).unwrap();
        assert!(Lease::load(&path).unwrap().is_none());
        // A disk failure must prevent any OS write.
        std::fs::write(&path, "not a directory").unwrap();
        assert!(acquire(&mut None, &path.join("child"), 10, 100, |_| panic!("disk failed")).is_err());
        std::fs::remove_file(path).unwrap(); std::fs::remove_dir(dir).unwrap();
    }
    #[test]
    fn crash_record_roundtrips_and_restores_only_matching_profile() {
        let dir = std::env::temp_dir().join(format!("nopbar-lease-{}", uuid::Uuid::new_v4()));
        let path = dir.join("lease.json");
        let lease = Lease::new(10, 100);
        lease.save(&path).unwrap();
        let mut lease = Lease::load(&path).unwrap().unwrap();
        assert_eq!((lease.started_ms, lease.deadline_ms), (10, 100));
        lease.deadline_ms = 200; lease.save(&path).unwrap();
        assert_eq!(Lease::load(&path).unwrap(), Some(lease.clone()));
        assert!(!lease.expired(199)); assert!(lease.expired(200));
        let mut writes = Vec::new();
        for current in [ON, OFF, "Microsoft.QuietHoursProfile.AlarmsOnly"] {
            lease.restore(current, |s| {writes.push(s.to_owned()); Ok::<_, ()>(())}).unwrap();
        }
        assert_eq!(writes, [OFF]);
        assert!(lease.restore(ON, |_| Err(())).is_err());
        assert!(Lease::load(&path).unwrap().is_some(), "failed restore keeps recovery record");
        clear(&path).unwrap(); assert!(Lease::load(&path).unwrap().is_none());
        std::fs::remove_dir(dir).unwrap();
    }
    #[test]
    fn malformed_lease_does_not_overwrite_user_settings() {
        let dir = std::env::temp_dir().join(format!("nopbar-lease-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&dir).unwrap(); let path = dir.join("lease.json");
        let mut lease = Lease::new(10, 100); lease.previous = "untrusted".into();
        lease.save(&path).unwrap(); assert!(Lease::load(&path).is_err());
        std::fs::remove_file(path).unwrap(); std::fs::remove_dir(dir).unwrap();
    }
}
