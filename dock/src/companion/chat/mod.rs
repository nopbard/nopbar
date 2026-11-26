pub mod auth;
pub mod cli;
pub mod install;
mod process;
mod ndjson;
#[cfg(test)] mod fixture;
pub mod connect;
pub mod credentials;
pub mod hint;
mod json_edit;
pub(crate) mod bubbles;
pub(crate) mod paint;
pub(crate) mod input;
pub(crate) mod say_queue;
mod history;
mod history_view;
#[cfg(test)] pub(super) mod snapshots;
pub mod mcp;
mod native;
pub mod persona;
pub mod protected;
mod proxy;
mod quota;
pub mod session;
pub mod settings;
pub(super) mod shim;
pub mod sse;
pub mod state;
pub mod transport;
pub use native::{busy, above_sprite, anchor, frame, is_open, open, open_history, say, shutdown};
pub(crate) use native::close_panels;
pub const HOTKEY_ID: i32 = 0x434a;
pub const HISTORY_HOTKEY_ID: i32 = 0x434b;
static HISTORY_REQUEST: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
pub fn request_history() {
    HISTORY_REQUEST.store(true, std::sync::atomic::Ordering::SeqCst);
    mcp::wake();
}
pub(super) fn poll_history() {
    if HISTORY_REQUEST.swap(false, std::sync::atomic::Ordering::SeqCst) { open_history(); }
}
pub fn apply_history_hotkey(owner: windows::Win32::Foundation::HWND, spec: &str) {
    super::hotkey::apply_history_hotkey(owner, spec);
}
static PREFERENCES: std::sync::RwLock<Option<state::Config>> = std::sync::RwLock::new(None);
pub fn configure(c: &state::Config) {
    let mut current=PREFERENCES.write().unwrap_or_else(|e|e.into_inner());
    let probe = c.provider != state::Provider::ChatGpt && cli::cached(c.provider).status==cli::Status::Checking;
    let close=current.as_ref().is_some_and(|old|old.provider!=c.provider || old.connected(auth::connected())!=c.connected(auth::connected()));
    *current=Some(c.clone());
    drop(current);
    native::configure(c.keep_history);
    if close{shutdown();}
    #[cfg(not(test))]
    if probe {cli::refresh(windows::Win32::Foundation::HWND::default());}
    #[cfg(test)] let _ = probe;
}
pub fn connected() -> bool {
    PREFERENCES.read().unwrap_or_else(|e|e.into_inner()).as_ref().is_some_and(|c|c.connected(auth::connected()) && (c.provider == state::Provider::ChatGpt || cli::usable(c.provider)))
}
pub fn awake() -> bool {
    PREFERENCES.read().unwrap_or_else(|e|e.into_inner()).as_ref().is_some_and(|c|
        c.connected(auth::connected()) && (c.provider == state::Provider::ChatGpt || cli::awake(c.provider)))
}
pub fn recheck_on_click() {
    let provider=PREFERENCES.read().unwrap_or_else(|e|e.into_inner()).as_ref().map(|c|c.provider);
    if let Some(p)=provider.filter(|p|*p!=state::Provider::ChatGpt) {cli::recheck_on_click(p);}
}
pub fn active_key() -> Option<String> {
    let prefs=PREFERENCES.read().unwrap_or_else(|e|e.into_inner());
    let c=prefs.as_ref()?;
    match c.provider {
        state::Provider::ChatGpt => auth::active_key(),
        p => (c.connected(false) && cli::usable(p)).then(|| p.name().to_string()),
    }
}
pub fn apply_hotkey(owner: windows::Win32::Foundation::HWND, spec: &str) {
    super::hotkey::apply_chat_hotkey(owner, spec);
}
