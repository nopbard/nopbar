//! Local device battery notices share the assistant card and history.
use super::{
    card::{Card, Kind, Line, Route},
    news::{self, Receipt},
};
use crate::desklayer::card::Mark;
use std::cell::RefCell;

thread_local! { static DELIVERED: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) }; }

pub fn enqueue(id: String, name: String, percent: u8) {
    if news::add(card(name, percent), Receipt::Battery(id)) {
        super::refresh();
    }
}

pub(super) fn card(name: String, percent: u8) -> Card {
    let mut line = Line::new(Mark::Battery, name, Route::Close);
    line.value = format!("{percent}%");
    Card {
        kind: Kind::Battery,
        title: crate::i18n::t("widgets.device_battery").into(),
        emphasis: None,
        lines: vec![line],
        buttons: vec![],
    }
}

pub fn clear() {
    retain(&[]);
    take_delivered();
}

pub fn retain(ids: &[String]) {
    news::retain(|r| !matches!(r, Receipt::Battery(id) if !ids.contains(id)));
}

pub(super) fn delivered(id: String) {
    DELIVERED.with(|ids| ids.borrow_mut().push(id));
}

pub fn take_delivered() -> Vec<String> {
    DELIVERED.with(|ids| std::mem::take(&mut *ids.borrow_mut()))
}
