//! Radios (Windows.Devices.Radios) and the Wi-Fi name.

use super::*;

// ---- radios (Windows.Devices.Radios) and the Wi-Fi name -----------------------

use crate::topbar::panels::{wifi_connect, wifi_disconnect, wifi_list_since, Wifi, WifiError};

pub(super) fn wifi_key(network: &Wifi) -> u64 {
    device_key(&network.ssid)
}

#[derive(Default)]
struct Networks {
    switch: Option<(u64, bool, Instant)>,
    next: Option<Instant>,
    first_scan: Option<Instant>,
}

impl Networks {
    fn radio_changed(&mut self, shared: &Shared, was: RadioView, now: RadioView) {
        if was.on() == now.on() {
            return;
        }
        self.next = None;
        self.switch = None;
        self.first_scan = now.on().then(Instant::now);
        shared.update(|r| {
            r.ssid = if now.on() { Ssid::Unknown } else { Ssid::None };
            r.wifi_busy = None;
            r.wifi_networks = None;
        });
        if now.on() {
            self.read(shared);
        }
    }

    fn read(&mut self, shared: &Shared) {
        let list = wifi_list_since(self.first_scan);
        if list != Err(WifiError::Pending) {
            self.first_scan = None;
        }
        let now = Instant::now();
        self.next = (list == Err(WifiError::Pending) || self.switch.is_some()).then_some(now + Duration::from_millis(350));
        let ssid = match &list {
            Ok(nets) => nets.iter().find(|n| n.connected).map(|n| Ssid::Some(n.ssid.clone())).unwrap_or(Ssid::None),
            Err(_) => Ssid::Unknown,
        };
        shared.update(|r| {
            if list != Err(WifiError::Pending) || r.wifi_networks.is_none() {
                r.ssid = ssid;
                r.wifi_networks = Some(list.clone());
            }
            if let Some((key, target, until)) = self.switch {
                let reached = list.as_ref().is_ok_and(|nets| nets.iter().any(|n| wifi_key(n) == key && n.connected) == target);
                if reached || now >= until {
                    r.wifi_busy = None;
                    r.wifi_failed = (!reached).then_some((key, now));
                    self.switch = None;
                }
            }
        });
    }

    fn switch(&mut self, shared: &Shared, key: u64, on: bool) {
        let snapshot = shared.snapshot();
        let network = snapshot.wifi_networks.as_ref().and_then(|r| r.as_ref().ok())
            .and_then(|nets| nets.iter().find(|n| wifi_key(n) == key));
        let accepted = network.is_some_and(|n| {
            if on {
                n.profile.as_ref().is_some_and(|profile| wifi_connect(&n.iface, profile))
            } else {
                wifi_disconnect(&n.iface, &n.ssid)
            }
        });
        if accepted {
            self.switch = Some((key, on, Instant::now() + Duration::from_secs(15)));
            self.read(shared);
        } else {
            shared.update(|r| {
                r.wifi_busy = None;
                r.wifi_failed = Some((key, Instant::now()));
            });
        }
    }
}

