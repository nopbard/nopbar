//! The dock's right-click menu: Windows-native items, drawn with our own
//! popup.
//!
//! The menu is built inside the dock's borrow, shown outside it (the popup
//! runs a nested message loop), and the picked action runs inside it again.
//! Actions name apps by path, not by index: the item list may change while
//! the menu is open.

use super::*;
use crate::popup::Item as Row;
use crate::i18n::{t as t_, tf};

#[derive(Clone, Debug, PartialEq)]
pub(super) enum DockAction {
    Undo,
    NewNote,
    Companion(u32),
    /// A jump list task: (program, arguments).
    /// (program, arguments, working directory)
    RunTask(String, String, String),
    OpenPackagedFile(String, String),
    CloseAll(String),
    /// Cooperative process shutdown, by resolved executable (not shortcut).
    QuitCompletely(full_quit::Target),
    /// The app's windows in the tray, shown again.
    FromTray(String),
    GatherHere(String),
    NewWindow(String),
    RunAdmin(String),
    OpenLocation(String),
    /// End not-responding processes: each (hung window, pid, process start
    /// time).
    Kill(Vec<(isize, u32, u64)>),
    /// Windows' Task Manager (for apps we may not end).
    TaskManager,
    CopyPath(String),
    Pin(String),
    Unpin(String),
    /// Keep a folder (from a File Explorer window) in the dock as a stack.
    PinFolder(String, String),
    /// Open a stack's folder in File Explorer.
    OpenFolder(String),
    Startup(String),
    OpenTrash,
    EmptyTrash,
    /// A divider at the end of the pinned apps.
    AddDivider,
    /// That divider off the dock (by its path).
    RemoveDivider(String),
    Position(Position),
    AutoHide,
    Settings,
    /// Settings, on the Dock page.
    DockSettings,
    /// The GitHub Sponsors page.
    Sponsor,
    /// Settings' About page, with "Report a problem" run.
    Report,
    Quit,
}

/// What to show: rows, the action of each row id (id = index + 1), where.
pub(super) struct MenuPlan {
    pub rows: Vec<Row>,
    pub actions: Vec<DockAction>,
    pub full_quit: Option<FullQuitWatch>,
    pub at: POINT,
    pub align: crate::popup::Align,
    pub scale: f32,
    pub theme: Theme,
    pub gfx: Rc<Gfx>,
}

#[derive(Default)]
struct Builder {
    rows: Vec<Row>,
    actions: Vec<DockAction>,
    full_quit: Option<FullQuitWatch>,
}

/// One reserved command slot. Only its label and enabled state change.
pub(super) struct FullQuitWatch {
    target: full_quit::Target,
    value: full_quit::Availability,
    row_id: u32,
}

impl FullQuitWatch {
    fn row(&self, value: full_quit::Availability) -> Row {
        let label = if value == full_quit::Availability::Denied { "dock.menu.quit_completely_denied" } else { "dock.menu.quit_completely" };
        Row::new(self.row_id, t_(label)).enabled(value == full_quit::Availability::Ready)
    }
}

struct LiveQuitState {
    watch: FullQuitWatch,
    actions: Rc<RefCell<Vec<DockAction>>>,
    timer: usize,
}
impl LiveQuitState {
    fn take_timer(&mut self) -> usize {
        std::mem::take(&mut self.timer)
    }

    fn stop_timer(&mut self) {
        let timer = self.take_timer();
        if timer != 0 { unsafe { let _ = KillTimer(None, timer); } }
    }
}
thread_local! {
    static LIVE_QUIT: RefCell<Option<LiveQuitState>> = const { RefCell::new(None) };
}

/// Thread timer, scoped to one modal popup. No dock-window timer/message
/// IDs and no cross-thread access to the popup or dock state.
pub(super) struct LiveQuit {
    actions: Rc<RefCell<Vec<DockAction>>>,
}
impl LiveQuit {
    pub fn begin(watch: Option<FullQuitWatch>, actions: Vec<DockAction>) -> Self {
        let actions = Rc::new(RefCell::new(actions));
        if crate::popup::is_tracking() { return Self { actions }; }
        if let Some(watch) = watch.filter(|w| w.value == full_quit::Availability::Pending) {
            let timer = unsafe { SetTimer(None, 0, 16, Some(refresh_full_quit)) };
            LIVE_QUIT.with(|s| *s.borrow_mut() = Some(LiveQuitState { watch, actions: actions.clone(), timer }));
        }
        Self { actions }
    }

    pub fn action(&self, id: u32) -> Option<DockAction> {
        self.actions.borrow().get((id as usize).wrapping_sub(1)).cloned()
    }
}
impl Drop for LiveQuit {
    fn drop(&mut self) {
        LIVE_QUIT.with(|s| {
            let mut s = s.borrow_mut();
            if s.as_ref().is_some_and(|state| Rc::ptr_eq(&state.actions, &self.actions)) {
                s.take().unwrap().stop_timer();
            }
        });
    }
}

unsafe extern "system" fn refresh_full_quit(_: HWND, _: u32, timer: usize, _: u32) {
    let update = LIVE_QUIT.with(|s| {
        let mut s = s.borrow_mut();
        let state = s.as_mut()?;
        // KillTimer does not remove already queued WM_TIMER messages.
        if timer == 0 || state.timer != timer { return None; }
        let value = full_quit::resolved(&state.watch.target)?;
        if value == state.watch.value {
            state.stop_timer();
            return None;
        }
        // A completed (or failed) probe ends polling even if the graphics
        // device cannot redraw this row. Reopening reads the cached result.
        state.stop_timer();
        Some((value, state.watch.row(value)))
    });
    if let Some((value, row)) = update {
        if crate::popup::update_command(row) {
            LIVE_QUIT.with(|s| {
                if let Some(state) = s.borrow_mut().as_mut() {
                    state.watch.value = value;
                    state.stop_timer();
                }
            });
        }
    }
}

impl Builder {
    fn add(&mut self, label: impl Into<String>, a: DockAction) -> &mut Row {
        self.actions.push(a);
        self.rows.push(Row::new(self.actions.len() as u32, label));
        self.rows.last_mut().unwrap()
    }
    fn sub(&mut self, label: impl Into<String>, rows: Vec<Row>) {
        self.rows.push(Row::new(0, label).submenu(rows));
    }
    /// A row for a submenu (ids share this builder's action list).
    fn row(&mut self, label: impl Into<String>, a: DockAction) -> Row {
        self.actions.push(a);
        Row::new(self.actions.len() as u32, label)
    }
    fn sep(&mut self) {
        if self.rows.last().is_some_and(|r| !r.separator) {
            self.rows.push(Row::sep());
        }
    }
}

fn pid_of(h: HWND) -> u32 {
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(h, Some(&mut pid)) };
    pid
}

/// When process `pid` started (FILETIME as u64), to tell it from a later
/// process that got the same id.
fn started(pid: u32) -> Option<u64> {
    use windows::Win32::Foundation::{CloseHandle, FILETIME};
    use windows::Win32::System::Threading::{GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let (mut c, mut e, mut k, mut u) = (FILETIME::default(), FILETIME::default(), FILETIME::default(), FILETIME::default());
        let ok = GetProcessTimes(h, &mut c, &mut e, &mut k, &mut u).is_ok();
        let _ = CloseHandle(h);
        ok.then(|| ((c.dwHighDateTime as u64) << 32) | c.dwLowDateTime as u64)
    }
}

/// "새 창(N)" -> "새 창": drop a trailing one-letter access key.
fn clean_task_title(t: &str) -> String {
    let c: Vec<char> = t.chars().collect();
    let n = c.len();
    if n >= 3 && c[n - 1] == ')' && c[n - 3] == '(' && c[n - 2].is_ascii_alphanumeric() {
        return c[..n - 3].iter().collect::<String>().trim_end().to_string();
    }
    t.to_string()
}

/// Foldable groups of the jump list (popup::Item::fold): on a screen too
/// short for the whole menu, named groups (recent, frequent) move into a
/// submenu first, then the app's tasks; the app's own rows never do.
const FOLD_NAMED: u8 = 2;
const FOLD_TASKS: u8 = 1;

/// Reserve a header, separator and the usual launch/pin/close footer.
fn destination_budget(configured: u32, work_height: f32) -> u32 {
    use crate::popup::{HEADER_H, PAD_Y, ROW_H, SEP_H};
    let fixed = 2.0 * PAD_Y + HEADER_H + SEP_H + 3.0 * ROW_H;
    configured.min(((work_height - fixed) / ROW_H).floor().max(0.0) as u32)
}

