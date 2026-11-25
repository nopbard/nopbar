//! Shared output-only stdio protocol. No UI, credentials or HTTP dependencies.
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::io::{BufRead, Write};
pub const PROTOCOL: &str = "2025-11-25";
pub const SUPPORTED: &[&str] = &["2025-11-25", "2025-06-18", "2025-03-26"];
pub fn negotiate(requested: &str) -> &str {
    if SUPPORTED.contains(&requested) {
        requested
    } else {
        PROTOCOL
    }
}
pub fn pipe_name() -> String {
    let mut session = 0;
    unsafe {
        let _ = windows::Win32::System::RemoteDesktop::ProcessIdToSessionId(
            windows::Win32::System::Threading::GetCurrentProcessId(),
            &mut session,
        );
    }
    format!(r"\\.\pipe\NopBar.{session}")
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Output {
    Say {
        text: String,
        mood: Option<String>,
    },
    Status {
        state: String,
        label: Option<String>,
    },
}
impl Output {
    pub fn parse(name: &str, args: &Value) -> Result<Self, &'static str> {
        let m = args.as_object().ok_or("arguments must be an object")?;
        let optional = |key: &str| -> Result<Option<String>, &'static str> {
            m.get(key)
                .map(|v| v.as_str().map(String::from).ok_or("expected a string"))
                .transpose()
        };
        match name {
            "say" => {
                if m.keys().any(|k| k != "text" && k != "mood") {
                    return Err("unknown argument");
                }
                let text = m
                    .get("text")
                    .and_then(Value::as_str)
                    .ok_or("text is required")?
                    .chars()
                    .take(140)
                    .collect::<String>();
                if text.is_empty() {
                    return Err("text must not be empty");
                }
                let mood = optional("mood")?;
                if mood.as_ref().is_some_and(|s| {
                    !["happy", "thinking", "worried", "done"].contains(&s.as_str())
                }) {
                    return Err("invalid mood");
                }
                Ok(Self::Say { text, mood })
            }
            "status" => {
                if m.keys().any(|k| k != "state" && k != "label") {
                    return Err("unknown argument");
                }
                let state = m
                    .get("state")
                    .and_then(Value::as_str)
                    .ok_or("state is required")?;
                if !["work", "wait", "done"].contains(&state) {
                    return Err("invalid state");
                }
                Ok(Self::Status {
                    state: state.into(),
                    label: optional("label")?.map(|s| s.chars().take(140).collect()),
                })
            }
            _ => Err("unknown tool"),
        }
    }
    pub fn clip(&self) -> &str {
        match self {
            Self::Say { mood, .. } => match mood.as_deref() {
                Some("thinking") => "work",
                Some("worried") => "error",
                Some("done") => "done",
                _ => "wait",
            },
            Self::Status { state, .. } => state,
        }
    }
}
pub trait Sink {
    fn send(&mut self, output: &Output) -> Result<(), &'static str>;
}
pub struct Pipe;
impl Sink for Pipe {
    fn send(&mut self, o: &Output) -> Result<(), &'static str> {
        use std::os::windows::fs::OpenOptionsExt;
        let name = format!("{}.agents", pipe_name());
        // Never spawn NopBar and never wait indefinitely for an unavailable pipe.
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .custom_flags(0x00100000)
            .open(name)
            .map_err(|_| "NopBar가 꺼져 있음")?;
        let line = serde_json::to_vec(&json!({"type":"CompanionOutput","output":o}))
            .map_err(|_| "메시지를 만들지 못함")?;
        f.write_all(&line)
            .and_then(|_| f.write_all(b"\n"))
            .map_err(|_| "NopBar가 꺼져 있음")
    }
}
#[derive(Default)]
pub struct Server {
    initialized: bool,
    ready: bool,
    chat_session: bool,
}
fn error(id: Value, code: i32, msg: &str) -> Value {
    json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":msg}})
}
pub fn schema() -> Value {
    json!({"tools":[
        {"name":"say","description":"Show a six-second gnome speech bubble. Text is limited to 140 characters; suppressed messages coalesce. Does not invoke AI.","inputSchema":{"type":"object","properties":{"text":{"type":"string","minLength":1},"mood":{"type":"string","enum":["happy","thinking","worried","done"]}},"required":["text"],"additionalProperties":false}},
        {"name":"status","description":"Show the gnome working, waiting or done. Optional label is limited to 140 characters.","inputSchema":{"type":"object","properties":{"state":{"type":"string","enum":["work","wait","done"]},"label":{"type":"string"}},"required":["state"],"additionalProperties":false}}
    ]})
}
impl Server {
    pub fn for_chat_session(chat_session: bool) -> Self {
        Self {
            chat_session,
            ..Self::default()
        }
    }
    pub fn handle(&mut self, v: Value, sink: &mut impl Sink) -> Option<Value> {
        let id = v.get("id").cloned();
        let method = v["method"].as_str();
        if v["jsonrpc"] != "2.0"
            || method.is_none()
            || id
                .as_ref()
                .is_some_and(|id| !id.is_string() && !id.is_i64() && !id.is_u64())
        {
            return Some(error(id.unwrap_or(Value::Null), -32600, "Invalid request"));
        }
        let method = method.unwrap_or("");
        if id.is_none() {
            if method == "notifications/initialized" && self.initialized {
                self.ready = true;
            }
            return None;
        }
        let id = id.unwrap_or(Value::Null);
        let result = match method {
            "initialize" if !self.initialized => {
                if !v["params"]["protocolVersion"].is_string()
                    || !v["params"]["clientInfo"].is_object()
                    || !v["params"]["capabilities"].is_object()
                {
                    return Some(error(id, -32602, "Invalid initialize parameters"));
                }
                self.initialized = true;
                json!({"protocolVersion":negotiate(v["params"]["protocolVersion"].as_str().unwrap_or("")),"capabilities":if self.chat_session {json!({})} else {json!({"tools":{}})},"serverInfo":{"name":"NopBar","version":env!("CARGO_PKG_VERSION")}})
            }
            "ping" => json!({}),
            _ if !self.ready => return Some(error(id, -32600, "Initialize first")),
            "tools/list" => {
                if self.chat_session {
                    json!({"tools":[]})
                } else {
                    schema()
                }
            }
            "tools/call" => {
                if self.chat_session {
                    return Some(error(id, -32601, "Tools disabled for companion chat"));
                }
                let name = v["params"]["name"].as_str().unwrap_or("");
                let o = match Output::parse(name, &v["params"]["arguments"]) {
                    Ok(o) => o,
                    Err(e) => return Some(error(id, -32602, e)),
                };
                let r = sink.send(&o);
                json!({"content":[{"type":"text","text":r.err().unwrap_or("NopBar 말풍선 대기열에 전달했어.")}],"isError":r.is_err()})
            }
            _ => return Some(error(id, -32601, "Method not found")),
        };
        Some(json!({"jsonrpc":"2.0","id":id,"result":result}))
    }
}
pub fn run() -> i32 {
    let mut server = Server::for_chat_session(std::env::var("NOPBAR_CHAT").as_deref() == Ok("1"));
    let mut sink = Pipe;
    let stdin = std::io::stdin();
    let mut input = stdin.lock();
    let mut out = std::io::stdout().lock();
    loop {
        let mut line = Vec::new();
        let read = std::io::Read::take(&mut input, 65537).read_until(b'\n', &mut line);
        match read {
            Ok(0) => return 0,
            Err(_) => return 1,
            _ => {}
        }
        if line.len() > 65536 {
            let _ = writeln!(out, "{}", error(Value::Null, -32600, "Message too large"));
            return 1;
        }
        let response = match serde_json::from_slice(&line) {
            Ok(v) => server.handle(v, &mut sink),
            Err(_) => Some(error(Value::Null, -32700, "Parse error")),
        };
        if let Some(r) = response {
            if writeln!(out, "{r}").and_then(|_| out.flush()).is_err() {
                return 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn companion_chat_has_no_tools_and_never_calls_sink() {
        struct Forbidden;
        impl Sink for Forbidden {
            fn send(&mut self, _: &Output) -> Result<(), &'static str> {
                panic!("chat must not reach the dock")
            }
        }
        let mut s = Server::for_chat_session(true);
        let r=s.handle(json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":PROTOCOL,"capabilities":{},"clientInfo":{}}}), &mut Forbidden).unwrap();
        assert!(r["result"]["capabilities"].get("tools").is_none());
        s.handle(
            json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
            &mut Forbidden,
        );
        let r = s
            .handle(
                json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
                &mut Forbidden,
            )
            .unwrap();
        assert_eq!(r["result"]["tools"], json!([]));
        for name in ["say", "status"] {
            let r=s.handle(json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":name,"arguments":{"text":"hello","state":"work"}}}), &mut Forbidden).unwrap();
            assert_eq!(r["error"]["code"], -32601);
        }
    }
    struct Offline;
    impl Sink for Offline {
        fn send(&mut self, _: &Output) -> Result<(), &'static str> {
            Err("offline")
        }
    }
    #[test]
    fn negotiates_every_supported_client_version_and_falls_back_for_unknown() {
        for requested in SUPPORTED.iter().copied().chain(["future-protocol"]) {
            let response=Server::default().handle(json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":requested,"capabilities":{},"clientInfo":{"name":"fixture","version":"1"}}}),&mut Offline).unwrap();
            assert_eq!(
                response["result"]["protocolVersion"],
                if SUPPORTED.contains(&requested) {
                    requested
                } else {
                    PROTOCOL
                }
            );
        }
    }
}
