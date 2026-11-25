//! A note while nobody types in it, drawn the way Apple's Notes widget
//! shows one: its first line as a bold title, the rest as a quieter
//! preview, the day it was last changed small at the foot. The editor
//! (crate::notes::note's RichEdit) takes over the moment the note has the
//! keyboard, so typing, the IME and Undo stay Windows' own.

//!
//! Drawn with Direct2D and DirectWrite (crate::desklayer::card), so Korean
//! wraps at words (crate::gfx::keep_words) as everywhere else in NopBar.

#[cfg(test)]
use super::Look;
/// Pure: a note's title (its first line with words, a checklist's box
/// left out) and the preview under it (the lines after, blank ones at
/// its start dropped).
pub fn split(text: &str) -> (String, String) {
    let mut lines = text.split(['\n', '\r']).skip_while(|l| l.trim().is_empty());
    let title = lines
        .next()
        .map(|l| {
            l.trim()
                .trim_start_matches(['\u{2610}', '\u{2611}'])
                .trim()
                .to_string()
        })
        .unwrap_or_default();
    let rest: Vec<&str> = lines.skip_while(|l| l.trim().is_empty()).collect();
    (title, rest.join("\n").trim_end().to_string())
}

/// What one face shows (pictures of it).
#[cfg(test)]
pub struct Face<'a> {
    pub text: &'a str,
    /// When it was last changed, as shown ("오후 3:12", "11월 8일").
    pub date: Option<String>,
    pub look: &'a Look,
    /// The display's scale (px per DIP).
    pub scale: f32,
}

/// A picture of a face `w` x `h` px (premultiplied BGRA): the same card
/// the note window draws (crate::notes::note::frame), offscreen.
#[cfg(test)]
pub fn picture(w: i32, h: i32, f: &Face<'_>) -> Option<Vec<u8>> {
    let mut theme = super::panel_theme();
    theme.is_light = f.look.light;
    let date = f.date.clone().unwrap_or_default();
    let frame = super::note::Frame { text: f.text, date: &date, editing: false, typing: None, alpha: 0., mark: None, under: None, lift: false, shadow: true };
    let (dw, dh) = (w as f32 / f.scale, h as f32 / f.scale);
    super::note::render(w, h, f.scale, |dc, gfx| unsafe { super::note::frame(dc, gfx, &theme, &frame, dw, dh) })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_line_is_the_title_and_the_rest_its_preview() {
        assert_eq!(
            split("장보기\n\u{2610} 우유\n\u{2611} 빵"),
            (
                "장보기".to_string(),
                "\u{2610} 우유\n\u{2611} 빵".to_string()
            )
        );
        assert_eq!(
            split("\n\n  제목  \n\n본문\n"),
            ("제목".to_string(), "본문".to_string())
        );
        // A checklist's first item: its text is the title, not its box.
        assert_eq!(split("\u{2610} 우유").0, "우유");
        assert_eq!(split(""), (String::new(), String::new()));
    }
}
