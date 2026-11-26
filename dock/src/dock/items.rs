//! Dock items: creation, worker events, window-list sync, pins.

use super::*;
use windows::UI::Composition::CompositionBorderMode;

impl Dock {
    // ---- items -----------------------------------------------------------

    /// Side of the icon square at the magnification peak (device px): the
    /// key size icons are loaded and kept on the shelf at (sharp.rs).
    pub(super) fn icon_px(&self) -> i32 {
        sharp::big_px(self.look.icon, self.look.max_scale, self.scale)
    }

    /// Side of a resting icon slot (device px).
    pub(super) fn slot_px(&self) -> u32 {
        sharp::slot_px(self.look.icon, self.scale)
    }

    /// The look this dock's icons are asked for (drawn tiles take its
    /// colors; packaged apps' logos its light / dark).
    pub(super) fn icon_look(&self) -> icons::Appearance {
        let hc = self.theme.hc.filter(|_| self.theme.high_contrast).map(|h| ([h.window.r, h.window.g, h.window.b], [h.text.r, h.text.g, h.text.b]));
        crate::icons::set_flat_tiles(!self.theme.glass());
        icons::Appearance { light: self.theme.is_light, hc, flat: !self.theme.glass() }
    }

    /// Asks the icon thread for `key` (icon, peak px) in `look`, with the
    /// sides this dock and others at its peak draw it at (sharp::ask_sides).
    fn ask_icon(&self, key: &(String, i32), look: icons::Appearance) {
        let mut slots = self.shelf.borrow().slots_at(key.1);
        slots.push(self.slot_px());
        let sides = sharp::ask_sides(&slots, key.1.max(1) as u32);
        self.workers.request_icon(&key.0, key.1, sides, look);
    }

    /// What dress_icon needs from the dock (the backplate masks of every
    /// step drawn).
    pub(super) fn dress_ctx(&mut self) -> DressCtx {
        let (big, slot) = (self.icon_px().max(1) as u32, self.slot_px());
        let masks: Vec<(u32, CompositionDrawingSurface)> = if self.look.squircle {
            sharp::step_slots(slot, big).into_iter().filter_map(|px| self.squircle_mask_at(px as i32).map(|m| (px, m))).collect()
        } else {
            Vec::new()
        };
        // Squircles only with the peak mask drawn (a smaller one missing:
        // the next larger one shows for it).
        let squircle = masks.last().is_some_and(|m| m.0 == big);
        DressCtx {
            c: self.gfx.compositor.clone(),
            squircle,
            masks,
            plate_fill: self.theme.backplate_fill(),
            light: self.theme.is_light,
            slot,
            big,
        }
    }

    /// Draws an icon's pictures (sharp.rs) for each (dress, resting slot)
    /// in `needs`: at every step from rest to the peak, cut to the squircle
    /// when masked. A smaller step that fails is marked (the next larger
    /// one shows for it, drawn again later); a dress whose peak picture
    /// fails is left out.
    /// `big_side`: the peak square (DressCtx::big).
    /// `small`: artwork prepared for smaller sides (step_source).
    fn draw_icon_sizes(&self, img: &icons::Icon, small: &[icons::Icon], needs: &[(sharp::Dress, u32)], big_side: u32) -> Vec<views::PicSize> {
        let mut sizes = Vec::new();
        for &(dress, slot) in needs {
            let sides = sharp::step_sides(slot, big_side, dress == sharp::Dress::Plated);
            let masked = dress == sharp::Dress::Masked;
            let mut steps = Vec::with_capacity(sides.len());
            let mut failed = false;
            for &side in &sides {
                let src = step_source(side, img, small);
                match self.gfx.surface_fit(src.width, src.height, &src.pixels, side, masked) {
                    Ok(s) => steps.push((side, s)),
                    Err(_) => failed = true,
                }
            }
            if steps.last().map(|s| s.0) != sides.last().copied() {
                continue;
            }
            sizes.push(views::PicSize { dress, slot, steps, failed });
        }
        sizes
    }

    /// Whether to ask for an icon again after dress_icon said `fit`: drawn
    /// for another dress or size always; a failed resting picture up to
    /// REST_TRIES times.
    fn redraw_for(&self, key: &str, fit: Fit) -> bool {
        match fit {
            Fit::Yes => false,
            Fit::Other => true,
            Fit::NoRest => self.rest_fails.get(key).is_none_or(|&n| n < REST_TRIES),
        }
    }

    pub(super) fn new_item(&mut self, kind: Kind, name: String, path: String, pinned: bool, id: AppId) -> Result<Item> {
        let c = self.gfx.compositor.clone();
        let c = &c;
        let icon = c.CreateSpriteVisual()?;
        let plate = c.CreateSpriteVisual()?;
        let tile = c.CreateSpriteVisual()?;
        let tile_geom = c.CreateRoundedRectangleGeometry()?;
        tile_geom.SetCornerRadius(Vector2 { X: 4.0, Y: 4.0 })?;
        tile.SetClip(&c.CreateGeometricClipWithGeometry(&tile_geom)?)?;
        tile.SetOpacity(0.0)?;
        fade(c, &tile, 90)?;
        // Their squares are on fractional pixels while magnified, and the
        // squircle meets the square's sides: antialiased edges, not edges
        // cut to whole pixels (no change at rest, on whole pixels).
        let _ = icon.SetBorderMode(CompositionBorderMode::Soft);
        let _ = plate.SetBorderMode(CompositionBorderMode::Soft);
        plate.SetOpacity(0.0)?;
        fade(c, &plate, 200)?;
        // Press feedback and icon arrival ease in and out.
        fade(c, &icon, 120)?;
        let mut remembered = None;
        let mut asked = false;
        let mut start = None;
        match kind {
            // Drawn at every step (sharp.rs).
            Kind::Start => {
                let (big, slot) = (self.icon_px().max(1) as u32, self.slot_px());
                let brush = |s: &CompositionDrawingSurface| -> Result<CompositionBrush> {
                    let b = c.CreateSurfaceBrushWithSurface(s)?;
                    b.SetStretch(CompositionStretch::Uniform)?;
                    b.cast()
                };
                let sides = sharp::step_slots(slot, big);
                let peak = brush(&self.gfx.start_icon(big)?)?;
                // A smaller step that fails: the next larger one shows for
                // it (at rest never snapped as if 1:1), drawn again with
                // the items (the next rebuild).
                let mut steps = Vec::with_capacity(sides.len());
                for &px in &sides[..sides.len() - 1] {
                    match self.gfx.start_icon(px).and_then(|r| brush(&r)) {
                        Ok(b) => steps.push((px, b)),
                        Err(e) => crate::log!("dock: Start icon at {px} px not drawn, a larger one shown: {e}"),
                    }
                }
                steps.push((big, peak));
                start = Some(Pics::new(steps, Vec::new()));
            }
            // Icons come from the icon thread; the slot stays empty until then.
            Kind::App | Kind::Trash => {
                let key = (self.icon_key(&path, &id), self.icon_px());
                remembered = self.shelf.borrow_mut().take(&key);
                // Show the remembered surface at once, but still ask the icon
                // thread: it re-checks the disk cache (mtime / age) and the
                // reply replaces the brush if the app's icon changed. (A
                // view takes what is on the shelf: the primary checks.)
                let ask = ask_for_icon(self.primary, kind == Kind::Trash, remembered.as_ref().map(|(p, _)| p.drawn), self.icon_look());
                if ask && !path.is_empty() && self.icon_wait.insert(key.clone()) {
                    self.ask_icon(&key, self.icon_look());
                    asked = true;
                }
            }
            Kind::Sep | Kind::Divider => {}
        }
        if matches!(kind, Kind::Sep | Kind::Divider) {
            icon.SetBrush(&c.CreateColorBrushWithColor(rgba(self.theme.dock_separator()))?)?;
        }
        let (dot, dot_geom) = make_dot(c, rgba(self.theme.indicator_color()), self.look.indicator, self.frame.vertical())?;
        self.layer.Children()?.InsertAtTop(&tile)?;
        self.layer.Children()?.InsertAtTop(&plate)?;
        self.layer.Children()?.InsertAtTop(&icon)?;
        self.layer.Children()?.InsertAtTop(&dot)?;
        let mut item = Item {
            kind,
            name,
            path,
            pinned,
            id,
            web: None,
            windows: Vec::new(),
            tray: Vec::new(),
            icon,
            plate,
            tile,
            tile_geom,
            tile_fill: None,
            plated: false,
            pics: None,
            shown: (usize::MAX, usize::MAX),
            dot,
            icon_ver: 0,
            dot_geom,
            active: false,
            attention: false,
            attention_hwnd: HWND::default(),
            hung: false,
            mag: 1.0,
            mag_t: 1.0,
            mag_v: 0.0,
            marks: badges::Marks::default(),
            stack: false,
            pres: 0.0,
            pres_t: 1.0,
            pres_v: 0.0,
            dot_on: false,
            bounce: None,
            x: 0.0,
            w: 0.0,
            slot: usize::MAX,
            was_dragged: false,
            dx: 0.0,
            slide: 0.0,
            slide_v: 0.0,
            gap: 0.0,
            gap_t: 0.0,
            gap_v: 0.0,
        };
        if let Some(p) = start {
            item.pics = Some(p);
            item.show_rest();
        }
        if let Some((pic, ver)) = remembered {
            let ctx = self.dress_ctx();
            let fit = dress_icon(&ctx, &mut item, &pic);
            item.icon_ver = ver;
            // Drawn for another size or look (another dock's, or before a
            // look change), or its resting picture failed: shown
            // meanwhile, and drawn again.
            let key = (self.icon_key(&item.path, &item.id), self.icon_px());
            if !asked && self.redraw_for(&key.0, fit) && self.icon_wait.insert(key.clone()) {
                self.ask_icon(&key, self.icon_look());
            }
        }
        Ok(item)
    }

    /// The squircle mask at `px` (drawn once per size).
    pub(super) fn squircle_mask_at(&mut self, px: i32) -> Option<CompositionDrawingSurface> {
        if !self.look.squircle || px <= 0 {
            return None;
        }
        if let Some(s) = self.shelf.borrow().mask(px) {
            return Some(s);
        }
        let s = self.gfx.squircle(px as u32).ok()?;
        self.shelf.borrow_mut().put_mask(px, s.clone());
        Some(s)
    }

    pub(super) fn build_items(&mut self) -> Result<()> {
        // The top bar's Windows button opens Start; the dock only has its
        // own Start button when the top bar is off.
        let mut items = Vec::new();
        if !self.cfg.topbar.enabled {
            items.push(self.new_item(Kind::Start, crate::i18n::t("dock.item.start").into(), String::new(), true, AppId::default())?);
        }
        let mut unknown = Vec::new();
        // Shown without redundant dividers (one at either end, two in a
        // row): the next save leaves them out too.
        for p in crate::config::normalize_dividers(&self.cfg.dock.pins) {
            if p.is_divider() {
                items.push(self.new_item(Kind::Divider, String::new(), p.path, true, AppId::default())?);
                continue;
            }
            // A pinned folder is a stack (no app, never has windows). Only a
            // checked folder is one; until the check is back (or if it
            // fails) the pin keeps the usual open-it behavior.
            let stack = crate::stacks::known_kind(&p.path) == Some(true);
            if crate::stacks::known_kind(&p.path).is_none() && crate::stacks::could_be_folder(&p.path) {
                unknown.push(p.path.clone());
            }
            // A web app's pin: how it starts and looks until its window is
            // read, and who it is when its shortcut is gone.
            if let Some(w) = &p.web {
                crate::webapps::seed(w);
            }
            let (id, stale) = if stack { (AppId::default(), false) } else { apps::identify_pin_checked(&p.path, p.web.as_ref()) };
            // Its shortcut is another app's now (apps::pin_identity): the
            // pin opens its own app (its shortcut found, else as it was
            // started: webapps::launch_for) and is saved so (pin_fixes).
            let path = match id.aumid.as_deref() {
                Some(a) if stale => {
                    let fixed = crate::webapps::apps_folder_path(a);
                    crate::log!("dock: pin {:?} is {a}'s, its shortcut another app's now: opens {fixed:?}", p.path);
                    if self.primary {
                        self.pin_fixes.push((p.clone(), PinEntry { path: fixed.clone(), ..p.clone() }));
                    }
                    fixed
                }
                _ => p.path.clone(),
            };
            // The Launchpad pin carries its name in the UI language.
            let name = if crate::launchpad::is_pin(&p.path) { crate::i18n::t("launchpad.title").into() } else { shown_pin_name(&p.name, &p.path) };
            let mut it = self.new_item(Kind::App, name, path, true, id)?;
            it.stack = stack;
            items.push(it);
        }
        if !unknown.is_empty() {
            self.workers.check_pin_kinds(unknown);
        }
        let mut sep = self.new_item(Kind::Sep, String::new(), String::new(), true, AppId::default())?;
        sep.pres_t = 0.0;
        items.push(sep);
        if self.cfg.dock.show_trash {
            items.push(self.new_item(Kind::Trash, crate::i18n::t("dock.item.trash").into(), TRASH.into(), true, AppId::default())?);
        }
        self.items = items;
        Ok(())
    }

    pub(super) fn sep_index(&self) -> usize {
        self.items.iter().position(|i| i.kind == Kind::Sep).unwrap_or(self.items.len())
    }