/// The app's jump list: each named group under its
/// header ("최근 플레이"), the tasks after, separated; foldable.
fn jump_list_rows(b: &mut Builder, mut tasks: Vec<crate::jumplist::Task>, mut destinations_left: u32) {
    if tasks.is_empty() {
        return;
    }
    // Preserve app order inside each destination group; tasks always follow.
    tasks.sort_by_key(|t| {
        let name = t.category.trim();
        if name.eq_ignore_ascii_case("pinned") || name == t_("dock.menu.pinned_group") { 0 }
        else if name.is_empty() { 2 }
        else { 1 }
    });
    let mut last_cat: Option<String> = None;
    for t in tasks {
        if !t.category.is_empty() {
            if destinations_left == 0 { continue; }
            destinations_left -= 1;
        }
        let fold = if t.category.is_empty() { FOLD_TASKS } else { FOLD_NAMED };
        if last_cat.as_ref() != Some(&t.category) {
            if last_cat.is_some() {
                b.sep();
            }
            // Named groups get a header ("자주 방문" ...).
            if !t.category.is_empty() {
                b.rows.push(Row::new(0, t.category.clone()).head().fold(fold).literal());
            }
        }
        last_cat = Some(t.category.clone());
        // The app's own text (a recent file's name): read as shown.
        let action = match t.packaged_file {
            Some((id, file)) => DockAction::OpenPackagedFile(id, file),
            None => DockAction::RunTask(t.path, t.args, t.dir),
        };
        let row = b.add(clean_task_title(&t.title), action);
        row.fold = fold;
        row.literal = true;
    }
    b.sep();
}

fn window_title(h: HWND) -> String {
    let mut buf = [0u16; 256];
    let n = unsafe { GetWindowTextW(h, &mut buf) }.max(0) as usize;
    let t: String = String::from_utf16_lossy(&buf[..n]).chars().take(60).collect();
    if t.is_empty() { t_("dock.menu.untitled").into() } else { t }
}

/// A window of the app as its menu lists it.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct WinFact {
    pub hwnd: isize,
    pub pid: u32,
    pub title: String,
    /// In front now (checked).
    pub active: bool,
    pub minimized: bool,
}

/// Force quit for a not-responding app: what its processes allow.
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) enum ForceQuit {
    #[default]
    None,
    /// Each (hung window, pid, process start time).
    Kill(Vec<(isize, u32, u64)>),
    /// Only processes we may not end (elevated).
    Denied,
}

/// What an app's menu is built from, read from the dock and its windows
/// (Dock::app_facts), so the rows themselves are pure (app_menu).
#[derive(Clone, Debug, Default)]
pub(super) struct AppFacts {
    pub path: String,
    pub name: String,
    pub pinned: bool,
    /// Store / packaged: no admin run, startup through Windows' page.
    pub packaged: bool,
    /// Its file can be shown in File Explorer.
    pub locatable: bool,
    /// Our Startup-folder shortcut is there.
    pub startup: bool,
    pub force_quit: ForceQuit,
    pub full_quit: full_quit::Availability,
    pub exe: Option<String>,
    pub aumid: Option<String>,
    pub web: bool,
    pub tasks: Vec<crate::jumplist::Task>,
    pub destination_limit: Option<u32>,
    pub windows: Vec<WinFact>,
    /// Some window's center is on another monitor.
    pub elsewhere: bool,
    /// Windows hidden to the tray.
    pub tray: usize,
    /// File Explorer: folders its windows show, not in the dock yet:
    /// (path, name).
    pub folders: Vec<(String, String)>,
}

/// Windows jump-list order: destinations/tasks, app launch, pin, close.
/// Shift exposes startup, file location, admin and copy as direct rows.
fn app_menu(b: &mut Builder, f: AppFacts, shift: bool) {
    let path = f.path;
    // Exceptional recovery and tray commands stay before the shell footer.
    match f.force_quit {
        ForceQuit::Kill(targets) => { b.add(tf("dock.menu.force_quit", &[("name", &f.name)]), DockAction::Kill(targets)); }
        ForceQuit::Denied => {
            b.rows.push(Row::new(0, t_("dock.menu.force_quit_denied")).enabled(false));
            b.add(t_("dock.menu.task_manager"), DockAction::TaskManager);
        }
        ForceQuit::None => {}
    }
    jump_list_rows(b, f.tasks, f.destination_limit.unwrap_or(12));
    for (folder, name) in f.folders {
        b.add(tf("dock.menu.keep_folder", &[("name", &name)]), DockAction::PinFolder(folder, name));
    }
    if f.tray > 0 {
        b.add(tf("dock.menu.from_tray", &[("n", &f.tray.to_string())]), DockAction::FromTray(path.clone()));
    }
    if !path.is_empty() {
        if shift {
            let label = t_(if f.packaged { "dock.menu.startup_dialog" } else { "dock.menu.startup" });
            b.add(label, DockAction::Startup(path.clone())).checked = f.startup;
            if f.locatable { b.add(t_("dock.menu.open_location"), DockAction::OpenLocation(path.clone())); }
            if !f.packaged { b.add(t_("dock.menu.run_admin"), DockAction::RunAdmin(path.clone())); }
            b.add(t_("dock.menu.copy_path"), DockAction::CopyPath(path.clone()));
        }
        b.sep();
        b.add(f.name.clone(), DockAction::NewWindow(path.clone())).literal = true;
    }
    if f.pinned { b.add(t_("dock.menu.unpin"), DockAction::Unpin(path.clone())); }
    else { b.add(t_("dock.menu.pin"), DockAction::Pin(path.clone())); }
    if f.elsewhere { b.add(t_("dock.menu.gather_here"), DockAction::GatherHere(path.clone())); }
    if !f.windows.is_empty() {
        let label = if f.windows.len() > 1 { tf("dock.menu.close_n_windows", &[("n", &f.windows.len().to_string())]) } else { t_("dock.menu.close_window").into() };
        b.add(label, DockAction::CloseAll(path));
    }
    if let Some(target) = f.exe.as_deref().and_then(|exe| full_quit::target(exe, f.aumid.as_deref(), f.web, f.packaged)) {
        if f.full_quit != full_quit::Availability::None {
            if !matches!(b.actions.last(), Some(DockAction::CloseAll(_))) { b.sep(); }
            let row_id = b.add("", DockAction::QuitCompletely(target.clone())).id;
            let watch = FullQuitWatch { target, value: f.full_quit, row_id };
            *b.rows.last_mut().unwrap() = watch.row(f.full_quit);
            b.full_quit = Some(watch);
        }
    }
    while b.rows.last().is_some_and(|r| r.separator) {
        b.rows.pop();
    }
}

/// Store / packaged apps cannot be run as admin or put in the Startup folder.
fn packaged(path: &str, exe: Option<&str>) -> bool {
    path.to_lowercase().starts_with("shell:appsfolder") || exe.is_some_and(|e| e.to_lowercase().contains("\\windowsapps\\"))
}

fn startup_dir() -> Option<std::path::PathBuf> {
    std::env::var("APPDATA").ok().map(|a| std::path::PathBuf::from(a).join("Microsoft\\Windows\\Start Menu\\Programs\\Startup"))
}

