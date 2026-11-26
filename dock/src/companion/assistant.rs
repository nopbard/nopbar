//! Local assistant content. Never starts a provider, reads credentials, or captures a screen.
use super::card::{Card, Emphasis, Kind, Line, Route};
use crate::desklayer::card::Mark;
use crate::notes::inbox::Row;

#[derive(Default)]
pub struct Morning {
    /// Local civil start/end seconds and title; all-day dates stay civil.
    pub calendar: Option<Vec<(i64, i64, String)>>,
    pub notes: Vec<Row>,
    pub batteries: Vec<(String, u8)>,
}
pub fn morning(data: Morning, now: i64, agenda_at: i64) -> Option<Card> {
    let mut lines = Vec::new();
    let mut emphasis = None;
    let day = now.div_euclid(86400);
    if let Some(mut events) = data.calendar {
        let midnight = day * 86400;
        events.retain(|(start, end, _)| {
            *start < midnight + 86400 && (*end > midnight || *start >= midnight)
        });
        events.sort_by_key(|(start, _, _)| *start);
        if let Some((at, _, title)) = events.first() {
            let time = crate::notes::time::clock((*at).max(midnight));
            emphasis = Some(Emphasis {
                value: time.clone(),
                title: title.clone(),
                route: Route::Agenda(agenda_at),
            });
            lines.push(Line {
                glyph: Mark::Calendar,
                label: crate::i18n::t("assistant.agenda").into(),
                value: format!("{} · {time}", events.len()),
                detail: title.clone(),
                route: Route::Agenda(agenda_at),
            });
        }
    }
    let mut reminders: Vec<_> = data
        .notes
        .iter()
        .filter(|r| r.completed.is_none())
        .filter_map(|r| {
            r.snoozed
                .or(r.remind)
                .filter(|at| at.div_euclid(86400) == day)
                .map(|at| (at, r))
        })
        .collect();
    reminders.sort_by_key(|(at, _)| *at);
    if let Some((at, row)) = reminders.first() {
        lines.push(Line {
            glyph: Mark::Note,
            label: crate::i18n::t("assistant.reminders").into(),
            value: format!("{} · {}", reminders.len(), crate::notes::time::clock(*at)),
            detail: String::new(),
            route: Route::Notes(row.id.clone()),
        });
    }
    lines.extend(data.notes.iter().filter_map(|row| {
        let scheduled = row.briefing_day?;
        if row.completed.is_some() || row.source.is_some() || scheduled > day {
            return None;
        }
        Some(Line::new(
            Mark::Note,
            row.text.clone(),
            Route::BriefingNote { id: row.id.clone(), day: scheduled },
        ))
    }).take(5));
    let count = data
        .notes
        .iter()
        .filter(|r| r.source.is_none() && r.completed.is_none())
        .count();
    if count > 0 {
        lines.push(Line {
            glyph: Mark::Inbox,
            label: crate::i18n::t("assistant.inbox").into(),
            value: count.to_string(),
            detail: String::new(),
            route: Route::Inbox,
        });
    }
    for (name, pct) in data.batteries.into_iter().filter(|(_, pct)| *pct <= 20) {
        lines.push(Line {
            glyph: Mark::Battery,
            label: name,
            value: format!("{pct}%"),
            detail: String::new(),
            route: Route::Close,
        });
    }
    (!lines.is_empty()).then(|| Card {
        kind: Kind::Morning,
        emphasis,
        title: crate::i18n::t("assistant.morning").into(),
        lines,
        buttons: vec![],
    })
}

