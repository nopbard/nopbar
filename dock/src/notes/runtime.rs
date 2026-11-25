//! Runs only on the notes thread; the dock and companion communicate by queues.
use super::{
    capture::{self, Key},
    inbox::{self, Row, Store},
    store::RealDisk,
    time, widgets,
};
use crate::{
    companion::{
        chat::input::Editor,
    },
    gfx::Gfx,
    theme::Theme,
};
use std::{cell::RefCell, rc::Rc, time::Instant};
use windows::{
    core::w,
    Win32::{
        Foundation::*,
        Graphics::{DirectWrite::*, Gdi::*},
        UI::{
            Controls::WM_MOUSELEAVE,
            Input::{Ime::*, KeyboardAndMouse::*},
            WindowsAndMessaging::*,
        },
    },
};
pub const TIMER: usize = 0x4e52;
const ANIMATE: usize = 0x4e53;
thread_local! {static STATE:RefCell<Option<Runtime>>=const{RefCell::new(None)};}
#[derive(Clone, Debug)]
pub enum Action {
    Complete(String),
    Snooze(String),
    Tomorrow(Vec<String>),
    BriefingShown(Vec<(String, i64)>),
    Missed(Vec<String>),
    Delivered(super::reminders::Notice),
    Reveal(String),
}
static ACTIONS: std::sync::Mutex<Vec<Action>> = std::sync::Mutex::new(Vec::new());
pub fn action(a: Action) {
    ACTIONS.lock().unwrap_or_else(|e| e.into_inner()).push(a);
    super::post(super::hub::WM_REMIND);
}
static ASSISTANT: std::sync::Mutex<Option<Vec<Row>>> = std::sync::Mutex::new(None);
pub fn assistant_snapshot()->Option<Vec<Row>> {ASSISTANT.lock().unwrap_or_else(|e|e.into_inner()).clone()}
struct Runtime {
    gfx: Rc<Gfx>,
    theme: Theme,
    store: Store,
    bar: Option<capture::View>,
    editor: Editor,
    target: usize,
    tabs: bool,
    popup: bool,
    lead: u8,
    changing: bool,
    change_editor: Editor,
    before: HWND,
    opened: Instant,
    exiting: Option<(Instant, bool)>,
    edit: capture::Editing,
    inbox: Option<crate::device_battery::view::View>,
    inbox_size: (u32, u32),
    inbox_pos: (i32, i32),
    dpi: f32,
    expanded: bool,
    offset: usize,
    hover: Option<usize>,
    flash: Option<(String, Instant)>,
    click: Option<capture::PendingClick>,
    startup: bool,
    pending: std::collections::HashSet<String>,
}
fn with<R>(f: impl FnOnce(&mut Runtime) -> R) -> Option<R> {
    STATE.with(|s| s.try_borrow_mut().ok()?.as_mut().map(f))
}
pub fn init(theme: Theme) {
    if super::headless() {
        return;
    }
    match Gfx::new() {
        Ok(gfx) => {
            let mut store = Store::load(&mut RealDisk::new());
            store.data.seed_notes(&super::hub::reminder_seed());
            store.save(&mut RealDisk::new());
            STATE.with(|s| {
                *s.borrow_mut() = Some(Runtime {
                    gfx: Rc::new(gfx),
                    theme,
                    store,
                    bar: None,
                    editor: Editor::default(),
                    target: 0,
                    tabs: false,
                    popup: false,
                    lead: 10,
                    changing: false,
                    change_editor: Editor::default(),
                    before: HWND::default(),
                    opened: Instant::now(),
                    exiting: None,
                    edit: capture::Editing::default(),
                    inbox: None,
                    inbox_size: (0, 0),
                    inbox_pos: (0, 0),
                    dpi: 1.,
                    expanded: false,
                    offset: 0,
                    hover: None,
                    flash: None,
                    click: None,
                    startup: true,
                    pending: Default::default(),
                })
            });
            unsafe {
                SetTimer(Some(super::hub_hwnd()), TIMER, 1000, None);
            }
            refresh();
        }
        Err(e) => crate::log!("capture: graphics unavailable: {e}"),
    }
}
pub fn shutdown() {
    with(|r| {
        r.store.save(&mut RealDisk::new());
    });
    STATE.with(|s| s.borrow_mut().take());
}
pub fn save() -> bool {
    with(|r| r.store.save(&mut RealDisk::new())).unwrap_or(true)
}
pub fn retheme(t: Theme) {
    with(|r| {
        r.theme = t;
        r.inbox = None;
        if r.bar.is_some() {
            r.close(false);
        }
    });
    refresh();
}
pub fn refresh() {
    with(|r| {
        if let Err(e) = unsafe { r.draw_inbox() } {
            crate::log!("inbox: {e}");
        }
    });
}
pub fn layer(front: bool) {
    with(|r| {
        if let Some(i) = &r.inbox {
            // Home screen only: never above other programs (owner 0.1.89).
            let _ = front;
            crate::desklayer::win::to_desktop(i.hwnd, crate::desklayer::win::shell_host());
        }
    });
}
pub fn hotkey() -> bool {
    with(|r| {
        for step in capture::hotkey_steps(
            r.bar.is_some(),
            &r.editor.field.text(),
            super::hub::layer() == crate::desklayer::layer::Layer::Summoned,
        ) {
            match step {
                capture::HotkeyStep::Restore => unsafe {
                    if IsWindow(Some(r.before)).as_bool() {
                        let _ = SetForegroundWindow(r.before);
                    }
                },
                capture::HotkeyStep::Toggle => {
                    r.edit.reset();
                    r.bar = None;
                    r.exiting = None;
                    return true;
                }
                capture::HotkeyStep::Open => {
                    if r.bar.is_none() {
                        unsafe {
                            r.open();
                        }
                    }
                }
            }
        }
        false
    })
    .unwrap_or(true)
}
pub fn owns(h: HWND) -> bool {
    STATE.with(|s| {
        s.try_borrow()
            .ok()
            .and_then(|s| {
                s.as_ref().map(|r| {
                    r.bar.as_ref().is_some_and(|b| b.hwnd == h)
                        || r.inbox.as_ref().is_some_and(|i| i.hwnd == h)
                })
            })
            .unwrap_or(false)
    })
}
pub fn note_saved(id: &str, body: &str, desk: Option<String>) {
    with(|r| {
        let old: Vec<_> = r
            .store
            .data
            .rows
            .iter()
            .filter(|r| r.source.as_ref().is_some_and(|(n, _)| n == id))
            .map(|r| r.id.clone())
            .collect();
        r.store.data.observe_note(
            id,
            body,
            desk,
            time::now(),
            crate::i18n::current(),
            &super::preferences(),
        );
        for id in old {
            if !r.store.data.rows.iter().any(|r| r.id == id) {
                r.pending.remove(&id);
                crate::companion::remind::cancel(&id);
            }
        }
    });
}
pub fn retain_notes(ids: &[String]) {
    with(|r| {
        let old: Vec<_> = r.store.data.rows.iter().map(|r| r.id.clone()).collect();
        r.store.data.retain_notes(ids);
        for id in old {
            if !r.store.data.rows.iter().any(|r| r.id == id) {
                r.pending.remove(&id);
                crate::companion::remind::cancel(&id);
            }
        }
    });
}
pub fn date_hour() -> Option<u8> {
    match super::preferences().notes_date_hour {
        9 => Some(9),
        10 => Some(10),
        _ => None,
    }
}
pub fn tick() {
    let actions = std::mem::take(&mut *ACTIONS.lock().unwrap_or_else(|e| e.into_inner()));
    let mut reveal = None;
    let mut check = None;
    with(|r| {
        let now = time::now();
        let mut changed = false;
        for a in actions {
            match &a {
                Action::Reveal(id) => {
                    reveal = Some(
                        r.store
                            .data
                            .rows
                            .iter()
                            .find(|x| &x.id == id)
                            .and_then(|x| x.source.as_ref().map(|(n, _)| n.clone())),
                    );
                }
                Action::Tomorrow(ids)=>{for id in inbox::tomorrow(&mut r.store.data.rows,ids,now){r.pending.remove(&id);crate::companion::remind::cancel(&id);changed=true;}},
                Action::BriefingShown(shown) => {
                    changed |= inbox::clear_shown_carries(&mut r.store.data.rows, shown);
                }
                Action::Delivered(notice) => {
                    changed |= super::reminders::delivered(&mut r.store.data.rows, notice);
                    for id in &notice.ids {
                        r.pending.remove(id);
                    }
                }
                Action::Missed(ids) => {
                    for row in r.store.data.rows.iter_mut().filter(|r| ids.contains(&r.id)) {
                        changed |= !row.missed;
                        row.missed = true;
                    }
                }
                Action::Complete(id) | Action::Snooze(id) => {
                    if let Some(row) = r.store.data.rows.iter_mut().find(|x| &x.id == id) {
                        match &a {
                            Action::Complete(_) => {
                                row.complete(now);
                                r.pending.remove(&row.id);
                                crate::companion::remind::cancel(&row.id);
                                check = row.source.clone();
                            }
                            Action::Snooze(_) => {
                                row.snooze(now);
                                r.pending.remove(&row.id);
                            }
                            _ => {}
                        }
                        changed = true;
                    }
                }
            }
        }
        if super::preferences().notes_reminders {
            if r.startup {
                let ids = super::reminders::missed_at_start(&r.store.data.rows, now);
                r.pending.extend(ids.iter().cloned());
                r.startup = false;
                if !ids.is_empty() {
                    crate::companion::remind::enqueue_notice(super::reminders::Notice::summary(
                        ids, true,
                    ));
                }
            }
            for id in super::reminders::unqueued(&r.store.data.rows, now, &mut r.pending) {
                if let Some(row) = r.store.data.rows.iter().find(|r| r.id == id) {
                    crate::companion::remind::enqueue(id, super::reminders::message(row, now));
                }
            }
        }

        if r.click
            .as_ref()
            .is_some_and(|c| c.at.elapsed().as_millis() > unsafe { GetDoubleClickTime() } as u128)
        {
            let click = r.click.take().unwrap();
            if let Some(row) = click.row(&mut r.store.data.rows) {
                if row.completed.is_some() {
                    row.completed = None;
                    row.delivered = false;
                } else {
                    row.complete(now);
                    r.pending.remove(&row.id);
                    crate::companion::remind::cancel(&row.id);
                }
                changed = true;
            }
        }
        if changed {
            r.store.save(&mut RealDisk::new());
        }
        let _ = unsafe { r.draw_inbox() };
    });
    if let Some((id, line)) = check {
        super::hub::complete_line(&id, &line);
    }
    if let Some(id) = reveal {
        super::hub::reveal(id.as_deref());
        layer(true);
    }
    crate::companion::refresh();
}
impl Runtime {
    fn active_editor(&mut self) -> &mut Editor {
        if self.changing {
            &mut self.change_editor
        } else {
            &mut self.editor
        }
    }
    fn found(&self) -> Option<time::Found> {
        let s = self.editor.displayed();
        let mut f = time::parse(&s, time::now(), crate::i18n::current(), date_hour());
        match self.edit.time {
            Some(None) => None,
            Some(Some(at)) => {
                if let Some(f) = &mut f {
                    f.at = at;
                } else {
                    f = Some(time::Found { at, span: 0..0 });
                }
                f
            }
            None => f,
        }
    }
    unsafe fn open(&mut self) {
        self.edit.reset();
        self.changing = false;
        self.popup = false;
        let Some(m) = crate::monitors::under_cursor().or_else(crate::monitors::primary) else {
            return;
        };
        self.before = GetForegroundWindow();
        self.lead = super::preferences().notes_lead;
        match capture::View::new(self.gfx.clone(), &self.theme, m.scale(), m.work, Some(proc)) {
            Ok(v) => {
                let h = v.hwnd;
                self.bar = Some(v);
                self.opened = Instant::now();
                self.exiting = None;
                self.draw_bar();
                let _ = ShowWindow(h, SW_SHOWNORMAL);
                let _ = SetForegroundWindow(h);
                let _ = SetFocus(Some(h));
                SetTimer(Some(h), ANIMATE, 16, None);
            }
            Err(e) => crate::log!("capture open: {e}"),
        }
    }
    fn close(&mut self, send: bool) {
        self.edit.reset();
        self.changing = false;
        self.popup = false;
        self.tabs = false;
        self.exiting = Some((Instant::now(), send));
        unsafe {
            if self
                .bar
                .as_ref()
                .is_some_and(|v| GetForegroundWindow() == v.hwnd)
                && IsWindow(Some(self.before)).as_bool()
            {
                let _ = SetForegroundWindow(self.before);
            }
        }
    }
    fn draw_bar(&mut self) {
        let Some(v) = &self.bar else {
            return;
        };
        let s = if self.changing {
            self.change_editor.displayed()
        } else {
            self.editor.displayed()
        };
        let f = self.found();
        let ed = if self.changing {
            &self.change_editor
        } else {
            &self.editor
        };
        let _ = v.draw(
            &self.theme,
            &widgets::Capture {
                text: &s,
                target: self.target,
                tabs: self.tabs,
                found: if self.changing { None } else { f.as_ref() },
                now: time::now(),
                caret: true,
                caret_at: Some(ed.caret()),
                selection: ed.selection(),
                popup: self.popup,
                lead: self.lead,
                changing: self.changing,
            },
        );
        let motion = crate::effects::motion();
        let mut done = false;
        let (scale, alpha, delta) = if let Some((at, send)) = self.exiting {
            let duration = if !motion {
                0.06
            } else if send {
                0.28
            } else {
                0.12
            };
            let t = (at.elapsed().as_secs_f32() / duration).min(1.);
            done = t >= 1.;
            let k = if motion && send {
                1. - crate::motion::SPRING_GLIDE.left(t * 0.28)
            } else {
                t
            };
            let inbox_visible = self.inbox.as_ref().is_some_and(|i| unsafe {
                IsWindowVisible(i.hwnd).as_bool()
                    && WindowFromPoint(POINT {
                        x: self.inbox_pos.0 + 80,
                        y: self.inbox_pos.1 + 20,
                    }) == i.hwnd
            });
            let dest = if inbox_visible {
                self.inbox_pos
            } else {
                crate::companion::capture_target().unwrap_or(self.inbox_pos)
            };
            (
                if motion && send { 1. - 0.7 * k } else { 1. },
                1. - t,
                if motion && send {
                    (
                        (dest.0 as f32 - v.origin.0 as f32 - (capture::MARGIN + 280.) * v.dpi) * k,
                        (dest.1 as f32 - v.origin.1 as f32 - (capture::MARGIN + 28.) * v.dpi) * k,
                    )
                } else {
                    (0., 0.)
                },
            )
        } else {
            let t = self.opened.elapsed().as_secs_f32();
            let k = if motion {
                (1. - crate::motion::SPRING_GLIDE.left(t)).clamp(0., 1.)
            } else {
                (t / 0.06).min(1.)
            };
            (if motion { 0.96 + 0.04 * k } else { 1. }, k, (0., 0.))
        };
        // Move the HWND too: a Composition translation alone would clip the
        // travelling bar at the original small host window's edge.
        unsafe {
            let _ = SetWindowPos(
                v.hwnd,
                None,
                v.origin.0 + delta.0 as i32,
                v.origin.1 + delta.1 as i32,
                0,
                0,
                SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOZORDER,
            );
        }
        let _ = v.animate(scale, alpha, (0., 0.));
        let (left, right) = widgets::input_edges(
            &self.gfx,
            self.target,
            self.tabs,
            if self.changing { None } else { f.as_ref() },
            time::now(),
        )
        .unwrap_or((95., 454.));
        let mut caret_x = 0.;
        if let Ok((layout, _, _)) = self.gfx.text_layout(
            &s,
            crate::fonts::THEME_DEFAULT,
            20.,
            DWRITE_FONT_WEIGHT_NORMAL,
        ) {
            let mut y = 0.;
            let mut hit = DWRITE_HIT_TEST_METRICS::default();
            unsafe {
                let _ =
                    layout.HitTestTextPosition(ed.caret(), false, &mut caret_x, &mut y, &mut hit);
            }
        }

        unsafe {
            crate::companion::chat::input::place_ime(
                v.hwnd,
                capture::MARGIN + left + caret_x.min((right - left - 3.).max(0.)),
                capture::MARGIN + 17.,
                v.dpi,
            );
        }
        if done {
            let sent = self.exiting.take().is_some_and(|(_, s)| s);
            self.bar = None;
            if sent {
                self.editor = Editor::default();
                self.edit.time = None;
                self.edit.id = None;
            }
        }
    }
    fn submit(&mut self, new_note: bool) {
        let text = self
            .editor
            .field
            .text()
            .replace(['\n', '\r'], " ")
            .trim()
            .to_owned();
        if text.is_empty() {
            self.edit.reset();
            return;
        }
        if new_note || self.target == 1 {
            if let Some(id) = super::hub::captured_note(&text) {
                self.edit.remember_note(id);
                self.close(true);
            }
            return;
        }
        let now = time::now();
        let remind = if super::preferences().notes_reminders {
            self.found().map(|f| f.at)
        } else {
            None
        };
        let id = if let Some(row) = self
            .edit
            .id
            .as_ref()
            .and_then(|id| self.store.data.rows.iter_mut().find(|r| &r.id == id))
        {
            self.pending.remove(&row.id);
            crate::companion::remind::cancel(&row.id);
            row.revise(text, remind, self.lead);
            row.id.clone()
        } else {
            let row = Row::new(text, now, remind, self.lead, super::hub::current_desktop());
            let id = row.id.clone();
            self.store.data.rows.push(row);
            id
        };
        self.edit.id = Some(id.clone());
        if self.store.save(&mut RealDisk::new()) {
            self.edit.last = Some(id.clone());
            self.flash = Some((id, Instant::now()));
            let _ = unsafe { self.draw_inbox() };
            if let Some(i) = &self.inbox {
                unsafe {
                    SetTimer(Some(i.hwnd), ANIMATE, 16, None);
                    if WindowFromPoint(POINT {
                        x: self.inbox_pos.0 + 80,
                        y: self.inbox_pos.1 + 20,
                    }) != i.hwnd
                    {
                        crate::companion::capture_received();
                    }
                }
            }
            self.close(true);
        }
    }
    fn indices(&self) -> (Vec<usize>, usize, usize) {
        let desk = super::hub::current_desktop();
        let (_,folded,_)=inbox::visible(&self.store.data.rows,time::now(),self.expanded,desk.as_deref(),super::per_desktop());
        let count=self.inbox.as_ref().map_or(4,|i|widgets::rows_fit(i.dimensions.1).max(1));
        let all:Vec<_>=self.store.data.rows.iter().enumerate().filter(|(_,r)|r.source.is_none()&&super::hub::shows_desktop(r.desktop.as_deref())&&(self.expanded||r.completed.is_none_or(|t|time::now()-t<86400))).map(|(i,_)|i).collect();
        let offset=self.offset.min(all.len().saturating_sub(1));
        (all.iter().skip(offset).take(count).copied().collect(),folded,all.len().saturating_sub(offset+count))
    }
    unsafe fn draw_inbox(&mut self) -> windows::core::Result<()> {
        *ASSISTANT.lock().unwrap_or_else(|e|e.into_inner())=Some(self.store.data.rows.clone());
        if self.inbox.as_ref().is_some_and(|i|crate::desklayer::edit::dragging(i.hwnd)||i.lifted.get()){return Ok(());}
        if !super::preferences().inbox {if let Some(i)=&self.inbox{i.hide();}return Ok(());}
        let (v, _, _) = self.indices();
        let desk = super::hub::current_desktop();
        let exists = self
            .store
            .data
            .rows
            .iter()
            .any(|r| r.source.is_none() && super::hub::shows_desktop(r.desktop.as_deref()));
        if !exists && self.store.data.places.iter().all(|(d, _)| d != &desk) {
            if let Some(i) = &self.inbox {
                i.hide();
            }
            return Ok(());
        }
        let Some(m) = crate::monitors::under_cursor().or_else(crate::monitors::primary) else {
            return Ok(());
        };
        let data = widgets::Inbox {
            rows: &self.store.data.rows,
            remaining: self
                .store
                .data
                .rows
                .iter()
                .filter(|r| {
                    r.source.is_none()
                        && r.completed.is_none()
                        && super::hub::shows_desktop(r.desktop.as_deref())
                })
                .count(),
            visible: &v,
            hover: self.hover,
            flash: self.flash.as_ref().and_then(|(id, t)| {
                self.store
                    .data
                    .rows
                    .iter()
                    .position(|r| &r.id == id)
                    .map(|i| (i, t.elapsed().as_secs_f32()))
            }),
        };
        let chosen=self.store.data.sizes.iter().find(|(d,_)|d==&desk).map_or(crate::desklayer::widget::Size::MEDIUM,|(_,s)|*s);
        let mons=crate::monitors::all();
        let saved=self.store.data.places.iter().find(|(d,_)|d==&desk).map(|(_,p)|*p);
        if saved.is_some_and(|p|crate::desklayer::place::connected(p.mon,&mons).is_none()) {
            if let Some(i)=&self.inbox {crate::desklayer::win::placing(||{let _=ShowWindow(i.hwnd,SW_HIDE);});}
            return Ok(());
        }
        if crate::desklayer::grid_state::display_settling() {return Ok(());}
        let old=saved.and_then(|p|crate::desklayer::place::to_px(&p,&mons,(1.,1.)));
        let mi=old.and_then(|r|crate::desklayer::grid::monitor_of(r,&mons)).unwrap_or_else(||mons.iter().position(|n|n.primary).unwrap_or(0));
        self.dpi=mons.get(mi).map_or(m.scale(),|m|m.scale());
        let grid=crate::desklayer::widget::desk(self.dpi);
        let reservation=crate::desklayer::icons::reserve(self.inbox.as_ref().map(|i|i.hwnd),|others|{
            if let Some(old)=old {crate::desklayer::widget::snap(&grid,&mons,old,Some(chosen),others).or(Some(old))}
            else {crate::desklayer::widget::auto(&grid,&mons,mi,chosen,others)}
        });
        let Some(reservation)=reservation else{return Ok(());};let r=reservation.rect;
        if let Some(p)=crate::desklayer::place::from_px(r,&mons) {
            if let Some((_,old))=self.store.data.places.iter_mut().find(|(d,_)|d==&desk) {*old=p;} else {self.store.data.places.push((desk,p));}
        }
        let dimensions=((r.right-r.left) as f32/self.dpi,(r.bottom-r.top) as f32/self.dpi);
        let size=(((dimensions.0+48.)*self.dpi).ceil() as u32,((dimensions.1+48.)*self.dpi).ceil() as u32);
        // A new size keeps the window (edit mode's chosen card and the
        // command being handled hold its handle): only its surface changes.
        use crate::device_battery::view::{remake,Remake,View};
        match remake(self.inbox.as_ref().map(View::shape),(chosen,self.dpi,dimensions)) {
            Remake::Make => {
                let view=View::classed(self.gfx.clone(),&self.theme,chosen,dimensions,self.dpi,Some(proc),w!("NopBarInboxGlass"))?;
                SetClassLongPtrW(view.hwnd,GCL_STYLE,CS_DBLCLKS.0 as isize);
                crate::desklayer::privacy::register(view.hwnd,false);
                crate::desklayer::icons::register_card(view.hwnd,0);
                self.inbox=Some(view);
                self.inbox_size=size;
            }
            Remake::Resize => {
                if let Some(i)=self.inbox.as_mut() {i.resize(chosen,dimensions,self.dpi)?;}
                self.inbox_size=size;
            }
            Remake::Keep => {}
        }
        let i=self.inbox.as_ref().unwrap();
        if let Some(p)=crate::desklayer::place::from_px(r,&mons){crate::desklayer::icons::remember_place(i.hwnd,p);}
        crate::desklayer::edit::remember(i.hwnd,crate::desklayer::edit::Key::Inbox,Some(chosen),crate::desklayer::edit::Preview::Inbox(data.rows.to_vec(),data.visible.to_vec(),data.remaining));
        i.aim(r);
        i.frame(&self.theme,crate::desklayer::card::Kind::Inbox,|dc,gfx,theme|{
            widgets::inbox_content(dc,gfx,theme,&data,dimensions,crate::desklayer::hover::alpha(i.hwnd))
        })?;
        self.inbox_pos=(r.left,r.top);
        i.place(r);
        Ok(())
    }
}
unsafe extern "system" fn proc(h: HWND, m: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    if m==WM_WINDOWPOSCHANGING{crate::desklayer::win::pin(crate::desklayer::layer::Layer::Desktop,h,&mut *(l.0 as *mut WINDOWPOS));}
    if m==crate::desklayer::edit::COMMAND {for a in crate::desklayer::edit::take_actions(h){let visibility=a.clone();widget_action(a);crate::desklayer::edit::apply_owner_visibility(h,&visibility);}return LRESULT(0);}
    if matches!(m, WM_CREATE | WM_NCCREATE | WM_NCDESTROY | WM_DESTROY) {
        return DefWindowProcW(h, m, w, l);
    }

    if (m==WM_CONTEXTMENU || (m==WM_LBUTTONDOWN && {let mut p=POINT::default();let _=GetCursorPos(&mut p);crate::desklayer::edit::control_at(h,p)})) && with(|r|r.inbox.as_ref().is_some_and(|i|i.hwnd==h)).unwrap_or(false) {
        let mut at=POINT::default();let _=GetCursorPos(&mut at);
        let data=with(|r|(r.theme.clone(),r.dpi));
        if let Some((theme,dpi))=data {
            if let Some(id @ 10..=12)=crate::desklayer::widget::menu(&theme,at,dpi,vec![]) {
                with(|r| {
                    let desk=super::hub::current_desktop();let size=crate::desklayer::widget::Size::ALL[(id-10) as usize];
                    let Some(current)=crate::desklayer::icons::card(h) else {return;};
                    if crate::desklayer::widget::snap(&crate::desklayer::widget::desk(dpi),&crate::monitors::all(),current,Some(size),&crate::desklayer::icons::widget_rects(Some(h))).is_none() {return;}
                    if let Some((_,s))=r.store.data.sizes.iter_mut().find(|(d,_)|d==&desk) {*s=size;}else{r.store.data.sizes.push((desk,size));}
                    let _=r.draw_inbox();r.store.save(&mut RealDisk::new());
                });
            }
        }
        return LRESULT(0);
    }
    if m == WM_IME_SETCONTEXT {
        return DefWindowProcW(
            h,
            m,
            w,
            LPARAM(l.0 & !(ISC_SHOWUICOMPOSITIONWINDOW as isize)),
        );
    }
    let mut handled = true;
    let answer = with(|r| {
        let bar = r.bar.as_ref().is_some_and(|v| v.hwnd == h);
        if !bar {
            let x = (l.0 & 0xffff) as i16 as f32 / r.dpi;
            let y = ((l.0 >> 16) & 0xffff) as i16 as f32 / r.dpi;
            let (v, folded, overflow) = r.indices();
            let at = if (widgets::ROW_TOP..widgets::ROW_TOP + v.len() as f32 * widgets::ROW_H).contains(&y) {
                v.get(((y - widgets::ROW_TOP) / widgets::ROW_H) as usize).copied()
            } else {
                None
            };
            match m {
                crate::desklayer::hover::REDRAW | crate::desklayer::backdrop::WM_BACKDROP => {let _=r.draw_inbox();}
                WM_TIMER => {
                    if r.flash
                        .as_ref()
                        .is_none_or(|(_, at)| at.elapsed().as_secs_f32() >= 0.9)
                    {
                        let _ = KillTimer(Some(h), ANIMATE);
                        r.flash = None;
                    }
                    let _ = r.draw_inbox();
                }
                WM_MOUSEACTIVATE => return LRESULT(MA_NOACTIVATE as isize),
                WM_LBUTTONDOWN if !crate::desklayer::edit::active() && r.inbox.as_ref().is_some_and(|i| crate::desklayer::grab::grabbable(crate::desklayer::card::Kind::Inbox, i.dimensions.0, (x, y), false, Some((widgets::ROW_TOP, widgets::ROW_TOP + v.len() as f32 * widgets::ROW_H)))) => {
                    // Anywhere but a row (its check, its text) or the
                    // button: a click, or a drag past 6 px.
                    if let Some(i) = &r.inbox {
                        crate::desklayer::grab::press(h, Some(i.size));
                    }
                }
                WM_MOUSEMOVE if crate::desklayer::grab::moved(h) => {}
                crate::desklayer::drag_hint::LIFTED => {
                    if let Some(i) = &r.inbox {
                        i.clear(r.theme.is_light);
                    }
                    crate::desklayer::drag_hint::hidden(h, w, &r.gfx.compositor);
                }
                crate::desklayer::drag_hint::LANDED => {
                    if let Some(i) = &r.inbox {
                        i.lifted.set(false);
                    }
                    if r.draw_inbox().is_ok() {
                        crate::desklayer::drag_hint::presented(h, w, &r.gfx.compositor);
                    }
                }
                WM_CAPTURECHANGED => {
                    crate::desklayer::grab::cancel(h);
                }
                WM_LBUTTONUP if matches!(
                    crate::desklayer::grab::release(h),
                    crate::desklayer::grab::Up::Dropped | crate::desklayer::grab::Up::Suppressed
                ) => {}
                WM_MOUSEMOVE => {
                    let mut track = TRACKMOUSEEVENT {
                        cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                        dwFlags: TME_LEAVE,
                        hwndTrack: h,
                        dwHoverTime: 0,
                    };
                    let _ = TrackMouseEvent(&mut track);
                    if r.hover != at {
                        r.hover = at;
                        let _ = r.draw_inbox();
                    }
                }
                WM_MOUSELEAVE => {
                    r.hover = None;
                    let _ = r.draw_inbox();
                }
                WM_LBUTTONUP if !crate::desklayer::edit::active() => {
                    if let Some(idx) = at {
                        r.click = Some(capture::PendingClick::new(&r.store.data.rows, idx));
                    } else if y >= widgets::ROW_TOP + v.len().max(1) as f32 * widgets::ROW_H {
                        if folded > 0 {
                            r.expanded = !r.expanded;
                        } else if overflow > 0 {
                            r.offset += 8;
                        }
                        let _ = r.draw_inbox();
                    }
                }
                WM_LBUTTONDBLCLK => {
                    r.click = None;
                    if let Some(idx) = at {
                        let row = r.store.data.rows[idx].clone();
                        r.open();
                        r.editor = Editor::default();
                        r.editor.insert(&row.text);
                        r.edit.id = Some(row.id);
                        r.edit.time = Some(row.remind);
                        r.lead = row.lead;
                        r.target = 0;
                        r.draw_bar();
                    }
                }
                WM_MOUSEWHEEL => {
                    if ((w.0 >> 16) as i16) < 0 {
                        if overflow > 0 {
                            r.offset += 1;
                        }
                    } else {
                        r.offset = r.offset.saturating_sub(1);
                    }
                    let _ = r.draw_inbox();
                }
                _ => {
                    let _ = x;
                    handled = false;
                }
            }
            return LRESULT(0);
        }
        let v = r.bar.as_ref().unwrap();
        let dpi = v.dpi;
        if m == WM_NCHITTEST {
            let mut p = POINT {
                x: (l.0 & 0xffff) as i16 as i32,
                y: ((l.0 >> 16) & 0xffff) as i16 as i32,
            };
            let _ = ScreenToClient(h, &mut p);
            let (x, y) = (
                p.x as f32 / dpi - capture::MARGIN,
                p.y as f32 / dpi - capture::MARGIN,
            );
            return LRESULT(
                if (0. ..560.).contains(&x)
                    && (0. ..if r.popup {
                        214.
                    } else if r.tabs {
                        88.
                    } else {
                        56.
                    })
                        .contains(&y)
                {
                    HTCLIENT as isize
                } else {
                    HTTRANSPARENT as isize
                },
            );
        }
        match m {
            WM_TIMER => r.draw_bar(),
            WM_CLOSE => r.close(false),
            WM_ACTIVATE if w.0 & 0xffff == WA_INACTIVE as usize => {
                if r.exiting.is_none() {
                    r.close(false);
                }
            }
            WM_IME_STARTCOMPOSITION => {
                r.active_editor().composing = true;
            }
            WM_IME_COMPOSITION => {
                r.active_editor().compose(h, l.0 as u32);
                if !r.changing {
                    r.edit.time = None;
                }
            }
            WM_IME_ENDCOMPOSITION => {
                r.active_editor().composing = false;
                r.active_editor().composition.clear();
            }
            WM_IME_CHAR => {}
            WM_KEYUP if w.0 == 13 => r.active_editor().suppress_enter = false,
            WM_KEYDOWN => {
                let shift = GetKeyState(VK_SHIFT.0 as i32) < 0;
                let ctrl = GetKeyState(VK_CONTROL.0 as i32) < 0;
                if w.0 == 13 && r.active_editor().composing {
                    r.active_editor().suppress_enter = true;
                    handled = false;
                    return LRESULT(0);
                }
                if w.0 == 13 && r.active_editor().suppress_enter {
                    return LRESULT(0);
                }
                if r.changing {
                    if w.0 == 13 {
                        if let Some(f) = time::parse(
                            &r.change_editor.field.text(),
                            time::now(),
                            crate::i18n::current(),
                            date_hour(),
                        ) {
                            r.edit.time = Some(Some(f.at));
                            r.changing = false;
                            r.popup = false;
                        }
                    } else if w.0 == 27 {
                        r.changing = false;
                    } else {
                        r.change_editor.edit_key(h, w.0 as u32, ctrl, shift);
                    }
                } else {
                    match capture::key(
                        w.0 as u32,
                        shift,
                        r.editor.composing,
                        r.editor.suppress_enter,
                    ) {
                        Key::Send | Key::NewNote => {
                            r.submit(shift);
                            r.editor.suppress_enter = true;
                        }
                        Key::Close => r.close(false),
                        Key::Next | Key::Previous => {
                            r.target = (r.target + 1) % 2;
                            r.tabs = true;
                        }
                        Key::Recall => {
                            if let Some(row) = r.edit.last.as_ref().and_then(|id| {
                                r.store
                                    .data
                                    .rows
                                    .iter()
                                    .find(|x| &x.id == id && x.source.is_none())
                            }) {
                                r.editor = Editor::default();
                                r.editor.insert(&row.text);
                                r.target = 0;
                                r.edit.id = Some(row.id.clone());
                                r.edit.time = Some(row.remind);
                                r.lead = row.lead;
                            }
                        }
                        Key::Native => r.editor.edit_key(h, w.0 as u32, ctrl, shift),
                    }
                }
            }
            WM_CHAR => {
                if w.0 >= 32 {
                    if r.changing {
                        r.change_editor.character(w.0 as u16);
                    } else {
                        r.editor.character(w.0 as u16);
                        r.edit.time = None;
                    }
                }
            }
            WM_LBUTTONDOWN => {
                let x = (l.0 & 0xffff) as i16 as f32 / dpi - capture::MARGIN;
                let y = ((l.0 >> 16) & 0xffff) as i16 as f32 / dpi - capture::MARGIN;
                let found = r.found();
                let (left, right) = widgets::input_edges(
                    &r.gfx,
                    r.target,
                    r.tabs,
                    if r.changing { None } else { found.as_ref() },
                    time::now(),
                )
                .unwrap_or((95., 454.));
                if y < 56. && x > right + 12. && found.is_some() && !r.changing {
                    r.popup = !r.popup;
                    r.tabs = false;
                } else if r.popup && (124. ..152.).contains(&y) {
                    let idx = ((x - (560. - 252. + 12.)) / 76.).floor() as usize;
                    if idx < 3 {
                        r.lead = [0, 5, 10][idx];
                    }
                } else if r.popup && y >= 164. {
                    if x > 458. {
                        r.edit.time = Some(None);
                        r.popup = false;
                    } else {
                        r.changing = true;
                        r.change_editor = Editor::default();
                    }
                } else if x < left - 12. && y < 56. {
                    r.target = (r.target + 1) % 2;
                    r.tabs = true;
                } else if y < 56. {
                    let text = r.active_editor().displayed();
                    if let Ok((layout, _, _)) = r.gfx.text_layout(
                        &text,
                        crate::fonts::THEME_DEFAULT,
                        20.,
                        DWRITE_FONT_WEIGHT_NORMAL,
                    ) {
                        let (mut trailing, mut inside, mut hit) = (
                            Default::default(),
                            Default::default(),
                            DWRITE_HIT_TEST_METRICS::default(),
                        );
                        let (mut caret_x, mut caret_y) = (0., 0.);
                        let _ = layout.HitTestTextPosition(
                            r.active_editor().caret(),
                            false,
                            &mut caret_x,
                            &mut caret_y,
                            &mut hit,
                        );
                        let scroll = (caret_x - (right - left - 3.)).max(0.);
                        let _ = layout.HitTestPoint(
                            (x - left + scroll).max(0.),
                            0.,
                            &mut trailing,
                            &mut inside,
                            &mut hit,
                        );
                        r.active_editor().click(
                            hit.textPosition + if trailing.as_bool() { hit.length } else { 0 },
                            false,
                        );
                    }
                }
            }
            _ => handled = false,
        }
        if r.editor.field.text().trim().is_empty() {
            r.edit.reset();
        }
        if m != WM_TIMER {
            r.draw_bar();
        }
        LRESULT(0)
    });
    if handled {
        answer.unwrap_or_else(|| DefWindowProcW(h, m, w, l))
    } else {
        DefWindowProcW(h, m, w, l)
    }
}

