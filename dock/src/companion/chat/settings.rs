//! UI actions enqueue independent workers; no UI callback opens the vault or waits on an OS lock.
use super::{auth, session, transport};
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Mutex,
};
use windows::Win32::Foundation::HWND;
static LOGIN: AtomicBool = AtomicBool::new(false);
static GENERATION: AtomicU64 = AtomicU64::new(0);
static NOTE: Mutex<String> = Mutex::new(String::new());
static MODELS: Mutex<Vec<transport::Model>> = Mutex::new(Vec::new());
type McpResult = (u8, bool, Result<(), String>);
static MCP_RESULTS: Mutex<Vec<McpResult>> = Mutex::new(Vec::new());
pub const UPDATED: u32 = windows::Win32::UI::WindowsAndMessaging::WM_APP + 181;
pub fn note() -> String {
    let n = NOTE.lock().unwrap_or_else(|e| e.into_inner()).clone();
    if n.is_empty() {
        auth::label()
    } else {
        n
    }
}
fn dispatch(work: impl FnOnce() + Send + 'static) {
    std::thread::spawn(work);
}
pub(super) fn updated(hwnd: isize) {
    unsafe {
        let _ = windows::Win32::UI::WindowsAndMessaging::PostMessageW(
            Some(HWND(hwnd as *mut _)),
            UPDATED,
            Default::default(),
            Default::default(),
        );
    }
}
pub fn begin(h: HWND, action: &str) {
    begin_action(h, action.to_string(), None);
}
fn begin_action(h: HWND, action: String, account: Option<usize>) {
    session::subscribe(h);
    let login = matches!(action.as_str(), "chat.login" | "chat.add");
    if login && LOGIN.swap(true, Ordering::SeqCst) {
        return;
    }
    let generation = GENERATION.fetch_add(1, Ordering::SeqCst) + 1;
    MODELS.lock().unwrap_or_else(|e| e.into_inner()).clear();
    NOTE.lock().unwrap_or_else(|e| e.into_inner()).clear();
    let hwnd = h.0 as isize;
    dispatch(move || {
        let result = (|| {
            let rt = auth::runtime()?;
            rt.block_on(async {
                if let Some(index) = account {
                    auth::select(index)?;
                }
                match action.as_str() {
                    "chat.login" => auth::sign_in(false).await?,
                    "chat.add" => auth::sign_in(true).await?,
                    "chat.disconnect" => {
                        auth::disconnect().await?;
                        return Ok(Vec::new());
                    }
                    _ => {}
                }
                transport::models().await
            })
        })();
        if GENERATION.load(Ordering::SeqCst) == generation {
            match result {
                Ok(models) => *MODELS.lock().unwrap_or_else(|e| e.into_inner()) = models,
                Err(e) => *NOTE.lock().unwrap_or_else(|e| e.into_inner()) = e.into(),
            }
        }
        if login {
            LOGIN.store(false, Ordering::SeqCst);
        }
        session::refresh();
        updated(hwnd);
    });
}
pub fn set_mcp(h: HWND, provider: u8, on: bool) {
    let hwnd = h.0 as isize;
    dispatch(move || {
        let result = if provider == 2 {
            super::connect::set_antigravity(on)
        } else {
            crate::agents::set_notifications(if provider == 0 { crate::agents::Agent::Claude } else { crate::agents::Agent::Codex }, on)
        };
        MCP_RESULTS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push((provider, on, result));
        updated(hwnd);
    });
}
pub fn take_mcp_results() -> Vec<McpResult> {
    std::mem::take(&mut *MCP_RESULTS.lock().unwrap_or_else(|e| e.into_inner()))
}
pub fn default_model(current: &str) -> Option<String> {
    let models = MODELS.lock().ok()?;
    models
        .iter()
        .find(|m| m.slug == current)
        .or_else(|| models.first())
        .map(|m| m.slug.clone())
}
pub fn pick_model(h: HWND) -> Option<String> {
    let models = MODELS.lock().ok()?.clone();
    if models.is_empty() {
        begin(h, "chat.models");
        return None;
    }
    pick(
        h,
        &models
            .iter()
            .map(|m| m.display_name.clone())
            .collect::<Vec<_>>(),
    )
    .map(|i| models[i].slug.clone())
}
pub fn select_account(h: HWND, index: usize) {begin_action(h, "chat.models".into(), Some(index));}
pub fn pick(h: HWND, labels: &[String]) -> Option<usize> {
    unsafe {
        use windows::{
            core::HSTRING,
            Win32::{Foundation::POINT, UI::WindowsAndMessaging::*},
        };
        let menu = CreatePopupMenu().ok()?;
        for (i, label) in labels.iter().enumerate() {
            let _ = AppendMenuW(
                menu,
                MF_STRING,
                i + 1,
                &HSTRING::from(label.replace('&', "&&")),
            );
        }
        let mut point = POINT::default();
        let _ = GetCursorPos(&mut point);
        let id = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_NONOTIFY,
            point.x,
            point.y,
            None,
            h,
            None,
        )
        .0;
        let _ = DestroyMenu(menu);
        (id > 0)
            .then_some(id as usize - 1)
            .filter(|&i| i < labels.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn slow_disconnect_does_not_serialize_account_selection_or_mcp_workers() {
        let (stop, wait) = std::sync::mpsc::channel();
        dispatch(move || {
            wait.recv().unwrap();
        });
        let (tx, rx) = std::sync::mpsc::channel();
        let select = tx.clone();
        dispatch(move || {
            select.send("select").unwrap();
        });
        dispatch(move || {
            tx.send("mcp").unwrap();
        });
        let mut replies = vec![
            rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap(),
            rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap(),
        ];
        replies.sort();
        assert_eq!(replies, ["mcp", "select"]);
        stop.send(()).unwrap();
    }
}
