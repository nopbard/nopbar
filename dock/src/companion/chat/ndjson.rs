//! Separate wire formats, one bounded incremental UTF-8 line decoder.
use super::state::Provider;
use serde_json::Value;
#[derive(Debug, PartialEq)]
pub enum Event {
    Delta(String),
    Done,
    Error(&'static str),
}
pub struct Parser {
    provider: Provider,
    buffer: Vec<u8>,
    complete: bool,
    streamed: bool,
}
impl Parser {
    pub fn new(provider: Provider) -> Self {
        Self {
            provider,
            buffer: Vec::new(),
            complete: false,
            streamed: false,
        }
    }
    pub fn push(&mut self, bytes: &[u8]) -> Result<Vec<Event>, &'static str> {
        if self.buffer.len() + bytes.len() > 1024 * 1024 {
            return Err("chat.error.protocol");
        }
        self.buffer.extend_from_slice(bytes);
        let mut events = Vec::new();
        while let Some(n) = self.buffer.iter().position(|b| *b == b'\n') {
            let line = self.buffer.drain(..=n).collect::<Vec<_>>();
            if line.iter().all(u8::is_ascii_whitespace) {
                continue;
            }
            let v: Value = serde_json::from_slice(&line).map_err(|_| "chat.error.protocol")?;
            if self.complete {
                return Err("chat.error.protocol");
            }
            let kind = v["type"].as_str().or(v["event"].as_str()).unwrap_or("");
            let mut delta = None;
            if kind == "error" {
                events.push(Event::Error(error(&v.to_string())));
            }
            match self.provider {
                Provider::ClaudeCode => match kind {
                    "stream_event"
                        if v["event"]["type"] == "content_block_delta"
                            && v["event"]["delta"]["type"] == "text_delta" =>
                    {
                        delta = v["event"]["delta"]["text"].as_str().map(str::to_string)
                    }
                    "result" => {
                        if v["is_error"] == true
                            || v["subtype"].as_str().is_some_and(|s| s != "success")
                        {
                            events.push(Event::Error(error(&v.to_string())));
                        } else {
                            if !self.streamed {
                                delta = v["result"].as_str().map(str::to_string)
                            }
                            self.complete = true;
                        }
                    }
                    _ => {}
                },
                Provider::Antigravity => match kind {
                    "step_update" if v["step_update"]["step_type"] == "agent_response" => {
                        delta = v["step_update"]["text_delta"].as_str().map(str::to_string)
                    }
                    "result" => {
                        let r = &v["result"];
                        if r["status"] != "SUCCESS" {
                            events.push(Event::Error(error(&r.to_string())));
                        } else {
                            if !self.streamed {
                                delta = r["response"].as_str().map(str::to_string)
                            }
                            self.complete = true;
                        }
                    }
                    _ => {}
                },
                _ => return Err("chat.error.protocol"),
            }
            if let Some(d) = delta {
                self.streamed = true;
                events.push(Event::Delta(d));
            }
            if self.complete {
                events.push(Event::Done)
            }
        }
        Ok(events)
    }
    pub fn end(&mut self, success: bool) -> Result<Vec<Event>, &'static str> {
        let events = if self.buffer.is_empty() {
            vec![]
        } else {
            self.push(b"\n")?
        };
        if !success || !self.complete {
            Err("chat.error.exit")
        } else {
            Ok(events)
        }
    }
}
pub fn error(text: &str) -> &'static str {
    // Free text can contain paths, prompts and unrelated HTTP log entries.
    // Only typed machine-readable error fields can change account state.
    if let Ok(v) = serde_json::from_str::<Value>(text) {
        return structured_error(&v);
    }
    for line in text.lines() {
        if let Ok(v) = serde_json::from_str::<Value>(line) {
            let key = structured_error(&v);
            if key != "chat.error.exit" {
                return key;
            }
        }
    }
    "chat.error.exit"
}
fn structured_error(v: &Value) -> &'static str {
    for key in [
        "code",
        "type",
        "status",
        "status_code",
        "error_code",
        "error",
    ] {
        let value = v.get(key);
        let name = value
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_ascii_lowercase();
        if value.and_then(Value::as_u64) == Some(401)
            || [
                "401",
                "unauthenticated",
                "authentication_error",
                "invalid_api_key",
                "unauthorized",
                "login_required",
            ]
            .contains(&name.as_str())
        {
            return "chat.error.login";
        }
        if value.and_then(Value::as_u64) == Some(429)
            || [
                "429",
                "rate_limit_error",
                "rate_limit_exceeded",
                "resource_exhausted",
                "quota_exceeded",
            ]
            .contains(&name.as_str())
        {
            return "chat.error.limit";
        }
        if ["unknown_option", "unsupported_version"].contains(&name.as_str()) {
            return "chat.error.version";
        }
    }
    if let Some(inner) = v.get("error").filter(|e| e.is_object()) {
        return structured_error(inner);
    }
    "chat.error.exit"
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unrelated_login_words_and_numbers_never_change_auth_state() {
        for text in [
            "unable to read C:/login/cache401",
            "request 4012 failed",
            r#"{"message":"login guide: response 401"}"#,
            r#"{"result":"401 login"}"#,
        ] {
            assert_eq!(error(text), "chat.error.exit");
        }
        assert_eq!(
            error(r#"{"error":{"type":"authentication_error"}}"#),
            "chat.error.login"
        );
        assert_eq!(error(r#"{"status_code":401}"#), "chat.error.login");
        assert_eq!(
            error(r#"{"error":{"code":"rate_limit_exceeded"}}"#),
            "chat.error.limit"
        );
    }
    #[test]
    fn fragmented_both_formats_no_duplicate_final_text() {
        for (p,s) in [(Provider::ClaudeCode,"{\"type\":\"stream_event\",\"event\":{\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"안녕\"}}}\n{\"type\":\"result\",\"subtype\":\"success\",\"result\":\"안녕\"}\n"),
            (Provider::Antigravity,"{\"event\":\"step_update\",\"step_update\":{\"step_type\":\"agent_response\",\"text_delta\":\"안녕\"}}\n{\"event\":\"result\",\"result\":{\"status\":\"SUCCESS\",\"response\":\"안녕\"}}\n")] {
            let mut parser=Parser::new(p);let mut events=vec![];
            for b in s.as_bytes(){events.extend(parser.push(&[*b]).unwrap())}
            assert_eq!(events,vec![Event::Delta("안녕".into()),Event::Done]);
            assert!(parser.end(true).is_ok());assert!(parser.end(false).is_err());
        }
    }
    #[test]
    fn errors_and_incomplete_streams() {
        for p in [Provider::ClaudeCode, Provider::Antigravity] {
            let mut parser = Parser::new(p);
            assert_eq!(
                parser
                    .push(b"{\"type\":\"error\",\"code\":\"authentication_error\"}\n")
                    .unwrap(),
                vec![Event::Error("chat.error.login")]
            );
            assert!(parser.end(true).is_err());
            assert!(parser.push(b"no json\n").is_err());
        }
    }
}
