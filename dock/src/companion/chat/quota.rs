use super::{
    credentials::{Lock, Result},
    state::Usage,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};
#[derive(Default, Serialize, Deserialize)]
struct Ledger {
    #[serde(flatten)]
    usage: Usage,
    #[serde(default)]
    pending: HashMap<String, u64>,
}
fn load(path: &Path) -> Result<Ledger> {
    match std::fs::read(path) {
        Ok(b) => serde_json::from_slice(&b).map_err(|_| crate::i18n::t("chat.error.usage_read")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Ledger::default()),
        Err(_) => Err(crate::i18n::t("chat.error.usage_read")),
    }
}
fn save(path: &Path, ledger: &Ledger) -> Result<()> {
    let data = serde_json::to_vec(ledger).map_err(|_| crate::i18n::t("chat.error.usage_save"))?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|_| crate::i18n::t("chat.error.usage_save"))?;
    }
    let temp = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
    let r = std::fs::write(&temp, data).and_then(|_| std::fs::rename(&temp, path));
    if r.is_err() {
        let _ = std::fs::remove_file(temp);
    }
    r.map_err(|_| crate::i18n::t("chat.error.usage_save"))
}
struct Reservation {
    path: PathBuf,
    id: String,
    day: i64,
}
fn reserve_at(path: PathBuf, day: i64, now: u64, limit: u32) -> Result<Reservation> {
    let mut ledger = load(&path)?;
    if ledger.usage.day != day {
        ledger = Ledger {
            usage: Usage { day, count: 0 },
            ..Default::default()
        };
    }
    ledger.pending.retain(|_, at| now.saturating_sub(*at) < 300);
    if Usage::limit_reached(ledger.usage.count.saturating_add(ledger.pending.len() as u32), limit) {
        return Err(crate::i18n::t("chat.error.daily_limit"));
    }
    let id = uuid::Uuid::new_v4().to_string();
    ledger.pending.insert(id.clone(), now);
    save(&path, &ledger)?;
    Ok(Reservation { path, id, day })
}
fn finish(reservation: Reservation, sent: bool) -> Result<()> {
    let mut ledger = load(&reservation.path)?;
    if ledger.usage.day == reservation.day && ledger.pending.remove(&reservation.id).is_some() {
        if sent {
            ledger.usage.reserve(reservation.day, 0);
        }
        save(&reservation.path, &ledger)?;
    }
    Ok(())
}
pub async fn send(request: reqwest::RequestBuilder, limit: u32) -> Result<reqwest::Response> {
    let reservation = {
        let _lock = Lock::named("chat-quota")?;
        reserve_at(
            crate::config::app_dir().join("companion-usage.json"),
            super::transport::local_day(),
            super::auth::now(),
            limit,
        )?
    };
    // Do not cancel before the headers: they distinguish a dispatched request
    // from DNS/TLS/connect failure. Cancellation drops the response immediately
    // afterwards; streaming itself remains immediately cancellable.
    let reply = request.send().await;
    let sent = reply
        .as_ref()
        .is_ok_and(|r| r.status().as_u16() != 401 && r.status().as_u16() != 403);
    {
        let _lock = Lock::named("chat-quota")?;
        finish(reservation, sent)?;
    }
    reply.map_err(|_| crate::i18n::t("chat.error.network"))
}
pub fn check(limit: u32) -> Result<()> {
    let _lock = Lock::named("chat-quota")?;
    let ledger = load(&crate::config::app_dir().join("companion-usage.json"))?;
    let count = ledger.usage.today(super::transport::local_day());
    if Usage::limit_reached(count, limit) {
        Err(crate::i18n::t("chat.error.daily_limit"))
    } else {
        Ok(())
    }
}
/// Reserve and commit around process creation under the same cross-process lock
/// as ChatGPT. A failed spawn is refunded; a dispatched CLI turn counts once.
pub fn dispatch<T>(limit: u32, spawn: impl FnOnce() -> Result<T>) -> Result<T> {
    let _lock = Lock::named("chat-quota")?;
    let ticket = reserve_at(crate::config::app_dir().join("companion-usage.json"),
        super::transport::local_day(), super::auth::now(), limit)?;
    let result = spawn();
    finish(ticket, result.is_ok())?;
    result
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn failed_dispatch_refunds_reservation_and_only_dispatched_requests_count() {
        let root = std::env::temp_dir().join(format!("nopbar-quota-{}", uuid::Uuid::new_v4()));
        let path = root.join("usage.json");
        let ticket = reserve_at(path.clone(), 1, 100, 1).unwrap();
        assert_eq!(load(&path).unwrap().usage.count, 0);
        assert!(reserve_at(path.clone(), 1, 100, 1).is_err());
        finish(ticket, false).unwrap();
        assert_eq!(load(&path).unwrap().usage.count, 0);
        let ticket = reserve_at(path.clone(), 1, 101, 1).unwrap();
        finish(ticket, true).unwrap();
        assert_eq!(load(&path).unwrap().usage.count, 1);
        assert!(reserve_at(path.clone(), 1, 101, 1).is_err());
        let unlimited = reserve_at(path.clone(), 1, 102, 0).unwrap();
        finish(unlimited, true).unwrap();
        assert_eq!(load(&path).unwrap().usage.count, 2);
        let ticket = reserve_at(path.clone(), 2, 102, 1).unwrap();
        finish(ticket, false).unwrap();
        assert_eq!(load(&path).unwrap().usage.count, 0);
        std::fs::remove_file(path).unwrap();
        std::fs::remove_dir(root).unwrap();
    }
}
