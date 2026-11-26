//! Explicit reasons for small, local reactions. No random activity selection.
use super::clips::catalog;

#[derive(Default)]
pub struct Output {
    pub clip: Option<(&'static str, f32)>,
    pub frame: Option<String>,
    pub blend: Option<(String, f32)>,
    pub x: Option<f32>,
    pub flip: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Budget {
    pub date: u32,
    pub counts: [u8; 7],
    pub walks: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reason {
    Night,
    Wake,
    Break,
    Work,
    Look,
    Reunion,
    Rest,
}
impl Reason {
    fn index(self) -> usize {
        self as usize
    }
    fn cap(self) -> u8 {
        match self {
            Self::Night => 4,
            Self::Break => 8,
            Self::Reunion => 3,
            Self::Wake => 16,
            Self::Work | Self::Look => 32,
            Self::Rest => 16,
        }
    }
    fn sequence(self) -> &'static [&'static str] {
        match self {
            Self::Night => &["yawn", "read_doze", "stand"],
            Self::Wake => &["stand", "stretch"],
            Self::Break => &["stretch"],
            Self::Work => &["sit"],
            Self::Look | Self::Reunion => &["look"],
            Self::Rest => &["sit"],
        }
    }
}
#[derive(Clone, Copy, Default)]
pub struct Sample {
    pub now: f64,
    pub date: u32,
    pub minute: u16,
    pub away: bool,
    pub idle_seconds: f64,
    pub working: bool,
    pub defer: bool,
    pub night_at: Option<u16>,
}
struct Playing {
    clips: Vec<&'static str>,
    at: f64,
    reason: Reason,
}
#[derive(Default)]
pub struct Purpose {
    date: u32,
    counts: [u8; 7],
    away_at: Option<f64>,
    awake_at: Option<f64>,
    last_break: Option<f64>,
    last_yawn: Option<f64>,
    wake: bool,
    reunion: bool,
    working: bool,
    reading: bool,
    seated: bool,
    done: bool,
    look: Option<bool>,
    playing: Option<Playing>,
    last: Sample,
}
impl Purpose {
    pub fn with_budget(b: Budget) -> Self {
        Self {
            date: b.date,
            counts: b.counts,
            ..Default::default()
        }
    }
    pub fn budget(&self, aside: &Aside) -> Budget {
        Budget {
            date: self.date,
            counts: self.counts,
            walks: if aside.date == self.date {
                aside.count
            } else {
                0
            },
        }
    }
    pub fn using(&self) -> bool {
        self.last.date != 0 && !self.last.away && self.last.idle_seconds < 300.
    }

    pub fn away(&mut self, away: bool, now: f64) -> bool {
        let returned = !away && self.away_at.is_some_and(|at| long_absence(at, now));
        let mut s = self.last;
        s.now = now;
        s.away = away;
        self.observe(s);
        returned
    }

    pub fn observe(&mut self, s: Sample) {
        if self.date != s.date {
            self.date = s.date;
            self.counts = [0; 7];
        }
        if s.away {
            self.away_at.get_or_insert(s.now);
            self.awake_at = None;
            self.playing = None;
            self.seated = false;
        } else if let Some(at) = self.away_at.take() {
            self.wake = true;
            self.reunion |= long_absence(at, s.now);
            self.awake_at = Some(s.now);
        }
        if !s.away {
            self.awake_at.get_or_insert(s.now);
        }
        // Five minutes without input is a break, not continuous work.
        if s.idle_seconds >= 300. {
            self.awake_at = Some(s.now);
        }
        if self.working && !s.working && self.reading {
            self.done = true;
        }
        self.working = s.working;
        self.last = s;
    }
    pub fn notice(&mut self, left: bool) {
        self.look = Some(left);
    }
    fn start(&mut self, reason: Reason, now: f64) -> bool {
        let n = &mut self.counts[reason.index()];
        if *n >= reason.cap() {
            return false;
        }
        *n += 1;
        self.playing = Some(Playing {
            clips: reason.sequence().to_vec(),
            at: now,
            reason,
        });
        true
    }
    pub fn frame_at(&mut self, now: f64, available: bool) -> Output {
        self.last.now = now;
        self.frame(available)
    }
    pub fn frame(&mut self, available: bool) -> Output {
        let s = self.last;
        if !available {
            self.playing = None;
            return Output::default();
        }
        if s.away {
            return output("sleep", s.now, false);
        }
        if self.done {
            self.done = false;
            self.reading = false;
            self.seated = false;
            self.playing = Some(Playing {
                clips: vec!["stand", "done"],
                at: s.now,
                reason: Reason::Work,
            });
        }
        if self.playing.is_none() {
            if self.wake && !s.defer {
                self.wake = false;
                self.start(Reason::Wake, s.now);
            } else if self.reunion && !s.defer {
                self.reunion = false;
                self.start(Reason::Reunion, s.now);
            } else if self.look.is_some() {
                if !self.start(Reason::Look, s.now) {
                    self.look = None;
                }
            } else if s.working
                && !self.reading
                && self.counts[Reason::Work.index()] < Reason::Work.cap()
            {
                if self.start(Reason::Work, s.now) {
                    self.reading = true;
                    self.seated = false;
                }
            } else if !s.defer
                && self.counts[Reason::Night.index()] < Reason::Night.cap()
                && s.idle_seconds < 300.
                && s.minute >= s.night_at.unwrap_or(23 * 60)
                && self.last_yawn.is_none_or(|at| s.now - at >= 1800.)
            {
                if self.start(Reason::Night, s.now) {
                    self.last_yawn = Some(s.now);
                }
            } else if !s.defer
                && self.counts[Reason::Break.index()] < Reason::Break.cap()
                && self.awake_at.is_some_and(|at| s.now - at > 3000.)
                && self.last_break.is_none_or(|at| s.now - at >= 3000.)
            {
                if self.start(Reason::Break, s.now) {
                    self.last_break = Some(s.now);
                }
            } else if !s.working && !self.seated && s.idle_seconds >= 600. {
                if self.start(Reason::Rest, s.now) {
                    self.seated = true;
                }
            } else if self.seated && (s.working || s.idle_seconds < 600.) {
                self.seated = false;
                self.playing = Some(Playing {
                    clips: vec!["stand"],
                    at: s.now,
                    reason: Reason::Rest,
                });
            }
        }
        if let Some(p) = &self.playing {
            let mut elapsed = s.now - p.at;
            for clip in &p.clips {
                let duration = catalog().clip(clip).duration() as f64;
                if elapsed < duration {
                    return output(clip, elapsed, self.look.unwrap_or(false));
                }
                elapsed -= duration;
            }
            if p.reason == Reason::Look {
                self.look = None;
            }
            self.playing = None;
        }
        if self.reading && s.working {
            output("read", s.now, false)
        } else if self.seated {
            output("sit", catalog().clip("sit").duration() as f64, false)
        } else {
            Output::default()
        }
    }
}
fn output(clip: &'static str, time: f64, flip: bool) -> Output {
    let c = catalog().clip(clip);
    Output {
        clip: Some((clip, time as f32)),
        frame: Some(c.frame(time as f32).into()),
        flip,
        blend: c.transition(time as f32).map(|(f, a)| (f.into(), a)),
        ..Default::default()
    }
}

/// One obstruction trip, with an exact return point. No patrol or window riding.
#[derive(Default)]
pub struct Aside {
    home: Option<f32>,
    window: isize,
    target: f32,
    date: u32,
    count: u8,
    attempted: std::collections::BTreeSet<isize>,
}
impl Aside {
    pub fn with_budget(b: Budget) -> Self {
        Self {
            date: b.date,
            count: b.walks,
            ..Default::default()
        }
    }
    pub fn observe(
        &mut self,
        date: u32,
        x: f32,
        left: f32,
        right: f32,
        obstacle: Option<(f32, f32)>,
        margin: f32,
    ) {
        if self.date != date {
            self.date = date;
            self.count = 0;
            self.attempted.clear();
        }
        let home = self.home.unwrap_or(x);
        let blocking = obstacle.filter(|(l, r)| home + margin > *l && home - margin < *r);
        if let Some((l, r)) = blocking {
            if self.home.is_none() && (self.count >= 16 || !self.attempted.insert(self.window)) {
                return;
            }
            let target = [l - margin, r + margin]
                .into_iter()
                .filter(|v| *v >= left && *v <= right)
                .min_by(|a, b| (a - x).abs().total_cmp(&(b - x).abs()));
            let Some(target) = target.filter(|t| (t - x).abs() >= 0.1) else {
                return;
            };
            if self.home.is_none() {
                self.count += 1;
                self.home = Some(x);
            }
            self.target = target;
        } else if self.home.is_some() {
            self.target = home;
        }
    }
    pub fn step(&mut self, x: f32, dt: f32) -> Option<(f32, bool)> {
        let home = self.home?;
        let delta = self.target - x;
        if delta.abs() < 0.1 {
            if (self.target - home).abs() < 0.1 {
                self.home = None;
            }
            return None;
        }
        let next = x + delta.clamp(-52. * dt.min(0.25), 52. * dt.min(0.25));
        Some((next, delta < 0.))
    }
    pub fn clear(&mut self) {
        self.home = None;
        self.window = 0;
    }
}

fn covers_monitor(
    r: windows::Win32::Foundation::RECT,
    m: windows::Win32::Foundation::RECT,
) -> bool {
    r.left <= m.left && r.top <= m.top && r.right >= m.right && r.bottom >= m.bottom
}
unsafe fn desktop_window(h: windows::Win32::Foundation::HWND) -> bool {
    let mut class = [0u16; 64];
    let n = windows::Win32::UI::WindowsAndMessaging::GetClassNameW(h, &mut class) as usize;
    matches!(
        String::from_utf16_lossy(&class[..n]).as_str(),
        "WorkerW" | "Progman" | "Shell_TrayWnd" | "Shell_SecondaryTrayWnd"
    )
}

fn long_absence(at: f64, now: f64) -> bool {
    now - at > 2. * 60. * 60.
}

#[cfg(test)]
#[test]
fn return_signal_requires_more_than_two_hours_and_fires_once() {
    let mut purpose = Purpose::default();
    assert!(!purpose.away(true, 10.));
    assert!(!purpose.away(false, 7210.));
    assert!(!purpose.away(true, 7450.));
    assert!(!purpose.away(true, 10_000.));
    assert!(purpose.away(false, 14_651.));
    assert!(!purpose.away(false, 14_652.));
}

pub fn uptime() -> f64 {
    unsafe { windows::Win32::System::SystemInformation::GetTickCount64() as f64 / 1000. }
}
impl super::App {
    pub(super) fn observe_purpose(
        &mut self,
        now: std::time::Instant,
        signal: &super::model::Signal,
        mode: super::visibility::Mode,
    ) {
        if now < self.purpose_due {
            return;
        }
        self.purpose_due = now + std::time::Duration::from_secs(1);
        use windows::Win32::{
            System::SystemInformation::GetTickCount,
            UI::{
                Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO},
                Shell::{SHQueryUserNotificationState, QUNS_ACCEPTS_NOTIFICATIONS},
                WindowsAndMessaging::*,
            },
        };
        let mut input = LASTINPUTINFO {
            cbSize: std::mem::size_of::<LASTINPUTINFO>() as u32,
            ..Default::default()
        };
        let idle = unsafe {
            if GetLastInputInfo(&mut input).as_bool() {
                GetTickCount().wrapping_sub(input.dwTime) as f64 / 1000.
            } else {
                0.
            }
        };
        let calendar = super::living::local_calendar();
        let quiet = unsafe {
            SHQueryUserNotificationState().is_ok_and(|s| s != QUNS_ACCEPTS_NOTIFICATIONS)
        };
        self.purpose.observe(Sample {
            now: uptime(),
            date: calendar.date,
            minute: calendar.minute,
            away: super::living::away(),
            idle_seconds: idle,
            working: signal.working || crate::dock::companion_working(),
            defer: idle < 2.
                || quiet
                || mode != super::visibility::Mode::Run
                || self.ground.is_some_and(|g| g.covered)
                || super::CALM.load(super::Ordering::SeqCst) != 0,
            night_at: self.cfg.routine.learned(),
        });
        self.cfg.routine.observe(
            crate::notes::time::now().div_euclid(86400),
            calendar.minute,
            !super::living::away() && idle < 60.,
        );
        if self.grab.busy() || self.peek_home().is_some() || self.trip.is_some() {
            self.aside.clear();
            return;
        }
        let Some(g) = self.ground else {
            return;
        };
        let margin = self.cfg.size.margin();
        let mut r = windows::Win32::Foundation::RECT::default();
        let obstacle = unsafe {
            // Keep following the obstruction even after focus changes.
            let h = if self.aside.home.is_some() && self.aside.window != 0 {
                windows::Win32::Foundation::HWND(self.aside.window as *mut _)
            } else {
                GetForegroundWindow()
            };
            let mut pid = 0;
            GetWindowThreadProcessId(h, Some(&mut pid));
            if pid != std::process::id()
                && GetWindowRect(h, &mut r).is_ok()
                && !IsIconic(h).as_bool()
                && !IsZoomed(h).as_bool()
                && !desktop_window(h)
                && IsWindowVisible(h).as_bool()
                && !g.covered
                && !covers_monitor(r, g.monitor)
                && r.bottom as f32 > g.floor - 100. * g.dpi
                && (r.top as f32) < g.floor
            {
                self.aside.window = h.0 as isize;
                Some((r.left as f32 / g.dpi, r.right as f32 / g.dpi))
            } else {
                None
            }
        };
        self.aside.observe(
            calendar.date,
            self.motion.x,
            g.monitor.left as f32 / g.dpi + margin,
            g.monitor.right as f32 / g.dpi - margin,
            obstacle,
            margin,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn review_1_fullscreen_obstruction_is_excluded_even_after_focus_changes() {
        use windows::Win32::Foundation::RECT;
        let m = RECT {
            left: -1920,
            top: 0,
            right: 0,
            bottom: 1080,
        };
        assert!(covers_monitor(m, m));
        assert!(covers_monitor(
            RECT {
                left: -1930,
                top: -10,
                right: 10,
                bottom: 1090
            },
            m
        ));
        assert!(!covers_monitor(RECT { bottom: 1000, ..m }, m));
    }
    #[test]
    fn review_1_no_escape_never_spends_budget_or_retries_same_window() {
        let mut a = Aside {
            window: 42,
            ..Default::default()
        };
        for _ in 0..30 {
            a.observe(1, 300., 30., 870., Some((0., 900.)), 30.);
            assert!(a.step(300., 0.1).is_none());
        }
        assert_eq!(a.count, 0);
        a.observe(1, 300., 30., 870., Some((250., 400.)), 30.);
        assert!(a.step(300., 0.1).is_none());
    }
    #[test]
    fn review_1_same_window_is_only_counted_once() {
        let mut a = Aside {
            window: 42,
            ..Default::default()
        };
        a.observe(1, 300., 0., 900., Some((250., 400.)), 30.);
        a.observe(1, 300., 0., 900., None, 30.);
        a.step(300., 0.1);
        a.window = 42;
        a.observe(1, 300., 0., 900., Some((250., 400.)), 30.);
        assert_eq!(a.count, 1);
    }
    #[test]
    fn daily_caps_survive_restart_and_reset_on_the_next_date() {
        let mut p = Purpose::default();
        p.observe(sample(0.));
        p.counts = [32; 7];
        let aside = Aside {
            date: 20261117,
            count: 16,
            ..Default::default()
        };
        let saved = serde_json::to_string(&p.budget(&aside)).unwrap();
        let b: Budget = serde_json::from_str(&saved).unwrap();
        let mut restored = Purpose::with_budget(b);
        restored.observe(sample(1.));
        assert!(!restored.start(Reason::Night, 1.));
        assert_eq!(Aside::with_budget(b).count, 16);
        let mut tomorrow = sample(2.);
        tomorrow.date += 1;
        restored.observe(tomorrow);
        assert!(restored.start(Reason::Night, 2.));
    }
    #[test]
    fn exhausted_look_does_not_starve_other_reasons() {
        let mut p = Purpose::default();
        p.observe(sample(0.));
        p.counts[Reason::Look.index()] = Reason::Look.cap();
        p.notice(true);
        assert!(p.frame(true).frame.is_none());
        let mut s = sample(1.);
        s.working = true;
        p.observe(s);
        assert_eq!(p.frame(true).clip.unwrap().0, "sit");
    }
    fn sample(now: f64) -> Sample {
        Sample {
            now,
            date: 20261117,
            minute: 600,
            idle_seconds: 10.,
            ..Default::default()
        }
    }
    #[test]
    fn time_without_a_reason_never_selects_an_activity() {
        let mut p = Purpose::default();
        for i in 0..1000 {
            let mut s = sample(i as f64 * 60.);
            s.defer = true;
            p.observe(s);
            assert!(p.frame(true).frame.is_none());
        }
    }
    #[test]
    fn night_and_break_defer_and_obey_spacing_and_daily_caps() {
        let mut p = Purpose::default();
        p.observe(sample(0.));
        let mut s = sample(3001.);
        s.defer = true;
        p.observe(s);
        assert!(p.frame(true).frame.is_none());
        s.defer = false;
        p.observe(s);
        assert_eq!(p.frame(true).clip.unwrap().0, "stretch");
        s.now += 5.;
        p.observe(s);
        p.frame(true);
        assert!(p.frame(true).frame.is_none());
        let mut p = Purpose::default();
        for i in 0..6 {
            let mut s = sample(i as f64 * 1800.);
            s.minute = 23 * 60;
            p.observe(s);
            let o = p.frame(true);
            assert_eq!(o.clip.is_some_and(|(clip, _)| clip == "yawn"), i < 4);
            s.now += 30.;
            p.observe(s);
            p.frame(true);
        }
        assert_eq!(p.counts[Reason::Night.index()], 4);
    }
    #[test]
    fn work_reads_until_finished_then_closes_book_and_done() {
        let mut p = Purpose::default();
        let mut s = sample(0.);
        s.working = true;
        p.observe(s);
        assert_eq!(p.frame(true).clip.unwrap().0, "sit");
        s.now = 10.;
        p.observe(s);
        assert_eq!(p.frame(true).clip.unwrap().0, "read");
        s.now = 20.;
        s.working = false;
        p.observe(s);
        assert_eq!(p.frame(true).clip.unwrap().0, "stand");
        s.now += catalog().clip("stand").duration() as f64 + 0.01;
        p.observe(s);
        assert_eq!(p.frame(true).clip.unwrap().0, "done");
    }
    #[test]
    fn absence_wakes_stretches_and_reunion_is_small_and_deferred() {
        let mut p = Purpose::default();
        let mut s = sample(0.);
        s.away = true;
        p.observe(s);
        assert_eq!(p.frame(true).clip.unwrap().0, "sleep");
        s.away = false;
        s.now = 7201.;
        s.defer = true;
        p.observe(s);
        assert!(p.frame(true).frame.is_none());
        s.defer = false;
        p.observe(s);
        assert_eq!(p.frame(true).clip.unwrap().0, "stand");
        s.now += 10.;
        p.observe(s);
        p.frame(true);
        assert_eq!(p.frame(true).clip.unwrap().0, "look");
    }
    #[test]
    fn ten_minutes_without_events_sits_then_stands_on_input() {
        let mut p = Purpose::default();
        let mut s = sample(0.);
        s.idle_seconds = 600.;
        p.observe(s);
        assert_eq!(p.frame(true).clip.unwrap().0, "sit");
        s.now = 10.;
        p.observe(s);
        p.frame(true);
        s.idle_seconds = 0.;
        p.observe(s);
        assert_eq!(p.frame(true).clip.unwrap().0, "stand");
    }
    #[test]
    fn every_reason_has_a_daily_cap_and_new_day_resets_it() {
        for reason in [
            Reason::Night,
            Reason::Wake,
            Reason::Break,
            Reason::Work,
            Reason::Look,
            Reason::Reunion,
            Reason::Rest,
        ] {
            let mut p = Purpose::default();
            p.observe(sample(0.));
            for _ in 0..reason.cap() {
                assert!(p.start(reason, 0.));
            }
            assert!(!p.start(reason, 90000.));
            let mut s = sample(90000.);
            s.date += 1;
            p.observe(s);
            assert!(p.start(reason, s.now));
        }
    }
    #[test]
    fn look_requires_an_event_and_remembers_its_side() {
        let mut p = Purpose::default();
        p.observe(sample(0.));
        assert!(p.frame(true).frame.is_none());
        p.notice(true);
        let frame = p.frame(true);
        assert_eq!(frame.clip.unwrap().0, "look");
        assert!(frame.flip);
    }
    #[test]
    fn obstruction_walks_aside_and_returns_without_patrol() {
        let mut a = Aside::default();
        a.observe(1, 300., 0., 900., None, 30.);
        assert!(a.step(300., 1.).is_none());
        a.observe(1, 300., 0., 900., Some((250., 400.)), 30.);
        let mut x = 300.;
        for _ in 0..100 {
            if let Some((next, _)) = a.step(x, 0.1) {
                x = next;
            }
        }
        assert!((x - 220.).abs() < 0.1);
        a.observe(1, x, 0., 900., None, 30.);
        for _ in 0..100 {
            if let Some((next, _)) = a.step(x, 0.1) {
                x = next;
            }
        }
        assert!((x - 300.).abs() < 0.1);
        assert!(a.step(x, 1.).is_none());
    }
}
