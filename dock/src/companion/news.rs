//! Local assistant delivery and today's in-memory history.
use super::{
    card::{self, Card, Kind, Line, Route},
    SlotAnchor,
};
use crate::{
    desklayer::card::Mark,
    notes::{reminders::Notice, runtime::Action},
};
use std::{
    cell::RefCell,
    collections::VecDeque,
    rc::Rc,
    sync::mpsc,
    time::{Duration, Instant},
};
use windows::Win32::Foundation::RECT;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Destination {
    Body,
    Slot,
    WaitAnchor,
    Toast,
}

fn destination(enabled: bool, topbar: bool, body: bool, body_expected: bool, slot: bool) -> Destination {
    if !enabled {
        Destination::Toast
    } else if body {
        Destination::Body
    } else if topbar && slot {
        Destination::Slot
    } else if topbar || body_expected {
        Destination::WaitAnchor
    } else {
        Destination::Toast
    }
}

#[derive(Clone, Copy)]
enum Delivery {
    Body,
    Slot,
    Tray(SlotPlacement),
    Defer(Option<u32>),
    WaitAnchor(u32),
    Idle,
    Toast,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HistoryRequest {
    Open,
    Keep,
}

struct PresentationState {
    enabled: bool,
    idle: bool,
    history_showing: bool,
    return_inputs_loading: bool,
}

enum Presentation<T> {
    Stop,
    History(HistoryRequest, Option<HistoryAnchor>),
    WaitForReturn,
    Automatic(T),
}

type SlotPlacement = (SlotAnchor, RECT);

#[derive(Clone, Copy)]
enum HistoryAnchor {
    Bar(SlotPlacement),
    Tray(SlotPlacement),
}

impl HistoryAnchor {
    fn placement(self) -> SlotPlacement {
        match self {
            Self::Bar(placement) | Self::Tray(placement) => placement,
        }
    }
}

fn available_slot(preferred: Option<SlotAnchor>) -> Option<SlotPlacement> {
    preferred
        .and_then(|slot| crate::topbar::companion_news_anchor(Some(slot)))
        .or_else(|| crate::topbar::companion_news_anchor(None))
}

fn history_anchor_covered(slot: SlotAnchor) -> bool {
    use windows::Win32::Graphics::Gdi::{MonitorFromRect, MONITOR_DEFAULTTONULL};
    let monitor = unsafe { MonitorFromRect(&slot.rect, MONITOR_DEFAULTTONULL) };
    crate::monitors::info(monitor.0 as isize)
        .is_none_or(|monitor| crate::dock::fullscreen_on_monitor(monitor.handle, monitor.rect))
}

#[derive(Clone)]
pub(super) enum Receipt {
    Morning(u32),
    Wrap(i64),
    Download(super::downloads::NoticeTime),
    Reminder(Notice),
    Battery(String),
    AgentWait(crate::agents::Wait),
    AgentResult(Instant),
}

impl Receipt {
    fn matches(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Morning(a), Self::Morning(b)) => a == b,
            (Self::Wrap(a), Self::Wrap(b)) => a == b,
            (Self::Reminder(a), Self::Reminder(b)) => a.ids == b.ids,
            (Self::Battery(a), Self::Battery(b)) => a == b,
            (Self::AgentWait(a), Self::AgentWait(b)) => a == b,
            _ => false,
        }
    }

    fn valid(&self, today: u32, day: i64, now: Instant) -> bool {
        match self {
            Self::Morning(date) => *date == today,
            Self::Wrap(date) => *date == day,
            Self::Download(time) => time.fresh(now),
            Self::Reminder(_) | Self::Battery(_) | Self::AgentWait(_) | Self::AgentResult(_) => true,
        }
    }

    fn enabled(&self, reminders: bool) -> bool {
        reminders || !matches!(self, Self::Reminder(_))
    }
}

const MAX_TOAST_ATTEMPTS: u32 = 8;
const RESULT_GROUP_WINDOW: Duration = Duration::from_secs(2);

struct Pending {
    id: u64,
    card: Card,
    receipt: Receipt,
    deferred: bool,
    during_absence: bool,
    attempts: u32,
    retry: Option<Instant>,
}

impl Pending {
    fn result_target(&self) -> Option<&crate::agents::Target> {
        match &self.card.lines.first()?.route {
            Route::AgentResult(target) => Some(target),
            _ => None,
        }
    }

    fn group_ready_at(&self, now: Instant) -> Instant {
        match self.receipt {
            Receipt::AgentResult(at) if !self.deferred => at + RESULT_GROUP_WINDOW,
            _ => now,
        }
    }

    fn ready_at(&self, now: Instant) -> Instant {
        let due = self.group_ready_at(now);
        self.retry.map_or(due, |retry| retry.max(due))
    }

    fn result_groups_with(&self, first: &Self) -> bool {
        match (&self.receipt, &first.receipt) {
            (Receipt::AgentResult(at), Receipt::AgentResult(start)) => *at >= *start && *at <= *start + RESULT_GROUP_WINDOW,
            _ => false,
        }
    }

    fn record_toast_attempt(&mut self, now: Instant) {
        self.attempts += 1;
        let seconds = (5 << (self.attempts - 1)).min(300);
        self.retry = Some(now + Duration::from_secs(seconds));
    }
}

#[derive(Default)]
struct History {
    day: Option<i64>,
    entries: VecDeque<(u64, Line, bool)>,
}

impl History {
    fn roll(&mut self, day: i64) {
        if self.day != Some(day) {
            self.entries.clear();
            self.day = Some(day);
        }
    }

    fn push(&mut self, id: u64, mut line: Line, at: i64, read: bool) {
        if !matches!(line.route, Route::AgentWait(_) | Route::AgentResult(_)) {
            line.value = crate::topbar::panels::menu_time_local(at);
        }
        if self.entries.len() == 20 {
            self.entries.pop_back();
        }
        self.entries.push_front((id, line, read));
    }

    fn unread(&self) -> usize {
        self.entries.iter().filter(|(_, _, read)| !read).count()
    }

    fn read_all(&mut self) {
        for (_, _, read) in &mut self.entries {
            *read = true;
        }
    }

    fn read(&mut self, ids: &[u64]) {
        for (id, _, read) in &mut self.entries {
            *read |= ids.contains(id);
        }
    }

    fn card(&self) -> Card {
        let mut card = news_card(Kind::News);
        card.lines
            .extend(self.entries.iter().map(|(_, line, _)| line.clone()));
        card
    }
}

fn news_card(kind: Kind) -> Card {
    Card {
        kind,
        title: crate::i18n::t("assistant.news").into(),
        emphasis: None,
        lines: vec![],
        buttons: vec![],
    }
}

fn summary(card: &Card) -> Line {
    if card.kind == Kind::Agent {
        return card.lines[0].clone();
    }
    if matches!(card.kind, Kind::Reminder | Kind::Battery | Kind::Download) {
        if let Some(first) = card.lines.first() {
            let label = if first.value.is_empty() {
                first.label.clone()
            } else {
                format!("{} {}", first.label, first.value)
            };
            let route = if card.kind == Kind::Download {
                card.buttons
                    .last()
                    .map_or_else(|| first.route.clone(), |b| b.route.clone())
            } else {
                first.route.clone()
            };
            return Line::new(card.kind.glyph(), label, route);
        }
    }
    let route = card
        .emphasis
        .as_ref()
        .map(|e| e.route.clone())
        .or_else(|| card.lines.first().map(|line| line.route.clone()))
        .unwrap_or(Route::Close);
    Line::new(card.kind.glyph(), card.title.clone(), route)
}

