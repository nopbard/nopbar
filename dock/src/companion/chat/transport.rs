use super::{
    auth,
    credentials::Result,
    persona,
    sse::{Event, Parser},
    state::Usage,
};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use std::sync::mpsc::Sender;
#[derive(Clone, Serialize, Deserialize)]
pub struct Model {
    pub slug: String,
    pub display_name: String,
}
pub async fn models() -> Result<Vec<Model>> {
    let token = auth::access().await?;
    let r = auth::client()?
        .get("https://api.openai.com/v1/models")
        .bearer_auth(token)
        .send()
        .await
        .map_err(|_| crate::i18n::t("chat.error.models_fetch"))?;
    if !r.status().is_success() {
        return Err(crate::i18n::t("chat.error.models_fetch"));
    }
    let v: serde_json::Value = r.json().await.map_err(|_| crate::i18n::t("chat.error.models_read"))?;
    let list = v["models"]
        .as_array()
        .ok_or(crate::i18n::t("chat.error.models_invalid"))?
        .iter()
        .filter(|m| m["visibility"] == "list")
        .filter_map(|m| {
            Some(Model {
                slug: m["slug"].as_str()?.into(),
                display_name: m["display_name"].as_str()?.into(),
            })
        })
        .collect::<Vec<_>>();
    if list.is_empty() {
        return Err(crate::i18n::t("chat.error.models_empty"));
    }
    Ok(list)
}
pub fn local_day() -> i64 {
    unsafe {
        let t = windows::Win32::System::SystemInformation::GetLocalTime();
        i64::from(t.wYear) * 10000 + i64::from(t.wMonth) * 100 + i64::from(t.wDay)
    }
}
pub fn usage() -> Usage {
    std::fs::read(crate::config::app_dir().join("companion-usage.json"))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}