pub(super) fn radio_worker(shared: &Shared, rx: Receiver<RadioCmd>, tx: Sender<RadioCmd>) {
    if cfg!(test) { return; }
    let access = wait(Radio::RequestAccessAsync(), 5000, shared, Kind::Radio, "radio access");
    if shared.quitting() {
        return;
    }
    let allowed = access == Some(RadioAccessStatus::Allowed);
    if !allowed {
        crate::log!("control: radio access not allowed ({:?})", access.map(|a| a.0));
    }
    let (mut wifi, mut bt) = (None, None);
    let list = wait(Radio::GetRadiosAsync(), 5000, shared, Kind::Radio, "radio list");
    // No list (Windows would not say): airplane mode is still offered.
    let any = list.as_ref().map(|l| l.Size().unwrap_or(0) > 0).unwrap_or(true);
    shared.update(|r| r.radios = Some(any));
    if let Some(list) = list {
        for i in 0..list.Size().unwrap_or(0) {
            let Ok(r) = list.GetAt(i) else { continue };
            match r.Kind() {
                Ok(RadioKind::WiFi) if wifi.is_none() => wifi = Some(r),
                Ok(RadioKind::Bluetooth) if bt.is_none() => bt = Some(r),
                _ => {}
            }
        }
    }
    if shared.quitting() {
        return;
    }
    // Removed however this function ends.
    let mut _subs = Vec::new();
    for r in [&wifi, &bt].into_iter().flatten() {
        let tx = tx.clone();
        let handler = TypedEventHandler::<Radio, windows::core::IInspectable>::new(move |_, _| {
            let _ = tx.send(RadioCmd::Changed);
            Ok(())
        });
        if let Ok(t) = r.StateChanged(&handler) {
            let r = r.clone();
            _subs.push(Defer::new(move || {
                let _ = r.RemoveStateChanged(t);
            }));
        }
    }
    // Per radio (Wi-Fi, Bluetooth): Windows refused us.
    let mut denied = [!allowed, !allowed];
    let view = |r: &Option<Radio>, denied: bool| match r {
        None => RadioView::Missing,
        Some(r) => view_of(r.State().ok(), denied),
    };
    // Switches Windows accepted and the radio does not show yet (Wi-Fi,
    // Bluetooth).
    let mut switches: [Option<Switch>; 2] = [None, None];
    // A switch on its way shows its target (no flicker back while Windows
    // works on it); the Wi-Fi view as Windows has it is returned.
    let report = |denied: &[bool; 2], switches: &[Option<Switch>; 2]| {
        let (w, b) = (view(&wifi, denied[0]), view(&bt, denied[1]));
        let shown = |v: RadioView, s: &Option<Switch>| match (v, s) {
            (RadioView::On | RadioView::Off, Some(s)) if !s.timed_out => if s.target { RadioView::On } else { RadioView::Off },
            _ => v,
        };
        let (sw, sb) = (shown(w, &switches[0]), shown(b, &switches[1]));
        shared.update(|r| {
            r.wifi = sw;
            r.bt = sb;
        });
        w
    };
    // Each switch looked at again (a state change, or its time is up).
    let follow = |switches: &mut [Option<Switch>; 2], denied: &[bool; 2]| {
        let now = std::time::Instant::now();
        for (k, slot) in switches.iter_mut().enumerate() {
            let Some(s) = slot.as_mut() else { continue };
            let radio = if k == 0 { &wifi } else { &bt };
            match s.seen(switch_state(view(radio, denied[k])), now) {
                SwitchStep::Wait | SwitchStep::Quiet => {}
                SwitchStep::Done => {
                    *slot = None;
                    shared.update(|r| {
                        r.radio_busy[k] = false;
                        r.radio_failed[k] = None;
                    });
                }
                SwitchStep::Failed => {
                    crate::log!("control: radio {k} -> {} not shown within {} s; the tile says so", s.target, SWITCH_WAIT.as_secs());
                    shared.update(|r| {
                        r.radio_busy[k] = false;
                        r.radio_failed[k] = Some(now);
                    });
                }
            }
        }
    };
    let mut wifi_was = report(&denied, &switches);
    let mut networks = Networks::default();
    if wifi_was.on() {
        networks.read(shared);
    }
    loop {
        // Woken by a command, or when the next switch's time is up.
        let due = switches.iter().flatten().filter(|s| !s.timed_out).map(|s| s.deadline).chain(networks.next).min();
        let cmd = match due {
            Some(d) => match rx.recv_timeout(d.saturating_duration_since(std::time::Instant::now())) {
                Ok(c) => Some(c),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => None,
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            },
            None => match rx.recv() {
                Ok(c) => Some(c),
                Err(_) => break,
            },
        };
        if shared.quitting() {
            break;
        }
        if matches!(cmd, Some(RadioCmd::Changed)) {
            shared.airplane_refresh.store(true, Ordering::Release);
        }
        match cmd {
            Some(RadioCmd::Quit) => break,
            Some(RadioCmd::Changed) | None => {
                follow(&mut switches, &denied);
                let now = report(&denied, &switches);
                networks.radio_changed(shared, wifi_was, now);
                wifi_was = now;
            }
            Some(RadioCmd::Ssid | RadioCmd::WifiList) => {
                if wifi_was.on() {
                    networks.read(shared);
                }
            }
            Some(RadioCmd::WifiSwitch(key, on)) => networks.switch(shared, key, on),
            Some(RadioCmd::Set(which, on)) => {
                let (radio, k) = if which == Which::Wifi { (&wifi, 0) } else { (&bt, 1) };
                // A new switch replaces one still followed (or a late one).
                switches[k] = None;
                let mut refused = radio.is_none();
                let mut accepted = false;
                if let Some(r) = radio {
                    let state = if on { RadioState::On } else { RadioState::Off };
                    match wait(r.SetStateAsync(state), 10_000, shared, Kind::Radio, "radio switch") {
                        Some(RadioAccessStatus::Allowed) => accepted = true,
                        Some(s) if s == RadioAccessStatus::DeniedByUser || s == RadioAccessStatus::DeniedBySystem => {
                            // Not a toggle that keeps snapping back: show it
                            // as unavailable.
                            crate::log!("control: {which:?} -> {on}: access denied ({})", s.0);
                            denied[k] = true;
                        }
                        other => {
                            crate::log!("control: {which:?} -> {on}: {:?}", other.map(|s| s.0));
                            refused = true;
                        }
                    }
                }
                // Accepted is not done: followed until the radio's state
                // shows it (StateChanged) or SWITCH_WAIT is over. Refused
                // (or no answer): said at once, the tile goes back.
                if accepted {
                    switches[k] = Some(Switch::new(on, std::time::Instant::now()));
                    follow(&mut switches, &denied);
                } else {
                    let failed = refused && !denied[k];
                    if failed {
                        crate::log!("control: {which:?} -> {on} refused; the tile says so");
                    }
                    shared.update(|r| {
                        r.radio_busy[k] = false;
                        r.radio_failed[k] = failed.then(std::time::Instant::now);
                    });
                }
                let now = report(&denied, &switches);
                networks.radio_changed(shared, wifi_was, now);
                wifi_was = now;
                // An attempted switch also cleared the UI's list. Restore it
                // if the adapter is still on and there is no read pending.
                if which == Which::Wifi && now.on() && networks.next.is_none()
                    && shared.remote.lock().is_ok_and(|r| r.wifi_networks.is_none())
                {
                    networks.read(shared);
                }
            }
        }
        if networks.next.is_some_and(|at| at <= Instant::now()) {
            networks.read(shared);
        }
    }
}
