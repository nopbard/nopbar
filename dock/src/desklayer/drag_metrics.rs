//! Per-drag telemetry. Samples are stored on the scene thread and summarized at release.
use crate::monitors::Monitor;
use std::{collections::VecDeque, fmt::Write, mem::size_of, sync::OnceLock};
use windows::{
    core::{s, w, HRESULT, PCSTR, PCWSTR},
    Win32::{
        Foundation::{HWND, RECT},
        Graphics::{
            DirectComposition::{
                COMPOSITION_FRAME_ID_COMPLETED, COMPOSITION_FRAME_ID_TYPE,
                COMPOSITION_FRAME_STATS, COMPOSITION_TARGET_ID,
            },
            Dwm::{DwmGetCompositionTimingInfo, DWM_TIMING_INFO},
            Gdi::{EnumDisplaySettingsW, GetMonitorInfoW, DEVMODEW, ENUM_CURRENT_SETTINGS, HMONITOR, MONITORINFOEXW},
        },
        System::{
            LibraryLoader::{GetProcAddress, LoadLibraryExW, LOAD_LIBRARY_SEARCH_SYSTEM32},
            Performance::{QueryPerformanceCounter, QueryPerformanceFrequency},
        },
    },
};

fn frequency() -> f64 {
    static FREQUENCY: OnceLock<f64> = OnceLock::new();
    *FREQUENCY.get_or_init(|| {
        let mut hz = 0;
        unsafe { QueryPerformanceFrequency(&mut hz) }.expect("QPC frequency");
        hz as f64
    })
}

pub(super) fn now() -> f64 {
    let mut ticks = 0;
    unsafe { QueryPerformanceCounter(&mut ticks) }.expect("QPC");
    ticks as f64 * 1000. / frequency()
}

pub(super) type RawProc = unsafe extern "system" fn() -> isize;

pub(super) fn dcomp_proc(name: PCSTR) -> Option<RawProc> {
    // Keep the system DLL mapped for the lifetime of the cached function pointers.
    static MODULE: OnceLock<Option<usize>> = OnceLock::new();
    let dll = MODULE.get_or_init(|| unsafe {
        LoadLibraryExW(w!("dcomp.dll"), None, LOAD_LIBRARY_SEARCH_SYSTEM32)
            .ok().map(|h| h.0 as usize)
    });
    unsafe { GetProcAddress(windows::Win32::Foundation::HMODULE((*dll)? as *mut _), name) }
}

type FrameId = unsafe extern "system" fn(COMPOSITION_FRAME_ID_TYPE, *mut u64) -> HRESULT;
type Statistics = unsafe extern "system" fn(
    u64, *mut COMPOSITION_FRAME_STATS, u32, *mut COMPOSITION_TARGET_ID, *mut u32,
) -> HRESULT;

#[derive(Clone, Copy)]
struct Frame {
    id: u64,
    period: f64,
    target: f64,
    source: &'static str,
}

fn dcomp_frame() -> Option<Frame> {
    static API: OnceLock<Option<(FrameId, Statistics)>> = OnceLock::new();
    let (frame_id, statistics) = (*API.get_or_init(|| unsafe {
        Some((
            std::mem::transmute::<RawProc, FrameId>(dcomp_proc(s!("DCompositionGetFrameId"))?),
            std::mem::transmute::<RawProc, Statistics>(dcomp_proc(s!("DCompositionGetStatistics"))?),
        ))
    }))?;
    let mut id = 0;
    let mut stats = COMPOSITION_FRAME_STATS::default();
    // Frame statistics are sufficient; use the bounded target buffer
    // from the compositor clock documentation.
    let mut targets = [COMPOSITION_TARGET_ID::default(); 4];
    let mut target_count = 0;
    unsafe {
        frame_id(COMPOSITION_FRAME_ID_COMPLETED, &mut id).ok().ok()?;
        statistics(id, &mut stats, targets.len() as u32, targets.as_mut_ptr(), &mut target_count).ok().ok()?;
    }
    Some(Frame {
        id,
        period: stats.framePeriod as f64 * 1000. / frequency(),
        target: stats.targetTime as f64 * 1000. / frequency(),
        source: "dcomp",
    }).filter(|frame| frame.period > 0.)
}

fn frame() -> Option<Frame> {
    dcomp_frame().or_else(|| {
        let mut info = DWM_TIMING_INFO { cbSize: size_of::<DWM_TIMING_INFO>() as u32, ..Default::default() };
        unsafe { DwmGetCompositionTimingInfo(HWND::default(), &mut info) }.ok()?;
        Some(Frame {
            id: info.cFrameDisplayed,
            period: info.qpcRefreshPeriod as f64 * 1000. / frequency(),
            target: info.qpcVBlank as f64 * 1000. / frequency(),
            source: "dwm",
        }).filter(|frame| frame.period > 0.)
    })
}