/// Our shortcut for an app in the Startup folder. The name carries our
/// prefix and a hash of the app's path, so it never collides with (or
/// removes) a shortcut the user made, nor another app's.
fn startup_link(name: &str, path: &str) -> Option<std::path::PathBuf> {
    let safe: String = name.chars().take(40).map(|c| if r#"\/:*?"<>|"#.contains(c) { '_' } else { c }).collect();
    let mut h: u32 = 0x811C_9DC5;
    for b in path.to_lowercase().bytes() {
        h = (h ^ b as u32).wrapping_mul(0x0100_0193);
    }
    startup_dir().map(|d| d.join(format!("NopBar - {safe} ({h:08x}).lnk")))
}

impl Dock {
    /// Right before a close: `h` is still a window, owned by `pid`, and the
    /// window list has it as that process's window.
    fn owned_now(&self, h: HWND, pid: u32) -> bool {
        let alive = unsafe { IsWindow(Some(h)).as_bool() };
        alive && pid != 0 && pid_of(h) == pid && self.listed(h).is_some_and(|w| w.pid == pid)
    }

    fn find_app(&self, path: &str) -> Option<usize> {
        self.items.iter().position(|it| it.kind == Kind::App && it.path == path)
    }

    /// Builds the menu for item `item` (None = empty dock area). `anchor`:
    /// where it opens from (screen px; keyboard and screen reader menus
    /// open at their icon), else at the pointer.
    pub(super) fn plan_menu(&mut self, item: Option<usize>, shift: bool, anchor: Option<POINT>) -> Option<MenuPlan> {
        let mut b = Builder::default();
        match item.map(|i| (i, self.items[i].kind)) {
            Some((_, Kind::Start)) => {
                // Windows' own power-user menu, like the taskbar's.
                crate::keys::win_x();
                return None;
            }
            Some((i, Kind::App)) => self.app_rows(&mut b, i, shift),
            Some((_, Kind::Trash)) => trash_menu(&mut b),
            Some((i, Kind::Divider)) => divider_menu(&mut b, self.items[i].path.clone()),
            _ => dock_menu(&mut b, &self.cfg),
        }
        if self.can_undo() { b.sep(); b.add(t_("shortcuts.dock_undo"), DockAction::Undo); }
        if b.rows.is_empty() {
            return None;
        }
        // The label goes in the frame the menu opens (7.4), at once, not
        // over its fade; none shows while it is up (label.rs).
        self.cut_label();
        let pt = anchor.unwrap_or_else(|| {
            let mut pt = POINT::default();
            unsafe {
                let _ = GetCursorPos(&mut pt);
            }
            pt
        });
        // Open off the dock (never over the bar or a magnified icon), the
        // popup gap away from it; an icon's menu over the icon's middle.
        let gap = crate::placement::popup_gap(self.scale);
        let icon = item.filter(|&i| i < self.items.len()).map(|i| self.standing_rect(i));
        let at = menu_point(self.cfg.dock.position, self.bar_screen_rect(), icon, pt, gap);
        let align = match self.cfg.dock.position {
            Position::Bottom => crate::popup::Align::Above,
            Position::Left => crate::popup::Align::RightOf,
            Position::Right => crate::popup::Align::LeftOf,
        };
        Some(MenuPlan { rows: b.rows, actions: b.actions, full_quit: b.full_quit, at, align, scale: self.scale, theme: self.theme.clone(), gfx: self.gfx.clone() })
    }

    /// A stack's menu: open the folder, remove it from the dock.
    fn stack_rows(&self, b: &mut Builder, i: usize) {
        let path = self.items[i].path.clone();
        b.add(t_("dock.stack.open_in_explorer"), DockAction::OpenFolder(path.clone()));
        b.sep();
        b.add(t_("dock.menu.unpin"), DockAction::Unpin(path));
    }

    fn app_rows(&self, b: &mut Builder, i: usize, shift: bool) {
        if self.items[i].stack {
            self.stack_rows(b, i);
            return;
        }
        let path = self.items[i].path.clone();
        if crate::launchpad::is_pin(&path) {
            // Not a program: open it, or take it off the dock.
            b.add(t_("dock.menu.open"), DockAction::NewWindow(path.clone()));
            b.sep();
            b.add(t_("dock.menu.unpin"), DockAction::Unpin(path));
            return;
        }
        app_menu(b, self.app_facts(i), shift);
    }

    /// What app `i`'s menu shows, read now (app_menu).
    fn app_facts(&self, i: usize) -> AppFacts {
        let it = &self.items[i];
        let exe = it.id.exe.clone();
        let fg = unsafe { GetAncestor(GetForegroundWindow(), GA_ROOT) };
        let here = self.monitor;
        let windows: Vec<WinFact> = it
            .windows
            .iter()
            .map(|&h| WinFact { hwnd: h.0 as isize, pid: pid_of(h), title: window_title(h), active: h == fg, minimized: unsafe { IsIconic(h).as_bool() } })
            .collect();
        // Any window whose center is on another monitor?
        let elsewhere = it.windows.iter().any(|&h| unsafe {
            let mut r = RECT::default();
            let (cx, cy) = if GetWindowRect(h, &mut r).is_ok() { ((r.left + r.right) / 2, (r.top + r.bottom) / 2) } else { (here.left, here.top) };
            !(cx >= here.left && cx < here.right && cy >= here.top && cy < here.bottom)
        });
        // An exe-only pin may have learned an explicit AppID from its
        // running window. Never choose between different profile IDs.
        let mut id = it.id.clone();
        if id.aumid.is_none() {
            let mut ids = self.last_wins.iter().filter(|w| it.windows.contains(&w.hwnd)).filter_map(|w| w.aumid.as_ref());
            if let Some(first) = ids.next() {
                if ids.all(|a| a.eq_ignore_ascii_case(first)) { id.aumid = Some(first.clone()); }
            }
        }
        let destination_limit = crate::taskbar::hidden_by_us().then(|| {
            let work = crate::monitors::info(self.mon).map_or(self.monitor, |m| m.work);
            destination_budget(crate::jumplist::max_destinations(), (work.bottom - work.top) as f32 / self.scale)
        });
        let tasks = crate::jumplist::tasks_for(&id, destination_limit.unwrap_or(12));
        // File Explorer: offer to keep the folders its windows show in the
        // dock (paths and names come from Explorer's own views on a helper
        // thread, at most 250 ms: nothing here touches the disk).
        let explorer = exe.as_deref().is_some_and(|e| e.to_lowercase().ends_with("\\explorer.exe"));
        let folders = if explorer && !it.windows.is_empty() {
            stack::explorer_folders(&it.windows, 250).into_iter().take(5).filter(|(f, _)| !self.cfg.dock.pins.iter().any(|p| p.path.eq_ignore_ascii_case(f))).collect()
        } else {
            Vec::new()
        };
        AppFacts {
            path: it.path.clone(),
            name: it.name.clone(),
            pinned: it.pinned,
            packaged: packaged(&it.path, exe.as_deref()),
            locatable: exe.is_some() || it.path.to_lowercase().ends_with(".lnk"),
            startup: !it.path.is_empty() && startup_link(&it.name, &it.path).is_some_and(|l| l.exists()),
            // Not responding (for hung::DEBOUNCE).
            force_quit: if it.hung { self.force_quit(i) } else { ForceQuit::None },
            full_quit: exe.as_deref().and_then(|e| full_quit::target(e, it.id.aumid.as_deref(), it.web.is_some(), packaged(&it.path, Some(e))))
                .map(|t| full_quit::probe(&t, windows.iter().map(|w| w.pid).chain(it.tray.iter().map(|&h| pid_of(h))).collect())).unwrap_or_default(),
            aumid: it.id.aumid.clone(),
            web: it.web.is_some(),
            exe,
            tasks,
            destination_limit,
            windows,
            elsewhere,
            tray: it.tray.len(),
            folders,
        }
    }

    /// The processes owning the app's hung windows that force quit may
    /// end. Protected processes (explorer, system, ours) are left out;
    /// processes we may not end (elevated) get a note and Task Manager
    /// instead.
    fn force_quit(&self, i: usize) -> ForceQuit {
        let mut targets: Vec<(isize, u32, u64)> = Vec::new();
        let mut denied = false;
        for h in self.hung_windows(i) {
            let pid = pid_of(h);
            if pid == 0 || targets.iter().any(|t| t.1 == pid) || hung::is_protected(pid) {
                continue;
            }
            match started(pid) {
                Some(t) if hung::can_terminate(pid) => targets.push((h.0 as isize, pid, t)),
                _ => denied = true,
            }
        }
        if !targets.is_empty() {
            ForceQuit::Kill(targets)
        } else if denied {
            ForceQuit::Denied
        } else {
            ForceQuit::None
        }
    }

    /// The app's window that was in front most recently: the first of its
    /// windows in z-order, preferring ones that are not minimized.
    pub(super) fn recent_window(&self, i: usize) -> Option<HWND> {
        let wins = &self.items.get(i)?.windows;
        if wins.len() <= 1 {
            return wins.first().copied();
        }
        if let Some(h) = self.top_window(i) {
            return Some(h);
        }
        unsafe {
            let mut h = GetTopWindow(None).ok()?;
            for _ in 0..4096 {
                if wins.contains(&h) {
                    return Some(h);
                }
                h = GetWindow(h, GW_HWNDNEXT).ok()?;
            }
        }
        wins.first().copied()
    }

    /// Runs a picked action.
    pub(super) unsafe fn run_action(&mut self, a: DockAction) {
        let post = |h: HWND, msg: u32, w: usize| {
            let _ = PostMessageW(Some(h), msg, WPARAM(w), LPARAM(0));
        };
        let wins_of = |d: &Dock, p: &str| d.find_app(p).map(|i| d.items[i].windows.clone()).unwrap_or_default();
        // Its windows sent to the tray (crate::traywin).
        let tray_of = |d: &Dock, p: &str| d.find_app(p).map(|i| d.items[i].tray.clone()).unwrap_or_default();
        match a {
            DockAction::Undo => self.undo_remove(),
            DockAction::CloseAll(p) => {
                // Asked, never forced: an app may ask to save first, and a
                // window that does not close is left alone. The same list
                // the menu counted: the app's windows, never those in the
                // tray (show one first: crate::traywin).
                for h in wins_of(self, &p) {
                    if !crate::traywin::contains(h) && self.listed(h).is_some_and(|w| self.owned_now(h, w.pid)) {
                        post(h, WM_CLOSE, 0);
                    }
                }
            }
            DockAction::QuitCompletely(exe) => full_quit::request(exe),
            DockAction::FromTray(p) => crate::traywin::restore_many(&tray_of(self, &p)),
            DockAction::GatherHere(p) => {
                let here = crate::monitors::info(self.mon).map(|m| m.work).unwrap_or(self.monitor);
                snap::gather(wins_of(self, &p), here);
            }
            DockAction::NewWindow(p) => {
                if let Some(i) = self.find_app(&p) {
                    self.launch(i);
                }
            }
            DockAction::RunAdmin(p) => {
                let name = self.find_app(&p).map(|i| self.items[i].name.clone()).unwrap_or_default();
                crate::launch::open(&crate::launch::Target::new(&p, &name, crate::launch::Source::Dock).verb("runas"));
            }
            DockAction::OpenPackagedFile(id, path) => {
                crate::launch::open_soon(crate::launch::Target::packaged_file(id, path));
            }
            DockAction::RunTask(path, args, dir) => {
                // A jump list task: its program may be gone since the list was read.
                let target = crate::launch::Target::new(&path, "", crate::launch::Source::JumpList).args(&args, &dir);
                crate::launch::open(&target);
            }
            DockAction::OpenLocation(p) => {
                let target = self.find_app(&p).and_then(|i| self.items[i].id.exe.clone()).unwrap_or(p);
                shell_open("explorer.exe", "open", Some(&format!("/select,\"{target}\"")));
            }
            DockAction::Kill(targets) => {
                // Only those processes, only if each is still the same one,
                // its window still does not respond and it is not one we
                // never end: checked and ended on one handle.
                let mut failed = false;
                for (h, pid, t) in targets {
                    let r = hung::end_verified(pid, t, HWND(h as *mut _));
                    crate::log!("menu: force quit of not-responding pid {pid}: {r:?}");
                    failed |= matches!(r, hung::EndResult::Failed | hung::EndResult::Denied);
                }
                if failed {
                    // Said, with the tool that can do more.
                    let acts = vec![crate::launch::Action::Open(t_("dock.menu.task_manager").into(), "taskmgr.exe".into())];
                    crate::launch::tell(t_("dock.force_quit.failed").into(), t_("dock.force_quit.why").into(), acts, None);
                }
            }
            DockAction::TaskManager => shell_open("taskmgr.exe", "open", None),
            DockAction::CopyPath(p) => {
                let target = self.find_app(&p).and_then(|i| self.items[i].id.exe.clone()).unwrap_or(p);
                if !copy_text(self.hwnd, &target) {
                    // The old clipboard is still there: say so.
                    crate::launch::tell(t_("dock.copy_path.failed").into(), t_("dock.copy_path.why").into(), Vec::new(), None);
                }
            }
            DockAction::Pin(p) => {
                if let Some(i) = self.find_app(&p) {
                    self.pin(i);
                }
            }
            DockAction::Unpin(p) => {
                if let Some(i) = self.find_app(&p) {
                    self.unpin(i);
                }
            }
            DockAction::PinFolder(p, name) => {
                if !self.cfg.dock.pins.iter().any(|x| x.path.eq_ignore_ascii_case(&p)) {
                    let entry = PinEntry::new(name, p);
                    self.update_config(move |c| {
                        if !c.dock.pins.iter().any(|x| x.path.eq_ignore_ascii_case(&entry.path)) {
                            c.dock.pins.push(entry.clone());
                        }
                    });
                }
            }
            DockAction::OpenFolder(p) => shell_open("explorer.exe", "open", Some(&format!("\"{p}\""))),
            DockAction::Startup(p) => {
                let Some(i) = self.find_app(&p) else { return };
                let (name, exe) = (self.items[i].name.clone(), self.items[i].id.exe.clone());
                if packaged(&p, exe.as_deref()) {
                    // Store apps register their own startup task; Windows'
                    // page is where it is switched.
                    shell_open("ms-settings:startupapps", "open", None);
                } else if let Some(link) = startup_link(&name, &p) {
                    // Checked afterwards, not assumed (on the helper
                    // thread): a change that did not take is said, with
                    // where to fix it.
                    crate::launch::prepare();
                    let at = (self.menu_anchor(i), self.menu_align());
                    spawn_toggle_startup(link, p, exe, name, at);
                }
            }
            DockAction::OpenTrash => shell_open("shell:RecycleBinFolder", "open", None),
            DockAction::EmptyTrash => {
                // Windows asks for confirmation itself.
                let _ = windows::Win32::UI::Shell::SHEmptyRecycleBinW(Some(self.hwnd), PCWSTR::null(), 0);
                self.refresh_trash();
            }
            DockAction::AddDivider => self.add_divider(),
            DockAction::RemoveDivider(p) => self.remove_divider(&p),
            DockAction::Position(pos) => self.update_config(move |c| c.dock.position = pos),
            DockAction::AutoHide => {
                let on = !self.cfg.dock.autohide;
                self.update_config(move |c| c.dock.autohide = on);
            }
            DockAction::NewNote => { if self.cfg.widgets.notes { crate::notes::new_note(); } },
            DockAction::Companion(id) => crate::companion::bar_picked(id),
            DockAction::Settings => crate::settings::launch(),
            DockAction::DockSettings => crate::settings::launch_page("dock"),
            DockAction::Sponsor => {
                use crate::launch::{open_soon, Source, Target};
                let url = crate::settings::SPONSORS_URL;
                open_soon(Target::new(url, crate::i18n::t("about.sponsor"), Source::Dock));
            }
            DockAction::Report => crate::settings::launch_page("report"),
            // Desktop notes first: called off (the notes shown) while a
            // text can be kept nowhere.
            DockAction::Quit if crate::notes::prepare_quit(crate::notes::Quit::User) => {
                self.shutdown();
                let _ = DestroyWindow(self.hwnd);
            }
            DockAction::Quit => {}
        }
    }
}

/// The notification-area icon's menu (trayicon.rs): rows, and the action
/// of each row id (id = index + 1). Settings, help, quit: switches live in
/// Settings. There is no "show the dock" row: the dock cannot be turned
/// off.
pub(super) fn tray_menu(notes: bool) -> (Vec<Row>, Vec<DockAction>) {
    tray_items(notes, crate::companion::bar_items())
}

fn tray_items(notes: bool, mut companion: Vec<Row>) -> (Vec<Row>, Vec<DockAction>) {
    let mut b = Builder::default();
    if notes { b.add(t_("notes.new"), DockAction::NewNote); }
    b.add(t_("dock.menu.settings"), DockAction::Settings);
    b.sep();
    b.add(t_("tray.menu.report"), DockAction::Report);
    b.add(t_("about.sponsor"), DockAction::Sponsor);
    b.sep();
    b.add(t_("dock.menu.quit"), DockAction::Quit);
    fn remap(rows: &mut [Row], actions: &mut Vec<DockAction>) {
        for row in rows {
            if row.id != 0 {
                actions.push(DockAction::Companion(row.id));
                row.id = actions.len() as u32;
            }
            remap(&mut row.sub, actions);
        }
    }
    if !companion.is_empty() {
        remap(&mut companion, &mut b.actions);
        b.rows.insert(0, Row::new(0, t_("companion.title")).submenu(companion));
    }
    (b.rows, b.actions)
}

#[cfg(test)]
#[test]
fn notes_entry_menus_follow_the_widget_setting() {
    for enabled in [false, true] {
        let (_, actions) = tray_menu(enabled);
        assert_eq!(actions.contains(&DockAction::NewNote), enabled);
        let mut cfg = Config::default();
        cfg.widgets.notes = enabled;
        let mut b = Builder::default();
        dock_menu(&mut b, &cfg);
        assert_eq!(b.actions.contains(&DockAction::NewNote), enabled);
    }
}

/// Pure: where a dock menu opens from (popup::Align by `pos`): `gap` px off
/// the dock's outer side, past both the bar and the icon (magnified, it
/// stands out of the bar), so the menu never covers the dock. Along the
/// dock it is over the icon's middle, or at the pointer for the empty area.
pub(super) fn menu_point(pos: Position, bar: RECT, icon: Option<RECT>, pointer: POINT, gap: i32) -> POINT {
    let mid = icon.map(|r| POINT { x: (r.left + r.right) / 2, y: (r.top + r.bottom) / 2 }).unwrap_or(pointer);
    match pos {
        Position::Bottom => POINT { x: mid.x, y: icon.map_or(bar.top, |r| r.top.min(bar.top)) - gap },
        Position::Left => POINT { x: icon.map_or(bar.right, |r| r.right.max(bar.right)) + gap, y: mid.y },
        Position::Right => POINT { x: icon.map_or(bar.left, |r| r.left.min(bar.left)) - gap, y: mid.y },
    }
}

/// The Recycle Bin's menu: open it, empty it (Windows asks first).
fn trash_menu(b: &mut Builder) {
    b.add(t_("dock.menu.open"), DockAction::OpenTrash);
    b.sep();
    b.add(t_("dock.menu.empty_trash"), DockAction::EmptyTrash);
}

/// A divider's menu: one row, to take it off.
fn divider_menu(b: &mut Builder, path: String) {
    b.add(t_("dock.menu.remove_divider"), DockAction::RemoveDivider(path));
}

/// The dock's own menu (its empty space): a divider for the pins, where it
/// sits, auto-hide, its Settings page, quit.
fn dock_menu(b: &mut Builder, cfg: &Config) {
    b.add(t_("dock.menu.add_divider"), DockAction::AddDivider);
    b.sep();
    let pos = cfg.dock.position;
    let rows = vec![
        b.row(t_("dock.menu.position.bottom"), DockAction::Position(Position::Bottom)).checked(pos == Position::Bottom),
        b.row(t_("dock.menu.position.left"), DockAction::Position(Position::Left)).checked(pos == Position::Left),
        b.row(t_("dock.menu.position.right"), DockAction::Position(Position::Right)).checked(pos == Position::Right),
    ];
    b.sub(t_("dock.menu.position"), rows);
    b.add(t_("dock.menu.autohide"), DockAction::AutoHide).checked = cfg.dock.autohide;
    b.sep();
    if cfg.widgets.notes { b.add(t_("notes.new"), DockAction::NewNote); }
    b.add(t_("dock.menu.dock_settings"), DockAction::DockSettings);
    b.add(t_("dock.menu.quit"), DockAction::Quit);
}

unsafe fn shell_open(file: &str, verb: &str, args: Option<&str>) {
    crate::launch::open_soon(crate::launch::Target::new(file, file, crate::launch::Source::Dock)
        .verb(verb).args(args.unwrap_or(""), ""));
}

/// toggle_startup on a helper thread (audit A13: reading the .lnk and
/// saving a shortcut may wait on a slow disk or a security scanner, never
/// on the UI thread). One at a time; a second click while one is stuck is
/// dropped (logged).
/// `name`, `at`: for the notice when the change does not take (told on
/// the UI thread, next to the icon).
fn spawn_toggle_startup(link: std::path::PathBuf, path: String, exe: Option<String>, name: String, at: (POINT, crate::popup::Align)) {
    static CAP: crate::worker::HelperCap = crate::worker::HelperCap::new(1);
    let Some(slot) = CAP.try_take() else {
        crate::log!("startup: a change is still in progress; ignored");
        return;
    };
    let spawned = std::thread::Builder::new().name("startup-link".into()).spawn(move || unsafe {
        let _slot = slot;
        let com = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_APARTMENTTHREADED).is_ok();
        let want = !link.exists();
        // True only once the change is read back: the shortcut gone, or
        // one whose bytes / target are what was meant.
        let ok = toggle_startup(&link, &path, exe.as_deref());
        if com {
            windows::Win32::System::Com::CoUninitialize();
        }
        if !ok {
            crate::log!("startup: {} did not change (wanted {want})", link.display());
            let key = if want { "dock.startup.failed_on" } else { "dock.startup.failed_off" };
            let mut acts = Vec::new();
            if let Some(dir) = startup_dir() {
                acts.push(crate::launch::Action::OpenFolder(dir.to_string_lossy().into_owned()));
            }
            acts.push(crate::launch::Action::Open(t_("settings.startup.open").into(), "ms-settings:startupapps".into()));
            crate::launch::tell_later(tf(key, &[("name", &name)]), t_("dock.startup.why").into(), acts, Some(at));
        }
    });
    if spawned.is_err() {
        crate::log!("startup: no thread for the change");
    }
}

