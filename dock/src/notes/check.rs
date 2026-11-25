//! Checklists (pure). On disk a note is Markdown: `- [ ] milk`,
//! `- [x] bread`. In the editor such a line starts with a box character
//! instead (☐ / ☑, U+2610 / U+2611, followed by a space): one character the
//! caret, Undo, the clipboard and the IME all treat as text, clicked to
//! toggle. Nothing is hidden inside the editor (RichEdit's hidden text
//! would let the caret and Backspace walk into the marker), so the text
//! the user sees is the text there is.
//!
//! Columns are UTF-16 units (RichEdit's), and every marker part is ASCII
//! or a BMP character, so a column in the marker is the same in both.

pub const OPEN: char = '\u{2610}';
pub const DONE: char = '\u{2611}';

/// The box for `checked`.
pub fn glyph(checked: bool) -> char {
    if checked {
        DONE
    } else {
        OPEN
    }
}

fn indent_len(line: &str) -> usize {
    line.len() - line.trim_start_matches([' ', '\t']).len()
}

/// A Markdown task line: (indent, checked, the text after the box).
fn md_task(line: &str) -> Option<(&str, bool, &str)> {
    let ind = indent_len(line);
    let t = &line[ind..];
    let r = ["- ", "* ", "+ "].iter().find_map(|b| t.strip_prefix(b))?;
    let (checked, after) = match r.strip_prefix("[ ]") {
        Some(a) => (false, a),
        None => (true, r.strip_prefix("[x]").or_else(|| r.strip_prefix("[X]"))?),
    };
    if after.is_empty() {
        return Some((&line[..ind], checked, ""));
    }
    after.strip_prefix(' ').map(|rest| (&line[..ind], checked, rest))
}

/// An editor task line: (indent, checked, the text after the box).
fn shown_task(line: &str) -> Option<(&str, bool, &str)> {
    let ind = indent_len(line);
    let mut chars = line[ind..].chars();
    let checked = match chars.next()? {
        OPEN => false,
        DONE => true,
        _ => return None,
    };
    let after = chars.as_str();
    if after.is_empty() {
        return Some((&line[..ind], checked, ""));
    }
    after.strip_prefix(' ').map(|rest| (&line[..ind], checked, rest))
}

/// Pure: the logical line (paragraph) holding UTF-16 position `caret` in
/// `text`: [start, end) up to the breaks ('\r' or '\n') around it, never
/// a line that only wraps on screen.
pub fn paragraph(text: &[u16], caret: usize) -> (usize, usize) {
    let brk = |c: &u16| *c == b'\r' as u16 || *c == b'\n' as u16;
    let caret = caret.min(text.len());
    let start = text[..caret].iter().rposition(brk).map(|i| i + 1).unwrap_or(0);
    let end = text[caret..].iter().position(brk).map(|i| caret + i).unwrap_or(text.len());
    (start, end)
}