pub(super) fn monitor_hz(handle: isize) -> Option<u32> {
    let mut info = MONITORINFOEXW::default();
    info.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
    let mut mode = DEVMODEW { dmSize: size_of::<DEVMODEW>() as u16, ..Default::default() };
    unsafe {
        if !GetMonitorInfoW(HMONITOR(handle as *mut _), &mut info.monitorInfo).as_bool()
            || !EnumDisplaySettingsW(PCWSTR(info.szDevice.as_ptr()), ENUM_CURRENT_SETTINGS, &mut mode).as_bool()
        {
            return None;
        }
    }
    (mode.dmDisplayFrequency > 1).then_some(mode.dmDisplayFrequency)
}

#[derive(Clone, Copy)]
struct Hand {
    at: f64,
    sample: f64,
    point: (f32, f32),
}

const FRAME_HISTORY: usize = 512;

fn remember<T>(history: &mut VecDeque<T>, value: T) {
    if history.len() == FRAME_HISTORY {
        history.pop_front();
    }
    history.push_back(value);
}

pub(super) struct Metrics {
    start: f64,
    first_frame: Option<Frame>,
    monitors: Vec<(Monitor, Option<u32>)>,
    visited: Vec<isize>,
    monitor: Option<isize>,
    live: bool,
    last_input: Option<f64>,
    last_probe: Option<f64>,
    moves: usize,
    input_dt: Vec<f64>,
    handling: Vec<f64>,
    ticks: VecDeque<f64>,
    tick_count: usize,
    tick_max: f64,
    stalls: usize,
    stall_max: f64,
    hand: VecDeque<Hand>,
    hints: usize,
    hint_ms: f64,
}

impl Metrics {
    pub fn new(start: f64, origin: RECT, monitors: &[Monitor]) -> Self {
        let mut metrics = Self {
            start,
            first_frame: frame(),
            monitors: monitors.iter().map(|m| (*m, monitor_hz(m.handle))).collect(),
            visited: Vec::with_capacity(monitors.len()),
            monitor: None,
            live: super::live::enabled(),
            last_input: None,
            last_probe: None,
            moves: 0,
            input_dt: Vec::with_capacity(512),
            handling: Vec::with_capacity(512),
            ticks: VecDeque::with_capacity(FRAME_HISTORY),
            tick_count: 0,
            tick_max: 0.,
            stalls: 0,
            stall_max: 0.,
            hand: VecDeque::with_capacity(FRAME_HISTORY),
            hints: 0,
            hint_ms: 0.,
        };
        metrics.monitor(super::grid::monitor_of(origin, monitors).map(|i| monitors[i].handle));
        metrics
    }

    pub fn monitor(&mut self, monitor: Option<isize>) {
        self.monitor = monitor;
        if let Some(id) = monitor.filter(|m| !self.visited.contains(m)) {
            self.visited.push(id);
        }
    }

    pub fn displays(&self) -> Vec<(isize, Option<u32>)> {
        self.monitors.iter().map(|(m, hz)| (m.handle, *hz)).collect()
    }

    fn period(&self) -> f64 {
        self.monitors.iter().find_map(|(m, hz)| {
            (Some(m.handle) == self.monitor).then_some(*hz).flatten()
        }).map_or(self.first_frame.map_or(1000. / 60., |f| f.period), |hz| 1000. / f64::from(hz))
    }

    pub fn input(&mut self, start: f64, end: f64) {
        self.moves += 1;
        if let Some(last) = self.last_input.replace(start) {
            self.input_dt.push(start - last);
        }
        self.handling.push(end - start);
    }

    pub fn probe(&mut self, at: f64) {
        if let Some(last) = self.last_probe.replace(at) {
            self.gap(at - last);
        }
    }

    pub fn tick(&mut self, elapsed: f64) {
        self.tick_count += 1;
        self.tick_max = self.tick_max.max(elapsed);
        remember(&mut self.ticks, elapsed);
    }

    fn gap(&mut self, ms: f64) {
        if ms > self.period() * 2. {
            self.stalls += 1;
            self.stall_max = self.stall_max.max(ms);
        }
    }

    pub fn hand(&mut self, point: (f32, f32), sample: f64) {
        remember(&mut self.hand, Hand { at: now(), sample, point });
    }

    pub fn hint_created(&mut self, ms: f64) {
        self.hint_ms += ms;
        self.hints += 1;
    }

