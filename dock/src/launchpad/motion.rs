//! Launchpad's motion, the pure part (design system v2.1, 7.1 and 7.4):
//! the plate's fade (opening SCRIM_MS, closing SCRIM_OUT_MS, straight: a
//! fade only, no zoom, no stagger), the close fallback and the rubber band
//! past the first and last page. Nothing here is Launchpad's own value: the
//! lengths, the curve and the page turn's spring (GLIDE, which carries a
//! swipe's speed on) are crate::motion's.
//!
//! Everything that moves is a Composition animation started once (view.rs):
//! nothing runs per frame on the CPU, nothing at all once it stops.

use crate::motion::{Curve, Mode, LINEAR, SCRIM_MS, SCRIM_OUT_MS};

/// The close fallback timer: the animation's length plus this, at most
/// CLOSE_FALLBACK_MAX_MS (the window always goes, even if the batch never
/// says it is done).
pub const CLOSE_SLACK_MS: i64 = 250;
pub const CLOSE_FALLBACK_MAX_MS: i64 = 3000;
/// Rubber band past the first / last page (the iOS constant).
pub const RUBBER: f32 = 0.55;

/// Does Launchpad animate? Only with full motion (crate::motion: Windows'
/// "Animation effects" and NopBar's small animations on), its own item on
/// and no high contrast. Otherwise nothing moves: 60 ms straight fades only
/// (design system 12.2).
pub fn allowed(full: bool, item: bool, high_contrast: bool) -> bool {
    full && item && !high_contrast
}

/// Launchpad's motion mode: full when it animates (`allowed`).
pub fn mode(allowed: bool) -> Mode {
    if allowed {
        Mode::Full
    } else {
        Mode::Reduced
    }
}

/// Pure: the plate's fade as it finally plays: (ms of real time, curve).
/// Opening SCRIM_MS, closing SCRIM_OUT_MS (closing is the quicker), both
/// straight, stretched by the speed setting `speed`; not animating:
/// crate::motion::REDUCED_MS straight whatever the speed (fade_plan
/// decides, as for every fade).
pub fn scrim(allowed: bool, opening: bool, speed: f32) -> (i64, Curve) {
    crate::motion::fade_plan(mode(allowed), if opening { SCRIM_MS } else { SCRIM_OUT_MS }, LINEAR, speed)
}

/// Pure: a change the pointer made (a hover's fill coming on or going) as
/// it finally plays in Launchpad: 90 in on the decelerating curve, 120 out
/// on the accelerating one (6.3); not animating: REDUCED_MS straight. High
/// contrast (`instant`): at once (a fading outline passes through colors
/// that break the contrast).
pub fn pointer(allowed: bool, on: bool, instant: bool, speed: f32) -> (i64, Curve) {
    if instant {
        return (0, LINEAR);
    }
    let curve = if on { crate::motion::EASE_OUT } else { crate::motion::EASE_IN };
    crate::motion::fade_plan(mode(allowed), crate::motion::pointer_ms(Mode::Full, on), curve, speed)
}

/// How long the close fallback timer waits for an animation of `anim_ms`
/// of real time (what the fade's plan said).
pub fn close_fallback(anim_ms: i64) -> u32 {
    (anim_ms.max(0) + CLOSE_SLACK_MS).clamp(CLOSE_SLACK_MS, CLOSE_FALLBACK_MAX_MS) as u32
}

/// A drag of `dx` DIP past the first or last page moves the strip only
/// this far: about as far as the finger at first, less and less after,
/// never a whole page (`width`).
pub fn rubber(dx: f32, width: f32) -> f32 {
    if !(dx.is_finite() && width.is_finite() && width > 0.0) {
        return 0.0;
    }
    let x = dx.abs();
    let r = (1.0 - 1.0 / (x * RUBBER / width + 1.0)) * width;
    r.copysign(dx)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_plate_fades_220_in_and_180_out_straight() {
        assert_eq!(scrim(true, true, 1.0), (220, LINEAR));
        assert_eq!(scrim(true, false, 1.0), (180, LINEAR), "closing is the quicker");
        assert_eq!(scrim(true, true, 2.0).0, 110, "twice as fast");
        assert_eq!(scrim(true, false, 0.5).0, 360, "half as fast");
    }

    #[test]
    fn not_animating_is_60ms_straight_at_any_speed() {
        for speed in [0.25f32, 1.0, 4.0] {
            for opening in [true, false] {
                assert_eq!(scrim(false, opening, speed), (crate::motion::REDUCED_MS, LINEAR), "{speed}");
            }
            for on in [true, false] {
                assert_eq!(pointer(false, on, false, speed), (crate::motion::REDUCED_MS, LINEAR), "{speed}");
            }
        }
        // Its own item off, or high contrast: not animating, even with
        // full motion everywhere else.
        assert!(!allowed(true, false, false) && !allowed(true, true, true) && !allowed(false, true, false));
        assert!(allowed(true, true, false));
    }

    #[test]
    fn a_hover_comes_in_90_and_goes_in_120() {
        assert_eq!(pointer(true, true, false, 1.0), (90, crate::motion::EASE_OUT));
        assert_eq!(pointer(true, false, false, 1.0), (120, crate::motion::EASE_IN));
        // High contrast: at once, both ways.
        assert_eq!(pointer(true, true, true, 1.0).0, 0);
        assert_eq!(pointer(false, false, true, 1.0).0, 0);
    }

    #[test]
    fn the_close_fallback_outlasts_the_animation() {
        for speed in [0.25f32, 0.5, 1.0, 2.0, 4.0] {
            for (allowed, opening) in [(true, true), (true, false), (false, false)] {
                let anim = scrim(allowed, opening, speed).0;
                let t = close_fallback(anim) as i64;
                assert!(t > anim, "{speed}: {t} <= {anim}");
                assert!(t <= CLOSE_FALLBACK_MAX_MS);
            }
        }
        assert_eq!(close_fallback(180) as i64, 180 + CLOSE_SLACK_MS);
        assert_eq!(close_fallback(-5) as i64, CLOSE_SLACK_MS);
        assert_eq!(close_fallback(i64::MAX / 2) as i64, CLOSE_FALLBACK_MAX_MS, "always closes in the end");
    }

    #[test]
    fn rubber_band_gives_less_and_less() {
        let w = 1920.0;
        assert_eq!(rubber(0.0, w), 0.0);
        // About the finger at first...
        let small = rubber(10.0, w);
        assert!(small > 5.0 && small <= 10.0, "{small}");
        // ...less after, never a whole page, and the same both ways.
        let mut last = 0.0;
        for k in 1..50 {
            let r = rubber(k as f32 * 100.0, w);
            assert!(r > last && r < w && r < k as f32 * 100.0);
            last = r;
        }
        assert_eq!(rubber(-300.0, w), -rubber(300.0, w));
        assert!(rubber(1e9, w) < w);
        assert_eq!(rubber(f32::NAN, w), 0.0);
        assert_eq!(rubber(10.0, 0.0), 0.0);
    }
}