    /// Everything the worker threads sent since the last WM_EVENTS.
    pub(super) fn drain_events(&mut self) {
        let evs: Vec<Event> = self.events.try_iter().collect();
        // Icon results dropped from a full queue: ask again for those the
        // dock still waits on (else they would stay blank).
        for k in self.events.take_lost_icons() {
            if self.icon_wait.contains(&k) {
                self.ask_icon(&k, self.icon_look());
            }
        }
        for ev in evs {
            match ev {
                Event::Snapshot { seq, wins } => {
                    if seq <= self.last_seq {
                        continue;
                    }
                    self.last_seq = seq;
                    self.apply_snapshot(wins.iter().map(|w| w.to_app()).collect());
                    self.report_active(unsafe { GetForegroundWindow() });
                    if !self.first_snapshot {
                        self.first_snapshot = true;
                        crate::log!("startup: first window list at {} ms", since_start());
                    }
                }
                Event::Icon { key, size, look, drawn, icon, small } => self.apply_icon(key, size, look, drawn, icon, small),
                Event::Stack(l) => self.stack_listed(l),
                Event::StackDropped { folder, ok } => self.stack_dropped(&folder, ok),
                Event::PinKinds(kinds) => {
                    // Pins found to be folders (or not): rebuild if any
                    // item's kind changed.
                    let changed = kinds.iter().any(|(p, folder)| {
                        self.items.iter().any(|it| it.kind == Kind::App && it.pinned && it.path.eq_ignore_ascii_case(p) && it.stack != *folder)
                    });
                    if changed {
                        self.rebuild_items();
                        self.layout();
                    }
                }
                Event::ExplorerGone(pid) => unsafe { on_explorer_gone(self.hwnd, pid) },
                Event::Wallpaper { mon, img } => self.apply_wallpaper(mon, img),
                Event::ConfigChanged(cfg, seq) => {
                    if seq < crate::config::write_seq() {
                        // We saved after this was read; our save already
                        // includes the newest disk state (update_config).
                        crate::log!("config: dropped a stale change event");
                    } else {
                        // Edited outside: shown with the dock's unsaved
                        // edits (and a preview's) on top.
                        self.show_disk_config(*cfg);
                    }
                }
                Event::ConfigSaved { gen, cfg, seq } => self.config_saved(gen, *cfg, seq),
                Event::Ipc { req, reply, at } => {
                    // The client stopped waiting (it was told "dock busy"):
                    // do not act on it late.
                    if at.elapsed() < std::time::Duration::from_millis(900) {
                        let _ = reply.send(self.answer_ipc(&req));
                    } else {
                        crate::log!("ipc: dropped a request that waited too long");
                    }
                }
            }
        }
    }

    pub(super) fn apply_icon(&mut self, key: String, size: i32, look: icons::Appearance, drawn: bool, icon: Option<icons::Icon>, small: Vec<icons::Icon>) {
        let k = (key, size);
        let now = self.icon_look();
        if icon.is_some() && icons::reply_stale(look, drawn, icons::themed(&k.0), now) {
            // Drawn or chosen for another look (it changed meanwhile, the
            // request was already out): asked again for this one (still
            // waited on).
            self.ask_icon(&k, now);
            return;
        }
        self.icon_wait.remove(&k);
        if k.0 == TRASH && std::mem::take(&mut self.trash_dirty) {
            self.refresh_trash();
        }
        let current = self.icon_px();
        if let Some(img) = icon.filter(|_| size == current) {
            let info = icons::analyze(&img);
            let bundled = icons::is_own_app_icon(&k.0);
            let ctx = self.dress_ctx();
            // Drawn for every dress the items showing it need (a system
            // app keeps its shape, another app with the same picture gets
            // a backplate: other sizes), plus what other docks drew it for;
            // the source pixels go when this returns.
            let squircle = ctx.squircle;
            let mut needs: Vec<(sharp::Dress, u32)> = self
                .items
                .iter()
                .filter(|it| matches!(it.kind, Kind::App | Kind::Trash) && self.icon_key(&it.path, &it.id) == k.0)
                .map(|it| (sharp::dress(squircle, bundled || keeps_own_shape(it, drawn), info.coverage), ctx.slot))
                .collect();
            if needs.is_empty() {
                needs.push((sharp::dress(squircle, bundled, info.coverage), ctx.slot));
            }
            needs.extend(self.shelf.borrow().drawn_for(&k));
            needs.sort_unstable();
            needs.dedup();
            let sizes = self.draw_icon_sizes(&img, &small, &needs, ctx.big);
            if !sizes.is_empty() {
                let pic = views::Pic { info, sizes, drawn: drawn.then_some(look), bundled };
                if pic.sizes.iter().any(|p| p.failed) {
                    let n = self.rest_fails.entry(k.0.clone()).or_insert(0);
                    if *n == 0 {
                        crate::log!("dock: an icon's smaller pictures were not all drawn ({} px peak), larger ones shown until they are", k.1);
                    }
                    *n = n.saturating_add(1);
                } else {
                    self.rest_fails.remove(&k.0);
                }
                // Keep only sizes some dock draws: older sizes would pin
                // GPU memory after an icon-size change.
                let ver = {
                    let mut shelf = self.shelf.borrow_mut();
                    shelf.use_size(self.hwnd.0 as isize, current, ctx.slot as i32);
                    shelf.put(k.clone(), pic.clone())
                };
                let keys: Vec<String> = self.items.iter().map(|it| self.icon_key(&it.path, &it.id)).collect();
                for (it, key) in self.items.iter_mut().zip(keys) {
                    if matches!(it.kind, Kind::App | Kind::Trash) && key == k.0 {
                        // (Drawn for every item here: only a failed resting
                        // picture can be missing, retried on the next dress.)
                        dress_icon(&ctx, it, &pic);
                        it.icon_ver = ver;
                    }
                }
                self.prune_icons();
                // Views at this size show the new picture too.
                if self.primary {
                    views::schedule_sync();
                }
                // Plate on/off changes the icon inset: lay out now.
                self.layout();
            }
        }
        if self.icon_wait.is_empty() && !self.icons_logged {
            self.icons_logged = true;
            crate::log!("startup: all icons at {} ms", since_start());
        }
    }

    /// Keeps the icon cache to the icons on the dock plus a few recently
    /// gone apps (audit A04: one-off apps used to pin their surfaces for
    /// the rest of the run).
    pub(super) fn prune_icons(&mut self) {
        let px = self.icon_px();
        let live: HashSet<(String, i32)> =
            self.items.iter().filter(|it| matches!(it.kind, Kind::App | Kind::Trash)).map(|it| (self.icon_key(&it.path, &it.id), px)).collect();
        let slot = self.slot_px() as i32;
        let mut shelf = self.shelf.borrow_mut();
        shelf.use_size(self.hwnd.0 as isize, px, slot);
        shelf.prune(self.hwnd.0 as isize, live);
    }

    /// "Show only apps on this display": a window moved to another display
    /// (or came back): show the apps where their windows are now.
    pub(super) fn refilter(&mut self) {
        if !self.filters_by_display() {
            return;
        }
        let mut now: Vec<isize> = views::windows_for(&self.last_wins, Some(self.mon), |w: &apps::AppWindow| views::window_monitor(w.hwnd)).iter().map(|w| w.hwnd.0 as isize).collect();
        let mut shown: Vec<isize> = self.items.iter().flat_map(|it| it.windows.iter().map(|h| h.0 as isize)).collect();
        now.sort_unstable();
        shown.sort_unstable();
        if now != shown {
            self.sync_windows();
        }
    }

    /// A view: items whose picture on the shelf is newer than the one they
    /// show (the primary loaded a changed icon) are dressed again.
    pub(super) fn redress_icons(&mut self) {
        let px = self.icon_px();
        let ctx = self.dress_ctx();
        let look = self.icon_look();
        let keys: Vec<(String, i32)> = self.items.iter().map(|it| (self.icon_key(&it.path, &it.id), px)).collect();
        let mut changed = false;
        let mut fits = Vec::new();
        for (it, key) in self.items.iter_mut().zip(keys) {
            if !matches!(it.kind, Kind::App | Kind::Trash) || it.path.is_empty() {
                continue;
            }
            let ver = self.shelf.borrow().ver(&key);
            if ver == 0 || ver == it.icon_ver {
                continue;
            }
            let taken = self.shelf.borrow_mut().take(&key);
            if let Some((pic, ver)) = taken {
                let stale = pic.drawn.is_some_and(|l| l != look);
                fits.push((key, dress_icon(&ctx, it, &pic), stale));
                it.icon_ver = ver;
                changed = true;
            }
        }
        // Pictures drawn for another look (or a failed resting picture), or
        // a drawn tile in another look's colors: this dock asks for its own.
        for (key, fit, stale) in fits {
            if (stale || self.redraw_for(&key.0, fit)) && self.icon_wait.insert(key.clone()) {
                self.ask_icon(&key, self.icon_look());
            }
        }
        if changed {
            self.layout();
        }
    }

    /// New window list from the tracker thread.
    pub(super) fn apply_snapshot(&mut self, wins: Vec<apps::AppWindow>) {
        self.last_wins = wins;
        self.sync_windows();
    }

    /// Sync running indicators / unpinned items with the last window list.
    pub(super) fn sync_windows(&mut self) {
        let mut pin_changes = std::mem::take(&mut self.pin_fixes);
        pin_changes.extend(self.sync_windows_inner());
        // Pinned web apps are read while not running too (their shortcut
        // renamed or deleted meanwhile: follow_web_apps).
        crate::webapps::watch_pins(self.hwnd.0 as isize, self.items.iter().filter(|it| it.pinned && it.kind == Kind::App).filter_map(|it| it.id.aumid.clone()).collect());
        // Pins of web apps that took a new name or shortcut (once the
        // list is settled: the save shows the new config).
        self.save_pin_changes(pin_changes);
        // Open preview cards follow the window list (of the dock they
        // were opened from).
        if views::owns_preview(self.hwnd) {
            let items = &self.items;
            preview::sync(&|app: &str| items.iter().find(|it| it.kind == Kind::App && it.path == app).map(|it| it.windows.clone()).unwrap_or_default());
        }
        if self.primary {
            self.wins_gen += 1;
            views::schedule_sync();
        }
        // Progress / badges follow apps that start, end or were rebuilt.
        self.refresh_marks();
        // Not-responding checks run while apps do.
        self.hang_sync();
        self.prune_icons();
    }

    fn sync_windows_inner(&mut self) -> Vec<(PinEntry, PinEntry)> {
        // Windows sent to the tray that closed or are not minimized any
        // more leave the list; topmost windows we track that closed too.
        if self.primary {
            crate::traywin::prune();
            ontop::prune();
        }
        // The whole list stays in last_wins; this dock may show part of it.
        let all = std::mem::take(&mut self.last_wins);
        let only = self.filters_by_display().then_some(self.mon);
        let wins = views::windows_for(&all, only, |w: &apps::AppWindow| views::window_monitor(w.hwnd));
        for it in &mut self.items {
            it.windows.clear();
            it.tray.clear();
        }
        // Windows sent to the tray (crate::traywin, minimized) are not the
        // app's windows (previews, the window menu, cycling) but keep it
        // running: its dot stays, with the tray badge (traywin::split).
        for w in &wins {
            let found = self.items.iter_mut().find(|it| it.kind == Kind::App && it.id.matches(w));
            match found {
                Some(it) => it.windows.push(w.hwnd),
                None => {
                    let id = AppId { exe: w.exe.clone(), aumid: w.aumid.clone() };
                    if let Ok(mut it) =
                        self.new_item(Kind::App, apps::display_name_cached(w), apps::launch_path(w), false, id)
                    {
                        it.web = w.aumid.as_deref().and_then(crate::webapps::read);
                        it.windows.push(w.hwnd);
                        self.items.push(it);
                    }
                }
            }
        }
        for it in &mut self.items {
            let (own, tucked) = crate::traywin::split(&it.windows, &crate::traywin::contains);
            it.windows = own;
            it.tray = tucked;
        }
        let pin_changes = self.follow_web_apps();
        for it in &mut self.items {
            let running = !it.windows.is_empty() || !it.tray.is_empty();
            if it.kind == Kind::App && !it.pinned {
                it.pres_t = if running { 1.0 } else { 0.0 };
            }
            let shown = running && self.look.indicator != Indicator::None;
            // (Only when it changes: show() starts a fade.)
            if it.dot_on != shown {
                it.dot_on = shown;
                show(&self.gfx.compositor, &it.dot, shown);
            }
            show_two(&it.dot, self.look.indicator, it.active, it.windows.len() > 1, self.frame.vertical());
        }
        // An attention request ends with the window that made it.
        let c = self.gfx.compositor.clone();
        let normal = rgba(self.theme.indicator_color());
        for it in self.items.iter_mut().filter(|it| it.attention && !it.windows.contains(&it.attention_hwnd)) {
            it.attention = false;
            if let Ok(b) = c.CreateColorBrushWithColor(normal) {
                paint_indicator(&it.dot, &b);
            }
        }
        // The trash stays last (new running apps were pushed after it).
        if let Some(t) = self.items.iter().position(|it| it.kind == Kind::Trash) {
            let trash = self.items.remove(t);
            self.items.push(trash);
        }
        self.place_sep();
        self.last_wins = all;
        self.kick();
        pin_changes
    }

