//! Pointer, keys and wheel.

use super::*;

const TILE_MOVE_DELAY: Duration = Duration::from_millis(400);

/// Pointer presses use the last painted positions. Keyboard and UIA address controls by identity.
#[derive(Default)]
pub(super) struct TileClicks(Vec<(Hit, D2D_RECT_F, Option<Instant>)>);

impl TileClicks {
    pub(super) fn update(&mut self, positions: impl Iterator<Item = (Hit, D2D_RECT_F)>, now: Instant) {
        let before = std::mem::take(&mut self.0);
        self.0 = positions.map(|(hit, rc)| {
            let old = before.iter().find(|(prior, _, _)| *prior == hit);
            let until = match old {
                Some((_, prior, until)) if *prior == rc => *until,
                _ if !before.is_empty() => Some(now + TILE_MOVE_DELAY),
                _ => None,
            };
            (hit, rc, until)
        }).collect();
    }

    pub(super) fn blocked(&self, hit: Hit, now: Instant) -> bool {
        hit.tile() && self.0.iter().any(|&(h, _, until)| h == hit && until.is_some_and(|at| now < at))
    }
}

impl Panel {
    pub(super) fn list_body_top(&self) -> f32 {
        let top = PAD + SUB_HEAD_H + GAP;
        if self.open.is_some() && self.height >= top + LINE_H + 2.0 * LIST_PAD { top } else { 0.0 }
    }

    pub(super) fn scrolls_with_body(&self, hit: Hit) -> bool {
        self.list_body_top() == 0.0 || !matches!(hit, Hit::Back | Hit::ScrollList(_))
    }

    pub(super) fn visible_hit(&self, rc: D2D_RECT_F, hit: Hit) -> Option<D2D_RECT_F> {
        let top = if self.scrolls_with_body(hit) { self.list_body_top() } else { 0.0 };
        clip_y(rc, top, self.height)
    }

    pub(super) fn reveal_focus(&mut self) -> bool {
        self.focus.is_some_and(|hit| self.reveal(hit))
    }

    pub(super) fn reveal(&mut self, hit: Hit) -> bool {
        if !self.scrolls_with_body(hit) {
            return false;
        }
        let Some((r, _)) = self.hits.iter().find(|(_, h)| *h == hit) else { return false };
        let r = rect(r.left, r.top - RING_OUT, r.right, r.bottom + RING_OUT);
        let offset = reveal_offset(r, self.list_body_top(), self.height, self.scroll, self.scroll_max);
        if offset == self.scroll {
            return false;
        }
        self.scroll = offset;
        true
    }

    pub(super) fn hit_at(&self, x: f32, y: f32) -> Option<Hit> {
        // The mute glyph lies on the volume slider and wins; so does a
        // chip on its row.
        self.hits.iter().find_map(|&(r, h)| self.visible_hit(r, h).filter(|r| inside(r, x, y)).map(|_| h))
    }

    /// Control `h` is drawn now (it takes input).
    pub(super) fn live(&self, h: Hit) -> bool {
        self.hits.iter().any(|x| x.1 == h)
    }

    /// The focus order: everything clickable, in drawing order, once each.
    pub(super) fn order(&self) -> Vec<Hit> {
        let mut v: Vec<Hit> = Vec::new();
        for (_, h) in &self.hits {
            if !v.contains(h) {
                v.push(*h);
            }
        }
        v
    }

    pub(super) fn finish(&mut self, out: Outcome, after: Option<After>) {
        self.done.get_or_insert(out);
        self.after = after;
    }

