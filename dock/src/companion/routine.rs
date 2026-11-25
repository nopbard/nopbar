//! Calendar-day numeric usage history. No titles, keystrokes, or application names.
use super::card::{Button, Card, Kind, Line, Route};
use serde::{Deserialize, Serialize};
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Routine {
    pub day: i64,
    pub last_use: Option<u16>,
    pub minutes: [Option<u16>; 14],
    pub wrapped: Option<i64>,
    pub empty_briefing: Option<u32>,
    #[serde(skip)]
    pub dirty: bool,
}
impl Routine {
    pub fn observe(&mut self, day: i64, minute: u16, using: bool) {
        if self.wrapped.is_some_and(|old| old > day) {
            self.wrapped = None;
            self.dirty = true;
        }
        if day != self.day {
            let delta = if day > self.day {
                (day - self.day).min(15) as usize
            } else {
                15
            };
            let old = self.minutes;
            self.minutes = [None; 14];
            if delta <= 14 {
                self.minutes[delta - 1] = self.last_use;
                self.minutes[delta..].copy_from_slice(&old[..14 - delta]);
            }
            self.day = day;
            self.last_use = None;
            self.dirty = true;
        }
        if day == self.day && using && minute < 1440 && self.last_use != Some(minute) {
            self.last_use = Some(minute);
            self.dirty = true;
        }
    }
    pub fn learned(&self) -> Option<u16> {
        let mut values: Vec<_> = self
            .minutes
            .iter()
            .flatten()
            .copied()
            .filter(|m| *m < 1440)
            .collect();
        if values.len() < 7 {
            return None;
        }
        values.sort_unstable();
        let n = values.len();
        Some(if n % 2 == 0 {
            (values[n / 2 - 1] + values[n / 2]) / 2
        } else {
            values[n / 2]
        })
    }
    pub fn closing(&self) -> u16 {
        self.learned().unwrap_or(18 * 60)
    }
    pub fn due(&self, day: i64, minute: u16) -> bool {
        self.wrapped.is_none_or(|old| old != day) && minute >= self.closing()
    }
    pub fn judged(&mut self, day: i64) {
        self.wrapped = Some(day);
        self.dirty = true;
    }
}
pub fn wrap(rows: &[crate::notes::inbox::Row], now: i64) -> Option<Card> {
    let remaining: Vec<_> = rows
        .iter()
        .filter(|r| r.source.is_none() && r.completed.is_none())
        .collect();
    if remaining.is_empty() {
        return None;
    }
    let ids = remaining
        .iter()
        .filter(|r| crate::notes::inbox::tomorrow_eligible(r, now))
        .map(|r| r.id.clone())
        .collect();
    let mut lines: Vec<_> = remaining
        .iter()
        .take(5)
        .map(|r| Line::new(crate::desklayer::card::Mark::Note, r.text.clone(), Route::Notes(r.id.clone())))
        .collect();
    if remaining.len() > lines.len() {
        lines.push(Line::new(
            crate::desklayer::card::Mark::Inbox,
            format!("+{}", remaining.len() - lines.len()),
            Route::Inbox,
        ));
    }
    Some(Card {
        kind: Kind::Wrap,
        emphasis: None,
        title: crate::i18n::t("assistant.wrap").into(),
        lines,
        buttons: vec![
            Button {
                primary: true,
                label: crate::i18n::t("assistant.tomorrow").into(),
                route: Route::Tomorrow(ids),
            },
            Button {
                primary: false,
                label: crate::i18n::t("pip.close").into(),
                route: Route::Close,
            },
        ],
    })
}
pub fn poll() {
    if super::card::active() || super::chat::busy() {
        return;
    }
    let local = crate::notes::time::now();
    let day = local.div_euclid(86400);
    let minute = (local.rem_euclid(86400) / 60) as u16;
    let due = super::APP.with(|a| {
        a.borrow()
            .as_ref()
            .is_some_and(|a| a.cfg.day_wrap && a.purpose.using() && a.cfg.routine.due(day, minute))
    });
    if !due || super::remind::delivery() != crate::notes::reminders::Delivery::Bubble {
        return;
    }
    let Some(rows) = crate::notes::runtime::assistant_snapshot() else {
        return;
    };
    let card = wrap(&rows, local);
    super::APP.with(|a| {
        let mut a = a.borrow_mut();
        let Some(a) = a.as_mut() else {
            return;
        };
        if let Some(card) = card {
            if !super::card::show(&a.gfx, card) {
                return;
            }
            a.model.landed(std::time::Instant::now());
            a.purpose.notice(false);
            if a.peek_home().is_some() {
                a.peek.notify(a.start.elapsed().as_secs_f64());
            }
        }
        a.cfg.routine.judged(day);
    });
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn review_2_wrap_ids_share_tomorrow_eligibility() {
        let mut rows: Vec<_> = [-10, 80000, 86400, 172800]
            .into_iter()
            .map(|at| crate::notes::inbox::Row::new("task".into(), 0, Some(at), 0, None))
            .collect();
        rows.push(crate::notes::inbox::Row::new(
            "untimed".into(),
            0,
            None,
            0,
            None,
        ));
        let card = wrap(&rows, 60000).unwrap();
        let Route::Tomorrow(ids) = &card.buttons[0].route else {
            panic!()
        };
        assert_eq!(
            *ids,
            [&rows[0], &rows[1], &rows[4]].map(|r| r.id.clone())
        );
        assert_eq!(crate::notes::inbox::tomorrow(&mut rows, ids, 60000), *ids);
    }
    #[test]
    fn review_6_future_clock_recovers_today_usage_and_wrap() {
        let mut r = Routine::default();
        r.observe(1000, 1200, true);
        r.judged(1000);
        r.observe(10, 1100, true);
        assert_eq!(r.day, 10);
        assert_eq!(r.last_use, Some(1100));
        assert!(r.due(10, 1100));
        r.judged(10);
        assert!(!r.due(10, 1100));
    }
    #[test]
    fn median_uses_fourteen_calendar_days_and_seven_samples() {
        let mut r = Routine::default();
        for day in 1..=7 {
            r.observe(day, 1000 + day as u16 * 10, true);
        }
        assert_eq!(r.closing(), 1080);
        r.observe(8, 600, true);
        assert_eq!(r.closing(), 1040);
        r.observe(9, 600, true);
        assert_eq!(r.closing(), 1035);
        r.observe(30, 600, true);
        assert_eq!(r.closing(), 1080);
        assert_eq!(r.minutes, [None; 14]);
    }
    #[test]
    fn wrap_once_each_day_even_empty_and_clock_backwards() {
        let mut r = Routine::default();
        assert!(!r.due(1, 1079));
        assert!(r.due(1, 1080));
        r.judged(1);
        assert!(!r.due(1, 1400));
        assert!(r.due(0, 1400));
        assert!(r.due(2, 1080));
        assert!(wrap(&[], 0).is_none());
    }
    #[test]
    fn only_active_minutes_are_kept_and_old_json_defaults() {
        let mut r: Routine = serde_json::from_str("{}").unwrap();
        r.observe(1, 900, true);
        r.observe(1, 1000, false);
        assert_eq!(r.last_use, Some(900));
        r.observe(2, 400, false);
        assert_eq!(r.minutes[0], Some(900));
        assert_eq!(r.last_use, None);
    }
    #[test]
    fn wrap_lists_five_and_carries_untimed_inbox_items() {
        let rows: Vec<_> = (0..8)
            .map(|i| crate::notes::inbox::Row::new(format!("Task {i}"), 0, None, 0, None))
            .collect();
        let c = wrap(&rows, 0).unwrap();
        assert_eq!(c.lines.len(), 6);
        assert_eq!(c.lines[5].label, "+3");
        assert!(matches!(c.lines[5].route, Route::Inbox));
        assert!(matches!(&c.buttons[0].route,Route::Tomorrow(ids) if ids.len() == rows.len()));
    }
    #[test]
    #[ignore = "offscreen wrap card"]
    fn harmony_assistant_wrap() {
        let _lang = crate::i18n::test_lock("ko");
        let stage = crate::gfx::promo_kit::stage();
        let rows: Vec<_> = [
            "견적 검토",
            "회의 자료",
            "메일 회신",
            "계약서 확인",
            "디자인 시안",
        ]
        .iter()
        .map(|s| crate::notes::inbox::Row::new((*s).into(), 0, None, 0, None))
        .collect();
        super::super::card::tests::snapshot(&stage.gfx, "wrap", &wrap(&rows, 0).unwrap());
    }
}