    /// Running web apps' items, and pinned ones not running (read without
    /// a window: webapps::warm_pins), follow their details read since
    /// (webapps::refresh): a shortcut made, renamed or deleted after the
    /// item was, the app's own name, its icon. Pins whose entry changed:
    /// (entry before, new entry), for save_pin_changes.
    fn follow_web_apps(&mut self) -> Vec<(PinEntry, PinEntry)> {
        let mut saves = Vec::new();
        let mut redraw = Vec::new();
        for i in 0..self.items.len() {
            let it = &self.items[i];
            if it.kind != Kind::App || it.stack || (!it.pinned && it.windows.is_empty() && it.tray.is_empty()) || !it.id.is_web_app() {
                continue;
            }
            let Some(aumid) = it.id.aumid.clone() else { continue };
            let Some(now) = crate::webapps::read(&aumid) else { continue };
            if it.web.as_ref() == Some(&now) {
                continue;
            }
            let prev = if it.pinned { self.cfg.dock.pins.iter().find(|p| p.path.eq_ignore_ascii_case(&it.path)).cloned() } else { None };
            let auto = prev.as_ref().and_then(|p| p.web.as_ref()).and_then(|w| w.name.clone());
            // (The Start menus' index busy: this item next time.)
            let Some(indexed) = crate::webapps::indexed(&it.path) else { continue };
            let r = crate::webapps::refresh(&aumid, &it.name, &it.path, it.pinned, auto.as_deref(), &now, &|_| indexed);
            let it = &mut self.items[i];
            let before = it.path.clone();
            if let Some(n) = r.name {
                it.name = n;
            }
            if let Some(p) = r.path {
                crate::log!("dock: web app {aumid} opens {p:?} now (was {before:?})");
                it.path = p;
            }
            // Its icon's files changed in place (same path and key): drawn
            // again, not taken from the shelf.
            let redo = it.web.as_ref().is_some_and(|w| w.ver != now.ver);
            it.web = Some(now.clone());
            redraw.push((i, redo));
            if let Some(prev) = prev {
                let web = crate::webapps::WebPin::of(&aumid, Some(&now)).map(|mut w| {
                    w.name = now.name.clone().or(auto);
                    w
                });
                let entry = PinEntry { name: it.name.clone(), path: it.path.clone(), web };
                if entry != prev {
                    saves.push((prev, entry));
                }
            }
        }
        // Its icon key may be another now (its shortcut, the window's own),
        // or its picture (`redo`).
        for (i, redo) in redraw {
            self.reicon(i, redo);
        }
        saves
    }

    /// Item `i` shows the icon of its key now: the picture on the shelf,
    /// else asked for (shown when it is back). `redo`: its files changed,
    /// so asked for again whatever the shelf has (the old one shows until
    /// then; the icon cache is by the files' times: icons::cache_path).
    fn reicon(&mut self, i: usize, redo: bool) {
        let key = (self.icon_key(&self.items[i].path, &self.items[i].id), self.icon_px());
        let taken = self.shelf.borrow_mut().take(&key).filter(|_| !redo);
        match taken {
            Some((pic, ver)) => {
                let ctx = self.dress_ctx();
                let it = &mut self.items[i];
                let fit = dress_icon(&ctx, it, &pic);
                it.icon_ver = ver;
                if self.redraw_for(&key.0, fit) && self.icon_wait.insert(key.clone()) {
                    self.ask_icon(&key, self.icon_look());
                }
            }
            None => {
                if self.icon_wait.insert(key.clone()) {
                    self.ask_icon(&key, self.icon_look());
                }
            }
        }
    }

    /// Pins that follow their web app (follow_web_apps): each entry is
    /// updated where it is (followed_pin: a name given meanwhile stays; its
    /// icon chosen in the settings goes along to a new path), shown at once
    /// and saved over the newest config.
    fn save_pin_changes(&mut self, changes: Vec<(PinEntry, PinEntry)>) {
        if changes.is_empty() {
            return;
        }
        let apply = move |c: &mut Config| {
            for (prev, e) in &changes {
                let Some(p) = c.dock.pins.iter_mut().find(|p| p.path.eq_ignore_ascii_case(&prev.path)) else { continue };
                let Some(next) = followed_pin(p, prev, e) else { continue };
                *p = next;
                if !prev.path.eq_ignore_ascii_case(&e.path) {
                    if let Some(icon) = c.dock.icon_overrides.remove(&prev.path) {
                        c.dock.icon_overrides.insert(e.path.clone(), icon);
                    }
                }
            }
        };
        // The items show it already: no rebuild.
        apply(&mut self.cfg);
        self.update_config(apply);
    }

    /// What the config keeps for item `it`'s pin: its name and path, and a
    /// web app's details (webapps::WebPin), with the name the dock gave it
    /// (kept from the pin it was, else the item's own now).
    pub(super) fn pin_entry(&self, it: &Item) -> PinEntry {
        let prev = self.cfg.dock.pins.iter().find(|p| p.path.eq_ignore_ascii_case(&it.path)).and_then(|p| p.web.as_ref());
        let web = it.id.aumid.as_deref().and_then(|a| crate::webapps::WebPin::of(a, crate::webapps::cached(a).as_ref())).map(|mut w| {
            w.name = match prev {
                Some(p) => p.name.clone(),
                None => w.name.or_else(|| Some(it.name.clone())),
            };
            w
        });
        PinEntry { name: it.name.clone(), path: it.path.clone(), web }
    }

    pub(super) fn save_pins(&mut self) {
        let pins: Vec<PinEntry> = self
            .items
            .iter()
            .filter(|i| i.pin_slot())
            .map(|i| self.pin_entry(i))
            .collect();
        // The items already show this order; no rebuild needed.
        let before = std::mem::replace(&mut self.cfg.dock.pins, pins.clone());
        // Only what the dock itself did (pinned / unpinned) goes into the
        // newest list on disk: pins changed elsewhere and not shown here
        // yet (the settings window, another display's dock, a hand edit)
        // stay, as do the settings window's unsaved edits on screen.
        // Dividers left redundant by it go (config::normalize_dividers).
        self.update_config(move |c| c.dock.pins = crate::config::normalize_dividers(&pin_change(&c.dock.pins, &before, &pins)));
    }

    /// A change made from the dock's own UI. It shows at once; the config
    /// saver thread then applies it to the newest config on disk, so an
    /// outside edit that has not reached us yet is not overwritten, and
    /// hands anything else that changed back as a config change (audit
    /// A13: no file I/O on the UI thread).
    pub(super) fn update_config(&mut self, f: impl Fn(&mut Config) + Send + Sync + 'static) {
        if !self.primary {
            // A view: shown here at once, saved (and shown everywhere) by
            // the primary. The edit is made to this display's dock: what
            // the display sets itself (dock.per_display) changes there.
            let f = std::sync::Arc::new(f);
            let mut shown = self.cfg.clone();
            f(&mut shown);
            shown.settle_dividers();
            self.apply_config(shown);
            let key = self.display.clone();
            let edit: crate::worker::ConfigEdit = std::sync::Arc::new(move |c: &mut Config| crate::config::edit_for_display(c, key.as_deref(), |c| f(c)));
            views::forward(edit);
            return;
        }
        let base = match self.preview.as_ref() {
            Some(p) => p.base.clone(),
            None => self.cfg.clone(),
        };
        let mut shown = base.clone();
        f(&mut shown);
        // As the save will have it (config::update settles the dividers).
        shown.settle_dividers();
        // Kept until the saver reports this request (by its number, not by
        // the order writes reached the disk), and re-applied over any config
        // read from disk meanwhile, so a result for an earlier edit never
        // undoes a later one on screen.
        self.edit_gen += 1;
        let edit: crate::worker::ConfigEdit = std::sync::Arc::new(f);
        self.pending_edits.push_back((self.edit_gen, edit.clone()));
        self.workers.save_config(crate::worker::ConfigSave { gen: self.edit_gen, edit, base });
        // A preview is showing: this becomes its new base; the window's
        // edits stay on top.
        if let Some(p) = self.preview.as_mut() {
            p.base = shown;
            let s = p.shown();
            self.apply_config(s);
            return;
        }
        self.apply_config(shown);
    }

    /// Numeric companion state uses the normal ordered/locked saver, but has
    /// no UI side effects. Pending edits and previews retain the same ordering.
    pub(super) fn save_companion_state(&mut self, r: crate::companion::routine::Routine, b: crate::companion::purpose::Budget) {
        let base=self.preview.as_ref().map_or_else(||self.cfg.clone(),|p|p.base.clone());
        let edit: crate::worker::ConfigEdit=std::sync::Arc::new(move|c| {
            c.companion.routine=r;c.companion.activity_budget=b;
        });
        edit(&mut self.cfg);
        if let Some(p)=self.preview.as_mut(){edit(&mut p.base);}
        self.edit_gen+=1;
        self.pending_edits.push_back((self.edit_gen,edit.clone()));
        self.workers.save_config(crate::worker::ConfigSave{gen:self.edit_gen,edit,base});
    }

    /// Shows a config read from disk (an outside edit, a save's result):
    /// the dock's own edits not saved yet go on top again first. Nothing
    /// happens if that is what is shown already.
    pub(super) fn show_disk_config(&mut self, cfg: Config) {
        let c = with_pending(cfg, &self.pending_edits);
        if let Some(p) = self.preview.as_mut() {
            if p.base == c {
                return;
            }
            p.base = c;
            let shown = p.shown();
            self.apply_config(shown);
            return;
        }
        if self.cfg != c {
            self.apply_config(c);
        }
    }

    /// The saver wrote the dock's edits up to request `gen`; `seq` is
    /// write_seq() right after. Older than the newest write: that write (or
    /// its own report) shows the newer state, so only the bookkeeping here.
    pub(super) fn config_saved(&mut self, gen: u64, cfg: Config, seq: u64) {
        drop_saved(&mut self.pending_edits, gen);
        if seq >= crate::config::write_seq() {
            self.show_disk_config(cfg);
        }
    }

    /// update_config that says whether the file was written (the change is
    /// applied in memory either way). Saves on the UI thread, under the same
    /// write lock as the saver thread: only for the few callers that need
    /// the answer (first run, tray choices retried).
    pub(super) fn try_update_config(&mut self, f: impl Fn(&mut Config)) -> std::result::Result<(), String> {
        let (latest, saved, _) = crate::config::update(&self.cfg.clone(), |c| f(c));
        if saved.is_ok() {
            // An open settings window shows pins the dock changed (dragged
            // on, off or around) without being reopened.
            crate::settings::notify_config_saved();
        }
        self.show_disk_config(latest);
        saved
    }

    /// True while the first-run flow has not been finished (Done or Skip
    /// there saves `welcome_done`; a window closed half way shows it again
    /// on the next start). Files from older builds count as done (see
    /// config::welcome_done_in). A config that cannot be saved never shows
    /// it: Done could not be kept and it would come back on every start, so
    /// a save is tried first (the old `welcomed` mark). Decided once per
    /// process, logged.
    pub(super) fn take_first_run(&mut self) -> bool {
        static DECIDED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        if DECIDED.swap(true, std::sync::atomic::Ordering::SeqCst) {
            return false;
        }
        let on_disk = crate::config::read_current();
        if self.cfg.welcome_done || on_disk.as_ref().is_some_and(|c| c.welcome_done) {
            return false;
        }
        if !self.cfg.writable() || on_disk.as_ref().is_some_and(|c| !c.writable()) {
            crate::log!("welcome: not shown, the settings file is read-only");
            return false;
        }
        match self.try_update_config(|c| c.welcomed = true) {
            Ok(()) => true,
            Err(e) => {
                crate::log!("welcome: not shown, could not save that it was shown: {e}");
                false
            }
        }
    }

    pub(super) fn pin(&mut self, i: usize) {
        let mut it = self.items.remove(i);
        it.pinned = true;
        it.pres_t = 1.0;
        let si = self.sep_index();
        self.items.insert(si, it);
        self.drop_redundant_dividers();
        self.save_pins();
        self.sync_windows();
    }

    pub(super) fn unpin(&mut self, i: usize) {
        let mut it = self.items.remove(i);
        it.pinned = false;
        self.items.push(it);
        // A divider it leaves at an end (or next to another) goes too.
        self.drop_redundant_dividers();
        self.save_pins();
        self.sync_windows();
    }

