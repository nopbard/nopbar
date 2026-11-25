//! Durable reminder toasts with protocol activation, including after restart.
use super::{reminders::Notice, runtime::{self, Action}};
use windows::{core::w, Win32::{Foundation::{LPARAM, WPARAM}, System::DataExchange::COPYDATASTRUCT, UI::WindowsAndMessaging::*}};

const SCHEME: &str = "nopbar-note";
const COPY_ID: usize = 0x4e5231;

#[derive(Debug, PartialEq)]
pub struct Activation { verb: String, ids: Vec<String> }

pub fn parse(url: &str) -> Option<Activation> {
    if url.len() > 16384 { return None; }
    let (verb, ids) = url.strip_prefix("nopbar-note:")?.split_once('/')?;
    if !matches!(verb, "open" | "snooze" | "complete") { return None; }
    let ids: Vec<_> = ids.split(',').map(str::to_string).collect();
    if ids.is_empty() || ids.len() > 128 || ids.iter().any(|s| !super::store::valid_id(s)) { return None; }
    Some(Activation { verb: verb.into(), ids })
}

fn dispatch(a: Activation) {
    if a.verb == "open" {
        runtime::action(Action::Reveal(if a.ids.len() == 1 { a.ids[0].clone() } else { String::new() }));
    } else {
        for id in a.ids { runtime::action(if a.verb == "snooze" { Action::Snooze(id) } else { Action::Complete(id) }); }
    }
}

/// true: handled (including an invalid URL); false: start the normal app,
/// with the activation queued until the notes runtime's first timer tick.
pub fn cli(url: &str) -> bool {
    let Some(a) = parse(url) else { return true; };
    unsafe {
        if let Ok(hwnd) = FindWindowW(w!("NopBarWindow"), None) {
            let bytes = url.as_bytes();
            let data = COPYDATASTRUCT { dwData: COPY_ID, cbData: bytes.len() as u32, lpData: bytes.as_ptr() as *mut _ };
            let mut result = 0;
            let _ = SendMessageTimeoutW(hwnd, WM_COPYDATA, WPARAM(0), LPARAM(&data as *const _ as isize), SMTO_ABORTIFHUNG | SMTO_BLOCK, 2000, Some(&mut result));
            return true; // never start a second dock when one is busy
        }
    }
    dispatch(a);
    false
}

pub unsafe fn receive(lp: LPARAM) -> bool {
    if lp.0 == 0 { return false; }
    let d = &*(lp.0 as *const COPYDATASTRUCT);
    if d.dwData != COPY_ID || d.cbData == 0 || d.cbData > 16384 || d.lpData.is_null() { return false; }
    let Ok(url) = std::str::from_utf8(std::slice::from_raw_parts(d.lpData as *const u8, d.cbData as usize)) else { return false; };
    let Some(a) = parse(url) else { return false; };
    dispatch(a);
    true
}

pub fn xml(notice: &Notice) -> String {
    let escape = crate::updates::xml_escape;
    let ids = notice.ids.join(",");
    format!("<toast activationType=\"protocol\" launch=\"{SCHEME}:open/{ids}\"><visual><binding template=\"ToastGeneric\"><text>{}</text><text>{}</text></binding></visual><actions><action content=\"{}\" activationType=\"protocol\" arguments=\"{SCHEME}:snooze/{ids}\"/><action content=\"{}\" activationType=\"protocol\" arguments=\"{SCHEME}:complete/{ids}\"/></actions></toast>",
        escape(crate::i18n::t("widgets.notes_reminders")), escape(&notice.text),
        escape(crate::i18n::t("remind.snooze")), escape(crate::i18n::t("remind.complete")))
}

static REGISTRATION: std::sync::OnceLock<Result<(), String>> = std::sync::OnceLock::new();
static ERRORS: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
fn log_failure(error: &str) {
    let mut errors = ERRORS.lock().unwrap_or_else(|e| e.into_inner());
    if !errors.iter().any(|e| e == error) {
        errors.push(error.into());
        crate::log!("reminder toast: {error}");
    }
}
pub fn init() {
    if cfg!(test) { return; }
    let result = REGISTRATION.get_or_init(|| {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        crate::updates::register_protocol(SCHEME, &format!("\"{}\" --note-reminder \"%1\"", exe.display()))?;
        crate::updates::register_aumid()
    });
    if let Err(e) = result { log_failure(e); }
}
pub fn show(notice: &Notice) -> bool {
    if cfg!(test) { return false; }
    if parse(&format!("{SCHEME}:open/{}", notice.ids.join(","))).is_none() { return false; }
    let Some(Ok(())) = REGISTRATION.get() else { return false; };
    let result = crate::updates::show_xml(&xml(notice));
    if let Err(e) = &result { log_failure(e); }
    result.is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    const ID: &str = "1234567890abcdef1234567890abcdef";
    #[test]
    fn activation_accepts_only_note_ids_and_known_actions() {
        for verb in ["open", "snooze", "complete"] { assert_eq!(parse(&format!("nopbar-note:{verb}/{ID}" )).unwrap().ids, [ID]); }
        let id = super::super::store::new_id(1_795_000_000_000, 1);
        assert_eq!(parse(&format!("nopbar-note:open/{id}")).unwrap().ids, [id]);
        let batch = vec!["x".repeat(64);128].join(",");
        assert_eq!(parse(&format!("nopbar-note:open/{batch}")).unwrap().ids.len(),128);
        for url in ["https://example.com", "nopbar-note:delete/abc", "nopbar-note:open/", "nopbar-note:open/../../secret", "nopbar-note:complete/\" --quit"] { assert!(parse(url).is_none()); }
    }
    #[test]
    fn toast_escapes_text_and_has_durable_protocol_actions() {
        let x = xml(&Notice::single(ID.into(), "<memo> & \"quote\"".into()));
        assert!(x.contains("&lt;memo&gt; &amp; &quot;quote&quot;"));
        for verb in ["open", "snooze", "complete"] { assert!(x.contains(&format!("nopbar-note:{verb}/{ID}"))); }
        assert!(!x.contains("activationType=\"system\""));
    }
}
