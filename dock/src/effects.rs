//! Every visual effect and animation NopBar has, in one list (the effects
//! registry), the presets over them, and the effects in force right now.
//!
//! RULE: a new animation or visual effect gets an entry in `ALL` (its
//! settings row, its "On / Off" line, its cost and its demo in the preview
//! strip) and asks `on()` / `current()` before it runs, so the Effects page
//! and the presets reach it.
//!
//! Like a game's graphics settings: a preset (Low, Medium, High, Ultra)
//! sets every item; changing one item afterwards makes it Custom. Nothing
//! is lowered behind the user's back: the items are used as set, unless
//! Low-spec mode is on (Settings > Appearance), which puts everything at
//! Low. Windows' "Animation effects" off stops what moves, and high
//! contrast always turns blur and shadows off.
//!
//! The settings window reads the registry; the dock resolves the config
//! (`resolve`) and publishes the result (`set`), which every animation
//! reads (`on`, `motion`, `speed`, ...).

use std::sync::RwLock;

use crate::config::{Config, Minimize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Level {
    Low,
    Medium,
    High,
    Ultra,
}

/// The presets by name (config text), with their text keys; "custom" is
/// handled apart.
pub const LEVELS: [(Level, &str, &str); 4] = [
    (Level::Low, "low", "effects.preset.low"),
    (Level::Medium, "medium", "effects.preset.medium"),
    (Level::High, "high", "effects.preset.high"),
    (Level::Ultra, "ultra", "effects.preset.ultra"),
];

/// The presets, in order (config text, text key). Older files' "auto"
/// (and anything unknown) reads as High, the items Auto stored.
pub const PRESETS: [(&str, &str); 5] = [
    ("low", "effects.preset.low"),
    ("medium", "effects.preset.medium"),
    ("high", "effects.preset.high"),
    ("ultra", "effects.preset.ultra"),
    ("custom", "effects.preset.custom"),
];

impl Level {
    pub fn from_name(n: &str) -> Option<Level> {
        LEVELS.iter().find(|(_, x, _)| x.eq_ignore_ascii_case(n.trim())).map(|(l, _, _)| *l)
    }
}

pub struct Effect {
    /// The settings row (and config) key.
    pub key: &'static str,
    /// An animation: off when Windows' animation effects are off.
    pub motion: bool,
    /// Forced off by high contrast.
    pub hc_off: bool,
}

const fn fx(key: &'static str, motion: bool, hc_off: bool) -> Effect {
    Effect { key, motion, hc_off }
}

pub static ALL: &[Effect] = &[
    fx("effects.magnify", true, false),
    fx("effects.spring", true, false),
    fx("animation.bounce", true, false),
    fx("effects.label_fade", true, false),
    fx("animation.minimize", true, false),
    fx("effects.blur", false, true),
    fx("effects.shadows", false, true),
    fx("effects.small_animations", true, false),
    fx("effects.launchpad_motion", true, false),
    fx("effects.live_previews", false, false),
    fx("effects.frame_cap", false, false),
    fx("animation.speed", false, false),
];

pub fn find(key: &str) -> Option<&'static Effect> {
    ALL.iter().find(|e| e.key == key)
}

/// Every item a preset sets.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Items {
    pub magnify: bool,
    pub spring: bool,
    pub bounce: bool,
    pub label_fade: bool,
    pub minimize: Minimize,
    pub blur: bool,
    pub shadows: bool,
    pub launchpad_motion: bool,
    pub live_previews: bool,
    /// The small animations: menus and stacks fade in, window previews
    /// pop, the Split View hint fades in, the Live Island springs open
    /// (and its music bars move).
    pub small: bool,
    pub frame_cap: u32,
    /// Internal, not a setting: window previews pop open (with `small`).
    /// Off in Medium and Low, as before the small animations became one.
    pub preview_pop: bool,
}

impl Items {
    /// Window previews pop open (the open / peek animation).
    pub fn pops_previews(&self) -> bool {
        self.small && self.preview_pop
    }

