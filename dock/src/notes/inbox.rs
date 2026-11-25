//! Durable inbox and note-line reminders. All writes use the notes Disk contract.
use super::store::{Disk, RECOVERY, RESCUE};
use serde::{Deserialize, Serialize};
pub const FILE: &str = "inbox.json";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Row {
    pub id: String,
    pub text: String,
    pub created: i64,
    pub remind: Option<i64>,
    pub completed: Option<i64>,
    pub desktop: Option<String>,
    #[serde(default)]
    pub lead: u8,
    #[serde(default, alias = "notified")]
    pub delivered: bool,
    #[serde(default)]
    pub missed: bool,
    #[serde(default)]
    pub snoozed: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub briefing_day: Option<i64>,
    /// A note file and the original Markdown line. None means inbox.
    #[serde(default)]
    pub source: Option<(String, String)>,
}
impl Row {
    pub fn revise(&mut self, text: String, remind: Option<i64>, lead: u8) {
        self.text = text;
        self.remind = remind;
        self.lead = lead;
        self.delivered = false;
        self.missed = false;
        self.snoozed = None;
        if remind.is_some() {
            self.briefing_day = None;
        }
    }
    pub fn new(
        text: String,
        now: i64,
        remind: Option<i64>,
        lead: u8,
        desktop: Option<String>,
    ) -> Self {
        Self {
            id: uuid::Uuid::new_v4().simple().to_string(),
            text,
            created: now,
            remind,
            completed: None,
            desktop,
            lead,
            delivered: false,
            missed: false,
            snoozed: None,
            briefing_day: None,
            source: None,
        }
    }
    pub fn due(&self) -> Option<i64> {
        self.snoozed
            .or(self.remind.map(|t| t - self.lead as i64 * 60))
    }
    pub fn complete(&mut self, now: i64) {
        self.completed = Some(now);
        self.missed = false;
    }
    pub fn snooze(&mut self, now: i64) {
        self.snooze_until(now+600);
    }
    pub fn snooze_until(&mut self, at:i64) {
        self.snoozed = Some(at);
        self.delivered = false;
        self.missed = false;
    }
}
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Data {
    pub sizes: Vec<(Option<String>, crate::desklayer::widget::Size)>,
    pub rows: Vec<Row>,
    pub places: Vec<(Option<String>, crate::desklayer::place::Place)>,
    pub known_notes: std::collections::BTreeMap<String, Vec<String>>,
}
impl Data {
    pub fn observe_note(
        &mut self,
        id: &str,
        body: &str,
        desk: Option<String>,
        now: i64,
        lang: &str,
        cfg: &crate::config::WidgetsCfg,
    ) -> bool {
        let lines: Vec<String> = body.lines().map(str::to_owned).collect();
        if self.known_notes.get(id) == Some(&lines) {
            return false;
        }
        let previous = self
            .known_notes
            .insert(id.into(), lines.clone())
            .unwrap_or_default();
        self.rows.retain(|r| {
            r.source
                .as_ref()
                .is_none_or(|(n, l)| n != id || lines.contains(l))
        });
        for line in lines {
            if !cfg.notes_reminders
                || previous.contains(&line)
                || line.starts_with("- [x]")
                || line.starts_with("- [X]")
                || self.rows.iter().any(|r| {
                    r.source
                        .as_ref()
                        .is_some_and(|(n, l)| n == id && l == &line)
                })
            {
                continue;
            }
            if let Some(f) = super::time::parse(
                &line,
                now,
                lang,
                matches!(cfg.notes_date_hour, 9 | 10).then_some(cfg.notes_date_hour),
            ) {
                let mut row = Row::new(
                    line.trim_start_matches("- [ ] ").into(),
                    now,
                    Some(f.at),
                    cfg.notes_lead,
                    desk.clone(),
                );
                row.source = Some((id.into(), line));
                self.rows.push(row);
            }
        }
        true
    }
    pub fn seed_notes(&mut self, notes: &[(String, String)]) {
        self.known_notes = notes
            .iter()
            .map(|(id, body)| (id.clone(), body.lines().map(str::to_owned).collect()))
            .collect();
        self.rows.retain(|r| {
            r.source.as_ref().is_none_or(|(id, line)| {
                self.known_notes
                    .get(id)
                    .is_some_and(|lines| lines.contains(line))
            })
        });
    }
    pub fn retain_notes(&mut self, ids: &[String]) {
        self.known_notes.retain(|id, _| ids.contains(id));
        self.rows
            .retain(|r| r.source.as_ref().is_none_or(|(id, _)| ids.contains(id)));
    }
}
#[derive(Default)]
pub struct Store {
    pub data: Data,
    original: Option<String>,
    pub writable: bool,
    exported: Option<String>,
    recovery: Vec<(String, String)>,
}
#[derive(Serialize, Deserialize)]
struct Recovery {
    base: Data,
    data: Data,
}

