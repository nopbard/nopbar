//! Hit region, mouse handling, click actions, context menu, shutdown.

use super::*;

impl Dock {
    // ---- input -------------------------------------------------------------

    pub(super) fn hit(&self, x: f32) -> Option<usize> {
        self.items
            .iter()
            .position(|it| it.clickable() && x >= it.dx - self.look.gap / 2.0 && x < it.dx + it.w + self.look.gap / 2.0)
    }

    /// Limits the window's hit area. At rest it is the bar; while the mouse
    /// is on the dock, anything animates, what the dock opened holds the
    /// magnification (6.2 open: its cards, its stack, a menu) or an icon is
    /// still drawn magnified, it grows to the full window height over the
    /// bar's span, so magnified icons, bounces and labels are not clipped
    /// and stay clickable (layout::region_grown). It only grows while
    /// expanded and shrinks back once the panel is closed and the icons
    /// have shrunk back.
    /// (HTTRANSPARENT cannot be used: it only passes clicks to windows of the
    /// same thread.)
    pub(super) fn apply_region(&mut self) {
        let f = self.frame;
        let b = self.bar_rect;
        if self.hidden && !self.strip_ready {
            // Sliding out: keep the old region so the slide is not clipped;
            // slide_done() switches to the strip.
            return;
        }
        let px = if self.hidden {
            // Auto-hidden: only a thin strip on the screen edge, along the
            // bar, is live (not snapped: it must stay thin).
            self.region_expanded = false;
            autohide_strip(f, b, self.scale)
        } else {
            let magnified = self.items.iter().any(|it| !it.is_line() && it.mag > 1.0);
            let expanded = layout::region_grown(self.mouse.is_some(), self.ticker.is_animating(), self.held, magnified);
            let r = layout::region_rect(b, self.along, f.depth, self.dragging(), expanded);
            let px = region_px(f.to_win(r), self.scale);
            // While expanded the region only grows (no rebuild per frame).
            let px = if expanded && self.region_expanded {
                let o = self.region;
                (px.0.min(o.0), px.1.min(o.1), px.2.max(o.2), px.3.max(o.3))
            } else {
                px
            };
            self.region_expanded = expanded;
            px
        };
        if px == self.region {
            return;
        }
        self.region = px;
        unsafe {
            let rgn = CreateRectRgn(px.0, px.1, px.2, px.3);
            // The system owns the region after this call.
            SetWindowRgn(self.hwnd, Some(rgn), false);
        }
    }

    // ---- auto-hide & fullscreen ----------------------------------------------

    /// Slide the dock away (true) or back (false). The compositor animates
    /// the slide; the hit region switches at once.
    pub(super) fn set_hidden(&mut self, hidden: bool) {
        if self.hidden == hidden {
            return;
        }
        self.hidden = hidden;
        self.companion_ground();
        if hidden {
            self.publish_minrects(Vec::new());
        }
        self.strip_ready = false;
        let d = if hidden { self.frame.depth - self.bar_rect.y + 4.0 } else { 0.0 };
        let (x, y) = self.frame.away(d);
        let _ = self.slide.SetOffset(Vector3 { X: x, Y: y, Z: 0.0 });
        // The shadow slides with it, far enough that none of it stays.
        self.shadow.slide_to(x, y, self.scale);
        // Its light (7.2 rules 2 and 3): the rim and the shadows come up
        // from nothing on the slide's own GLIDE as the dock comes back,
        // and go with it over the closing fade.
        let c = &self.gfx.compositor;
        let _ = crate::motion::light(c, &self.highlight_v, !hidden);
        let _ = crate::motion::light(c, self.shadow.sprite(), !hidden);
        unsafe {
            if hidden {
                self.hide_label();
                SetTimer(Some(self.hwnd), SLIDE_DONE_TIMER, crate::effects::ms(SLIDE_MS) as u32 + 30, None);
            } else {
                let _ = KillTimer(Some(self.hwnd), SLIDE_DONE_TIMER);
                self.restack();
            }
        }
        self.apply_region();
        self.sync_edge_cursor();
    }

    /// The hide slide is over: only the edge strip stays live.
    pub(super) fn slide_done(&mut self) {
        if self.hidden {
            self.strip_ready = true;
            self.apply_region();
            self.sync_edge_cursor();
        }
    }

    fn edge_cursor_active(&self) -> bool {
        self.hidden && self.strip_ready && !self.fullscreen && !session::idle()
            && !SHUTTING_DOWN.load(Ordering::Relaxed)
    }

    pub(super) fn sync_edge_cursor(&self) {
        unsafe {
            if self.edge_cursor_active() {
                SetTimer(Some(self.hwnd), EDGE_CURSOR_TIMER, EDGE_CURSOR_MS, None);
            } else {
                let _ = KillTimer(Some(self.hwnd), EDGE_CURSOR_TIMER);
            }
        }
    }

    /// The strip may be under another window and receive no mouse messages.
    pub(super) fn poll_edge_cursor(&mut self) {
        if !self.edge_cursor_active() {
            return;
        }
        let mut cursor = POINT::default();
        unsafe {
            if GetCursorPos(&mut cursor).is_err() || !edge_contains(self.origin, self.region, cursor)
                || sensors::button_down()
            {
                return;
            }
            self.refresh_cover();
            if self.fullscreen {
                return;
            }
            self.set_hidden(false);
            // A stationary cursor may never generate a leave event for this window.
            SetTimer(Some(self.hwnd), AUTOHIDE_TIMER, self.cfg.dock.autohide_delay(), None);
        }
    }