    /// config.json changed on disk (edited by hand or by the settings app).
    pub(super) fn apply_config(&mut self, new: Config) {
        let pins_changed = new.dock.pins != self.cfg.dock.pins
            || new.dock.icon_overrides != self.cfg.dock.icon_overrides
            || new.topbar.enabled != self.cfg.topbar.enabled
            || new.dock.show_trash != self.cfg.dock.show_trash;
        let language_changed = new.language != self.cfg.language;
        if language_changed {
            crate::i18n::init(&new.language);
            crate::desklayer::shell::sync_current();
        }
        // Start and Recycle Bin carry their names: rebuild on a new language.
        let pins_changed = pins_changed || language_changed;
        let taskbar_changed = new.hide_taskbar != self.cfg.hide_taskbar;
        let theme_changed = new.theme != self.cfg.theme || new.appearance != self.cfg.appearance;
        let pos_changed = new.dock.position != self.cfg.dock.position;
        let follow_changed = new.dock.display_mode() != self.cfg.dock.display_mode();
        // Whether this dock shows only its display's apps (mode and switch).
        let filtered_before = self.filters_by_display();
        let autohide_changed = new.dock.autohide != self.cfg.dock.autohide
            || new.dock.reserve_work_area != self.cfg.dock.reserve_work_area;
        // (A view's effects were published by the primary already.)
        let fx_before = if self.primary { crate::effects::current() } else { self.fx_seen };
        if self.primary {
            super::fx::publish(&new, &self.sys);
        }
        self.fx_seen = crate::effects::current();
        let look = Look::from_cfg(&new.dock, crate::effects::items().magnify);
        let look_changed = look != self.look;
        self.cfg = new;
        self.look = look;
        if self.primary {
            // Every shortcut (crate::shortcuts; unchanged ones cost nothing),
            // unless Settings is recording one (they come back after).
            if !super::keys_suspended() {
                crate::shortcuts::apply_all(self.hwnd, &self.cfg);
            }
            // Wakes the update scheduler only when the switch flipped.
            crate::updates::set_auto(self.cfg.updates.auto_check);
            crate::desklayer::apply_config(&self.cfg, self.theme.non_shell());
            crate::focus_timer::apply_config(&self.cfg);
            crate::agenda::apply_config(&self.cfg);
            crate::agents::apply_config(&self.cfg);
            crate::companion::apply_config(&self.cfg);
            crate::launchers::set_choice(&self.cfg.search.with);
            crate::ime::apply_config(&self.cfg.input);
        }
        self.effects_reapply(fx_before);
        crate::log!(
            "config: reloaded (pins changed: {pins_changed}, taskbar: {taskbar_changed}, theme: {theme_changed})"
        );
        if theme_changed {
            if self.primary {
                super::fx::close_drawn_with_old_look();
            }
            self.theme_raw = self.cfg.load_theme(&crate::config::app_dir());
            self.theme = self.theme_raw.resolve(&self.sys).windows_shell();
            self.apply_theme();
            self.apply_material();
        }
        if theme_changed && !(pins_changed || look_changed) {
            self.rebuild_items();
        }
        if look_changed {
            // New icon size: re-place the window (height depends on it) and
            // rebuild items so icons are rasterized at the new size.
            unsafe { self.place_window() };
        }
        if pins_changed || look_changed {
            self.rebuild_items();
        }
        if taskbar_changed && !self.primary {
            // A view: its own AppBar follows the hide-taskbar setting.
            unsafe { self.apply_taskbar_mode() };
        }
        if taskbar_changed && self.primary {
            // Switched (back) on: a taskbar that refused auto-hide earlier
            // in this run gets another try.
            if self.cfg.hide_taskbar {
                crate::taskbar::forget_refusal();
            }
            unsafe { self.apply_taskbar_mode() };
        }
        if self.primary {
            if crate::topbar::apply_config(&self.gfx, &self.cfg, &self.theme) {
                unsafe { self.place_window() };
                // The top bar's reservation moved: the views follow.
                views::post_reconcile();
            }
            if language_changed {
                crate::topbar::language_changed();
            }
            // A re-created bar starts visible and nameless: bring it up to date.
            crate::topbar::refresh_fullscreen();
            crate::corners::apply(&self.cfg.hot_corners);
            unsafe { super::trayicon::set_enabled(self.cfg.tray_icon) };
        }
        if follow_changed && self.primary {
            // Following off: back to the primary monitor; on: sensors
            // appear. Every display: the views come (or go).
            if !self.cfg.dock.follows_mouse() {
                self.mon = crate::monitors::primary().map(|m| m.handle).unwrap_or(self.mon);
            }
            unsafe { self.replace_on_monitor() };
            views::post_reconcile();
        }
        if self.filters_by_display() != filtered_before {
            // Running apps from every display, or only this one's.
            self.sync_windows();
        }
        self.report_active(unsafe { GetForegroundWindow() });
        if pos_changed || autohide_changed {
            // New edge: new window rect, AppBar edge and indicator shape.
            self.set_hidden(false);
            unsafe { self.place_window() };
            self.apply_theme();
            if pos_changed {
                self.refresh_mica();
            }
        }
        self.layout();
        // Auto-hide stays in force across edge changes.
        if (pos_changed || autohide_changed) && self.cfg.dock.autohide && self.mouse.is_none() {
            self.set_hidden(true);
        }
        if self.primary {
            views::schedule_sync();
        }
    }

    // ---- several displays (views.rs) ---------------------------------------

    /// "Show only apps on this display" is in force for this dock.
    pub(super) fn filters_by_display(&self) -> bool {
        self.cfg.dock.only_display_apps()
    }

    /// The primary's model, for a view to pull.
    pub(super) fn model_snap(&self) -> ModelSnap {
        ModelSnap {
            cfg: self.cfg.clone(),
            wins: self.last_wins.clone(),
            wins_gen: self.wins_gen,
            active: self.active_top,
            previewing: self.preview.is_some(),
        }
    }

    /// What a new view starts from.
    pub(super) fn seed(&self) -> Seed {
        Seed {
            gfx: self.gfx.clone(),
            theme_raw: self.theme_raw.clone(),
            theme: self.theme.clone(),
            sys: self.sys,
            shelf: self.shelf.clone(),
            snap: self.model_snap(),
        }
    }

    /// A view takes the primary's model: config, effects, window list, the
    /// app in front and newer icons.
    pub(super) fn pull(&mut self, s: ModelSnap) {
        self.view_previewing = s.previewing;
        // As this display's dock sees it (its own settings over the main).
        let cfg = s.cfg.for_display(self.display.as_deref());
        if cfg != self.cfg {
            self.apply_config(cfg);
        }
        // The effects in force may have changed on the primary.
        let now = crate::effects::current();
        if now != self.fx_seen {
            let before = std::mem::replace(&mut self.fx_seen, now);
            self.effects_changed(before);
        }
        if s.wins_gen != self.wins_gen {
            self.wins_gen = s.wins_gen;
            self.apply_snapshot(s.wins);
        }
        if s.active != self.active_top {
            self.set_active(s.active);
        }
        self.redress_icons();
    }

    /// The view is on display `key` now (its monitor changed hands): its
    /// own settings follow on the next pull.
    pub(super) fn set_display(&mut self, key: Option<String>) {
        self.display = key;
    }

    /// A new view is up: its AppBar (when the taskbar is hidden), its
    /// place and its running apps' marks.
    pub(super) unsafe fn view_started(&mut self) {
        self.apply_taskbar_mode();
        self.take_marks();
        self.layout();
    }

    /// A view closes (its display went, or one dock again).
    pub(super) unsafe fn view_shutdown(&mut self) {
        // A drag in progress ends here (capture, keys, Split View hint).
        if self.dragging() {
            self.end_drag(true, None);
            self.pressed = None;
        }
        split::teardown(self.hwnd.0 as isize);
        if views::owns_preview(self.hwnd) {
            preview::hide();
        }
        if views::owns_stack(self.hwnd) {
            stack::cancel();
        }
        self.leave_keyboard(false);
        self.sound_hover(false);
        if self.appbar {
            taskbar::appbar_remove(self.hwnd);
            self.appbar = false;
        }
        self.shelf.borrow_mut().forget(self.hwnd.0 as isize);
        crate::worker::forget_events(self.hwnd.0 as isize);
    }

    /// Tell the top bar which app is in front. Our own windows and the
    /// desktop keep the last name.
    pub(super) fn report_active(&mut self, fg: HWND) {
        use crate::topbar::menus::ActiveApp;
        if fg.is_invalid() {
            return;
        }
        let mut pid = 0u32;
        unsafe { GetWindowThreadProcessId(fg, Some(&mut pid)) };
        if pid == std::process::id() {
            // Our own dock, bar or menus.
            return;
        }
        let top = unsafe { GetAncestor(fg, GA_ROOT) };
        if !self.primary {
            // The top bar hears it from the primary.
            self.set_active(top);
            return;
        }
        // The input memory and indicator follow the app in front.
        crate::ime::foreground(top);
        let owner = unsafe { GetAncestor(fg, GA_ROOTOWNER) };
        let hits = |h: HWND| self.items.iter().find(|it| it.kind == Kind::App && it.windows.contains(&h));
        let app = match hits(top).or_else(|| hits(owner)) {
            Some(it) => ActiveApp {
                name: it.name.clone(),
                hwnd: top.0 as isize,
                exe: it.id.exe.clone().or_else(|| crate::apps::window_info(top).and_then(|w| w.exe)),
                path: it.path.clone(),
                windows: it.windows.iter().map(|h| h.0 as isize).collect(),
                desktop: false,
                locked: crate::apps::is_locked(pid),
            },
            None => {
                let mut buf = [0u16; 32];
                let n = unsafe { GetClassNameW(fg, &mut buf) }.max(0) as usize;
                let class = String::from_utf16_lossy(&buf[..n]);
                if matches!(class.as_str(), "Progman" | "WorkerW") {
                    ActiveApp { name: crate::i18n::t("dock.desktop").into(), desktop: true, ..Default::default() }
                } else {
                    let Some(w) = crate::apps::window_info(top) else { return };
                    if shell_surface(&class, w.exe.as_deref()) {
                        // Start, search, notifications, quick settings, Task
                        // View: Windows' own, over the app. The bar keeps the
                        // app's name (it showed "Searchhost").
                        return;
                    }
                    ActiveApp {
                        name: crate::apps::display_name_cached(&w),
                        hwnd: top.0 as isize,
                        path: crate::apps::launch_path(&w),
                        exe: w.exe,
                        windows: vec![top.0 as isize],
                        desktop: false,
                        locked: crate::apps::is_locked(pid),
                    }
                }
            }
        };
        self.set_active(top);
        crate::topbar::set_app(app);
    }

    /// Re-color everything that comes from the theme.
    pub(super) fn apply_theme(&mut self) {
        crate::companion::environment_changed();
        let c = self.gfx.compositor.clone();
        let r = self.look.radius;
        let _ = self.bar_geom.SetCornerRadius(Vector2 { X: r, Y: r });
        let _ = self.border_geom.SetCornerRadius(Vector2 { X: r - 0.5, Y: r - 0.5 });
        // The glass's tint for the backdrop the bar has (the Luminosity
        // layer folded in where its brush does not draw it).
        let backdrop = self.bar_fill.first().and_then(|b| b.Brush().ok());
        self.set_tint(crate::glass::tint_for(&self.theme, crate::theme::Material::Dock, backdrop.as_ref()));
        if let Ok(brush) = c.CreateColorBrushWithColor(rgba(self.theme.dock_border())) {
            let _ = self.border_shape.SetStrokeBrush(&brush);
        }
        // The rim (theme::rim): light along the inner edge, one device
        // pixel wide (its geometry: layout.rs).
        paint_highlight(&c, &self.highlight_shape, &self.theme, crate::theme::Material::Dock);
        let vertical = self.frame.vertical();
        let style = self.look.indicator;
        let normal = rgba(self.theme.indicator_color());
        let accent = normal;
        for it in self.items.iter_mut() {
            let fill = if it.attention { rgba(self.theme.attention()) } else if it.active { accent } else { normal };
            it.dot_geom = style_indicator(&c, &it.dot, style, fill, vertical, it.active, it.windows.len() > 1).ok().flatten();
            if let Some(g) = &it.dot_geom {
                let _ = size_anim(&c, g);
            }
        }
        if let Ok(b) = c.CreateColorBrushWithColor(rgba(self.theme.dock_separator())) {
            for it in self.items.iter().filter(|it| it.is_line()) {
                let _ = it.icon.SetBrush(&b);
            }
        }
        self.labels.clear();
        self.style_label();
        // Accent may have changed: redraw progress / badges.
        self.reset_marks();
    }

    /// Where an item's icon comes from: the picture chosen for it in the
    /// settings (if the file is still there), else the app itself.
    /// Where item (`path`, `id`)'s icon is read from: the icon chosen for
    /// it, else a browser's web app's own (webapps::icon_key: its shortcut
    /// found since, the window's icon), else its path. A web app's
    /// AppsFolder path with nothing known: its browser's icon.
    pub(super) fn icon_key(&self, path: &str, id: &AppId) -> String {
        let web_path = crate::webapps::apps_folder_aumid(path).is_some_and(crate::webapps::is_web_app);
        self.cfg
            .dock
            .icon_overrides
            .get(path)
            .and_then(|rel| crate::config::icon_file(rel))
            .or_else(|| id.aumid.as_deref().filter(|a| a.eq_ignore_ascii_case(crate::updates::AUMID)).map(|_| icons::NOPBAR_ICON_KEY.into()))
            .or_else(|| id.aumid.as_deref().and_then(crate::webapps::cached).and_then(|w| crate::webapps::icon_key(path, &w)))
            .or_else(|| id.exe.clone().filter(|_| web_path))
            .unwrap_or_else(|| path.to_string())
    }

    /// The trash may have filled up or been emptied: ask for its icon again.
    pub(super) fn refresh_trash(&mut self) {
        if self.items.iter().any(|it| it.kind == Kind::Trash) {
            let key = (TRASH.to_string(), self.icon_px());
            if self.icon_wait.insert(key.clone()) {
                self.ask_icon(&key, self.icon_look());
            } else {
                // One request at a time; ask again when this one is back.
                self.trash_dirty = true;
            }
        }
    }

    /// A window of some app flashes: mark it until the app comes to the
    /// front (static mark, one short bounce; polish plan §3).
    pub(super) fn attention(&mut self, h: HWND) {
        let top = unsafe { GetAncestor(h, GA_ROOT) };
        let fg = unsafe { GetAncestor(GetForegroundWindow(), GA_ROOT) };
        if top.is_invalid() || top == fg {
            return;
        }
        let c = self.gfx.compositor.clone();
        // Full ink, no hue (the bounce tells it apart).
        let orange = rgba(self.theme.attention());
        if let Some(it) = self.items.iter_mut().find(|it| it.kind == Kind::App && it.windows.contains(&top)) {
            if it.attention {
                return;
            }
            it.attention = true;
            it.attention_hwnd = top;
            if let Ok(b) = c.CreateColorBrushWithColor(orange) {
                paint_indicator(&it.dot, &b);
            }
            if motion() && crate::effects::items().bounce {
                it.bounce = Some(Instant::now());
            }
            self.kick();
        }
    }

    /// Screen rect (physical px) of the dock icon of the app owning window
    /// `h`, for the minimize animation. None if it has no icon or the dock
    /// is not showing.
    /// Publishes where each app icon is drawn, for the minimize animation
    /// (`answer_minrect`). Nothing while hidden or in fullscreen.
    pub(super) fn publish_minrects(&self, drawn: Vec<(Vec<HWND>, RECT)>) {
        let off = self.fullscreen || self.hidden;
        let me = self.hwnd.0 as isize;
        let entry = views::MinEntry { dock: me, mon: self.mon, primary: self.primary, rects: if off { Vec::new() } else { drawn } };
        MINRECTS.with(|m| {
            let mut m = m.borrow_mut();
            match m.iter_mut().find(|e| e.dock == me) {
                Some(e) => *e = entry,
                None => m.push(entry),
            }
        });
    }