/// Line breaks as "\n" (RichEdit gives "\r").
pub fn normalize(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

/// Markdown (as stored) to the editor's text.
pub fn to_display(md: &str) -> String {
    normalize(md)
        .split('\n')
        .map(|l| match md_task(l) {
            Some((ind, c, rest)) => format!("{ind}{} {rest}", glyph(c)),
            None => l.to_string(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The editor's text to Markdown (as stored).
pub fn to_markdown(shown: &str) -> String {
    normalize(shown)
        .split('\n')
        .map(|l| match shown_task(l) {
            Some((ind, c, rest)) => {
                let b = if c { "[x]" } else { "[ ]" };
                if rest.is_empty() {
                    format!("{ind}- {b}")
                } else {
                    format!("{ind}- {b} {rest}")
                }
            }
            None => l.to_string(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The box on an editor line: its column and whether it is checked.
pub fn marker(line: &str) -> Option<(usize, bool)> {
    shown_task(line).map(|(ind, c, _)| (ind.len(), c))
}

/// What typed Markdown at the start of a line becomes: the UTF-16 range
/// [start, end) to replace and the box with its space. "- [ ] ", "* [ ] ",
/// "[] ", "[ ] " and their checked forms.
pub fn autoconvert(line: &str) -> Option<(usize, usize, String)> {
    let ind = indent_len(line);
    let t = &line[ind..];
    const OPEN_FORMS: [&str; 5] = ["- [ ] ", "* [ ] ", "+ [ ] ", "[ ] ", "[] "];
    const DONE_FORMS: [&str; 6] = ["- [x] ", "- [X] ", "* [x] ", "+ [x] ", "[x] ", "[X] "];
    let hit = |forms: &[&str]| forms.iter().find(|f| t.starts_with(**f)).map(|f| f.len());
    let (len, checked) = match (hit(&OPEN_FORMS), hit(&DONE_FORMS)) {
        (Some(n), _) => (n, false),
        (None, Some(n)) => (n, true),
        _ => return None,
    };
    Some((ind, ind + len, format!("{} ", glyph(checked))))
}

/// What Enter does on a line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Enter {
    /// A plain new line.
    Normal,
    /// A new line, then this (the next empty box, same indent).
    Continue(String),
    /// An empty item: Enter ends the list (the line's [start, end) UTF-16
    /// range is cleared instead of a new line).
    EndList { start: usize, end: usize },
}

/// Pure: Enter at UTF-16 column `caret` of `line`.
pub fn on_enter(line: &str, caret: usize) -> Enter {
    let Some((ind, _, rest)) = shown_task(line) else { return Enter::Normal };
    // Before or on the box: a plain line break.
    if caret <= ind.len() {
        return Enter::Normal;
    }
    if rest.trim().is_empty() {
        return Enter::EndList { start: 0, end: line.encode_utf16().count() };
    }
    Enter::Continue(format!("{ind}{OPEN} "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown_tasks_show_as_boxes_and_go_back() {
        let md = "Shopping\n- [ ] milk\n- [x] bread\n  - [X] nested\n- [ ]\nplain - [ ] not a task\n";
        let shown = to_display(md);
        assert_eq!(shown, "Shopping\n\u{2610} milk\n\u{2611} bread\n  \u{2611} nested\n\u{2610} \nplain - [ ] not a task\n");
        assert_eq!(to_markdown(&shown), "Shopping\n- [ ] milk\n- [x] bread\n  - [x] nested\n- [ ]\nplain - [ ] not a task\n");
    }

    #[test]
    fn other_bullets_and_broken_forms() {
        assert_eq!(to_display("* [ ] a\n+ [x] b"), "\u{2610} a\n\u{2611} b");
        // No space after the box, or no bullet: left alone.
        assert_eq!(to_display("- [ ]x\n[ ] y\n- [y] z"), "- [ ]x\n[ ] y\n- [y] z");
        // A box glued to text in the editor stays literal on disk.
        assert_eq!(to_markdown("\u{2610}abc"), "\u{2610}abc");
    }

    #[test]
    fn line_breaks_from_the_editor_are_normalized() {
        assert_eq!(to_markdown("a\r\u{2610} b\r\nc"), "a\n- [ ] b\nc");
        assert_eq!(normalize("a\r\nb\rc\n"), "a\nb\nc\n");
    }

    #[test]
    fn hangul_round_trips() {
        let md = "- [ ] 우유 사기\n- [x] 빵\n한글 메모";
        assert_eq!(to_markdown(&to_display(md)), md);
    }

    #[test]
    fn the_box_is_found_for_clicks() {
        assert_eq!(marker("\u{2610} milk"), Some((0, false)));
        assert_eq!(marker("\t  \u{2611} done"), Some((3, true)));
        assert_eq!(marker("\u{2611}"), Some((0, true)));
        assert_eq!(marker("milk \u{2610}"), None);
        assert_eq!(marker(""), None);
    }

    #[test]
    fn typed_markdown_becomes_a_box() {
        assert_eq!(autoconvert("- [ ] "), Some((0, 6, "\u{2610} ".into())));
        assert_eq!(autoconvert("  [] milk"), Some((2, 5, "\u{2610} ".into())));
        assert_eq!(autoconvert("- [x] "), Some((0, 6, "\u{2611} ".into())));
        assert_eq!(autoconvert("- [ ]"), None, "waits for the space");
        assert_eq!(autoconvert("text [] "), None);
    }

    /// Review #9: Enter reads the whole item, wherever the screen wraps it.
    #[test]
    fn a_wrapped_item_is_still_one_item() {
        let text = "intro\r\u{2610} a very long item that wraps on a narrow note\rnext";
        let u: Vec<u16> = text.encode_utf16().collect();
        // The caret at the item's end (far past where a wrap would be).
        let caret = text.find("\rnext").map(|b| text[..b].encode_utf16().count()).unwrap();
        let (s, e) = paragraph(&u, caret);
        let item = String::from_utf16_lossy(&u[s..e]);
        assert!(item.starts_with('\u{2610}') && item.ends_with("narrow note"));
        assert_eq!(on_enter(&item, caret - s), Enter::Continue("\u{2610} ".into()));
        assert_eq!(paragraph(&u, 0), (0, 5));
        assert_eq!(paragraph(&u, u.len()), (u.len() - 4, u.len()));
        assert_eq!(paragraph(&[], 3), (0, 0));
    }

    #[test]
    fn enter_continues_a_list_and_ends_it_on_an_empty_item() {
        assert_eq!(on_enter("\u{2610} milk", 6), Enter::Continue("\u{2610} ".into()));
        assert_eq!(on_enter("  \u{2611} done", 8), Enter::Continue("  \u{2610} ".into()));
        assert_eq!(on_enter("\u{2610} ", 2), Enter::EndList { start: 0, end: 2 });
        assert_eq!(on_enter("  \u{2610}", 3), Enter::EndList { start: 0, end: 3 });
        // Before the box, or not a task: a plain line break.
        assert_eq!(on_enter("\u{2610} milk", 0), Enter::Normal);
        assert_eq!(on_enter("milk", 4), Enter::Normal);
    }
}