    pub(super) fn activate(&mut self, h: Hit) {
        match h {
            Hit::WifiToggle | Hit::BtToggle => {
                let which = if h == Hit::WifiToggle { Which::Wifi } else { Which::Bt };
                // Cannot be switched here (no adapter, no access, a
                // hardware switch): the row says why; pressing it opens
                // the Windows page that can.
                let view = self.shared.snapshot();
                let view = if which == Which::Wifi { view.wifi } else { view.bt };
                if let Some(page) = radio_fix(which == Which::Wifi, view) {
                    self.finish(Outcome::Closed, Some(After::Open(page)));
                    return;
                }
                let mut want = None;
                if let Ok(mut r) = self.shared.remote.lock() {
                    let k = if which == Which::Wifi { 0 } else { 1 };
                    if r.radio_busy[k] {
                        // The last switch is still on its way.
                        return;
                    }
                    let v = if which == Which::Wifi { &mut r.wifi } else { &mut r.bt };
                    // Show the change at once; the worker confirms it.
                    match *v {
                        RadioView::On => {
                            *v = RadioView::Off;
                            want = Some(false);
                        }
                        RadioView::Off => {
                            *v = RadioView::On;
                            want = Some(true);
                        }
                        _ => {}
                    }
                    if want.is_some() {
                        r.radio_busy[k] = true;
                        r.radio_failed[k] = None;
                        if which == Which::Wifi {
                            r.wifi_networks = None;
                            r.ssid = Ssid::Unknown;
                        }
                    }
                }
                if let Some(on) = want {
                    if self.workers.radio.send(RadioCmd::Set(which, on)).is_err() {
                        // No worker to answer: nothing is on its way.
                        let k = if which == Which::Wifi { 0 } else { 1 };
                        self.shared.set_quiet(|r| {
                            r.radio_busy[k] = false;
                            r.radio_failed[k] = Some(std::time::Instant::now());
                        });
                        self.sync();
                    }
                }
            }
            Hit::WifiName => self.set_section(Some(Section::Wifi)),
            Hit::BtName => self.set_section(Some(Section::Bluetooth)),
            Hit::AccessList => self.set_section(Some(Section::Accessibility)),
            Hit::Studio => self.finish(Outcome::Closed, Some(After::Studio)),
            Hit::BluetoothSettings => self.finish(Outcome::Closed, Some(After::Open("ms-settings:bluetooth"))),
            Hit::AccessibilitySettings => self.finish(Outcome::Closed, Some(After::Open("ms-settings:easeofaccess"))),
            Hit::WifiLocation => self.finish(Outcome::Closed, Some(After::Open("ms-settings:privacy-location"))),
            Hit::WifiSettings => self.finish(Outcome::Closed, Some(After::Open("ms-settings:network-wifi"))),
            Hit::WifiRefresh => {
                let _ = self.workers.radio.send(RadioCmd::WifiList);
            }
            Hit::ScrollList(up) => { self.scroll_list(up); }
            Hit::WifiRow(key) => self.activate_wifi(key, true),
            Hit::WifiDisconnect(key) => self.activate_wifi(key, false),
            Hit::BtDevice(key) => self.activate_bluetooth(key),
            Hit::Access(a) => self.activate_accessibility(a),
            Hit::Dark => {
                self.dark = !self.dark;
                self.dark_failed = None;
                // Shown at once; the worker sends back what really happened.
                self.dark_seq += 1;
                set_dark(self.dark_seq, self.dark, &self.shared);
            }
            Hit::Cast => self.finish(Outcome::Closed, Some(After::Cast)),
            Hit::Quick(Quick::Hotspot) => {
                let mut want = None;
                if let Ok(mut r) = self.shared.remote.lock() {
                    if r.hotspot_busy {
                        // The last switch is still on its way.
                        return;
                    }
                    // Shown at once; the worker confirms it.
                    want = switch_state(r.hotspot).map(|on| !on);
                    if let Some(on) = want {
                        r.hotspot = if on { RadioView::On } else { RadioView::Off };
                        r.hotspot_busy = true;
                        r.hotspot_failed = None;
                    }
                }
                if let Some(on) = want {
                    if self.workers.quick.send(QuickCmd::Hotspot(on)).is_err() {
                        self.shared.set_quiet(|r| {
                            r.hotspot = if on { RadioView::Off } else { RadioView::On };
                            r.hotspot_busy = false;
                            r.hotspot_failed = Some(std::time::Instant::now());
                        });
                        self.sync();
                    }
                }
            }
            Hit::Quick(Quick::Awake) => {
                // A power request: a moment, on this thread.
                let want = !self.shared.snapshot().awake;
                let now = awake::set(want);
                self.shared.set_quiet(|r| {
                    r.awake = now;
                    r.awake_failed = (now != want).then(std::time::Instant::now);
                });
                self.sync();
            }
            Hit::Quick(q) => {
                if let Some(setting) = q.system_setting() {
                    let index = setting as usize;
                    let mut settings = false;
                    let mut automatic = false;
                    let mut target = None;
                    self.shared.set_quiet(|r| {
                        let tile = &mut r.system[index];
                        if !tile.ready || tile.busy {
                            return;
                        }
                        if tile.state.writable {
                            target = tile.state.selected.or(tile.state.on).map(|on| !on);
                            tile.busy = target.is_some();
                            tile.failed = None;
                        } else {
                            settings = true;
                            automatic = setting == Setting::Dnd && tile.state.automatic_dnd();
                        }
                    });
                    if let Some(on) = target {
                        if self.workers.system.send(SystemCmd::Set(setting, on)).is_err() {
                            self.shared.set_quiet(|r| {
                                r.system[index].busy = false;
                                r.system[index].failed = Some(Instant::now());
                            });
                            self.sync();
                        }
                        return;
                    }
                    if automatic {
                        self.finish(Outcome::Closed, Some(After::Open("ms-settings:notifications")));
                        return;
                    }
                    if !settings {
                        return;
                    }
                }
                let page = if q == Quick::EnergySaver && self.shared.snapshot().battery.is_none() {
                    Some("ms-settings:powersleep")
                } else {
                    quick_page(q, windows_build())
                };
                if let Some(page) = page {
                    self.finish(Outcome::Closed, Some(After::Open(page)));
                }
            }
            Hit::Project => self.set_section(Some(Section::Project)),
            Hit::Outputs => {
                // Nothing to list yet (still asking): nothing to go into.
                if !section_cards(Section::Sound, &self.shared.snapshot()).is_empty() {
                    self.set_section(Some(Section::Sound));
                }
            }
            Hit::Back => self.set_section(None),
            Hit::Mode(m) => {
                if !mode_switches(self.shared.snapshot().displays, m) {
                    // Already so: just go back.
                    self.set_section(None);
                } else {
                    self.finish(Outcome::Closed, Some(After::Project(m)));
                }
            }
            Hit::Mute => {
                if let Some((p, m)) = self.volume {
                    self.volume = Some((p, !m));
                }
                if !cfg!(test) {
                    crate::topbar::status::toggle_mute();
                }
            }
            Hit::Prev | Hit::PlayPause | Hit::Next => {
                let a = match h {
                    Hit::Prev => MediaAct::Prev,
                    Hit::Next => MediaAct::Next,
                    _ => MediaAct::Toggle,
                };
                let _ = self.workers.media.send(MediaCmd::Act(a));
            }
            Hit::Device(flow, key) => {
                let mut pick = None;
                if let Ok(mut r) = self.shared.remote.lock() {
                    // By ID, as the list is now (it may have changed
                    // since the press or the focus).
                    let id = device_list(&r, flow).iter().find(|e| device_key(&e.id) == key && !e.default).map(|e| e.id.clone());
                    if let Some(id) = id {
                        // "Switching…" until the worker reads back what
                        // Windows did; the check moves only then (a
                        // refused switch leaves it on the old device).
                        r.audio_fail = None;
                        r.audio_busy = Some((flow, id.clone()));
                        pick = Some(id);
                    }
                }
                if let Some(id) = pick {
                    let _ = self.workers.audio.send(AudioCmd::SetDefault(flow, id));
                }
                self.sync();
            }
            Hit::SoundSettings => self.finish(Outcome::Closed, Some(After::Open("ms-settings:sound"))),
            Hit::Eject(k) => {
                // By connection, as the list is now; the helper thread
                // ejects it and the strip says "Ejecting…" at once.
                if !cfg!(test) {
                    if let Some(c) = crate::removable::rows().into_iter().find(|x| removable_key(&x.device) == k).map(|x| x.device.conn()) {
                        crate::removable::eject(&c);
                    }
                }
                self.sync();
            }
            Hit::Brightness | Hit::Volume | Hit::App(_) => {}
        }
    }

