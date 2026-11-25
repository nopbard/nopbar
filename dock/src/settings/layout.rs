//! What a page drew and where: its headings, its cards' rows, each row's
//! label and control, and every text. Logged only while a test draws a
//! page (App::log is None in the window: nothing is kept), then checked
//! against the one set of spacing rules every page follows (check):
//!
//! - everything sits in a card, a heading over one, or a picture of its
//!   own (no loose text, fields or buttons between the cards);
//! - two cards are GROUP_GAP apart; a heading HEADING_ABOVE under what is
//!   over it and its card right under it (its box holds the space);
//! - a card's rows follow each other with no gap, a hairline over every
//!   row but the first, from the text's inset;
//! - nothing overlaps; the page ends PAGE_END under its last card;
//! - a row's control ends on the card's inner right edge (ROW_PAD in),
//!   is centered on the row's line, as tall as its kind's token (and as
//!   wide, for switches and fields), and the label (with its "i") stops
//!   before it;
//! - every row's buttons and fields hug an edge of its card (the last
//!   ends on the inner right edge, or the first starts on the inner left
//!   one: a list's "Add…"), and no two of them, nor an "i", overlap (a
//!   button inside a control's box is that control's part);
//! - an "i" sits in a card or a heading; nothing drawn is empty; a
//!   hairline runs from inset to inset; no text is cut short where it is
//!   drawn (Kind::Cut: a text wider than its box).
//!
//! What a click reaches is checked apart (hit_conflicts): two hit areas
//! overlap only when one holds the other and the inner one, pushed later,
//! wins.
//!
//! The vertical rules are the same functions the pages lay out with
//! (gap_before): drawing and checking cannot drift apart.

#[cfg(test)]
use windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F;

use super::rows::tok;

/// Space over a heading that follows a card (its box, HEADER_H, holds
/// the space between its text and the card under it).
pub(super) const HEADING_ABOVE: f32 = tok::GROUP_GAP - tok::ROW_GAP;
/// Space under a page's last card.
pub(super) const PAGE_END: f32 = tok::GROUP_GAP;

/// What was drawn last on a page (for the space before the next thing).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Block {
    /// Nothing yet.
    Start,
    /// A heading.
    Header,
    /// A card (or a picture of its own).
    Card,
}

/// The space before a `next` block after `prev`. Pure: tested.
pub(super) fn gap_before(prev: Block, next: Block) -> f32 {
    match (prev, next) {
        (Block::Start, _) => 0.0,
        (Block::Header, _) => 0.0,
        (Block::Card, Block::Card) => tok::GROUP_GAP,
        (Block::Card, Block::Header) => HEADING_ABOVE,
        (Block::Card, Block::Start) => 0.0,
    }
}

/// A row's control, by kind (each kind one height; switches and fields
/// one width too).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum CtlKind {
    Toggle,
    /// A list button or a key field (one width token: FIELD_W).
    Field,
    Choice,
    Slider,
    Button,
    /// The accent's colors.
    Swatches,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub(super) enum Kind {
    /// A heading's whole box.
    Header,
    /// One row of a card (`first` / `last`: its card's ends); a card of
    /// one row is both.
    Seg { first: bool, last: bool },
    /// A picture of its own (the display map, the bar's preview): a card.
    Picture,
    /// The hairline over a row.
    Hairline,
    /// A row's control; `cy`: the center of the row's line.
    Ctl { kind: CtlKind, cy: f32 },
    /// A row's label with its "i" (the room they take).
    Label,
    /// An "i"'s hit target.
    Info,
    /// A text drawn anywhere on the page.
    #[cfg(test)]
    Text,
    /// A field or button drawn anywhere on the page.
    Field,
    Button,
    /// A text wider than the box it is drawn in (cut at its edge): its
    /// box, the text as key.
    Cut,
    /// The keyboard's focus ring (its outer edge).
    Ring,
    /// A line under a row (status::row_note): its box, the text as key.
    Note,
}

#[cfg(test)]
#[derive(Clone, Debug)]
pub(super) struct Item {
    pub kind: Kind,
    pub key: String,
    pub r: D2D_RECT_F,
}

#[cfg(test)]
const EPS: f32 = 0.51;