pub fn request(model: &str, input: serde_json::Value) -> serde_json::Value {
    serde_json::json!({"model":model,"instructions":persona::instructions(),"input":input,"stream":true,"store":false})
}
pub enum Update {
    Delta(u64, String),
    Finished(u64, Option<&'static str>),
}
async fn consume<S, B, E>(mut stream: S, generation: u64, tx: &Sender<Update>) -> Result<()>
where
    S: futures_util::Stream<Item = std::result::Result<B, E>> + Unpin,
    B: AsRef<[u8]>,
{
    consume_idle(
        &mut stream,
        generation,
        tx,
        std::time::Duration::from_secs(60),
    )
    .await
}
async fn consume_idle<S, B, E>(
    mut stream: S,
    generation: u64,
    tx: &Sender<Update>,
    idle: std::time::Duration,
) -> Result<()>
where
    S: futures_util::Stream<Item = std::result::Result<B, E>> + Unpin,
    B: AsRef<[u8]>,
{
    let mut parser = Parser::default();
    while let Some(chunk) = tokio::time::timeout(idle, stream.next())
        .await
        .map_err(|_| crate::i18n::t("chat.error.response_timeout"))?
    {
        for event in parser.push(
            chunk
                .map_err(|_| crate::i18n::t("chat.error.connection_lost"))?
                .as_ref(),
        ) {
            match event {
                Event::Delta(text) => {
                    if tx.send(Update::Delta(generation, text)).is_err() {
                        return Err(crate::i18n::t("chat.error.chat_closed"));
                    }
                }
                Event::Error(error) => return Err(error),
                Event::Complete => return Ok(()),
            }
        }
    }
    parser.end()
}
async fn cancellable<F>(cancel: &mut tokio::sync::oneshot::Receiver<()>, work: F) -> Result<()>
where
    F: std::future::Future<Output = Result<()>>,
{
    tokio::select! {biased; _=cancel=>Ok(()), result=work=>result}
}
pub fn start(
    generation: u64,
    provider: super::state::Provider,
    model: String,
    input: serde_json::Value,
    limit: u32,
    tx: Sender<Update>,
) -> tokio::sync::oneshot::Sender<()> {
    if provider != super::state::Provider::ChatGpt {
        return super::cli::start(provider, generation, model, input, limit, tx);
    }
    let (cancel, mut cancelled) = tokio::sync::oneshot::channel();
    std::thread::spawn(move || {
        let result = (|| {
            let rt = auth::runtime()?;
            rt.block_on(async {
                if cancelled.try_recv().is_ok() {
                    return Ok(());
                }
                if model.is_empty() {
                    return Err(crate::i18n::t("chat.error.model_required"));
                }
                super::quota::check(limit)?;
                // Auth/rotation is protected; cancellation is checked after its commit.
                let token = auth::access().await?;
                if cancelled.try_recv().is_ok() {
                    return Ok(());
                }
                let request = auth::client()?
                    .post("https://api.openai.com/v1/responses")
                    .bearer_auth(token)
                    .json(&request(&model, input));
                let r = super::quota::send(request, limit).await?;
                if cancelled.try_recv().is_ok() {
                    return Ok(());
                }
                cancellable(&mut cancelled, async {
                    if !r.status().is_success() {
                        return Err(match r.status().as_u16() {
                            401 | 403 => crate::i18n::t("chat.error.expired"),
                            429 => crate::i18n::t("chat.error.rate_limit"),
                            _ => crate::i18n::t("chat.error.response_missing"),
                        });
                    }
                    consume(r.bytes_stream(), generation, &tx).await
                })
                .await
            })
        })();
        let _ = tx.send(Update::Finished(generation, result.err()));
    });
    cancel
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stream_timeout_measures_idle_not_total_duration() {
        auth::runtime().unwrap().block_on(async {
            let (tx, _rx) = std::sync::mpsc::channel();
            let idle = std::time::Duration::from_millis(50);
            let pending = futures_util::stream::pending::<std::result::Result<Vec<u8>, ()>>();
            assert!(consume_idle(pending, 1, &tx, idle).await.is_err());
            let stream = futures_util::stream::unfold(0, |n| async move {
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                let data = if n < 5 {
                    b": keepalive\n\n".as_slice()
                } else {
                    b"data: {\"type\":\"response.completed\"}\n\n".as_slice()
                };
                Some((Ok::<_, ()>(data.to_vec()), n + 1))
            })
            .boxed();
            assert!(consume_idle(stream, 1, &tx, idle).await.is_ok());
        });
    }
    #[test]
    fn fake_stream_completion_failure_and_cancellation_use_no_network() {
        auth::runtime().unwrap().block_on(async {
            let (tx, rx) = std::sync::mpsc::channel();
            let chunks = vec![
                Ok::<_, ()>(
                    b"data: {\"type\":\"response.output_text.delta\",\"delta\":\"hello\"}\n\n"
                        .to_vec(),
                ),
                Ok(b"data: {\"type\":\"response.completed\"}\n\n".to_vec()),
            ];
            assert!(consume(futures_util::stream::iter(chunks), 7, &tx)
                .await
                .is_ok());
            assert!(matches!(rx.try_recv(),Ok(Update::Delta(7,s)) if s=="hello"));
            assert!(consume(
                futures_util::stream::empty::<std::result::Result<Vec<u8>, ()>>(),
                8,
                &tx
            )
            .await
            .is_err());
            let (stop, mut stopped) = tokio::sync::oneshot::channel();
            let mut requests = 0;
            stop.send(()).unwrap();
            cancellable(&mut stopped, async {
                requests += 1;
                Ok(())
            })
            .await
            .unwrap();
            assert_eq!(requests, 0);
            let (stop, mut stopped) = tokio::sync::oneshot::channel();
            let work = consume(
                futures_util::stream::pending::<std::result::Result<Vec<u8>, ()>>(),
                9,
                &tx,
            );
            let cancel = async {
                tokio::task::yield_now().await;
                stop.send(()).unwrap();
            };
            let (result, ()) = tokio::join!(cancellable(&mut stopped, work), cancel);
            assert!(result.is_ok());
            assert!(rx.try_recv().is_err());
        });
    }
    #[test]
    fn private_data_has_no_config_or_log_sink() {
        let cfg = serde_json::to_string(&crate::config::Config::default()).unwrap();
        for forbidden in [
            "access_token",
            "refresh_token",
            "id_token",
            "turns",
            "pending",
        ] {
            assert!(!cfg.contains(forbidden));
        }
        for source in [
            include_str!("auth.rs"),
            include_str!("credentials.rs"),
            include_str!("native.rs"),
            include_str!("sse.rs"),
        ] {
            assert!(!source.contains("crate::log!"));
            assert!(!source.contains("println!"));
        }
        for source in [include_str!("native.rs"), include_str!("state.rs")] {
            assert!(!source.contains("std::fs::"));
        }
    }
    #[test]
    fn request_contains_only_user_history_and_instructions() {
        let r = request(
            "allowed-model",
            serde_json::json!([{"role":"user","content":"hi"}]),
        );
        assert_eq!(r.as_object().unwrap().len(), 5);
        assert_eq!(r["store"], false);
        assert_eq!(r["stream"], true);
        for key in [
            "tools",
            "metadata",
            "previous_response_id",
            "max_output_tokens",
            "temperature",
            "background",
        ] {
            assert!(r.get(key).is_none());
        }
        assert_eq!(r["instructions"], persona::instructions());
    }
}