/// Field-wise three-way merge: independent external/local edits both survive.
/// During recovery the primary wins a conflicting field; while editing locally
/// the user's new edit wins. A stable id is never cloned into a second task.
fn merge(base: &Data, local: &Data, remote: &Data, prefer_local: bool) -> Data {
    let mut out = remote.clone();
    out.rows.retain(|r| {
        !base.rows.iter().any(|b| b.id == r.id)
            || local.rows.iter().any(|l| l.id == r.id)
            || (!prefer_local && base.rows.iter().find(|b| b.id == r.id) != Some(r))
    });
    for row in &local.rows {
        let old = base.rows.iter().find(|b| b.id == row.id);
        if let Some(other) = out.rows.iter_mut().find(|r| r.id == row.id) {
            if let Some(old) = old {
                macro_rules! field { ($($f:ident),*) => { $(if row.$f != old.$f && (prefer_local || other.$f == old.$f) { other.$f.clone_from(&row.$f); })* }; }
                field!(
                    text, created, remind, completed, desktop, lead, delivered, missed, snoozed,
                    source
                );
            } else if prefer_local {
                *other = row.clone();
            }
        } else if old != Some(row) {
            out.rows.push(row.clone());
        }
    }
    for (desk, place) in &local.places {
        if !base.places.contains(&(desk.clone(), *place)) {
            if let Some((_, p)) = out.places.iter_mut().find(|(d, _)| d == desk) {
                *p = *place;
            } else {
                out.places.push((desk.clone(), *place));
            }
        }
    }
    out.known_notes
        .retain(|id, _| !base.known_notes.contains_key(id) || local.known_notes.contains_key(id));
    for (id, lines) in &local.known_notes {
        if base.known_notes.get(id) != Some(lines) {
            out.known_notes.insert(id.clone(), lines.clone());
        }
    }
    out
}
impl Store {
    pub fn load(d: &mut impl Disk) -> Self {
        let mut s = Self {
            writable: true,
            exported: d.read("", "inbox.md"),
            ..Default::default()
        };
        match d.read("", FILE) {
            Some(raw) => match serde_json::from_str::<Data>(&raw) {
                Ok(data) => {
                    s.data = data;
                    s.original = Some(raw);
                }
                Err(_) => {
                    s.writable = false;
                    let name = format!("inbox.json.broken-{}", uuid::Uuid::new_v4().simple());
                    if d.rename("", FILE, "", &name) {
                        s.writable = true;
                    }
                }
            },
            None => s.writable = d.exists("", FILE) == Some(false),
        }
        // Reconcile recovery by stable id and its original base. Legacy copies
        // add missing ids only; the primary is authoritative for existing ids.
        for sub in [RECOVERY, RESCUE] {
            if let Ok(names) = d.list(sub) {
                for name in names
                    .into_iter()
                    .filter(|n| n.starts_with("inbox-") && n.ends_with(".json"))
                {
                    if let Some(raw) = d.read(sub, &name) {
                        if let Ok(copy) = serde_json::from_str::<Recovery>(&raw) {
                            s.data = merge(&copy.base, &copy.data, &s.data, false);
                        } else if let Ok(copy) = serde_json::from_str::<Data>(&raw) {
                            s.data = merge(&Data::default(), &copy, &s.data, false);
                        } else {
                            continue;
                        }
                        s.recovery.push((sub.into(), name));
                    }
                }
            }
        }
        if !s.recovery.is_empty() {
            s.save(d);
        }
        s
    }
    pub fn save(&mut self, d: &mut impl Disk) -> bool {
        let current = d.read("", FILE);
        let readable = current.is_some() || d.exists("", FILE) == Some(false);
        let disk_data = current
            .as_deref()
            .map(serde_json::from_str::<Data>)
            .transpose();
        if !readable || disk_data.is_err() {
            self.backup(d);
            return false;
        }
        self.writable = true;
        if current != self.original {
            let base = self
                .original
                .as_deref()
                .and_then(|s| serde_json::from_str(s).ok())
                .unwrap_or_default();
            self.data = merge(
                &base,
                &self.data,
                &disk_data.unwrap().unwrap_or_default(),
                true,
            );
            self.original = current.clone();
        }
        let raw = serde_json::to_string_pretty(&self.data).expect("inbox serializable");
        if current.as_ref() != Some(&raw) && !d.replace("", FILE, &raw) {
            self.backup(d);
            return false;
        }
        self.original = Some(raw);
        // Markdown is an export; never allow the normal note importer to claim it.
        let export = markdown(&self.data.rows);
        if self.exported.as_ref() != Some(&export) && !d.replace("", "inbox.md", &export) {
            return false;
        }
        self.exported = Some(export);
        self.recovery.retain(|(sub, name)| !d.remove(sub, name));
        self.recovery.is_empty()
    }
    fn backup(&mut self, d: &mut impl Disk) {
        let copy = Recovery {
            base: self
                .original
                .as_deref()
                .and_then(|s| serde_json::from_str(s).ok())
                .unwrap_or_default(),
            data: self.data.clone(),
        };
        let raw = serde_json::to_string(&copy).expect("recovery serializable");
        if let Some((sub, name)) = self.recovery.first() {
            if d.replace(sub, name, &raw) {
                return;
            }
        }
        let name = format!("inbox-{}.json", uuid::Uuid::new_v4().simple());
        for sub in [RECOVERY, RESCUE] {
            if d.create(sub, &name, &raw) {
                self.recovery.push((sub.into(), name));
                break;
            }
        }
    }
}
pub fn markdown(rows: &[Row]) -> String {
    rows.iter()
        .filter(|r| r.source.is_none())
        .map(|r| {
            format!(
                "- [{}] {}\n",
                if r.completed.is_some() { "x" } else { " " },
                r.text.replace(['\n', '\r'], " ")
            )
        })
        .collect()
}
pub fn visible(
    rows: &[Row],
    now: i64,
    expanded: bool,
    desktop: Option<&str>,
    per_desktop: bool,
) -> (Vec<usize>, usize, usize) {
    let mut shown = Vec::new();
    let mut folded = 0;
    for (i, r) in rows.iter().enumerate() {
        if r.source.is_some()
            || per_desktop && r.desktop.as_deref().is_some_and(|d| Some(d) != desktop)
        {
            continue;
        }
        if !expanded && r.completed.is_some_and(|at| now - at >= 86400) {
            folded += 1;
        } else {
            shown.push(i);
        }
    }
    let overflow = shown.len().saturating_sub(8);
    shown.truncate(8);
    (shown, folded, overflow)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    #[derive(Default)]
    struct Memory {
        f: BTreeMap<(String, String), String>,
        fail: bool,
        unreadable: bool,
        writes: usize,
    }
    impl Disk for Memory {
        fn list(&self, s: &str) -> Result<Vec<String>, ()> {
            Ok(self
                .f
                .keys()
                .filter(|(p, _)| p == s)
                .map(|(_, n)| n.clone())
                .collect())
        }
        fn read(&self, s: &str, n: &str) -> Option<String> {
            if self.unreadable && n == FILE {
                None
            } else {
                self.f.get(&(s.into(), n.into())).cloned()
            }
        }
        fn exists(&self, s: &str, n: &str) -> Option<bool> {
            if self.unreadable && n == FILE {
                None
            } else {
                Some(self.f.contains_key(&(s.into(), n.into())))
            }
        }
        fn replace(&mut self, s: &str, n: &str, t: &str) -> bool {
            if self.fail && s.is_empty() {
                return false;
            }
            if s.is_empty() && n == FILE {
                self.writes += 1;
            }
            self.f.insert((s.into(), n.into()), t.into());
            true
        }
        fn create(&mut self, s: &str, n: &str, t: &str) -> bool {
            if self.f.contains_key(&(s.into(), n.into())) {
                false
            } else {
                self.replace(s, n, t)
            }
        }
        fn rename(&mut self, s: &str, n: &str, ts: &str, tn: &str) -> bool {
            if let Some(v) = self.f.remove(&(s.into(), n.into())) {
                self.f.insert((ts.into(), tn.into()), v);
                true
            } else {
                false
            }
        }
        fn remove(&mut self, s: &str, n: &str) -> bool {
            self.f.remove(&(s.into(), n.into()));
            true
        }
    }
    #[test]
    fn review_old_notes_seed_without_reminders_and_only_changed_lines_register() {
        let mut data = Data::default();
        data.seed_notes(&[("n".into(), "내일 9시 old\n30분 뒤 old".into())]);
        assert!(data.rows.is_empty());
        assert!(!data.observe_note(
            "n",
            "내일 9시 old\n30분 뒤 old",
            None,
            100,
            "ko",
            &Default::default()
        ));
        assert!(data.observe_note(
            "n",
            "내일 9시 old\n2시간 뒤 new",
            None,
            200,
            "ko",
            &Default::default()
        ));
        assert_eq!(data.rows.len(), 1);
        assert!(data.rows[0].text.contains("new"));
        data.retain_notes(&[]);
        assert!(data.rows.is_empty());
    }
    #[test]
    fn review_batch_observation_and_unchanged_save_do_not_rewrite_inbox() {
        let mut d = Memory::default();
        let mut s = Store::load(&mut d);
        for n in 0..10 {
            s.data.observe_note(
                &n.to_string(),
                "15:00 task",
                None,
                0,
                "en",
                &Default::default(),
            );
        }
        assert!(s.save(&mut d));
        assert_eq!(d.writes, 1);
        for n in 0..10 {
            assert!(!s.data.observe_note(
                &n.to_string(),
                "15:00 task",
                None,
                1,
                "en",
                &Default::default()
            ));
        }
        assert!(s.save(&mut d));
        assert_eq!(d.writes, 1);
    }
    #[test]
    fn review_revising_snoozed_row_uses_new_time() {
        let mut row = Row::new("old".into(), 0, Some(100), 0, None);
        row.snooze(90);
        row.revise("new".into(), Some(200), 0);
        assert_eq!(row.due(), Some(200));
    }
    #[test]
    fn review_legacy_stale_recovery_cannot_clone_or_uncomplete_primary() {
        let mut d = Memory::default();
        let mut store = Store::load(&mut d);
        store
            .data
            .rows
            .push(Row::new("task".into(), 0, None, 0, None));
        let stale = store.data.clone();
        store.data.rows[0].complete(50);
        assert!(store.save(&mut d));
        d.create(
            RECOVERY,
            "inbox-legacy.json",
            &serde_json::to_string(&stale).unwrap(),
        );
        let loaded = Store::load(&mut d);
        assert_eq!(loaded.data.rows.len(), 1);
        assert_eq!(loaded.data.rows[0].completed, Some(50));
    }
    #[test]
    fn review_deleted_lines_and_disabled_edits_do_not_rearm_old_text() {
        let mut data = Data::default();
        let mut cfg = crate::config::WidgetsCfg::default();
        data.seed_notes(&[("n".into(), "15:00 old".into())]);
        data.observe_note("n", "15:00 old\n16:00 new", None, 0, "en", &cfg);
        assert_eq!(data.rows.len(), 1);
        data.observe_note("n", "15:00 old", None, 0, "en", &cfg);
        assert!(data.rows.is_empty());
        cfg.notes_reminders = false;
        data.observe_note("n", "17:00 disabled", None, 0, "en", &cfg);
        cfg.notes_reminders = true;
        assert!(!data.observe_note("n", "17:00 disabled", None, 0, "en", &cfg));
        assert!(data.rows.is_empty());
    }
    #[test]
    fn store_roundtrip_and_markdown() {
        let mut d = Memory::default();
        let mut s = Store::load(&mut d);
        s.data
            .rows
            .push(Row::new("회의".into(), 1, Some(60), 10, None));
        assert!(s.save(&mut d));
        assert_eq!(Store::load(&mut d).data, s.data);
        assert_eq!(d.read("", "inbox.md").unwrap(), "- [ ] 회의\n");
        assert_eq!(super::super::store::parse_body("inbox.md"), None);
    }
    #[test]
    fn unreadable_and_external_edits_are_not_overwritten() {
        let mut d = Memory::default();
        let mut s = Store::load(&mut d);
        s.save(&mut d);
        let before = d.read("", FILE);
        d.unreadable = true;
        s.data.rows.push(Row::new("keep".into(), 0, None, 0, None));
        assert!(!s.save(&mut d));
        d.unreadable = false;
        assert_eq!(d.read("", FILE), before);
        assert!(!d.list(RECOVERY).unwrap().is_empty());
        d.replace("", FILE, "{broken");
        assert!(!s.save(&mut d));
        assert_eq!(d.read("", FILE).as_deref(), Some("{broken"));
    }
    #[test]
    fn failures_recover_without_losing_versions() {
        let mut d = Memory::default();
        let mut s = Store::load(&mut d);
        let row = Row::new("v1".into(), 0, None, 0, None);
        s.data.rows.push(row);
        s.save(&mut d);
        s.data.rows[0].text = "v2".into();
        d.fail = true;
        assert!(!s.save(&mut d));
        d.fail = false;
        let restored = Store::load(&mut d);
        assert_eq!(restored.data.rows.len(), 1);
        assert_eq!(restored.data.rows[0].text, "v2");
        assert!(d.list(RECOVERY).unwrap().is_empty());
    }
    #[test]
    fn review_external_change_merges_by_id_and_session_can_keep_saving() {
        let mut d = Memory::default();
        let mut s = Store::load(&mut d);
        s.data
            .rows
            .push(Row::new("original".into(), 0, None, 0, None));
        assert!(s.save(&mut d));
        let mut external = s.data.clone();
        external.rows[0].complete(20);
        external
            .rows
            .push(Row::new("external".into(), 1, None, 0, None));
        d.replace("", FILE, &serde_json::to_string(&external).unwrap());
        s.data.rows[0].text = "local edit".into();
        assert!(s.save(&mut d));
        assert_eq!(s.data.rows.len(), 2);
        assert_eq!(s.data.rows[0].completed, Some(20));
        assert_eq!(s.data.rows[0].text, "local edit");
        assert!(s.save(&mut d));
    }
    #[test]
    fn review_retry_cleans_recovery_and_completed_row_never_resurrects() {
        let mut d = Memory::default();
        let mut s = Store::load(&mut d);
        s.data.rows.push(Row::new("task".into(), 0, None, 0, None));
        assert!(s.save(&mut d));
        d.fail = true;
        s.data.rows[0].text = "edited".into();
        assert!(!s.save(&mut d));
        s.data.rows[0].complete(50);
        assert!(!s.save(&mut d));
        d.fail = false;
        assert!(s.save(&mut d));
        assert!(d.list(RECOVERY).unwrap().is_empty());
        let restored = Store::load(&mut d);
        assert_eq!(restored.data.rows.len(), 1);
        assert_eq!(restored.data.rows[0].completed, Some(50));
    }
    #[test]
    fn corrupted_file_kept_and_rows_fold_after_a_day() {
        let mut d = Memory::default();
        d.replace("", FILE, "broken");
        let s = Store::load(&mut d);
        assert!(s.writable);
        assert!(d.f.values().any(|v| v == "broken"));
        let mut rows: Vec<_> = (0..11)
            .map(|_| Row::new("x".into(), 0, None, 0, None))
            .collect();
        rows[0].complete(0);
        assert_eq!(
            visible(&rows, 86400, false, None, true),
            ((1..9).collect(), 1, 2)
        );
        assert_eq!(visible(&rows, 86399, false, None, true).1, 0);
    }
}

