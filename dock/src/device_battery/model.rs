use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub const RETENTION: i64 = 7 * 24 * 60 * 60;
pub const REFRESH: i64 = 300;
/// A device seen connected this recently still shows, dimmed, with its
/// last level (Logitech BLE devices drop off while they rest, or are used
/// through their own receiver for a while).
pub const RECENT: i64 = 24 * 60 * 60;

/// Now (unix seconds).
pub fn clock() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs() as i64
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub enum Kind {
    Headphones,
    Mouse,
    Keyboard,
    Gamepad,
    #[default]
    Other,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Device {
    pub id: String,
    pub name: String,
    pub kind: Kind,
    pub percent: Option<u8>,
    pub connected: bool,
    pub charging: bool,
    pub last_seen: i64,
    pub disconnected_at: Option<i64>,
    pub notified: bool,
    /// A battery sample received while connected in this process.
    #[serde(skip)]
    pub fresh: bool,
}

impl Device {
    /// Connected now, or seen connected less than RECENT before `now`.
    pub fn recent(&self, now: i64) -> bool {
        self.connected || (self.last_seen > 0 && now.saturating_sub(self.last_seen) < RECENT)
    }
    pub fn needs_notice(&self) -> bool {
        self.connected
            && self.fresh
            && !self.charging
            && !self.notified
            && self.percent.is_some_and(|p| p <= 20)
    }
}

/// None means absent in an incremental Updated event, not zero/false.
#[derive(Clone, Debug, Default)]
pub struct Update {
    pub id: String,
    pub name: Option<String>,
    pub kind: Option<Kind>,
    pub percent: Option<Option<u8>>,
    pub connected: Option<bool>,
    pub charging: Option<bool>,
}

pub use crate::desklayer::widget::Size;
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Desktop {
    pub size: Size,
    pub place: Option<crate::desklayer::place::Place>,
    pub hidden: Vec<String>,
    pub names: BTreeMap<String, String>,
    pub closed: bool,
}
impl Desktop {
    pub fn name<'a>(&'a self, d: &'a Device) -> &'a str {
        self.names.get(&d.id).map(String::as_str).unwrap_or(&d.name)
    }
    pub fn rename(&mut self, id: &str, text: &str) {
        let name: String = text
            .trim()
            .chars()
            .filter(|c| !c.is_control())
            .take(80)
            .collect();
        if name.is_empty() {
            self.names.remove(id);
        } else {
            self.names.insert(id.into(), name);
        }
    }
    pub fn toggle(&mut self, id: &str) {
        if self.hidden.iter().any(|s| s == id) {
            self.hidden.retain(|s| s != id);
        } else {
            self.hidden.push(id.into());
        }
    }
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Model {
    pub devices: BTreeMap<String, Device>,
    pub desktops: BTreeMap<String, Desktop>,
    pub expired: BTreeSet<String>,
}
impl Model {
    /// Migrate a saved pre-container identity without duplicating preferences or delivery latches.
    pub fn merge_identity(&mut self, raw: &str, id: &str) -> bool {
        if raw == id {
            return false;
        }
        let mut changed = false;
        if let Some(mut old) = self.devices.remove(raw) {
            changed = true;
            old.id = id.into();
            self.devices
                .entry(id.into())
                .and_modify(|d| d.notified |= old.notified)
                .or_insert(old);
        }
        if self.expired.remove(raw) {
            if !self.devices.contains_key(id) {
                self.expired.insert(id.into());
            }
            changed = true;
        }
        for prefs in self.desktops.values_mut() {
            if prefs.hidden.iter().any(|x| x == raw) {
                prefs.hidden.retain(|x| x != raw);
                if !prefs.hidden.iter().any(|x| x == id) {
                    prefs.hidden.push(id.into());
                }
                changed = true;
            }
            if let Some(name) = prefs.names.remove(raw) {
                prefs.names.entry(id.into()).or_insert(name);
                changed = true;
            }
        }
        changed
    }
    /// True only when persisted/displayed state changes; notification delivery is separate.
    pub fn update(&mut self, u: Update, now: i64) -> bool {
        if self.expired.contains(&u.id) && u.connected != Some(true) {
            return false;
        }
        if !self.devices.contains_key(&u.id) && !matches!(u.percent, Some(Some(0..=100))) {
            return false;
        }
        let removed_tombstone = self.expired.remove(&u.id);
        let existed = self.devices.contains_key(&u.id);
        let d = self.devices.entry(u.id.clone()).or_insert_with(|| Device {
            id: u.id,
            ..Default::default()
        });
        let before = d.clone();
        if let Some(n) = u.name {
            d.name = n;
        }
        if let Some(k) = u.kind {
            d.kind = k;
        }
        if let Some(p) = u.percent {
            d.percent = p.filter(|p| *p <= 100);
        }
        if let Some(c) = u.charging {
            d.charging = c;
        }
        if let Some(c) = u.connected {
            d.connected = c;
        }
        if d.connected {
            d.last_seen = now;
            d.disconnected_at = None;
            if u.percent.is_some() {
                d.fresh = d.percent.is_some();
            }
        } else {
            d.disconnected_at.get_or_insert(now);
            d.fresh = false;
        }
        if d.fresh && d.percent.is_some_and(|p| p > 20) {
            d.notified = false;
        }
        !existed || removed_tombstone || *d != before
    }
    pub fn acknowledge(&mut self, id: &str) -> bool {
        let Some(d) = self.devices.get_mut(id).filter(|d| d.needs_notice()) else {
            return false;
        };
        d.notified = true;
        true
    }
    pub fn expire(&mut self, now: i64) -> bool {
        let mut changed = false;
        self.devices.retain(|id, d| {
            let keep = d.connected
                || now.saturating_sub(d.disconnected_at.unwrap_or(d.last_seen)) < RETENTION;
            if !keep {
                self.expired.insert(id.clone());
                changed = true;
            }
            keep
        });
        changed
    }
    pub fn rows(&self, desktop: &Desktop) -> Vec<&Device> {
        let mut rows: Vec<_> = self
            .devices
            .values()
            .filter(|d| d.percent.is_some() && !desktop.hidden.contains(&d.id))
            .collect();
        rows.sort_by(|a, b| {
            (!a.connected, a.percent, &a.id).cmp(&(!b.connected, b.percent, &b.id))
        });
        rows
    }
    pub fn load(path: &std::path::Path) -> std::io::Result<Self> {
        match std::fs::read(path) {
            Ok(bytes) => match serde_json::from_slice(&bytes) {
                Ok(model) => Ok(model),
                Err(_) => {
                    // Never overwrite an earlier recovery copy.
                    let mut backup = path.with_extension("json.bak");
                    if backup.exists() {
                        backup = path.with_extension(format!("{}.bak", uuid::Uuid::new_v4()));
                    }
                    std::fs::rename(path, backup)?;
                    Ok(Self::default())
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e),
        }
    }
    pub fn save(&self, path: &std::path::Path) -> std::io::Result<()> {
        crate::desklayer::disk::write_atomic(path, &serde_json::to_vec_pretty(self)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn review_02_container_migration_preserves_names_visibility_and_delivery() {
        let mut m = Model::default();
        m.update(update("profile", Some(18)), 1);
        m.acknowledge("profile");
        let prefs = m.desktops.entry("desk".into()).or_default();
        prefs.rename("profile", "mine");
        prefs.toggle("profile");
        assert!(m.merge_identity("profile", "container"));
        assert!(!m.devices.contains_key("profile"));
        assert!(m.devices["container"].notified);
        assert_eq!(m.desktops["desk"].names["container"], "mine");
        assert_eq!(m.desktops["desk"].hidden, ["container"]);
        assert!(!m.merge_identity("profile", "container"));
    }
    #[test]
    fn review_07_corrupt_json_is_preserved_and_defaults_load() {
        let dir =
            std::env::temp_dir().join(format!("nopbar-battery-corrupt-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("device-battery.json");
        std::fs::write(&path, b"{broken").unwrap();
        let result = Model::load(&path);
        assert!(result.is_ok(), "corrupt JSON must recover: {result:?}");
        assert_eq!(
            std::fs::read(path.with_extension("json.bak")).unwrap(),
            b"{broken"
        );
        result.unwrap().save(&path).unwrap();
        assert!(Model::load(&path).unwrap().devices.is_empty());
        std::fs::remove_file(&path).unwrap();
        std::fs::remove_file(path.with_extension("json.bak")).unwrap();
        std::fs::remove_dir(&dir).unwrap();
    }
    /// A file saved by 0.1.97 (every device "connected": false, the
    /// keyboard last seen 3 hours ago): it loads as it was, and what was
    /// seen in the last day shows (dimmed) instead of "연결 안 됨".
    #[test]
    fn an_old_save_file_keeps_recent_devices_on_the_card() {
        let json = r#"{
  "devices": {
    "container:{11111111-1111-4111-8111-111111111111}": { "id": "container:{11111111-1111-4111-8111-111111111111}", "name": "Wireless Keyboard", "kind": "Keyboard", "percent": 50, "connected": false, "charging": false, "last_seen": 100000, "disconnected_at": 100100, "notified": false },
    "container:{22222222-2222-4222-8222-222222222222}": { "id": "container:{22222222-2222-4222-8222-222222222222}", "name": "Wireless Mouse", "kind": "Mouse", "percent": 95, "connected": false, "charging": false, "last_seen": 20000, "disconnected_at": 20000, "notified": false }
  },
  "desktops": { "": { "size": { "w": 2, "h": 1 }, "hidden": [], "names": {}, "closed": false } },
  "expired": []
}"#;
        let dir = std::env::temp_dir().join(format!("nopbar-battery-old-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("device-battery.json");
        std::fs::write(&path, json).unwrap();
        let m = Model::load(&path).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(m.devices.len(), 2);
        let prefs = m.desktops.get("").cloned().unwrap_or_default();
        let rows = m.rows(&prefs);
        let now = 100000 + 3 * 3600;
        let shown = crate::device_battery::paint::shown_at(&rows, now);
        assert_eq!(shown.iter().map(|d| d.name.as_str()).collect::<Vec<_>>(), ["Wireless Keyboard"], "the keyboard (3 h); the mouse is over a day");
        assert!(crate::device_battery::paint::shown_at(&rows, 100000 + RECENT).is_empty());
    }
    #[test]
    fn review_09_unsent_low_notification_does_not_consume_latch() {
        let mut m = Model::default();
        m.update(update("a", Some(18)), 1);
        assert!(
            !m.devices["a"].notified,
            "disabled/hidden/deferred alerts have not been delivered"
        );
    }
    pub fn update(id: &str, p: Option<u8>) -> Update {
        Update {
            id: id.into(),
            name: Some(id.into()),
            percent: Some(p),
            connected: Some(true),
            ..Default::default()
        }
    }
    #[test]
    fn low_once_and_rearm_strictly_above_twenty() {
        let mut m = Model::default();
        m.update(update("a", Some(21)), 1);
        assert!(!m.devices["a"].needs_notice());
        m.update(update("a", Some(20)), 2);
        assert!(m.devices["a"].needs_notice());
        assert!(m.acknowledge("a"));
        for p in [18, 20, 19] {
            m.update(update("a", Some(p)), 3);
            assert!(!m.devices["a"].needs_notice());
        }
        m.update(update("a", Some(21)), 4);
        assert!(!m.devices["a"].needs_notice());
        m.update(update("a", Some(20)), 5);
        assert!(m.devices["a"].needs_notice());
    }
    #[test]
    fn partial_updates_missing_invalid_and_sorting() {
        let mut m = Model::default();
        for (id, p) in [
            ("a", Some(91)),
            ("b", Some(18)),
            ("c", None),
            ("d", Some(255)),
        ] {
            m.update(update(id, p), 1);
        }
        m.update(
            Update {
                id: "b".into(),
                name: Some("renamed".into()),
                ..Default::default()
            },
            2,
        );
        assert_eq!(
            m.rows(&Desktop::default())
                .iter()
                .map(|d| d.id.as_str())
                .collect::<Vec<_>>(),
            ["b", "a"]
        );
        assert_eq!(m.devices["b"].percent, Some(18));
    }
    #[test]
    fn disconnected_expires_after_seven_days_without_extending_on_updates() {
        let mut m = Model::default();
        m.update(update("a", Some(18)), 1);
        let off = Update {
            id: "a".into(),
            connected: Some(false),
            ..Default::default()
        };
        m.update(off.clone(), 10);
        m.update(off, 100);
        m.expire(10 + RETENTION - 1);
        assert_eq!(m.devices.len(), 1);
        m.expire(10 + RETENTION);
        assert!(m.devices.is_empty());
    }
    #[test]
    fn selection_names_sizes_and_latches_survive_disk() {
        let dir = std::env::temp_dir().join(format!("nopbar-device-{}", uuid::Uuid::new_v4()));
        let path = dir.join("battery.json");
        let mut m = Model::default();
        m.update(update("a", Some(18)), 1);
        m.acknowledge("a");
        let d = m.desktops.entry("desk".into()).or_default();
        d.toggle("a");
        d.rename("a", "  이어폰  ");
        d.size = Size::MEDIUM;
        m.save(&path).unwrap();
        let loaded = Model::load(&path).unwrap();
        assert_eq!(loaded.desktops, m.desktops);
        assert!(loaded.devices["a"].notified);
        assert!(loaded.rows(&loaded.desktops["desk"]).is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn disconnected_low_does_not_notify_and_reconnect_keeps_the_latch() {
        let mut m = Model::default();
        let mut u = update("a", Some(18));
        u.connected = Some(false);
        m.update(u, 1);
        assert!(!m.devices["a"].needs_notice());
        m.update(update("a", Some(18)), 2);
        assert!(m.devices["a"].needs_notice());
        m.acknowledge("a");
        m.update(
            Update {
                id: "a".into(),
                connected: Some(false),
                ..Default::default()
            },
            3,
        );
        m.update(update("a", Some(17)), 4);
        assert!(!m.devices["a"].needs_notice());
    }
    #[test]
    fn desktop_choices_are_independent_and_names_are_bounded() {
        let mut m = Model::default();
        m.update(update("a", Some(50)), 0);
        let a = m.desktops.entry("a".into()).or_default();
        a.toggle("a");
        a.rename("a", &"x".repeat(200));
        assert_eq!(a.names["a"].len(), 80);
        assert_eq!(m.rows(&Desktop::default()).len(), 1);
        assert!(m.rows(&m.desktops["a"]).is_empty());
        let a = m.desktops.get_mut("a").unwrap();
        a.toggle("a");
        a.rename("a", " \n ");
        assert!(a.names.is_empty());
        assert_eq!(m.rows(&m.desktops["a"]).len(), 1);
    }
}
