//! The reader boundary. Tests never start a real device watcher.
use super::model::{Model, Update, REFRESH};
use std::collections::BTreeMap;
#[derive(Clone, Debug)]
pub enum Event {
    Update(Update),
    Mapped { raw: String, update: Update },
    Reset,
    Removed(String),
    Enumerated,
}
pub trait Source {
    fn drain(&mut self) -> Vec<Event>;
    fn refresh(&mut self, ids: &[String]);
}
#[derive(Default)]
pub struct Reader {
    last: BTreeMap<String, i64>,
}
impl Reader {
    pub fn tick(&mut self, source: &mut dyn Source, model: &mut Model, now: i64) -> bool {
        // Expire before draining: an offline Added must not refresh its own retention.
        let mut changed = model.expire(now);
        for event in source.drain() {
            let event = match event {
                Event::Mapped { raw, update } => {
                    changed |= model.merge_identity(&raw, &update.id);
                    self.last.remove(&raw);
                    Event::Update(update)
                }
                event => event,
            };
            match event {
                Event::Update(u) => {
                    let id = u.id.clone();
                    changed |= model.update(u, now);
                    if model.devices.get(&id).is_some_and(|d| d.percent.is_some()) {
                        self.last.insert(id, now);
                    }
                }
                Event::Removed(id) => {
                    self.last.remove(&id);
                    changed |= model.update(
                        Update {
                            id,
                            connected: Some(false),
                            ..Default::default()
                        },
                        now,
                    );
                }
                Event::Reset => {
                    self.last.clear();
                    for d in model.devices.values_mut() {
                        changed |= d.connected || d.fresh || d.disconnected_at.is_none();
                        d.connected = false;
                        d.fresh = false;
                        d.disconnected_at.get_or_insert(now);
                    }
                }
                Event::Enumerated | Event::Mapped { .. } => {}
            }
        }
        changed |= model.expire(now);
        self.last
            .retain(|id, _| model.devices.get(id).is_some_and(|d| d.percent.is_some()));
        let stale: Vec<_> = self
            .last
            .iter()
            .filter(|(_, at)| now.saturating_sub(**at) >= REFRESH)
            .map(|(id, _)| id.clone())
            .collect();
        if !stale.is_empty() {
            source.refresh(&stale);
            for id in stale {
                self.last.insert(id, now);
            }
        }
        changed
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn review_05_idle_ticks_report_no_changes_or_refreshes() {
        let mut r = Reader::default();
        let mut m = Model::default();
        let mut s = Fake {
            events: vec![Event::Update(Update {
                id: "a".into(),
                percent: Some(Some(50)),
                connected: Some(true),
                ..Default::default()
            })],
            ..Default::default()
        };
        assert!(r.tick(&mut s, &mut m, 0));
        for now in 1..300 {
            assert!(!r.tick(&mut s, &mut m, now));
        }
        assert!(s.requests.is_empty());
        s.events.push(Event::Update(Update {
            id: "a".into(),
            percent: Some(Some(49)),
            ..Default::default()
        }));
        assert!(r.tick(&mut s, &mut m, 300));
    }
    #[test]
    fn review_10_restart_invalidates_connection_and_stale_low_value() {
        let mut r = Reader::default();
        let mut m = Model::default();
        let mut s = Fake {
            events: vec![Event::Update(Update {
                id: "a".into(),
                percent: Some(Some(18)),
                connected: Some(true),
                ..Default::default()
            })],
            ..Default::default()
        };
        r.tick(&mut s, &mut m, 0);
        assert!(m.devices["a"].needs_notice());
        s.events.push(Event::Reset);
        assert!(r.tick(&mut s, &mut m, 1));
        assert!(!m.devices["a"].connected && !m.devices["a"].needs_notice());
        s.events.push(Event::Update(Update {
            id: "a".into(),
            connected: Some(true),
            ..Default::default()
        }));
        r.tick(&mut s, &mut m, 2);
        assert!(
            !m.devices["a"].needs_notice(),
            "connection alone must not announce cached 18%"
        );
        s.events.push(Event::Update(Update {
            id: "a".into(),
            percent: Some(Some(18)),
            ..Default::default()
        }));
        r.tick(&mut s, &mut m, 3);
        assert!(m.devices["a"].needs_notice());
    }
    #[test]
    fn review_01_expired_device_stays_deleted_after_refresh_and_restart() {
        let mut m = Model::default();
        let mut r = Reader::default();
        let off = Update {
            id: "old".into(),
            percent: Some(Some(18)),
            connected: Some(false),
            ..Default::default()
        };
        let mut s = Fake {
            events: vec![Event::Update(off.clone())],
            ..Default::default()
        };
        r.tick(&mut s, &mut m, 0);
        r.tick(&mut s, &mut m, super::super::model::RETENTION);
        assert!(
            !r.last.contains_key("old"),
            "expired IDs must leave the refresh set"
        );
        let bytes = serde_json::to_vec(&m).unwrap();
        let mut m: Model = serde_json::from_slice(&bytes).unwrap();
        let mut r = Reader::default();
        s.events.push(Event::Update(off.clone()));
        r.tick(&mut s, &mut m, super::super::model::RETENTION + 1);
        assert!(
            m.devices.is_empty(),
            "offline enumeration resurrected an expired device"
        );
        s.events.push(Event::Update(Update {
            connected: Some(true),
            ..off
        }));
        r.tick(&mut s, &mut m, super::super::model::RETENTION + 2);
        assert_eq!(m.devices.len(), 1);
    }
    #[test]
    fn review_03_batteryless_nodes_are_not_stored_or_refreshed() {
        let mut m = Model::default();
        let mut r = Reader::default();
        let mut s = Fake {
            events: vec![Event::Update(Update {
                id: "service".into(),
                connected: Some(true),
                ..Default::default()
            })],
            ..Default::default()
        };
        r.tick(&mut s, &mut m, 0);
        assert!(m.devices.is_empty());
        r.tick(&mut s, &mut m, 300);
        assert!(s.requests.is_empty());
    }
    #[derive(Default)]
    struct Fake {
        events: Vec<Event>,
        requests: Vec<Vec<String>>,
    }
    impl Source for Fake {
        fn drain(&mut self) -> Vec<Event> {
            std::mem::take(&mut self.events)
        }
        fn refresh(&mut self, ids: &[String]) {
            self.requests.push(ids.to_vec());
        }
    }
    #[test]
    fn only_silent_devices_are_refreshed_after_five_minutes() {
        let mut s = Fake::default();
        let mut r = Reader::default();
        let mut m = Model::default();
        for id in ["silent", "active"] {
            s.events.push(Event::Update(Update {
                id: id.into(),
                percent: Some(Some(50)),
                connected: Some(true),
                ..Default::default()
            }));
        }
        r.tick(&mut s, &mut m, 0);
        assert!(s.requests.is_empty());
        s.events.push(Event::Update(Update {
            id: "active".into(),
            percent: Some(Some(49)),
            ..Default::default()
        }));
        r.tick(&mut s, &mut m, 299);
        assert!(s.requests.is_empty());
        r.tick(&mut s, &mut m, 300);
        assert_eq!(s.requests, vec![vec!["silent".to_string()]]);
        s.events.push(Event::Removed("silent".into()));
        r.tick(&mut s, &mut m, 301);
        assert!(!m.devices["silent"].connected);
    }
}