    /// AUTOHIDE_TIMER fired: hide unless the mouse came back.
    /// False (the caller re-arms the timer) while a slip of the pointer or
    /// the Settings window should keep the dock up.
    pub(super) fn autohide_now(&mut self) -> bool {
        // Never while something is dragged on or over it.
        if !(self.cfg.dock.autohide && self.mouse.is_none() && self.pressed.is_none() && !self.dragging() && !self.ole_drag) {
            return true;
        }
        // The dock stays up while one of its menus is open
        // (an app's right-click menu, a stack) or its window cards are,
        // wherever the pointer goes; it hides once they are closed and the
        // pointer is away.
        if self.start_hidden.is_some() || crate::popup::is_tracking() || preview::is_open()
            || unsafe { self.pointer_in_grace() || settings_in_front() }
        {
            return false;
        }
        self.set_hidden(true);
        true
    }

    /// The pointer is still just beside the dock (a slip while aiming, or
    /// moving between the dock and its menus): AUTOHIDE_GRACE_DIP around it.
    unsafe fn pointer_in_grace(&self) -> bool {
        let mut p = POINT::default();
        let mut win = RECT::default();
        if GetCursorPos(&mut p).is_err() || GetWindowRect(self.hwnd, &mut win).is_err() {
            return false;
        }
        // The visible bar (not the whole window, which spans the edge).
        let b = self.frame.to_win(self.bar_rect);
        let s = self.scale;
        let bar = RECT {
            left: win.left + (b.x * s).floor() as i32,
            top: win.top + (b.y * s).floor() as i32,
            right: win.left + ((b.x + b.w) * s).ceil() as i32,
            bottom: win.top + ((b.y + b.h) * s).ceil() as i32,
        };
        let g = (AUTOHIDE_GRACE_DIP * s).round() as i32;
        grace_contains(bar, g, p)
    }

    /// Lie behind the foreground window while it covers our monitor
    /// (games, video, F11). Shell notices (ABN_FULLSCREENAPP), activation
    /// and foreground moves only trigger this check; the decision is always
    /// ours.
    pub(super) fn check_fullscreen(&mut self) {
        if self.primary {
            // Each monitor's top bar decides for itself.
            if !crate::topbar::refresh_fullscreen() {
                schedule_fullscreen_check();
            }
            // Hot corners stop listening while every cornered screen is fullscreen.
            crate::corners::refresh_fullscreen();
            // And the follow-the-mouse sensors on the other monitors.
            unsafe { sensors::refresh_fullscreen() };
        }
        self.refresh_cover();
    }

    /// Our own monitor only: a fullscreen app in front of it covers the
    /// dock (as it covers the Windows taskbar) instead of hiding it; it is
    /// never hidden, so a wrong guess leaves it under one window, not gone.
    /// Meanwhile it rests: no minimize targets, no hover, magnification,
    /// labels, cards, stack or keyboard mode, nothing for screen readers.
    pub(super) fn refresh_cover(&mut self) {
        let front = unsafe { GetForegroundWindow() };
        let start = crate::keys::start_monitor(front);
        let app = if start.is_some() { crate::topbar::last_app_window() } else { front };
        let full = unsafe { window_covers_monitor(app, self.hwnd, Some(self.mon), self.monitor) };
        let show_with_start = full && start == Some(self.mon);
        let hidden = start_visibility(show_with_start, self.hidden, self.cfg.dock.autohide, &mut self.start_hidden);
        let covered = full && !show_with_start;
        let anchor = if covered { app } else { HWND::default() };
        if covered == self.fullscreen && anchor == self.fullscreen_window && hidden.is_none() {
            // Topmost, yet Windows sometimes leaves us under a normal window
            // (seen after fullscreen apps): the edge strip is then out of
            // reach and the dock never comes up. Back on top.
            if !covered && unsafe { sunk(self.hwnd, false) } {
                unsafe { self.restack() };
            }
            return;
        }
        let changed = self.fullscreen != covered;
        self.fullscreen = covered;
        self.fullscreen_window = anchor;
        if let Some(hidden) = hidden {
            self.set_hidden(hidden);
        }
        if changed {
            self.sync_edge_cursor();
        }
        self.companion_ground();
        if changed {
            if covered {
                crate::log!("fullscreen app: true ({})", unsafe { window_summary(anchor) });
                self.publish_minrects(Vec::new());
                self.rest_under_cover();
            } else {
                crate::log!("fullscreen app: false");
            }
        }
        unsafe { self.restack() };
    }

    /// The dock is built: it goes to its place in the z-order (behind a
    /// fullscreen app already in front of its monitor, else on top) and is
    /// shown, then its shadow right under it. Nothing of it is shown before
    /// (BarShadow starts held), so nothing flashes over a game.
    pub(super) unsafe fn reveal(&mut self) {
        self.refresh_cover();
        self.restack();
        let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
        self.shadow.release(self.hwnd);
    }

    /// Everything hover and focus opened closes (the pointer left, as far
    /// as the dock is concerned): nothing of ours stays over the app.
    fn rest_under_cover(&mut self) {
        if !self.dragging() && (self.mouse.is_some() || self.hover_item.is_some()) {
            self.on_mouse_leave();
        }
        self.reset_label();
        if views::owns_preview(self.hwnd) {
            preview::hide();
        }
        if views::owns_stack(self.hwnd) {
            stack::cancel();
        }
        // And one asked for but not open yet (its listing still on the way).
        self.drop_stack_request();
        unsafe { self.leave_keyboard(false) };
    }

    /// The dock's place in the z-order, and its shadow's right under it:
    /// above everything, or right behind the fullscreen app in front of its
    /// monitor (see z_place).
    pub(super) unsafe fn restack(&self) {
        let flags = SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE;
        let insert = z_after_window(self.fullscreen, self.hwnd, self.fullscreen_window);
        let _ = SetWindowPos(self.hwnd, Some(insert), 0, 0, 0, 0, flags);
        self.shadow.restack(self.hwnd);
    }

