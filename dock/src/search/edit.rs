//! A single-line text field's text, caret and selection (no drawing).
//! Positions are char indexes.

/// The most characters the field holds (typed or pasted).
pub const MAX_LEN: usize = 512;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Field {
    text: Vec<char>,
    caret: usize,
    /// The other end of the selection (== caret: nothing selected).
    anchor: usize,
}

fn is_space(c: char) -> bool {
    c.is_whitespace()
}

impl Field {
    pub fn text(&self) -> String {
        self.text.iter().collect()
    }
    pub fn caret(&self) -> usize {
        self.caret
    }
    /// The selected range (start, end); empty when start == end.
    pub fn selection(&self) -> (usize, usize) {
        (self.caret.min(self.anchor), self.caret.max(self.anchor))
    }

    fn delete_selection(&mut self) -> bool {
        let (a, b) = self.selection();
        if a == b {
            return false;
        }
        self.text.drain(a..b);
        self.caret = a;
        self.anchor = a;
        true
    }

    /// Types `s` over the selection. Line breaks and other control
    /// characters become spaces (one line only). What would go past
    /// MAX_LEN is left out.
    pub fn insert(&mut self, s: &str) {
        self.insert_text(s, false);
    }

    /// Same editor mechanics for the companion's multiline field.
    pub fn insert_multiline(&mut self, s: &str) {
        self.insert_text(&s.replace("\r\n", "\n").replace('\r', "\n"), true);
    }

    fn insert_text(&mut self, s: &str, multiline: bool) {
        self.delete_selection();
        let room = (if multiline { 8192 } else { MAX_LEN }).saturating_sub(self.text.len());
        let add: Vec<char> = s.chars().take(room).map(|c| if c.is_control() && !(multiline && c == '\n') { ' ' } else { c }).collect();
        let n = add.len();
        self.text.splice(self.caret..self.caret, add);
        self.caret += n;
        self.anchor = self.caret;
    }

    /// Start of the word left of `i` (spaces skipped first).
    fn word_left(&self, mut i: usize) -> usize {
        while i > 0 && is_space(self.text[i - 1]) {
            i -= 1;
        }
        while i > 0 && !is_space(self.text[i - 1]) {
            i -= 1;
        }
        i
    }

    /// End of the word right of `i` (then the spaces after it).
    fn word_right(&self, mut i: usize) -> usize {
        let n = self.text.len();
        while i < n && !is_space(self.text[i]) {
            i += 1;
        }
        while i < n && is_space(self.text[i]) {
            i += 1;
        }
        i
    }

    /// Backspace (Ctrl: the word before the caret).
    pub fn backspace(&mut self, word: bool) {
        if self.delete_selection() || self.caret == 0 {
            return;
        }
        let from = if word { self.word_left(self.caret) } else { self.caret - 1 };
        self.text.drain(from..self.caret);
        self.caret = from;
        self.anchor = from;
    }

    /// Delete (Ctrl: to the end of the word).
    pub fn delete(&mut self, word: bool) {
        if self.delete_selection() || self.caret >= self.text.len() {
            return;
        }
        let to = if word { self.word_right(self.caret) } else { self.caret + 1 };
        self.text.drain(self.caret..to);
    }

    fn move_to(&mut self, i: usize, extend: bool) {
        self.caret = i.min(self.text.len());
        if !extend {
            self.anchor = self.caret;
        }
    }

    pub fn left(&mut self, word: bool, extend: bool) {
        let (a, b) = self.selection();
        if a != b && !extend {
            self.move_to(a, false);
            return;
        }
        let i = if word { self.word_left(self.caret) } else { self.caret.saturating_sub(1) };
        self.move_to(i, extend);
    }

    pub fn right(&mut self, word: bool, extend: bool) {
        let (a, b) = self.selection();
        if a != b && !extend {
            self.move_to(b, false);
            return;
        }
        let i = if word { self.word_right(self.caret) } else { self.caret + 1 };
        self.move_to(i, extend);
    }

