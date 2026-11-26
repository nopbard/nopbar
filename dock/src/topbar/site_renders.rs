//! Repeatable 2x stills of the app's UI over a supplied desktop wallpaper.

use std::path::{Path, PathBuf};

use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Direct2D::D2D1_INTERPOLATION_MODE_NEAREST_NEIGHBOR;
use windows_numerics::Matrix3x2;

use super::{bar_harmony, control, island, Cell};
use crate::desklayer::{self, backdrop, graphite_snapshots as fixtures};
use crate::gfx::promo_kit::{self as kit, Canvas, GlassWallpaper, Stage};
use crate::icons::Icon;
use crate::wallpaper::{self, MicaPicture};

const SCALE: f32 = 2.0;
const WIDTH: u32 = 3840;
const HEIGHT: u32 = 2160;
const DISPLAY: RECT = RECT { left: 0, top: 0, right: WIDTH as i32, bottom: HEIGHT as i32 };

/// Paint full-screen first so every surface samples the wallpaper at the
/// correct desktop position. The detail shots only change the framing.
fn canvas(stage: &Stage, wall: &Icon) -> Canvas {
    let cv = Canvas::of(stage.gfx.d2d_factory().clone(), wall.width as f32 / SCALE, wall.height as f32 / SCALE, SCALE);
    unsafe {
        cv.dc.SetTransform(&Matrix3x2::identity());
        let bitmap = fixtures::bitmap(&cv.dc, wall);
        cv.dc.DrawBitmap(&bitmap, None, 1.0, D2D1_INTERPOLATION_MODE_NEAREST_NEIGHBOR, None, None);
    }
    cv
}

fn around(rect: RECT, x: i32, y: i32) -> RECT {
    RECT {
        left: (rect.left - x).max(0),
        top: (rect.top - y).max(0),
        right: (rect.right + x).min(DISPLAY.right),
        bottom: (rect.bottom + y).min(DISPLAY.bottom),
    }
}

fn save(img: &Icon, rect: RECT, out: &Path, name: &str, look: &str) {
    let path = out.join(format!("{name}-{look}.png"));
    if rect == DISPLAY {
        kit::save_png(&img.pixels, img.width, img.height, &path);
    } else {
        let crop = crop(img, rect);
        kit::save_png(&crop.pixels, crop.width, crop.height, &path);
    }
}

fn crop(img: &Icon, rect: RECT) -> Icon {
    assert!(rect.left >= 0 && rect.top >= 0 && rect.right <= img.width as i32 && rect.bottom <= img.height as i32);
    let w = ((rect.right - rect.left) as usize) * 4;
    let h = (rect.bottom - rect.top) as usize;
    let stride = img.width as usize * 4;
    let mut pixels = Vec::with_capacity(w * h);
    for y in rect.top..rect.bottom {
        let start = y as usize * stride + rect.left as usize * 4;
        pixels.extend_from_slice(&img.pixels[start..start + w]);
    }
    Icon { width: (w / 4) as u32, height: h as u32, pixels }
}

fn finish(cv: Canvas) -> Icon {
    let (pixels, width, height) = cv.finish();
    Icon { width, height, pixels }
}