    pub fn finish(mut self) {
        let end = now();
        let last = frame();
        let frames = self.first_frame.zip(last)
            .filter(|(a, b)| a.source == b.source)
            .and_then(|(a, b)| b.id.checked_sub(a.id))
            .map_or_else(|| "na".into(), |n| n.to_string());
        let period = last.map_or(self.period(), |f| f.period);
        let mut lag: Vec<_> = self.hand.iter().map(|h| {
            let to_frame = last.map_or(period / 2., |f| (f.target - h.at).rem_euclid(period));
            (h.at - h.sample).max(0.) + to_frame
        }).collect();
        let cv = frame_cv(self.hand.make_contiguous(), period, self.tick_count > 0);
        let env = self.monitors.iter().fold(String::new(), |mut env, (m, hz)| {
            let _ = write!(env, "{}{}:{}Hz@{:.2}x", if env.is_empty() { "" } else { "," },
                m.handle, hz.map_or_else(|| "na".into(), |h| h.to_string()), m.scale());
            env
        });
        self.input_dt.sort_unstable_by(f64::total_cmp);
        self.handling.sort_unstable_by(f64::total_cmp);
        let ticks = self.ticks.make_contiguous();
        ticks.sort_unstable_by(f64::total_cmp);
        lag.sort_unstable_by(f64::total_cmp);
        let dt = |q| quantile(&self.input_dt, q);
        let handle = |q| quantile(&self.handling, q);
        let tick = |q| quantile(ticks, q);
        crate::log!(
            "drag: {:.2}s moves={} dt50={} dt95={} dtmax={} handle50={}ms handle95={}ms handle99={}ms handlemax={}ms ticks={} tick50={}ms tick95={}ms tick99={}ms tickmax={}ms stall={} stallmax={:.2}ms frames={} hz={} frame_source={} cv={} lag50={}ms hint_create_in_drag={} hint_create_ms={:.2} monitors={} primary={:?} visited={:?} live={}",
            (end - self.start) / 1000., self.moves, dt(0.5), dt(0.95), dt(1.),
            handle(0.5), handle(0.95), handle(0.99), handle(1.), self.tick_count, tick(0.5), tick(0.95), tick(0.99),
            value((self.tick_count > 0).then_some(self.tick_max)),
            self.stalls, self.stall_max, frames,
            value(last.map(|f| 1000. / f.period)), last.map_or("na", |f| f.source),
            value(cv), quantile(&lag, 0.5), self.hints, self.hint_ms, env,
            self.monitors.iter().find(|(m, _)| m.primary).map(|(m, _)| m.handle),
            self.visited, self.live,
        );
    }
}

fn value(x: Option<f64>) -> String {
    x.map_or_else(|| "na".into(), |x| format!("{x:.2}"))
}

fn quantile(sorted: &[f64], q: f64) -> String {
    let rank = ((sorted.len() as f64 * q).ceil() as usize).saturating_sub(1);
    value(sorted.get(rank).copied())
}

// Compare displacement at the frame cadence during roughly straight, constant-speed
// sweeps. Each four-frame block must agree with both neighbors within 20%.
fn frame_cv(hand: &[Hand], period: f64, frame_paced: bool) -> Option<f64> {
    let points: Vec<_> = if frame_paced {
        hand.iter().map(|h| h.point).collect()
    } else {
        let mut t = hand.first()?.at;
        let end = hand.last()?.at;
        let mut i = 0;
        let mut points = Vec::new();
        while t <= end {
            while hand.get(i + 1).is_some_and(|h| h.at <= t) {
                i += 1;
            }
            points.push(hand[i].point);
            t += period;
        }
        points
    };
    let mut count = 0;
    let mut squared_error = 0.;
    for w in points.windows(13).step_by(4) {
        let shift = |a: usize, b: usize| (w[b].0 - w[a].0, w[b].1 - w[a].1);
        let (x, y) = shift(4, 8);
        let distance = x.hypot(y);
        if distance < 8. || [(0, 4), (8, 12)].into_iter().any(|(a, b)| {
            let (dx, dy) = shift(a, b);
            (dx - x).hypot(dy - y) > distance * 0.2
        }) {
            continue;
        }
        let steps = [4, 5, 6, 7].map(|a| {
            let (dx, dy) = shift(a, a + 1);
            dx.hypot(dy)
        });
        let mean = steps.iter().sum::<f32>() / 4.;
        for step in steps {
            squared_error += f64::from((step / mean - 1.).powi(2));
        }
        count += 4;
    }
    (count >= 8).then(|| (squared_error / f64::from(count)).sqrt())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_history_keeps_only_the_most_recent_samples() {
        let mut samples = VecDeque::new();
        for i in 0..FRAME_HISTORY * 3 {
            remember(&mut samples, i);
        }
        assert_eq!(samples.len(), FRAME_HISTORY);
        assert_eq!(samples.front(), Some(&(FRAME_HISTORY * 2)));
        assert_eq!(samples.back(), Some(&(FRAME_HISTORY * 3 - 1)));
    }
}
