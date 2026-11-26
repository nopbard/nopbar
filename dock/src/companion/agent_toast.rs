//! Windows delivery for agent news when the companion has no card surface.
use super::card::{Card, Route};
use crate::agents::Wait;
use std::{cell::RefCell, collections::VecDeque, sync::OnceLock};
use uuid::Uuid;
use windows::{
    core::w,
    Win32::{
        Foundation::{LPARAM, WPARAM},
        System::{
            DataExchange::COPYDATASTRUCT,
            WinRT::{RoInitialize, RoUninitialize, RO_INIT_MULTITHREADED},
        },
        UI::WindowsAndMessaging::*,
    },
};

pub(crate) const SCHEME: &str = "nopbar-agent";
const GROUP: &str = "agent-news";
const COPY_ID: usize = 0x41474e54;
const MAX_DELIVERED: usize = 128;

#[derive(Clone)]
enum Action {
    Wait(Wait),
}

#[derive(Clone)]
struct Row {
    token: Uuid,
    label: String,
    action: Action,
}

pub(super) struct Toast {
    tag: String,
    title: String,
    body: String,
    rows: Vec<Row>,
}

struct Active {
    tag: String,
    rows: Vec<Row>,
    shown: bool,
}

thread_local! {
    static ACTIVE: RefCell<VecDeque<Active>> = const { RefCell::new(VecDeque::new()) };
    static CLICKS: RefCell<Vec<Uuid>> = const { RefCell::new(Vec::new()) };
}

impl Toast {
    pub(super) fn new(card: &Card, title: String, body: String) -> Option<Self> {
        let rows: Vec<_> = card
            .lines
            .iter()
            .filter_map(|line| {
                let action = match &line.route {
                    Route::AgentWait(wait) => Action::Wait(wait.clone()),
                    _ => return None,
                };
                Some(Row {
                    token: Uuid::new_v4(),
                    label: line.label.clone(),
                    action,
                })
            })
            .collect();
        let tag = rows.first()?.token.simple().to_string()[..16].to_owned();
        Some(Self {
            tag,
            title,
            body,
            rows,
        })
    }

    pub(super) fn track(&self) -> String {
        ACTIVE.with(|active| {
            let mut active = active.borrow_mut();
            if active.len() == MAX_DELIVERED {
                active.pop_front();
            }
            active.push_back(Active {
                tag: self.tag.clone(),
                rows: self.rows.clone(),
                shown: false,
            });
        });
        self.tag.clone()
    }

    fn xml(&self) -> String {
        use crate::updates::xml_escape;
        let body = crate::updates::clip(&self.body, 400);
        let mut xml = format!(
            "<toast activationType=\"protocol\" launch=\"{SCHEME}:{}\"><visual><binding template=\"ToastGeneric\"><text>{}</text><text>{}</text></binding></visual>",
            self.rows[0].token, xml_escape(&self.title), xml_escape(&body)
        );
        if self.rows.len() > 1 {
            xml.push_str("<actions>");
            for row in &self.rows {
                xml.push_str(&format!(
                    "<action content=\"{}\" activationType=\"protocol\" arguments=\"{SCHEME}:{}\"/>",
                    xml_escape(&crate::updates::clip(&row.label, 48)), row.token
                ));
            }
            xml.push_str("</actions>");
        }
        xml.push_str("</toast>");
        xml
    }

    pub(super) fn send(self) -> bool {
        if cfg!(test) {
            return false;
        }
        static REGISTRATION: OnceLock<Result<(), String>> = OnceLock::new();
        let initialized = unsafe { RoInitialize(RO_INIT_MULTITHREADED) }.is_ok();
        let registered = REGISTRATION.get_or_init(|| {
            let exe = std::env::current_exe().map_err(|e| e.to_string())?;
            crate::updates::register_protocol(
                SCHEME,
                &format!("\"{}\" --agent-news \"%1\"", exe.display()),
            )?;
            crate::updates::register_aumid()
        });
        let shown = registered.as_ref().is_ok_and(|_| {
            crate::updates::show_grouped_xml(&self.xml(), Some((&self.tag, GROUP))).is_ok()
        });
        if initialized {
            unsafe {
                RoUninitialize();
            }
        }
        shown
    }
}

pub(super) fn finished(tag: String, success: bool) {
    let show = ACTIVE.with(|active| {
        let mut active = active.borrow_mut();
        let Some(i) = active.iter().position(|entry| entry.tag == tag) else {
            return false;
        };
        if success {
            active[i].shown = true;
        } else {
            active.remove(i);
        }
        true
    });
    if success && !show {
        remove(vec![tag]);
    }
}