    fn activate_wifi(&mut self, key: u64, on: bool) {
        let r = self.shared.snapshot();
        let Some(n) = wifi_network(&r, key) else { return };
        if r.wifi_busy.is_some() || n.connected == on {
            return;
        }
        if on && n.profile.is_none() {
            self.finish(Outcome::Closed, Some(After::Open("ms-availablenetworks:")));
            return;
        }
        self.shared.set_quiet(|r| {
            r.wifi_busy = Some((key, on));
            r.wifi_failed = None;
        });
        if self.workers.radio.send(RadioCmd::WifiSwitch(key, on)).is_err() {
            self.shared.set_quiet(|r| {
                r.wifi_busy = None;
                r.wifi_failed = Some((key, Instant::now()));
            });
            self.sync();
        }
    }

    fn activate_bluetooth(&mut self, key: u64) {
        let r = self.shared.snapshot();
        let Some(device) = bluetooth_device(&r, key) else { return };
        if r.bt_busy.contains(&key) || (device.kind == BtKind::Audio && r.bt.on() && !r.bt_audio_ready) {
            return;
        }
        if !device.can_switch || !r.bt.on() {
            self.finish(Outcome::Closed, Some(After::Open("ms-settings:bluetooth")));
            return;
        }
        self.shared.set_quiet(|r| {
            r.bt_busy.insert(key);
            r.bt_failed.remove(&key);
        });
        let on = !device.connected;
        if self.workers.devices.send(DeviceCmd::Switch(key, device.name.clone(), on)).is_err() {
            bluetooth::log_switch(&device.name, on, false);
            self.shared.set_quiet(|r| {
                r.bt_busy.remove(&key);
                r.bt_failed.insert(key, Instant::now());
            });
            self.sync();
        }
    }