fn batch(items: &[&Pending]) -> (Card, Vec<u64>) {
    let morning = items.iter().find(|p| p.card.kind == Kind::Morning);
    let mut card = morning.map_or_else(|| news_card(Kind::Bundle), |p| p.card.clone());
    if items.iter().all(|p| matches!(p.receipt, Receipt::AgentResult(_))) {
        card.title.clear();
    }
    let extra = items.iter().filter(|p| p.card.kind != Kind::Morning);
    let count = extra.clone().count();
    let mut shown: Vec<_> = morning.into_iter().map(|p| p.id).collect();
    for p in extra.take(5) {
        card.lines.push(summary(&p.card));
        shown.push(p.id);
    }
    if count > 5 {
        card.lines.push(Line::new(
            Mark::Inbox,
            format!("+{}", count - 5),
            Route::News,
        ));
    }
    (card, shown)
}

#[derive(Default)]
struct News {
    pending: VecDeque<Pending>,
    exhausted: Vec<Receipt>,
    history: History,
    next_id: u64,
    open: bool,
    history_anchor: Option<HistoryAnchor>,
    missing_anchor_since: Option<Instant>,
    returned_at: Option<Instant>,
    replay: Option<Replay>,
    toast: Option<ToastFlight>,
}

struct Replay {
    view: card::Interrupted,
    morning_date: Option<u32>,
}

struct ToastFlight {
    ids: Vec<u64>,
    result: mpsc::Receiver<bool>,
    agent_tag: Option<String>,
}

enum ToastPayload {
    Reminder(Notice),
    Agent(super::agent_toast::Toast),
    Text { title: String, body: String },
}

impl ToastPayload {
    fn send(self) -> bool {
        match self {
            Self::Reminder(notice) => {
                use windows::Win32::System::WinRT::{
                    RoInitialize, RoUninitialize, RO_INIT_MULTITHREADED,
                };
                let initialized = unsafe { RoInitialize(RO_INIT_MULTITHREADED) }.is_ok();
                let shown = crate::notes::toast::show(&notice);
                if initialized {
                    unsafe {
                        RoUninitialize();
                    }
                }
                shown
            }
            Self::Text { title, body } => {
                !cfg!(test) && crate::updates::try_notice_now(&title, &body)
            }
            Self::Agent(toast) => toast.send(),
        }
    }
}

