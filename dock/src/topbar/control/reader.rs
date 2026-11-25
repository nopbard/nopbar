//! Screen readers: the tree the panel publishes and what it does.

use super::*;

impl Panel {
    // ---- screen readers ----

    /// The tree for screen readers (only while one reads this window).
    pub(super) fn publish(&self) {
        if a11y::wanted(self.hwnd) {
            a11y::publish(self.hwnd, self.tree());
        }
    }

    /// Every control the panel last drew, in drawing order, at its place on
    /// screen (right to left: mirrored, as drawn).
    pub(super) fn tree(&self) -> Tree {
        let r = self.shared.snapshot();
        let (ds, origin) = (self.ds(), (self.rect.left, self.rect.top));
        let px = |rc: &D2D_RECT_F| {
            let (l, rr) = crate::i18n::mirror::edges(self.rtl, rc.left, rc.right, 0.0, W);
            a11y::px_rect(origin, ds, l, rc.top, rr - l, rc.bottom - rc.top)
        };
        let texts = self.texts.borrow();
        let inert = self.inert.borrow();
        let text_at: Vec<usize> = texts.iter().map(|x| x.0).collect();
        let inert_at: Vec<usize> = inert.iter().map(|x| x.0).collect();
        let mut nodes = Vec::new();
        let mut seen: Vec<Hit> = Vec::new();
        for read in reading_order(self.hits.len(), &text_at, &inert_at) {
            match read {
                Read::Text(i) => {
                    let (_, rc, key, said) = &texts[i];
                    let top = if *key == "text:list_title" { 0.0 } else { self.list_body_top() };
                    let shown = clip_y(*rc, top, self.height);
                    let mut node = Node::new(*key, Role::Text, said.as_str(), px(&shown.unwrap_or(*rc)));
                    node.hidden = shown.is_none();
                    nodes.push(node);
                }
                // Drawn but taking no input now: read, disabled (node_of),
                // with its status.
                Read::Inert(i) => {
                    let (_, rc, h) = &inert[i];
                    let shown = self.visible_hit(*rc, *h);
                    let mut node = self.node(*h, &r, px(&shown.unwrap_or(*rc)));
                    node.hidden = shown.is_none();
                    nodes.push(node);
                }
                Read::Hit(i) => {
                    let (rc, h) = &self.hits[i];
                    if !seen.contains(h) {
                        seen.push(*h);
                        let shown = self.visible_hit(*rc, *h);
                        let mut node = self.node(*h, &r, px(&shown.unwrap_or(*rc)));
                        node.hidden = shown.is_none();
                        node.scroll = self.scroll_max > 0.0 && self.scrolls_with_body(*h);
                        nodes.push(node);
                    }
                }
            }
        }
        Tree {
            role: Role::Group,
            name: t("control.title").into(),
            nodes,
            focus: reader_focus(self.focus, &seen),
            focusable: true,
            active: true,
            focus_in: None,
        }
    }

    /// One control for screen readers: its name, and its toggle, expand or
    /// slider state, as the panel shows it.
    pub(super) fn node(&self, h: Hit, r: &Remote, rect: RECT) -> Node {
        let n = self.node_of(h, r, rect);
        // The status line drawn with it ("Switching…", "Couldn't change",
        // on / off, the mode, the device), as its value.
        match self.values.borrow().iter().find(|v| v.0 == h) {
            Some((_, said)) => n.value(said.as_str()),
            None => n,
        }
    }