pub fn widget_action(action:crate::desklayer::edit::Action){
    use crate::desklayer::{edit::{self,Action},widget};
    with(|r|{
        let mons=crate::monitors::all();let desk=super::hub::current_desktop();
        if !edit::allow_geometry(r.inbox.as_ref().map(|i| i.hwnd), &action, &mons) { return; }
        r.click=None;
        let placement=match action {
            Action::Place(p)=>Some((None,p)),
            Action::Resize(p,size)=>Some((Some(size),p)),
            Action::Add{size,monitor,..}=>mons.get(monitor).and_then(|m|widget::auto(&widget::desk(m.scale()),&mons,monitor,size,&crate::desklayer::icons::widget_rects(r.inbox.as_ref().map(|i|i.hwnd)))).map(|p|(Some(size),p)),
            _=>None,
        };
        if let Some((size,p))=placement {
            if let Some(p)=crate::desklayer::place::from_px(p,&mons){if let Some((_,old))=r.store.data.places.iter_mut().find(|(d,_)|d==&desk){*old=p;}else{r.store.data.places.push((desk.clone(),p));}}
            if let Some(size)=size{if let Some((_,old))=r.store.data.sizes.iter_mut().find(|(d,_)|d==&desk){*old=size;}else{r.store.data.sizes.push((desk,size));}}
            r.store.save(&mut RealDisk::new());
        }
        let _=unsafe{r.draw_inbox()};
    });edit::refresh();
}