#[cfg(test)]
fn near(a: f32, b: f32) -> bool {
    (a - b).abs() <= EPS
}

#[cfg(test)]
fn inside(a: D2D_RECT_F, b: D2D_RECT_F) -> bool {
    a.left >= b.left - EPS && a.right <= b.right + EPS && a.top >= b.top - EPS && a.bottom <= b.bottom + EPS
}

#[cfg(test)]
fn overlap(a: D2D_RECT_F, b: D2D_RECT_F) -> bool {
    a.left < b.right - EPS && b.left < a.right - EPS && a.top < b.bottom - EPS && b.top < a.bottom - EPS
}

#[cfg(test)]
fn fmt(r: D2D_RECT_F) -> String {
    format!("({:.1},{:.1})-({:.1},{:.1})", r.left, r.top, r.right, r.bottom)
}

/// A card: its rows' union, with the key of its first row.
#[cfg(test)]
#[derive(Clone, Debug)]
pub(super) struct Card {
    pub key: String,
    pub r: D2D_RECT_F,
}

/// The page's blocks in order: headings and cards (rows joined into their
/// cards, pictures as cards), with what is wrong inside a card.
#[cfg(test)]
pub(super) fn blocks(items: &[Item], bad: &mut Vec<String>) -> Vec<(Block, Card)> {
    let mut out: Vec<(Block, Card)> = Vec::new();
    let mut open: Option<Card> = None;
    for it in items {
        match it.kind {
            Kind::Header => {
                if let Some(c) = open.take() {
                    bad.push(format!("{}: card not closed before heading {}", c.key, it.key));
                    out.push((Block::Card, c));
                }
                out.push((Block::Header, Card { key: it.key.clone(), r: it.r }));
            }
            Kind::Picture => {
                if let Some(c) = open.take() {
                    bad.push(format!("{}: card not closed before picture {}", c.key, it.key));
                    out.push((Block::Card, c));
                }
                out.push((Block::Card, Card { key: it.key.clone(), r: it.r }));
            }
            Kind::Seg { first, last } => {
                match (&mut open, first) {
                    (Some(c), false) => {
                        if !near(c.r.bottom, it.r.top) {
                            bad.push(format!("{}: row {} starts {:.1} under the row over it", c.key, it.key, it.r.top - c.r.bottom));
                        }
                        if !near(c.r.left, it.r.left) || !near(c.r.right, it.r.right) {
                            bad.push(format!("{}: row {} not as wide as its card", c.key, it.key));
                        }
                        c.r.bottom = c.r.bottom.max(it.r.bottom);
                    }
                    (Some(c), true) => {
                        bad.push(format!("{}: card not closed before {}", c.key, it.key));
                        let c = open.take().unwrap();
                        out.push((Block::Card, c));
                        open = Some(Card { key: it.key.clone(), r: it.r });
                    }
                    (None, true) => open = Some(Card { key: it.key.clone(), r: it.r }),
                    (None, false) => {
                        bad.push(format!("{}: a card's row with no first row", it.key));
                        open = Some(Card { key: it.key.clone(), r: it.r });
                    }
                }
                if last {
                    if let Some(c) = open.take() {
                        out.push((Block::Card, c));
                    }
                }
            }
            _ => {}
        }
    }
    if let Some(c) = open {
        bad.push(format!("{}: card not closed at the page's end", c.key));
        out.push((Block::Card, c));
    }
    out
}