    pub(super) fn node_of(&self, h: Hit, r: &Remote, rect: RECT) -> Node {
        let key = hit_key(h);
        let button = |name: &str| Node::new(key.clone(), Role::Button, name, rect).invoke();
        let slider = |name: &str, v: u8| {
            Node::new(key.clone(), Role::Slider, name, rect)
                .value(tf("control.percent", &[("p", &v.to_string())]))
                .range(a11y::Range { value: v as f64, min: 0.0, max: 100.0, step: 1.0 })
        };
        match h {
            Hit::WifiToggle | Hit::BtToggle => {
                let wifi = h == Hit::WifiToggle;
                let v = if wifi { r.wifi } else { r.bt };
                let n = button(t(if wifi { "control.wifi" } else { "control.bluetooth" }));
                match radio_reason(v) {
                    Some(why) => n.help(t(why)),
                    // While a switch is on its way: not switchable.
                    None => n.toggle(v.on()).enabled(v != RadioView::Loading && !r.radio_busy[if wifi { 0 } else { 1 }]),
                }
            }
            Hit::WifiName => button(t("control.wifi_list")),
            Hit::BtName => button(t("control.bluetooth_list")),
            Hit::AccessList => button(t("control.accessibility")),
            Hit::BluetoothSettings => button(t("control.bluetooth_settings")),
            Hit::AccessibilitySettings => button(t("control.accessibility_settings")),
            Hit::WifiSettings => button(t("control.wifi_settings")),
            Hit::WifiLocation => button(t("control.settings")),
            Hit::WifiRefresh => button(t("launch.action.retry")),
            Hit::ScrollList(up) => {
                let label = if up { "control.scroll_up" } else { "control.scroll_down" };
                button(t(label)).enabled(if up { self.scroll > 0.0 } else { self.scroll < self.scroll_max })
            }
            Hit::Studio => button(t("control.studio_effects")),
            Hit::WifiRow(key) => {
                let Some(n) = wifi_network(r, key) else { return button(t("control.wifi")).enabled(false) };
                if n.connected {
                    Node::new(hit_key(h), Role::Text, &n.ssid, rect)
                } else {
                    button(&n.ssid).help(t("control.connect")).enabled(r.wifi_busy.is_none())
                }
            }
            Hit::WifiDisconnect(key) => button(t("control.disconnect"))
                .help(wifi_network(r, key).map_or("", |n| n.ssid.as_str()))
                .enabled(r.wifi_busy.is_none() && wifi_network(r, key).is_some_and(|n| n.connected)),
            Hit::BtDevice(key) => {
                let Some(d) = bluetooth_device(r, key) else { return button(t("control.bluetooth")).enabled(false) };
                let action = if !d.can_switch || !r.bt.on() {
                    "control.opens_settings"
                } else if d.connected {
                    "control.disconnect"
                } else {
                    "control.connect"
                };
                let ready = d.kind != BtKind::Audio || !r.bt.on() || r.bt_audio_ready;
                button(&d.name).help(t(action)).enabled(ready && !r.bt_busy.contains(&key))
            }
            Hit::Access(a) => {
                let v = r.access[a as usize];
                let n = button(t(a.label())).enabled(!v.busy);
                match v.on.filter(|_| a != Access::MonoAudio) {
                    Some(on) => n.toggle(on),
                    None => n.help(t("control.opens_settings")),
                }
            }
            Hit::Dark => button(t("control.dark_mode")).toggle(self.dark).enabled(self.dark_seq == self.dark_done),
            Hit::Cast => button(t("control.screen_cast")).accel("Win+K").help(t("control.cast.find")),
            Hit::Quick(q) => {
                let v = quick_view(q, r, std::time::Instant::now());
                let mut n = button(t(quick_label(q))).enabled(v.ready);
                if v.switch {
                    // While a switch is on its way: not switchable.
                    let busy = q == Quick::Hotspot && r.hotspot_busy || q.system_setting().is_some_and(|s| r.system[s as usize].busy);
                    n = n.toggle(v.on).enabled(!busy);
                }
                if v.link {
                    n = n.help(t("control.opens_settings"));
                }
                n
            }
            Hit::Project => button(t("control.project")),
            Hit::Back => button(t("control.back")),
            Hit::Mode(m) => {
                let current = r.displays == Displays::Many(Some(m));
                Node::new(key, Role::MenuItem, t(m.label()), rect).invoke().toggle(current)
            }
            Hit::Brightness => slider(t("control.brightness"), r.brightness.flatten().unwrap_or(0)),
            Hit::Volume => slider(t("control.volume"), self.volume.map(|v| v.0).unwrap_or(0)),
            Hit::App(k) => {
                let a = r.apps.as_deref().unwrap_or(&[]).iter().find(|a| a.key == k);
                let level = self.app_level(k, a.map(|a| a.level).unwrap_or(0));
                slider(a.map(|a| a.name.as_str()).unwrap_or(t("control.unknown_device")), level)
            }
            Hit::Mute => button(t("control.muted")).toggle(self.volume.is_some_and(|v| v.1)),
            Hit::Outputs => button(t("control.output")).help(t("control.sound")),
            Hit::Device(flow, k) => {
                let e = device_list(r, flow).iter().find(|e| device_key(&e.id) == k);
                let name = e.map(|e| e.name.clone()).unwrap_or_else(|| t("control.unknown_device").into());
                let busy = r.audio_busy.as_ref().is_some_and(|(f, id)| *f == flow && e.is_some_and(|e| e.id == *id));
                // An output and an input often share a name (a headset).
                let name = device_name(flow, &name);
                Node::new(key, Role::MenuItem, name, rect).invoke().toggle(e.is_some_and(|e| e.default)).enabled(!busy)
            }
            Hit::Prev => button(t("island.prev")),
            Hit::Next => button(t("island.next")),
            Hit::PlayPause => button(t(if r.media.as_ref().is_some_and(|m| m.playing) { "island.pause" } else { "island.play" })),
            Hit::SoundSettings => button(t("topbar.panel.volume.settings")).help(t("control.audio.open_settings")),
            Hit::Eject(k) => {
                let rows = removable_now();
                let row = rows.iter().find(|x| removable_key(&x.device) == k);
                let name = row.map(|x| crate::removable::model::eject_name(t("control.eject.name_drives"), t("control.eject.name"), &x.device));
                // Not again while it is on its way, or once it is safe.
                let ready = row.is_some_and(|x| eject_ready(&x.note));
                button(&name.unwrap_or_else(|| t("control.unknown_device").into())).enabled(ready)
            }
        }
    }

    /// What a screen reader asked of control `key`.
    pub(super) fn a11y_act(&mut self, key: &str, act: Act) {
        if act == Act::Refresh {
            self.publish();
            return;
        }
        let Some(h) = self.order().into_iter().find(|h| hit_key(*h) == key) else { return };
        match act {
            Act::Invoke | Act::Toggle if !h.slider() => self.activate(h),
            Act::SetValue if h.slider() => {
                if let Some(v) = a11y::take_value(self.hwnd, key).and_then(|v| v.trim().parse::<f32>().ok()) {
                    self.set_slider(h, v / 100.0);
                }
            }
            Act::Focus => {
                // The focus moves; the ring as it was, or with Windows'
                // keyboard cues always on (crate::input).
                self.focus = Some(h);
                self.keyboard = crate::input::reader_focus(self.keyboard);
                self.reveal_focus();
            }
            Act::ScrollIntoView => {
                self.reveal(h);
            }
            _ => return,
        }
        if self.done.is_none() {
            self.redraw();
        }
    }
}