/// Untimed inbox rows can be carried forward. Future reminders stay scheduled.
pub fn tomorrow_eligible(row: &Row, now: i64) -> bool {
    let today = now.div_euclid(86400);
    row.source.is_none() && row.completed.is_none()
        && row.briefing_day.is_none_or(|day| day <= today)
        && row.snoozed.or(row.remind).is_none_or(|at| at < (today + 1) * 86400)
}

pub fn tomorrow(rows: &mut [Row], ids: &[String], now: i64) -> Vec<String> {
    let day = now.div_euclid(86400) + 1;
    let mut changed = Vec::new();
    for row in rows.iter_mut().filter(|r| ids.contains(&r.id) && tomorrow_eligible(r, now)) {
        if row.snoozed.or(row.remind).is_some() {
            row.snooze_until(day * 86400 + 9 * 3600);
        } else {
            row.briefing_day = Some(day);
        }
        changed.push(row.id.clone());
    }
    changed
}
/// Clear only the entries shown by the card, retaining anything rescheduled since then.
pub fn clear_shown_carries(rows: &mut [Row], shown: &[(String, i64)]) -> bool {
    let mut changed = false;
    for row in rows {
        if shown.iter().any(|(id, day)| row.id == *id && row.briefing_day == Some(*day)) {
            row.briefing_day = None;
            changed = true;
        }
    }
    changed
}

