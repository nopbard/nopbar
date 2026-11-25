//! Dock-thread lifecycle. Menus run outside the model borrow.
use super::{
    model::{Model, Size},
    source::Reader,
    view::View,
    windows::WindowsSource,
};
use crate::{
    config::Config,
    desklayer::{grid, icons, place, win},
    gfx::Gfx,
    i18n::t,
    popup::{self, Item},
    theme::Theme,
};
use std::{
    cell::{Cell, RefCell},
    path::PathBuf,
    rc::Rc,
};
use windows::{
    core::Result,
    Win32::{
        Foundation::*,
        UI::{
            Controls::{EM_SETLIMITTEXT, EM_SETSEL},
            Input::KeyboardAndMouse::SetFocus,
            Shell::{DefSubclassProc, SetWindowSubclass},
            WindowsAndMessaging::*,
        },
    },
};
const DESKTOP: u32 = WM_APP + 1;
const RENAME: u32 = WM_APP + 2;
// Side-effect boundaries are injectable so tests never create a desktop window.
fn flush_changed(
    pending: &mut bool,
    save: impl FnOnce() -> std::io::Result<()>,
) -> std::io::Result<()> {
    if !*pending {
        return Ok(());
    }
    save()?;
    *pending = false;
    Ok(())
}
fn draw_when_idle(dragging: bool, draw: impl FnOnce() -> Result<()>) -> Result<bool> {
    if dragging {
        return Ok(false);
    }
    draw()?;
    Ok(true)
}
fn rebuild_for_theme(old: &Theme, new: &Theme) -> bool {
    old.popup != new.popup
        || old.ui != new.ui
        || old.font != new.font
        || old.is_light != new.is_light
        || old.finish != new.finish
        || old.builtin != new.builtin
        || old.flat_grain != new.flat_grain
        || old.high_contrast != new.high_contrast
        || old.hc != new.hc
        || old.accent_rgba != new.accent_rgba
}
#[cfg(test)]
mod review_tests {
    use super::*;
    #[test]
    fn review_04_foreground_repaint_does_not_write_state() {
        let mut pending = false;
        for _ in 0..10 {
            flush_changed(&mut pending, || panic!("foreground paint wrote state")).unwrap();
        }
        pending = true;
        let mut writes = 0;
        flush_changed(&mut pending, || {
            writes += 1;
            Ok(())
        })
        .unwrap();
        assert_eq!(writes, 1);
        assert!(!pending);
    }
    #[test]
    fn review_06_dirty_during_drag_does_not_place_window() {
        assert!(!draw_when_idle(true, || panic!("drag snapped back to stored position")).unwrap());
        let mut calls = 0;
        assert!(draw_when_idle(false, || {
            calls += 1;
            Ok(())
        })
        .unwrap());
        assert_eq!(calls, 1);
    }
    #[test]
    fn review_11_unrelated_settings_preserve_window() {
        let before = Theme::graphite();
        assert!(!rebuild_for_theme(&before, &before.clone()));
        let mut after = before.clone();
        after.font.size += 1.;
        assert!(rebuild_for_theme(&before, &after));
    }
    #[test]
    fn review_08_reentrant_menu_does_not_panic() {
        APP.with(|cell| {
            let _held = cell.borrow_mut();
            menu(POINT::default());
        });
    }
}
struct App {
    model: Model,
    path: PathBuf,
    reader: Reader,
    source: WindowsSource,
    theme: Theme,
    gfx: Rc<Gfx>,
    view: Option<View>,
    desk: String,
    dirty: bool,
    pending_save: bool,
    alerts: bool,
    language: String,
    edit: Option<(HWND, String)>,
    /// The device whose last sighting the tooltip shows.
    tip: Option<String>,
}
thread_local! {static APP:RefCell<Option<App>>=const{RefCell::new(None)};}
thread_local! {static ENABLED:Cell<Option<bool>>=const{Cell::new(None)};}
fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}
fn desktop() -> Option<String> {
    crate::vdesk::sure().map(|s| {
        s.current_id()
            .map(|g| crate::vdesk::guid_text(&g))
            .unwrap_or_default()
    })
}
pub fn apply(cfg: &Config, theme: &Theme) {
    let reopen = ENABLED.with(|e| e.replace(Some(cfg.widgets.device_battery)) == Some(false));
    if !cfg.widgets.device_battery {
        shutdown();
        return;
    }
    APP.with(|cell| {
        let Ok(mut a) = cell.try_borrow_mut() else {
            return;
        };
        if let Some(a) = a.as_mut() {
            if rebuild_for_theme(&a.theme, theme) {
                a.finish_edit(true);
                a.view = None;
                a.dirty = true;
            }
            if a.language != cfg.language {
                a.dirty = true;
            }
            a.language = cfg.language.clone();
            a.theme = theme.clone();
            a.alerts = cfg.widgets.device_battery_alerts;
            if !a.alerts {
                crate::companion::device_battery::clear();
            }
            return;
        }
        let path = crate::desklayer::disk::dir().join("device-battery.json");
        let mut model = match Model::load(&path) {
            Ok(m) => m,
            Err(e) => {
                crate::log!("device battery state: {e}");
                return;
            }
        };
        // Reconcile with the new enumeration; never persist a false connection.
        for d in model.devices.values_mut() {
            d.connected = false;
            d.fresh = false;
            d.disconnected_at.get_or_insert(d.last_seen);
        }
        let gfx = match crate::gfx::shared()
            .map(Ok)
            .unwrap_or_else(|| Gfx::new().map(Rc::new))
        {
            Ok(g) => g,
            Err(e) => {
                crate::log!("device battery graphics: {e}");
                return;
            }
        };
        let desk = desktop().unwrap_or_default();
        let prefs = model.desktops.entry(desk.clone()).or_default();
        if reopen {
            prefs.closed = false;
        }
        *a = Some(App {
            model,
            path,
            reader: Reader::default(),
            source: WindowsSource::start(),
            theme: theme.clone(),
            gfx,
            view: None,
            desk,
            dirty: true,
            pending_save: true,
            alerts: cfg.widgets.device_battery_alerts,
            language: cfg.language.clone(),
            edit: None,
            tip: None,
        });
    });
    tick();
}
pub fn shutdown() {
    crate::companion::device_battery::clear();
    let app = APP.with(|a| a.try_borrow_mut().ok().and_then(|mut a| a.take()));
    if let Some(mut a) = app {
        a.save();
    }
}
pub fn tick() {
    let ready = APP.with(|cell| {
        let Ok(mut cell) = cell.try_borrow_mut() else {
            return false;
        };
        let Some(a) = cell.as_mut() else { return false };
        if a.reader.tick(&mut a.source, &mut a.model, now()) {
            a.dirty = true;
            a.pending_save = true;
        }
        let desk = desktop().unwrap_or_else(|| a.desk.clone());
        if desk != a.desk {
            a.finish_edit(true);
            a.desk = desk;
            a.dirty = true;
        }
        let prefs = a.model.desktops.get(&a.desk).cloned().unwrap_or_default();
        let valid: Vec<_> = a
            .model
            .devices
            .values()
            .filter(|d| a.alerts && d.needs_notice() && !prefs.hidden.contains(&d.id))
            .map(|d| d.id.clone())
            .collect();
        crate::companion::device_battery::retain(&valid);
        for id in valid {
            if let Some(d) = a.model.devices.get(&id) {
                crate::companion::device_battery::enqueue(
                    id,
                    format!("{} {}%", prefs.name(d), d.percent.unwrap_or(0)),
                );
            }
        }
        // No JSON serialization, monitor enumeration, or fullscreen scan on an idle tick.
        if a.dirty {
            match draw_when_idle(a.view.as_ref().is_some_and(|v| v.lifted.get()), || unsafe { a.draw() }) {
                Ok(true) => a.dirty = false,
                Ok(false) => {}
                Err(e) => crate::log!("device battery draw: {e}"),
            }
        }
        a.save();
        true
    });
    // A nested tick cannot deliver a notice whose acknowledgement it cannot record.
    if !ready {
        return;
    }
    if let Some(id) = crate::companion::device_battery::poll() {
        APP.with(|cell| {
            if let Ok(mut cell) = cell.try_borrow_mut() {
                if let Some(a) = cell.as_mut() {
                    a.pending_save |= a.model.acknowledge(&id);
                    a.save();
                }
            }
        });
    }
}
pub fn reattach() {
    APP.with(|c| {
        if let Ok(mut a) = c.try_borrow_mut() {
            if let Some(a) = a.as_mut() {
                if let Some(v) = a
                    .view
                    .as_ref()
                    .filter(|_| !a.model.desktops.get(&a.desk).is_some_and(|d| d.closed))
                {
                    win::to_desktop(v.hwnd, win::shell_host());
                }
                a.dirty = true;
            }
        }
    });
}
impl App {
    /// The tooltip for the device under the pointer: when it is not
    /// connected now, when it was last seen ("2시간 전").
    fn tip_at_cursor(&mut self) {
        let Some(v) = &self.view else { return };
        let mut p = POINT::default();
        let mut wr = RECT::default();
        unsafe {
            let _ = GetCursorPos(&mut p);
            let _ = GetWindowRect(v.hwnd, &mut wr);
        }
        let s = v.dpi.max(0.5);
        let (x, y) = ((p.x - wr.left) as f32 / s, (p.y - wr.top) as f32 / s);
        let prefs = self.model.desktops.get(&self.desk).cloned().unwrap_or_default();
        let rows = self.model.rows(&prefs);
        let now = now();
        let under = super::paint::device_at(&prefs, &rows, now, v.dimensions, x, y).filter(|d| !d.connected).map(|d| (d.id.clone(), d.last_seen));
        if under.as_ref().map(|u| &u.0) == self.tip.as_ref() {
            return;
        }
        self.hide_tip();
        let Some((id, seen)) = under else { return };
        let work = crate::monitors::under_cursor().map(|m| m.work).unwrap_or(wr);
        let at = POINT { x: p.x, y: p.y + (20. * s) as i32 };
        crate::topbar::show_tip_at(&self.gfx, &self.theme, s, &super::paint::ago(seen, now), at, false, work);
        self.tip = Some(id);
    }
    fn hide_tip(&mut self) {
        if self.tip.take().is_some() {
            crate::topbar::hide_tip();
        }
    }
    fn save(&mut self) {
        if let Err(e) = flush_changed(&mut self.pending_save, || self.model.save(&self.path)) {
            crate::log!("device battery save: {e}");
        }
    }
    unsafe fn draw(&mut self) -> Result<()> {
        // Dragged or gliding into its place: the drag hint draws it, and
        // its window is where the drag puts it (LANDED draws it again).
        if self.view.as_ref().is_some_and(|v|crate::desklayer::edit::dragging(v.hwnd)||v.lifted.get()){return Ok(());}
        let mons = crate::monitors::all();
        let Some(_m) = mons.iter().find(|m| m.primary).or(mons.first()) else {
            return Ok(());
        };
        if !self.model.desktops.contains_key(&self.desk) {
            self.pending_save = true;
        }
        let prefs = self.model.desktops.entry(self.desk.clone()).or_default();
        if prefs.closed || prefs.place.is_some_and(|p| place::connected(p.mon, &mons).is_none()) {
            if let Some(v) = &self.view {
                win::placing(|| {
                    let _ = ShowWindow(v.hwnd, SW_HIDE);
                });
            }
            return Ok(());
        }
        if crate::desklayer::grid_state::display_settling() { return Ok(()); }
        let wanted = prefs.place.as_ref().and_then(|p| place::to_px(p,&mons,(1.,1.)));
        let mi = wanted.and_then(|r| grid::monitor_of(r,&mons)).unwrap_or_else(|| mons.iter().position(|n|n.primary).unwrap_or(0));
        let dpi=mons[mi].scale();
        let desk=crate::desklayer::widget::desk(dpi);
        let reservation=icons::reserve(self.view.as_ref().map(|v|v.hwnd),|occupied| {
            let placed=if let Some(wanted)=wanted {crate::desklayer::widget::snap(&desk,&mons,wanted,Some(prefs.size),occupied)}
                else {crate::desklayer::widget::auto(&desk,&mons,mi,prefs.size,occupied)};
            crate::desklayer::widget::placed_or_stored(placed,wanted)
        });
        let Some(reservation)=reservation else{return Ok(());};let r=reservation.rect;
        let next=place::from_px(r,&mons);
        self.pending_save |= prefs.place != next;
        prefs.place=next;
        let dimensions=((r.right-r.left) as f32/dpi,(r.bottom-r.top) as f32/dpi);
        // A new size keeps the window (its handle is the card's for edit
        // mode and for the command being handled): only its surface changes.
        match super::view::remake(self.view.as_ref().map(View::shape),(prefs.size,dpi,dimensions)) {
            super::view::Remake::Make => {
                let v=View::sized(self.gfx.clone(),&self.theme,prefs.size,dimensions,dpi,Some(proc))?;
                crate::desklayer::privacy::register(v.hwnd,false);
                icons::register_card(v.hwnd,0);
                icons::read(v.hwnd);
                crate::vdesk::listen(v.hwnd,DESKTOP);
                self.view=Some(v);
            }
            super::view::Remake::Resize => {
                if let Some(v)=self.view.as_mut() {v.resize(prefs.size,dimensions,dpi)?;}
            }
            super::view::Remake::Keep => {}
        }
        let v = self.view.as_ref().unwrap();
        if let Some(p) = next { icons::remember_place(v.hwnd, p); }
        let prefs = self
            .model
            .desktops
            .get(&self.desk)
            .cloned()
            .unwrap_or_default();
        crate::desklayer::edit::remember(v.hwnd,crate::desklayer::edit::Key::Battery,Some(prefs.size),crate::desklayer::edit::Preview::Battery(prefs.clone(),self.model.rows(&prefs).into_iter().cloned().collect()));
        v.aim(r);
        v.draw(&self.theme, &prefs, &self.model.rows(&prefs))?;
        v.place(r);
        icons::push(v.hwnd, v.hwnd);
        Ok(())
    }
    fn finish_edit(&mut self, cancel: bool) {
        if let Some((h, id)) = self.edit.take() {
            unsafe {
                if !cancel {
                    let mut text = vec![0u16; GetWindowTextLengthW(h).max(0) as usize + 1];
                    let len = GetWindowTextW(h, &mut text);
                    let prefs = self.model.desktops.entry(self.desk.clone()).or_default();
                    let before = prefs.names.clone();
                    prefs.rename(&id, &String::from_utf16_lossy(&text[..len.max(0) as usize]));
                    self.pending_save |= prefs.names != before;
                    self.save();
                    self.dirty = true;
                }
                // Its field (the popup holding it) goes with it.
                let _ = DestroyWindow(GetParent(h).unwrap_or(h));
            }
        }
    }
    unsafe fn rename(&mut self, id: String) {
        self.finish_edit(true);
        let Some(v) = &self.view else { return };
        let Some(d) = self.model.devices.get(&id) else {
            return;
        };
        let prefs = self
            .model
            .desktops
            .get(&self.desk)
            .cloned()
            .unwrap_or_default();
        let name = prefs.name(d).to_string();
        let s = v.dpi;
        // A field over the device's name: the card's own pixels there, the
        // text 12/600 in the first grey (a child EDIT never shows in the
        // card's window: it has no redirection bitmap).
        let mut card = RECT::default();
        let _ = GetWindowRect(v.hwnd, &mut card);
        let rows = self.model.rows(&prefs);
        let (x, y, w, hh) = super::paint::name_rect(&prefs, &rows, &id, v.dimensions);
        let px = |f: f32| (f * s).round() as i32;
        let cut = RECT { left: px(x), top: px(y), right: px(x + w), bottom: px(y + hh) };
        let at = RECT { left: card.left + cut.left, top: card.top + cut.top, right: card.left + cut.right, bottom: card.top + cut.bottom };
        let under = crate::desklayer::backdrop::under_for(v.hwnd, card);
        let theme = crate::desklayer::card::theme_for(&self.theme, under.as_ref());
        let bg = crate::desklayer::field::piece(card, s, theme, cut, under.as_ref());
        let look = crate::desklayer::field::Look { size: 12., weight: 600, ink: crate::desklayer::card::ink(theme, 1), light: theme.is_light };
        let h = crate::desklayer::field::line(v.hwnd, at, bg, &look, s, &name).map(|(_, e)| e).ok_or(());
        if let Ok(h) = h {
            let _ = SetWindowSubclass(h, Some(edit_proc), 1, v.hwnd.0 as usize);
            SendMessageW(h, EM_SETLIMITTEXT, Some(WPARAM(80)), Some(LPARAM(0)));
            let _ = SetForegroundWindow(GetParent(h).unwrap_or(h));
            let _ = SetFocus(Some(h));
            SendMessageW(h, EM_SETSEL, Some(WPARAM(0)), Some(LPARAM(-1)));
            self.edit = Some((h, id));
        }
    }
}
fn menu(at: POINT) {
    let data = APP.with(|c| {
        let a = c.try_borrow().ok()?;
        let a = a.as_ref()?;
        let v = a.view.as_ref()?;
        let d = a.model.desktops.get(&a.desk).cloned().unwrap_or_default();
        let ids: Vec<_> = a
            .model
            .devices
            .values()
            .filter(|x| x.percent.is_some())
            .map(|x| x.id.clone())
            .collect();
        let visible = ids
            .iter()
            .enumerate()
            .map(|(i, id)| {
                Item::new(100 + i as u32, d.name(&a.model.devices[id]))
                    .literal()
                    .checked(!d.hidden.contains(id))
            })
            .collect();
        let names = ids
            .iter()
            .enumerate()
            .map(|(i, id)| Item::new(10000 + i as u32, d.name(&a.model.devices[id])).literal())
            .collect();
        let items = vec![
            Item::new(0, t("battery.size")).submenu(vec![
                Item::new(1, t("battery.small")).checked(d.size == Size::SMALL),
                Item::new(2, t("battery.medium")).checked(d.size == Size::MEDIUM),
                Item::new(4, Size::LARGE.label()).literal().checked(d.size == Size::LARGE),
            ]),
            Item::new(50,crate::i18n::t("widgets.edit")).literal(),
            Item::new(0, t("battery.choose")).submenu(visible),
            Item::new(0, t("battery.rename")).submenu(names),
            Item::sep(),
            Item::new(3, t("battery.close")),
        ];
        Some((a.gfx.clone(), a.theme.clone(), v.dpi, ids, items))
    });
    let Some((gfx, theme, scale, ids, items)) = data else {
        return;
    };
    let result = popup::track(popup::Request {
        gfx: &gfx,
        theme: &theme,
        items,
        at,
        align: popup::Align::TopLeft,
        scale,
        titles: Default::default(),
        on_slide: None,
        drag_ids: 0..0,
        on_space: None,
        on_pick: None,
        hangs: false,
    });
    if let popup::Outcome::Picked(50)=result {crate::desklayer::edit::enter();return;}
    if let popup::Outcome::Picked(id) = result {
        APP.with(|c| {
            let Ok(mut cell) = c.try_borrow_mut() else {
                return;
            };
            if let Some(a) = cell.as_mut() {
                let d = a.model.desktops.entry(a.desk.clone()).or_default();
                let before = d.clone();
                match id {
                    1 | 2 | 4 => {
                        let size=if id==1 {Size::SMALL}else if id==2{Size::MEDIUM}else{Size::LARGE};
                        if let Some(v)=&a.view {
                            let desk=crate::desklayer::widget::desk(v.dpi);let mons=crate::monitors::all();
                            if let Some(r)=icons::card(v.hwnd).and_then(|r|crate::desklayer::widget::snap(&desk,&mons,r,Some(size),&icons::widget_rects(Some(v.hwnd)))) {
                                d.size=size;d.place=place::from_px(r,&mons);
                            }
                        }
                    }
                    3 => d.closed = true,
                    100..=9999 => {
                        if let Some(id) = ids.get((id - 100) as usize) {
                            d.toggle(id)
                        }
                    }
                    10000.. => {
                        if let Some(id) = ids.get((id - 10000) as usize) {
                            unsafe { a.rename(id.clone()) }
                        }
                    }
                    _ => {}
                }
                a.pending_save |= a.model.desktops.get(&a.desk) != Some(&before);
                a.dirty = true;
                a.save();
            }
        });
        tick();
    }
}
unsafe extern "system" fn edit_proc(
    h: HWND,
    m: u32,
    w: WPARAM,
    l: LPARAM,
    _: usize,
    parent: usize,
) -> LRESULT {
    if m == WM_KEYDOWN && matches!(w.0, 13 | 27) {
        use windows::Win32::UI::Input::Ime::{
            ImmGetCompositionStringW, ImmGetContext, ImmReleaseContext, GCS_COMPSTR,
        };
        let ctx = ImmGetContext(h);
        if !ctx.is_invalid() {
            let composing = ImmGetCompositionStringW(ctx, GCS_COMPSTR, None, 0) > 0;
            let _ = ImmReleaseContext(h, ctx);
            if composing {
                return DefSubclassProc(h, m, w, l);
            }
        }
        let _ = PostMessageW(
            Some(HWND(parent as *mut _)),
            RENAME,
            WPARAM(usize::from(w.0 == 27)),
            LPARAM(0),
        );
        return LRESULT(0);
    }
    if m == WM_KILLFOCUS {
        let _ = PostMessageW(Some(HWND(parent as *mut _)), RENAME, WPARAM(0), LPARAM(0));
    }
    DefSubclassProc(h, m, w, l)
}
unsafe extern "system" fn proc(h: HWND, m: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    if m==crate::desklayer::edit::COMMAND {for action in crate::desklayer::edit::take_actions(h){let visibility=action.clone();widget_action(action);crate::desklayer::edit::apply_owner_visibility(h,&visibility);}return LRESULT(0);}
    if m == WM_CONTEXTMENU || (m==WM_LBUTTONDOWN && {let mut p=POINT::default();let _=GetCursorPos(&mut p);crate::desklayer::edit::control_at(h,p)}) {
        let mut p = POINT::default();
        let _ = GetCursorPos(&mut p);
        menu(p);
        return LRESULT(0);
    }
    if m == WM_WINDOWPOSCHANGING {
        if let Some(p) = (l.0 as *mut WINDOWPOS).as_mut() {
            win::pin(crate::desklayer::layer::Layer::Desktop, h, p);
        }
        return LRESULT(0);
    }
    APP.with(|c| {
        let Ok(mut cell) = c.try_borrow_mut() else {
            return;
        };
        let Some(a) = cell.as_mut() else { return };
        match m {
            RENAME => a.finish_edit(w.0 != 0),
            crate::desklayer::hover::REDRAW => {if let Some(v)=&a.view {let d=a.model.desktops.get(&a.desk).cloned().unwrap_or_default();let _=v.draw(&a.theme,&d,&a.model.rows(&d));}},
            windows::Win32::UI::Controls::WM_MOUSELEAVE => a.hide_tip(),
            WM_LBUTTONDOWN => {
                a.hide_tip();
                // Anywhere on the card but its button (handled above).
                if let Some(v) = &a.view {
                    crate::desklayer::grab::press(h, Some(v.size));
                }
            }
            WM_MOUSEMOVE => {
                if crate::desklayer::grab::moved(h) {
                    a.hide_tip();
                } else {
                    a.tip_at_cursor();
                }
            }
            WM_LBUTTONUP => {
                crate::desklayer::grab::release(h);
            }
            crate::desklayer::drag_hint::LIFTED => {
                if let Some(v) = &a.view {
                    v.clear(a.theme.is_light);
                }
                crate::desklayer::drag_hint::hidden(h, w, &a.gfx.compositor);
            }
            crate::desklayer::drag_hint::LANDED => {
                if let Some(v) = &a.view {
                    v.lifted.set(false);
                }
                if a.draw().is_ok() {
                    crate::desklayer::drag_hint::presented(h, w, &a.gfx.compositor);
                } else {
                    a.dirty = true;
                }
            }
            WM_CAPTURECHANGED => {
                crate::desklayer::grab::cancel(h);
            }
            WM_DISPLAYCHANGE => {crate::desklayer::grid_state::begin_display_change();a.dirty=true;}
            WM_DPICHANGED | DESKTOP | crate::desklayer::backdrop::WM_BACKDROP => a.dirty = true,
            x if x == icons::wm_grid() && w.0 == icons::READ => a.dirty = true,
            _ => {}
        }
    });
    DefWindowProcW(h, m, w, l)
}