    pub const HIGH: Items = Items {
        magnify: true,
        spring: true,
        bounce: true,
        label_fade: true,
        minimize: Minimize::Shrink,
        blur: true,
        shadows: true,
        launchpad_motion: true,
        live_previews: true,
        small: true,
        frame_cap: 0,
        preview_pop: true,
    };
}

/// The items of a preset.
pub fn preset(level: Level) -> Items {
    match level {
        Level::Low => Items {
            magnify: false,
            spring: false,
            bounce: false,
            label_fade: false,
            minimize: Minimize::None,
            blur: false,
            shadows: false,
            launchpad_motion: false,
            live_previews: false,
            small: false,
            frame_cap: 30,
            preview_pop: false,
        },
        Level::Medium => Items { spring: false, shadows: false, frame_cap: 60, preview_pop: false, ..Items::HIGH },
        Level::High => Items::HIGH,
        Level::Ultra => Items { minimize: Minimize::Genie, ..Items::HIGH },
    }
}

/// The items as the config holds them. The internal limits (preview pop)
/// are not in the file: they follow a preset picked by name
/// (Low, Medium), else High's.
pub fn items_of(c: &Config) -> Items {
    let e = &c.effects;
    let limits = Level::from_name(preset_name(c)).map(preset).unwrap_or(Items::HIGH);
    Items {
        magnify: e.magnify,
        spring: e.spring,
        bounce: c.animation.bounce,
        label_fade: e.label_fade,
        minimize: c.animation.minimize,
        // Only Glass blurs: Flat is opaque whatever the switch says.
        blur: e.blur && c.glass(),
        shadows: e.shadows,
        launchpad_motion: e.launchpad_motion,
        live_previews: e.live_previews,
        small: e.small_animations,
        frame_cap: e.frame_cap,
        preview_pop: limits.preview_pop,
    }
}

/// Test setup: `i` written into the config's items.
#[cfg(test)]
pub fn set_items(c: &mut Config, i: &Items) {
    let e = &mut c.effects;
    e.magnify = i.magnify;
    e.spring = i.spring;
    c.animation.bounce = i.bounce;
    e.label_fade = i.label_fade;
    c.animation.minimize = i.minimize;
    e.blur = i.blur;
    e.shadows = i.shadows;
    e.launchpad_motion = i.launchpad_motion;
    e.live_previews = i.live_previews;
    e.small_animations = i.small;
    e.frame_cap = i.frame_cap;
}

/// The preset's config text, normalized (unknown, and older files'
/// "auto" = "high").
pub fn preset_name(c: &Config) -> &'static str {
    named(&c.effects.preset)
}

fn named(name: &str) -> &'static str {
    PRESETS.iter().find(|(n, _)| n.eq_ignore_ascii_case(name.trim())).map(|(n, _)| *n).unwrap_or("high")
}

/// Picks a preset by name: sets it and (except for "custom", which keeps
/// the items as they are) every item it covers. Test setup.
#[cfg(test)]
pub fn pick_preset(c: &mut Config, name: &str) {
    let name = named(name);
    c.effects.preset = name.to_string();
    if let Some(l) = Level::from_name(name) {
        set_items(c, &preset(l));
    }
}

/// Does High keep what `c` holds? (For files from before the page: their
/// minimize effect and bounce are High's.)
pub fn high_keeps(c: &Config) -> bool {
    c.animation.minimize == Items::HIGH.minimize && c.animation.bounce == Items::HIGH.bounce
}

/// The system around the dock that decides what is forced.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Env {
    /// Windows' "Animation effects".
    pub animations: bool,
    pub high_contrast: bool,
}

/// What is in force.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Resolved {
    pub items: Items,
    /// Animations run at all (Windows' animation effects off: no, whatever
    /// the preset).
    pub motion: bool,
    /// Animation speed (1 = as designed, 2 = twice as fast).
    pub speed: f32,
}

impl Resolved {
    pub const DEFAULT: Resolved = Resolved { items: Items::HIGH, motion: true, speed: 1.0 };
}

/// The animation speed held in the config, kept sane.
/// The dock's material with the blur as in force: the theme's (acrylic: the
/// DWM host backdrop; mica: the blurred wallpaper) only while something
/// blurs (Glass), else "solid" (an opaque fill, no backdrop). Pure: tested.
pub fn material(blur: bool, theme_material: &str) -> &str {
    if blur { theme_material } else { "solid" }
}

