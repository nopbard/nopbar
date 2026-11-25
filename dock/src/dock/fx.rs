//! The dock's end of crate::effects: resolves the config into the effects
//! in force (publish) and re-attaches what changed. Nothing is lowered on
//! its own: the effects are what the user set, or the lightest with
//! Low-spec mode on.

use std::cell::{Cell, RefCell};

use super::*;
use crate::effects::{self, Env, Resolved};

thread_local! {
    /// The config and system the effects were last resolved from.
    static LAST: RefCell<Option<(Config, Env)>> = const { RefCell::new(None) };
    /// The display's frame time (ms), read once.
    static REFRESH_MS: Cell<f32> = const { Cell::new(0.0) };
}

/// Ticker frames per dock tick (the frame cap): 1 = every display frame.
static TICK_DIV: AtomicU32 = AtomicU32::new(1);

pub(super) fn tick_div() -> u32 {
    TICK_DIV.load(Ordering::SeqCst).max(1)
}

fn env_of(sys: &crate::syslook::SysLook) -> Env {
    Env { animations: sys.animations, high_contrast: sys.high_contrast.is_some() }
}

/// The display's frame time in ms (DWM's refresh rate; 60 Hz if unknown).
fn refresh_ms() -> f32 {
    let cached = REFRESH_MS.get();
    if cached > 0.0 {
        return cached;
    }
    use windows::Win32::Graphics::Dwm::{DwmGetCompositionTimingInfo, DWM_TIMING_INFO};
    let mut info = DWM_TIMING_INFO { cbSize: std::mem::size_of::<DWM_TIMING_INFO>() as u32, ..Default::default() };
    let ms = unsafe { DwmGetCompositionTimingInfo(HWND::default(), &mut info) }
        .ok()
        .filter(|_| info.rateRefresh.uiNumerator > 0 && info.rateRefresh.uiDenominator > 0)
        .map(|_| 1000.0 * info.rateRefresh.uiDenominator as f32 / info.rateRefresh.uiNumerator as f32)
        .filter(|ms| (2.0..=100.0).contains(ms))
        .unwrap_or(1000.0 / 60.0);
    REFRESH_MS.set(ms);
    ms
}

/// Pure: display frames per dock tick for a cap (0 = none) at
/// `refresh_ms`: the smallest whole divider that keeps the ticks at or
/// under the cap (144 Hz capped at 60: every 3rd frame, 48 fps). A rate a
/// hair over a multiple (120.0001 Hz, 60.06 Hz) counts as that multiple.
pub(super) fn div_for(cap: u32, refresh_ms: f32) -> u32 {
    if cap == 0 || refresh_ms <= 0.0 {
        return 1;
    }
    let hz = 1000.0 / refresh_ms;
    ((hz / cap as f32 - 0.02).ceil() as u32).max(1)
}

/// The display (its refresh rate) may have changed: read it again (the
/// frame cap's divider follows).
pub(super) fn display_changed() {
    REFRESH_MS.set(0.0);
    republish();
}

/// Resolves `cfg` in `sys` and puts it in force.
pub(super) fn publish(cfg: &Config, sys: &crate::syslook::SysLook) {
    let env = env_of(sys);
    LAST.with(|l| *l.borrow_mut() = Some((cfg.clone(), env)));
    republish();
}

/// Resolves again from what publish() was last given.
fn republish() {
    let Some((cfg, env)) = LAST.with(|l| l.borrow().clone()) else { return };
    let r = effects::resolve(&cfg, env);
    TICK_DIV.store(div_for(r.items.frame_cap, refresh_ms()), Ordering::SeqCst);
    effects::set(r);
}

/// What was drawn with the look before closes: menus, stacks and the dock's
/// previews (they keep the Theme they were made with); reopened, they draw
/// in the new one. The panels that follow a new look themselves (Control
/// Center, search, Launchpad) are rethemed by
/// topbar::apply_config. One place for every look change: Windows' look
/// (transparency, high contrast, light / dark), the effects (Glass <->
/// Flat), the renderer (gpu.rs closes everything anyway).
pub(super) fn close_drawn_with_old_look() {
    crate::popup::cancel();
    stack::cancel();
    preview::hide();
}

/// Pure: whether a look change has to close what was drawn with the old
/// look (close_drawn_with_old_look): the primary dock does it, once, when
/// the resolved theme really changed.
pub(super) fn closes_old_look(primary: bool, before: &Theme, now: &Theme) -> bool {
    primary && before != now
}

impl Dock {
    /// The look in force may have changed (the blur effect, the renderer):
    /// the theme is resolved again from the raw one; if it changed, what was
    /// drawn with the old look closes and every surface follows: the dock
    /// and its plates, then (primary) the top bars and their panels. The
    /// caller re-attaches the material. Returns whether it changed.
    pub(super) fn retheme(&mut self) -> bool {
        let theme = self.theme_raw.resolve(&self.sys).windows_shell();
        if theme == self.theme {
            return false;
        }
        let close = closes_old_look(self.primary, &self.theme, &theme);
        self.theme = theme;
        if close {
            close_drawn_with_old_look();
        }
        self.apply_theme();
        self.rebuild_items();
        if self.primary {
            crate::topbar::apply_config(&self.gfx, &self.cfg, &self.theme);
        }
        true
    }