    /// Mark which item's app is in front: long accent indicator.
    pub(super) fn set_active(&mut self, top: HWND) {
        if self.primary && top != self.active_top {
            views::schedule_sync();
        }
        self.active_top = top;
        // Owned dialogs count for their owner's app.
        let owner = unsafe { GetAncestor(top, GA_ROOTOWNER) };
        let c = self.gfx.compositor.clone();
        let vertical = self.frame.vertical();
        let style = self.look.indicator;
        let (normal, accent) = (rgba(self.theme.indicator_color()), rgba(self.theme.indicator_color()));
        let mut changed = false;
        for it in self.items.iter_mut() {
            let active = it.kind == Kind::App && !top.is_invalid() && (it.windows.contains(&top) || it.windows.contains(&owner));
            // Coming to the front answers the attention request.
            let answered = active && it.attention;
            if answered {
                it.attention = false;
            }
            if active == it.active && !answered {
                continue;
            }
            it.active = active;
            changed = true;
            let size = upright(indicator_size(style, active, it.windows.len() > 1), vertical);
            let _ = it.dot.SetSize(size);
            if let Some(g) = &it.dot_geom {
                let _ = g.SetSize(size);
            }
            let fill = if it.attention { rgba(self.theme.attention()) } else if active { accent } else { normal };
            if let Ok(b) = c.CreateColorBrushWithColor(fill) {
                paint_indicator(&it.dot, &b);
            }
        }
        if changed {
            self.layout();
        }
    }

    /// Windows changed light/dark, accent, transparency, high contrast or
    /// animations: re-resolve the theme and repaint everything.
    pub(super) fn refresh_syslook(&mut self) {
        let sys = crate::syslook::current();
        if sys == self.sys {
            return;
        }
        if self.primary {
            crate::log!("system look changed: light={} transparency={} high_contrast={} animations={}", sys.light, sys.transparency, sys.high_contrast.is_some(), sys.animations);
        }
        self.sys = sys;
        if self.primary {
            self.sys_for_effects(&sys);
            // Menus, stacks and previews were drawn with the old colors.
            super::fx::close_drawn_with_old_look();
        }
        self.fx_seen = crate::effects::current();
        self.theme = self.theme_raw.resolve(&sys).windows_shell();
        let look = Look::from_cfg(&self.cfg.dock, crate::effects::items().magnify);
        let look_changed = look != self.look;
        self.look = look;
        if look_changed {
            unsafe { self.place_window() };
        }
        self.apply_theme();
        self.apply_material();
        // Plates are tinted from the icon and the light/dark backdrop.
        self.rebuild_items();
        // Long-lived visuals get their animations re-attached (or emptied).
        let c = self.gfx.compositor.clone();
        let _ = slide_anim(&c, &self.slide, SLIDE_MS);
        let _ = slide_anim(&c, self.shadow.slide(), SLIDE_MS);
        self.style_label();
        let _ = theme_fade(&c, &self.mica);
        if let Some(b) = self.bar_fill.first() {
            let _ = theme_fade(&c, b);
        }
        if self.primary {
            crate::topbar::apply_config(&self.gfx, &self.cfg, &self.theme);
        }
        // The rebuilt items start inactive: mark the app in front again.
        self.report_active(unsafe { GetForegroundWindow() });
        self.layout();
    }

    /// The dock's material: the theme's, or "solid" with the "Blur and
    /// transparency" effect off.
    pub(super) fn material(&self) -> &str {
        crate::effects::material(crate::effects::items().blur, self.theme.dock.material.as_str())
    }

    /// Blur off (the Low preset, or the effect switched off): the theme's
    /// matte slab instead of a flat opaque tint (fable review 6, look B).
    /// Not when the theme itself is solid (transparency off, high contrast).
    pub(super) fn matte(&self) -> Option<crate::theme::Matte> {
        if crate::effects::items().blur { None } else { self.theme.dock_matte().cloned() }
    }

    /// The bar's shadow layers now: the theme's (the matte slab's with blur
    /// off), none with the "Shadows" effect off or in high contrast.
    fn shadow_layers(&self) -> Vec<shadow::Layer> {
        if !crate::effects::items().shadows || self.theme.high_contrast {
            return Vec::new();
        }
        let matte = self.matte().is_some();
        self.theme.dock_shadows(matte).into_iter().map(|(ox, oy, blur, color)| shadow::Layer { ox, oy, blur, color }).collect()
    }

    /// The tint over the bar's backdrop (glass::tint_for: the one for the
    /// brush the backdrop has; Flat v2's fall from top to bottom).
    fn set_tint(&self, t: crate::theme::Rgba) {
        if let (Some(tint), Ok(brush)) = (self.bar_fill.get(1), crate::glass::tint_brush(&self.gfx.compositor, &self.theme, crate::theme::Material::Dock, t)) {
            let _ = tint.SetBrush(&brush);
        }
    }

    /// Backdrop for the theme's material: acrylic (DWM host backdrop),
    /// mica (blurred wallpaper image, loaded on the icon thread) or solid.
    pub(super) fn apply_material(&mut self) {
        let c = self.gfx.compositor.clone();
        // The matte slab is the whole fill: no tint over it.
        let slab = self.material() == "solid" && self.matte().is_some();
        if let Some(tint) = self.bar_fill.get(1) {
            let _ = tint.SetOpacity(if slab { 0.0 } else { 1.0 });
        }
        self.shadow_spec = shadow::Spec { layers: self.shadow_layers(), radius: self.look.radius, thick: self.shadow_spec.thick, contact: Some(self.theme.contact()) };
        let Some(backdrop) = self.bar_fill.first().cloned() else { return };
        match self.material() {
            "mica" => {
                let _ = backdrop.SetOpacity(0.0);
                self.refresh_mica();
            }
            "solid" => {
                let t = self.theme.dock_tint();
                match self.matte().and_then(|m| matte_brush(&c, &m).ok()) {
                    Some(b) => {
                        let _ = backdrop.SetBrush(&b);
                    }
                    None => {
                        if let Ok(b) = c.CreateColorBrushWithColor(color(255, t.r, t.g, t.b)) {
                            let _ = backdrop.SetBrush(&b);
                        }
                    }
                }
                let _ = backdrop.SetOpacity(1.0);
                let _ = self.mica.SetOpacity(0.0);
            }
            _ => {
                // DWM's host backdrop through the glass's graph: the brush
                // it has is kept and retuned (a new brush on every look
                // change makes DWM set the backdrop up again).
                let t = crate::glass::refresh_backdrop(&self.gfx, &backdrop, &self.theme, crate::theme::Material::Dock);
                self.set_tint(t);
                let _ = backdrop.SetOpacity(1.0);
                let _ = self.mica.SetOpacity(0.0);
            }
        }
        // The glass's top light and grain over the tint (Flat v2: its
        // grain alone, under the slab's absence).
        if let Some(tint) = self.bar_fill.get(1).and_then(|t| t.cast::<windows::UI::Composition::Visual>().ok()) {
            let shown = self.material() == "acrylic" || (!slab && self.theme.flat_grain(crate::theme::Material::Dock) > 0.0);
            crate::glass::place_top_light(&self.gfx, &self.theme, crate::theme::Material::Dock, &self.bar, &tint, shown);
        }
    }

    /// (Re)build the blurred wallpaper if the material is mica.
    pub(super) fn refresh_mica(&mut self) {
        if self.material() == "mica" {
            let m = self.monitor;
            self.workers.request_wallpaper((m.left, m.top, m.right, m.bottom));
        }
    }

    pub(super) fn apply_wallpaper(&mut self, mon: (i32, i32, i32, i32), img: Option<icons::Icon>) {
        let m = self.monitor;
        if self.material() != "mica" || mon != (m.left, m.top, m.right, m.bottom) {
            return; // material changed meanwhile, or made for an old screen layout
        }
        let c = self.gfx.compositor.clone();
        let surface = img.and_then(|i| self.gfx.surface_from_bgra(i.width, i.height, &i.pixels).ok());
        match surface.and_then(|s| c.CreateSurfaceBrushWithSurface(&s).ok()) {
            Some(brush) => {
                let _ = brush.SetStretch(CompositionStretch::Fill);
                let _ = self.mica.SetBrush(&brush);
                let _ = self.mica.SetOpacity(1.0);
                // A previous failure may have left the solid fallback on top.
                if let Some(backdrop) = self.bar_fill.first() {
                    let _ = backdrop.SetOpacity(0.0);
                }
            }
            None => {
                // No wallpaper image (solid color desktop, unreadable file):
                // fall back to the solid material.
                crate::log!("mica: no wallpaper image; using solid");
                let t = self.theme.dock_tint();
                if let (Some(backdrop), Ok(b)) = (self.bar_fill.first(), c.CreateColorBrushWithColor(color(255, t.r, t.g, t.b))) {
                    let _ = backdrop.SetBrush(&b);
                    let _ = backdrop.SetOpacity(1.0);
                }
                let _ = self.mica.SetOpacity(0.0);
            }
        }
    }

    /// Pins changed outside the dock: rebuild the item list from config.
    pub(super) fn rebuild_items(&mut self) {
        // Unanswered attention requests survive the rebuild.
        let waiting: Vec<(String, HWND)> = self.items.iter().filter(|it| it.attention).map(|it| (it.path.clone(), it.attention_hwnd)).collect();
        // So does each item's motion: a theme, DPI or display change
        // redraws the dock, it does not regrow it. Only items new to the
        // dock grow in from nothing (polish audit F1).
        let before: Vec<(CarryKey, Carried)> = self.items.iter().map(|it| (carry_key(it.kind, &it.path, &it.id), Carried::of(it))).collect();
        self.rebuild_items_inner();
        let keys: Vec<CarryKey> = self.items.iter().map(|it| carry_key(it.kind, &it.path, &it.id)).collect();
        // (A bounce under way only while bounces still may run: the
        // rebuild may be for Windows' animation effects going off.)
        let bounces = super::motion() && crate::effects::items().bounce;
        for (it, was) in self.items.iter_mut().zip(carry(&before, &keys)) {
            if let Some(was) = was {
                was.onto(it, bounces);
            }
        }
        // Full ink, no hue (the bounce tells it apart).
        let orange = rgba(self.theme.attention());
        let c = self.gfx.compositor.clone();
        for (path, h) in waiting {
            if let Some(it) = self.items.iter_mut().find(|it| it.kind == Kind::App && it.path == path && it.windows.contains(&h)) {
                it.attention = true;
                it.attention_hwnd = h;
                if let Ok(b) = c.CreateColorBrushWithColor(orange) {
                    paint_indicator(&it.dot, &b);
                }
            }
        }
    }

    fn rebuild_items_inner(&mut self) {
        if let Ok(ch) = self.layer.Children() {
            for it in &mut self.items {
                let _ = ch.Remove(&it.icon);
                let _ = ch.Remove(&it.plate);
                let _ = ch.Remove(&it.tile);
                let _ = ch.Remove(&it.dot);
                it.marks.remove_from(&ch);
            }
        }
        if self.build_items().is_ok() {
            self.sync_windows();
        }
    }

    pub(super) fn answer_ipc(&mut self, req: &str) -> String {
        if req.starts_with('{') {
            return self.answer_settings(req);
        }
        match req {
            "WidgetsAdd" => {
                crate::desklayer::edit::open_gallery();
                serde_json::json!({"type":"Ok"}).to_string()
            },
            "GetState" => serde_json::json!({
                "type": "State",
                "config": self.cfg,
                "theme": self.theme,
                "pipe": worker::ipc::pipe_name(),
                "hotkeys": crate::hotkeys::snapshot(),
                "widgets": crate::desklayer::load::status_json(),
            })
            .to_string(),
            other => serde_json::json!({"type": "Error", "msg": format!("unknown request {other:?}")}).to_string(),
        }
    }
}

impl Dock {
    /// Preview / Commit / Revert from the settings app. Only the fields the
    /// window edited (`keys`) are taken; everything else (pins, icons,
    /// changes made meanwhile) comes from the saved config.
    fn answer_settings(&mut self, line: &str) -> String {
        let reply = |m: serde_json::Value| m.to_string();
        let err = |m: &str| serde_json::json!({"type": "Error", "msg": m}).to_string();
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else { return err("bad json") };
        let conn = v.get("conn").and_then(|c| c.as_u64()).unwrap_or(0);
        let kind = v.get("type").and_then(|t| t.as_str()).unwrap_or("").to_string();
        if kind != "Quit" && self.preview.as_ref().is_some_and(|p| p.owner != conn) {
            return err("another settings window is previewing");
        }
        let edits = || -> Option<(Config, Vec<String>)> {
            let cfg: Config = serde_json::from_value(v.get("config")?.clone()).ok()?;
            let keys = v.get("keys")?.as_array()?.iter().filter_map(|k| k.as_str().map(String::from)).collect();
            Some((cfg, keys))
        };
        match v.get("type").and_then(|t| t.as_str()).unwrap_or("") {
            "NewNote" => {
                if self.cfg.widgets.notes && crate::notes::new_note() {
                    reply(serde_json::json!({"type": "Ok"}))
                } else { err("notes unavailable") }
            }
            "Preview" => {
                let Some((cfg, keys)) = edits() else { return err("bad preview") };
                let base = self.preview.take().map(|p| p.base).unwrap_or_else(|| self.cfg.clone());
                let p = PreviewState { owner: conn, base, edits: cfg, keys };
                let shown = p.shown();
                self.preview = Some(p);
                self.apply_config(shown);
                // Whether the previewed hotkeys could be registered.
                reply(serde_json::json!({"type": "Ok", "hotkeys": crate::hotkeys::snapshot(), "widgets": crate::desklayer::load::status_json()}))
            }
            "Commit" => {
                let Some((cfg, keys)) = edits() else { return err("bad commit") };
                let base = self.preview.as_ref().map(|p| p.base.clone()).unwrap_or_else(|| self.cfg.clone());
                // The window's keys over the newest file, under the write
                // lock (never over a save the saver thread is making); the
                // answer goes back only once it is on disk.
                let (out, saved, _) = crate::config::update(&base, |c| crate::settings::apply_keys(&cfg, c, &keys));
                if let Err(e) = saved {
                    // The preview (and 취소) stay as they were.
                    return err(&format!("save failed: {e}"));
                }
                let startup_ok = if keys.iter().any(|k| k == "start_with_windows") {
                    crate::settings::set_run_at_login(out.start_with_windows)
                } else {
                    true
                };
                self.preview = None;
                self.show_disk_config(out);
                reply(serde_json::json!({"type": "Ok", "startup_ok": startup_ok, "hotkeys": crate::hotkeys::snapshot()}))
            }
            "Quit" => {
                // Nothing previewed survives the quit.
                if let Some(p) = self.preview.take() {
                    self.apply_config(p.base);
                }
                // Quit shortly after: the pipe thread writes the reply first.
                let h = self.hwnd.0 as isize;
                let _ = std::thread::Builder::new().name("quit".into()).spawn(move || {
                    std::thread::sleep(std::time::Duration::from_millis(300));
                    unsafe {
                        let _ = PostMessageW(Some(HWND(h as *mut _)), WM_QUIT_REQUEST, WPARAM(0), LPARAM(0));
                    }
                });
                reply(serde_json::json!({"type": "Ok"}))
            }
            "Revert" => {
                // (Also the window went away: its recording is over.)
                self.suspend_keys(false);
                if let Some(p) = self.preview.take() {
                    self.apply_config(p.base);
                }
                reply(serde_json::json!({"type": "Ok"}))
            }
            // A shortcut is being recorded in Settings: our own hotkeys and
            // keyboard hook stand aside so the keys reach the recorder.
            "SuspendKeys" => {
                let on = v.get("on").and_then(|b| b.as_bool()).unwrap_or(false);
                self.suspend_keys(on);
                reply(serde_json::json!({"type": "Ok", "hotkeys": crate::hotkeys::snapshot()}))
            }
            other => err(&format!("unknown request {other:?}")),
        }
    }
}