fn render(stage: &Stage, wallpaper: &Icon, mica: &MicaPicture, out: &Path, look: &str) {
    let theme = bar_harmony::look_theme("glass", look == "light");
    let glass = GlassWallpaper { blurred: &mica.blurred, size: (WIDTH as f32 / SCALE, HEIGHT as f32 / SCALE) };
    let save_scene = |img: &Icon, rect, name| save(img, rect, out, name, look);

    let cv = canvas(stage, wallpaper);
    let bar = bar_harmony::paint_site_bar(stage, &cv, &theme, mica, SCALE, None);
    island::promo::site_pill(stage, &cv, &theme, SCALE, &bar.spans, bar_harmony::bar_h());
    let widgets = desklayer::paint_site_widgets(&cv, &theme, SCALE);
    crate::dock::paint_site_dock(&cv, stage, &theme, &glass, SCALE, false);
    let desktop = finish(cv);
    save_scene(&desktop, DISPLAY, "desktop");
    save_scene(&desktop, around(widgets, 72, 54), "widgets");
    drop(desktop);

    render_island(stage, wallpaper, &theme, |img, frame| save_scene(img, frame, "topbar-island"));

    let cv = canvas(stage, wallpaper);
    let bar = bar_harmony::paint_site_bar(stage, &cv, &theme, mica, SCALE, Some(Cell::Control));
    let panel = control::paint_site_control(stage, &cv, &theme, &glass, bar.control, SCALE);
    save_scene(&finish(cv), around(panel, 128, 100), "quick-settings");

    let cv = canvas(stage, wallpaper);
    let dock = crate::dock::paint_site_dock(&cv, stage, &theme, &glass, SCALE, true);
    save_scene(&finish(cv), dock, "dock");

    let cv = canvas(stage, wallpaper);
    let bar = bar_harmony::paint_site_bar(stage, &cv, &theme, mica, SCALE, Some(Cell::Companion));
    let briefing = crate::companion::paint_site_news(stage, &cv, &theme, bar.companion);
    save_scene(&finish(cv), around(briefing, 160, 144), "gnome");
}

fn render_island(stage: &Stage, wallpaper: &Icon, theme: &crate::theme::Theme, save: impl FnOnce(&Icon, RECT)) {
    // The same display scale on a narrower desktop keeps the expanded
    // island and the status cells together in the detail image.
    let wall = crop(wallpaper, RECT { left: WIDTH as i32 - 2304, ..DISPLAY });
    let mica = fixtures::mica_with_radius(&wall, wallpaper::mica_radius(SCALE));
    let glass = GlassWallpaper { blurred: &mica.blurred, size: (wall.width as f32 / SCALE, wall.height as f32 / SCALE) };
    let cv = canvas(stage, &wall);
    let bar = bar_harmony::paint_site_bar(stage, &cv, theme, &mica, SCALE, None);
    let pill = island::promo::site_pill(stage, &cv, theme, SCALE, &bar.spans, bar_harmony::bar_h());
    let expanded = island::paint_site_island(stage, &cv, theme, &glass, pill, SCALE);
    let frame = RECT { left: expanded.left - 144, top: 0, right: wall.width as i32, bottom: expanded.bottom + 192 };
    save(&finish(cv), frame);
}

#[test]
#[ignore = "renders English website stills with environment-provided wallpapers"]
fn render_site_scenes() {
    let (Some(light), Some(dark)) = (
        std::env::var_os("NOPBAR_SITE_WALLPAPER_LIGHT"),
        std::env::var_os("NOPBAR_SITE_WALLPAPER_DARK"),
    ) else {
        println!("Set NOPBAR_SITE_WALLPAPER_LIGHT and NOPBAR_SITE_WALLPAPER_DARK to render the site stills.");
        return;
    };
    let out = PathBuf::from(std::env::var_os("NOPBAR_SITE_RENDER_DIR").expect("site render output directory"));
    let _lang = crate::i18n::test_lock("en");
    let stage = kit::stage();
    for (look, path) in [("light", light), ("dark", dark)] {
        let wallpaper = crate::icons::decode_image(Path::new(&path)).expect("read the supplied wallpaper");
        assert_eq!((wallpaper.width, wallpaper.height), (WIDTH, HEIGHT));
        let mica = fixtures::mica_with_radius(&wallpaper, wallpaper::mica_radius(SCALE));
        backdrop::set_test(backdrop::grey_of_mica(DISPLAY, &mica));
        render(&stage, &wallpaper, &mica, &out, look);
        backdrop::set_test(None);
    }
    println!("Site PNGs written to {}", out.display());
}