    /// First move after the cursor entered: arm WM_MOUSELEAVE.
    pub(super) unsafe fn on_mouse_enter(&mut self) {
        let mut tme = TRACKMOUSEEVENT {
            cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
            dwFlags: TME_LEAVE,
            hwndTrack: self.hwnd,
            dwHoverTime: 0,
        };
        let _ = TrackMouseEvent(&mut tme);
        self.tracking = true;
        schedule_refresh();
        self.sound_hover(true);
    }

    pub(super) fn on_mouse_leave(&mut self) {
        if self.dragging() {
            // The pointer may leave while dragging (captured); keep going.
            return;
        }
        self.tracking = false;
        self.mouse = None;
        self.sound_hover(false);
        self.reset_label();
        self.hover_item = None;
        self.hover_app = None;
        unsafe {
            let _ = KillTimer(Some(self.hwnd), PREVIEW_TIMER);
        }
        preview::schedule_hide();
        if self.cfg.dock.autohide {
            unsafe {
                SetTimer(Some(self.hwnd), AUTOHIDE_TIMER, self.cfg.dock.autohide_delay(), None);
            }
        }
        if let Some((i, _)) = self.pressed.take() {
            self.let_go(i);
        }
        self.update_targets();
    }

    pub(super) fn on_mouse_move(&mut self, x: f32, y: f32) {
        if self.dragging() {
            self.mouse = Some((x, y));
            self.drag_move(x, y);
            self.update_targets();
            return;
        }
        self.maybe_start_drag(x, y);
        if self.dragging() {
            return;
        }
        self.mouse = Some((x, y));
        if self.hidden {
            unsafe {
                let _ = KillTimer(Some(self.hwnd), AUTOHIDE_TIMER);
            }
            self.set_hidden(false);
        }
        // Grow the region before magnification starts so nothing is clipped.
        self.apply_region();
        self.update_targets();
        self.preview_hover(self.hit(x));
        // Ctrl+Z puts a pin removed a moment ago back (only while here).
    }

    /// Previews follow the pointer: after a rest on a running app, and at
    /// once from one app to the next while they are open.
    fn preview_hover(&mut self, at: Option<usize>) {
        if at == self.hover_item || self.ole_drag {
            return;
        }
        self.hover_item = at;
        self.hover_app = at.map(|i| self.items[i].path.clone());
        unsafe {
            let _ = KillTimer(Some(self.hwnd), PREVIEW_TIMER);
        }
        let running = at.filter(|&i| self.items[i].kind == Kind::App && !self.items[i].windows.is_empty());
        match running {
            Some(i) if preview::is_open() => self.show_preview(i),
            Some(_) => unsafe {
                SetTimer(Some(self.hwnd), PREVIEW_TIMER, PREVIEW_DELAY_MS, None);
            },
            None => {}
        }
    }

    /// Opens the window cards for item `i` (next to its icon).
    pub(super) fn show_preview(&mut self, i: usize) {
        let Some(it) = self.items.get(i) else { return };
        if it.kind != Kind::App || it.windows.is_empty() {
            return;
        }
        // Same app with the same windows already open: leave it.
        if preview::open_for().as_deref() == Some(it.path.as_str()) && preview::windows() == it.windows {
            return;
        }
        // The popup gap off the dock, past the bar and the tallest a
        // magnified icon gets (it stays put as the pointer moves).
        let gap = crate::placement::popup_gap(self.scale);
        let anchor = super::menu::menu_point(self.frame.pos, self.bar_screen_rect(), Some(self.peak_rect(i)), POINT::default(), gap);
        let work = crate::monitors::info(self.mon).map(|m| m.work).unwrap_or(self.monitor);
        let (path, wins) = (it.path.clone(), it.windows.clone());
        let opts = preview::Opts {
            icon: it.large_brush(),
            peek: self.cfg.dock.preview_peek,
            pop: crate::effects::items().preview_pop,
        };
        views::previews_from(self.hwnd);
        unsafe { preview::show(&self.gfx, &self.theme, self.scale, &path, &wins, anchor, self.frame.pos, work, opts) };
        self.hide_label();
    }

    pub(super) fn on_down(&mut self, x: f32, y: f32, button: u32) {
        // The pointer takes over from the keyboard: keyboard mode (and its
        // ring) ends (crate::input).
        if self.kb.is_some() {
            unsafe { self.leave_keyboard(false) };
        }
        self.pressed = self.hit(x).map(|i| (i, button));
        self.press_at = Some((x, y));
        self.press_path = self.pressed.map(|(i, _)| self.items[i].path.clone());
        if self.pressed.is_some() {
            // Dimmed at once (layout: the pressed dim is one of its inputs,
            // so no frame of the magnification undoes it).
            self.press_back = None;
            self.layout();
        }
    }

    /// The press on icon `i` is over: its dim eases back (press_dim).
    pub(super) fn let_go(&mut self, i: usize) {
        self.press_back = Some((i, Instant::now()));
        self.kick();
    }