    fn activate_accessibility(&mut self, access: Access) {
        let i = access as usize;
        let view = self.shared.snapshot().access[i];
        if view.busy {
            return;
        }
        let Some(on) = view.on.filter(|_| access != Access::MonoAudio) else {
            self.finish(Outcome::Closed, Some(After::Open(access.page())));
            return;
        };
        self.shared.set_quiet(|r| {
            r.access[i].busy = true;
            r.access[i].failed = None;
        });
        if self.workers.system.send(SystemCmd::Access(access, !on)).is_err() {
            self.shared.set_quiet(|r| {
                r.access[i].busy = false;
                r.access[i].failed = Some(Instant::now());
            });
            self.sync();
        }
    }

    /// Sets slider `h` to `v` (0..1).
    pub(super) fn set_slider(&mut self, h: Hit, v: f32) {
        let p = (v.clamp(0.0, 1.0) * 100.0).round() as u8;
        match h {
            Hit::Volume => {
                // set_volume also unmutes.
                self.volume = Some((p, false));
                if !cfg!(test) {
                    crate::topbar::status::set_volume(p as f32 / 100.0);
                }
            }
            Hit::Brightness => {
                self.shared.set_quiet(|r| r.brightness = Some(Some(p)));
                let _ = self.workers.bright.send(BrightCmd::Set(p));
            }
            Hit::App(k) => {
                // Shown at once, numbered so a read the worker began before
                // carrying it out does not undo it (merge_apps).
                let mut seq = None;
                self.shared.set_quiet(|r| {
                    if let Some(a) = r.apps.as_mut().and_then(|l| l.iter_mut().find(|a| a.key == k)) {
                        a.level = p;
                        if p > 0 {
                            a.muted = false;
                        }
                        r.app_seq += 1;
                        r.app_wrote.insert(k, r.app_seq);
                        seq = Some(r.app_seq);
                    }
                });
                if let Some(seq) = seq {
                    self.app_drag = Some((k, p));
                    let _ = self.workers.audio.send(AudioCmd::AppVolume(k, p, seq));
                }
            }
            _ => {}
        }
    }

