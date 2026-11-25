//! Screen readers in the settings window: a UI Automation tree of the
//! controls the page shows now (crate::a11y), built from the same hits the
//! mouse uses, so every control a reader names is one that can be
//! clicked; every kind of control drawn is in it (a11y_key matches every
//! Hit: a new kind of control cannot be left out without the compiler
//! saying so).
//!
//! Each control's key names what it is, not where it is: the page, the
//! setting's row key (and the display whose dock it sets), a pin's path,
//! a display's key, an input rule's app. A provider a
//! reader kept from another page, another display or a list that changed
//! finds no control with its key: it answers "not available" and does
//! nothing (a11y::publish drops it), and never acts on whatever now sits
//! at the same index.
//!
//! Names: a row's control by its label (a choice's option as "label:
//! option", a list's options as menu items), with the row's grey line and
//! any note under it (the setting not working as chosen) as its help text;
//! a slider with its range (RangeValue: a reader can set it), a display on
//! the Dock page's map by its dock ("Dock 2, DELL U2720Q, 2560 × 1440,
//! custom settings"), pins by their apps, repeated buttons with what they
//! are for ("Live Island, Open settings", "Work, Restore"), the search box
//! as an edit field whose text a reader can set. The page's sentence and
//! the window's last note are text in the tree. Invoke / Toggle / Expand
//! click the control, Focus moves the keyboard focus there.

use windows::Win32::Foundation::{HWND, POINT, RECT};
use windows::Win32::Graphics::Gdi::ClientToScreen;
use windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow;

use super::model::{self, Ctl};
use super::{App, Hit, NOTE_H, win_h, PAD, SIDEBAR, win_w};
use crate::a11y::{self, Act, Node, Range, Role, Tree};
use crate::i18n::{t, tf};

/// A control's key from its parts (pure: tested).
pub(super) fn compose(parts: &[&str]) -> String {
    parts.iter().map(|p| p.replace('|', "/")).collect::<Vec<_>>().join("|")
}

/// "Item, action" (the language's list separator).
fn named(item: &str, action: &str) -> String {
    a11y::join(&[item.to_string(), action.to_string()])
}

/// Sentences one after another (empty ones left out).
fn sentences(parts: &[&str]) -> String {
    parts.iter().filter(|p| !p.is_empty()).copied().collect::<Vec<_>>().join(" ")
}