    /// Button released. Right clicks return what the menu is for (item or
    /// empty area, and whether Shift was held); the caller shows it outside
    /// the dock's borrow.
    pub(super) unsafe fn on_up(&mut self, x: f32, y: f32, button: u32) -> Option<(Option<usize>, bool)> {
        use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, VK_CONTROL, VK_SHIFT};
        let shift = GetKeyState(VK_SHIFT.0 as i32) < 0;
        let ctrl = GetKeyState(VK_CONTROL.0 as i32) < 0;
        self.press_at = None;
        // The end of a drag is not a click.
        if button == WM_LBUTTONUP && self.end_drag(false, Some((x, y))) {
            self.pressed = None;
            return None;
        }
        let Some((i, b)) = self.pressed.take() else {
            return (button == WM_RBUTTONUP).then_some((None, shift));
        };
        self.let_go(i);
        if b != button || self.hit(x) != Some(i) || self.press_path.take().as_deref() != Some(self.items[i].path.as_str()) {
            return None;
        }
        match button {
            // Windows' taskbar conventions: Shift = new instance,
            // Ctrl+Shift = as administrator.
            WM_LBUTTONUP if shift && ctrl && self.items[i].kind == Kind::App && !self.items[i].stack => {
                let p = self.items[i].path.clone();
                self.run_action(menu::DockAction::RunAdmin(p));
            }
            WM_LBUTTONUP if shift => {
                preview::hide();
                self.launch(i);
            }
            // Several windows: pick one from the cards (Ctrl+click cycles,
            // like the taskbar).
            WM_LBUTTONUP if !ctrl && self.items[i].kind == Kind::App && self.items[i].windows.len() > 1 => {
                if preview::open_for().as_deref() == Some(self.items[i].path.as_str()) {
                    preview::hide();
                } else {
                    self.show_preview(i);
                }
            }
            WM_LBUTTONUP => {
                preview::hide();
                self.activate(i);
            }
            // A new instance (the taskbar's way), or with "Middle-click on
            // a dock icon: Close the app's windows" a close request to each
            // of its windows (dock.middle_click).
            // Windows in the tray are never closed from here.
            WM_MBUTTONUP => match middle_click(
                self.cfg.dock.middle_click_closes(),
                self.items[i].kind == Kind::App && !self.items[i].stack,
                !self.items[i].windows.is_empty(),
                !self.items[i].tray.is_empty(),
            ) {
                Middle::Close => {
                    preview::hide();
                    let p = self.items[i].path.clone();
                    self.run_action(menu::DockAction::CloseAll(p));
                }
                Middle::Launch => self.launch(i),
                Middle::Nothing => {}
            },
            WM_RBUTTONUP => {
                preview::hide();
                // The label goes in the frame the menu opens (7.4), at
                // once (not the pointer leaving's fade).
                self.cut_label();
                return Some((Some(i), shift));
            }
            _ => {}
        }
        None
    }

    pub(super) unsafe fn activate(&mut self, i: usize) {
        match self.items[i].kind {
            Kind::Start => crate::keys::win(),
            Kind::Sep | Kind::Divider => {}
            Kind::Trash => {
                crate::launch::open_soon(crate::launch::Target::new("shell:RecycleBinFolder", "Recycle Bin", crate::launch::Source::Dock));
            }
            Kind::App if self.items[i].stack => self.open_stack(i),
            Kind::App => {
                let wins = self.items[i].windows.clone();
                if wins.is_empty() && !self.items[i].tray.is_empty() {
                    // Its only windows are in the tray: bring them back.
                    let tray = self.items[i].tray.clone();
                    crate::traywin::restore_many(&tray);
                    return;
                }
                if wins.is_empty() {
                    self.launch(i);
                    return;
                }
                let fg = GetForegroundWindow();
                crate::log!("activate {} wins={:?} fg={:?}", self.items[i].name, wins, fg);
                match wins.iter().position(|&h| h == fg) {
                    // Focused single window: minimize, like the taskbar.
                    Some(_) if wins.len() == 1 => self.minimize_from_dock(fg),
                    // Several windows and one is focused: cycle to the next.
                    Some(p) => focus(wins[(p + 1) % wins.len()]),
                    None => focus(wins[0]),
                }
            }
        }
    }

    /// Dock-started minimizes get the genie unless it is off in the config
    /// or Windows' animation effects are off.
    pub(super) fn genie_on(&self) -> bool {
        crate::effects::items().minimize == crate::config::Minimize::Genie && motion()
    }

    /// Minimize `h` for a dock click or menu: the genie (DWM thumbnails),
    /// else as minimize_plain().
    pub(super) unsafe fn minimize_from_dock(&self, h: HWND) {
        let taken = self.genie_on() && genie_thumb::minimize(h, self.frame.pos);
        if !taken {
            self.minimize_plain(h);
        }
    }

    /// Every other dock minimize (e.g. the other windows of "Minimize all",
    /// which never get a genie): no animation for "None", else the posted
    /// SC_MINIMIZE (OS animation into the icon via GETMINRECT). The top
    /// bar's minimize items are not the dock's and keep the OS animation.
    pub(super) unsafe fn minimize_plain(&self, h: HWND) {
        let quiet = crate::effects::items().minimize == crate::config::Minimize::None && genie_thumb::minimize_instant(h);
        if !quiet {
            let _ = PostMessageW(Some(h), WM_SYSCOMMAND, WPARAM(SC_MINIMIZE as usize), LPARAM(0));
        }
    }

    /// The front-most visible, not minimized window of item `i`'s app.
    pub(super) fn top_window(&self, i: usize) -> Option<HWND> {
        let wins = &self.items.get(i)?.windows;
        unsafe {
            let mut h = GetTopWindow(None).ok()?;
            for _ in 0..4096 {
                if wins.contains(&h) && IsWindowVisible(h).as_bool() && !IsIconic(h).as_bool() {
                    return Some(h);
                }
                h = GetWindow(h, GW_HWNDNEXT).ok()?;
            }
        }
        None
    }

    pub(super) fn launch(&mut self, i: usize) {
        if self.items[i].kind != Kind::App || self.items[i].path.is_empty() {
            return;
        }
        if crate::launchpad::is_pin(&self.items[i].path) {
            // Opens a moment later, outside the dock's borrow.
            crate::launchpad::post_open(Some(self.mon));
            return;
        }
        let it = &mut self.items[i];
        if it.windows.is_empty() && !it.stack && motion() && crate::effects::items().bounce {
            it.bounce = Some(Instant::now());
        }
        // A broken shortcut or a moved app is told next to its icon.
        let source = if it.pinned { crate::launch::Source::DockPin(it.name.clone(), it.path.clone()) } else { crate::launch::Source::Dock };
        let target = if it.pinned { crate::launch::Target::for_pin(&it.path, &it.name, source, it.id.aumid.as_deref()) } else { crate::launch::Target::new(&it.path, &it.name, source) };
        let (at, align) = (self.menu_anchor(i), self.menu_align());
        if !crate::launch::open_at(&target, at, align) {
            self.items[i].bounce = None;
        }
        self.kick();
    }

    pub(super) unsafe fn on_appbar(&mut self, code: u32, lp: LPARAM) {
        if code == ABN_POSCHANGED {
            self.place_window();
        } else if code == ABN_FULLSCREENAPP {
            // Get out of the way of games / video.
            let _ = lp;
            schedule_fullscreen_check();
        }
    }

    pub(super) unsafe fn shutdown(&mut self) {
        if !self.primary {
            // A display's dock: its AppBar, hotkeys and Split View hint only.
            split::teardown(self.hwnd.0 as isize);
            let _ = windows::Win32::UI::Input::KeyboardAndMouse::UnregisterHotKey(Some(self.hwnd), drag::ESC_HOTKEY);
            if self.appbar {
                taskbar::appbar_remove(self.hwnd);
                self.appbar = false;
            }
            return;
        }
        // Our tray icon first, while the top bar's tray spy still forwards.
        super::trayicon::remove();
        if self.dragging() {
            self.end_drag(true, None);
        }
        split::teardown(self.hwnd.0 as isize);
        split::shutdown();
        genie_thumb::shutdown();
        // Keys held for a drag or an undo go back to the other apps.
        let _ = windows::Win32::UI::Input::KeyboardAndMouse::UnregisterHotKey(Some(self.hwnd), drag::ESC_HOTKEY);
        let watch = TRASH_WATCH.swap(0, Ordering::SeqCst);
        if watch != 0 {
            let _ = windows::Win32::UI::Shell::SHChangeNotifyDeregister(watch);
        }
        // Stop re-hiding before showing the taskbar, or our own SHOW hook
        // would hide it again.
        HIDE_TASKBAR.store(false, Ordering::SeqCst);
        let _ = KillTimer(Some(self.hwnd), REHIDE_TIMER);
        crate::topbar::shutdown();
        if self.appbar {
            taskbar::appbar_remove(self.hwnd);
            self.appbar = false;
        }
        taskbar::restore();
    }
}