    pub(super) fn slider_now(&self, h: Hit) -> Option<f32> {
        match h {
            Hit::Volume => self.volume.map(|(p, _)| p as f32 / 100.0),
            Hit::Brightness => self.shared.snapshot().brightness.flatten().map(|b| b as f32 / 100.0),
            Hit::App(k) => {
                let r = self.shared.snapshot();
                let a = r.apps.as_deref().unwrap_or(&[]).iter().find(|a| a.key == k).map(|a| a.level)?;
                Some(self.app_level(k, a) as f32 / 100.0)
            }
            _ => None,
        }
    }

    pub(super) fn slide_to(&mut self, h: Hit, x: f32) {
        // Not once it went (the list was left, the device is gone).
        if !self.live(h) {
            return;
        }
        if let Some(&(_, r)) = self.sliders.iter().find(|s| s.0 == h) {
            let v = if matches!(h, Hit::Brightness | Hit::Volume) { cap_value(x, r) } else { slider_value(x, r.left, r.right) };
            self.set_slider(h, v);
        }
    }

    /// A thick slider moved: only its surface is drawn again (the value
    /// follows the pointer at once).
    pub(super) fn redraw_pill(&mut self, h: Hit) {
        let Some(&(_, sr)) = self.sliders.iter().find(|s| s.0 == h) else { return self.redraw() };
        let r = self.shared.snapshot();
        let spec = self.pill_spec(h, sr, &r, &self.tones());
        self.sync_parts_only(vec![Spec::Pill(spec)]);
        self.publish();
    }

    /// Panel DIP of a client point (lParam).
    pub(super) fn dip(&self, lp: LPARAM) -> (f32, f32) {
        let m = (SHADOW_M * self.scale).ceil();
        let x = (lp.0 & 0xFFFF) as i16 as f32;
        let y = ((lp.0 >> 16) & 0xFFFF) as i16 as f32;
        // Right to left: back into the (unmirrored) layout's space.
        (crate::i18n::mirror::x(self.rtl, (x - m) / self.ds(), 0.0, W), (y - m) / self.ds())
    }

    pub(super) fn on_move(&mut self, lp: LPARAM) {
        let (x, y) = self.dip(lp);
        // A drag from the mute glyph moves the volume instead.
        if self.pressed == Some(Hit::Mute) && (x - self.press_at.0).abs() > 4.0 {
            self.pressed = None;
            self.sliding = Some(Hit::Volume);
            self.focus = Some(Hit::Volume);
            self.slide_to(Hit::Volume, x);
            self.redraw();
            return;
        }
        // A drag whose slider went (the list was left) ends here.
        if self.sliding.is_some_and(|h| !self.live(h)) {
            self.sliding = None;
            self.app_drag = None;
        }
        if let Some(h) = self.sliding {
            self.slide_to(h, x);
            if matches!(h, Hit::Brightness | Hit::Volume) {
                self.redraw_pill(h);
            } else {
                self.redraw();
            }
            return;
        }
        let h = self.hit_at(x, y);
        if h != self.hover {
            self.hover = h;
            self.redraw();
            self.tip_follow(h);
        }
    }

    pub(super) fn on_button(&mut self, hwnd: HWND, lp: LPARAM, down: bool, left: bool) {
        let mut p = POINT { x: (lp.0 & 0xFFFF) as i16 as i32, y: ((lp.0 >> 16) & 0xFFFF) as i16 as i32 };
        unsafe {
            let _ = ClientToScreen(hwnd, &mut p);
        }
        if !down {
            if left && self.sliding.take().is_some() {
                self.app_drag = None;
                self.redraw();
                return;
            }
            let (x, y) = self.dip(lp);
            // Only the button the press began on, let go over it, does
            // anything.
            let pressed = self.pressed.take();
            if left {
                if let Some(h) = pressed.filter(|&h| Some(h) == self.hit_at(x, y) && !self.tile_clicks.blocked(h, Instant::now())) {
                    self.activate(h);
                }
            }
            if self.done.is_none() {
                self.redraw();
            }
            return;
        }
        if !contains(&self.rect, p) {
            // Outside, the control button included: close. The click is
            // not passed on (like a menu), so the button does not reopen it.
            self.finish(Outcome::Closed, None);
            return;
        }
        // A press of any button inside hides the keyboard's ring
        // (crate::input).
        self.keyboard = false;
        if !left {
            self.redraw();
            return;
        }
        self.hide_tip();
        let (x, y) = self.dip(lp);
        self.press_at = (x, y);
        self.hover = self.hit_at(x, y);
        match self.hover.filter(|&h| !self.tile_clicks.blocked(h, Instant::now())) {
            Some(h) if h.slider() => {
                self.sliding = Some(h);
                self.focus = Some(h);
                self.slide_to(h, x);
            }
            Some(h) => self.pressed = Some(h),
            None => {}
        }
        self.redraw();
    }