pub fn speed_of(c: &Config) -> f32 {
    let s = c.animation.speed;
    if s.is_finite() { s.clamp(0.25, 4.0) } else { 1.0 }
}

/// The effects in force for config `c` in `env`: the items as set, or
/// Low's with Low-spec mode on (no blur, so Flat and no backdrop work; no
/// motion items; the lowest frame rate).
pub fn resolve(c: &Config, env: Env) -> Resolved {
    let mut items = if c.effects.low_spec { preset(Level::Low) } else { items_of(c) };
    // Windows' "Animation effects" off wins over every preset (polish audit
    // F4): what moves comes down to short fades (crate::motion::Mode).
    let motion = env.animations;
    if !motion {
        // (As before the page: no magnification without animations.)
        items.magnify = false;
        items.spring = false;
        items.bounce = false;
        items.label_fade = false;
        items.launchpad_motion = false;
        items.small = false;
        items.preview_pop = false;
        if items.minimize == Minimize::Genie {
            items.minimize = Minimize::Shrink;
        }
    }
    if env.high_contrast {
        items.blur = false;
        items.shadows = false;
    }
    Resolved { items, motion, speed: speed_of(c) }
}

/// Why item `key` is off whatever it is set to (a text key), or None.
/// (`_c`: the config; no preset changes the answer since Windows'
/// animation effects win over every one.)
pub fn forced_off(key: &str, _c: &Config, env: Env) -> Option<&'static str> {
    let e = find(key)?;
    if e.hc_off && env.high_contrast {
        return Some("effects.forced.high_contrast");
    }
    if e.motion && !env.animations && key != "animation.minimize" {
        return Some("effects.forced.animations");
    }
    None
}

// ---------------------------------------------------------------- in force

static STATE: RwLock<Resolved> = RwLock::new(Resolved::DEFAULT);

/// The dock resolved the effects anew.
pub fn set(r: Resolved) {
    if let Ok(mut s) = STATE.write() {
        *s = r;
    }
}

pub fn current() -> Resolved {
    STATE.read().map(|s| *s).unwrap_or(Resolved::DEFAULT)
}

/// Animations run at all.
pub fn motion() -> bool {
    current().motion
}

/// Item checks for the code that animates.
pub fn items() -> Items {
    current().items
}

/// A duration in seconds at the chosen speed.
pub fn secs(base: f32) -> f32 {
    base / current().speed
}

/// A duration in ms at the chosen speed (never below 1 ms). Every
/// animation length goes through this or `secs` (one speed for all).
pub fn ms(base: i64) -> i64 {
    ms_at(base, current().speed)
}