/// What a middle click on a dock icon does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Middle {
    Launch,
    Close,
    Nothing,
}

/// Pure: a middle click on an icon (`app`: an app, not a stack / the
/// trash / Start) with windows of its own (`windows`) or only in the tray
/// (`tucked`), the setting set to close (`closes`). It closes the app's
/// windows only; with only tray windows it does nothing (they are never
/// closed from the dock, and a second copy is not what was asked).
/// Nothing running: a new window, as before.
pub(super) fn middle_click(closes: bool, app: bool, windows: bool, tucked: bool) -> Middle {
    match (closes, app, windows, tucked) {
        (true, true, true, _) => Middle::Close,
        (true, true, false, true) => Middle::Nothing,
        _ => Middle::Launch,
    }
}

/// Restores (if minimized) and fronts another app's window. Nothing here
/// waits on that app (audit A02): the restore is posted and a helper waits
/// for it; a hung window is left alone. See snap::bring.
pub(super) unsafe fn focus(h: HWND) {
    // WM_SNAP_DONE needs no dock state: the primary takes it.
    let dock = HWND(DOCK_HWND.load(Ordering::SeqCst) as *mut _);
    snap::bring(h, dock, WM_SNAP_DONE);
}

/// Where a bar of ours goes in the z-order (pure, tested).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ZPlace {
    /// Above everything (the normal place).
    Topmost,
    /// Right under window `.0`, the fullscreen app in front (out of the
    /// topmost band in the same step: no frame with us over it).
    Behind(isize),
    /// The top of the normal band: under every topmost window, the
    /// fullscreen app included when it is topmost itself.
    NoTopmost,
}

/// The place of a bar of ours (`me`): topmost, or with a fullscreen app in
/// front (`covered`), right behind the foreground window `fg` (0 = none);
/// behind every topmost window when `fg` itself is topmost, or when it is
/// ours or gone.
pub(crate) fn z_place(covered: bool, fg: isize, fg_topmost: bool, me: isize) -> ZPlace {
    if !covered {
        ZPlace::Topmost
    } else if fg == 0 || fg == me || fg_topmost {
        ZPlace::NoTopmost
    } else {
        ZPlace::Behind(fg)
    }
}

/// z_place for window `me` now, as SetWindowPos's insert-after.
pub(crate) unsafe fn z_after(covered: bool, me: HWND) -> HWND {
    z_after_window(covered, me, GetForegroundWindow())
}

unsafe fn z_after_window(covered: bool, me: HWND, fg: HWND) -> HWND {
    let fg_topmost = !fg.is_invalid() && (GetWindowLongW(fg, GWL_EXSTYLE) as u32 & WS_EX_TOPMOST.0) != 0;
    match z_place(covered, fg.0 as isize, fg_topmost, me.0 as isize) {
        ZPlace::Topmost => HWND_TOPMOST,
        ZPlace::NoTopmost => HWND_NOTOPMOST,
        ZPlace::Behind(h) => HWND(h as *mut _),
    }
}

fn start_visibility(show: bool, hidden: bool, autohide: bool, saved: &mut Option<bool>) -> Option<bool> {
    if show {
        saved.get_or_insert(hidden);
        hidden.then_some(false)
    } else {
        saved.take().filter(|was_hidden| *was_hidden && autohide && !hidden)
    }
}

