use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq)]
pub enum Provider {
    #[default]
    ChatGpt,
    ClaudeCode,
    Antigravity,
}
impl Provider {
    pub fn name(self) -> &'static str {
        match self { Self::ChatGpt => "ChatGPT", Self::ClaudeCode => "Claude Code", Self::Antigravity => "Antigravity" }
    }
}

/// Only preferences are serialized. History and credentials cannot enter Config.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Config {
    pub keep_history: bool,
    pub provider: Provider,
    pub model: String,
    pub daily_limit: u32,
    pub claude_mcp: bool,
    pub codex_mcp: bool,
    pub antigravity_mcp: bool,
    pub claude_connected: bool,
    pub antigravity_connected: bool,
    pub claude_model: String,
    pub antigravity_model: String,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            keep_history: false,
            provider: Provider::ChatGpt,
            model: String::new(),
            daily_limit: 100,
            claude_mcp: false,
            codex_mcp: false,
            antigravity_mcp: false,
            claude_connected: false,
            antigravity_connected: false,
            claude_model: String::new(),
            antigravity_model: String::new(),
        }
    }
}
impl Config {
    pub fn connected(&self, chatgpt: bool) -> bool {
        match self.provider { Provider::ChatGpt => chatgpt, Provider::ClaudeCode => self.claude_connected, Provider::Antigravity => self.antigravity_connected }
    }
    pub fn active_model(&self) -> &str {
        match self.provider { Provider::ChatGpt => &self.model, Provider::ClaudeCode => &self.claude_model, Provider::Antigravity => &self.antigravity_model }
    }
    pub fn set_model(&mut self, model: String) {
        match self.provider { Provider::ChatGpt => self.model=model, Provider::ClaudeCode => self.claude_model=model, Provider::Antigravity => self.antigravity_model=model }
    }
    pub fn activate(&mut self, provider: Provider) {
        self.provider = provider;
        match provider { Provider::ClaudeCode => self.claude_connected=true, Provider::Antigravity => self.antigravity_connected=true, _=>{} }
    }
    pub fn disconnect_cli(&mut self) {
        match self.provider { Provider::ClaudeCode => self.claude_connected=false, Provider::Antigravity => self.antigravity_connected=false, _=>{} }
    }
}

#[derive(Default)]
pub struct Conversation {
    pub turns: VecDeque<(String, String)>,
    pub pending: Option<String>,
    pub answer: String,
    pub error: Option<&'static str>,
    pub generation: u64,
}
impl Conversation {
    pub fn begin(&mut self, text: String) -> bool {
        if self.pending.is_some() || text.trim().is_empty() || text.len() > 32_768 {
            return false;
        }
        self.generation += 1;
        self.pending = Some(text);
        self.answer.clear();
        self.error = None;
        true
    }
    pub fn input(&self) -> serde_json::Value {
        let mut messages = Vec::new();
        for (u, a) in &self.turns {
            messages.push(serde_json::json!({"role":"user","content":u}));
            messages.push(serde_json::json!({"role":"assistant","content":a}));
        }
        if let Some(u) = &self.pending {
            messages.push(serde_json::json!({"role":"user","content":u}));
        }
        messages.into()
    }
    pub fn delta(&mut self, generation: u64, text: &str) {
        if self.generation == generation
            && self.pending.is_some()
            && self.answer.len() + text.len() <= 256 * 1024
        {
            self.answer.push_str(text);
        }
    }
    pub fn finish(&mut self, generation: u64, error: Option<&'static str>) {
        if self.generation != generation {
            return;
        }
        if let Some(u) = self.pending.take() {
            if error.is_none() {
                self.turns.push_back((u, self.answer.clone()));
                while self.turns.len() > 20 {
                    self.turns.pop_front();
                }
            }
        }
        self.error = error;
    }
    pub fn cancel(&mut self) {
        self.generation += 1;
        self.pending = None;
    }
    pub fn clear(&mut self) {
        self.cancel();
        self.turns.clear();
        self.answer.clear();
        self.error = None;
    }
}

#[derive(Debug, PartialEq)]
pub enum Key {
    Send,
    Newline,
    Close,
    Native,
}
pub fn key(vk: u32, shift: bool, composing: bool) -> Key {
    if composing {
        return Key::Native;
    }
    match vk {
        13 if shift => Key::Newline,
        13 => Key::Send,
        27 => Key::Close,
        _ => Key::Native,
    }
}

#[derive(Default, Serialize, Deserialize, Debug, PartialEq)]
pub struct Usage {
    pub day: i64,
    pub count: u32,
}
impl Usage {
    pub fn limit_reached(count: u32, limit: u32) -> bool {
        limit != 0 && count >= limit
    }
    pub fn today(&self, day: i64) -> u32 {
        if self.day == day {
            self.count
        } else {
            0
        }
    }
    pub fn reserve(&mut self, day: i64, limit: u32) -> bool {
        if self.day != day {
            self.day = day;
            self.count = 0;
        }
        if Self::limit_reached(self.count, limit) {
            return false;
        }
        self.count = self.count.saturating_add(1);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn memory_is_twenty_complete_turns_and_late_events_are_ignored() {
        let mut c = Conversation::default();
        for i in 0..23 {
            assert!(c.begin(i.to_string()));
            c.delta(c.generation, "답");
            c.finish(c.generation, None);
        }
        assert_eq!(c.turns.len(), 20);
        assert_eq!(c.turns[0].0, "3");
        c.begin("cancel".into());
        let g = c.generation;
        c.cancel();
        c.delta(g, "late");
        c.finish(g, None);
        assert_eq!(c.turns.len(), 20);
        c.clear();
        assert!(c.turns.is_empty());
        assert!(c.answer.is_empty());
    }
    #[test]
    fn ime_and_keys() {
        assert_eq!(key(13, false, true), Key::Native);
        assert_eq!(key(13, true, false), Key::Newline);
        assert_eq!(key(13, false, false), Key::Send);
        assert_eq!(key(27, false, false), Key::Close);
    }
    #[test]
    fn quota_resets_and_never_calls_after_limit() {
        let mut u = Usage::default();
        let mut requests = 0;
        for _ in 0..120 {
            if u.reserve(20261114, 100) {
                requests += 1;
            }
        }
        assert_eq!(requests, 100);
        assert!(!u.reserve(20261114, 100));
        assert!(u.reserve(20261115, 100));
        assert_eq!(u.count, 1);
        assert!(u.reserve(20261116, 0));
        assert!(u.reserve(20261116, 0));
        assert_eq!(u.count, 2);
    }
    #[test]
    fn preferences_have_no_secrets_or_history() {
        let c = serde_json::to_string(&Config::default()).unwrap();
        for k in ["token", "turns", "messages", "password"] {
            assert!(!c.contains(k));
        }
    }
}