#[cfg(test)]
mod assistant_tests {
    use super::*;
    #[test]
    fn tomorrow_preserves_untimed_rows_without_creating_a_reminder(){
        let mut rows=vec![Row::new("timed".into(),0,Some(100),0,None),Row::new("untimed".into(),0,None,0,None),Row::new("done".into(),0,Some(100),0,None),Row::new("note".into(),0,Some(100),0,None)];
        rows[2].complete(1);rows[3].source=Some(("note".into(),"line".into()));let ids=rows.iter().map(|r|r.id.clone()).collect::<Vec<_>>();
        assert_eq!(tomorrow(&mut rows, &ids, 18 * 3600), ids[..2]);
        assert_eq!(rows[0].due(), Some(86400 + 9 * 3600));
        assert_eq!(rows[1].due(), None);
        assert_eq!(rows[1].briefing_day, Some(1));
        assert!(rows[2..].iter().all(|r| r.briefing_day.is_none()));
        assert!(tomorrow(&mut rows, &ids, 18 * 3600).is_empty());
        let restored: Vec<Row> = serde_json::from_slice(&serde_json::to_vec(&rows).unwrap()).unwrap();
        assert_eq!(restored[1].briefing_day, Some(1));
    }
}

#[cfg(test)]
mod review_tests {
 use super::*;
 #[test]
 fn review_2_tomorrow_does_not_pull_future_or_snoozed_reminders_back() {
  let mut rows:Vec<_>=[-1,80000,86400,172800].into_iter().map(|at|Row::new("task".into(),0,Some(at),0,None)).collect();
  let mut snoozed=Row::new("snoozed".into(),0,Some(100),0,None);
  snoozed.snooze_until(172800);rows.push(snoozed);
  let ids:Vec<_>=rows.iter().map(|r|r.id.clone()).collect();
  assert_eq!(tomorrow(&mut rows,&ids,60000),ids[..2]);
  assert_eq!(rows[2].due(),Some(86400));
  assert_eq!(rows[3].due(),Some(172800));
  assert_eq!(rows[4].due(),Some(172800));
 }
}

#[cfg(test)] mod future_lead_review {
 use super::*;
 #[test] fn review_2_tomorrow_midnight_with_lead_stays_tomorrow() {
  let mut rows=vec![Row::new("tomorrow".into(),0,Some(86400),15,None)];
  let ids=vec![rows[0].id.clone()];
  assert!(tomorrow(&mut rows,&ids,86300).is_empty());
 }
}