/// A shown (visible, not cloaked) window that is not topmost lies above
/// `me` in the z-order, although `me` is topmost.
pub(crate) unsafe fn sunk(me: HWND, topbar: bool) -> bool {
    use windows::Win32::UI::WindowsAndMessaging::GW_HWNDPREV;
    let mut own = RECT::default();
    if GetWindowRect(me, &mut own).is_err() {
        return false;
    }
    let mut h = GetWindow(me, GW_HWNDPREV).unwrap_or_default();
    for _ in 0..4096 {
        if h.is_invalid() {
            break;
        }
        let topmost = (GetWindowLongW(h, GWL_EXSTYLE) as u32 & WS_EX_TOPMOST.0) != 0;
        let mut rect = RECT::default();
        if !topmost && IsWindowVisible(h).as_bool() && !IsIconic(h).as_bool()
            && GetWindowRect(h, &mut rect).is_ok()
            && overlaps(own, rect) && !crate::apps::is_cloaked(h)
        {
            log_sunk(h, topbar);
            return true;
        }
        h = GetWindow(h, GW_HWNDPREV).unwrap_or_default();
    }
    false
}

fn overlaps(a: RECT, b: RECT) -> bool {
    a.left.max(b.left) < a.right.min(b.right) && a.top.max(b.top) < a.bottom.min(b.bottom)
}

fn restack_log_due(last: Option<std::time::Instant>, now: std::time::Instant) -> bool {
    last.is_none_or(|last| now.duration_since(last) >= std::time::Duration::from_secs(30))
}

unsafe fn log_sunk(blocker: HWND, topbar: bool) {
    thread_local! {
        static LAST: std::cell::Cell<[Option<std::time::Instant>; 2]> = const { std::cell::Cell::new([None; 2]) };
    }
    let now = std::time::Instant::now();
    let due = LAST.with(|cell| {
        let mut last = cell.get();
        let i = usize::from(topbar);
        let due = restack_log_due(last[i], now);
        if due {
            last[i] = Some(now);
            cell.set(last);
        }
        due
    });
    if due {
        let name = if topbar { "topbar" } else { "dock" };
        crate::log!("{name}: restacking above {}", window_identity(blocker));
    }
}

unsafe fn window_identity(hwnd: HWND) -> String {
    let exe = crate::apps::exe_of_window(hwnd).and_then(|p| std::path::Path::new(&p).file_name().map(|n| n.to_string_lossy().into_owned())).unwrap_or_default();
    let mut class = [0u16; 64];
    let n = GetClassNameW(hwnd, &mut class) as usize;
    format!("exe={exe:?} class={:?}", String::from_utf16_lossy(&class[..n]))
}

/// What the foreground window is, for the log: exe name, class, maximized,
/// style and rect (why it was taken for a fullscreen app).
unsafe fn window_summary(fg: HWND) -> String {
    let identity = window_identity(fg);
    let mut r = RECT::default();
    let _ = GetWindowRect(fg, &mut r);
    format!(
        "{identity} zoomed={} style={:#010x} rect=({},{},{},{})",
        IsZoomed(fg).as_bool(),
        GetWindowLongW(fg, GWL_STYLE) as u32,
        r.left,
        r.top,
        r.right,
        r.bottom
    )
}

/// The foreground window covers the whole of monitor `hmon` (its rect when
/// last seen: `mon`) and is not a framed maximized window or the desktop.
pub(super) unsafe fn fullscreen_window(me: HWND, hmon: Option<isize>, mon: RECT) -> bool {
    window_covers_monitor(GetForegroundWindow(), me, hmon, mon)
}

unsafe fn window_covers_monitor(fg: HWND, me: HWND, hmon: Option<isize>, mon: RECT) -> bool {
    if fg.is_invalid() || fg == me || genie_thumb::is_overlay(fg) || !IsWindowVisible(fg).as_bool() || IsIconic(fg).as_bool() {
        return false;
    }
    let mut class = [0u16; 32];
    let n = GetClassNameW(fg, &mut class) as usize;
    let class = String::from_utf16_lossy(&class[..n]);
    // (Launchpad covers the monitor too, but it is ours: bars stay.)
    if matches!(class.as_str(), "Progman" | "WorkerW" | "Shell_TrayWnd" | "Shell_SecondaryTrayWnd" | "NopBarLaunchpad") {
        return false;
    }
    let mut r = RECT::default();
    if GetWindowRect(fg, &mut r).is_err() {
        return false;
    }
    let live = live_monitor(hmon, mon);
    covers_as_fullscreen(r, IsZoomed(fg).as_bool(), GetWindowLongW(fg, GWL_STYLE) as u32, mon, live)
        && !capture_overlay(crate::apps::exe_of_window(fg).as_deref())
}

/// The monitor's rect and work area now. By handle when the caller has
/// one; else the monitor at `mon`, only if it still has exactly that rect
/// (a rect alone does not tell which monitor it came from once the
/// displays changed).
unsafe fn live_monitor(hmon: Option<isize>, mon: RECT) -> Option<(RECT, RECT)> {
    if let Some(m) = hmon.and_then(crate::monitors::info) {
        return Some((m.rect, m.work));
    }
    use windows::Win32::Graphics::Gdi::{MonitorFromRect, MONITOR_DEFAULTTONULL};
    let h = MonitorFromRect(&mon, MONITOR_DEFAULTTONULL);
    if h.is_invalid() {
        return None;
    }
    crate::monitors::info(h.0 as isize).filter(|m| m.rect == mon).map(|m| (m.rect, m.work))
}