/// Which item a rebuilt item is: its kind and the app it groups windows
/// by (crate::apps::AppId::matches): its AUMID first, else its exe, else
/// (case aside) its path. Two apps of one exe with their own AUMIDs are two
/// items, and each keeps its own motion.
pub(super) type CarryKey = (Kind, String);

pub(super) fn carry_key(kind: Kind, path: &str, id: &AppId) -> CarryKey {
    let ident = match (&id.aumid, &id.exe) {
        (Some(a), _) if !a.trim().is_empty() => format!("aumid:{}", a.trim().to_lowercase()),
        (_, Some(e)) if !e.trim().is_empty() => format!("exe:{}", e.trim().to_lowercase()),
        _ => format!("path:{}", path.to_lowercase()),
    };
    (kind, ident)
}

/// The motion an item is in, kept through a rebuild of the item list.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Carried {
    pres: f32,
    mag: f32,
    mag_t: f32,
    mag_v: f32,
    bounce: Option<Instant>,
}

impl Carried {
    fn of(it: &Item) -> Self {
        Carried { pres: it.pres, mag: it.mag, mag_t: it.mag_t, mag_v: it.mag_v, bounce: it.bounce }
    }

    /// Onto the rebuilt item (its presence target stays the rebuilt one's);
    /// its bounce only while `bounces` (animations and bounces on now).
    fn onto(self, it: &mut Item, bounces: bool) {
        it.pres = self.pres;
        it.mag = self.mag;
        it.mag_t = self.mag_t;
        it.mag_v = self.mag_v;
        it.bounce = carried_bounce(self.bounce, bounces);
    }
}

/// Pure: a bounce under way kept through a rebuild only while bounces may
/// run (crate::dock::motion and the bounce effect).
pub(super) fn carried_bounce(b: Option<Instant>, bounces: bool) -> Option<Instant> {
    b.filter(|_| bounces)
}

/// Pure: for each rebuilt item (`new`, in order) the motion of the item it
/// was before the rebuild (`old`), each old item taken once; None for an
/// item new to the dock (it grows in from nothing).
pub(super) fn carry(old: &[(CarryKey, Carried)], new: &[CarryKey]) -> Vec<Option<Carried>> {
    let mut used = vec![false; old.len()];
    new.iter()
        .map(|k| {
            let i = (0..old.len()).find(|&i| !used[i] && old[i].0 == *k)?;
            used[i] = true;
            Some(old[i].1)
        })
        .collect()
}

/// A pin saved under its shortcut's file name ("File Explorer", imported
/// from the taskbar) shows the name Windows gives that shortcut ("파일
/// 탐색기"). A pin with any other name keeps it.
fn shown_pin_name(name: &str, path: &str) -> String {
    let p = std::path::Path::new(path);
    let lnk = p.extension().is_some_and(|e| e.eq_ignore_ascii_case("lnk"));
    if lnk && p.file_stem().is_some_and(|s| s.to_string_lossy() == name) {
        if let Some(n) = crate::apps::localized_file_name(path) {
            return n;
        }
    }
    name.to_string()
}

/// Windows' own surfaces that open over the app in front (Start, search,
/// notifications and quick settings, the taskbar, Alt+Tab, Task View, the
/// hidden icons): not an app the top bar should name.
fn shell_surface(class: &str, exe: Option<&str>) -> bool {
    const CLASSES: [&str; 7] = [
        "Shell_TrayWnd",
        "Shell_SecondaryTrayWnd",
        "XamlExplorerHostIslandWindow",
        "TopLevelWindowForOverflowXamlIsland",
        "NotifyIconOverflowWindow",
        "MultitaskingViewFrame",
        "ForegroundStaging",
    ];
    const EXES: [&str; 4] = ["searchhost.exe", "searchapp.exe", "shellexperiencehost.exe", "shellhost.exe"];
    let name = exe.map(|e| e.rsplit(['\\', '/']).next().unwrap_or(e).to_ascii_lowercase());
    CLASSES.contains(&class) || crate::keys::is_start_app(exe) || name.is_some_and(|n| EXES.contains(&n.as_str()))
}

#[cfg(test)]
mod pin_name_tests {
    use super::shown_pin_name;