fn toast_payload(items: &[&Pending]) -> Option<ToastPayload> {
    let first = items.first()?;
    if let Receipt::Reminder(notice) = &first.receipt {
        if items.len() == 1 {
            return Some(ToastPayload::Reminder(notice.clone()));
        }
        let ids = items
            .iter()
            .filter_map(|p| match &p.receipt {
                Receipt::Reminder(n) => Some(n.ids.iter().cloned()),
                _ => None,
            })
            .flatten()
            .collect();
        return Some(ToastPayload::Reminder(Notice::summary(ids, true)));
    }
    let grouped = (items.len() > 1).then(|| batch(items).0);
    let card = grouped.as_ref().unwrap_or(&first.card);
    let body = card
        .lines
        .iter()
        .map(|line| {
            if line.value.is_empty() {
                line.label.clone()
            } else {
                format!("{} {}", line.label, line.value)
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    let title = if card.title.is_empty() { crate::i18n::t("assistant.news").into() } else { card.title.clone() };
    if matches!(first.receipt, Receipt::AgentWait(_) | Receipt::AgentResult(_)) {
        super::agent_toast::Toast::new(card, title, body).map(ToastPayload::Agent)
    } else {
        Some(ToastPayload::Text { title, body })
    }
}

thread_local! { static NEWS: RefCell<News> = RefCell::new(News::default()); }

pub(super) fn add(card: Card, receipt: Receipt) -> bool {
    add_with_presence(card, receipt, super::living::away())
}

pub(super) fn add_with_presence(card: Card, receipt: Receipt, away: bool) -> bool {
    NEWS.with(|news| {
        let mut news = news.borrow_mut();
        if news.exhausted.iter().any(|old| old.matches(&receipt)) {
            return false;
        }
        if let Some(old) = news
            .pending
            .iter_mut()
            .find(|p| p.receipt.matches(&receipt))
        {
            if matches!(receipt, Receipt::Battery(_)) {
                old.card = card;
            }
            return false;
        }
        news.next_id += 1;
        let id = news.next_id;
        let during_absence = away || matches!(&receipt, Receipt::Download(time) if time.away());
        let deferred = (!during_absence && matches!(&receipt, Receipt::Reminder(n) if n.missed))
            || matches!(&receipt, Receipt::Download(time) if time.long_return());
        news.pending.push_back(Pending {
            id,
            card,
            receipt,
            deferred,
            during_absence,
            attempts: 0,
            retry: None,
        });
        true
    })
}

pub(super) fn waiting(receipt: &Receipt) -> bool {
    NEWS.with(|news| {
        let news = news.borrow();
        matches!(receipt, Receipt::Morning(date) if news.replay.as_ref().is_some_and(|p| p.morning_date == Some(*date)))
            || news.pending.iter().map(|p| &p.receipt).chain(&news.exhausted).any(|old| old.matches(receipt))
    })
}

pub(super) fn retain(keep: impl Fn(&Receipt) -> bool) {
    NEWS.with(|news| {
        let mut news = news.borrow_mut();
        news.pending.retain(|p| keep(&p.receipt));
        news.exhausted.retain(keep);
    });
}

pub(super) fn resolve_waits(alive: impl Fn(&crate::agents::Wait) -> bool) {
    NEWS.with(|news| news.borrow_mut().resolve_waits(&alive));
    card::resolve_waits(&alive);
    super::agent_toast::resolve_waits(alive);
}

pub(super) fn suppress_pending_wait(wait: &crate::agents::Wait) {
    NEWS.with(|news| {
        news.borrow_mut().pending.retain(|p| {
            !matches!(&p.receipt, Receipt::AgentWait(pending) if pending == wait)
        });
    });
}

pub(super) fn returned() {
    NEWS.with(|news| news.borrow_mut().returned_at = Some(Instant::now()));
}

pub(super) fn unread() -> usize {
    NEWS.with(|news| {
        news.try_borrow().ok().map_or(0, |news| {
            let day = crate::notes::time::now().div_euclid(86400);
            if news.history.day == Some(day) {
                news.history.unread()
            } else {
                0
            }
        })
    })
}

pub(super) fn open() {
    NEWS.with(|news| news.borrow_mut().open = true);
    super::refresh();
}

pub(super) fn shutdown() {
    NEWS.with(|state| *state.borrow_mut() = News::default());
}

pub(super) struct Context<'a> {
    pub gfx: &'a Rc<crate::gfx::Gfx>,
    pub enabled: bool,
    pub topbar: bool,
    pub body: Option<card::Anchor>,
    pub body_expected: bool,
    pub light: bool,
    pub motion: bool,
    pub return_inputs_loading: bool,
}

#[derive(Default)]
pub(super) struct Output {
    pub slot: Option<SlotAnchor>,
    pub body: bool,
    pub next: Option<u32>,
    greet: Option<Option<u32>>,
}

pub(super) fn poll(ctx: Context<'_>) -> Output {
    let local = crate::notes::time::now();
    let day = local.div_euclid(86400);
    let today = super::living::local_calendar().date;
    let now = Instant::now();
    let (output, done) = NEWS.with(|state| {
        let Ok(mut news) = state.try_borrow_mut() else {
            return (Output::default(), vec![]);
        };
        if news.history.day != Some(day) {
            news.replay = None;
            if card::is_history() {
                card::close();
            }
        }
        news.history.roll(day);
        news.history.read(&card::take_read());
        news.pending.retain(|p| {
            p.receipt.valid(today, day, now) && p.result_target().is_none_or(crate::agents::show_result_for)
        });
        news.exhausted
            .retain(|receipt| receipt.valid(today, day, now));
        let mut done = news.poll_toast(local);
        news.retire_exhausted(local);
        let mut output = news.present(&ctx, now, local, today, &mut done);
        if !news.history.entries.is_empty() || card::is_history() {
            let midnight = ((86400 - local.rem_euclid(86400)) * 1000) as u32;
            output.next = Some(output.next.map_or(midnight, |ms| ms.min(midnight)));
        }
        (output, done)
    });
    for (receipt, shown) in done {
        acknowledge(receipt, shown);
    }
    if let Some(date) = output.greet {
        set_greet(date);
    }
    output
}

type Acknowledgement = (Receipt, Vec<(String, i64)>);

struct Shown {
    read_ids: Vec<u64>,
    done: Vec<Acknowledgement>,
}

impl News {
    fn resolve_waits(&mut self, alive: impl Fn(&crate::agents::Wait) -> bool) {
        let keep = |receipt: &Receipt| !matches!(receipt, Receipt::AgentWait(wait) if !alive(wait));
        self.pending.retain(|p| keep(&p.receipt));
        self.exhausted.retain(keep);
        self.history.entries.retain(|(_, line, _)| {
            !matches!(&line.route, Route::AgentWait(wait) if !alive(wait))
        });
        if let Some(replay) = &mut self.replay {
            card::retain_waits(&mut replay.view.card, alive);
        }
    }

    fn deliver(
        &mut self,
        ids: &[u64],
        local: i64,
        carries: &[(String, i64)],
    ) -> Vec<Acknowledgement> {
        let mut done = vec![];
        self.pending.retain(|p| {
            if !ids.contains(&p.id) {
                return true;
            }
            self.history.push(p.id, summary(&p.card), local, false);
            let carried = if matches!(p.receipt, Receipt::Morning(_)) {
                carries.to_vec()
            } else {
                vec![]
            };
            done.push((p.receipt.clone(), carried));
            false
        });
        done
    }

    fn next_batch(&self, reminders: bool, now: Instant) -> Vec<&Pending> {
        let available = || {
            self.pending.iter().filter(|p| {
                p.receipt.enabled(reminders)
                    && self
                        .toast
                        .as_ref()
                        .is_none_or(|flight| !flight.ids.contains(&p.id))
            })
        };
        let Some(first) = available().find(|p| p.group_ready_at(now) <= now) else {
            return vec![];
        };
        if first.deferred || first.card.kind == Kind::Morning && available().any(|p| p.deferred) {
            available()
                .filter(|p| p.deferred || p.card.kind == Kind::Morning)
                .collect()
        } else if matches!(first.receipt, Receipt::AgentResult(_)) {
            available().filter(|p| !p.deferred && p.result_groups_with(first)).collect()
        } else if matches!(first.card.kind, Kind::Download | Kind::Reminder) {
            available()
                .skip_while(|p| p.id != first.id)
                .take_while(|p| p.card.kind == first.card.kind && !p.deferred)
                .collect()
        } else {
            vec![first]
        }
    }

    fn take_history_request(&mut self, showing: bool) -> Option<HistoryRequest> {
        if std::mem::take(&mut self.open) {
            Some(HistoryRequest::Open)
        } else {
            showing.then_some(HistoryRequest::Keep)
        }
    }

    fn history_target(
        &self,
        request: HistoryRequest,
        open_slot: impl FnOnce() -> Option<SlotPlacement>,
        keep_slot: impl FnOnce(SlotAnchor) -> Option<SlotPlacement>,
        tray: impl FnOnce() -> Option<SlotPlacement>,
        covered: impl FnOnce(SlotAnchor) -> bool,
    ) -> Option<HistoryAnchor> {
        if request == HistoryRequest::Open {
            return open_slot()
                .map(HistoryAnchor::Bar)
                .or_else(|| tray().map(HistoryAnchor::Tray));
        }
        let anchor = self.history_anchor?;
        let (slot, _) = anchor.placement();
        if covered(slot) {
            return None;
        }
        match anchor {
            HistoryAnchor::Bar(_) => keep_slot(slot).map(HistoryAnchor::Bar),
            HistoryAnchor::Tray(_) => Some(anchor),
        }
    }

    fn plan_presentation<T>(
        &mut self,
        now: Instant,
        state: PresentationState,
        history: impl FnOnce(&Self, HistoryRequest) -> Option<HistoryAnchor>,
        automatic: impl FnOnce(&mut Self) -> T,
    ) -> Presentation<T> {
        let manual = self.take_history_request(state.history_showing);
        if state.idle && (state.enabled || manual.is_some()) {
            return Presentation::Stop;
        }
        if let Some(request) = manual {
            return if state.enabled {
                Presentation::History(request, history(self, request))
            } else {
                Presentation::Stop
            };
        }
        self.history_anchor = None;
        if state.enabled
            && self.returned_at.is_some_and(|at| {
                let elapsed = now.saturating_duration_since(at);
                elapsed < Duration::from_secs(1)
                    || elapsed < Duration::from_secs(3) && state.return_inputs_loading
            })
        {
            return Presentation::WaitForReturn;
        }
        Presentation::Automatic(automatic(self))
    }

    fn select_delivery(
        &mut self,
        destination: Destination,
        now: Instant,
        waiting: bool,
        interrupted: impl FnOnce() -> bool,
        tray: impl FnOnce() -> Option<SlotPlacement>,
    ) -> Delivery {
        if destination != Destination::WaitAnchor {
            self.missing_anchor_since = None;
        }
        let route = match destination {
            Destination::Body => Delivery::Body,
            Destination::Slot => Delivery::Slot,
            Destination::Toast => Delivery::Toast,
            Destination::WaitAnchor => {
                if !waiting && self.missing_anchor_since.is_none() {
                    return Delivery::Idle;
                }
                if let Some(ms) = self.anchor_retry(now) {
                    return Delivery::WaitAnchor(ms);
                }
                tray().map_or(Delivery::Toast, Delivery::Tray)
            }
        };
        if matches!(route, Delivery::Body | Delivery::Slot | Delivery::Tray(_)) && interrupted() {
            self.pending.iter_mut().for_each(|p| p.deferred = true);
            // State changes normally wake the dock. Recheck in case one was missed.
            return Delivery::Defer(waiting.then_some(30_000));
        }
        route
    }

    fn anchor_retry(&mut self, now: Instant) -> Option<u32> {
        let since = *self.missing_anchor_since.get_or_insert(now);
        let elapsed = now.saturating_duration_since(since);
        let left = Duration::from_secs(5).checked_sub(elapsed)?;
        if left.is_zero() {
            return None;
        }
        let delay = if elapsed < Duration::from_secs(1) { 1000 } else { 2000 };
        Some(delay.min((left.as_millis() as u32).max(1)))
    }

    fn show_pending(
        &mut self,
        reminders: bool,
        local: i64,
        now: Instant,
        show: impl FnOnce(Card) -> bool,
    ) -> Option<Shown> {
        let items = self.next_batch(reminders, now);
        let first = items.first()?;
        let (card, read_ids) = if items.len() == 1 {
            (first.card.clone(), vec![first.id])
        } else {
            batch(&items)
        };
        let ids: Vec<_> = items.iter().map(|p| p.id).collect();
        let carries = card.shown_carries();
        if !show(card) {
            return None;
        }
        Some(Shown {
            read_ids,
            done: self.deliver(&ids, local, &carries),
        })
    }

    fn finish_absence(&mut self) {
        let long_return = self.returned_at.take().is_some();
        for p in &mut self.pending {
            p.deferred |= long_return && p.during_absence;
            p.during_absence = false;
        }
    }

    fn remember_interruption(
        &mut self,
        view: card::Interrupted,
        today: u32,
    ) -> Option<Option<u32>> {
        let morning_date = (view.card.kind == Kind::Morning).then_some(today);
        self.replay = Some(Replay { view, morning_date });
        morning_date.map(|_| None)
    }

    fn show_replay(&mut self, show: impl FnOnce(Card) -> bool) -> Option<Replay> {
        let replay = self.replay.as_ref()?;
        if !show(replay.view.card.clone()) {
            return None;
        }
        self.replay.take()
    }

    fn present(
        &mut self,
        ctx: &Context<'_>,
        now: Instant,
        local: i64,
        today: u32,
        done: &mut Vec<Acknowledgement>,
    ) -> Output {
        let mut output = Output::default();
        let preferences = crate::notes::preferences();
        let reminders = preferences.notes && preferences.notes_reminders;
        let pending = self.pending.iter().any(|p| p.receipt.enabled(reminders));
        if !pending
            && !self.open
            && !card::active()
            && self.toast.is_none()
            && self.returned_at.is_none()
            && self.replay.is_none()
        {
            self.missing_anchor_since = None;
            return output;
        }
        let plan = self.plan_presentation(
            now,
            PresentationState {
                enabled: ctx.enabled,
                idle: crate::dock::session_idle(),
                history_showing: card::is_history(),
                return_inputs_loading: ctx.return_inputs_loading,
            },
            |news, request| news.history_target(
                request,
                || available_slot(card::current_slot()),
                |slot| crate::topbar::companion_news_anchor(Some(slot)),
                || crate::dock::companion_tray_news_anchor(true),
                history_anchor_covered,
            ),
            |news| {
                let slot = available_slot(card::current_slot());
                let chat = super::chat::busy();
                let blocked = chat || crate::dock::companion_mode() != super::visibility::Mode::Run;
                let route = news.select_delivery(
                    destination(ctx.enabled, ctx.topbar, ctx.body.is_some(), ctx.body_expected, slot.is_some()),
                    now,
                    pending || news.replay.is_some(),
                    || super::notifications_quiet() || crate::monitors::all()
                        .iter()
                        .any(|m| crate::dock::fullscreen_on_monitor(m.handle, m.rect)),
                    || if blocked { None } else { crate::dock::companion_tray_news_anchor(false) },
                );
                (route, slot, chat, blocked)
            },
        );
        let (route, slot, chat, blocked) = match plan {
            Presentation::Stop => {
                self.history_anchor = None;
                card::close();
                return output;
            }
            Presentation::History(request, target) => {
                let anchor = target.map(|target| {
                    let (slot, work) = target.placement();
                    card::Anchor::Slot { slot, work }
                });
                if request == HistoryRequest::Open || anchor.is_none() {
                    card::close();
                }
                if let Some(anchor) = anchor {
                    if request == HistoryRequest::Open {
                        card::show_at(ctx.gfx, self.history.card(), anchor);
                    }
                    if card::anchor_at(anchor, ctx.light, ctx.motion) {
                        self.history.read_all();
                    }
                }
                self.history_anchor = target.filter(|_| card::is_history());
                let follow_up = pending || self.returned_at.is_some() || self.replay.is_some();
                output.next = card::interval().or_else(|| follow_up.then_some(100));
                return output;
            }
            Presentation::WaitForReturn => {
                output.next = Some(100);
                return output;
            }
            Presentation::Automatic(automatic) => automatic,
        };

        match route {
            Delivery::Defer(next) => {
                card::close();
                output.next = next;
                return output;
            }
            Delivery::WaitAnchor(ms) => {
                card::close();
                output.next = Some(ms);
                return output;
            }
            Delivery::Idle => {
                self.finish_absence();
                card::close();
                return output;
            }
            Delivery::Toast => {
                self.replay = None;
                self.finish_absence();
                card::close();
                output.next = self.toast(now, reminders);
                return output;
            }
            Delivery::Body | Delivery::Slot | Delivery::Tray(_) => {}
        }
        self.finish_absence();
        if blocked {
            if chat {
                if let Some(view) = card::interrupt_for_chat() {
                    output.greet = self.remember_interruption(view, today);
                }
            }
            output.next = (pending || card::active() || self.replay.is_some()).then_some(100);
            return output;
        }

        let target = if let Delivery::Tray((slot, work)) = route {
            Some(card::Anchor::Slot { slot, work })
        } else if matches!(route, Delivery::Slot) || slot.is_some() && card::current_slot().is_some()
        {
            slot.map(|(slot, work)| card::Anchor::Slot { slot, work })
        } else {
            ctx.body
        };
        let Some(target) = target else {
            return output;
        };
        let show = |new_card| {
            card::show_at(ctx.gfx, new_card, target)
                && card::anchor_at(target, ctx.light, ctx.motion)
        };
        let shown = if card::active() {
            card::anchor_at(target, ctx.light, ctx.motion);
            false
        } else if self.replay.is_some() {
            self.show_replay(show).is_some_and(|replay| {
                card::track_read(replay.view.read_ids);
                output.greet = replay.morning_date.map(Some);
                true
            })
        } else {
            self.show_pending(reminders, local, now, show)
                .is_some_and(|shown| {
                    card::track_read(shown.read_ids);
                    done.extend(shown.done);
                    true
                })
        };
        if shown {
            output.slot = target.slot().filter(|_| ctx.motion && !matches!(route, Delivery::Tray(_)));
            output.body = target.slot().is_none();
        }
        output.next = card::interval().or_else(|| {
            (self.replay.is_some() || pending).then_some(1000)
        });
        output
    }

    fn retire_exhausted(&mut self, local: i64) {
        self.pending.retain(|p| {
            if p.attempts < MAX_TOAST_ATTEMPTS
                || self
                    .toast
                    .as_ref()
                    .is_some_and(|flight| flight.ids.contains(&p.id))
            {
                return true;
            }
            self.history.push(p.id, summary(&p.card), local, false);
            if !matches!(p.receipt, Receipt::Download(_) | Receipt::AgentResult(_)) {
                self.exhausted.push(p.receipt.clone());
            }
            false
        });
    }

    fn poll_toast(&mut self, local: i64) -> Vec<Acknowledgement> {
        let Some(flight) = &self.toast else {
            return vec![];
        };
        let success = match flight.result.try_recv() {
            Ok(ok) => ok,
            Err(mpsc::TryRecvError::Disconnected) => false,
            Err(mpsc::TryRecvError::Empty) => return vec![],
        };
        let ToastFlight { ids, agent_tag, .. } = self.toast.take().unwrap();
        if let Some(tag) = agent_tag {
            super::agent_toast::finished(tag, success);
        }
        if !success {
            return vec![];
        }
        let carries: Vec<_> = self
            .pending
            .iter()
            .filter(|p| ids.contains(&p.id) && matches!(p.receipt, Receipt::Morning(_)))
            .flat_map(|p| p.card.shown_carries())
            .collect();
        self.deliver(&ids, local, &carries)
    }

    fn toast_batch(&self, now: Instant, reminders: bool) -> Vec<&Pending> {
        let available = || {
            self.pending.iter().filter(|p| {
                p.receipt.enabled(reminders)
                    && p.attempts < MAX_TOAST_ATTEMPTS
                    && p.retry.is_none_or(|at| at <= now)
            })
        };
        let Some(first) = available().find(|p| p.ready_at(now) <= now) else {
            return vec![];
        };
        if matches!(&first.receipt, Receipt::Reminder(n) if n.startup_missed) {
            available()
                .filter(|p| matches!(&p.receipt, Receipt::Reminder(n) if n.startup_missed))
                .take(128)
                .collect()
        } else if matches!(first.receipt, Receipt::AgentResult(_)) {
            available().filter(|p| p.result_groups_with(first)).collect()
        } else {
            vec![first]
        }
    }

    fn toast(&mut self, now: Instant, reminders: bool) -> Option<u32> {
        if self.toast.is_some() {
            return None;
        }
        let items = self.toast_batch(now, reminders);
        if let Some(payload) = toast_payload(&items) {
            let ids: Vec<_> = items.iter().map(|p| p.id).collect();
            let agent_tag = match &payload {
                ToastPayload::Agent(toast) => Some(toast.track()),
                _ => None,
            };
            for pending in self.pending.iter_mut().filter(|p| ids.contains(&p.id)) {
                pending.record_toast_attempt(now);
            }
            let (tx, result) = mpsc::channel();
            let worker = std::thread::Builder::new()
                .name("companion-toast".into())
                .spawn(move || {
                    let _ = tx.send(payload.send());
                    super::refresh();
                });
            self.toast = Some(ToastFlight { ids, result, agent_tag });
            if worker.is_err() {
                // A failed spawn drops the sender. Handle it like a failed delivery.
                super::refresh();
            }
            return None;
        }
        self.pending
            .iter()
            .filter(|p| p.receipt.enabled(reminders) && p.attempts < MAX_TOAST_ATTEMPTS)
            .map(|p| p.ready_at(now))
            .min()
            .map(|at| (at.saturating_duration_since(now).as_millis() as u32).max(100))
    }
}

fn set_greet(date: Option<u32>) {
    super::APP.with(|app| {
        if let Some(app) = app.borrow_mut().as_mut() {
            app.cfg.last_greet = date;
            app.pending_greet = Some(date);
        }
    });
}

fn acknowledge(receipt: Receipt, carries: Vec<(String, i64)>) {
    match receipt {
        Receipt::Morning(date) => {
            set_greet(Some(date));
            if !carries.is_empty() {
                crate::notes::runtime::action(Action::BriefingShown(carries));
            }
        }
        Receipt::Wrap(day) => super::APP.with(|app| {
            if let Some(app) = app.borrow_mut().as_mut() {
                app.cfg.routine.judged(day);
            }
        }),
        Receipt::Reminder(notice) => crate::notes::runtime::action(Action::Delivered(notice)),
        Receipt::Battery(id) => super::device_battery::delivered(id),
        Receipt::Download(_) | Receipt::AgentWait(_) | Receipt::AgentResult(_) => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_wait_reuses_body_slot_defer_and_windows_delivery() {
        let now = Instant::now();
        let session = crate::agents::test_session(crate::agents::Agent::Claude, "project", crate::agents::St::Wait, now, false);
        let card = super::super::agent_news::wait_card(&session);
        let mut news = News::default();
        news.pending.push_back(pending(1, card, Receipt::AgentWait(session.wait()), false));
        for (enabled, topbar, body, slot) in [(true, true, true, false), (true, true, false, true)] {
            let route = destination(enabled, topbar, body, body, slot);
            assert!(matches!(news.select_delivery(route, now, true, || true, || None), Delivery::Defer(Some(_))));
            assert!(news.pending[0].deferred);
            let chosen = news.select_delivery(route, now, true, || false, || None);
            assert!(if body { matches!(chosen, Delivery::Body) } else { matches!(chosen, Delivery::Slot) });
        }
        for (enabled, topbar) in [(false, true), (true, false), (false, false)] {
            let route = destination(enabled, topbar, false, false, false);
            assert!(matches!(news.select_delivery(route, now, true, || false, || None), Delivery::Toast));
            assert!(matches!(toast_payload(&news.toast_batch(now, true)), Some(ToastPayload::Agent(_))));
        }
    }

    #[test]
    fn completed_sessions_share_one_card_and_keep_hidden_rows_unread() {
        let now = Instant::now();
        let due = now + RESULT_GROUP_WINDOW;
        let mut news = News::default();
        for n in 0..7 {
            let at = now + Duration::from_millis(n);
            let session = crate::agents::test_session(crate::agents::Agent::Codex, "project", crate::agents::St::Done, at, false);
            let mut card = news_card(Kind::Agent);
            card.lines.push(Line::new(Mark::Agent(session.agent), format!("Project {n}"), Route::AgentResult(session.target())));
            news.pending.push_back(pending(n, card, Receipt::AgentResult(at), false));
        }
        assert!(news.next_batch(true, now).is_empty());
        assert_eq!(news.toast_batch(due, true).len(), 7);
        let shown = news.show_pending(true, 0, due, |card| {
            assert_eq!(card.lines.len(), 6);
            assert!(card.lines[..5].iter().all(|row| matches!(row.route, Route::AgentResult(_))));
            assert_eq!(card.lines[5].label, "+2");
            assert!(matches!(card.lines[5].route, Route::News));
            true
        }).unwrap();
        news.history.read(&shown.read_ids);
        assert_eq!(news.history.unread(), 2);
    }

    #[test]
    fn resolved_wait_is_removed_from_deferred_queue_and_unread_news() {
        let now = Instant::now();
        let session = crate::agents::test_session(crate::agents::Agent::Claude, "project", crate::agents::St::Wait, now, false);
        let wait = session.wait();
        let mut card = news_card(Kind::Agent);
        card.lines.push(Line::new(Mark::Agent(session.agent), "Project", Route::AgentWait(wait.clone())));
        let mut news = News::default();
        news.pending.push_back(pending(1, card.clone(), Receipt::AgentWait(wait.clone()), true));
        news.exhausted.push(Receipt::AgentWait(wait));
        news.history.push(2, summary(&card), 0, false);
        news.history.push(3, summary(&news_card(Kind::Battery)), 0, false);
        assert_eq!(news.history.unread(), 2);
        news.resolve_waits(|_| false);
        assert!(news.pending.is_empty() && news.exhausted.is_empty());
        assert_eq!(news.history.unread(), 1);
    }
    #[test]
    fn missing_slot_waits_and_body_does_not_need_a_bar() {
        use Destination::*;
        assert_eq!(destination(true, false, true, true, false), Body);
        assert_eq!(destination(true, true, true, true, false), Body);
        assert_eq!(destination(true, true, false, false, false), WaitAnchor);
        assert_eq!(destination(true, true, false, false, true), Slot);
        assert_eq!(destination(true, false, false, false, false), Toast);
        assert_eq!(destination(false, true, false, false, false), Toast);
    }

    #[test]
    fn present_opens_history_before_automatic_delivery() {
        let now = Instant::now();
        let bar = placement(10);
        for destination in [
            Destination::Slot,
            Destination::Toast,
            Destination::WaitAnchor,
        ] {
            let mut news = News {
                open: true,
                ..Default::default()
            };
            news.pending.push_back(device(1));
            let plan = news.plan_presentation(
                now,
                presentation_state(),
                |news, request| {
                    news.history_target(
                        request,
                        || Some(bar),
                        |_| unreachable!(),
                        || None,
                        |_| false,
                    )
                },
                |news| news.select_delivery(destination, now, true, || true, || None),
            );
            assert!(matches!(plan,
                Presentation::History(HistoryRequest::Open, Some(HistoryAnchor::Bar((slot, _))))
                    if slot.rect == bar.0.rect
            ));
            assert!(!news.pending[0].deferred);
            assert!(news.missing_anchor_since.is_none());
        }
    }

    #[test]
    fn a_manual_open_without_an_anchor_does_not_reopen_when_defer_ends() {
        let now = Instant::now();
        let mut news = News {
            open: true,
            ..Default::default()
        };
        news.pending.push_back(device(1));
        let present = |news: &mut News, slot: Option<SlotPlacement>, quiet| {
            news.plan_presentation(
                now,
                presentation_state(),
                |news, request| news.history_target(request, || slot, |_| None, || None, |_| false),
                |news| news.select_delivery(Destination::Slot, now, true, || quiet, || None),
            )
        };
        assert!(matches!(
            present(&mut news, None, true),
            Presentation::History(HistoryRequest::Open, None)
        ));
        let slot = Some(placement(10));
        assert!(matches!(
            present(&mut news, slot, true),
            Presentation::Automatic(Delivery::Defer(Some(30_000)))
        ));
        assert!(matches!(
            present(&mut news, slot, false),
            Presentation::Automatic(Delivery::Slot)
        ));
    }

    #[test]
    fn keep_checks_the_original_monitor_and_uses_tray_only_when_opening() {
        let bar = placement(10);
        let moved_bar = placement(20);
        let tray = placement(500);
        let mut news = News {
            history_anchor: Some(HistoryAnchor::Bar(bar)),
            ..Default::default()
        };
        for (covered, slot) in [
            (true, Some(moved_bar)),
            (false, None),
            (false, Some(moved_bar)),
        ] {
            let keep = news.history_target(
                HistoryRequest::Keep,
                || panic!("Keep must not look on another bar"),
                |old| {
                    assert!(!covered);
                    assert_eq!(old.rect, bar.0.rect);
                    slot
                },
                || panic!("Keep must not find a new tray anchor"),
                |old| {
                    assert_eq!(old.rect, bar.0.rect);
                    covered
                },
            );
            if covered || slot.is_none() {
                assert!(keep.is_none());
            } else {
                assert!(
                    matches!(keep, Some(HistoryAnchor::Bar((slot, _))) if slot.rect == moved_bar.0.rect)
                );
            }
        }
        let opened = news.history_target(
            HistoryRequest::Open,
            || None,
            |_| None,
            || Some(tray),
            |_| false,
        );
        assert!(matches!(opened, Some(HistoryAnchor::Tray((slot, _))) if slot.rect == tray.0.rect));
        news.history_anchor = opened;
        for covered in [false, true] {
            let keep = news.history_target(
                HistoryRequest::Keep,
                || panic!("Keep must not find a new bar"),
                |_| panic!("A tray card keeps its own placement"),
                || panic!("Keep must not find a new tray anchor"),
                |old| {
                    assert_eq!(old.rect, tray.0.rect);
                    covered
                },
            );
            if covered {
                assert!(keep.is_none());
            } else {
                assert!(
                    matches!(keep, Some(HistoryAnchor::Tray((slot, _))) if slot.rect == tray.0.rect)
                );
            }
        }
    }

    fn presentation_state() -> PresentationState {
        PresentationState {
            enabled: true,
            idle: false,
            history_showing: false,
            return_inputs_loading: false,
        }
    }

    fn placement(left: i32) -> SlotPlacement {
        let rect = RECT {
            left,
            top: 0,
            right: left + 24,
            bottom: 24,
        };
        (
            SlotAnchor { rect, dpi: 1. },
            RECT {
                left: 0,
                top: 0,
                right: 640,
                bottom: 480,
            },
        )
    }

    #[test]
    fn lock_and_return_wait_precede_automatic_delivery_when_enabled() {
        let at = Instant::now();
        let mut news = News {
            returned_at: Some(at),
            open: true,
            ..Default::default()
        };
        let present = |news: &mut News, elapsed, enabled, idle, return_inputs_loading| {
            let now = at + Duration::from_millis(elapsed);
            news.plan_presentation(
                now,
                PresentationState {
                    enabled,
                    idle,
                    return_inputs_loading,
                    ..presentation_state()
                },
                |_, _| panic!("The locked session must discard a manual request"),
                |news| {
                    news.select_delivery(
                        Destination::Toast,
                        now,
                        true,
                        || panic!("Toast ignores quiet"),
                        || None,
                    )
                },
            )
        };
        assert!(matches!(
            present(&mut news, 0, true, true, false),
            Presentation::Stop
        ));
        assert!(matches!(
            present(&mut news, 999, true, false, false),
            Presentation::WaitForReturn
        ));
        assert!(matches!(
            present(&mut news, 1000, true, false, false),
            Presentation::Automatic(Delivery::Toast)
        ));
        assert!(matches!(
            present(&mut news, 2999, true, false, true),
            Presentation::WaitForReturn
        ));
        assert!(matches!(
            present(&mut news, 3000, true, false, true),
            Presentation::Automatic(Delivery::Toast)
        ));
        assert!(matches!(
            present(&mut news, 0, false, true, true),
            Presentation::Automatic(Delivery::Toast)
        ));
    }

    #[test]
    fn disabled_companion_does_not_enqueue_briefing_wrap_or_downloads() {
        let now = Instant::now();
        let push = |kind, receipt| assert!(add_with_presence(news_card(kind), receipt, false));
        push(Kind::Battery, Receipt::Battery("keyboard".into()));
        push(
            Kind::Reminder,
            Receipt::Reminder(Notice::single("note".into(), "Reminder".into())),
        );
        let mut expected = vec![Kind::Battery, Kind::Reminder];
        for enabled in [false, true] {
            let loading = super::super::poll_news_sources(
                enabled,
                || {
                    push(Kind::Morning, Receipt::Morning(20261126));
                    true
                },
                || push(Kind::Wrap, Receipt::Wrap(20_000)),
                || {
                    push(
                        Kind::Download,
                        Receipt::Download(super::super::downloads::NoticeTime::present(now)),
                    );
                    false
                },
            );
            assert_eq!(loading, enabled);
            if enabled {
                expected.extend([Kind::Morning, Kind::Wrap, Kind::Download]);
            }
            NEWS.with(|news| {
                assert_eq!(
                    news.borrow()
                        .pending
                        .iter()
                        .map(|p| p.card.kind)
                        .collect::<Vec<_>>(),
                    expected
                );
            });
        }
        shutdown();
    }

    #[test]
    fn missing_slot_retries_until_five_seconds_then_uses_the_tray_or_toast() {
        let now = Instant::now();
        let mut news = News::default();
        let destination = destination(true, true, false, false, false);
        for (elapsed, next) in [(0, 1000), (1000, 2000), (3000, 2000), (4999, 1)] {
            let at = now + Duration::from_millis(elapsed);
            let route = news.select_delivery(
                destination,
                at,
                true,
                || panic!("No card yet"),
                || panic!("Tray before timeout"),
            );
            assert!(matches!(route, Delivery::WaitAnchor(ms) if ms == next));
        }
        let deadline = now + Duration::from_secs(5);
        let tray = placement(500);
        assert!(matches!(
            news.select_delivery(destination, deadline, true, || false, || Some(tray)),
            Delivery::Tray((slot, _)) if slot.rect == tray.0.rect
        ));
        assert!(matches!(
            news.select_delivery(destination, deadline, true, || true, || Some(tray)),
            Delivery::Defer(Some(30_000))
        ));
        assert!(matches!(
            news.select_delivery(
                destination,
                deadline,
                true,
                || panic!("Toast ignores quiet"),
                || None
            ),
            Delivery::Toast
        ));
    }

    #[test]
    fn waiting_for_a_body_recovers_before_the_deadline_and_can_time_out_later() {
        use super::super::{expects_news_body, Config};
        let cfg = Config {
            visible: true,
            ..Default::default()
        };
        let expected = expects_news_body(cfg);
        let missing = destination(cfg.enabled, false, false, expected, false);
        let body = destination(cfg.enabled, false, true, expected, false);
        let now = Instant::now();
        let mut news = News::default();
        let mut select = |destination, elapsed| {
            news.select_delivery(
                destination,
                now + Duration::from_secs(elapsed),
                true,
                || false,
                || None,
            )
        };
        assert!(matches!(select(missing, 0), Delivery::WaitAnchor(1000)));
        assert!(matches!(select(body, 1), Delivery::Body));
        assert!(matches!(select(missing, 4), Delivery::WaitAnchor(1000)));
        assert!(matches!(select(missing, 8), Delivery::WaitAnchor(1000)));
        assert!(matches!(select(missing, 9), Delivery::Toast));
    }

    #[test]
    fn windows_delivery_skips_quiet_when_either_surface_is_disabled() {
        let now = Instant::now();
        for (enabled, topbar) in [(false, true), (true, false)] {
            let mut news = News::default();
            news.pending.push_back(device(1));
            let route = news.select_delivery(
                destination(enabled, topbar, false, false, topbar),
                now,
                true,
                || panic!("Windows owns quiet mode"),
                || panic!("No anchor wait"),
            );
            assert!(matches!(route, Delivery::Toast));
            assert!(!news.pending[0].deferred);
        }
    }

    #[test]
    fn route_maps_the_body_visibility_conditions_used_by_draw() {
        use super::super::{expects_news_body, has_news_body, Config};
        let visible = Config {
            enabled: true,
            visible: true,
            ..Default::default()
        };
        let hidden = Config {
            visible: false,
            ..visible
        };
        let snoozed = Config {
            snooze_until: Some(10),
            ..visible
        };
        for (cfg, away, travelling, expected) in [
            (visible, false, false, Destination::Body),
            (hidden, false, false, Destination::Slot),
            (snoozed, false, false, Destination::Slot),
            (visible, true, false, Destination::Slot),
            (visible, false, true, Destination::Slot),
        ] {
            let outside = has_news_body(cfg, away, travelling);
            assert_eq!(
                destination(cfg.enabled, true, outside, expects_news_body(cfg), true),
                expected
            );
        }
    }

    #[test]
    fn defer_then_release_presents_pending_cards_only_once_after_success() {
        let mut news = News::default();
        let first = device(1);
        let second = device(2);
        news.pending.extend([first, second]);
        let now = Instant::now();
        assert!(matches!(
            news.select_delivery(Destination::Slot, now, true, || true, || None),
            Delivery::Defer(Some(30_000))
        ));
        assert!(news.pending.iter().all(|p| p.deferred));
        assert!(news.history.entries.is_empty());

        assert!(matches!(
            news.select_delivery(Destination::Slot, now, true, || false, || None),
            Delivery::Slot
        ));
        assert!(news.show_pending(true, 86400, Instant::now(), |_| false).is_none());
        assert_eq!(news.pending.len(), 2);
        let shown = news
            .show_pending(true, 86400, Instant::now(), |card| {
                assert_eq!(card.kind, Kind::Bundle);
                assert_eq!(card.lines.len(), 2);
                true
            })
            .unwrap();
        assert_eq!(shown.done.len(), 2);
        assert_eq!(shown.read_ids, [1, 2]);
        assert_eq!(news.history.unread(), 2);
        assert!(news
            .show_pending(true, 86400, Instant::now(), |_| panic!("already shown"))
            .is_none());
    }

    fn device(id: u64) -> Pending {
        pending(
            id,
            super::super::device_battery::card("Keyboard".into(), 17),
            Receipt::Battery(id.to_string()),
            false,
        )
    }

    #[test]
    fn only_a_successful_toast_acknowledges_and_inflight_items_cannot_be_shown() {
        for result in [Some(true), Some(false), None] {
            let mut news = News::default();
            news.pending.extend([device(1), device(2)]);
            let (tx, rx) = mpsc::channel();
            news.toast = Some(ToastFlight {
                ids: vec![1],
                result: rx,
                agent_tag: None,
            });
            assert!(news.poll_toast(86400).is_empty());
            assert_eq!(
                news.next_batch(true, Instant::now())
                    .iter()
                    .map(|p| p.id)
                    .collect::<Vec<_>>(),
                [2]
            );
            if let Some(result) = result {
                tx.send(result).unwrap();
            }
            drop(tx);
            let ack = news.poll_toast(86400);
            assert!(news.toast.is_none());
            if result == Some(true) {
                assert!(matches!(ack.as_slice(), [(Receipt::Battery(id), _)] if id == "1"));
                assert_eq!(news.history.unread(), 1);
                assert_eq!(news.pending.front().unwrap().id, 2);
            } else {
                assert!(ack.is_empty());
                assert!(news.history.entries.is_empty());
                assert_eq!(news.next_batch(true, Instant::now())[0].id, 1);
            }
            assert!(news.poll_toast(86400).is_empty());
        }
    }

    #[test]
    fn toast_backoff_caps_at_300_seconds_and_the_eighth_failure_goes_to_news() {
        let now = Instant::now();
        let mut news = News::default();
        news.pending.push_back(device(1));
        let mut at = now;
        for (index, seconds) in [5, 10, 20, 40, 80, 160, 300, 300].into_iter().enumerate() {
            assert_eq!(news.toast_batch(at, true).len(), 1);
            let p = news.pending.front_mut().unwrap();
            p.record_toast_attempt(at);
            assert_eq!(p.attempts, (index + 1) as u32);
            at += Duration::from_secs(seconds);
            assert_eq!(p.retry, Some(at));
            assert!(news
                .toast_batch(at - Duration::from_millis(1), true)
                .is_empty());
        }
        assert!(news.toast_batch(at, true).is_empty());
        let (tx, rx) = mpsc::channel();
        news.toast = Some(ToastFlight {
            ids: vec![1],
            result: rx,
            agent_tag: None,
        });
        news.retire_exhausted(86400);
        assert_eq!(news.pending.len(), 1);
        tx.send(false).unwrap();
        assert!(news.poll_toast(86400).is_empty());
        news.retire_exhausted(86400);
        assert!(news.pending.is_empty());
        assert_eq!(news.history.unread(), 1);
        assert!(matches!(news.exhausted.as_slice(), [Receipt::Battery(id)] if id == "1"));
    }

    #[test]
    fn accepted_briefing_toast_acknowledges_its_carry_rows() {
        let mut news = News::default();
        let mut card = news_card(Kind::Morning);
        card.lines.push(Line::new(
            Mark::Note,
            "Carry",
            Route::BriefingNote {
                id: "a".into(),
                day: 1,
            },
        ));
        news.pending
            .push_back(pending(1, card, Receipt::Morning(20261125), false));
        let (tx, result) = mpsc::channel();
        news.toast = Some(ToastFlight {
            ids: vec![1],
            result,
            agent_tag: None,
        });
        tx.send(true).unwrap();
        let done = news.poll_toast(86400);
        assert!(
            matches!(done.as_slice(), [(Receipt::Morning(20261125), carries)] if carries == &[("a".into(), 1)])
        );
    }

    #[test]
    fn interrupted_briefing_allows_a_restart_and_replays_once_after_chat() {
        use super::super::{
            rhythm::{Calendar, Rhythm},
            Config, State,
        };
        let date = 20261125;
        let mut cfg = Config {
            last_greet: Some(date),
            ..Default::default()
        };
        let mut news = News::default();
        let old = card::Interrupted {
            card: news_card(Kind::Morning),
            read_ids: vec![3],
        };
        cfg.last_greet = news.remember_interruption(old, date).unwrap();
        let loaded: Config = serde_json::from_str(&serde_json::to_string(&cfg).unwrap()).unwrap();
        assert!(Rhythm::default().morning_due(
            Calendar { date, minute: 500 },
            loaded.last_greet,
            Some(State::Idle)
        ));
        assert!(news.show_replay(|_| false).is_none());
        let replay = news.show_replay(|card| card.kind == Kind::Morning).unwrap();
        assert_eq!(replay.morning_date, Some(date));
        assert_eq!(replay.view.read_ids, [3]);
        assert!(news.show_replay(|_| panic!("already replayed")).is_none());
        assert!(news.history.entries.is_empty());
    }

    #[test]
    fn history_caps_at_twenty_and_resets_at_local_midnight() {
        let mut history = History::default();
        history.roll(1);
        for id in 0..23 {
            history.push(
                id,
                Line::new(Mark::Note, id.to_string(), Route::Notes(id.to_string())),
                86400,
                id % 2 == 0,
            );
        }
        assert_eq!(history.entries.len(), 20);
        assert_eq!(history.entries.front().unwrap().1.label, "22");
        assert_eq!(history.entries.back().unwrap().1.label, "3");
        assert_eq!(history.unread(), 10);
        history.roll(1);
        history.read_all();
        assert_eq!(history.unread(), 0);
        assert_eq!(history.entries.len(), 20);
        history.roll(2);
        assert!(history.entries.is_empty());
    }

    fn pending(id: u64, card: Card, receipt: Receipt, deferred: bool) -> Pending {
        Pending {
            id,
            card,
            receipt,
            deferred,
            during_absence: false,
            attempts: 0,
            retry: None,
        }
    }

    #[test]
    fn deferred_news_merges_into_briefing_and_marks_only_visible_rows_read() {
        let mut news = News::default();
        news.history.roll(1);
        for id in 0..7 {
            let notice = Notice::single(id.to_string(), format!("Note {id}"));
            news.pending.push_back(pending(
                id,
                super::super::remind::card(&notice),
                Receipt::Reminder(notice),
                true,
            ));
        }
        let brief = super::super::assistant::morning(
            super::super::assistant::Morning {
                calendar: Some(vec![(87000, 88000, "Agenda".into())]),
                ..Default::default()
            },
            86400,
            123,
        )
        .unwrap();
        news.pending
            .push_back(pending(7, brief, Receipt::Morning(2), false));
        let unrelated = Notice::single("new".into(), "New".into());
        news.pending.push_back(pending(
            8,
            super::super::remind::card(&unrelated),
            Receipt::Reminder(unrelated),
            false,
        ));
        let items = news.next_batch(true, Instant::now());
        let (card, shown) = batch(&items);
        assert_eq!(card.kind, Kind::Morning);
        assert_eq!(card.lines.last().unwrap().label, "+2");
        assert!(matches!(card.lines.last().unwrap().route, Route::News));
        assert_eq!(shown, [7, 0, 1, 2, 3, 4]);
        let ids: Vec<_> = items.iter().map(|p| p.id).collect();
        assert_eq!(news.deliver(&ids, 86400, &[]).len(), 8);
        assert_eq!(news.history.unread(), 8);
        news.history.read(&shown);
        assert_eq!(news.history.unread(), 2);
        assert!(matches!(
            news.history.entries.front().unwrap().1.route,
            Route::Agenda(123)
        ));
        assert!(
            matches!(news.history.entries.back().unwrap().1.route, Route::Notes(ref id) if id == "0")
        );
        assert!(news.next_batch(false, Instant::now()).is_empty());
        assert_eq!(news.next_batch(true, Instant::now())[0].id, 8);
        assert!(news.deliver(&ids, 86400, &[]).is_empty());
    }

    #[test]
    fn toast_history_is_unread_and_expired_downloads_never_reach_the_batch() {
        let mut news = News::default();
        news.history.roll(1);
        let notice = Notice::single("note".into(), "Reminder".into());
        news.pending.push_back(pending(
            1,
            super::super::remind::card(&notice),
            Receipt::Reminder(notice),
            true,
        ));
        news.deliver(&[1], 86400, &[]);
        assert_eq!(news.history.unread(), 1);
        let at = Instant::now();
        let download = Receipt::Download(super::super::downloads::NoticeTime::present(at));
        assert!(download.valid(1, 1, at + Duration::from_secs(599)));
        assert!(!download.valid(1, 1, at + Duration::from_secs(600)));
    }

    #[test]
    fn startup_missed_reminders_use_one_bounded_toast_with_all_member_ids() {
        let mut news = News::default();
        for id in 0..130 {
            let notice = Notice::at_start(id.to_string(), format!("Note {id}"));
            news.pending.push_back(pending(
                id,
                super::super::remind::card(&notice),
                Receipt::Reminder(notice),
                true,
            ));
        }
        let batch = news.toast_batch(Instant::now(), true);
        let Some(ToastPayload::Reminder(summary)) = toast_payload(&batch) else {
            panic!("startup summary");
        };
        assert!(summary.grouped && summary.missed);
        assert_eq!(
            summary.ids,
            (0..128).map(|id| id.to_string()).collect::<Vec<_>>()
        );
        let ids: Vec<_> = batch.iter().map(|p| p.id).collect();
        assert_eq!(news.deliver(&ids, 86400, &[]).len(), 128);
        assert_eq!(news.toast_batch(Instant::now(), true).len(), 2);
    }

    #[test]
    fn only_a_long_absence_groups_its_download_and_reminder() {
        for long in [false, true] {
            let now = Instant::now();
            let mut news = News {
                returned_at: long.then_some(now),
                ..Default::default()
            };
            let mut download = pending(
                0,
                news_card(Kind::Download),
                Receipt::Download(super::super::downloads::NoticeTime::present(now)),
                false,
            );
            download.during_absence = true;
            news.pending.push_back(download);
            let notice = Notice::single("away".into(), "Reminder".into());
            let mut reminder = pending(
                1,
                super::super::remind::card(&notice),
                Receipt::Reminder(notice),
                false,
            );
            reminder.during_absence = true;
            news.pending.push_back(reminder);
            news.finish_absence();
            assert_eq!(news.next_batch(true, Instant::now()).len(), if long { 2 } else { 1 });
            assert!(news.pending.iter().all(|p| !p.during_absence));
        }
    }
}

#[cfg(test)]
#[path = "news_snapshots.rs"]
mod snapshots;
#[cfg(test)]
pub(crate) use snapshots::paint_site_news;