/// The decision, from the window's rect, maximized state and style, the
/// monitor rect last seen and the monitor's live (rect, work area).
///
/// - The live rect wins: an exclusive fullscreen game may have changed the
///   display mode since the bar measured it.
/// - A maximized window with a frame (caption or sizing border) is a normal
///   window, even if it fills the monitor (no reserved work area).
/// - A maximized frameless window is a game's "windowed fullscreen" only
///   if it covers more than the work area: with part of the monitor
///   reserved (our top bar, a docked dock) a normal maximized window stops
///   at the work area. Without a reserved strip the two cannot be told
///   apart, and the window is left alone.
pub(crate) fn covers_as_fullscreen(win: RECT, zoomed: bool, style: u32, mon: RECT, live: Option<(RECT, RECT)>) -> bool {
    let rect = live.map(|(r, _)| r).unwrap_or(mon);
    if !edge::covers(win, rect) {
        return false;
    }
    if !zoomed {
        return true;
    }
    if framed(style) {
        return false;
    }
    matches!(live, Some((r, work)) if work != r)
}

/// The style has a caption or a sizing border.
fn framed(style: u32) -> bool {
    (style & WS_CAPTION.0) == WS_CAPTION.0 || (style & WS_THICKFRAME.0) != 0
}

/// DIP rect -> px (x0, y0, x1, y1), edges snapped outward to REGION_SNAP.
pub(super) fn region_px(r: Rect, scale: f32) -> (i32, i32, i32, i32) {
    let down = |v: f32| ((v * scale).floor() as i32).div_euclid(REGION_SNAP) * REGION_SNAP;
    let up = |v: f32| {
        let p = (v * scale).ceil() as i32;
        (p + REGION_SNAP - 1).div_euclid(REGION_SNAP) * REGION_SNAP
    };
    (down(r.x), down(r.y), up(r.x + r.w), up(r.y + r.h))
}

/// How far around the visible dock the pointer may slip before auto-hide
/// goes ahead (DIP).
const AUTOHIDE_GRACE_DIP: f32 = 28.0;

/// `p` lies within `g` px around `r`.
fn grace_contains(r: RECT, g: i32, p: POINT) -> bool {
    p.x >= r.left - g && p.x < r.right + g && p.y >= r.top - g && p.y < r.bottom + g
}

fn autohide_strip(frame: Frame, bar: Rect, scale: f32) -> (i32, i32, i32, i32) {
    let local = frame.to_win(Rect { x: bar.x, y: frame.depth - 1.0, w: bar.w, h: 1.0 });
    let x0 = (local.x * scale).floor() as i32;
    let x1 = ((local.x + local.w) * scale).ceil() as i32;
    let y0 = (local.y * scale).floor() as i32;
    let y1 = ((local.y + local.h) * scale).ceil() as i32;
    let edge = (frame.depth * scale).ceil() as i32;
    let t = AUTOHIDE_STRIP_PX;
    match frame.pos {
        Position::Bottom => (x0, edge - t, x1, edge),
        Position::Left => (0, y0, t, y1),
        Position::Right => (edge - t, y0, edge, y1),
    }
}

fn edge_contains(origin: POINT, strip: (i32, i32, i32, i32), cursor: POINT) -> bool {
    let (x, y) = (cursor.x - origin.x, cursor.y - origin.y);
    let (left, top, right, bottom) = strip;
    left <= x && x < right && top <= y && y < bottom
}