/// Our Startup-folder shortcut: remove it if present, else create it
/// (a copy of the pinned .lnk, or a new link to the exe).
/// Returns whether the change is in place, read back from disk: removed,
/// or a shortcut that really starts the app (a broken or half-written one
/// made here is deleted again and counts as a failure).
unsafe fn toggle_startup(link: &std::path::Path, path: &str, exe: Option<&str>) -> bool {
    if link.exists() {
        let r = std::fs::remove_file(link);
        crate::log!("startup: removed {} ({r:?})", link.display());
        return !link.exists();
    }
    let ok = if path.to_lowercase().ends_with(".lnk") {
        // Never overwrite: create_new fails if something appeared there.
        // A shortcut is a few KB; never pull a huge file in.
        match crate::config::read_capped(std::path::Path::new(path), 1 << 20) {
            Ok(bytes) => {
                use std::io::Write;
                match std::fs::OpenOptions::new().write(true).create_new(true).open(link) {
                    Ok(mut f) => {
                        let wrote = f.write_all(&bytes).and_then(|_| f.sync_all());
                        drop(f);
                        // Read back: the same bytes as the pinned shortcut.
                        let ok = wrote.is_ok() && same_bytes(std::fs::read(link).ok().as_deref(), &bytes);
                        if !ok {
                            let _ = std::fs::remove_file(link);
                        }
                        ok
                    }
                    Err(e) => {
                        crate::log!("startup: cannot create {} ({e})", link.display());
                        false
                    }
                }
            }
            Err(e) => {
                crate::log!("startup: cannot read {path} ({e})");
                false
            }
        }
    } else if let Some(exe) = exe.or(Some(path)).filter(|p| p.to_lowercase().ends_with(".exe")) {
        let saved = save_link(link, exe);
        // Read back: the saved shortcut points at the app.
        let ok = saved && link_target(link).is_some_and(|t| t.eq_ignore_ascii_case(exe));
        if !ok && link.exists() {
            let _ = std::fs::remove_file(link);
        }
        ok
    } else {
        false
    };
    crate::log!("startup: add {} -> {ok}", link.display());
    ok
}