impl App {
    /// The page shown (its title key: stable).
    fn page_id(&self) -> &'static str {
        self.sections.get(self.section).map(|s| s.title).unwrap_or("")
    }

    /// Row `ri`'s identity: the page, its setting key, and for a display's
    /// dock rows the display they set.
    fn row_id(&self, ri: usize) -> Option<String> {
        let r = self.rows().get(ri)?;
        let display = if model::DISPLAY_ROWS.contains(&r.key) || r.key == model::SAME_AS_MAIN { model::editing().map(|e| e.key).unwrap_or_else(|| "main".into()) } else { String::new() };
        Some(compose(&[self.page_id(), r.key, &display]))
    }

    /// The list of pins shown: whose (a display's own, or the main dock's).
    fn pins_id(&self) -> String {
        let own = model::editing().map(|e| e.key).filter(|k| self.cfg.dock.per_display.get(k).is_some_and(|d| d.pins.is_some()));
        own.unwrap_or_else(|| "main".into())
    }

    /// Control `h`'s key (None: not one a reader is shown).
    pub(super) fn a11y_key(&self, h: Hit) -> Option<String> {
        let row = |ri: usize, kind: &str| self.row_id(ri).map(|r| compose(&[&r, kind]));
        let pin = |i: usize, kind: &str| self.pins().get(i).map(|p| compose(&["pin", &self.pins_id(), &p.path, kind]));
        let rule = |i: usize, kind: &str| self.cfg.input.rules.get(i).map(|r| compose(&["rule", &r.app, kind]));
        // A calendar link by its hash (never the link itself).
        let cal = |i: usize, kind: &str| self.cfg.calendar.feeds.get(i).map(|f| compose(&["cal", &crate::agenda::source_key(&f.url), kind]));
        Some(match h {
            Hit::Chat(a) => compose(&["chat",a]),
            Hit::Tab(i) => compose(&["tab", model::Tab::ALL.get(i)?.title()]),
            Hit::Section(i) => compose(&["page", self.sections.get(i)?.title]),
            Hit::Toggle(ri) => row(ri, "toggle")?,
            Hit::Slider(ri, ..) => row(ri, "slider")?,
            Hit::Choice(ri, oi) => row(ri, &format!("choice{oi}"))?,
            Hit::Menu(ri) => row(ri, "menu")?,
            Hit::MenuItem(ri, oi) => row(ri, &format!("item{oi}"))?,
            Hit::Swatch(ri, oi) => row(ri, &format!("swatch{oi}"))?,
            Hit::Info(id) if id >= super::tooltip::CARD_TIP => compose(&["info", super::tooltip::CARD_TIPS.get(id - super::tooltip::CARD_TIP)?.1]),
            Hit::Info(ri) => row(ri, "info")?,
            Hit::MenuDismiss => return None,
            Hit::NewNote(ri) => row(ri, "new-note")?,
            Hit::Button(ri) => row(ri, "button")?,
            Hit::Fold(ri) => row(ri, "fold")?,
            Hit::Shortcut(ri) => row(ri, "shortcut")?,
            Hit::ShortcutReset(ri) => row(ri, "shortcut-reset")?,
            Hit::RowFix(ri) => row(ri, "fix")?,
            Hit::BarCell(ri) => row(ri, "bar")?,
            Hit::Display(i) => compose(&["display", self.page_id(), &self.screens.get(i)?.key]),
            Hit::Identify => "identify".into(),
            Hit::SameUndo => compose(&["same-undo", &model::editing()?.key]),
            Hit::PinRow(i) => pin(i, "row")?,
            Hit::PinIcon(i) => pin(i, "icon")?,
            Hit::PinReset(i) => pin(i, "reset")?,
            Hit::PinRemove(i) => pin(i, "remove")?,
            Hit::PinAdd => compose(&["pins", &self.pins_id(), "add"]),
            Hit::PinAddRunning => compose(&["pins", &self.pins_id(), "running"]),
            Hit::PinsCustomize => compose(&["pins", &model::editing()?.key, "customize"]),
            Hit::AppItem(i) => compose(&["app", &self.apps_menu.as_ref()?.items.get(i)?.1]),
            Hit::Search => "search".into(),
            Hit::SearchClear => "search|clear".into(),
            Hit::FilterAll => compose(&["search", "show-all", self.page_id()]),
            Hit::SearchResult(k) => {
                let f = *self.results().get(k)?;
                let sec = self.sections.get(f.section)?;
                compose(&["result", sec.title, f.row.and_then(|r| sec.rows.get(r)).map(|r| r.key).unwrap_or("")])
            }
            Hit::ResetAll => "reset-all".into(),
            Hit::ResetYes => compose(&["reset", self.page_id(), "yes", &self.confirm_target.as_ref().map(|t| t.0.clone()).unwrap_or_default()]),
            Hit::ResetNo => compose(&["reset", self.page_id(), "no", &self.confirm_target.as_ref().map(|t| t.0.clone()).unwrap_or_default()]),
            Hit::WelcomeCard(k) => compose(&["welcome", "taskbar", &k.to_string()]),
            Hit::WelcomeBack => "welcome|back".into(),
            Hit::WelcomeNext => "welcome|next".into(),
            Hit::WelcomeSkip => "welcome|skip".into(),
            Hit::Theme(i) => compose(&["theme", &self.themes.get(i)?.0]),
            Hit::Link(i) => compose(&["link", super::extra::LINKS.get(i)?.0]),
            Hit::ReportProblem => "report".into(),
            Hit::UpdateCheck => "update|check".into(),
            Hit::UpdateDownload => "update|download".into(),
            Hit::UpdateAuto => "update|auto".into(),
            Hit::UpdateInstall => "update|install".into(),
            Hit::InputAdd => "rule|add".into(),
            Hit::InputSuggest(k) => compose(&["rule-suggest", crate::ime::logic::SUGGESTIONS.get(k)?.0]),
            Hit::InputMode(i) => rule(i, "mode")?,
            Hit::InputRemove(i) => rule(i, "remove")?,
            Hit::CalUrl => "cal|url".into(),
            Hit::CalAdd => "cal|add".into(),
            Hit::CalOpen => "cal|open".into(),
            Hit::CalName => "cal|name".into(),
            Hit::CalRename(i) => cal(i, "rename")?,
            Hit::CalRemove(i) => cal(i, "remove")?,
        })
    }

    /// Row `ri`'s help: its grey line, then the note under it (why it
    /// does not work as chosen), if any.
    fn row_help(&self, ri: usize) -> String {
        let Some(r) = self.rows().get(ri) else { return String::new() };
        let note = self.row_note(r.key).map(|(s, _)| s).unwrap_or_default();
        sentences(&[&self.row_hint(r), &note])
    }

    /// The question the reset / "Same as Dock 1" buttons answer.
    fn confirm_text(&self) -> (String, &'static str) {
        use super::reset::Confirm;
        let dock = self.dock_name(self.display_sel);
        match self.confirm {
            Some(Confirm::SameAsMain) => (tf("settings.dock.same.confirm", &[("dock", &dock)]), t("settings.dock.same.yes")),
            _ => (t("settings.reset.all.confirm").to_string(), t("settings.reset.confirm")),
        }
    }

    /// What a reader hears for control `h` (None: not named, left out).
    pub(super) fn a11y_node(&self, h: Hit, rect: RECT) -> Option<Node> {
        let rows = self.rows();
        let label = |ri: usize| rows.get(ri).map(|r| self.row_label(r));
        let key = self.a11y_key(h)?;
        let on_off = |on: bool| t(if on { "settings.topbar.view.on" } else { "settings.topbar.view.off" });
        let node = match h {
            Hit::Chat(a) => Node::new(key,Role::Button,t(a),rect).invoke(),
            Hit::Tab(i) => Node::new(key, Role::Button, t(model::Tab::ALL.get(i)?.title()), rect).invoke().toggle(model::Tab::ALL.get(i) == Some(&self.tab())),
            Hit::Section(i) => Node::new(key, Role::Button, t(self.sections.get(i)?.title), rect).invoke().toggle(i == self.section),
            Hit::Toggle(ri) => {
                let Some(Ctl::Toggle { get, .. }) = rows.get(ri).map(|r| &r.ctl) else { return None };
                let help = self.row_help(ri);
                Node::new(key, Role::Button, label(ri)?, rect).toggle(get(&self.cfg)).help(help)
            }
            Hit::Choice(ri, oi) => {
                let Some(Ctl::Choice { opts, get, .. }) = rows.get(ri).map(|r| &r.ctl) else { return None };
                let name = format!("{}: {}", label(ri)?, model::opt_text(opts.get(oi)?));
                Node::new(key, Role::Button, name, rect).invoke().toggle(get(&self.cfg) == oi).help(self.row_help(ri))
            }
            Hit::Slider(ri, ..) => {
                let Some(Ctl::Slider { get, step, unit, min, max, .. }) = rows.get(ri).map(|r| &r.ctl) else { return None };
                let u = model::unit_text(unit);
                let range = tf("settings.a11y.slider", &[("min", &model::fmt_value(*min, *step, u)), ("max", &model::fmt_value(*max, *step, u))]);
                let v = get(&self.cfg);
                Node::new(key, Role::Slider, label(ri)?, rect)
                    .value(model::fmt_value(v, *step, u))
                    .range(Range { value: v as f64, min: *min as f64, max: *max as f64, step: *step as f64 })
                    .help(sentences(&[&range, &self.row_help(ri)]))
            }
            Hit::Menu(ri) => {
                let (opts, get, _) = rows.get(ri)?.ctl.options()?;
                let cur = get(&self.cfg).min(opts.len().saturating_sub(1));
                Node::new(key, Role::Button, label(ri)?, rect).value(model::opt_text(opts.get(cur)?)).expand(self.menu.map(|m| m.0) == Some(ri)).help(self.row_help(ri))
            }
            // An accent color: "Accent color: Blue", picked or not.
            Hit::Swatch(ri, oi) => {
                let (opts, get, _) = rows.get(ri)?.ctl.options()?;
                let name = format!("{}: {}", label(ri)?, model::opt_text(opts.get(oi)?));
                Node::new(key, Role::Button, name, rect).invoke().toggle(get(&self.cfg) == oi).help(self.row_help(ri))
            }
            // The "i": "<label>: Details", its description as the help (the
            // row's control carries it too).
            Hit::Info(id) => {
                let text = self.tip_text(id);
                if text.is_empty() {
                    return None;
                }
                Node::new(key, Role::Button, format!("{}: {}", self.tip_title(id), t("settings.a11y.details")), rect).invoke().help(text)
            }
            Hit::MenuItem(ri, oi) => {
                let (opts, get, _) = rows.get(ri)?.ctl.options()?;
                Node::new(key, Role::MenuItem, model::opt_text(opts.get(oi)?), rect).invoke().toggle(get(&self.cfg) == oi)
            }
            Hit::NewNote(ri) => Node::new(key, Role::Button, t("notes.new"), rect).invoke().help(self.row_help(ri)),
            Hit::Button(ri) => {
                let Some(Ctl::Button { text }) = rows.get(ri).map(|r| &r.ctl) else { return None };
                Node::new(key, Role::Button, t(text), rect).invoke().help(sentences(&[&label(ri)?, &self.row_help(ri)]))
            }
            Hit::Fold(ri) => Node::new(key, Role::Button, label(ri)?, rect).expand(self.fold_open(rows, ri)),
            Hit::Shortcut(ri) => {
                let Some(Ctl::Shortcut { id }) = rows.get(ri).map(|r| &r.ctl) else { return None };
                Node::new(key, Role::Button, label(ri)?, rect).invoke().value(crate::shortcuts::display_spec(&self.cfg.shortcut(id))).help(self.row_help(ri))
            }
            Hit::ShortcutReset(ri) => Node::new(key, Role::Button, named(&label(ri)?, t("settings.a11y.reset")), rect).invoke(),
            Hit::RowFix(ri) => {
                let (_, fix) = self.row_note(rows.get(ri)?.key)?;
                Node::new(key, Role::Button, fix?.label(), rect).invoke()
            }
            Hit::BarCell(ri) => {
                let r = rows.get(ri)?;
                let on = self.cfg.topbar.enabled && super::topbar_view::shown(&self.cfg, r.key);
                Node::new(key, Role::Button, label(ri)?, rect).invoke().value(on_off(on))
            }
            Hit::Display(i) => {
                let n = Node::new(key, Role::Button, self.display_a11y_name(i), rect).invoke();
                if self.per_display() {
                    n.toggle(i == self.display_sel)
                } else {
                    n.help(t("settings.dock.map.hint"))
                }
            }
            Hit::Identify => Node::new(key, Role::Button, t("settings.dock.identify"), rect).invoke().help(sentences(&[t("settings.dock.identify.hint"), t("settings.dock.map.numbers")])),
            Hit::SameUndo => Node::new(key, Role::Button, t("settings.undo"), rect).invoke().help(tf("settings.dock.same.done", &[("dock", &self.dock_name(self.display_sel))])),
            Hit::PinRow(i) => {
                let p = self.pins().get(i)?;
                // A divider is named, without its inner path.
                let (name, path) = if p.is_divider() { (t("dock.item.divider").to_string(), "") } else { (p.name.clone(), p.path.as_str()) };
                let help = if self.pins_locked() { sentences(&[path, t("settings.pins.read_only")]) } else { path.to_string() };
                Node::new(key, Role::Button, name, rect).help(help)
            }
            Hit::PinIcon(i) => Node::new(key, Role::Button, named(&self.pins().get(i)?.name, t("settings.pins.icon")), rect).invoke(),
            Hit::PinReset(i) => Node::new(key, Role::Button, named(&self.pins().get(i)?.name, t("settings.pins.reset")), rect).invoke(),
            Hit::PinRemove(i) => {
                let p = self.pins().get(i)?;
                let name = if p.is_divider() { t("dock.menu.remove_divider").to_string() } else { tf("settings.pins.unpin_named", &[("app", &p.name)]) };
                Node::new(key, Role::Button, name, rect).invoke()
            }
            Hit::PinAdd => Node::new(key, Role::Button, t("settings.pins.add"), rect).invoke(),
            Hit::PinAddRunning => Node::new(key, Role::Button, t("settings.pins.add_running"), rect).expand(self.apps_menu.as_ref().is_some_and(|m| !m.rules)),
            Hit::PinsCustomize => Node::new(key, Role::Button, t("settings.pins.customize"), rect).invoke().help(tf("settings.pins.same_as_main", &[("dock", &self.dock_name(self.display_sel))])),
            Hit::AppItem(i) => Node::new(key, Role::MenuItem, self.apps_menu.as_ref()?.items.get(i)?.0.clone(), rect).invoke(),
            Hit::Search => Node::new(key, Role::Edit, t("settings.search.placeholder"), rect).value(self.query()),
            Hit::SearchClear => Node::new(key, Role::Button, t("settings.search.clear"), rect).invoke(),
            Hit::FilterAll => Node::new(key, Role::Button, t("settings.search.show_all"), rect).invoke().help(tf("settings.search.filtered", &[("q", self.query().trim())])),
            Hit::SearchResult(k) => {
                let f = *self.results().get(k)?;
                let sec = self.sections.get(f.section)?;
                let name = match f.row.and_then(|r| sec.rows.get(r)) {
                    Some(r) => format!("{} – {}", self.row_label(r), t(sec.title)),
                    None => t(sec.title).to_string(),
                };
                Node::new(key, Role::Button, name, rect).invoke()
            }
            Hit::ResetAll => Node::new(key, Role::Button, t("settings.reset.confirm"), rect).invoke().help(t("settings.reset.all")),
            Hit::ResetYes => {
                let (q, yes) = self.confirm_text();
                Node::new(key, Role::Button, yes, rect).invoke().help(q)
            }
            Hit::ResetNo => Node::new(key, Role::Button, t("settings.button.cancel"), rect).invoke().help(self.confirm_text().0),
            Hit::WelcomeCard(k) => {
                let name = super::welcome::TASKBAR_CARD_NAMES.get(k)?;
                let chosen = if self.cfg.hide_taskbar { 0 } else { 1 };
                Node::new(key, Role::Button, t(name), rect).invoke().toggle(chosen == k)
            }
            Hit::WelcomeBack => Node::new(key, Role::Button, t("welcome.back"), rect).invoke(),
            Hit::WelcomeNext => Node::new(key, Role::Button, t(self.welcome_next_label()), rect).invoke(),
            Hit::WelcomeSkip => Node::new(key, Role::Button, t("welcome.skip"), rect).invoke(),
            Hit::Theme(i) => Node::new(key, Role::Button, self.themes.get(i)?.1.clone(), rect).invoke().toggle(self.cfg.theme.source == self.themes.get(i)?.0),
            Hit::Link(i) => Node::new(key, Role::Button, t(super::extra::LINKS.get(i)?.0), rect).invoke(),
            Hit::ReportProblem => Node::new(key, Role::Button, t("about.report.button"), rect).invoke().help(t("about.report.hint")),
            Hit::UpdateCheck => Node::new(key, Role::Button, t("about.updates.check_now"), rect).invoke().enabled(!self.upd_checking),
            Hit::UpdateDownload => Node::new(key, Role::Button, t("about.updates.download"), rect).invoke(),
            Hit::UpdateInstall => Node::new(key, Role::Button, t("about.updates.update_now"), rect).invoke(),
            Hit::UpdateAuto => Node::new(key, Role::Button, t("about.updates.auto"), rect).toggle(self.cfg.updates.auto_check),
            Hit::InputAdd => Node::new(key, Role::Button, t("settings.input.add"), rect).expand(self.apps_menu.as_ref().is_some_and(|m| m.rules)).help(t("settings.input.rules")),
            Hit::InputSuggest(k) => {
                let (_, name, mode) = crate::ime::logic::SUGGESTIONS.get(k)?;
                Node::new(key, Role::Button, named(super::input::suggestion_name(name), t("settings.input.suggest_add")), rect).invoke().help(super::input::mode_text(*mode))
            }
            Hit::InputMode(i) => {
                let r = self.cfg.input.rules.get(i)?;
                Node::new(key, Role::Button, r.name.clone(), rect).invoke().value(super::input::mode_text(crate::ime::logic::RuleMode::parse(&r.mode))).help(t("settings.input.rules"))
            }
            Hit::InputRemove(i) => Node::new(key, Role::Button, named(&self.cfg.input.rules.get(i)?.name, t("settings.a11y.remove")), rect).invoke(),
            // A protected field: its value is a stand-in, never the link.
            Hit::CalUrl => Node::new(key, Role::Edit, t("settings.calendar.url_name"), rect).secret().value(super::calendar::masked(&self.cal.url)).help(t("settings.calendar.feeds.hint")),
            Hit::CalAdd => Node::new(key, Role::Button, t("settings.calendar.add"), rect).invoke().help(t("settings.calendar.feeds")),
            Hit::CalOpen => Node::new(key, Role::Button, t("settings.calendar.add_feed"), rect).invoke().help(t("settings.calendar.feeds.hint")),
            Hit::CalName => Node::new(key, Role::Edit, t("settings.a11y.name"), rect).value(self.cal.rename.as_ref().map(|(_, s)| s.clone()).unwrap_or_default()),
            Hit::CalRename(i) | Hit::CalRemove(i) => {
                let f = self.cfg.calendar.feeds.get(i)?;
                let name = if f.name.trim().is_empty() { crate::agenda::host(&f.url) } else { f.name.clone() };
                let what = match h {
                    Hit::CalRename(_) => t("settings.calendar.rename"),
                    _ => t("settings.a11y.remove"),
                };
                Node::new(key, Role::Button, named(&name, what), rect).invoke().help(crate::agenda::host(&f.url))
            }
            Hit::MenuDismiss => return None,
        };
        Some(node)
    }

    /// Text in the tree that is no control: the page's sentence first, the
    /// window's note last (a save that failed).
    fn a11y_text(&self, origin: (i32, i32)) -> (Option<Node>, Option<Node>) {
        let s = self.scale;
        let rtl = crate::i18n::rtl();
        let (l, r) = if rtl { (PAD, win_w() - SIDEBAR - PAD) } else { (SIDEBAR + PAD, win_w() - PAD) };
        let intro = self.sections.get(self.section).filter(|sec| self.welcome.is_none() && !sec.intro.is_empty()).map(|sec| {
            let y = self.content_top() - self.scroll;
            Node::new(compose(&["intro", sec.title]), Role::Text, t(sec.intro), a11y::px_rect(origin, s, l, y, r - l, 20.0))
        });
        let note = (!self.note.is_empty() && self.welcome.is_none()).then(|| Node::new("note", Role::Text, self.note.clone(), a11y::px_rect(origin, s, l, win_h() - NOTE_H, r - l, NOTE_H)));
        (intro, note)
    }

    /// The tree of what is shown now.
    fn a11y_tree(&self) -> Tree {
        let mut origin = POINT::default();
        unsafe {
            let _ = ClientToScreen(self.hwnd, &mut origin);
        }
        let s = self.scale;
        let (intro, note) = self.a11y_text((origin.x, origin.y));
        let mut nodes: Vec<Node> = intro.into_iter().collect();
        for (r, h) in &self.hits {
            let rect = a11y::px_rect((origin.x, origin.y), s, r.left, r.top, r.right - r.left, r.bottom - r.top);
            if let Some(n) = self.a11y_node(*h, rect) {
                if !nodes.iter().any(|x| x.key == n.key) {
                    nodes.push(n);
                }
            }
        }
        nodes.extend(note);
        let focus = self.focus.and_then(|f| self.a11y_key(f)).filter(|k| nodes.iter().any(|n| &n.key == k));
        let active = unsafe { GetForegroundWindow() } == self.hwnd;
        Tree { role: Role::Group, name: t("settings.window_title").to_string(), nodes, focus, focusable: true, active, focus_in: None }
    }

    /// Publishes the tree (only while a reader reads this window).
    pub(super) fn publish_a11y(&self) {
        if a11y::wanted(self.hwnd) {
            a11y::publish(self.hwnd, self.a11y_tree());
        }
    }

    /// A reader asked control `key` to do something. Only a control shown
    /// now under that very key acts (see the module comment).
    pub(super) fn a11y_act(&mut self, key: &str, act: Act) {
        if act == Act::Refresh {
            self.publish_a11y();
            return;
        }
        let Some((r, h)) = self.hits.iter().find(|(_, h)| self.a11y_key(*h).as_deref() == Some(key)).copied() else {
            // Gone (another page, display or list): nothing happens.
            self.publish_a11y();
            return;
        };
        match act {
            Act::Focus => {
                // The focus moves; the ring as it was, or with Windows'
                // keyboard cues always on (crate::input).
                self.keyboard = crate::input::reader_focus(self.keyboard);
                self.tip_key = self.keyboard;
                self.focus = Some(h);
                self.redraw();
            }
            // The same path as typing it (the page previewed, narrowed).
            Act::SetValue if h == Hit::Search => {
                let Some(text) = a11y::take_value(self.hwnd, key) else { return };
                self.focus = Some(Hit::Search);
                self.set_search(&text);
            }
            // A slider set to a number (RangeValue): snapped to its steps.
            Act::SetValue if matches!(h, Hit::Slider(..)) => {
                let Some(text) = a11y::take_value(self.hwnd, key) else { return };
                let Hit::Slider(ri, ..) = h else { return };
                if let (Ok(v), Some(Ctl::Slider { get, set, min, max, step, .. })) = (text.trim().parse::<f32>(), self.rows().get(ri).map(|r| &r.ctl)) {
                    let (get, set) = (*get, *set);
                    let v = model::snap(v, *min, *max, *step);
                    if v != get(&self.cfg) {
                        set(&mut self.cfg, v);
                        self.changed(ri);
                    }
                }
            }
            // A calendar link or name typed by a reader.
            Act::SetValue if matches!(h, Hit::CalUrl | Hit::CalName) => {
                let Some(text) = a11y::take_value(self.hwnd, key) else { return };
                self.cal_set_value(h, text);
            }
            Act::Invoke | Act::Toggle | Act::Expand | Act::Collapse => {
                self.keyboard = crate::input::reader_focus(self.keyboard);
                self.click(h, (r.left + r.right) / 2.0);
            }
            _ => {}
        }
    }
}