    #[test]
    fn imported_shortcuts_show_the_windows_name_renamed_pins_keep_theirs() {
        let dir = std::env::temp_dir().join(format!("nopbar-pin-name-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // How Windows writes it: UTF-16 with a BOM.
        let ini = "[LocalizedFileNames]\r\nFile Explorer.lnk=파일 탐색기\r\n";
        let mut bytes = vec![0xFF, 0xFE];
        bytes.extend(ini.encode_utf16().flat_map(u16::to_le_bytes));
        std::fs::write(dir.join("desktop.ini"), bytes).unwrap();
        let lnk = dir.join("File Explorer.lnk").to_string_lossy().into_owned();
        let other = dir.join("Google Chrome.lnk").to_string_lossy().into_owned();
        assert_eq!(shown_pin_name("File Explorer", &lnk), "파일 탐색기");
        assert_eq!(shown_pin_name("내 탐색기", &lnk), "내 탐색기", "renamed: kept");
        assert_eq!(shown_pin_name("Google Chrome", &other), "Google Chrome", "not in desktop.ini");
        assert_eq!(shown_pin_name("Steam", r"c:\program files (x86)\steam\steam.exe"), "Steam", "not a shortcut");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod shell_surface_tests {
    use super::shell_surface;

    #[test]
    fn windows_own_surfaces_are_not_apps() {
        let sys = r"c:\windows\systemapps\microsoftwindows.client.cbs_cw5n1h2txyewy\searchhost.exe";
        assert!(shell_surface("Windows.UI.Core.CoreWindow", Some(sys)), "Start / search");
        assert!(shell_surface("Windows.UI.Core.CoreWindow", Some(r"C:\Windows\SystemApps\ShellExperienceHost_cw5n1h2txyewy\ShellExperienceHost.exe")), "notifications");
        assert!(shell_surface("Windows.UI.Core.CoreWindow", Some(r"c:\windows\systemapps\microsoftwindows.client.cbs_cw5n1h2txyewy\shellhost.exe")), "quick settings");
        assert!(shell_surface("Shell_TrayWnd", Some(r"c:\windows\explorer.exe")), "the taskbar (Win+T, Win+B)");
        assert!(shell_surface("XamlExplorerHostIslandWindow", Some(r"c:\windows\explorer.exe")), "Alt+Tab, Task View");
        assert!(!shell_surface("CabinetWClass", Some(r"c:\windows\explorer.exe")), "a File Explorer window is an app");
        assert!(!shell_surface("Chrome_WidgetWin_1", Some(r"c:\program files\google\chrome\application\chrome.exe")));
        assert!(!shell_surface("Notepad", None));
    }
}

#[cfg(test)]
mod carry_tests {
    use super::*;

    fn at(pres: f32, mag: f32) -> Carried {
        Carried { pres, mag, mag_t: mag, mag_v: 0.0, bounce: None }
    }

    #[test]
    fn a_rebuild_keeps_items_where_they_were_and_grows_only_new_ones() {
        let old = vec![
            (carry_key(Kind::App, "C:/Apps/Edit.exe", &none()), at(1.0, 1.4)),
            (carry_key(Kind::Sep, "", &none()), at(1.0, 1.0)),
            (carry_key(Kind::App, "C:/Apps/Mail.exe", &none()), at(0.5, 1.0)),
            (carry_key(Kind::Trash, "trash", &none()), at(1.0, 1.0)),
        ];
        // A pin added (Notes), a path's case changed (settings).
        let new = vec![
            carry_key(Kind::App, "c:/apps/edit.exe", &none()),
            carry_key(Kind::App, "C:/Apps/Notes.exe", &none()),
            carry_key(Kind::Sep, "", &none()),
            carry_key(Kind::App, "C:/Apps/Mail.exe", &none()),
            carry_key(Kind::Trash, "trash", &none()),
        ];
        let got = carry(&old, &new);
        assert_eq!(got[0], Some(at(1.0, 1.4)), "a magnified pin stays magnified and fully shown");
        assert_eq!(got[1], None, "the new pin grows in");
        assert_eq!(got[2], Some(at(1.0, 1.0)));
        assert_eq!(got[3], Some(at(0.5, 1.0)), "one half way in goes on from there");
        assert_eq!(got[4], Some(at(1.0, 1.0)));
    }

    #[test]
    fn each_old_item_is_carried_once() {
        let k = carry_key(Kind::App, "a", &none());
        let old = vec![(k.clone(), at(1.0, 1.0))];
        assert_eq!(carry(&old, &[k.clone(), k]), vec![Some(at(1.0, 1.0)), None]);
        // Another kind with the same path is another item.
        assert_eq!(carry(&old, &[carry_key(Kind::Trash, "a", &none())]), vec![None]);
    }

    fn none() -> AppId {
        AppId { exe: None, aumid: None }
    }

    fn app(exe: &str, aumid: Option<&str>) -> AppId {
        AppId { exe: Some(exe.to_lowercase()), aumid: aumid.map(String::from) }
    }

    #[test]
    fn apps_of_one_exe_keep_their_own_motion_when_they_swap_places() {
        // One host exe, two apps (their own AUMIDs): A magnified and
        // bouncing, B at rest; the rebuild lists them B, A.
        let (a, b) = (app("C:/Host/host.exe", Some("Vendor.A")), app("C:/Host/host.exe", Some("Vendor.B")));
        let t = Instant::now();
        let moving = Carried { pres: 1.0, mag: 1.5, mag_t: 1.5, mag_v: 0.2, bounce: Some(t) };
        let still = at(1.0, 1.0);
        let path = "C:/Host/host.exe";
        let old = vec![(carry_key(Kind::App, path, &a), moving), (carry_key(Kind::App, path, &b), still)];
        let new = vec![carry_key(Kind::App, path, &b), carry_key(Kind::App, path, &a)];
        assert_eq!(carry(&old, &new), vec![Some(still), Some(moving)], "each state follows its own app");
        // The AUMID's case does not matter (AppId::matches); without one,
        // the exe; without either, the path.
        assert_eq!(carry_key(Kind::App, path, &app(path, Some("vendor.a"))), carry_key(Kind::App, path, &a));
        assert_ne!(carry_key(Kind::App, path, &app(path, None)), carry_key(Kind::App, path, &a));
        assert_eq!(carry_key(Kind::App, "C:/X/Y.exe", &none()), carry_key(Kind::App, "c:/x/y.exe", &none()));
    }

    #[test]
    fn a_rebuild_after_animations_go_off_does_not_bring_a_bounce_back() {
        let t = Some(Instant::now());
        assert_eq!(carried_bounce(t, true), t, "still allowed: it goes on");
        assert_eq!(carried_bounce(t, false), None, "Windows' animation effects (or bounces) off: gone");
        assert_eq!(carried_bounce(None, true), None);
    }
}

/// Icon surfaces kept for apps no longer on the dock (one that comes back
/// soon shows its icon at once); past this, the least recently used go.
pub(super) const SPARE_ICONS: usize = 24;

/// Cached keys to drop: those not on the dock (`live`) beyond the `spare`
/// most recently used (`cached`: key and last-use stamp).
pub(super) fn icons_to_drop<K: Clone + Eq + std::hash::Hash>(cached: &[(K, u64)], live: &HashSet<K>, spare: usize) -> Vec<K> {
    let mut idle: Vec<&(K, u64)> = cached.iter().filter(|(k, _)| !live.contains(k)).collect();
    idle.sort_by(|a, b| b.1.cmp(&a.1));
    idle.into_iter().skip(spare).map(|(k, _)| k.clone()).collect()
}

#[cfg(test)]
mod prune_tests {
    use super::*;

    #[test]
    fn live_icons_stay_and_idle_ones_are_capped() {
        let cached: Vec<(&str, u64)> = vec![("a", 1), ("b", 9), ("c", 5), ("d", 7), ("e", 2)];
        let live: HashSet<&str> = ["a", "e"].into_iter().collect();
        // Two spares: the newest idle ones (b, d) stay; c goes; live never.
        assert_eq!(icons_to_drop(&cached, &live, 2), vec!["c"]);
        let mut all = icons_to_drop(&cached, &live, 0);
        all.sort();
        assert_eq!(all, vec!["b", "c", "d"]);
        assert!(icons_to_drop(&cached, &live, 10).is_empty());
    }
}

/// An item's brushes (icon, backplate) at each step from rest to the peak
/// (sharp.rs), with the sides (device px) of the pictures they show,
/// ascending: the resting one first (when it could be drawn), the peak one
/// last.
pub(super) struct Pics {
    pub icon: Vec<CompositionBrush>,
    pub icon_px: Vec<u32>,
    /// Empty: no backplate.
    pub plate: Vec<CompositionBrush>,
    pub plate_px: Vec<u32>,
}

impl Pics {
    pub(super) fn new(icon: Vec<(u32, CompositionBrush)>, plate: Vec<(u32, CompositionBrush)>) -> Self {
        let (icon_px, icon) = icon.into_iter().unzip();
        let (plate_px, plate) = plate.into_iter().unzip();
        Pics { icon, icon_px, plate, plate_px }
    }

    /// Side of the icon picture shown at rest.
    pub(super) fn rest_px(&self) -> u32 {
        self.icon_px.first().copied().unwrap_or(0)
    }
}

impl Item {
    /// Shows the pictures for an icon drawn `icon_px` wide on a backplate
    /// (or slot) `plate_px` wide, in device px (sharp::pick).
    pub(super) fn show_pics(&mut self, icon_px: f32, plate_px: f32) {
        let Some(p) = &self.pics else { return };
        let i = sharp::pick(icon_px, &p.icon_px);
        if i != self.shown.0 {
            if let Some(b) = p.icon.get(i) {
                let _ = self.icon.SetBrush(b);
            }
            self.shown.0 = i;
        }
        let j = sharp::pick(plate_px, &p.plate_px);
        if j != self.shown.1 && !p.plate.is_empty() {
            if let Some(b) = p.plate.get(j) {
                let _ = self.plate.SetBrush(b);
            }
            self.shown.1 = j;
        }
    }

    /// New pictures: the resting ones shown until the next layout says
    /// otherwise.
    pub(super) fn show_rest(&mut self) {
        self.shown = (usize::MAX, usize::MAX);
        self.show_pics(0.0, 0.0);
    }

    /// Its icon for something drawn larger than the dock at rest (a
    /// preview card, the split overlay, the island): the peak picture.
    pub(super) fn large_brush(&self) -> Option<CompositionBrush> {
        self.pics.as_ref().and_then(|p| p.icon.last().cloned()).or_else(|| self.icon.Brush().ok())
    }
}

/// The picture a step `side` px wide is drawn from: the icon thread's own
/// one of that side if it sent one, else the peak's `img`, shrunk.
pub(super) fn step_source<'a>(side: u32, img: &'a icons::Icon, small: &'a [icons::Icon]) -> &'a icons::Icon {
    small.iter().find(|i| i.width == side && i.height == side).unwrap_or(img)
}

/// Windows' own apps, the Recycle Bin and stacks keep their own shape,
/// unless the picture is one of NopBar's own full tiles (`drawn`,
/// icons::drawn_tile): those are cut to the squircle like other full-bleed
/// icons. (Drive roots, the shell's pictures and chosen pictures are not.)
pub(super) fn keeps_own_shape(it: &Item, drawn: bool) -> bool {
    own_shape(it.kind == Kind::Trash || it.stack, it.id.is_system(), drawn)
}

/// keeps_own_shape: pure.
pub(super) fn own_shape(trash_or_stack: bool, system: bool, drawn: bool) -> bool {
    system || (trash_or_stack && !drawn)
}

/// Whether a dock asks the icon thread for an icon it is about to show:
/// the primary always (it re-checks the disk cache), the trash always (its
/// state), a dock with nothing remembered, and one whose remembered
/// picture is a drawn tile for another look (`remembered`: Some(the
/// picture's drawn look, None when not drawn)) - shown meanwhile.
pub(super) fn ask_for_icon(primary: bool, trash: bool, remembered: Option<Option<icons::Appearance>>, now: icons::Appearance) -> bool {
    match remembered {
        None => true,
        Some(drawn) => primary || trash || drawn.is_some_and(|l| l != now),
    }
}

/// What dress_icon needs from its dock.
pub(super) struct DressCtx {
    pub c: Compositor,
    /// Squircles on (and their masks drawn).
    pub squircle: bool,
    /// Backplate masks at each step's slot side (sharp::step_slots),
    /// ascending; empty with squircles off.
    pub masks: Vec<(u32, CompositionDrawingSurface)>,
    pub plate_fill: Option<crate::theme::Rgba>,
    pub light: bool,
    /// Resting slot and peak square sides (device px).
    pub slot: u32,
    pub big: u32,
}

/// Times a failed resting picture is drawn again (on the next dress).
pub(super) const REST_TRIES: u8 = 3;

/// How well the pictures dress_icon was given fit the item.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Fit {
    /// Drawn for its dress and resting size.
    Yes,
    /// Drawn for another dress or size (or none could be shown): shown
    /// anyway; to be drawn again.
    Other,
    /// A smaller step failed (the resting one too, maybe: then a larger
    /// one is shown at rest, at its own size, not snapped as if 1:1); to
    /// be drawn again (bounded).
    NoRest,
}

/// Puts an app icon on an item (sharp::dress), as brushes for each step
/// from rest to the peak:
/// - squircle off, or the item keeps its own shape: the icon as is;
/// - full-bleed icon (coverage >= 0.9): the icon cut to the squircle (in
///   its pictures: Gfx::surface_fit);
/// - anything else: the icon at 78% on a squircle backplate in its own
///   (darkened) average color.
///
/// The pictures used are those drawn for the item's dress and resting
/// slot; without them, the closest are shown until they are drawn.
pub(super) fn dress_icon(x: &DressCtx, it: &mut Item, pic: &views::Pic) -> Fit {
    let c = &x.c;
    let d = sharp::dress(x.squircle, pic.bundled || keeps_own_shape(it, pic.drawn.is_some()), pic.info.coverage);
    let plated = d == sharp::Dress::Plated;
    let exact = pic.get(d, x.slot);
    let Some(size) = exact.or_else(|| pic.closest(d)) else { return Fit::Other };
    let plate = match x.plate_fill {
        Some(f) => color(f.a, f.r, f.g, f.b),
        None => {
            let (r, g, b) = icons::plate_color(pic.info, x.light);
            color(255, r, g, b)
        }
    };
    // One picture sampled per brush (the squircle is in the masked ones).
    let icon_brush = |s: &CompositionDrawingSurface| -> Option<CompositionBrush> {
        let b = c.CreateSurfaceBrushWithSurface(s).ok()?;
        let _ = b.SetStretch(CompositionStretch::Uniform);
        b.cast().ok()
    };
    let icons: Vec<(u32, CompositionBrush)> = size.steps.iter().filter_map(|(px, s)| Some((*px, icon_brush(s)?))).collect();
    if icons.last().map(|i| i.0) != size.steps.last().map(|s| s.0) {
        return Fit::Other;
    }
    // The backplate: its color through the squircle mask of each step.
    let plates: Vec<(u32, CompositionBrush)> = if plated {
        let Ok(fill) = c.CreateColorBrushWithColor(plate).and_then(|b| b.cast::<CompositionBrush>()) else { return Fit::Other };
        x.masks
            .iter()
            .filter_map(|(px, m)| {
                let mb = c.CreateSurfaceBrushWithSurface(m).ok()?;
                let _ = mb.SetStretch(CompositionStretch::Fill);
                let masked = c.CreateMaskBrush().ok()?;
                let _ = masked.SetSource(&fill);
                let _ = masked.SetMask(&mb);
                Some((*px, masked.cast().ok()?))
            })
            .collect()
    } else {
        Vec::new()
    };
    if plated && plates.is_empty() {
        return Fit::Other;
    }
    let rest_ok = !size.failed && plates.len() == if plated { x.masks.len() } else { 0 };
    it.pics = Some(Pics::new(icons, plates));
    it.plated = plated;
    let _ = it.plate.SetOpacity(if plated { 1.0 } else { 0.0 });
    // Shown at rest until the next layout says otherwise.
    it.show_rest();
    if exact.is_none() {
        Fit::Other
    } else if !rest_ok {
        Fit::NoRest
    } else {
        Fit::Yes
    }
}

/// `cfg` with the dock's unsaved edits applied over it, oldest first.
fn with_pending(mut cfg: Config, pending: &std::collections::VecDeque<(u64, crate::worker::ConfigEdit)>) -> Config {
    for (_, edit) in pending {
        edit(&mut cfg);
    }
    cfg
}

/// The dock's own pin / unpin (`before` -> `after`, the lists it showed)
/// applied to `latest`, the newest list on disk: pins it took off are
/// removed, pins it added go in after the pin they follow on the dock (at
/// the front if they lead, else at their place, else last). Everything
/// else in `latest` stays as it is there, order included (pins added,
/// removed or moved elsewhere and not shown here yet). Paths compare
/// without case, as drag.rs's merge_pins does.
pub(super) fn pin_change(latest: &[PinEntry], before: &[PinEntry], after: &[PinEntry]) -> Vec<PinEntry> {
    let same = |a: &PinEntry, b: &PinEntry| a.path.eq_ignore_ascii_case(&b.path);
    let mut out: Vec<PinEntry> = latest.iter().filter(|l| !(before.iter().any(|b| same(b, l)) && !after.iter().any(|a| same(a, l)))).cloned().collect();
    for (i, p) in after.iter().enumerate() {
        if before.iter().any(|b| same(b, p)) || out.iter().any(|o| same(o, p)) {
            continue;
        }
        let at = match i.checked_sub(1).and_then(|j| after.get(j)) {
            None => 0,
            Some(prev) => out.iter().position(|o| same(o, prev)).map(|k| k + 1).unwrap_or(i.min(out.len())),
        };
        out.insert(at, p.clone());
    }
    out
}

/// Pure: pin `latest` (the newest config's, at `prev`'s path) once the
/// dock followed its web app from `prev` (the entry it had) to `new`. None:
/// another app's pin is there now (not this one to change). Its name is
/// `new`'s unless it was renamed meanwhile (the settings, a hand edit:
/// the user's name stays); the path and the app's details are `new`'s.
pub(super) fn followed_pin(latest: &PinEntry, prev: &PinEntry, new: &PinEntry) -> Option<PinEntry> {
    let who = |e: &PinEntry| e.web.as_ref().map(|w| w.aumid.to_ascii_lowercase());
    if let (Some(l), Some(n)) = (who(latest), who(new)) {
        if l != n {
            return None;
        }
    }
    let name = if latest.name == prev.name { new.name.clone() } else { latest.name.clone() };
    Some(PinEntry { name, path: new.path.clone(), web: new.web.clone() })
}

/// Edits up to request `gen` are on disk now.
fn drop_saved(pending: &mut std::collections::VecDeque<(u64, crate::worker::ConfigEdit)>, gen: u64) {
    pending.retain(|(g, _)| *g > gen);
}

#[cfg(test)]
mod pending_edit_tests {
    use super::*;

    #[test]
    fn a_followed_pin_keeps_a_name_given_meanwhile() {
        let web = |aumid: &str, name: &str| Some(crate::webapps::WebPin { aumid: aumid.into(), name: Some(name.into()), ..Default::default() });
        let a = "Chrome._crx_aaaa";
        let prev = PinEntry { name: "App".into(), path: r"C:\SM\App.lnk".into(), web: web(a, "App") };
        // The dock saw the shortcut renamed: new path, the app's new name.
        let new = PinEntry { name: "New".into(), path: r"C:\SM\New.lnk".into(), web: web(a, "New") };
        // Nothing else changed: the dock's entry.
        assert_eq!(followed_pin(&prev, &prev, &new), Some(new.clone()));
        // Renamed in the settings before the save: the user's name stays,
        // the path and the app's details follow.
        let renamed = PinEntry { name: "업무 메일".into(), ..prev.clone() };
        let out = followed_pin(&renamed, &prev, &new).unwrap();
        assert_eq!(out.name, "업무 메일");
        assert_eq!(out.path, new.path);
        assert_eq!(out.web, new.web);
        // (Next time that name is not the dock's: never renamed again.)
        assert_ne!(out.web.as_ref().and_then(|w| w.name.as_deref()), Some(out.name.as_str()));
        // Another app's pin at that path now: left alone.
        let other = PinEntry { name: "B".into(), path: prev.path.clone(), web: web("Chrome._crx_bbbb", "B") };
        assert_eq!(followed_pin(&other, &prev, &new), None);
        // An older pin there (no web details): followed (same AUMID, any case).
        let old = PinEntry::new("App".into(), prev.path.clone());
        assert_eq!(followed_pin(&old, &prev, &new).map(|p| p.path), Some(new.path.clone()));
        let upper = PinEntry { web: web(&a.to_uppercase(), "App"), ..prev.clone() };
        assert!(followed_pin(&upper, &prev, &new).is_some());
    }

    #[test]
    fn an_earlier_save_result_keeps_later_edits_on_screen() {
        let mut pending: std::collections::VecDeque<(u64, crate::worker::ConfigEdit)> = Default::default();
        pending.push_back((1, std::sync::Arc::new(|c: &mut Config| c.dock.autohide = true)));
        pending.push_back((2, std::sync::Arc::new(|c: &mut Config| c.hide_taskbar = false)));
        // The saver reports request 1: the file has edit 1 and an outside
        // change, not yet edit 2.
        let mut disk = Config::default();
        disk.dock.autohide = true;
        disk.hide_taskbar = true;
        disk.topbar.enabled = !Config::default().topbar.enabled;
        drop_saved(&mut pending, 1);
        assert_eq!(pending.len(), 1);
        let shown = with_pending(disk, &pending);
        assert!(shown.dock.autohide, "saved edit");
        assert!(!shown.hide_taskbar, "later edit not undone");
        assert_ne!(shown.topbar.enabled, Config::default().topbar.enabled, "outside change kept");
        drop_saved(&mut pending, 2);
        assert!(pending.is_empty());
    }

    #[test]
    fn menu_pin_and_unpin_keep_outside_pin_changes() {
        let p = |s: &str| PinEntry::new(s.into(), s.into());
        let paths = |v: Vec<PinEntry>| v.into_iter().map(|e| e.path).collect::<Vec<_>>();
        // The dock showed A, B and pins C from its menu; meanwhile the
        // settings window added X and removed B on disk.
        let latest = vec![p("A"), p("X")];
        let out = pin_change(&latest, &[p("A"), p("B")], &[p("A"), p("B"), p("C")]);
        assert_eq!(paths(out), ["A", "X", "C"], "B stays removed, X stays, C goes in after B's place (B gone: at its index)");
        // Unpinning A keeps the outside X.
        let out = pin_change(&latest, &[p("A"), p("B")], &[p("B")]);
        assert_eq!(paths(out), ["X"]);
        // A pin added after a pin that is still there goes right after it.
        let out = pin_change(&[p("A"), p("B"), p("X")], &[p("A")], &[p("A"), p("C")]);
        assert_eq!(paths(out), ["A", "C", "B", "X"]);
        // Already pinned elsewhere: not twice (case does not matter).
        let out = pin_change(&[p("a"), p("c")], &[p("A")], &[p("A"), p("C")]);
        assert_eq!(paths(out), ["a", "c"]);
        // Nothing changed on disk: exactly the dock's list.
        let out = pin_change(&[p("A"), p("B")], &[p("A"), p("B")], &[p("A"), p("B"), p("C")]);
        assert_eq!(paths(out), ["A", "B", "C"]);
    }
}

/// The matte slab's fill: the theme's tint at its opacity, a little
/// lighter at the top and darker at the bottom.
fn matte_brush(c: &Compositor, m: &crate::theme::Matte) -> Result<windows::UI::Composition::CompositionLinearGradientBrush> {
    let t = crate::theme::parse_color(&m.tint).unwrap_or(Rgba { r: 0x20, g: 0x20, b: 0x24, a: 255 });
    let a = (m.opacity.clamp(0.0, 1.0) * 255.0).round() as u8;
    let g = m.gradient.clamp(0.0, 0.3);
    let mix = |v: u8, to: f32, k: f32| (v as f32 + (to - v as f32) * k).round().clamp(0.0, 255.0) as u8;
    let top = color(a, mix(t.r, 255.0, g), mix(t.g, 255.0, g), mix(t.b, 255.0, g));
    let bottom = color(a, mix(t.r, 0.0, g), mix(t.g, 0.0, g), mix(t.b, 0.0, g));
    let b = c.CreateLinearGradientBrush()?;
    b.SetStartPoint(Vector2 { X: 0.0, Y: 0.0 })?;
    b.SetEndPoint(Vector2 { X: 0.0, Y: 1.0 })?;
    let stops = b.ColorStops()?;
    stops.Append(&c.CreateColorGradientStopWithOffsetAndColor(0.0, top)?)?;
    stops.Append(&c.CreateColorGradientStopWithOffsetAndColor(1.0, bottom)?)?;
    Ok(b)
}

#[cfg(test)]
mod icon_look_tests {
    use super::*;
    use crate::icons::{reply_stale, Appearance};

    const DARK: Appearance = Appearance { light: false, hc: None, flat: false };
    const LIGHT: Appearance = Appearance { light: true, hc: None, flat: false };
    const HC_A: Appearance = Appearance { light: false, hc: Some(([0, 0, 0], [255, 255, 255])), flat: false };
    const HC_B: Appearance = Appearance { light: false, hc: Some(([0, 0, 0], [255, 255, 0])), flat: false };

    /// The dock's handling of icon replies in the order given: each reply
    /// (the look it was drawn for) is applied or asked for again in the
    /// dock's look at that moment (look changes between replies). Returns
    /// (the look of what is shown, the looks asked for again).
    fn run(drawn: bool, themed: bool, mut steps: Vec<Step>) -> (Option<Appearance>, Vec<Appearance>) {
        let (mut now, mut shown, mut asked) = (DARK, None, Vec::new());
        let mut queue: std::collections::VecDeque<Appearance> = Default::default();
        steps.reverse();
        while let Some(step) = steps.pop() {
            match step {
                Step::Look(l) => now = l,
                Step::Reply(got) => queue.push_back(got),
            }
            while let Some(got) = queue.pop_front() {
                if reply_stale(got, drawn, themed, now) {
                    asked.push(now);
                    // The worker answers the new request in that look.
                    steps.insert(0, Step::Reply(now));
                } else {
                    shown = Some(got);
                }
            }
        }
        (shown, asked)
    }

    #[derive(Clone, Copy)]
    enum Step {
        Look(Appearance),
        Reply(Appearance),
    }

    #[test]
    fn a_tile_drawn_for_the_old_look_is_asked_for_again() {
        // Dark request out, the dock turns light, the dark tile lands.
        let (shown, asked) = run(true, false, vec![Step::Look(LIGHT), Step::Reply(DARK)]);
        assert_eq!((shown, asked), (Some(LIGHT), vec![LIGHT]));
        // Another high-contrast palette, same light / dark.
        let (shown, asked) = run(true, false, vec![Step::Look(HC_A), Step::Look(HC_B), Step::Reply(HC_A)]);
        assert_eq!((shown, asked), (Some(HC_B), vec![HC_B]));
        // In time: taken as it is.
        assert_eq!(run(true, false, vec![Step::Reply(DARK)]), (Some(DARK), vec![]));
    }

    #[test]
    fn other_sources_keep_their_old_rules() {
        // A shell icon does not depend on the look.
        assert_eq!(run(false, false, vec![Step::Look(LIGHT), Step::Reply(DARK)]), (Some(DARK), vec![]));
        // A packaged app's logo: light / dark only, not the palette.
        assert_eq!(run(false, true, vec![Step::Look(LIGHT), Step::Reply(DARK)]), (Some(LIGHT), vec![LIGHT]));
        assert_eq!(run(false, true, vec![Step::Look(HC_B), Step::Reply(HC_A)]), (Some(HC_A), vec![]));
    }

    #[test]
    fn docks_ask_again_for_remembered_tiles_of_another_look() {
        // A view (not the primary) with a remembered picture skipped the
        // request; a drawn tile of another look is asked for anyway.
        assert!(!ask_for_icon(false, false, Some(None), LIGHT), "shell icon remembered");
        assert!(!ask_for_icon(false, false, Some(Some(LIGHT)), LIGHT), "tile of this look");
        assert!(ask_for_icon(false, false, Some(Some(DARK)), LIGHT), "tile of the old look");
        assert!(ask_for_icon(false, false, Some(Some(HC_A)), HC_B), "old palette");
        assert!(ask_for_icon(false, false, None, LIGHT));
        assert!(ask_for_icon(true, false, Some(None), LIGHT));
        assert!(ask_for_icon(false, true, Some(None), LIGHT));
    }

    #[test]
    fn only_drawn_tiles_lose_their_own_shape() {
        // Trash / stack drawn by NopBar: full tile, cut like other icons.
        assert!(!own_shape(true, false, true));
        // A drive root, the shell's bin or a chosen picture: as before.
        assert!(own_shape(true, false, false));
        // Windows' own apps keep theirs; other apps never did.
        assert!(own_shape(false, true, false) && own_shape(false, true, true));
        assert!(!own_shape(false, false, false));
    }
}

#[cfg(test)]
mod drawn_step_tests {
    use super::*;
    use crate::glyphs::{line_tile, tile, tile_kind, Glyph, TileKind};
    use crate::icons::{drawn_pictures, Appearance};
    use crate::launchpad::PIN;

    const DARK: Appearance = Appearance { light: false, hc: None, flat: false };
    const LIGHT: Appearance = Appearance { light: true, hc: None, flat: false };

    /// One picture a dock makes of the Launchpad pin: its side, and the
    /// side and pixels of the picture it is made from.
    struct Step {
        side: u32,
        from: u32,
        pixels: Vec<u8>,
    }

    /// The pictures a dock of `icon` DIP magnified up to `max` at `scale`
    /// makes of the Launchpad pin (a drawn tile), by the dock's own path:
    /// the sides it asks for (ask_icon: sharp::ask_sides), what the icon
    /// thread draws for them (icons::drawn_pictures), and what each step is
    /// made from (draw_pic: step_source, at sharp::step_sides). Also the
    /// resting slot and the peak.
    fn steps(icon: f32, max: f32, scale: f32, look: Appearance) -> (u32, u32, Vec<Step>) {
        let (big, slot) = (sharp::big_px(icon, max, scale) as u32, sharp::slot_px(icon, scale));
        let sides = sharp::ask_sides(&[slot], big);
        let (peak, small) = drawn_pictures(PIN, big as i32, &sides, look).unwrap();
        assert_eq!(peak.width, big);
        let v = sharp::step_sides(slot, big, false)
            .into_iter()
            .map(|side| {
                let s = step_source(side, &peak, &small);
                Step { side, from: s.width, pixels: s.pixels.clone() }
            })
            .collect();
        (slot, big, v)
    }

    #[test]
    fn a_small_resting_tile_is_drawn_at_its_own_size() {
        // The review's case: 100 %, icons 32, magnified 1.4: the peak is 45
        // px, the resting 32 px picture is the simple tile drawn at 32,
        // not the 45 px one shrunk.
        for look in [DARK, LIGHT] {
            let (slot, big, v) = steps(32.0, 1.4, 1.0, look);
            assert_eq!((slot, big), (32, 45));
            let rest = &v[0];
            assert_eq!((rest.side, rest.from), (32, 32), "light {}", look.light);
            assert_eq!(rest.pixels, tile(32, Glyph::Launchpad, look.tile()).pixels);
            assert!(v[1..].iter().all(|s| s.from == 45), "the magnified ones from the peak");
            // Not magnified (1.0): the same 32 px picture at rest.
            let (_, big, one) = steps(32.0, 1.0, 1.0, look);
            assert_eq!((big, one.len(), one[0].from), (32, 1, 32));
            assert_eq!(one[0].pixels, rest.pixels, "the magnification no longer changes the resting picture");
        }
    }

    #[test]
    fn the_line_and_detail_boundaries() {
        // 20 / 21: resting at 20 the line tile at 20; the magnified steps
        // (21 px up) from the simple 28 px peak.
        let (slot, big, v) = steps(20.0, 1.4, 1.0, DARK);
        assert_eq!((slot, big), (20, 28));
        assert_eq!((v[0].side, v[0].from), (20, 20));
        assert_eq!(v[0].pixels, line_tile(20, Glyph::Launchpad, DARK.tile()).pixels);
        assert!(v[1..].iter().all(|s| s.side > 20 && s.from == 28));
        // Resting at 21: everything from the simple peak.
        let (_, big, v) = steps(21.0, 1.4, 1.0, DARK);
        assert!(v.iter().all(|s| s.from == big), "{:?}", v.iter().map(|s| (s.side, s.from)).collect::<Vec<_>>());
        // 32 / 33: resting at 32 below a 34 px peak: its own; at 33 all
        // from the full peak.
        let (slot, big, v) = steps(32.0, 1.04, 1.0, DARK);
        assert_eq!((slot, big), (32, 34));
        assert_eq!((v[0].from, v.last().map(|s| s.from)), (32, Some(34)));
        let (_, big, v) = steps(33.0, 1.4, 1.0, DARK);
        assert!(v.iter().all(|s| s.from == big));
        // Drawn apart: only the smaller sides of another kind.
        assert_eq!(crate::icons::own_sides(45, &[32, 36, 40, 45], DARK), vec![32]);
        assert_eq!(crate::icons::own_sides(28, &[28, 25, 22, 20, 20], DARK), vec![20]);
        assert!(crate::icons::own_sides(30, &[21, 25, 30], DARK).is_empty());
        // High contrast: the line tile at every size, shrunk from the peak.
        let hc = Appearance { light: false, hc: Some(([0, 0, 0], [255, 255, 0])), flat: false };
        let (_, big, v) = steps(32.0, 1.4, 1.0, hc);
        assert!(v.iter().all(|s| s.from == big));
    }

    #[test]
    fn every_step_is_made_from_a_tile_of_its_kind() {
        // Docks across the settings' range: no step is shrunk from a tile
        // of another kind (line / simple / full: glyphs::tile_kind), and
        // a step of its own is the tile drawn at its size.
        let t = DARK.tile();
        for scale in [1.0, 1.25, 1.5, 2.0] {
            for icon in [16.0, 20.0, 21.0, 24.0, 26.0, 32.0, 33.0, 40.0, 48.0] {
                for max in [1.0, 1.2, 1.4, 2.0] {
                    let (_, big, v) = steps(icon, max, scale, DARK);
                    for s in &v {
                        let what = format!("{icon} DIP x{max} at {scale}: {} px from {}", s.side, s.from);
                        assert_eq!(tile_kind(s.side, &t), tile_kind(s.from, &t), "{what}");
                        assert!(s.from == big || s.from == s.side, "{what}");
                        if s.from != big {
                            assert_ne!(tile_kind(s.side, &t), TileKind::Full, "{what}: only smaller kinds drawn apart");
                        }
                    }
                }
            }
        }
    }
}