/// The Settings window is in front (someone is tuning the dock: keep it
/// visible so changes can be seen).
unsafe fn settings_in_front() -> bool {
    let Ok(s) = FindWindowW(w!("NopBarSettings"), PCWSTR::null()) else { return false };
    GetForegroundWindow() == s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn region_snaps_outward() {
        let r = Rect { x: 10.2, y: 0.0, w: 100.0, h: 50.5 };
        assert_eq!(region_px(r, 2.0), (16, 0, 224, 112));
    }

    #[test]
    fn region_exact_multiples_unchanged() {
        let r = Rect { x: 8.0, y: 8.0, w: 8.0, h: 8.0 };
        assert_eq!(region_px(r, 2.0), (16, 16, 32, 32));
    }

    #[test]
    fn a_covered_bar_lies_right_behind_the_app_in_front() {
        assert_eq!(z_place(false, 42, false, 7), ZPlace::Topmost);
        assert_eq!(z_place(false, 42, true, 7), ZPlace::Topmost);
        assert_eq!(z_place(true, 42, false, 7), ZPlace::Behind(42));
        // A topmost app: under the whole topmost band, it included.
        assert_eq!(z_place(true, 42, true, 7), ZPlace::NoTopmost);
        // No window in front, or our own: never "after" ourselves.
        assert_eq!(z_place(true, 0, false, 7), ZPlace::NoTopmost);
        assert_eq!(z_place(true, 7, false, 7), ZPlace::NoTopmost);
    }

    #[test]
    fn start_reveals_an_autohidden_dock_and_restores_it_on_close() {
        let mut saved = None;
        assert_eq!(start_visibility(true, true, true, &mut saved), Some(false));
        assert_eq!(saved, Some(true));
        assert_eq!(start_visibility(true, false, true, &mut saved), None);
        assert_eq!(start_visibility(false, false, true, &mut saved), Some(true));
        assert_eq!(saved, None);
        assert_eq!(start_visibility(true, false, true, &mut saved), None);
        assert_eq!(start_visibility(false, false, true, &mut saved), None);
        assert_eq!(start_visibility(true, true, true, &mut saved), Some(false));
        assert_eq!(start_visibility(false, false, false, &mut saved), None);
    }

    #[test]
    fn a_blocker_must_overlap_and_repeated_restack_logs_are_throttled() {
        let own = RECT { left: -100, top: 0, right: 0, bottom: 32 };
        let other_display = RECT { left: 0, top: 0, right: 100, bottom: 40 };
        assert!(!overlaps(own, other_display));
        assert!(overlaps(own, RECT { left: -5, ..other_display }));
        let now = std::time::Instant::now();
        let seconds = std::time::Duration::from_secs;
        assert!(restack_log_due(None, now));
        assert!(!restack_log_due(Some(now), now + seconds(29)));
        assert!(restack_log_due(Some(now), now + seconds(30)));
    }

    #[test]
    fn the_hidden_strip_can_be_hit_in_screen_coordinates_without_a_window_event() {
        let origin = POINT { x: -1_440, y: 200 };
        let bar = Rect { x: 32.0, y: 80.0, w: 64.0, h: 20.0 };
        for pos in [Position::Bottom, Position::Left, Position::Right] {
            let strip = autohide_strip(Frame { pos, depth: 100.0 }, bar, 1.5);
            let (left, top, right, bottom) = strip;
            assert!(edge_contains(origin, strip, POINT { x: origin.x + left, y: origin.y + top }));
            assert!(!edge_contains(origin, strip, POINT { x: origin.x + right, y: origin.y + bottom - 1 }));
            assert!(!edge_contains(origin, strip, POINT { x: origin.x + right - 1, y: origin.y + bottom }));
            let thickness = if pos == Position::Bottom { bottom - top } else { right - left };
            assert_eq!(thickness, AUTOHIDE_STRIP_PX);
        }
    }

    #[test]
    fn only_framed_maximized_windows_are_not_fullscreen() {
        // A browser / Explorer window: caption and sizing border.
        assert!(framed(0x15C7_0000));
        // A dialog-like window with a caption only.
        assert!(framed(0x14C0_0000));
        // A sizing border without a caption.
        assert!(framed(0x1604_0000));
        // A game's borderless window: WS_POPUP | WS_VISIBLE | WS_MAXIMIZE.
        assert!(!framed(0x9100_0000));
        // WS_BORDER alone (half of WS_CAPTION) is not a frame.
        assert!(!framed(0x9080_0000));
    }

    const MON: RECT = RECT { left: 0, top: 0, right: 2560, bottom: 1600 };
    /// The top bar reserves the top 35 px.
    const WORK: RECT = RECT { left: 0, top: 35, right: 2560, bottom: 1600 };
    const POPUP: u32 = 0x9100_0000;
    const FRAMED: u32 = 0x15C7_0000;

    #[test]
    fn a_game_covering_the_monitor_is_fullscreen() {
        assert!(covers_as_fullscreen(MON, false, POPUP, MON, Some((MON, WORK))));
        // A window short of the monitor is not.
        let short = RECT { bottom: 1500, ..MON };
        assert!(!covers_as_fullscreen(short, false, POPUP, MON, Some((MON, WORK))));
    }

    #[test]
    fn maximized_windows_need_more_than_the_work_area() {
        // Windowed fullscreen past the reserved top bar: fullscreen.
        assert!(covers_as_fullscreen(MON, true, POPUP, MON, Some((MON, WORK))));
        // A framed maximized window never is.
        let big = RECT { left: -13, top: -13, right: 2573, bottom: 1613 };
        assert!(!covers_as_fullscreen(big, true, FRAMED, MON, Some((MON, MON))));
        assert!(!covers_as_fullscreen(big, true, FRAMED, MON, Some((MON, WORK))));
        // Nothing reserved: a frameless app maximized normally fills the
        // monitor too, so it is left alone.
        assert!(!covers_as_fullscreen(MON, true, POPUP, MON, Some((MON, MON))));
        // The monitor could not be read now: left alone as well.
        assert!(!covers_as_fullscreen(MON, true, POPUP, MON, None));
    }

    #[test]
    fn the_live_monitor_rect_wins() {
        // An exclusive game switched 2560x1600 to 1920x1080.
        let mode = RECT { left: 0, top: 0, right: 1920, bottom: 1080 };
        assert!(covers_as_fullscreen(mode, false, POPUP, MON, Some((mode, mode))));
        // The old rect alone does not make a smaller window fullscreen...
        assert!(!covers_as_fullscreen(mode, false, POPUP, MON, None));
        // ...and a window covering the old rect is measured on the new one.
        let other = RECT { left: 1920, top: 0, right: 3000, bottom: 1600 };
        assert!(covers_as_fullscreen(MON, false, POPUP, MON, Some((mode, mode))));
        assert!(!covers_as_fullscreen(other, false, POPUP, MON, Some((mode, mode))));
    }

    #[test]
    fn a_slip_beside_the_dock_is_forgiven() {
        let bar = RECT { left: 100, top: 700, right: 600, bottom: 760 };
        assert!(grace_contains(bar, 56, POINT { x: 350, y: 650 }));
        assert!(grace_contains(bar, 56, POINT { x: 60, y: 740 }));
        assert!(!grace_contains(bar, 56, POINT { x: 350, y: 600 }));
        assert!(!grace_contains(bar, 56, POINT { x: 700, y: 740 }));
    }
}

fn capture_overlay(exe: Option<&str>) -> bool {
    exe.is_some_and(|p| {
        let name=p.rsplit(['\\','/']).next().unwrap_or(p).to_ascii_lowercase();
        name!="explorer.exe" && crate::shot::CAPTURE_TOOLS.contains(&name.as_str())
    })
}
#[cfg(test)] mod assistant_capture_tests {
 use super::*;
 #[test] fn review_8_capture_overlays_never_cover_but_explorer_can() {
  for exe in crate::shot::CAPTURE_TOOLS {
   assert_eq!(capture_overlay(Some(&format!("C:/Tools/{}",exe.to_uppercase()))),exe!="explorer.exe");
  }
  assert!(!capture_overlay(None));
  assert!(!capture_overlay(Some("game.exe")));
 }
}