    pub fn home(&mut self, extend: bool) {
        self.move_to(0, extend);
    }

    pub fn end(&mut self, extend: bool) {
        self.move_to(self.text.len(), extend);
    }

    pub fn select_all(&mut self) {
        self.anchor = 0;
        self.caret = self.text.len();
    }

    /// Puts the caret at `i` (a click).
    pub fn click(&mut self, i: usize, extend: bool) {
        self.move_to(i, extend);
    }

    /// The text with `comp` (an IME composition) at the caret, over the
    /// selection, and where the composition sits in it (char indexes).
    pub fn with_composition(&self, comp: &[char]) -> (Vec<char>, usize) {
        if comp.is_empty() {
            return (self.text.clone(), self.caret);
        }
        let (a, b) = self.selection();
        let mut v = self.text[..a].to_vec();
        v.extend_from_slice(comp);
        v.extend_from_slice(&self.text[b..]);
        (v, a)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field(s: &str) -> Field {
        let mut f = Field::default();
        f.insert(s);
        f
    }

    #[test]
    fn typing_and_moving() {
        let mut f = field("helo");
        f.left(false, false);
        f.insert("l");
        assert_eq!(f.text(), "hello");
        assert_eq!(f.caret(), 4);
        f.home(false);
        f.insert(">");
        f.end(false);
        f.insert("!");
        assert_eq!(f.text(), ">hello!");
        f.right(false, false);
        assert_eq!(f.caret(), 7);
        f.home(false);
        f.left(false, false);
        assert_eq!(f.caret(), 0);
    }

    #[test]
    fn deleting() {
        let mut f = field("abc");
        f.backspace(false);
        assert_eq!(f.text(), "ab");
        f.home(false);
        f.delete(false);
        assert_eq!(f.text(), "b");
        f.backspace(false);
        assert_eq!(f.text(), "b");
        f.end(false);
        f.delete(false);
        assert_eq!(f.text(), "b");
    }

    #[test]
    fn words() {
        let mut f = field("open the  door");
        f.backspace(true);
        assert_eq!(f.text(), "open the  ");
        f.backspace(true);
        assert_eq!(f.text(), "open ");
        f.home(false);
        f.delete(true);
        assert_eq!(f.text(), "");
        let mut f = field("ab cd ef");
        f.home(false);
        f.right(true, false);
        assert_eq!(f.caret(), 3);
        f.end(false);
        f.left(true, false);
        assert_eq!(f.caret(), 6);
    }

    #[test]
    fn selection() {
        let mut f = field("hello world");
        f.select_all();
        assert_eq!(f.selection(), (0, 11));
        f.insert("x");
        assert_eq!(f.text(), "x");
        let mut f = field("hello");
        f.left(false, true);
        f.left(false, true);
        assert_eq!(f.selection(), (3, 5));
        f.backspace(false);
        assert_eq!(f.text(), "hel");
        let mut f = field("abc");
        f.home(true);
        f.right(false, false);
        // Collapses to the selection's end.
        assert_eq!((f.caret(), f.selection()), (3, (3, 3)));
    }

    #[test]
    fn length_is_capped() {
        let mut f = field(&"a".repeat(MAX_LEN + 50));
        assert_eq!(f.text().chars().count(), MAX_LEN);
        f.insert("b");
        assert_eq!(f.text().chars().count(), MAX_LEN);
        // Typing over a selection makes room first.
        f.select_all();
        f.insert("xyz");
        assert_eq!(f.text(), "xyz");
    }

    #[test]
    fn paste_is_one_line() {
        let mut f = field("a");
        f.insert("b\r\nc\td");
        assert_eq!(f.text(), "ab  c d");
    }

    #[test]
    fn composition_goes_at_the_caret() {
        let mut f = field("카카");
        f.left(false, false);
        let (v, at) = f.with_composition(&['오']);
        assert_eq!((v.iter().collect::<String>(), at), ("카오카".to_string(), 1));
        let (v, at) = field("ab").with_composition(&[]);
        assert_eq!((v.len(), at), (2, 2));
    }
}