/// Pure: `base` ms at speed `speed` (never below 1 ms).
pub fn ms_at(base: i64, speed: f32) -> i64 {
    ((base as f32 / speed).round() as i64).max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Flat is opaque all the way to the screen: no DWM backdrop, no blur,
    /// whatever the blur switch says; only a glass blurs (and its backdrop
    /// still gives way to high contrast and to Low-spec mode).
    #[test]
    fn flat_draws_no_backdrop_and_no_blur() {
        let env = Env { animations: true, high_contrast: false };
        let sys = crate::syslook::SysLook { transparency: true, ..Default::default() };
        let look = |c: &Config, env: Env| {
            let r = resolve(c, env);
            let theme = c.theme.load(std::path::Path::new(".")).resolve_fx(&sys, crate::theme::glass_blur(r.items.blur, false));
            (r.items.blur, material(r.items.blur, "acrylic").to_string(), material(r.items.blur, "mica").to_string(), theme.finish)
        };
        let flat_look = (false, "solid".to_string(), "solid".to_string(), crate::theme::Finish::Flat);
        let mut flat = Config::default();
        flat.set_finish_choice(0);
        for blur in [true, false] {
            flat.effects.blur = blur;
            assert_eq!(look(&flat, env), flat_look, "Flat, the switch {blur}");
            // No preset brings it back.
            for (_, name, _) in LEVELS {
                pick_preset(&mut flat, name);
                flat.effects.blur = blur;
                assert!(!resolve(&flat, env).items.blur, "{name}");
            }
        }
        for (i, finish) in [crate::theme::Finish::Glass].into_iter().enumerate() {
            let mut c = Config::default();
            c.set_finish_choice(i + 1);
            assert_eq!(look(&c, env), (true, "acrylic".into(), "mica".into(), finish));
        }
        let mut glass = Config::default();
        glass.set_finish_choice(1);
        let hc = look(&glass, Env { animations: true, high_contrast: true });
        assert_eq!((hc.0, hc.1.as_str()), (false, "solid"), "high contrast: opaque");
        // Low-spec mode: Flat, no backdrop, no mica snapshot.
        glass.set_low_spec(true);
        assert_eq!(look(&glass, env), flat_look, "low spec");
    }

    const ON: Env = Env { animations: true, high_contrast: false };

    /// A config with the Glass finish: the blur is used (Flat never blurs),
    /// so the items read as their switches are.
    fn glass() -> Config {
        let mut c = Config::default();
        c.set_finish_choice(2);
        c
    }

    /// Low-spec mode puts everything at Low; off, the items are used as
    /// set: nothing is lowered on its own any more.
    #[test]
    fn low_spec_is_the_lightest_and_off_keeps_the_choice() {
        let mut c = glass();
        assert!(!c.effects.low_spec, "off by default");
        assert_eq!(resolve(&c, ON).items, Items { bounce: false, ..Items::HIGH }, "a new config: launch bounce off");
        c.set_low_spec(true);
        let r = resolve(&c, ON);
        assert_eq!(r.items, preset(Level::Low));
        assert!(!r.items.blur && !r.items.shadows && !r.items.magnify && !r.items.small && r.items.minimize == Minimize::None);
        assert_eq!(r.items.frame_cap, 30);
        assert_eq!(crate::motion::mode(r.motion, r.items.small), crate::motion::Mode::Reduced, "animations reduced");
        // Whatever preset or switches are kept underneath.
        pick_preset(&mut c, "ultra");
        assert_eq!(resolve(&c, ON).items, preset(Level::Low));
        // Off: the preset as chosen, untouched.
        c.set_low_spec(false);
        assert_eq!(resolve(&c, ON).items, preset(Level::Ultra));
        let mut m = glass();
        pick_preset(&mut m, "medium");
        assert_eq!(resolve(&m, ON).items, preset(Level::Medium));
    }

    #[test]
    fn every_effect_has_a_name() {
        let mut keys = std::collections::HashSet::new();
        for e in ALL {
            assert!(keys.insert(e.key), "{} twice", e.key);
        }
        for (_, k) in PRESETS {
            assert!(crate::i18n::get(k).is_some(), "{k}");
        }
        for k in ["effects.forced.high_contrast", "effects.forced.animations", "effects.low_spec", "effects.low_spec.hint"] {
            assert!(crate::i18n::get(k).is_some(), "{k}");
        }
    }

    #[test]
    fn presets_set_every_item() {
        for (l, name, _) in LEVELS {
            let mut c = glass();
            pick_preset(&mut c, name);
            assert_eq!(c.effects.preset, name);
            assert_eq!(items_of(&c), preset(l));
        }
        assert!(preset(Level::Low).frame_cap == 30 && !preset(Level::Low).magnify && preset(Level::Low).minimize == Minimize::None);
        assert_eq!(preset(Level::Ultra).minimize, Minimize::Genie);
        assert!(!preset(Level::Medium).shadows && preset(Level::Medium).magnify);
        // Lower presets never have more on than higher ones.
        let count = |i: Items| [i.magnify, i.spring, i.bounce, i.label_fade, i.blur, i.shadows, i.launchpad_motion, i.live_previews, i.small].iter().filter(|b| **b).count();
        assert!(count(preset(Level::Low)) < count(preset(Level::Medium)));
        assert!(count(preset(Level::Medium)) < count(preset(Level::High)));
        assert!(!preset(Level::Low).small && preset(Level::Medium).small, "small animations: off only in Low");
        // The internal limits, per level.
        assert!(!preset(Level::Medium).pops_previews() && !preset(Level::Low).pops_previews() && Items::HIGH.pops_previews() && preset(Level::Ultra).pops_previews());
        // A new config is High; Custom keeps; a hand change makes Custom.
        let mut c = glass();
        assert_eq!(preset_name(&c), "high");
        assert_eq!(items_of(&c), Items { bounce: false, ..Items::HIGH });
        pick_preset(&mut c, "low");
        pick_preset(&mut c, "custom");
        assert_eq!(Items { preview_pop: false, ..items_of(&c) }, preset(Level::Low), "Custom keeps the stored items (its internal limits are High's)");
        pick_preset(&mut c, "custom");
        assert_eq!(preset_name(&c), "custom");
        // Older files' Auto, and anything unknown: High (what Auto stored).
        for old in ["auto", "AUTO", "turbo"] {
            c.effects.preset = old.into();
            assert_eq!(preset_name(&c), "high", "{old}");
        }
        pick_preset(&mut c, "auto");
        assert_eq!((c.effects.preset.as_str(), items_of(&c)), ("high", Items::HIGH));
        // Speed and the renderer are not a preset's.
        let mut c = glass();
        c.animation.speed = 1.5;
        c.effects.render = "software".into();
        pick_preset(&mut c, "low");
        assert_eq!((c.animation.speed, c.effects.render.as_str()), (1.5, "software"));
    }

    #[test]
    fn internal_limits_follow_the_preset() {
        let c = Config::default();
        let high = resolve(&c, ON).items;
        assert!(high.pops_previews());
        // A preset picked by name keeps its limits; Custom gets High's.
        let mut m = Config::default();
        pick_preset(&mut m, "medium");
        let r = resolve(&m, ON).items;
        assert!(r.small && !r.pops_previews(), "menus still fade; previews open at once");
        m.effects.preset = "custom".into();
        assert!(resolve(&m, ON).items.pops_previews());
        // No animations: no pop either.
        assert!(!resolve(&c, Env { animations: false, high_contrast: false }).items.pops_previews());
    }

    #[test]
    fn resolve_follows_windows_and_high_contrast() {
        let mut c = glass();
        assert_eq!(resolve(&c, ON).items, Items { bounce: false, ..Items::HIGH });
        // Windows' animation effects off stop what moves.
        let still = resolve(&c, Env { animations: false, high_contrast: false });
        assert!(!still.motion && !still.items.spring && !still.items.bounce && !still.items.magnify && !still.items.small);
        assert!(still.items.blur && still.items.live_previews, "not animations");
        assert!(forced_off("effects.spring", &c, Env { animations: false, high_contrast: false }).is_some());
        assert!(forced_off("effects.blur", &c, Env { animations: false, high_contrast: false }).is_none());
        // A preset is used as it is, but Windows' animation effects off
        // still wins (polish audit F4).
        pick_preset(&mut c, "ultra");
        let r = resolve(&c, ON);
        assert!(r.motion && r.items.minimize == Minimize::Genie);
        assert!(forced_off("effects.spring", &c, ON).is_none());
        let r = resolve(&c, Env { animations: false, high_contrast: false });
        assert!(!r.motion && !r.items.spring && !r.items.small && r.items.minimize == Minimize::Shrink);
        assert_eq!(forced_off("effects.spring", &c, Env { animations: false, high_contrast: false }), Some("effects.forced.animations"));
        // High contrast: no blur, no shadows, whatever is chosen.
        let hc = Env { animations: true, high_contrast: true };
        let r = resolve(&c, hc);
        assert!(!r.items.blur && !r.items.shadows && r.items.magnify);
        assert_eq!(forced_off("effects.blur", &c, hc), Some("effects.forced.high_contrast"));
        assert_eq!(forced_off("effects.shadows", &c, hc), Some("effects.forced.high_contrast"));
        assert_eq!(forced_off("effects.magnify", &c, hc), None);
        // Speed is kept sane.
        c.animation.speed = f32::NAN;
        assert_eq!(resolve(&c, ON).speed, 1.0);
        c.animation.speed = 100.0;
        assert_eq!(resolve(&c, ON).speed, 4.0);
    }
}