    /// The config or the system look changed: re-attach what the effects
    /// changed (`before`: what was in force).
    pub(super) fn effects_reapply(&mut self, before: Resolved) {
        let now = effects::current();
        if now == before {
            return;
        }
        if now.items.blur != before.items.blur {
            // Glass needs the blur: Flat without it, Glass again with it,
            // everywhere (Flat itself does not change with it). The
            // material follows below.
            self.retheme();
        }
        if now.items.blur != before.items.blur || now.items.shadows != before.items.shadows {
            // (Asks for the mica image itself; a cached one comes back.)
            self.apply_material();
            self.layout();
        }
        // (`small`: the one gate of what moves, crate::motion::now.)
        if now.motion != before.motion || now.speed != before.speed || now.items.label_fade != before.items.label_fade || now.items.small != before.items.small {
            let c = self.gfx.compositor.clone();
            let _ = slide_anim(&c, &self.slide, SLIDE_MS);
            let _ = slide_anim(&c, self.shadow.slide(), SLIDE_MS);
            self.style_label();
            let _ = theme_fade(&c, &self.mica);
            if let Some(b) = self.bar_fill.first() {
                let _ = theme_fade(&c, b);
            }
            // The running dots' resize rides the same gate.
            for it in &self.items {
                let _ = implicit_on(&c, &it.dot, &[("Size", SNAP)]);
                if let Some(g) = &it.dot_geom {
                    let _ = size_anim(&c, g);
                }
            }
        }
    }

    /// The effects in force may have changed outside a config reload (the
    /// primary's, for a view): put them in force,
    /// the dock's look too.
    pub(super) fn effects_changed(&mut self, before: Resolved) {
        let now = effects::current();
        self.fx_seen = now;
        if now == before {
            return;
        }
        if self.primary {
            // The other displays' docks follow.
            super::views::schedule_sync();
        }
        let look = Look::from_cfg(&self.cfg.dock, now.items.magnify);
        if look != self.look {
            self.look = look;
            unsafe { self.place_window() };
            self.rebuild_items();
        }
        self.effects_reapply(before);
    }

    /// Windows' look changed (animations, high contrast).
    pub(super) fn sys_for_effects(&mut self, sys: &crate::syslook::SysLook) {
        publish(&self.cfg, sys);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_look_closes_what_the_old_one_drew_once() {
        use crate::syslook::SysLook;
        let mut glass = Theme::graphite();
        glass.finish = crate::theme::Finish::Glass;
        let flat = Theme::graphite();
        for light in [false, true] {
            let sys = SysLook { light, ..Default::default() };
            // Low-spec mode / the switch take the blur: Glass turns
            // Flat, so menus, stacks and previews close (the primary dock
            // closes them; the other displays' docks only retheme).
            let (on, off) = (glass.resolve_fx(&sys, true), glass.resolve_fx(&sys, false));
            assert!(closes_old_look(true, &on, &off) && closes_old_look(true, &off, &on));
            assert!(!closes_old_look(false, &on, &off));
            // Flat with the blur: only its grain goes (Flat v2: a Flat the
            // PC forces is lighter), a new look all the same.
            assert!(closes_old_look(true, &flat.resolve_fx(&sys, true), &flat.resolve_fx(&sys, false)));
            // Windows' transparency off: Glass turns Flat (a new look);
            // the Flat one chose loses its grain (Flat v2: transparency
            // off forces a Flat), a new look too: what was drawn with the
            // grain closes and every surface is drawn again.
            let solid = SysLook { light, transparency: false, ..Default::default() };
            assert!(closes_old_look(true, &glass.resolve_fx(&sys, true), &glass.resolve_fx(&solid, true)));
            let (on, off) = (flat.resolve_fx(&sys, true), flat.resolve_fx(&solid, true));
            assert!(on.flat_grain && !off.flat_grain);
            assert!(closes_old_look(true, &on, &off) && closes_old_look(true, &off, &on));
            // High contrast: a new look, either finish.
            let (white, black) = (crate::theme::Rgba { r: 255, g: 255, b: 255, a: 255 }, crate::theme::Rgba { r: 0, g: 0, b: 0, a: 255 });
            let hc = SysLook { light, high_contrast: Some(crate::syslook::HighContrast { window: black, text: white, highlight: white, highlight_text: black, gray_text: crate::syslook::GRAY_TEXT }), ..Default::default() };
            for raw in [&glass, &flat] {
                assert!(closes_old_look(true, &raw.resolve_fx(&sys, true), &raw.resolve_fx(&hc, true)));
            }
        }
    }

    #[test]
    fn frame_caps_divide_the_display_rate() {
        assert_eq!(div_for(0, 1000.0 / 60.0), 1);
        assert_eq!(div_for(60, 1000.0 / 60.0), 1);
        assert_eq!(div_for(30, 1000.0 / 60.0), 2);
        assert_eq!(div_for(60, 1000.0 / 120.0), 2);
        assert_eq!(div_for(60, 1000.0 / 120.0001), 2);
        assert_eq!(div_for(60, 1000.0 / 59.94), 1);
        assert_eq!(div_for(30, 1000.0 / 144.0), 5);
        assert_eq!(div_for(60, 1000.0 / 30.0), 1, "never faster than the display");
        // Never over the cap: 144 Hz / 60 is every 3rd frame, 75 Hz every 2nd.
        for hz in [60.0f32, 75.0, 90.0, 100.0, 120.0, 144.0, 165.0, 240.0] {
            for cap in [30u32, 60] {
                let d = div_for(cap, 1000.0 / hz);
                assert!(hz / d as f32 <= cap as f32 + 0.5, "{hz} Hz cap {cap}: div {d}");
            }
        }
        assert_eq!(div_for(60, 1000.0 / 144.0), 3);
        assert_eq!(div_for(60, 1000.0 / 75.0), 2);
    }
}
