//! Local reminders never enter the chat provider or conversation history.
use super::{
    card::{Button, Card, Kind, Line, Route},
    news::{self, Receipt},
};
use crate::{desklayer::card::Mark, notes::reminders::Notice};
use std::sync::Mutex;

pub(super) fn card(notice: &Notice) -> Card {
    let id = notice.single_id();
    let mut buttons = vec![];
    if !id.is_empty() {
        buttons.push(Button {
            label: crate::i18n::t("remind.complete").into(),
            route: Route::Complete(id.into()),
            primary: true,
        });
        buttons.push(Button {
            label: crate::i18n::t("remind.snooze").into(),
            route: Route::Snooze(id.into()),
            primary: false,
        });
    }
    Card {
        kind: Kind::Reminder,
        title: crate::i18n::t("widgets.notes_reminders").into(),
        emphasis: None,
        lines: vec![Line::new(Mark::Note, &notice.text, Route::Notes(id.into()))],
        buttons,
    }
}

enum Command {
    Notice(Notice, bool),
    Cancel(String),
}
static QUEUE: Mutex<Vec<Command>> = Mutex::new(Vec::new());

pub fn enqueue(notice: Notice) {
    enqueue_all(std::iter::once(notice));
}

pub fn enqueue_all(notices: impl IntoIterator<Item = Notice>) {
    let away = super::living::away();
    let commands: Vec<_> = notices
        .into_iter()
        .map(|mut notice| {
            notice.missed |= away;
            Command::Notice(notice, away)
        })
        .collect();
    if commands.is_empty() {
        return;
    }
    QUEUE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .extend(commands);
    super::refresh();
}

pub fn cancel(id: &str) {
    QUEUE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push(Command::Cancel(id.into()));
    super::refresh();
}

/// The desktop notes thread only enqueues. The dock thread owns presentation.
pub(super) fn poll() {
    let commands = std::mem::take(&mut *QUEUE.lock().unwrap_or_else(|e| e.into_inner()));
    for command in commands {
        match command {
            Command::Notice(notice, away) => {
                news::add_with_presence(card(&notice), Receipt::Reminder(notice), away);
            }
            Command::Cancel(id) => news::retain(
                |r| !matches!(r, Receipt::Reminder(notice) if notice.ids.contains(&id)),
            ),
        }
    }
}

#[cfg(test)]
#[test]
fn notes_thread_can_submit_and_cancel_a_dock_thread_card() {
    let notice = Notice::single("cross-thread-fixture".into(), "Reminder".into());
    let submitted = notice.clone();
    std::thread::spawn(move || enqueue(submitted))
        .join()
        .unwrap();
    poll();
    let receipt = Receipt::Reminder(notice);
    assert!(news::waiting(&receipt));
    std::thread::spawn(|| cancel("cross-thread-fixture"))
        .join()
        .unwrap();
    poll();
    assert!(!news::waiting(&receipt));
}