    pub(super) fn on_key(&mut self, vk: u16) {
        let shift = unsafe { GetKeyState(VK_SHIFT.0 as i32) } < 0;
        let focus_slider = self.focus.filter(|h| h.slider() && self.live(*h));
        // The ring shows once the keyboard moves the focus, not on a media,
        // volume or letter key (crate::input; final/owner-volume-ring.png).
        if crate::input::navigates(vk) {
            self.keyboard = true;
        }
        self.hide_tip();
        if let Some(h) = focus_slider {
            if let Some(nv) = self.slider_now(h).and_then(|old| slider_key(vk, old, self.rtl)) {
                self.set_slider(h, nv);
                self.redraw();
                return;
            }
        }
        match vk {
            // Esc leaves a list first, then closes the panel.
            v if v == VK_ESCAPE.0 && self.open.is_some() => self.set_section(None),
            v if v == VK_ESCAPE.0 => self.finish(Outcome::Closed, None),
            // Backspace and Alt+Left go back too.
            v if v == VK_BACK.0 && self.open.is_some() => self.set_section(None),
            v if v == VK_TAB.0 => self.focus = step_focus(&self.order(), self.focus, if shift { -1 } else { 1 }),
            v if v == VK_DOWN.0 => self.focus = step_focus(&self.order(), self.focus, 1),
            v if v == VK_UP.0 => self.focus = step_focus(&self.order(), self.focus, -1),
            v if v == VK_RIGHT.0 || v == VK_LEFT.0 => self.focus = step_focus(&self.order(), self.focus, crate::i18n::mirror::arrow(self.rtl, v == VK_RIGHT.0)),
            v if v == VK_RETURN.0 || v == VK_SPACE.0 => {
                if let Some(h) = self.focus.filter(|h| !h.slider() && self.live(*h)) {
                    self.activate(h);
                }
            }
            _ => return,
        }
        if self.done.is_none() {
            self.reveal_focus();
            self.redraw();
        }
    }

    pub(super) fn on_wheel(&mut self, up: bool) {
        if self.sliding.is_none() && self.scroll_list(up) {
            return;
        }
        // Over a slider (or the mute glyph): WHEEL_STEP a notch.
        let h = self.hover.filter(|h| self.live(*h)).and_then(|h| if h.slider() { Some(h) } else { (h == Hit::Mute).then_some(Hit::Volume) });
        if let Some(h) = h {
            if let Some(old) = self.slider_now(h) {
                self.set_slider(h, old + if up { WHEEL_STEP } else { -WHEEL_STEP });
                self.redraw();
            }
        }
    }

    fn scroll_list(&mut self, up: bool) -> bool {
        if self.scroll_max <= 0.0 {
            return false;
        }
        let step = if self.open.is_some() { 44.0 } else { UNIT + GAP };
        let offset = (self.scroll + if up { -step } else { step }).clamp(0.0, self.scroll_max);
        if offset == self.scroll {
            return true;
        }
        self.scroll = offset;
        if let Some(Hit::ScrollList(direction)) = self.focus {
            let at_end = if direction { self.scroll == 0.0 } else { self.scroll == self.scroll_max };
            if at_end {
                self.focus = Some(Hit::ScrollList(!direction));
            }
        }
        self.hide_tip();
        self.hover = None;
        self.pressed = None;
        self.redraw();
        true
    }
}