pub(super) fn resolve_waits(alive: impl Fn(&Wait) -> bool) {
    let removals = ACTIVE.with(|active| {
        let mut removed = Vec::new();
        active.borrow_mut().retain(|entry| {
            let keep = entry
                .rows
                .iter()
                .all(|row| !matches!(&row.action, Action::Wait(wait) if !alive(wait)));
            if !keep && entry.shown {
                removed.push(entry.tag.clone());
            }
            keep
        });
        removed
    });
    remove(removals);
}

fn remove(tags: Vec<String>) {
    if tags.is_empty() || cfg!(test) {
        return;
    }
    let _ = std::thread::Builder::new()
        .name("agent-toast-remove".into())
        .spawn(move || {
            use windows::{core::HSTRING, UI::Notifications::ToastNotificationManager};
            let initialized = unsafe { RoInitialize(RO_INIT_MULTITHREADED) }.is_ok();
            if let Ok(history) = ToastNotificationManager::History() {
                let group = HSTRING::from(GROUP);
                let app = HSTRING::from(crate::updates::AUMID);
                for tag in tags {
                    let _ = history.RemoveGroupedTagWithId(&HSTRING::from(tag), &group, &app);
                }
            }
            if initialized {
                unsafe {
                    RoUninitialize();
                }
            }
        });
}

fn parse(url: &str) -> Option<Uuid> {
    let token = url.strip_prefix(SCHEME)?.strip_prefix(':')?;
    if token.len() != 36 {
        return None;
    }
    Uuid::parse_str(token).ok()
}

pub(crate) fn cli(url: &str) {
    if parse(url).is_none() {
        return;
    }
    unsafe {
        let Ok(hwnd) = FindWindowW(w!("NopBarWindow"), None) else {
            return;
        };
        let mut pid = 0;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid != 0 {
            let _ = AllowSetForegroundWindow(pid);
        }
        let data = COPYDATASTRUCT {
            dwData: COPY_ID,
            cbData: url.len() as u32,
            lpData: url.as_ptr() as *mut _,
        };
        let _ = SendMessageTimeoutW(
            hwnd,
            WM_COPYDATA,
            WPARAM(0),
            LPARAM(&data as *const _ as isize),
            SMTO_ABORTIFHUNG | SMTO_BLOCK,
            2000,
            None,
        );
    }
}

pub(crate) unsafe fn receive(lp: LPARAM) -> bool {
    if lp.0 == 0 {
        return false;
    }
    let data = &*(lp.0 as *const COPYDATASTRUCT);
    if data.dwData != COPY_ID || data.cbData != (SCHEME.len() + 37) as u32 || data.lpData.is_null()
    {
        return false;
    }
    let bytes = std::slice::from_raw_parts(data.lpData as *const u8, data.cbData as usize);
    let Some(token) = std::str::from_utf8(bytes).ok().and_then(parse) else {
        return false;
    };
    CLICKS.with(|clicks| clicks.borrow_mut().push(token));
    super::refresh();
    true
}

pub(super) fn dispatch() {
    for token in CLICKS.with(|clicks| std::mem::take(&mut *clicks.borrow_mut())) {
        let action = ACTIVE.with(|active| {
            active
                .borrow()
                .iter()
                .flat_map(|entry| &entry.rows)
                .find(|row| row.token == token)
                .map(|row| row.action.clone())
        });
        match action {
            Some(Action::Wait(wait)) => {
                if crate::agents::waiting_sessions()
                    .iter()
                    .any(|s| s.wait() == wait)
                {
                    crate::agents::focus(&wait.target);
                }
            }
            None => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::{test_session, Agent, St};
    use std::time::Instant;

    #[test]
    fn toast_uses_an_ephemeral_token_and_retires_a_resolved_wait() {
        let now = Instant::now();
        let session = test_session(Agent::Claude, "fixture<&", St::Wait, now, false);
        let card = super::super::agent_news::wait_card(&session);
        let toast = Toast::new(&card, "News".into(), "fixture<&".into()).unwrap();
        let token = toast.rows[0].token;
        let url = format!("{SCHEME}:{token}");
        let xml = toast.xml();
        assert!(xml.contains(&url) && xml.contains("fixture&lt;&amp;"));
        assert!(!xml.contains("claude:fixture"));
        assert_eq!(parse(&url), Some(token));
        assert_eq!(parse("nopbar-agent:../../fixture"), None);
        let tag = toast.track();
        resolve_waits(|_| false);
        finished(tag, true);
        ACTIVE.with(|active| assert!(active.borrow().is_empty()));
    }
}