/// Pure: a written copy reads back as `want`.
fn same_bytes(read: Option<&[u8]>, want: &[u8]) -> bool {
    read.is_some_and(|r| !want.is_empty() && r == want)
}

/// A new shortcut at `link` to `exe`; true if it was saved.
unsafe fn save_link(link: &std::path::Path, exe: &str) -> bool {
    use windows::Win32::System::Com::{CoCreateInstance, IPersistFile, CLSCTX_INPROC_SERVER};
    use windows::Win32::UI::Shell::{IShellLinkW, ShellLink};
    let Ok(sl) = CoCreateInstance::<_, IShellLinkW>(&ShellLink, None, CLSCTX_INPROC_SERVER) else { return false };
    if sl.SetPath(&HSTRING::from(exe)).is_err() {
        return false;
    }
    let Ok(pf) = sl.cast::<IPersistFile>() else { return false };
    pf.Save(&HSTRING::from(link.to_string_lossy().as_ref()), true).is_ok()
}

/// The program a shortcut file starts (loaded from disk).
unsafe fn link_target(link: &std::path::Path) -> Option<String> {
    use windows::Win32::System::Com::{CoCreateInstance, IPersistFile, CLSCTX_INPROC_SERVER, STGM_READ};
    use windows::Win32::UI::Shell::{IShellLinkW, ShellLink};
    let sl: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).ok()?;
    sl.cast::<IPersistFile>().ok()?.Load(&HSTRING::from(link.to_string_lossy().as_ref()), STGM_READ).ok()?;
    let mut buf = [0u16; 1024];
    sl.GetPath(&mut buf, std::ptr::null_mut(), 0).ok()?;
    let n = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    let s = String::from_utf16_lossy(&buf[..n]);
    (!s.is_empty()).then_some(s)
}

/// Puts text on the clipboard; true once it is there. On any failure the
/// old contents stay and nothing leaks (the clipboard owns the memory only
/// after success).
pub(crate) unsafe fn copy_text(owner: HWND, text: &str) -> bool {
    use windows::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL};
    use windows::Win32::System::DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData};
    use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
    let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    let Ok(mem) = GlobalAlloc(GMEM_MOVEABLE, wide.len() * 2) else { return false };
    let free = |m: HGLOBAL| {
        let _ = GlobalFree(Some(m));
    };
    let p = GlobalLock(mem) as *mut u16;
    if p.is_null() {
        free(mem);
        return false;
    }
    std::ptr::copy_nonoverlapping(wide.as_ptr(), p, wide.len());
    let _ = GlobalUnlock(mem);
    if OpenClipboard(Some(owner)).is_err() {
        free(mem);
        return false;
    }
    // CF_UNICODETEXT.
    let ok = EmptyClipboard().is_ok() && SetClipboardData(13, Some(HANDLE(mem.0))).is_ok();
    if !ok {
        free(mem);
    }
    let _ = CloseClipboard();
    ok
}

