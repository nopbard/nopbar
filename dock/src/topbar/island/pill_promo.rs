//! The Live Island's pill drawn off screen for the promo film (tests only;
//! see crate::gfx::promo_kit): the pill for a song playing with one more
//! activity behind it, its parts where model::frame puts them (the lead
//! glyph, the title on the pill's line, the dot), in pill_colors' fill and
//! ink. One pill for each of the film's bars, placed between that bar's
//! cells as model::place places it. The music bars move on screen (a
//! compositor animation): they are left out of the picture, and the film
//! draws them where `island-pill.json` says, in the same color. The shadows
//! (E1's two layers) and the contact ring are the compositor's too: the
//! film draws them from the same file.

use windows::Win32::Graphics::Direct2D::{D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_ELLIPSE};
use windows::Win32::Graphics::DirectWrite::DWRITE_FONT_WEIGHT_SEMI_BOLD;
use windows_numerics::Vector2;

use super::*;
use crate::gfx::promo_kit::{self as kit, col, css, rect};

#[test]
#[ignore = "writes the promo film's pictures: cargo test -p dock promo_ -- --ignored"]
fn promo_island_pill() {
    kit::each_lang(pill_pictures);
}

fn pill_pictures() {
    let stage = kit::stage();
    let live = card::promo::live();
    let bar_h = crate::config::Config::default().topbar.height.clamp(20.0, 48.0);
    let h = model::pill_height(bar_h);
    let mut looks = serde_json::Map::new();
    let mut pills = serde_json::Map::new();
    for (folder, light) in kit::LOOKS {
        let theme = kit::theme(light);
        let (fill, ink, _dim, border) = pill_colors(&theme);
        let (words, _) = model::pill_words(&live[0]);
        let dots: Vec<Tone> = live.iter().skip(1).take(model::MAX_DOTS).map(model::shown_tone).collect();
        let eq_w = EQ_BARS as f32 * EQ_W + (EQ_BARS - 1) as f32 * EQ_GAP;
        // TopBar::island_draw: the title takes what the widest pill leaves.
        let others = Content { lead: LEAD, trail: eq_w, dots: dots.len(), ..Default::default() };
        let title = |max: f32| stage.gfx.text_layout_line(&words, &theme.font.family, TEXT_SIZE, DWRITE_FONT_WEIGHT_SEMI_BOLD, max).expect("the title");
        // One pill for each of the film's bars: where and how wide
        // TopBar::island_layout puts it between that bar's cells (in the
        // bar's middle when there is room; a narrow bar pushes it aside,
        // and cuts its title when even that is too little).
        for (name, bar_w) in crate::topbar::promo::STAGES {
            let (mut layout, mut tw, mut th) = title(model::text_room(&others, model::PILL_MAX_W));
            let mut c = Content { lead: LEAD, text: tw, text_h: th, trail: eq_w, trail_h: EQ_H, dots: dots.len(), baseline: None };
            let want = model::pill_width(&c);
            let cells = crate::topbar::promo::cell_spans(&stage, bar_w);
            let (x, w) = model::place(bar_w, &cells, want, model::pill_need(&c)).expect("room for the pill on the film's bar");
            if w + 0.5 < want {
                let max = model::text_room(&c, w);
                assert!(max > 0.0, "the {name} bar leaves no room for the title");
                (layout, tw, th) = title(max);
                (c.text, c.text_h) = (tw, th);
            }
            c.baseline = Some(crate::gfx::first_baseline(&layout, th));
            let f = model::frame(false, w, h, &c, kit::scale());
            // model::frame's x 0 is the pill's center.
            let ox = w / 2.0;
            let cv = stage.canvas(w, h);
            cv.fill_rr(rect(0.0, 0.0, w, h), h / 2.0, fill);
            if border.a > 0 {
                cv.stroke_rr(rect(f.edge.x, f.edge.y, f.edge.x + f.edge.w, f.edge.y + f.edge.h), (h - 1.0) / 2.0, border, 1.0);
            }
            unsafe {
                // TopBar::island_glyph: the glyph fitted in the lead's square.
                let s = kit::scale();
                let side = (LEAD * s).round();
                crate::glyphs::draw_device(&cv.dc, G_MUSIC, &crate::glyphs::fitted(G_MUSIC, (ox + f.lead.x) * s, f.lead.y * s, side, 14.0 * s), col(ink)).unwrap();
                let b = cv.dc.CreateSolidColorBrush(&col(ink), None).unwrap();
                cv.dc.DrawTextLayout(Vector2 { X: ox + f.text.x, Y: f.text.y }, &layout, &b, D2D1_DRAW_TEXT_OPTIONS_NONE);
                for (d, tone) in f.dots.iter().zip(&dots) {
                    let b = cv.dc.CreateSolidColorBrush(&col(tone_rgba(*tone, &theme)), None).unwrap();
                    cv.dc.FillEllipse(&D2D1_ELLIPSE { point: Vector2 { X: ox + d.x + d.w / 2.0, Y: d.y + d.h / 2.0 }, radiusX: d.w / 2.0, radiusY: d.h / 2.0 }, &b);
                }
            }
            cv.save(&format!("{folder}/island-pill-{name}.png"));
            pills.insert(
                name.into(),
                serde_json::json!({
                    // Its left edge on the bar, its size.
                    "x": x,
                    "w": w,
                    "h": h,
                    "inset": model::PILL_INSET,
                    // The music bars: their box (they stand on its bottom),
                    // each bar's width and the gap between them.
                    "eq": { "x": ox + f.trail.x, "y": f.trail.y, "w": f.trail.w, "h": f.trail.h, "bars": EQ_BARS, "barW": EQ_W, "gap": EQ_GAP },
                }),
            );
        }
        looks.insert(folder.into(), serde_json::json!({ "eq": css(eq_color(&theme)), "shadows": kit::shadows_json(&pill_shadows(&theme, true)), "contact": css(theme.contact()) }));
    }
    kit::save_json("island-pill.json", &serde_json::json!({ "scale": kit::scale(), "pills": pills, "looks": looks }));
    assert!(kit::out_dir().join("dark/island-pill-wide.png").is_file() && kit::out_dir().join("light/island-pill-tall.png").is_file());
}
