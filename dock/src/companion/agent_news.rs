use super::{
    card::{Card, Kind, Line, Route},
    news::{self, Receipt},
};
use crate::{
    agents::{Completion, Session, Wait},
    desklayer::card::Mark,
    i18n::t,
};
use std::{
    cell::RefCell,
    collections::HashSet,
    time::{Duration, Instant},
};

const WAIT_DELAY: Duration = Duration::from_millis(750);

#[derive(Default)]
struct Waits {
    handled: HashSet<Wait>,
}

impl Waits {
    fn poll(
        &mut self,
        sessions: &[Session],
        now: Instant,
        foreground: impl Fn(&Session) -> bool,
    ) -> (Vec<(Card, Receipt)>, Option<u32>) {
        let active: HashSet<_> = sessions.iter().map(Session::wait).collect();
        self.handled.retain(|wait| active.contains(wait));
        let mut cards = Vec::new();
        let mut next = None;
        for session in sessions {
            let wait = session.wait();
            if self.handled.contains(&wait) {
                continue;
            }
            if session.wait_was_foreground() || foreground(session) {
                self.handled.insert(wait);
                continue;
            }
            if let Some(left) = (wait.since + WAIT_DELAY)
                .checked_duration_since(now)
                .filter(|d| !d.is_zero())
            {
                let ms = left.as_millis() as u32 + 1;
                next = Some(next.map_or(ms, |old: u32| old.min(ms)));
                continue;
            }
            cards.push((wait_card(session), Receipt::AgentWait(wait.clone())));
            self.handled.insert(wait);
        }
        (cards, next)
    }
}

thread_local! { static WAITS: RefCell<Waits> = RefCell::new(Waits::default()); }

pub(super) fn session_label(session: &Session) -> String {
    let tool = session.agent.title();
    if session.name == tool {
        tool.into()
    } else {
        format!("{tool} · {}", session.name)
    }
}

pub(super) fn wait_card(session: &Session) -> Card {
    agent_card(
        session,
        t("assistant.agent_wait").into(),
        Route::AgentWait(session.wait()),
    )
}

pub(super) fn result_card(completion: &Completion) -> Card {
    let session = &completion.session;
    let value = if session.failed {
        t("assistant.agent_failed").into()
    } else {
        format!(
            "{} · {}{}",
            t("assistant.agent_done"),
            completion.elapsed.as_secs() / 60,
            t("timer.unit.min"),
        )
    };
    agent_card(session, value, Route::AgentResult(session.target()))
}

fn agent_card(session: &Session, value: String, route: Route) -> Card {
    let mut line = Line::new(Mark::Agent(session.agent), session_label(session), route);
    line.value = value;
    Card {
        kind: Kind::Agent,
        title: String::new(),
        emphasis: None,
        lines: vec![line],
        buttons: vec![],
    }
}

pub(super) fn poll(now: Instant) -> Option<u32> {
    let sessions = crate::agents::waiting_sessions();
    let active: HashSet<_> = sessions.iter().map(Session::wait).collect();
    news::resolve_waits(|wait| active.contains(wait));
    let foreground: HashSet<_> = sessions
        .iter()
        .filter(|s| s.foreground())
        .map(Session::wait)
        .collect();
    for wait in &foreground {
        news::suppress_pending_wait(wait);
    }
    let (cards, next) = WAITS.with(|waits| {
        waits
            .borrow_mut()
            .poll(&sessions, now, |s| foreground.contains(&s.wait()))
    });
    for (card, receipt) in cards {
        news::add(card, receipt);
    }
    for completion in crate::agents::take_completions() {
        if !completion.session.foreground() {
            news::add(
                result_card(&completion),
                Receipt::AgentResult(completion.session.since),
            );
        }
    }
    next
}

pub(super) fn shutdown() {
    WAITS.with(|waits| *waits.borrow_mut() = Waits::default());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::{test_session, Agent, St};

    #[test]
    fn wait_is_debounced_and_rearms_on_a_new_wait() {
        let now = Instant::now();
        let session = test_session(Agent::Claude, "project", St::Wait, now, false);
        let mut waits = Waits::default();
        let (cards, next) = waits.poll(std::slice::from_ref(&session), now, |_| false);
        assert!(cards.is_empty() && next.is_some());
        assert!(waits
            .poll(&[], now + Duration::from_millis(250), |_| false)
            .0
            .is_empty());
        let again = test_session(
            Agent::Claude,
            "project",
            St::Wait,
            now + Duration::from_millis(300),
            false,
        );
        assert!(waits
            .poll(std::slice::from_ref(&again), now + WAIT_DELAY, |_| false)
            .0
            .is_empty());
        let due = now + WAIT_DELAY + Duration::from_millis(300);
        let cards = waits.poll(std::slice::from_ref(&again), due, |_| false).0;
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0].0.lines[0].label, "Claude · project");
        assert!(waits.poll(&[again], due, |_| false).0.is_empty());
    }

    #[test]
    fn wait_seen_in_foreground_stays_suppressed_until_it_resolves() {
        let now = Instant::now();
        for (front_at_event, front_during_poll) in [(true, false), (false, true)] {
            let mut waits = Waits::default();
            let session = test_session(Agent::Claude, "project", St::Wait, now, front_at_event);
            assert!(waits
                .poll(std::slice::from_ref(&session), now, |_| front_during_poll)
                .0
                .is_empty());
            let (cards, next) = waits.poll(&[session], now + WAIT_DELAY, |_| false);
            assert!(cards.is_empty() && next.is_none());
        }
    }
}