/// Pictures for the design audit (tests only).
#[cfg(test)]
#[path = "menu_harmony.rs"]
mod harmony;
#[cfg(test)]
pub(crate) use harmony::font_compare as font_compare_menu;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn menus_open_off_the_dock() {
        let bar = RECT { left: 100, top: 900, right: 700, bottom: 968 };
        // Pointer low on an icon: the menu still opens above the bar.
        let icon = RECT { left: 200, top: 910, right: 252, bottom: 962 };
        assert_eq!(menu_point(Position::Bottom, bar, Some(icon), POINT { x: 210, y: 960 }, 10), POINT { x: 226, y: 890 });
        // Magnified icon standing out of the bar: above the icon.
        let big = RECT { left: 190, top: 850, right: 290, bottom: 962 };
        assert_eq!(menu_point(Position::Bottom, bar, Some(big), POINT { x: 240, y: 955 }, 10), POINT { x: 240, y: 840 });
        // Empty area: at the pointer along the dock, above the bar.
        assert_eq!(menu_point(Position::Bottom, bar, None, POINT { x: 650, y: 950 }, 10), POINT { x: 650, y: 890 });
        // Side docks: beside the bar, never over it.
        let side = RECT { left: 0, top: 100, right: 68, bottom: 700 };
        let ic = RECT { left: 8, top: 200, right: 60, bottom: 252 };
        assert_eq!(menu_point(Position::Left, side, Some(ic), POINT { x: 10, y: 205 }, 10), POINT { x: 78, y: 226 });
        let side_r = RECT { left: 1852, top: 100, right: 1920, bottom: 700 };
        let ic_r = RECT { left: 1860, top: 200, right: 1912, bottom: 252 };
        assert_eq!(menu_point(Position::Right, side_r, Some(ic_r), POINT { x: 1900, y: 240 }, 10), POINT { x: 1842, y: 226 });
    }

    #[test]
    fn dock_menus_sit_the_popup_gap_off_the_dock() {
        use crate::popup::{level_origin, level_room, Align};
        // The rule (placement::POPUP_GAP_DIP): the menu's own rect exactly
        // popup_gap px off the dock's outer edge (the bar's, or a
        // magnified icon's when it stands out further), at every scale and
        // on every side.
        let wa = RECT { left: 0, top: 0, right: 2560, bottom: 1440 };
        for (s, want) in [(1.0f32, 6), (1.25, 8), (1.5, 9), (2.0, 12)] {
            let gap = crate::placement::popup_gap(s);
            assert_eq!(gap, want, "{s}");
            let d = (64.0 * s) as i32;
            let (w, h) = (220, 300);
            for magnified in [false, true] {
                // Resting, the icon sits inside the bar; magnified, it
                // stands `out` past it.
                let out = if magnified { (40.0 * s) as i32 } else { -(8.0 * s) as i32 };
                // Bottom.
                let bar = RECT { left: 800, top: 1440 - d, right: 1800, bottom: 1440 };
                let icon = RECT { left: 1000, top: bar.top - out, right: 1000 + d, bottom: bar.bottom - (8.0 * s) as i32 };
                let edge = icon.top.min(bar.top);
                let at = menu_point(Position::Bottom, bar, Some(icon), POINT::default(), gap);
                let room = level_room(Align::Above, true, at, wa);
                let (_, y) = level_origin(Align::Above, None, false, at, w, h.min(room), wa, s);
                assert_eq!(edge - (y + h), gap, "bottom at {s}, magnified {magnified}");
                // The empty area: off the bar.
                let at = menu_point(Position::Bottom, bar, None, POINT { x: 1500, y: 1430 }, gap);
                let (_, y) = level_origin(Align::Above, None, false, at, w, h, wa, s);
                assert_eq!(bar.top - (y + h), gap);
                // Left.
                let side = RECT { left: 0, top: 300, right: d, bottom: 1100 };
                let ic = RECT { left: (8.0 * s) as i32, top: 500, right: side.right + out, bottom: 500 + d };
                let edge = ic.right.max(side.right);
                let at = menu_point(Position::Left, side, Some(ic), POINT::default(), gap);
                let (x, _) = level_origin(Align::RightOf, None, false, at, w, h, wa, s);
                assert_eq!(x - edge, gap, "left at {s}, magnified {magnified}");
                // Right.
                let side_r = RECT { left: 2560 - d, top: 300, right: 2560, bottom: 1100 };
                let ic_r = RECT { left: side_r.left - out, top: 500, right: 2560 - (8.0 * s) as i32, bottom: 500 + d };
                let edge = ic_r.left.min(side_r.left);
                let at = menu_point(Position::Right, side_r, Some(ic_r), POINT::default(), gap);
                let (x, _) = level_origin(Align::LeftOf, None, false, at, w, h, wa, s);
                assert_eq!(edge - (x + w), gap, "right at {s}, magnified {magnified}");
            }
        }
    }

    #[test]
    fn startup_copies_are_read_back() {
        assert!(same_bytes(Some(b"abc"), b"abc"));
        assert!(!same_bytes(Some(b"ab"), b"abc"), "a half-written copy is a failure");
        assert!(!same_bytes(Some(b""), b""), "an empty shortcut is a failure");
        assert!(!same_bytes(None, b"abc"));
    }

    #[test]
    fn task_titles_lose_access_keys() {
        assert_eq!(clean_task_title("새 창(N)"), "새 창");
        assert_eq!(clean_task_title("New window"), "New window");
        assert_eq!(clean_task_title("(A)"), "");
    }

    #[test]
    fn packaged_apps() {
        assert!(packaged("shell:AppsFolder\\Microsoft.WindowsTerminal_8wekyb3d8bbwe!App", None));
        assert!(packaged("x.lnk", Some("c:\\program files\\windowsapps\\foo\\foo.exe")));
        assert!(!packaged("c:\\x\\chrome.lnk", Some("c:\\program files\\google\\chrome\\application\\chrome.exe")));
    }

    #[test]
    fn startup_link_names_are_safe() {
        let a = startup_link("A/B", "c:\\a.exe").unwrap();
        let b = startup_link("A:B", "c:\\b.exe").unwrap();
        let name = a.file_name().unwrap().to_string_lossy().to_string();
        assert!(name.starts_with("NopBar - A_B (") && name.ends_with(").lnk"), "{name}");
        assert_ne!(a, b, "same display name, different apps");
    }

    /// Each row's label ("-" for a separator), a submenu's after "▸".
    fn labels(rows: &[Row]) -> Vec<String> {
        rows.iter()
            .map(|r| {
                if r.separator {
                    "-".to_string()
                } else if r.sub.is_empty() {
                    r.label.clone()
                } else {
                    format!("{} ▸ {}", r.label, r.sub.iter().map(|s| s.label.as_str()).collect::<Vec<_>>().join(" / "))
                }
            })
            .collect()
    }

    /// Every pickable row's action, submenus included, in order.
    fn picked(b: &Builder) -> Vec<DockAction> {
        fn walk(rows: &[Row], b: &Builder, out: &mut Vec<DockAction>) {
            for r in rows {
                if r.id > 0 {
                    out.push(b.actions[r.id as usize - 1].clone());
                }
                walk(&r.sub, b, out);
            }
        }
        let mut out = Vec::new();
        walk(&b.rows, b, &mut out);
        out
    }

    #[test]
    fn tray_companion_actions_are_remapped_in_nested_submenus() {
        let options = vec![Row::new(0, "Size").submenu(vec![Row::new(10, "Small")])];
        let (rows, actions) = tray_items(false, options);
        let id = rows[0].sub[0].sub[0].id;
        assert_eq!(actions[id as usize - 1], DockAction::Companion(10));
        assert_eq!(actions[rows[1].id as usize - 1], DockAction::Settings);
    }

    #[test]
    fn tray_menu_has_settings_report_sponsor_and_quit() {
        let _l = crate::i18n::test_lock("ko");
        let (rows, actions) = tray_menu(false);
        assert_eq!(actions, vec![DockAction::Settings, DockAction::Report, DockAction::Sponsor, DockAction::Quit]);
        assert_eq!(labels(&rows), ["NopBar 설정…", "-", "문제 보내기…", "후원", "-", "NopBar 종료"]);
        assert!(rows.iter().all(|r| !r.checked), "no switches");
        for r in rows.iter().filter(|r| !r.separator) {
            assert!(actions.get(r.id as usize - 1).is_some(), "each row's id names its action");
        }
    }

    /// "Report a problem" has one name: the tray menu says what Settings'
    /// button says, in every language.
    #[test]
    fn report_has_one_name() {
        for tag in ["ko", "en", "de", "ja"] {
            let _l = crate::i18n::test_lock(tag);
            assert_eq!(t_("tray.menu.report"), t_("about.report.button"), "{tag}");
        }
    }

    /// A failure notice says why in one short sentence, in every language.
    #[test]
    fn failure_reasons_are_one_short_sentence() {
        const KEYS: [&str; 12] = [
            "dock.copy_path.why",
            "traywin.refused.why",
            "traywin.refused.elevated",
            "traywin.refused.hung",
            "traywin.refused.full",
            "ontop.refused.why",
            "ontop.refused.elevated",
            "ontop.refused.hung",
            "dock.split.hung.why",
            "dock.split.fullscreen.why",
            "dock.split.failed.why",
            "dock.split.elevated.why",
        ];
        for (tag, _) in crate::i18n::languages() {
            let _l = crate::i18n::test_lock(tag);
            for k in KEYS {
                let v = crate::i18n::get(k).unwrap_or_else(|| panic!("{tag} lacks {k}"));
                let body = v.trim_end_matches(['.', '。', '!', '।', '؟', '?']);
                assert!(!body.contains(['.', '。', '!', '।', '؟', '?']), "{tag} {k}: one sentence: {v}");
                assert!(v.chars().count() <= 60, "{tag} {k}: short: {v}");
            }
        }
    }

    #[test]
    fn trash_menu_is_open_and_empty() {
        let _l = crate::i18n::test_lock("ko");
        let mut b = Builder::default();
        trash_menu(&mut b);
        assert_eq!(labels(&b.rows), ["열기", "-", "휴지통 비우기…"]);
        assert_eq!(b.actions, vec![DockAction::OpenTrash, DockAction::EmptyTrash]);
    }

    #[test]
    fn dock_menu_is_divider_position_autohide_settings_quit() {
        let l = crate::i18n::test_lock("ko");
        let mut cfg = Config::default();
        cfg.dock.position = Position::Left;
        cfg.dock.autohide = true;
        let mut b = Builder::default();
        dock_menu(&mut b, &cfg);
        assert_eq!(labels(&b.rows), ["구분선 추가", "-", "위치 ▸ 아래 / 왼쪽 / 오른쪽", "자동으로 숨기기", "-", "새 메모", "독 설정…", "NopBar 종료"]);
        assert!(b.rows[3].checked && !b.rows[0].checked);
        assert_eq!(b.rows[2].sub.iter().map(|r| r.checked).collect::<Vec<_>>(), [false, true, false]);
        assert_eq!(picked(&b)[0], DockAction::AddDivider);
        assert_eq!(picked(&b)[4..], [DockAction::AutoHide, DockAction::NewNote, DockAction::DockSettings, DockAction::Quit]);
        drop(l);
        let _l = crate::i18n::test_lock("en");
        let mut b = Builder::default();
        dock_menu(&mut b, &cfg);
        assert_eq!(b.rows[0].label, "Add Divider");
    }

    #[test]
    fn divider_menu_is_remove_only() {
        let l = crate::i18n::test_lock("ko");
        let mut b = Builder::default();
        divider_menu(&mut b, "nopbar:divider:1".into());
        assert_eq!(labels(&b.rows), ["구분선 삭제"]);
        assert_eq!(b.actions, vec![DockAction::RemoveDivider("nopbar:divider:1".into())]);
        drop(l);
        let _l = crate::i18n::test_lock("en");
        let mut b = Builder::default();
        divider_menu(&mut b, "nopbar:divider:1".into());
        assert_eq!(labels(&b.rows), ["Remove Divider"]);
    }

    fn win(h: isize, title: &str, active: bool, minimized: bool) -> WinFact {
        WinFact { hwnd: h, pid: 7, title: title.into(), active, minimized }
    }

    fn notepad(windows: Vec<WinFact>) -> AppFacts {
        AppFacts { path: "c:\\n.lnk".into(), name: "메모장".into(), pinned: true, locatable: true, windows, ..Default::default() }
    }

    fn menu(f: AppFacts, shift: bool) -> Builder {
        let mut b = Builder::default();
        app_menu(&mut b, f, shift);
        b
    }

    #[test]
    fn app_menu_one_window() {
        let _l = crate::i18n::test_lock("ko");
        let b = menu(notepad(vec![win(1, "a.txt", true, false)]), false);
        assert_eq!(labels(&b.rows), ["메모장", "독에서 고정 해제", "창 닫기"]);
        let p = "c:\\n.lnk".to_string();
        assert_eq!(picked(&b), [DockAction::NewWindow(p.clone()), DockAction::Unpin(p.clone()), DockAction::CloseAll(p)]);
        assert!(b.rows.iter().all(|r| r.sub.is_empty() && r.shortcut.is_empty()));
        assert!(b.rows.iter().any(|r| r.label == "메모장" && r.literal));
    }

    #[test]
    fn full_quit_timer_is_released_only_once() {
        let mut state = LiveQuitState {
            watch: FullQuitWatch {
                target: full_quit::target(r"C:\Apps\chat.exe", None, false, false).unwrap(),
                value: full_quit::Availability::None,
                row_id: 1,
            },
            actions: Rc::new(RefCell::new(Vec::new())),
            timer: 42,
        };
        assert_eq!(state.take_timer(), 42);
        // Popup Drop must not kill this ID again: Windows can reuse it.
        assert_eq!(state.take_timer(), 0);
    }

    #[test]
    fn full_quit_protected_apps_never_get_a_row_or_watch() {
        let _l = crate::i18n::test_lock("ko");
        for exe in [r"C:\Windows\explorer.exe", r"C:\Windows\System32\svchost.exe", r"C:\Apps\NOPBAR.EXE", ""] {
            let b = menu(AppFacts { full_quit: full_quit::Availability::Ready, exe: Some(exe.into()), ..notepad(vec![]) }, false);
            assert!(!b.actions.iter().any(|a| matches!(a, DockAction::QuitCompletely(_))));
            assert!(b.full_quit.is_none());
        }
    }

    #[test]
    fn full_quit_shared_items_never_offer_a_command() {
        let _l = crate::i18n::test_lock("en");
        let base = AppFacts { exe: Some(r"C:\Apps\chrome.exe".into()), full_quit: full_quit::Availability::Pending, ..notepad(vec![]) };
        for facts in [
            AppFacts { web: true, ..base.clone() },
            AppFacts { aumid: Some("Chrome._crx_cmkncekebbplodngbpllndjkfo".into()), ..base.clone() },
            AppFacts { packaged: true, ..base.clone() },
            AppFacts { exe: Some(r"C:\Apps\ApplicationFrameHost.exe".into()), ..base.clone() },
            AppFacts { exe: Some(r"C:\Apps\python.exe".into()), ..base.clone() },
            AppFacts { exe: Some(r"C:\Program Files\WindowsApps\Calc\app.exe".into()), ..base },
        ] {
            let b = menu(facts, false);
            assert!(b.full_quit.is_none());
            assert!(!b.actions.iter().any(|a| matches!(a, DockAction::QuitCompletely(_))));
        }
        assert!(packaged("x.lnk", Some(r"C:\Program Files\WindowsApps\Calc\app.exe")));
    }

    #[test]
    fn full_quit_row_follows_close_and_uses_executable() {
        let _l = crate::i18n::test_lock("ko");
        let exe = r"C:\Apps\chat.exe".to_string();
        let b = menu(AppFacts { full_quit: full_quit::Availability::Ready, exe: Some(exe.clone()), ..notepad(vec![win(1, "a", false, false)]) }, false);
        let labels = labels(&b.rows);
        assert_eq!(&labels[labels.len() - 2..], ["창 닫기", "완전 종료"]);
        assert_eq!(picked(&b).last(), Some(&DockAction::QuitCompletely(full_quit::target(&exe, None, false, false).unwrap())));
    }

    #[test]
    fn full_quit_windowless_tray_and_denied_rows() {
        let _l = crate::i18n::test_lock("ko");
        for tray in [0, 1] {
            let facts = AppFacts { full_quit: full_quit::Availability::Ready, exe: Some(r"C:\Apps\chat.exe".into()), tray, ..notepad(vec![]) };
            let b = menu(facts.clone(), false);
            assert_eq!(b.rows.last().unwrap().label, "완전 종료");
            assert!(!b.actions.iter().any(|a| matches!(a, DockAction::CloseAll(_))));
            let b = menu(AppFacts { full_quit: full_quit::Availability::Denied, ..facts.clone() }, false);
            let row = b.rows.last().unwrap();
            assert_eq!(row.label, "완전 종료할 수 없음");
            assert!(!row.enabled);
            assert!(row.id > 0, "disabled command keeps its reserved action ID");
            // No process, or all processes protected: no row/action.
            let b = menu(AppFacts { full_quit: full_quit::Availability::None, ..facts }, false);
            assert!(!labels(&b.rows).iter().any(|s| s.contains("완전 종료")));
        }
    }

    #[test]
    fn full_quit_other_locales_fall_back_to_english() {
        for locale in ["en", "ja", "fr"] {
            let _l = crate::i18n::test_lock(locale);
            let b = menu(AppFacts { full_quit: full_quit::Availability::Ready, exe: Some(r"C:\Apps\chat.exe".into()), ..notepad(vec![]) }, false);
            assert_eq!(b.rows.last().unwrap().label, "Quit Completely");
            assert_eq!(t_("dock.menu.quit_completely_denied"), "Cannot Quit Completely");
        }
    }

    #[test]
    fn full_quit_late_probe_keeps_row_order_and_action_ids() {
        let _l = crate::i18n::test_lock("ko");
        for windows in [vec![], vec![win(1, "a", false, false)]] {
            let initial = menu(AppFacts { exe: Some(r"C:\Apps\chat.exe".into()), full_quit: full_quit::Availability::Pending, ..notepad(windows) }, false);
            let watch = initial.full_quit.as_ref().unwrap();
            assert!(!initial.rows.last().unwrap().enabled);
            for value in [full_quit::Availability::Ready, full_quit::Availability::Denied, full_quit::Availability::None] {
                let row = watch.row(value);
                assert_eq!(row.id, initial.rows.last().unwrap().id);
                assert_eq!(row.enabled, value == full_quit::Availability::Ready);
                assert_eq!(row.separator, initial.rows.last().unwrap().separator);
                assert_eq!(row.sub, initial.rows.last().unwrap().sub);
                assert!(matches!(initial.actions[(row.id - 1) as usize], DockAction::QuitCompletely(_)));
            }
        }
    }

    #[test]
    fn app_menu_many_minimized_windows() {
        let _l = crate::i18n::test_lock("ko");
        for minimized in [true, false] {
            let wins = vec![win(1, "a", false, minimized), win(2, "b", true, minimized), win(3, "c", false, minimized)];
            let b = menu(AppFacts { elsewhere: true, ..notepad(wins) }, false);
            assert_eq!(b.rows.last().unwrap().label, "창 3개 닫기");
            assert!(b.rows.iter().all(|r| r.sub.is_empty()));
            assert_eq!(picked(&b).last(), Some(&DockAction::CloseAll("c:\\n.lnk".into())));
        }
    }

    #[test]
    fn app_menu_without_windows() {
        let _l = crate::i18n::test_lock("ko");
        let b = menu(AppFacts { pinned: false, ..notepad(Vec::new()) }, false);
        assert_eq!(labels(&b.rows), ["메모장", "독에 고정"]);
        let b = menu(AppFacts { path: String::new(), pinned: false, ..notepad(Vec::new()) }, false);
        assert_eq!(labels(&b.rows), ["독에 고정"]);
    }

    #[test]
    fn app_menu_shift_adds_admin_and_copy_path() {
        let _l = crate::i18n::test_lock("ko");
        let b = menu(notepad(vec![win(1, "a", true, false)]), true);
        let l = labels(&b.rows);
        assert!(l.iter().any(|x| x == "관리자 권한으로 실행") && l.iter().any(|x| x == "경로 복사"));
        assert!(b.rows.iter().all(|r| r.sub.is_empty()));
        let acts = picked(&b);
        assert!(acts.contains(&DockAction::RunAdmin("c:\\n.lnk".into())) && acts.contains(&DockAction::CopyPath("c:\\n.lnk".into())));
        let l = labels(&menu(notepad(vec![win(1, "a", true, false)]), false).rows);
        assert!(!l.iter().any(|x| x == "관리자 권한으로 실행" || x == "경로 복사"));
        let b = menu(AppFacts { packaged: true, locatable: false, ..notepad(Vec::new()) }, true);
        assert_eq!(labels(&b.rows), ["Windows 시작 시 실행…", "경로 복사", "-", "메모장", "독에서 고정 해제"]);
    }

    #[test]
    fn app_menu_force_quit_acts_at_once() {
        let _l = crate::i18n::test_lock("ko");
        let b = menu(AppFacts { force_quit: ForceQuit::Kill(vec![(1, 7, 99)]), ..notepad(vec![win(1, "a", false, false)]) }, false);
        assert_eq!(b.rows[0].label, "메모장 작업 끝내기");
        assert!(b.rows[0].sub.is_empty());
        assert_eq!(b.actions[b.rows[0].id as usize - 1], DockAction::Kill(vec![(1, 7, 99)]));
        let b = menu(AppFacts { force_quit: ForceQuit::Denied, ..notepad(vec![win(1, "a", false, false)]) }, false);
        assert!(!b.rows[0].enabled);
        assert!(picked(&b).contains(&DockAction::TaskManager));
    }

    #[test]
    fn app_menu_tray_and_folders() {
        let _l = crate::i18n::test_lock("ko");
        let f = AppFacts { tray: 2, folders: vec![("c:\\d".into(), "d".into())], ..notepad(vec![win(1, "a", true, false)]) };
        let b = menu(f, false);
        assert!(picked(&b).contains(&DockAction::PinFolder("c:\\d".into(), "d".into())));
        assert!(picked(&b).contains(&DockAction::FromTray("c:\\n.lnk".into())));
        assert!(!labels(&b.rows).contains(&"옵션".to_string()));
    }

    /// What the menu no longer offers: window placement (Windows Snap),
    /// always on top and the tray (the preview's menu), splitting, a
    /// close submenu, hints.
    #[test]
    fn app_menu_leaves_out_the_extras() {
        let _l = crate::i18n::test_lock("ko");
        let wins = vec![win(1, "a", true, false), win(2, "b", false, false)];
        let b = menu(AppFacts { tray: 1, elsewhere: true, ..notepad(wins) }, true);
        let all = labels(&b.rows).join("\n");
        for gone in ["창 이동", "항상 위", "트레이로 보내기", "나란히 놓기", "모두 최소화", "모두 복원", "가운데 클릭", "Ctrl+Shift", "지금 종료", "고급"] {
            assert!(!all.contains(gone), "{gone} in\n{all}");
        }
    }

    fn task(title: &str, category: &str) -> crate::jumplist::Task {
        crate::jumplist::Task { packaged_file: None, title: title.into(), path: format!("steam://{title}"), args: String::new(), dir: String::new(), category: category.into() }
    }

    /// The menu from the report: Steam with 5 recent games and 7 tasks, one
    /// window, on a short screen.
    fn steam_menu() -> Builder {
        let mut tasks: Vec<_> = (1..=5).map(|i| task(&format!("game {i}"), "최근 플레이")).collect();
        for t in ["상점", "내 게임", "커뮤니티", "친구", "설정", "Big Picture", "SteamVR"] {
            tasks.push(task(t, ""));
        }
        let f = AppFacts { path: "s".into(), name: "Steam".into(), pinned: true, locatable: true, tasks, windows: vec![win(1, "Steam", true, false)], ..Default::default() };
        menu(f, false)
    }

    #[test]
    fn pinned_rows_share_the_destination_budget_and_keep_tasks() {
        let _lang = crate::i18n::test_lock("en");
        let mut f = notepad(Vec::new());
        f.tasks = (0..150).map(|i| task(&format!("pin {i}"), "Pinned")).collect();
        f.tasks.extend([task("Recent document", "Recent"), task("New document", "")]);
        let mut small = f.clone();
        small.destination_limit = Some(destination_budget(100, 240.0));
        let small = menu(small, false);
        let short_count = small.rows.iter().filter(|r| r.label.starts_with("pin ")).count();
        assert!((1..12).contains(&short_count));
        let (visible, density) = crate::popup::fit(small.rows, 240.0, 0.0);
        assert!(crate::popup::row_layout(&visible, density, 0.0).1 <= 240.0);
        for row in &visible {
            assert!(crate::popup::row_layout(&row.sub, density, 0.0).1 <= 240.0);
        }
        assert_eq!(destination_budget(0, 240.0), 0);
        f.destination_limit = Some(destination_budget(12, 1600.0));
        let built = menu(f, false);
        assert_eq!(built.rows.iter().filter(|r| r.label.starts_with("pin ")).count(), 12);
        assert!(!built.rows.iter().any(|r| r.label == "Recent document"));
        assert!(built.rows.iter().any(|r| r.label == "New document"));
    }

    #[test]
    fn windows_feel_jump_destinations_precede_tasks_and_footer() {
        let _l = crate::i18n::test_lock("en");
        let mut facts = notepad(vec![win(1, "a", false, false)]);
        facts.tasks = vec![task("New document", ""), task("Yesterday", "Recent"), task("Always here", "Pinned")];
        let b = menu(facts, false);
        let labels = labels(&b.rows);
        assert_eq!(labels, ["Pinned", "Always here", "-", "Recent", "Yesterday", "-", "New document", "-", "메모장", "Unpin from Dock", "Close window"]);
        assert!(matches!(picked(&b)[3], DockAction::NewWindow(_)));
    }

    #[test]
    fn jump_lists_are_foldable_groups() {
        let _l = crate::i18n::test_lock("ko");
        let b = steam_menu();
        let folds: Vec<(&str, u8)> = b.rows.iter().take(15).map(|r| (r.label.as_str(), r.fold)).collect();
        assert_eq!(folds[0], ("최근 플레이", FOLD_NAMED), "the header folds with its group");
        assert!(!b.rows[0].enabled && b.rows[0].id == 0);
        assert!(folds[1..6].iter().all(|f| f.1 == FOLD_NAMED));
        assert_eq!(folds[6], ("", 0), "separator");
        assert!(folds[7..14].iter().all(|f| f.1 == FOLD_TASKS));
        assert!(b.rows[14..].iter().all(|r| r.fold == 0), "the app's own rows never fold");
        assert!(b.rows[15..].iter().any(|r| r.label == "Steam"));
    }

    #[test]
    fn short_screens_keep_the_apps_own_rows() {
        use crate::popup::{fit, row_layout, Density};
        let _l = crate::i18n::test_lock("ko");
        let b = steam_menu();
        let own = |rows: &[Row]| {
            let ids: Vec<u32> = rows.iter().map(|r| r.id).collect();
            let labels: Vec<&str> = rows.iter().map(|r| r.label.as_str()).collect();
            labels.contains(&"독에서 고정 해제")
                && ["Steam", "창 닫기"].iter().all(|l| {
                    let id = b.rows.iter().find(|r| r.label == *l).unwrap().id;
                    ids.contains(&id)
                })
        };
        // (No clock row: its height is 0.)
        let normal = row_layout(&b.rows, Density::Normal, 0.0).1;
        let compact = row_layout(&b.rows, Density::Compact, 0.0).1;
        assert_eq!((normal, compact), (468.0, 400.0), "rows 28 (24 compact), a 24 DIP header, 8 DIP separators");
        assert_eq!(fit(b.rows.clone(), normal, 0.0).1, Density::Normal);
        // Compact rows fit everything, nothing folds.
        let (rows, d) = fit(b.rows.clone(), compact, 0.0);
        assert_eq!((rows.len(), d), (b.rows.len(), Density::Compact));
        // Shorter: the recent games fold into "최근 플레이 ›" first.
        let short = compact - 100.0;
        let (rows, d) = fit(b.rows.clone(), short, 0.0);
        assert!(row_layout(&rows, d, 0.0).1 <= short);
        assert_eq!((rows[0].label.as_str(), rows[0].sub.len()), ("최근 플레이", 5));
        assert!(own(&rows));
        // Much shorter: the tasks fold too; the rest scrolls, nothing is cut.
        let (rows, _) = fit(b.rows.clone(), 150.0, 0.0);
        assert_eq!((rows[2].label.as_str(), rows[2].sub.len()), ("더 보기", 7));
        assert!(own(&rows));
        assert_eq!(rows.last().unwrap().label, "창 닫기");
    }

    #[test]
    fn builder_ids_map_to_actions() {
        let mut b = Builder::default();
        b.add("one", DockAction::Quit);
        let r = b.row("two", DockAction::AutoHide);
        b.sub("more", vec![r]);
        b.sep();
        b.sep();
        assert_eq!(b.rows.len(), 3, "double separators collapse");
        assert_eq!(b.actions[(b.rows[1].sub[0].id - 1) as usize], DockAction::AutoHide);
    }
}