/// Everything wrong with a page's layout (empty: none). `top`: where the
/// page's content starts, `end`: where it ends (the scroll height's
/// bottom); `rtl`: the page reads right to left (its controls at the
/// cards' left).
#[cfg(test)]
pub(super) fn check(page: &str, items: &[Item], top: f32, end: f32, rtl: bool) -> Vec<String> {
    let mut bad = Vec::new();
    let segs: Vec<&Item> = items.iter().filter(|i| matches!(i.kind, Kind::Seg { .. })).collect();
    let blocks = blocks(items, &mut bad);
    // The vertical rhythm.
    let mut prev = Block::Start;
    let mut y = top;
    for (b, c) in &blocks {
        let want = y + gap_before(prev, *b);
        if !near(c.r.top, want) {
            bad.push(format!("{} after {:?}: starts at {:.1}, wants {:.1} (gap {:.1} -> {:.1})", c.key, prev, c.r.top, want, c.r.top - y, want - y));
        }
        if *b == Block::Header && !near(c.r.bottom - c.r.top, tok::HEADER_H) {
            bad.push(format!("{}: heading {:.1} high, wants {}", c.key, c.r.bottom - c.r.top, tok::HEADER_H));
        }
        y = c.r.bottom;
        prev = *b;
    }
    if prev == Block::Header {
        bad.push("page ends on a heading".into());
    }
    if !blocks.is_empty() && !near(end - y, PAGE_END) {
        bad.push(format!("page end: {:.1} under the last card, wants {PAGE_END}", end - y));
    }
    // Nothing overlaps.
    for (i, (_, a)) in blocks.iter().enumerate() {
        for (_, b) in blocks.iter().skip(i + 1) {
            if overlap(a.r, b.r) {
                bad.push(format!("{} overlaps {}", a.key, b.key));
            }
        }
    }
    // A hairline over every row but a card's first, from the text inset
    // to the other (one end further in under an open "More options").
    for s in &segs {
        if let Kind::Seg { first: false, .. } = s.kind {
            let (l, r) = (s.r.left + tok::ROW_PAD, s.r.right - tok::ROW_PAD);
            let line = items.iter().any(|h| {
                h.kind == Kind::Hairline
                    && (h.r.top - s.r.top).abs() <= 1.0
                    && h.r.left >= l - EPS
                    && h.r.right <= r + EPS
                    && (near(h.r.left, l) || near(h.r.right, r))
                    && h.r.right - h.r.left >= r - l - tok::FOLD_INDENT - EPS
            });
            if !line {
                bad.push(format!("{}: no hairline over it from inset to inset", s.key));
            }
        }
    }
    // Nothing loose: every text, field, button and "i" in a card or a
    // heading; nothing drawn empty (a text with no words is not drawn).
    let homes: Vec<D2D_RECT_F> = blocks.iter().map(|(_, c)| c.r).collect();
    for it in items.iter().filter(|i| matches!(i.kind, Kind::Text | Kind::Field | Kind::Button | Kind::Ctl { .. } | Kind::Label | Kind::Info)) {
        if it.r.bottom - it.r.top <= 0.5 || it.r.right - it.r.left <= 0.5 {
            if !(it.kind == Kind::Text && it.key.trim().is_empty()) {
                bad.push(format!("{:?} {:?} {} has no size", it.kind, it.key, fmt(it.r)));
            }
            continue;
        }
        // An "i" is its 24 DIP target: its middle (the glyph) is what sits
        // in its heading or card.
        let at = if it.kind == Kind::Info {
            let (x, y) = ((it.r.left + it.r.right) / 2.0, (it.r.top + it.r.bottom) / 2.0);
            D2D_RECT_F { left: x, top: y, right: x, bottom: y }
        } else {
            it.r
        };
        if !homes.iter().any(|h| inside(at, *h)) {
            bad.push(format!("{:?} {:?} {} outside every card", it.kind, it.key, fmt(it.r)));
        }
    }
    // No text cut short.
    for it in items.iter().filter(|i| i.kind == Kind::Cut) {
        bad.push(format!("text {:?} cut at {}", it.key, fmt(it.r)));
    }
    // What can be clicked or focused does not overlap: buttons, fields,
    // controls, "i"s (a button or field inside a control's box is part of
    // that control: the update card's buttons).
    let parts: Vec<&Item> = items.iter().filter(|i| matches!(i.kind, Kind::Button | Kind::Field | Kind::Ctl { .. } | Kind::Info)).collect();
    for (i, a) in parts.iter().enumerate() {
        for b in parts.iter().skip(i + 1) {
            if !overlap(a.r, b.r) {
                continue;
            }
            let part_of = |x: &Item, y: &Item| matches!(x.kind, Kind::Button | Kind::Field) && matches!(y.kind, Kind::Ctl { .. }) && inside(x.r, y.r);
            if !part_of(a, b) && !part_of(b, a) {
                bad.push(format!("{:?} {:?} {} overlaps {:?} {:?} {}", a.kind, a.key, fmt(a.r), b.kind, b.key, fmt(b.r)));
            }
        }
    }
    // A row's buttons and fields hug an edge of its card.
    for s in &segs {
        let (l, r) = (s.r.left + tok::ROW_PAD, s.r.right - tok::ROW_PAD);
        let inner: Vec<&&Item> = parts.iter().filter(|p| matches!(p.kind, Kind::Button | Kind::Field | Kind::Ctl { .. }) && inside(p.r, s.r)).collect();
        if inner.is_empty() {
            continue;
        }
        let first = inner.iter().map(|p| p.r.left).fold(f32::MAX, f32::min);
        let last = inner.iter().map(|p| p.r.right).fold(f32::MIN, f32::max);
        if !near(last, r) && !near(first, l) {
            bad.push(format!("{}: its controls ({:.1}..{:.1}) hug neither edge ({l:.1}, {r:.1})", s.key, first, last));
        }
    }
    // The controls.
    let mut sizes: Vec<(CtlKind, f32, f32, String)> = Vec::new();
    for it in items {
        let Kind::Ctl { kind, cy } = it.kind else { continue };
        let Some(seg) = segs.iter().find(|s| inside(it.r, s.r)) else { continue };
        let (edge, want) = if rtl { (it.r.left, seg.r.left + tok::ROW_PAD) } else { (it.r.right, seg.r.right - tok::ROW_PAD) };
        if !near(edge, want) {
            bad.push(format!("{}: {:?} ends at {:.1}, wants {:.1} ({:+.1})", it.key, kind, edge, want, edge - want));
        }
        if !near((it.r.top + it.r.bottom) / 2.0, cy) {
            bad.push(format!("{}: {:?} centered at {:.1}, its line at {:.1}", it.key, kind, (it.r.top + it.r.bottom) / 2.0, cy));
        }
        sizes.push((kind, it.r.right - it.r.left, it.r.bottom - it.r.top, it.key.clone()));
        // Its label (and "i") stops before it.
        for l in items.iter().filter(|l| l.kind == Kind::Label && l.key == it.key) {
            let into = if rtl { l.r.left < it.r.right + tok::ROW_GAP - EPS && l.r.right > it.r.left } else { l.r.right > it.r.left - tok::ROW_GAP + EPS && l.r.left < it.r.right };
            if into {
                bad.push(format!("{}: label {} runs into its {:?} {}", it.key, fmt(l.r), kind, fmt(it.r)));
            }
        }
    }
    // A field's value (a key field's, a list button's) on the field's
    // center line, so a column of them reads as one; only an end's
    // chevron, Reset or × pushes a long one off it (rows::field_value_x).
    for it in items {
        let Kind::Ctl { kind: CtlKind::Field, .. } = it.kind else { continue };
        let fc = (it.r.left + it.r.right) / 2.0;
        for v in items.iter().filter(|v| v.kind == Kind::Text && !v.key.trim().is_empty() && inside(v.r, it.r)) {
            let vc = (v.r.left + v.r.right) / 2.0;
            let pushed = if rtl { near(v.r.left, it.r.left + super::rows::RESET_W) } else { near(v.r.right, it.r.right - super::rows::RESET_W) };
            if !near(vc, fc) && !pushed {
                bad.push(format!("{}: value {:?} centered at {:.1}, its field at {:.1}", it.key, v.key, vc, fc));
            }
        }
    }
    // No text on a hairline (a slider's mark names over the next row's).
    for h in items.iter().filter(|h| h.kind == Kind::Hairline) {
        let y = (h.r.top + h.r.bottom) / 2.0;
        for t in items.iter().filter(|t| t.kind == Kind::Text && !t.key.trim().is_empty()) {
            if t.r.top < y - 1.0 && t.r.bottom > y + 1.0 && t.r.left < h.r.right && t.r.right > h.r.left {
                bad.push(format!("text {:?} {} runs over the hairline at {:.1}", t.key, fmt(t.r), y));
            }
        }
    }
    // A row with a note under it: as much room under the note's last line
    // as over the label's first (rows::note_box).
    for n in items.iter().filter(|n| n.kind == Kind::Note) {
        let Some(s) = segs.iter().find(|s| inside(n.r, s.r)) else {
            bad.push(format!("note {:?} {} outside every row", n.key, fmt(n.r)));
            continue;
        };
        let Some(l) = items.iter().find(|l| l.kind == Kind::Label && inside(l.r, s.r)) else { continue };
        let (over, under) = (l.r.top - s.r.top, s.r.bottom - n.r.bottom);
        if (over - under).abs() > 1.0 {
            bad.push(format!("{}: {:.1} over its label, {:.1} under its note", s.key, over, under));
        }
    }
    // The focus ring on an "i" goes round its glyph, centered on it (not
    // round its larger target). Paired by the "i"'s id (both keyed by
    // their hit), never by where the ring is: one pushed off its "i", or
    // mirrored to the wrong side, is caught too.
    for ring in items.iter().filter(|r| r.kind == Kind::Ring && r.key.starts_with("Info(")) {
        let (cx, cy) = ((ring.r.left + ring.r.right) / 2.0, (ring.r.top + ring.r.bottom) / 2.0);
        let Some(i) = items.iter().find(|i| i.kind == Kind::Info && i.key == ring.key) else {
            bad.push(format!("a ring for {} {} with no such i drawn", ring.key, fmt(ring.r)));
            continue;
        };
        let (ix, iy) = ((i.r.left + i.r.right) / 2.0, (i.r.top + i.r.bottom) / 2.0);
        let most = tok::INFO_D / 2.0 + tok::INFO_RING_GAP + tok::RING_W;
        if !near(cx, ix) || !near(cy, iy) || (ring.r.right - cx) > most + 0.01 || (ring.r.bottom - cy) > most + 0.01 {
            bad.push(format!("the ring {} round the i at ({ix:.1},{iy:.1}): wants its center and at most {most} out", fmt(ring.r)));
        }
    }
    let want_h = |k: CtlKind| match k {
        CtlKind::Toggle => Some(tok::SWITCH_H),
        CtlKind::Field | CtlKind::Choice | CtlKind::Button => Some(tok::CONTROL_H),
        CtlKind::Slider | CtlKind::Swatches => None,
    };
    let want_w = |k: CtlKind| match k {
        CtlKind::Toggle => Some(tok::SWITCH_W),
        CtlKind::Field => Some(tok::FIELD_W),
        _ => None,
    };
    for (k, w, h, key) in &sizes {
        if let Some(wh) = want_h(*k) {
            if !near(*h, wh) {
                bad.push(format!("{key}: {k:?} {h:.1} high, wants {wh}"));
            }
        }
        if let Some(ww) = want_w(*k) {
            if !near(*w, ww) {
                bad.push(format!("{key}: {k:?} {w:.1} wide, wants {ww}"));
            }
        }
    }
    bad.into_iter().map(|b| format!("[{page}] {b}")).collect()
}