/// Returns true while a due briefing still needs data.
pub fn poll() -> bool {
    let calendar = super::living::local_calendar();
    let receipt = super::news::Receipt::Morning(calendar.date);
    let due = super::APP.with(|a| {
        a.borrow().as_ref().is_some_and(|a| {
            let state = a.news_body.map_or(super::State::Idle, |_| a.model.state);
            a.cfg.enabled && a.cfg.morning_briefing
                && a.purpose.using()
                && a.cfg.routine.empty_briefing != Some(calendar.date)
                && a.rhythm.morning_due(calendar, a.cfg.last_greet, Some(state))
        })
    });
    if !due || super::news::waiting(&receipt) {
        return false;
    }
    let notes = if crate::notes::preferences().notes {
        let Some(rows) = crate::notes::runtime::assistant_snapshot() else {
            return true;
        };
        rows
    } else {
        vec![]
    };
    let unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    let agenda = crate::agenda::Snapshot::read(crate::agenda::companion_day(unix));
    if agenda.enabled && agenda.loading {
        return true;
    }
    let events = (agenda.enabled && agenda.has_source).then(|| {
        agenda
            .events
            .into_iter()
            .map(|e| {
                (
                    if e.ev.all_day {
                        e.ev.start
                    } else {
                        crate::agenda::tz::local_from_utc(e.ev.start)
                    },
                    if e.ev.all_day {
                        e.ev.end
                    } else {
                        crate::agenda::tz::local_from_utc(e.ev.end)
                    },
                    e.ev.title,
                )
            })
            .collect()
    });
    let data = Morning {
        calendar: events,
        notes,
        batteries: crate::device_battery::runtime::assistant_snapshot(),
    };
    let Some(card) = morning(data, crate::notes::time::now(), unix) else {
        super::APP.with(|a| {
            if let Some(a) = a.borrow_mut().as_mut() {
                a.cfg.routine.empty_briefing = Some(calendar.date);
                a.cfg.routine.dirty = true;
            }
        });
        return false;
    };
    super::news::add(card, receipt);
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn empty_morning_has_no_card_and_disconnected_calendar_has_no_row() {
        assert!(morning(Morning::default(), 0, 0).is_none());
    }
    #[test]
    fn morning_counts_today_and_outstanding_and_includes_twenty_percent() {
        let _lang = crate::i18n::test_lock("ko");
        let mut done = Row::new("done".into(), 0, Some(300), 0, None);
        done.complete(1);
        let data = Morning {
            calendar: Some(vec![
                (86400, 87000, "tomorrow".into()),
                (600, 800, "first".into()),
                (900, 1000, "second".into()),
            ]),
            notes: vec![Row::new("pending".into(), 0, Some(700), 0, None), done],
            batteries: vec![("Mouse".into(), 20), ("Keyboard".into(), 21)],
        };
        let card = morning(data, 500, 500).unwrap();
        assert_eq!(card.lines.len(), 4);
        assert_eq!(card.lines[0].value, "2 · 0:10");
        assert_eq!(card.lines[0].detail, "first");
        assert_eq!(card.emphasis.as_ref().unwrap().value, "0:10");
        assert!(matches!(card.lines[1].route, Route::Notes(_)));
        assert!(matches!(card.lines[2].route, Route::Inbox));
        assert_eq!(card.lines[3].value, "20%");
    }

    #[test]
    fn friday_carry_waits_for_the_first_briefing_even_after_the_weekend() {
        use crate::notes::inbox::{clear_shown_carries, tomorrow};
        let friday = 86400;
        let monday = 4 * 86400;
        let mut notes = vec![Row::new("Follow up".into(), friday, None, 0, None)];
        let id = notes[0].id.clone();
        assert_eq!(tomorrow(&mut notes, std::slice::from_ref(&id), friday + 60_000), [id.clone()]);
        let briefing = |notes: &[Row], day| {
            morning(Morning { notes: notes.to_vec(), ..Default::default() }, day, 0).unwrap()
        };
        assert!(briefing(&notes, friday).shown_carries().is_empty());
        let shown = briefing(&notes, monday).shown_carries();
        assert_eq!(shown, [(id, 2)]);
        assert_eq!(notes[0].briefing_day, Some(2));
        assert!(clear_shown_carries(&mut notes, &shown));
        assert_eq!(notes[0].briefing_day, None);
        assert!(briefing(&notes, monday + 86400).shown_carries().is_empty());
        assert!(!clear_shown_carries(&mut notes, &shown));
    }

    #[test]
    fn morning_uses_one_inbox_link_for_items_beyond_the_five_visible_notes() {
        let notes: Vec<_> = (0..8).map(|i| {
            let mut note = Row::new(format!("Task {i}"), 0, None, 0, None);
            note.briefing_day = Some(1);
            note
        }).collect();
        let card = morning(Morning { notes, ..Default::default() }, 86400, 0).unwrap();
        assert_eq!(card.shown_carries().len(), 5);
        let inbox: Vec<_> = card.lines.iter().filter(|line| matches!(line.route, Route::Inbox)).collect();
        assert_eq!(inbox.len(), 1);
        assert_eq!(inbox[0].value, "8");
    }

    #[test]
    fn morning_includes_overnight_events_but_excludes_events_ending_at_midnight() {
        let _lang = crate::i18n::test_lock("ko");
        let card = morning(
            Morning {
                calendar: Some(vec![
                    (-3600, 3600, "overnight".into()),
                    (-86400, 0, "yesterday".into()),
                    (0, 0, "point".into()),
                    (86400, 172800, "tomorrow".into()),
                ]),
                ..Default::default()
            },
            600,
            600,
        )
        .unwrap();
        assert_eq!(card.lines.len(), 1);
        assert_eq!(card.lines[0].value, "2 · 0:00");
        assert_eq!(card.emphasis.as_ref().unwrap().title, "overnight");
    }
}
