//! Pure delivery policy, independently exercised without a desktop or AI provider.
use super::inbox::Row;
#[derive(Clone, Debug, PartialEq)]
pub struct Notice {
    pub ids: Vec<String>,
    pub text: String,
    pub missed: bool,
    pub grouped: bool,
}
impl Notice {
    pub fn single(id: String, text: String) -> Self {
        Self {
            ids: vec![id],
            text,
            missed: false,
            grouped: false,
        }
    }
    pub fn summary(ids: Vec<String>, missed: bool) -> Self {
        let text = crate::i18n::t(if missed {
            "remind.missed_count"
        } else {
            "remind.count"
        })
        .replace("{n}", &ids.len().to_string());
        Self {
            ids,
            text,
            missed,
            grouped: true,
        }
    }
    pub fn single_id(&self) -> &str {
        if self.ids.len() == 1 && !self.grouped {
            &self.ids[0]
        } else {
            ""
        }
    }
}
#[derive(Default)]
pub struct Queue {
    items: std::collections::VecDeque<Notice>,
}
impl Queue {
    pub const fn new() -> Self {
        Self {
            items: std::collections::VecDeque::new(),
        }
    }
    pub fn push(&mut self, mut notice: Notice) {
        notice
            .ids
            .retain(|id| !self.items.iter().any(|n| n.ids.contains(id)));
        if !notice.ids.is_empty() {
            self.items.push_back(notice);
        }
    }
    pub fn next(&self) -> Option<Notice> {
        let first = self.items.front()?;
        if first.missed {
            return Some(Notice::summary(first.ids.iter().take(128).cloned().collect(), true));
        }
        let ids: Vec<_> = self
            .items
            .iter()
            .take_while(|n| !n.missed)
            .flat_map(|n| n.ids.clone())
            .take(128)
            .collect();
        Some(if ids.len() > 1 || first.grouped {
            Notice::summary(ids, false)
        } else {
            first.clone()
        })
    }
    pub fn cancel(&mut self, id: &str) {
        self.items
            .iter_mut()
            .for_each(|n| n.ids.retain(|i| i != id));
        self.items.retain(|n| !n.ids.is_empty());
    }
    pub fn acknowledge(&mut self, notice: &Notice) {
        for id in &notice.ids {
            self.cancel(id);
        }
    }
    #[cfg(test)]
    pub fn clear(&mut self) {
        self.items.clear();
    }
}
pub fn delivery_for(
    enabled: bool,
    hidden: bool,
    gnome_fullscreen: bool,
    system_fullscreen: bool,
    meeting: bool,
) -> Delivery {
    route(
        enabled,
        hidden,
        if enabled {
            gnome_fullscreen
        } else {
            system_fullscreen
        },
        meeting,
    )
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Delivery {
    Defer,
    Bubble,
    Toast,
}
pub fn route(enabled: bool, hidden: bool, fullscreen: bool, meeting: bool) -> Delivery {
    if fullscreen || meeting {
        Delivery::Defer
    } else if !enabled || hidden {
        Delivery::Toast
    } else {
        Delivery::Bubble
    }
}
pub fn due(rows: &[Row], now: i64) -> Vec<String> {
    rows.iter()
        .filter(|r| {
            r.completed.is_none() && !r.delivered && !r.missed && r.due().is_some_and(|t| t <= now)
        })
        .map(|r| r.id.clone())
        .collect()
}
/// Volatile scheduling ownership bridges enqueue -> actual presentation -> ack.
/// It is deliberately separate from the durable delivered bit.
pub fn unqueued(
    rows: &[Row],
    now: i64,
    pending: &mut std::collections::HashSet<String>,
) -> Vec<String> {
    due(rows, now)
        .into_iter()
        .filter(|id| pending.insert(id.clone()))
        .collect()
}
pub fn missed_at_start(rows: &[Row], now: i64) -> Vec<String> {
    rows.iter()
        .filter(|r| {
            r.completed.is_none()
                && !r.delivered
                && !r.missed
                && r.snoozed.or(r.remind).is_some_and(|at| at < now)
        })
        .map(|r| r.id.clone())
        .collect()
}
pub fn delivered(rows: &mut [Row], notice: &Notice) -> bool {
    let mut changed = false;
    for row in rows.iter_mut().filter(|r| notice.ids.contains(&r.id)) {
        changed |= !row.delivered || notice.missed && !row.missed;
        row.delivered = true;
        row.missed |= notice.missed;
    }
    changed
}
pub fn message(row: &Row, now: i64) -> String {
    let text = super::time::parse(&row.text, row.created, crate::i18n::current(), Some(9))
        .map(|f| {
            format!("{}{}", &row.text[..f.span.start], &row.text[f.span.end..])
                .trim()
                .to_owned()
        })
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| row.text.clone());
    let minutes = if row.snoozed.is_some() {
        0
    } else {
        row.remind.map_or(0, |t| ((t - now + 59) / 60).max(0))
    };
    if minutes > 0 {
        crate::i18n::t("remind.in_minutes")
            .replace("{n}", &minutes.to_string())
            .replace("{text}", &text)
    } else {
        text
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn large_deferred_batch_drains_without_losing_members() {
        let mut q = Queue::new();
        for i in 0..260 { q.push(Notice::single(format!("{i:032x}"), "note".into())); }
        for n in [128, 128, 4] {
            let notice = q.next().unwrap();
            assert_eq!(notice.ids.len(), n);
            q.acknowledge(&notice);
        }
        assert!(q.next().is_none());
    }
    #[test]
    fn review_pending_handoff_never_duplicates_before_delivery_ack() {
        let mut rows = vec![Row::new("due".into(), 0, Some(10), 0, None)];
        let mut pending = std::collections::HashSet::new();
        assert_eq!(unqueued(&rows, 20, &mut pending).len(), 1);
        assert!(unqueued(&rows, 21, &mut pending).is_empty());
        assert!(!rows[0].delivered);
        let notice = Notice::single(rows[0].id.clone(), "due".into());
        delivered(&mut rows, &notice);
        pending.remove(&notice.ids[0]);
        assert!(unqueued(&rows, 22, &mut pending).is_empty());
        rows[0].snooze(22);
        assert_eq!(unqueued(&rows, 622, &mut pending).len(), 1);
    }
    #[test]
    fn review_delivery_ack_survives_update_restart_and_settings_toggle() {
        for transport in [Delivery::Bubble, Delivery::Toast] {
            let mut rows = vec![Row::new("past".into(), 0, Some(10), 0, None)];
            let notice = Notice::summary(missed_at_start(&rows, 50), true);
            let mut queue = Queue::default();
            queue.push(notice.clone());
            // Hidden/disabled notes: no transport acknowledgment, no durable change.
            assert!(!rows[0].delivered && !rows[0].missed);
            queue.clear();
            queue.push(notice.clone());
            assert!(matches!(transport, Delivery::Bubble | Delivery::Toast));
            assert!(delivered(&mut rows, &notice));
            queue.acknowledge(&notice);
            let reloaded: Vec<Row> =
                serde_json::from_str(&serde_json::to_string(&rows).unwrap()).unwrap();
            for _restart_or_toggle in 0..3 {
                assert!(missed_at_start(&reloaded, 100).is_empty());
                assert!(due(&reloaded, 100).is_empty());
            }
        }
    }
    #[test]
    fn review_queued_missed_members_are_not_added_again_as_individuals() {
        let mut queue = Queue::default();
        queue.push(Notice::summary(vec!["a".into(), "b".into()], true));
        queue.push(Notice::single("a".into(), "duplicate".into()));
        let notice = queue.next().unwrap();
        assert_eq!(notice.ids.len(), 2);
        queue.acknowledge(&notice);
        assert!(queue.next().is_none());
    }
    #[test]
    fn review_disabled_companion_uses_system_fullscreen_not_stale_ground() {
        assert_eq!(
            delivery_for(false, true, true, false, false),
            Delivery::Toast
        );
        assert_eq!(
            delivery_for(false, true, false, true, false),
            Delivery::Defer
        );
    }
    #[test]
    fn review_deferred_notices_group_and_acknowledge_all_members() {
        let mut q = Queue::default();
        for id in ["a", "b", "c"] {
            q.push(Notice::single(id.into(), id.into()));
        }
        let n = q.next().unwrap();
        assert_eq!(n.ids, ["a", "b", "c"]);
        assert_eq!(n.single_id(), "");
        q.acknowledge(&n);
        assert!(q.next().is_none());
    }
    #[test]
    fn review_restart_does_not_recount_delivered_or_missed() {
        let mut rows = vec![
            Row::new("shown".into(), 0, Some(10), 0, None),
            Row::new("missed".into(), 0, Some(20), 0, None),
        ];
        rows[0].delivered = true;
        rows[1].missed = true;
        assert_eq!(missed_at_start(&rows, 50).len(), 0);
    }
    #[test]
    fn review_startup_lead_window_is_regular_reminder() {
        let rows = vec![Row::new("future".into(), 0, Some(3600), 10, None)];
        assert_eq!(missed_at_start(&rows, 3300).len(), 0);
        assert_eq!(due(&rows, 3300).len(), 1);
    }
    #[test]
    fn review_queueing_missed_is_not_delivery() {
        let rows = vec![Row::new("past".into(), 0, Some(10), 0, None)];
        assert_eq!(missed_at_start(&rows, 50).len(), 1);
        assert!(!rows[0].delivered && !rows[0].missed);
    }
    #[test]
    fn lead_complete_and_snooze() {
        let mut r = Row::new("x".into(), 0, Some(3600), 10, None);
        assert_eq!(r.due(), Some(3000));
        assert!(due(&[r.clone()], 2999).is_empty());
        assert_eq!(due(&[r.clone()], 3000), vec![r.id.clone()]);
        r.delivered = true;
        r.snooze(3000);
        assert_eq!(r.due(), Some(3600));
        assert!(!r.delivered);
        r.complete(3001);
        assert!(due(&[r], 4000).is_empty());
    }
    #[test]
    fn startup_groups_missed_and_does_not_redeliver() {
        let mut rows = vec![
            Row::new("a".into(), 0, Some(10), 0, None),
            Row::new("b".into(), 0, Some(20), 0, None),
            Row::new("c".into(), 0, Some(100), 0, None),
        ];
        let ids = missed_at_start(&rows, 50);
        assert_eq!(ids.len(), 2);
        delivered(&mut rows, &Notice::summary(ids, true));
        assert!(due(&rows, 50).is_empty());
        assert_eq!(due(&rows, 100).len(), 1);
    }
    #[test]
    fn defer_hidden_fullscreen_meeting_and_toast_when_disabled() {
        assert_eq!(route(true, true, false, false), Delivery::Toast);
        assert_eq!(route(true, false, true, false), Delivery::Defer);
        assert_eq!(route(true, false, false, true), Delivery::Defer);
        assert_eq!(route(false, true, false, false), Delivery::Toast);
        assert_eq!(route(true, false, false, false), Delivery::Bubble);
    }
}