/// What is wrong with a page's hit areas, in the order pushed (the last
/// one over a point wins): two that overlap must be one strictly inside
/// the other, pushed after it (it wins where it is), and a pair `nests`
/// allows (outer, inner: a "Reset" in its key field, an "i" on a row that
/// opens). The same area twice, an unrelated action inside another, or a
/// partial overlap is a click that lands on the wrong thing, or a control
/// that cannot be reached. Pure: tested.
#[cfg(test)]
pub(super) fn hit_conflicts<H: std::fmt::Debug>(hits: &[(D2D_RECT_F, H)], nests: impl Fn(&H, &H) -> bool) -> Vec<String> {
    let mut bad = Vec::new();
    for (i, (a, ha)) in hits.iter().enumerate() {
        for (b, hb) in hits.iter().skip(i + 1) {
            if !overlap(*a, *b) {
                continue;
            }
            let same = near(a.left, b.left) && near(a.top, b.top) && near(a.right, b.right) && near(a.bottom, b.bottom);
            let why = if same {
                "on the very same area as"
            } else if inside(*a, *b) {
                "under a later one that covers it:"
            } else if !inside(*b, *a) {
                "partly under"
            } else if !nests(ha, hb) {
                "holding an unrelated"
            } else {
                continue;
            };
            bad.push(format!("{ha:?} {} {why} {hb:?} {}", fmt(*a), fmt(*b)));
        }
    }
    bad
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(l: f32, t: f32, rr: f32, b: f32) -> D2D_RECT_F {
        D2D_RECT_F { left: l, top: t, right: rr, bottom: b }
    }

    fn seg(key: &str, t: f32, b: f32, first: bool, last: bool) -> Item {
        Item { kind: Kind::Seg { first, last }, key: key.into(), r: r(0.0, t, 400.0, b) }
    }

    #[test]
    fn the_space_between_blocks_is_one_rule() {
        assert_eq!(gap_before(Block::Start, Block::Card), 0.0);
        assert_eq!(gap_before(Block::Card, Block::Card), tok::GROUP_GAP);
        assert_eq!(gap_before(Block::Card, Block::Header), HEADING_ABOVE);
        assert_eq!(gap_before(Block::Header, Block::Card), 0.0);
    }

    #[test]
    fn a_page_that_follows_the_rules_passes_and_a_tight_card_fails() {
        let hl = |t: f32| Item { kind: Kind::Hairline, key: String::new(), r: r(tok::ROW_PAD, t, 400.0 - tok::ROW_PAD, t + 1.0) };
        let mut items = vec![seg("a", 0.0, 40.0, true, false), hl(40.0), seg("b", 40.0, 80.0, false, true)];
        let y = 80.0 + HEADING_ABOVE;
        items.push(Item { kind: Kind::Header, key: "h".into(), r: r(0.0, y, 400.0, y + tok::HEADER_H) });
        let y = y + tok::HEADER_H;
        items.push(seg("c", y, y + 40.0, true, true));
        let end = y + 40.0 + PAGE_END;
        assert_eq!(check("p", &items, 0.0, end, false), Vec::<String>::new());
        // A card right under another (the reset card under the taskbar's).
        items.push(seg("d", y + 44.0, y + 96.0, true, true));
        let bad = check("p", &items, 0.0, y + 96.0 + PAGE_END, false);
        assert!(bad.iter().any(|b| b.contains("d after Card")), "{bad:?}");
        // Loose text between cards.
        let mut items = vec![seg("a", 0.0, 40.0, true, true)];
        items.push(Item { kind: Kind::Text, key: "hint".into(), r: r(0.0, 44.0, 200.0, 60.0) });
        let bad = check("p", &items, 0.0, 40.0 + PAGE_END, false);
        assert!(bad.iter().any(|b| b.contains("outside every card")), "{bad:?}");
    }

    /// The checks catch what they are for (each failure put in on
    /// purpose, settings-info review P2).
    #[test]
    fn the_checks_catch_overlaps_strays_and_cuts() {
        let right = 400.0 - tok::ROW_PAD;
        let card = || vec![seg("a", 0.0, 56.0, true, true)];
        let btn = |l: f32, rr: f32| Item { kind: Kind::Button, key: "b".into(), r: r(l, 16.0, rr, 16.0 + tok::CONTROL_H) };
        let ok = |items: &[Item]| check("p", items, 0.0, 56.0 + PAGE_END, false);
        // Two buttons at the row's end: fine.
        let mut items = card();
        items.extend([btn(right - 100.0, right - 60.0), btn(right - 50.0, right)]);
        assert_eq!(ok(&items), Vec::<String>::new());
        // Rename and Remove overlapping.
        let mut items = card();
        items.extend([btn(right - 100.0, right - 40.0), btn(right - 50.0, right)]);
        assert!(ok(&items).iter().any(|b| b.contains("overlaps")), "{:?}", ok(&items));
        // Both moved off the end (and not at the start either).
        let mut items = card();
        items.extend([btn(right - 140.0, right - 100.0), btn(right - 90.0, right - 40.0)]);
        assert!(ok(&items).iter().any(|b| b.contains("hug neither edge")), "{:?}", ok(&items));
        // A list's "Add…" at the start: fine.
        let mut items = card();
        items.push(btn(tok::ROW_PAD, tok::ROW_PAD + 80.0));
        assert_eq!(ok(&items), Vec::<String>::new());
        // An "i" over a control, an "i" outside every card.
        let mut items = card();
        items.push(btn(right - 50.0, right));
        items.push(Item { kind: Kind::Info, key: String::new(), r: r(right - 30.0, 16.0, right - 6.0, 40.0) });
        assert!(ok(&items).iter().any(|b| b.contains("Info") && b.contains("overlaps")), "{:?}", ok(&items));
        let mut items = card();
        items.push(Item { kind: Kind::Info, key: String::new(), r: r(10.0, 60.0, 34.0, 84.0) });
        assert!(ok(&items).iter().any(|b| b.contains("outside every card")), "{:?}", ok(&items));
        // A cut text, an empty button.
        let mut items = card();
        items.push(Item { kind: Kind::Cut, key: "Ein sehr langer Name".into(), r: r(20.0, 10.0, 80.0, 30.0) });
        items.push(Item { kind: Kind::Button, key: "b".into(), r: r(right, 16.0, right, 40.0) });
        let bad = ok(&items);
        assert!(bad.iter().any(|b| b.contains("cut at")) && bad.iter().any(|b| b.contains("has no size")), "{bad:?}");
        // A short hairline in the middle of the row.
        let mut items = vec![seg("a", 0.0, 40.0, true, false), seg("c", 40.0, 80.0, false, true)];
        items.push(Item { kind: Kind::Hairline, key: String::new(), r: r(150.0, 40.0, 250.0, 41.0) });
        let bad = check("p", &items, 0.0, 80.0 + PAGE_END, false);
        assert!(bad.iter().any(|b| b.contains("no hairline")), "{bad:?}");
    }

    #[test]
    fn hits_overlap_only_nested_with_the_inner_one_on_top() {
        let a = r(0.0, 0.0, 100.0, 40.0);
        let inner = r(70.0, 8.0, 94.0, 32.0);
        let nests = |o: &&str, i: &&str| (*o, *i) == ("field", "reset");
        assert!(hit_conflicts(&[(a, "field"), (inner, "reset")], nests).is_empty(), "a Reset in its field: nested, the inner one wins");
        assert!(!hit_conflicts(&[(inner, "reset"), (a, "field")], nests).is_empty(), "the inner one could never be clicked");
        assert!(!hit_conflicts(&[(a, "x"), (r(90.0, 0.0, 140.0, 40.0), "y")], nests).is_empty(), "partly over another");
        assert!(hit_conflicts(&[(a, "x"), (r(100.0, 0.0, 140.0, 40.0), "y")], nests).is_empty(), "side by side");
        // Two actions on the very same area (review P3): the first can never
        // be clicked, whatever they are.
        let bad = hit_conflicts(&[(r(0.0, 0.0, 30.0, 30.0), "field"), (r(0.0, 0.0, 30.0, 30.0), "reset")], nests);
        assert!(bad.iter().any(|b| b.contains("very same area")), "{bad:?}");
        // An unrelated action inside another.
        let bad = hit_conflicts(&[(a, "row"), (inner, "delete")], nests);
        assert!(bad.iter().any(|b| b.contains("unrelated")), "{bad:?}");
    }

    /// What the owner saw on 0.1.46 (Shortcuts, "Dock"): each put in on
    /// purpose, each caught.
    #[test]
    fn the_checks_catch_what_the_pixels_showed() {
        let right = 400.0 - tok::ROW_PAD;
        let field = |cy: f32| r(right - tok::FIELD_W, cy - tok::CONTROL_H / 2.0, right, cy + tok::CONTROL_H / 2.0);
        let ctl = |cy: f32| Item { kind: Kind::Ctl { kind: CtlKind::Field, cy }, key: "a".into(), r: field(cy) };
        let text = |key: &str, l: f32, t: f32, w: f32| Item { kind: Kind::Text, key: key.into(), r: r(l, t, l + w, t + 16.0) };
        let fc = right - tok::FIELD_W / 2.0;
        let ok = |items: &[Item], end: f32| check("p", items, 0.0, end + PAGE_END, false);
        // A value centered in its field; one pushed by its Reset; one at
        // the field's start (a list button's old left-aligned value).
        let mut items = vec![seg("a", 0.0, 40.0, true, true), ctl(20.0), text("Alt+Space", fc - 30.0, 12.0, 60.0)];
        assert_eq!(ok(&items, 40.0), Vec::<String>::new());
        items[2] = text("A long value", right - super::super::rows::RESET_W - 140.0, 12.0, 140.0);
        assert_eq!(ok(&items, 40.0), Vec::<String>::new(), "pushed off center by its Reset");
        items[2] = text("Off", right - tok::FIELD_W + 12.0, 12.0, 20.0);
        assert!(ok(&items, 40.0).iter().any(|b| b.contains("value")), "{:?}", ok(&items, 40.0));
        // A mark's name over the next row's hairline.
        let hl = Item { kind: Kind::Hairline, key: String::new(), r: r(tok::ROW_PAD, 40.0, right, 41.0) };
        let items = vec![seg("a", 0.0, 40.0, true, false), hl, seg("b", 40.0, 80.0, false, true), text("Small", 100.0, 33.0, 30.0)];
        assert!(ok(&items, 80.0).iter().any(|b| b.contains("hairline")), "{:?}", ok(&items, 80.0));
        // A note with 16 under it, the label 11 under the row's top.
        let label = Item { kind: Kind::Label, key: "a".into(), r: r(tok::ROW_PAD, 11.0, 120.0, 29.0) };
        let note = |b: f32| Item { kind: Kind::Note, key: "In use".into(), r: r(tok::ROW_PAD, 36.0, right, b) };
        let items = vec![seg("a", 0.0, 68.0, true, true), label.clone(), note(52.0)];
        assert!(ok(&items, 68.0).iter().any(|b| b.contains("under its note")), "{:?}", ok(&items, 68.0));
        let items = vec![seg("a", 0.0, 63.0, true, true), label, note(52.0)];
        assert_eq!(ok(&items, 63.0), Vec::<String>::new());
        // An "i"'s ring round its 24 DIP target, not its 12 DIP glyph.
        let info = Item { kind: Kind::Info, key: "Info(3)".into(), r: r(88.0, 8.0, 112.0, 32.0) };
        let ring_at = |x: f32, o: f32| Item { kind: Kind::Ring, key: "Info(3)".into(), r: r(x - o, 20.0 - o, x + o, 20.0 + o) };
        let ring = |o: f32| ring_at(100.0, o);
        let items = vec![seg("a", 0.0, 40.0, true, true), info.clone(), ring(15.0)];
        assert!(ok(&items, 40.0).iter().any(|b| b.contains("ring")), "{:?}", ok(&items, 40.0));
        let items = vec![seg("a", 0.0, 40.0, true, true), info.clone(), ring(9.0)];
        assert_eq!(ok(&items, 40.0), Vec::<String>::new());
        // The right size, pushed 13 DIP off (out of its 24 DIP target), or
        // mirrored to the row's other side: still its "i"'s, still caught.
        for x in [113.0, 300.0] {
            let items = vec![seg("a", 0.0, 40.0, true, true), info.clone(), ring_at(x, 9.0)];
            assert!(ok(&items, 40.0).iter().any(|b| b.contains("ring")), "{x}: {:?}", ok(&items, 40.0));
        }
        // A ring for an "i" that is not drawn.
        let items = vec![seg("a", 0.0, 40.0, true, true), Item { key: "Info(4)".into(), ..info }, ring(9.0)];
        assert!(ok(&items, 40.0).iter().any(|b| b.contains("no such i")), "{:?}", ok(&items, 40.0));
        // A note with a link under it: its box runs to the link's end.
        let label = Item { kind: Kind::Label, key: "s".into(), r: r(tok::ROW_PAD, 11.0, 120.0, 29.0) };
        let linked = Item { kind: Kind::Note, key: "Not on".into(), r: r(tok::ROW_PAD, 36.0, right, 52.0 + super::super::rows::NOTE_LINK_H) };
        let items = vec![seg("s", 0.0, 63.0 + super::super::rows::NOTE_LINK_H, true, true), label, linked];
        assert_eq!(ok(&items, 63.0 + super::super::rows::NOTE_LINK_H), Vec::<String>::new());
    }

    #[test]
    fn a_control_off_the_right_edge_or_off_center_fails() {
        let mut items = vec![seg("a", 0.0, 40.0, true, true)];
        let right = 400.0 - tok::ROW_PAD;
        items.push(Item { kind: Kind::Ctl { kind: CtlKind::Field, cy: 20.0 }, key: "a".into(), r: r(right - tok::FIELD_W - 38.0, 6.0, right - 38.0, 6.0 + tok::CONTROL_H) });
        let bad = check("p", &items, 0.0, 40.0 + PAGE_END, false);
        assert!(bad.iter().any(|b| b.contains("ends at")), "{bad:?}");
    }
}
