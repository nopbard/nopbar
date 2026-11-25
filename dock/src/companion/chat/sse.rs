/// Incremental byte parser: preserves split UTF-8, CRLF, multi-line SSE and
/// terminal events. A connection ending without response.completed is failure.
#[derive(Default)]
pub struct Parser {
    buffer: Vec<u8>,
    pub completed: bool,
    pub failed: bool,
}
#[derive(Debug, PartialEq)]
pub enum Event {
    Delta(String),
    Complete,
    Error(&'static str),
}
pub fn message(code: &str) -> &'static str {
    match code {
        "subscription_sharing_usage_limit_exceeded"
        | "subscription_sharing_usage_unavailable"
        | "rate_limit_exceeded" => crate::i18n::t("chat.error.rate_limit"),
        "invalid_token" | "invalid_grant" => crate::i18n::t("chat.error.expired"),
        _ => crate::i18n::t("chat.error.response_missing"),
    }
}
impl Parser {
    pub fn push(&mut self, bytes: &[u8]) -> Vec<Event> {
        if self.completed || self.failed {
            return vec![];
        }
        self.buffer.extend_from_slice(bytes);
        let mut out = Vec::new();
        if self.buffer.len() > 1024 * 1024 {
            self.failed = true;
            return vec![Event::Error(message("oversize"))];
        }
        loop {
            let boundary = self
                .buffer
                .windows(2)
                .position(|v| v == b"\n\n")
                .map(|p| (p, 2));
            let crlf = self
                .buffer
                .windows(4)
                .position(|v| v == b"\r\n\r\n")
                .map(|p| (p, 4));
            let Some((p, n)) = boundary.into_iter().chain(crlf).min_by_key(|(p, _)| *p) else {
                break;
            };
            let block: Vec<_> = self.buffer.drain(..p + n).collect();
            let Ok(block) = std::str::from_utf8(&block) else {
                self.failed = true;
                out.push(Event::Error(message("utf8")));
                break;
            };
            let data = block
                .lines()
                .filter_map(|l| {
                    l.strip_prefix("data:")
                        .map(|v| v.strip_prefix(' ').unwrap_or(v))
                })
                .collect::<Vec<_>>()
                .join("\n");
            if data.is_empty() {
                continue;
            }
            if data == "[DONE]" {
                continue;
            }
            let Ok(v) = serde_json::from_str::<serde_json::Value>(&data) else {
                self.failed = true;
                out.push(Event::Error(message("json")));
                break;
            };
            match v["type"].as_str().unwrap_or("") {
                "response.output_text.delta" => {
                    if let Some(s) = v["delta"].as_str() {
                        out.push(Event::Delta(s.into()));
                    }
                }
                "response.completed" => {
                    self.completed = true;
                    out.push(Event::Complete);
                    break;
                }
                "response.failed" | "response.incomplete" | "error" => {
                    self.failed = true;
                    let code = v
                        .pointer("/response/error/code")
                        .or_else(|| v.get("code"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    out.push(Event::Error(message(code)));
                    break;
                }
                _ => {}
            }
        }
        out
    }
    pub fn end(&self) -> Result<(), &'static str> {
        if self.completed {
            Ok(())
        } else {
            Err(crate::i18n::t("chat.error.connection_lost"))
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn arbitrary_byte_splits_and_terminal_errors() {
        let wire="data: {\"type\":\"response.output_text.delta\",\"delta\":\"안녕\"}\r\n\r\ndata: {\"type\":\"response.completed\"}\n\n";
        for n in 1..wire.len() {
            let mut p = Parser::default();
            let mut events = vec![];
            for chunk in wire.as_bytes().chunks(n) {
                events.extend(p.push(chunk));
            }
            assert_eq!(events, vec![Event::Delta("안녕".into()), Event::Complete]);
            assert_eq!(p.end(), Ok(()));
        }
        let mut p = Parser::default();
        assert!(p.end().is_err());
        assert_eq!(p.push(b"data: {\"type\":\"response.failed\",\"response\":{\"error\":{\"code\":\"subscription_sharing_usage_limit_exceeded\"}}}\n\n"),vec![Event::Error(message("subscription_sharing_usage_limit_exceeded"))]);
        assert!(p.end().is_err());
    }
}