pub fn widget_action(action:crate::desklayer::edit::Action){
    use crate::desklayer::{edit::{self,Action},widget};
    APP.with(|cell|{let Ok(mut cell)=cell.try_borrow_mut()else{return;};let Some(a)=cell.as_mut()else{return;};
        let mons=crate::monitors::all();
        if !edit::allow_geometry(a.view.as_ref().map(|v| v.hwnd), &action, &mons) { return; }
        if matches!(action, Action::DisplaysChanged) {a.dirty=true;}
        let placement=match action {
            Action::Place(r)=>Some((None,r)),
            Action::Resize(r,size)=>Some((Some(size),r)),
            Action::Add{size,monitor,..}=>mons.get(monitor).and_then(|m|widget::auto(&widget::desk(m.scale()),&mons,monitor,size,&icons::widget_rects(a.view.as_ref().map(|v|v.hwnd)))).map(|r|(Some(size),r)),
            Action::Enter=>None,
            _=>None,
        };
        if let Some((size,r))=placement{let d=a.model.desktops.entry(a.desk.clone()).or_default();if let Some(size)=size{d.size=size;}d.place=place::from_px(r,&mons);d.closed=false;a.pending_save=true;a.dirty=true;a.save();}
    });tick();edit::refresh();
}

/// Existing device cache only; no new discovery or polling.
pub fn assistant_snapshot()->Vec<(String,u8)>{APP.with(|a|a.try_borrow().ok().and_then(|a|a.as_ref().map(|a|a.model.devices.values().filter(|d|d.connected&&d.fresh&&!d.charging).filter_map(|d|d.percent.map(|p|(d.name.clone(),p))).collect())).unwrap_or_default())}