/// The window is going away: its providers answer "not available".
pub(super) fn forget(hwnd: HWND) {
    a11y::forget(hwnd);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_name_the_control_not_its_place() {
        // A page and a setting, not a row number: the same index on
        // another page is another key.
        let a = compose(&["settings.section.dock", "dock.autohide", "main", "toggle"]);
        let b = compose(&["settings.section.topbar", "topbar.enabled", "", "toggle"]);
        assert_ne!(a, b);
        // The same row for another display's dock is another control.
        let c = compose(&["settings.section.dock", "dock.autohide", "DEL40F2@0000000a", "toggle"]);
        assert_ne!(a, c);
        // A '|' inside a part cannot make two keys alike.
        assert_ne!(compose(&["a|b", "c"]), compose(&["a", "b|c"]));
        assert_eq!(compose(&["pin", "main", r"C:\x.lnk", "remove"]), r"pin|main|C:\x.lnk|remove");
    }

    #[test]
    fn the_names_readers_hear_exist() {
        for k in [
            "settings.a11y.slider",
            "settings.a11y.reset",
            "settings.a11y.remove",
            "settings.a11y.icon",
            "settings.a11y.name",
            "settings.search.clear",
            "settings.pins.unpin_named",
            "settings.pins.read_only",
            "welcome.taskbar.hide.name",
            "welcome.taskbar.keep.name",
        ] {
            assert!(crate::i18n::get(k).is_some(), "{k}");
        }
        let _l = crate::i18n::test_lock("en");
        let s = tf("settings.a11y.slider", &[("min", "32"), ("max", "96")]);
        assert!(s.contains("32") && s.contains("96"), "{s}");
        assert_eq!(sentences(&["A.", "", "B."]), "A. B.");
    }
}
