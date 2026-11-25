//! Launchpad's App Library (pure): one last page after the Home pages with
//! every app, grouped into categories shown as big folder-like tiles (like
//! iOS). A tile shows its first apps as big icons (a click opens that app)
//! and the rest as a mini grid (a click opens the whole category).
//!
//! Categories come from a table of rules (RULES, first match wins) over
//! what the app list has: the app's name and its shell:AppsFolder id (an
//! AppUserModelID, a known-folder path of the program or the Start menu
//! shortcut, which names its Start menu folder, or a URL). Offline and
//! deterministic. The user's own choice (`launchpad.library`) wins over the
//! rules; hidden apps (`launchpad.hidden`) are in Hidden, at the end.
//!
//! Also here: where the Library page is among the pages, its dot, and the
//! tiles' geometry, hit tests and arrow keys.

use std::collections::{BTreeMap, HashSet};

use super::grid::{self, Arrow, Grid, Rect};

/// A category, in the order the Library shows them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Cat {
    Dev,
    Games,
    Creative,
    Office,
    Internet,
    Utilities,
    Other,
    Hidden,
}

impl Cat {
    /// Every category, in the Library's order.
    pub const ALL: [Cat; 8] = [Cat::Dev, Cat::Games, Cat::Creative, Cat::Office, Cat::Internet, Cat::Utilities, Cat::Other, Cat::Hidden];
    /// The ones "Move to category…" offers (Hidden is "Hide").
    pub const PICKABLE: [Cat; 7] = [Cat::Dev, Cat::Games, Cat::Creative, Cat::Office, Cat::Internet, Cat::Utilities, Cat::Other];

    /// As kept in `launchpad.library`.
    pub fn key(self) -> &'static str {
        match self {
            Cat::Dev => "dev",
            Cat::Games => "games",
            Cat::Creative => "creative",
            Cat::Office => "office",
            Cat::Internet => "internet",
            Cat::Utilities => "utilities",
            Cat::Other => "other",
            Cat::Hidden => "hidden",
        }
    }

    /// A kept key (None: unknown, ignored; "hidden" is not a choice).
    pub fn from_key(k: &str) -> Option<Cat> {
        Cat::PICKABLE.into_iter().find(|c| c.key() == k)
    }

    /// Its name's string key.
    pub fn label_key(self) -> &'static str {
        match self {
            Cat::Dev => "launchpad.library.cat.dev",
            Cat::Games => "launchpad.library.cat.games",
            Cat::Creative => "launchpad.library.cat.creative",
            Cat::Office => "launchpad.library.cat.office",
            Cat::Internet => "launchpad.library.cat.internet",
            Cat::Utilities => "launchpad.library.cat.utilities",
            Cat::Other => "launchpad.library.cat.other",
            Cat::Hidden => "launchpad.library.cat.hidden",
        }
    }
}

/// What a rule looks at.
#[derive(Clone, Copy, Debug)]
pub enum M {
    /// A whole word (or words) of the name, any case ("git" finds "Git
    /// Bash", not "Digit").
    Word(&'static str),
    /// Anywhere in the name, any case (names in scripts without spaces
    /// between words, and parts of words).
    Name(&'static str),
    /// Anywhere in the id, any case: an AUMID's package, a folder of the
    /// program's path or of its Start menu shortcut, a URL scheme.
    Id(&'static str),
}

use M::{Id, Name, Word};

/// Known-folder ids in app ids: System32, the Windows folder, SysWOW64.
const SYSTEM32: &str = "{1ac14e77-02e7-4e5d-b744-2eb1ae5198b7}";
const WINDIR: &str = "{f38bf404-1d43-42f2-9305-67de0b28fc23}";
const SYSWOW64: &str = "{d65231b0-b2f1-4857-a4ce-a8e7c6ea7d27}";

/// The category rules, first match wins (strong evidence before words:
/// a game's URL scheme or folder before any name, so a game called Rust or
/// "... Unity" is a game; then specific before general: PDF readers
/// before Adobe, developer tools before terminals and Windows' own tools,
/// games before drivers).
pub const RULES: &[(Cat, M)] = &[
    // Games by where they live: launcher URLs, library folders, packages.
    (Cat::Games, Id("steam://")),
    (Cat::Games, Id("com.epicgames.launcher://")),
    (Cat::Games, Id("uplay://")),
    (Cat::Games, Id("battlenet://")),
    (Cat::Games, Id("\\steamapps\\")),
    (Cat::Games, Id("steamlibrary")),
    (Cat::Games, Id("\\games\\")),
    (Cat::Games, Id("\\riot games\\")),
    (Cat::Games, Id("\\ubisoft")),
    (Cat::Games, Id("\\ea games\\")),
    (Cat::Games, Id("\\gog galaxy")),
    (Cat::Games, Id("microsoft.gamingapp")),
    (Cat::Games, Id("microsoft.xbox")),
    // PDF readers and Acrobat are office tools, not Adobe's creative suite.
    (Cat::Office, Name("acrobat")),
    (Cat::Office, Word("pdf")),
    // Development: IDEs, editors, SDKs, languages, terminals, developer prompts.
    (Cat::Dev, Word("visual studio")),
    (Cat::Dev, Id("\\microsoft visual studio")),
    (Cat::Dev, Id("visual studio")),
    (Cat::Dev, Id("microsoft.visualstudiocode")),
    (Cat::Dev, Id("vscode")),
    (Cat::Dev, Word("vs code")),
    (Cat::Dev, Word("developer")),
    (Cat::Dev, Name("native tools")),
    (Cat::Dev, Name("cross tools")),
    (Cat::Dev, Word("git")),
    (Cat::Dev, Word("github")),
    (Cat::Dev, Word("gitlab")),
    (Cat::Dev, Word("node")),
    (Cat::Dev, Word("node.js")),
    (Cat::Dev, Id("nodejs")),
    (Cat::Dev, Word("npm")),
    (Cat::Dev, Word("python")),
    (Cat::Dev, Id("\\python")),
    (Cat::Dev, Word("idle")),
    (Cat::Dev, Word("powershell")),
    (Cat::Dev, Id("microsoft.powershell")),
    (Cat::Dev, Word("terminal")),
    (Cat::Dev, Id("microsoft.windowsterminal")),
    (Cat::Dev, Word("command prompt")),
    (Cat::Dev, Name("명령 프롬프트")),
    (Cat::Dev, Name("터미널")),
    (Cat::Dev, Word("sdk")),
    (Cat::Dev, Word("jdk")),
    (Cat::Dev, Word("java")),
    (Cat::Dev, Word("rustup")),
    (Cat::Dev, Word("rust analyzer")),
    (Cat::Dev, Word("cmake")),
    (Cat::Dev, Word("jetbrains")),
    (Cat::Dev, Id("jetbrains")),
    (Cat::Dev, Word("intellij")),
    (Cat::Dev, Word("pycharm")),
    (Cat::Dev, Word("webstorm")),
    (Cat::Dev, Word("rider")),
    (Cat::Dev, Word("clion")),
    (Cat::Dev, Word("android studio")),
    (Cat::Dev, Word("docker")),
    (Cat::Dev, Word("wsl")),
    (Cat::Dev, Word("ubuntu")),
    (Cat::Dev, Word("debian")),
    (Cat::Dev, Word("putty")),
    (Cat::Dev, Word("winscp")),
    (Cat::Dev, Word("postman")),
    (Cat::Dev, Word("sql")),
    (Cat::Dev, Word("sql server")),
    (Cat::Dev, Word("mysql")),
    (Cat::Dev, Word("postgresql")),
    (Cat::Dev, Word("pgadmin")),
    (Cat::Dev, Word("dbeaver")),
    (Cat::Dev, Word("unity hub")),
    (Cat::Dev, Id("\\unity\\hub\\")),
    (Cat::Dev, Id("\\unity\\editor\\")),
    (Cat::Dev, Word("unreal engine")),
    (Cat::Dev, Id("\\ue_")),
    (Cat::Dev, Word("godot")),
    (Cat::Dev, Word("notepad++")),
    (Cat::Dev, Name("notepad++")),
    (Cat::Dev, Word("sublime")),
    (Cat::Dev, Word("cursor")),
    (Cat::Dev, Word("windsurf")),
    (Cat::Dev, Word("neovim")),
    (Cat::Dev, Word("vim")),
    (Cat::Dev, Word("emacs")),
    (Cat::Dev, Word("wireshark")),
    (Cat::Dev, Word("fiddler")),
    (Cat::Dev, Word("debuggable package manager")),
    // Games by name: launchers, stores, well-known games.
    (Cat::Games, Word("steam")),
    (Cat::Games, Word("epic games")),
    (Cat::Games, Word("battle.net")),
    (Cat::Games, Word("ubisoft")),
    (Cat::Games, Word("ubisoft connect")),
    (Cat::Games, Word("ea")),
    (Cat::Games, Word("origin")),
    (Cat::Games, Word("gog galaxy")),
    (Cat::Games, Word("riot client")),
    (Cat::Games, Word("league of legends")),
    (Cat::Games, Word("valorant")),
    (Cat::Games, Word("minecraft")),
    (Cat::Games, Word("roblox")),
    (Cat::Games, Word("xbox")),
    (Cat::Games, Word("game")),
    (Cat::Games, Word("games")),
    (Cat::Games, Word("playnite")),
    (Cat::Games, Word("nexon")),
    (Cat::Games, Name("넥슨")),
    (Cat::Games, Name("게임")),
    // Creative: images, video, audio, 3D, design; players too.
    (Cat::Creative, Word("adobe")),
    (Cat::Creative, Id("\\adobe\\")),
    (Cat::Creative, Word("photoshop")),
    (Cat::Creative, Word("illustrator")),
    (Cat::Creative, Word("premiere")),
    (Cat::Creative, Word("after effects")),
    (Cat::Creative, Word("lightroom")),
    (Cat::Creative, Word("blender")),
    (Cat::Creative, Word("figma")),
    (Cat::Creative, Word("gimp")),
    (Cat::Creative, Word("inkscape")),
    (Cat::Creative, Word("krita")),
    (Cat::Creative, Word("paint")),
    (Cat::Creative, Id("microsoft.paint")),
    (Cat::Creative, Name("그림판")),
    (Cat::Creative, Word("clip studio")),
    (Cat::Creative, Word("affinity")),
    (Cat::Creative, Word("davinci resolve")),
    (Cat::Creative, Word("obs studio")),
    (Cat::Creative, Word("obs")),
    (Cat::Creative, Word("audacity")),
    (Cat::Creative, Word("fl studio")),
    (Cat::Creative, Word("ableton")),
    (Cat::Creative, Word("reaper")),
    (Cat::Creative, Word("cubase")),
    (Cat::Creative, Word("capcut")),
    (Cat::Creative, Word("canva")),
    (Cat::Creative, Word("substance")),
    (Cat::Creative, Word("maya")),
    (Cat::Creative, Word("3ds max")),
    (Cat::Creative, Word("cinema 4d")),
    (Cat::Creative, Word("zbrush")),
    (Cat::Creative, Word("houdini")),
    (Cat::Creative, Word("aseprite")),
    (Cat::Creative, Word("photos")),
    (Cat::Creative, Id("microsoft.windows.photos")),
    (Cat::Creative, Word("clipchamp")),
    (Cat::Creative, Word("vlc")),
    (Cat::Creative, Word("spotify")),
    (Cat::Creative, Word("media player")),
    (Cat::Creative, Id("microsoft.zunemusic")),
    (Cat::Creative, Word("potplayer")),
    (Cat::Creative, Word("itunes")),
    // Internet and communication: browsers, mail, chat, calls, transfers.
    (Cat::Internet, Word("chrome")),
    (Cat::Internet, Word("firefox")),
    (Cat::Internet, Word("edge")),
    (Cat::Internet, Id("msedge")),
    (Cat::Internet, Word("opera")),
    (Cat::Internet, Word("brave")),
    (Cat::Internet, Word("vivaldi")),
    (Cat::Internet, Word("whale")),
    (Cat::Internet, Name("웨일")),
    (Cat::Internet, Word("internet explorer")),
    (Cat::Internet, Word("tor browser")),
    (Cat::Internet, Word("mail")),
    (Cat::Internet, Name("메일")),
    (Cat::Internet, Id("microsoft.windowscommunicationsapps")),
    (Cat::Internet, Word("outlook")),
    (Cat::Internet, Word("thunderbird")),
    (Cat::Internet, Word("slack")),
    (Cat::Internet, Word("discord")),
    (Cat::Internet, Word("teams")),
    (Cat::Internet, Id("msteams")),
    (Cat::Internet, Word("zoom")),
    (Cat::Internet, Word("skype")),
    (Cat::Internet, Word("telegram")),
    (Cat::Internet, Word("whatsapp")),
    (Cat::Internet, Word("signal")),
    (Cat::Internet, Word("kakaotalk")),
    (Cat::Internet, Name("카카오톡")),
    (Cat::Internet, Word("line")),
    (Cat::Internet, Word("wechat")),
    (Cat::Internet, Word("messenger")),
    (Cat::Internet, Word("filezilla")),
    (Cat::Internet, Word("qbittorrent")),
    // Productivity and office.
    (Cat::Office, Word("word")),
    (Cat::Office, Word("excel")),
    (Cat::Office, Word("powerpoint")),
    (Cat::Office, Word("onenote")),
    (Cat::Office, Word("access")),
    (Cat::Office, Word("publisher")),
    (Cat::Office, Word("office")),
    (Cat::Office, Id("microsoft.office")),
    (Cat::Office, Word("notion")),
    (Cat::Office, Word("obsidian")),
    (Cat::Office, Word("evernote")),
    (Cat::Office, Word("libreoffice")),
    (Cat::Office, Word("wps office")),
    (Cat::Office, Name("한컴")),
    (Cat::Office, Word("hwp")),
    (Cat::Office, Word("calendar")),
    (Cat::Office, Name("일정")),
    (Cat::Office, Word("to do")),
    (Cat::Office, Id("microsoft.todos")),
    (Cat::Office, Word("sticky notes")),
    (Cat::Office, Word("notepad")),
    (Cat::Office, Id("microsoft.windowsnotepad")),
    (Cat::Office, Name("메모장")),
    (Cat::Office, Word("wordpad")),
    (Cat::Office, Word("onedrive")),
    (Cat::Office, Word("google drive")),
    (Cat::Office, Word("dropbox")),
    (Cat::Office, Word("xmind")),
    (Cat::Office, Word("zotero")),
    (Cat::Office, Word("anki")),
    // Utilities and system: Windows' own tools, settings, drivers, archivers.
    (Cat::Utilities, Word("settings")),
    (Cat::Utilities, Id("windows.immersivecontrolpanel")),
    (Cat::Utilities, Name("설정")),
    (Cat::Utilities, Word("control panel")),
    (Cat::Utilities, Name("제어판")),
    (Cat::Utilities, Word("task manager")),
    (Cat::Utilities, Name("작업 관리자")),
    (Cat::Utilities, Word("file explorer")),
    (Cat::Utilities, Name("파일 탐색기")),
    (Cat::Utilities, Id("microsoft.windows.explorer")),
    (Cat::Utilities, Word("registry editor")),
    (Cat::Utilities, Word("calculator")),
    (Cat::Utilities, Id("microsoft.windowscalculator")),
    (Cat::Utilities, Name("계산기")),
    (Cat::Utilities, Word("snipping tool")),
    (Cat::Utilities, Id("microsoft.screensketch")),
    (Cat::Utilities, Name("캡처 도구")),
    (Cat::Utilities, Word("clock")),
    (Cat::Utilities, Id("microsoft.windowsalarms")),
    (Cat::Utilities, Word("camera")),
    (Cat::Utilities, Word("remote desktop")),
    (Cat::Utilities, Word("magnifier")),
    (Cat::Utilities, Word("narrator")),
    (Cat::Utilities, Word("on-screen keyboard")),
    (Cat::Utilities, Word("character map")),
    (Cat::Utilities, Word("disk")),
    (Cat::Utilities, Word("event viewer")),
    (Cat::Utilities, Word("services")),
    (Cat::Utilities, Word("system")),
    (Cat::Utilities, Word("windows tools")),
    (Cat::Utilities, Word("administrative tools")),
    (Cat::Utilities, Word("powertoys")),
    (Cat::Utilities, Word("7-zip")),
    (Cat::Utilities, Word("winrar")),
    (Cat::Utilities, Word("bandizip")),
    (Cat::Utilities, Word("everything")),
    (Cat::Utilities, Word("nvidia")),
    (Cat::Utilities, Word("geforce")),
    (Cat::Utilities, Word("amd")),
    (Cat::Utilities, Word("radeon")),
    (Cat::Utilities, Word("intel")),
    (Cat::Utilities, Word("realtek")),
    (Cat::Utilities, Word("logitech")),
    (Cat::Utilities, Word("razer")),
    (Cat::Utilities, Word("driver")),
    (Cat::Utilities, Word("backup")),
    (Cat::Utilities, Word("recovery")),
    (Cat::Utilities, Word("security")),
    (Cat::Utilities, Word("defender")),
    (Cat::Utilities, Word("vpn")),
    (Cat::Utilities, Word("microsoft store")),
    (Cat::Utilities, Word("nopbar")),
    (Cat::Utilities, Word("windows")),
    (Cat::Utilities, Id(SYSTEM32)),
    (Cat::Utilities, Id(WINDIR)),
    (Cat::Utilities, Id(SYSWOW64)),
];

/// Is `needle` (lowercase) in `hay` (lowercase) as a whole word: nothing
/// letter- or digit-like right before or after it?
pub fn has_word(hay: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return false;
    }
    let edge = |c: Option<char>| c.is_none_or(|c| !c.is_alphanumeric());
    hay.match_indices(needle).any(|(i, _)| edge(hay[..i].chars().next_back()) && edge(hay[i + needle.len()..].chars().next()))
}

/// Does rule `m` match `name` and `id` (both lowercase)?
fn matches(m: M, name: &str, id: &str) -> bool {
    match m {
        Word(w) => has_word(name, w),
        Name(s) => name.contains(s),
        Id(s) => id.contains(s),
    }
}

/// The category the rules give an app (Other when none matches).
pub fn categorize(name: &str, id: &str) -> Cat {
    let (n, i) = (name.to_lowercase(), id.to_lowercase());
    RULES.iter().find(|(_, m)| matches(*m, &n, &i)).map_or(Cat::Other, |(c, _)| *c)
}

/// Where an app goes in the Library: Hidden if hidden, else the user's
/// choice (`overrides`, an unknown key ignored), else the rules.
pub fn place(id: &str, name: &str, overrides: &BTreeMap<String, String>, hidden: &HashSet<&str>) -> Cat {
    if hidden.contains(id) {
        return Cat::Hidden;
    }
    overrides.get(id).and_then(|k| Cat::from_key(k)).unwrap_or_else(|| categorize(name, id))
}

/// The user moves app `id` to `cat`: kept as an override, or none when the
/// rules give that category anyway (the file stays small). True: changed.
pub fn set_category(overrides: &mut BTreeMap<String, String>, id: &str, name: &str, cat: Cat) -> bool {
    if cat == Cat::Hidden {
        return false;
    }
    let before = overrides.get(id).cloned();
    if categorize(name, id) == cat {
        overrides.remove(id);
    } else {
        overrides.insert(id.to_string(), cat.key().to_string());
    }
    overrides.get(id) != before.as_ref()
}

/// A category on the Library page and its apps (ids), most relevant first.
#[derive(Clone, Debug, PartialEq)]
pub struct Group {
    pub cat: Cat,
    pub apps: Vec<String>,
}

/// The Library's groups: every app (`apps`: (id, name), in name order) in
/// its category; categories with no app left out. Inside a category the
/// most relevant come first (they are the big icons): apps that do not
/// look like clutter (`clutter`), then those on Home (`home`), then by name.
pub fn groups(apps: &[(String, String)], overrides: &BTreeMap<String, String>, hidden: &HashSet<&str>, home: &HashSet<&str>, clutter: &dyn Fn(&str) -> bool) -> Vec<Group> {
    let mut by: BTreeMap<Cat, Vec<(u8, usize, String)>> = BTreeMap::new();
    let mut seen: HashSet<&str> = HashSet::new();
    for (k, (id, name)) in apps.iter().enumerate() {
        if id.is_empty() || !seen.insert(id.as_str()) {
            continue;
        }
        let rank = u8::from(clutter(id)) * 2 + u8::from(!home.contains(id.as_str()));
        by.entry(place(id, name, overrides, hidden)).or_default().push((rank, k, id.clone()));
    }
    Cat::ALL
        .into_iter()
        .filter_map(|cat| {
            let mut v = by.remove(&cat)?;
            v.sort();
            Some(Group { cat, apps: v.into_iter().map(|(_, _, id)| id).collect() })
        })
        .collect()
}

// ---- the page --------------------------------------------------------------------

/// The Library page's index: right after the Home pages, when it shows.
pub fn page(home_pages: usize, shown: bool) -> Option<usize> {
    shown.then_some(home_pages)
}

/// What page dot `p` looks like: a plain dot, or the Library's (a little
/// grid of four squares, like iOS's App Library).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dot {
    Page,
    Library,
}

pub fn dot(p: usize, library: Option<usize>) -> Dot {
    if library == Some(p) { Dot::Library } else { Dot::Page }
}

// ---- tiles -------------------------------------------------------------------------

/// A tile's edge at most (DIP): its big icons are never drawn larger than
/// the grid's icons (they come from the same icon size, kept sharp).
pub const TILE_MAX: f32 = 184.0;
pub const TILE_MIN: f32 = 96.0;
/// Between tiles, and the name band under each.
pub const TILE_GAP: f32 = 36.0;
pub const NAME_H: f32 = 30.0;
/// A category's name: the ramp's body strong (13, semibold).
pub const NAME_SIZE: f32 = crate::theme::ramp::BODY_STRONG.size;
/// At most this many tiles a row.
pub const MAX_COLS: usize = 4;
/// Inside a tile: margin and gap (shares of its edge) around the 2 x 2
/// quadrants; the last quadrant holds a 2 x 2 mini grid.
const MARGIN: f32 = 0.10;
const GAP: f32 = 0.07;
pub const MINI_SIDE: usize = 2;
/// The "new" dot on an icon: its diameter (DIP).
pub const NEW_DOT: f32 = 9.0;
/// A hidden app found by search shows this faintly: the one opacity of
/// what cannot be used now (6.2).
pub const DIMMED: f32 = crate::theme::DISABLED_OPACITY;

/// How many big icons and mini icons a tile of `n` apps shows: up to four
/// apps all big; more, three big and the next four small.
pub fn parts(n: usize) -> (usize, usize) {
    if n <= 4 { (n, 0) } else { (3, (n - 3).min(MINI_SIDE * MINI_SIDE)) }
}

/// The tiles' arrangement on the Library page (relative to the work area,
/// like the grid).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tiles {
    pub n: usize,
    pub cols: usize,
    pub rows: usize,
    pub side: f32,
    pub left: f32,
    pub top: f32,
    /// Between tiles: TILE_GAP, less in step with tiles under TILE_MIN.
    pub gap: f32,
    /// The name band under each tile: NAME_H, 0 when the names are hidden
    /// (tiles that would be smaller than TILE_NAMED with them).
    pub name_h: f32,
    /// The region's sides: a name (a little wider than its tile) is cut
    /// to them.
    pub bound: (f32, f32),
    /// On whole device pixels (Tiles::on_pixels): the scale and the work
    /// area's origin (monitor DIP). None: as laid out.
    pub px: Option<(f32, f32, f32)>,
}

/// A laid-out edge this close (device pixels) to a pixel's is on it.
const PX_SLACK: f32 = 1e-3;

/// Pure: the device pixels whose whole width is inside `a..b` (DIP, from
/// an origin `o` DIP) at scale `s`: the first one's start and the last
/// one's end, in device pixels.
fn pixels_in(a: f32, b: f32, o: f32, s: f32) -> (f32, f32) {
    (((o + a) * s - PX_SLACK).ceil(), ((o + b) * s + PX_SLACK).floor())
}

/// Tiles smaller than this with their names hide the names instead.
pub const TILE_NAMED: f32 = 64.0;

/// Pure: the biggest tiles for `n` in `cols` columns in a box `aw` x `ah`
/// with names `name_h` tall: their edge (TILE_MAX at most) and the gap
/// between them, TILE_GAP. `shrink`: under TILE_MIN the gaps shrink in
/// step with the tiles, so they always fit the box (0 with no room);
/// else the edge as TILE_GAP leaves it, whatever it is.
fn side_for(n: usize, cols: usize, aw: f32, ah: f32, name_h: f32, shrink: bool) -> (f32, f32) {
    let cols = cols.max(1);
    let (c, rows) = (cols as f32, n.div_ceil(cols).max(1) as f32);
    let w = (aw - TILE_GAP * (c - 1.0)) / c;
    let h = (ah - name_h * rows - TILE_GAP * (rows - 1.0)) / rows;
    let side = w.min(h).min(TILE_MAX);
    if side >= TILE_MIN || !shrink {
        return (side, TILE_GAP);
    }
    // Gaps a share of the tile (as at TILE_MIN): cols tiles and cols - 1
    // gaps across, rows tiles, names and gaps down.
    let k = TILE_GAP / TILE_MIN;
    let w = aw / (c + (c - 1.0) * k);
    let h = (ah - name_h * rows) / (rows + (rows - 1.0) * k);
    let side = w.min(h).max(0.0);
    (side, side * k)
}

/// Pure: the columns (1..=most) that give `n` tiles the biggest edge in
/// the box (ties: more columns), and that edge and gap.
fn best_cols(n: usize, most: usize, aw: f32, ah: f32, name_h: f32, shrink: bool) -> (usize, f32, f32) {
    let most = most.max(1);
    let side = |c: usize| side_for(n, c, aw, ah, name_h, shrink).0;
    let cols = (1..=most).rev().fold(most, |best, c| if side(c) > side(best) + 0.5 { c } else { best });
    let (side, gap) = side_for(n, cols, aw, ah, name_h, shrink);
    (cols, side, gap)
}

/// `n` tiles on the Library page, the app grid `g` laid out in the grid's
/// region `area` (grid::Regions::grid): in the grid's own box as big as
/// fit (TILE_MIN..TILE_MAX), at most MAX_COLS a row (more columns when
/// that makes them no smaller), centered. When they do not fit there at
/// TILE_MIN, in the whole region, between the grid's sides: as many
/// columns as make them biggest, tiles and gaps smaller in step, then
/// without names (tiles under TILE_NAMED). Never outside the region, so
/// every tile drawn is a tile a click finds.
pub fn tiles(n: usize, g: &Grid, area: &Rect) -> Tiles {
    let aw = g.cell_w * g.cols as f32;
    let ah = g.cell_h * g.rows as f32;
    let (mut cols, mut side, mut gap) = best_cols(n, n.clamp(1, MAX_COLS), aw, ah, NAME_H, false);
    let (mut bw, mut bh, mut name_h) = (aw, ah, NAME_H);
    let (mut left, mut top) = (g.left, g.top);
    if side < TILE_MIN {
        // The region's height (and the grid's width), any number of columns.
        (bh, top) = (area.h().max(0.0), area.t);
        let l = g.left.max(area.l);
        (bw, left) = ((g.left + aw).min(area.r) - l, l);
        (cols, side, gap) = best_cols(n, n.max(1), bw, bh, NAME_H, true);
        if side < TILE_NAMED {
            name_h = 0.0;
            (cols, side, gap) = best_cols(n, n.max(1), bw, bh, 0.0, true);
        }
    }
    let rows = n.div_ceil(cols).max(1);
    let tw = side * cols as f32 + gap * (cols as f32 - 1.0);
    let th = (side + name_h) * rows as f32 + gap * (rows as f32 - 1.0);
    Tiles { n, cols, rows, side, left: left + (bw - tw) / 2.0, top: top + ((bh - th) / 2.0).max(0.0), gap, name_h, bound: (area.l, area.r), px: None }
}

impl Tiles {
    /// No tiles yet.
    pub fn none() -> Tiles {
        Tiles { n: 0, cols: 1, rows: 1, side: TILE_MAX, left: 0.0, top: 0.0, gap: TILE_GAP, name_h: NAME_H, bound: (f32::NEG_INFINITY, f32::INFINITY), px: None }
    }

    /// The same tiles as drawn at scale `s` in a work area at `origin`
    /// (monitor DIP): squares of whole device pixels (Tiles::edge), each
    /// inside the one laid out, so inside the grid's region. What a click
    /// finds (tile, item, hit) is then what is drawn.
    pub fn on_pixels(self, s: f32, origin: (f32, f32)) -> Tiles {
        if !(s.is_finite() && s > 0.0) {
            return self;
        }
        Tiles { px: Some((s, origin.0, origin.1)), ..self }
    }

    /// Tile `k`'s square as laid out (right to left: rows mirrored).
    fn laid(&self, k: usize, rtl: bool) -> Rect {
        let (row, col) = (k / self.cols.max(1), k % self.cols.max(1));
        let col = if rtl { self.cols - 1 - col } else { col };
        let l = self.left + col as f32 * (self.side + self.gap);
        let t = self.top + row as f32 * (self.side + self.name_h + self.gap);
        Rect::new(l, t, l + self.side, t + self.side)
    }

    /// On pixels: the tiles' edge in device pixels, the biggest whole
    /// number that fits inside every place a tile is laid out (all the
    /// same size). None: not on pixels.
    fn edge_px(&self) -> Option<f32> {
        let (s, ox, oy) = self.px?;
        let cols = self.cols.max(1);
        let across = (0..cols).map(|c| self.laid(c, false)).map(|r| pixels_in(r.l, r.r, ox, s));
        let down = (0..self.rows.max(1)).map(|row| self.laid(row * cols, false)).map(|r| pixels_in(r.t, r.b, oy, s));
        Some(across.chain(down).map(|(a, b)| b - a).fold(f32::INFINITY, f32::min).max(0.0))
    }

    /// The tiles' edge as drawn: `side`, on pixels a whole number of
    /// device pixels (never more than `side`).
    pub fn edge(&self) -> f32 {
        match (self.edge_px(), self.px) {
            (Some(e), Some((s, ..))) => e / s,
            _ => self.side,
        }
    }

    /// Tile `k`'s square (right to left: rows mirrored): as laid out, or on
    /// pixels where it is drawn: Tiles::edge across, on whole device
    /// pixels, as near the middle of the one laid out as they go, inside
    /// it.
    pub fn tile(&self, k: usize, rtl: bool) -> Rect {
        let r = self.laid(k, rtl);
        let (Some(e), Some((s, ox, oy))) = (self.edge_px(), self.px) else { return r };
        let at = |a: f32, b: f32, o: f32| {
            let (first, end) = pixels_in(a, b, o, s);
            let mid = ((o + (a + b) / 2.0) * s - e / 2.0).round();
            mid.clamp(first, (end - e).max(first)) / s - o
        };
        let (l, t) = (at(r.l, r.r, ox), at(r.t, r.b, oy));
        Rect::new(l, t, l + e / s, t + e / s)
    }

    /// Tile `k`'s name, under it (no height: names hidden), inside the
    /// region's sides.
    pub fn name(&self, k: usize, rtl: bool) -> Rect {
        let r = self.tile(k, rtl);
        let (l, rr) = ((r.l - self.gap / 2.0).max(self.bound.0.min(r.l)), (r.r + self.gap / 2.0).min(self.bound.1.max(r.r)));
        Rect::new(l, r.b, rr, r.b + self.name_h)
    }

    /// The tile and its name (what a click on it hits).
    pub fn item(&self, k: usize, rtl: bool) -> Rect {
        let r = self.tile(k, rtl);
        Rect::new(r.l, r.t, r.r, r.b + self.name_h)
    }

    /// The grid the arrow keys walk (one page of tiles).
    pub fn grid(&self) -> Grid {
        Grid { cols: self.cols.max(1), rows: self.rows.max(1), left: self.left, top: self.top, cell_w: self.side + self.gap, cell_h: self.side + self.name_h + self.gap, label_h: grid::LABEL_H, icon_edge: grid::ICON }
    }

    /// What is under (x, y): a tile, and which part of it.
    pub fn hit(&self, x: f32, y: f32, rtl: bool, counts: &[usize]) -> Option<(usize, Part)> {
        let k = (0..self.n).find(|&k| self.item(k, rtl).contains(x, y))?;
        let r = self.tile(k, rtl);
        let (big, _) = parts(counts.get(k).copied().unwrap_or(0));
        let part = (0..big).find(|&q| quadrant(q, self.edge(), rtl).offset(r.l, r.t).contains(x, y)).map_or(Part::Rest, Part::Big);
        Some((k, part))
    }
}

/// A part of a tile.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Part {
    /// Big icon `k` (its app opens).
    Big(usize),
    /// The mini grid, the name, the rest (the category opens).
    Rest,
}

/// Quadrant `q` (0..4, row by row; mirrored right to left) of a tile
/// `side` wide, relative to the tile.
pub fn quadrant(q: usize, side: f32, rtl: bool) -> Rect {
    let m = side * MARGIN;
    let gap = side * GAP;
    let cell = (side - 2.0 * m - gap) / 2.0;
    let (row, col) = (q / 2, q % 2);
    let col = if rtl { 1 - col } else { col };
    let l = m + col as f32 * (cell + gap);
    let t = m + row as f32 * (cell + gap);
    Rect::new(l, t, l + cell, t + cell)
}

/// A big icon's square inside quadrant `q`: at most the grid's icon edge.
pub fn big_icon(q: usize, side: f32, rtl: bool) -> Rect {
    let r = quadrant(q, side, rtl);
    let e = r.w().min(grid::ICON);
    Rect::new(r.cx() - e / 2.0, r.cy() - e / 2.0, r.cx() + e / 2.0, r.cy() + e / 2.0)
}

/// Mini icon `k` (0..4) in the last quadrant.
pub fn mini_icon(k: usize, side: f32, rtl: bool) -> Rect {
    let q = quadrant(3, side, rtl);
    let gap = q.w() * 0.12;
    let cell = (q.w() - gap) / 2.0;
    let (row, col) = (k / MINI_SIDE, k % MINI_SIDE);
    let col = if rtl { MINI_SIDE - 1 - col } else { col };
    let l = q.l + col as f32 * (cell + gap);
    let t = q.t + row as f32 * (cell + gap);
    Rect::new(l, t, l + cell, t + cell)
}

/// Arrow key on tile `sel` of `n`: the tile it goes to (None at an edge).
pub fn step(sel: usize, t: &Tiles, key: Arrow, rtl: bool) -> Option<usize> {
    grid::step(sel, t.n, &t.grid(), key, rtl)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cat(name: &str, id: &str) -> Cat {
        categorize(name, id)
    }

    #[test]
    fn words_are_whole_words() {
        assert!(has_word("git bash", "git"));
        assert!(has_word("node.js website", "node"));
        assert!(has_word("node.js", "node.js"));
        assert!(!has_word("digit", "git"));
        assert!(!has_word("timeline", "line"));
        assert!(has_word("paint 3d", "paint"));
        assert!(has_word("paint.net", "paint"));
        assert!(!has_word("", "a") && !has_word("a", ""));
        assert!(has_word("microsoft edge", "edge") && !has_word("knowledge", "edge"));
    }

    #[test]
    fn realistic_apps_land_in_sensible_categories() {
        let pf = "{6D809377-6AF0-444B-8957-A3773F02200E}";
        let progs = "{0139D44E-6AFE-49F2-8690-3DAFCAE6FFB8}";
        let cases: &[(&str, &str, Cat)] = &[
            ("Visual Studio Code", "Microsoft.VisualStudioCode", Cat::Dev),
            ("Visual Studio 2022", &format!("{pf}\\Microsoft Visual Studio\\2022\\Community\\Common7\\IDE\\devenv.exe"), Cat::Dev),
            ("Developer Command Prompt for VS 2022", &format!("{progs}\\Visual Studio 2022\\Visual Studio Tools\\Developer Command Prompt for VS 2022.lnk"), Cat::Dev),
            ("Blend for Visual Studio 2022", &format!("{pf}\\Microsoft Visual Studio\\2022\\Community\\Common7\\IDE\\Blend.exe"), Cat::Dev),
            ("Git Bash", &format!("{pf}\\Git\\git-bash.exe"), Cat::Dev),
            ("Node.js", &format!("{pf}\\nodejs\\node.exe"), Cat::Dev),
            ("Python 3.15 (64-bit)", "C:\\Python315\\python.exe", Cat::Dev),
            ("IDLE (Python 3.15 64-bit)", "C:\\Python315\\Lib\\idlelib\\idle.pyw", Cat::Dev),
            ("Terminal", "Microsoft.WindowsTerminal_8wekyb3d8bbwe!App", Cat::Dev),
            ("Command Prompt", "{1AC14E77-02E7-4E5D-B744-2EB1AE5198B7}\\cmd.exe", Cat::Dev),
            ("Windows PowerShell", "{1AC14E77-02E7-4E5D-B744-2EB1AE5198B7}\\WindowsPowerShell\\v1.0\\powershell.exe", Cat::Dev),
            ("Unity Hub", &format!("{pf}\\Unity Hub\\Unity Hub.exe"), Cat::Dev),
            ("Unity 2022.3.10f1", &format!("{pf}\\Unity\\Hub\\Editor\\2022.3.10f1\\Editor\\Unity.exe"), Cat::Dev),
            ("Unreal Engine", &format!("{pf}\\Epic Games\\UE_5.4\\Engine\\Binaries\\Win64\\UnrealEditor.exe"), Cat::Dev),
            // Strong game evidence wins over a developer word in the name.
            ("Rust", "steam://rungameid/252490", Cat::Games),
            ("Assassin's Creed Unity", "C:\\Games\\Assassins Creed Unity\\ACU.exe", Cat::Games),
            ("Unreal Tournament", "com.epicgames.launcher://apps/UnrealTournament?action=launch", Cat::Games),
            ("Rust", "D:\\SteamLibrary\\steamapps\\common\\Rust\\RustClient.exe", Cat::Games),
            ("Steam", "{7C5A40EF-A0FB-4BFC-874A-C0F2E0B9FA8E}\\Steam\\steam.exe", Cat::Games),
            ("Counter-Strike 2", "steam://rungameid/730", Cat::Games),
            ("Hades", "D:\\SteamLibrary\\steamapps\\common\\Hades\\x64\\Hades.exe", Cat::Games),
            ("Fortnite", "com.epicgames.launcher://apps/Fortnite?action=launch", Cat::Games),
            ("Battle.net", "{7C5A40EF-A0FB-4BFC-874A-C0F2E0B9FA8E}\\Battle.net\\Battle.net Launcher.exe", Cat::Games),
            ("Epic Games Launcher", "{7C5A40EF-A0FB-4BFC-874A-C0F2E0B9FA8E}\\Epic Games\\Launcher\\Portal\\Binaries\\Win32\\EpicGamesLauncher.exe", Cat::Games),
            ("Xbox", "Microsoft.GamingApp_8wekyb3d8bbwe!Microsoft.Xbox.App", Cat::Games),
            ("Some Indie Game", "E:\\Games\\Indie\\game.exe", Cat::Games),
            ("Blender 4.2", &format!("{pf}\\Blender Foundation\\Blender 4.2\\blender-launcher.exe"), Cat::Creative),
            ("Adobe Photoshop 2025", &format!("{pf}\\Adobe\\Adobe Photoshop 2025\\Photoshop.exe"), Cat::Creative),
            ("Figma", "com.squirrel.figma.Figma", Cat::Creative),
            ("Paint", "Microsoft.Paint_8wekyb3d8bbwe!App", Cat::Creative),
            ("그림판", "Microsoft.Paint_8wekyb3d8bbwe!App", Cat::Creative),
            ("Adobe Acrobat", &format!("{pf}\\Adobe\\Acrobat DC\\Acrobat\\Acrobat.exe"), Cat::Office),
            ("Google Chrome", &format!("{pf}\\Google\\Chrome\\Application\\chrome.exe"), Cat::Internet),
            ("Microsoft Edge", "MSEdge", Cat::Internet),
            ("Discord", "com.squirrel.Discord.Discord", Cat::Internet),
            ("Slack", "com.squirrel.slack.slack", Cat::Internet),
            ("카카오톡", "{7C5A40EF-A0FB-4BFC-874A-C0F2E0B9FA8E}\\Kakao\\KakaoTalk\\KakaoTalk.exe", Cat::Internet),
            ("Outlook (new)", "Microsoft.OutlookForWindows_8wekyb3d8bbwe!Microsoft.OutlookforWindows", Cat::Internet),
            ("Word", "Microsoft.Office.WINWORD.EXE.15", Cat::Office),
            ("Excel", "Microsoft.Office.EXCEL.EXE.15", Cat::Office),
            ("Notion", "com.squirrel.Notion.Notion", Cat::Office),
            ("메모장", "Microsoft.WindowsNotepad_8wekyb3d8bbwe!App", Cat::Office),
            ("Settings", "windows.immersivecontrolpanel_cw5n1h2txyewy!microsoft.windows.immersivecontrolpanel", Cat::Utilities),
            ("설정", "windows.immersivecontrolpanel_cw5n1h2txyewy!microsoft.windows.immersivecontrolpanel", Cat::Utilities),
            ("계산기", "Microsoft.WindowsCalculator_8wekyb3d8bbwe!App", Cat::Utilities),
            ("Task Manager", "{1AC14E77-02E7-4E5D-B744-2EB1AE5198B7}\\Taskmgr.exe", Cat::Utilities),
            ("Registry Editor", "{F38BF404-1D43-42F2-9305-67DE0B28FC23}\\regedit.exe", Cat::Utilities),
            ("7-Zip File Manager", &format!("{pf}\\7-Zip\\7zFM.exe"), Cat::Utilities),
            ("NVIDIA app", &format!("{pf}\\NVIDIA Corporation\\NVIDIA app\\CEF\\NVIDIA app.exe"), Cat::Utilities),
            ("Weather", "Microsoft.BingWeather_8wekyb3d8bbwe!App", Cat::Other),
            ("Fancy Tool", "C:\\Tools\\fancy.exe", Cat::Other),
        ];
        for (name, id, want) in cases {
            assert_eq!(cat(name, id), *want, "{name} ({id})");
        }
    }

    #[test]
    fn the_users_clutter_sorts_into_categories_too() {
        let progs = "{0139D44E-6AFE-49F2-8690-3DAFCAE6FFB8}";
        for name in [
            "Uninstall Node.js",
            "Node.js website",
            "Node.js documentation",
            "Python 3.15 Manuals (64-bit)",
            "Python 3.15 Module Docs (64-bit)",
            "Git Release Notes",
            "Git FAQs (Frequently Asked Questions)",
            "Developer Command Prompt for VS 2022",
            "Developer PowerShell for VS 2022",
            "x64 Native Tools Command Prompt for VS 2022",
            "x86 Native Tools Command Prompt for VS 2022",
            "x64_x86 Cross Tools Command Prompt for VS 2022",
            "x86_x64 Cross Tools Command Prompt for VS 2022",
            "ARM64 Native Tools Command Prompt for VS 2022",
            "Developer Command Prompt for VS 2019",
            "Windows PowerShell (x86)",
            "Windows PowerShell ISE (x86)",
        ] {
            assert_eq!(cat(name, &format!("{progs}\\Tools\\{name}.lnk")), Cat::Dev, "{name}");
        }
        assert_eq!(cat("ODBC Data Sources (32-bit)", "{D65231B0-B2F1-4857-A4CE-A8E7C6EA7D27}\\odbcad32.exe"), Cat::Utilities, "a Windows tool");
        assert_eq!(cat("7-Zip Help", "{6D809377-6AF0-444B-8957-A3773F02200E}\\7-Zip\\7-zip.chm"), Cat::Utilities);
    }

    #[test]
    fn the_users_choice_wins_and_hidden_is_last() {
        let mut o = BTreeMap::new();
        let hidden: HashSet<&str> = ["h"].into_iter().collect();
        assert_eq!(place("g", "Git Bash", &o, &hidden), Cat::Dev);
        assert!(set_category(&mut o, "g", "Git Bash", Cat::Utilities));
        assert_eq!(place("g", "Git Bash", &o, &hidden), Cat::Utilities, "the override wins over the rules");
        assert!(!set_category(&mut o, "g", "Git Bash", Cat::Utilities), "already there");
        assert!(set_category(&mut o, "g", "Git Bash", Cat::Dev), "back to what the rules say");
        assert!(o.is_empty(), "no override kept for that");
        assert!(!set_category(&mut o, "g", "Git Bash", Cat::Hidden), "Hidden is not a category to move to");
        o.insert("x".into(), "nonsense".into());
        assert_eq!(place("x", "Steam", &o, &hidden), Cat::Games, "an unknown key is ignored");
        o.insert("h".into(), "games".into());
        assert_eq!(place("h", "Steam", &o, &hidden), Cat::Hidden, "hidden over everything");
        assert_eq!(Cat::from_key("hidden"), None);
        for c in Cat::PICKABLE {
            assert_eq!(Cat::from_key(c.key()), Some(c));
        }
    }

    #[test]
    fn groups_order_and_relevance() {
        let apps: Vec<(String, String)> = [
            ("b", "Blender"),
            ("dc", "Developer Command Prompt for VS 2022"),
            ("g", "Git Bash"),
            ("h", "Hidden Thing"),
            ("s", "Steam"),
            ("v", "Visual Studio Code"),
            ("w", "Weather"),
            ("s", "Steam"),
        ]
        .iter()
        .map(|(i, n)| (i.to_string(), n.to_string()))
        .collect();
        let hidden: HashSet<&str> = ["h"].into_iter().collect();
        let home: HashSet<&str> = ["v"].into_iter().collect();
        let clutter = |id: &str| id == "dc";
        let gs = groups(&apps, &BTreeMap::new(), &hidden, &home, &clutter);
        let cats: Vec<Cat> = gs.iter().map(|g| g.cat).collect();
        assert_eq!(cats, [Cat::Dev, Cat::Games, Cat::Creative, Cat::Other, Cat::Hidden], "empty categories left out, Hidden last");
        assert_eq!(gs[0].apps, ["v", "g", "dc"], "on Home first, clutter last, else by name");
        assert_eq!(gs[1].apps, ["s"], "listed twice: once");
        let total: usize = gs.iter().map(|g| g.apps.len()).sum();
        assert_eq!(total, 7, "every app once");
    }

    #[test]
    fn the_library_page_and_its_dot() {
        assert_eq!(page(3, true), Some(3), "right after the Home pages");
        assert_eq!(page(3, false), None);
        assert_eq!(page(0, true), Some(0), "even with no Home page");
        assert_eq!(dot(3, Some(3)), Dot::Library);
        assert_eq!(dot(2, Some(3)), Dot::Page);
        assert_eq!(dot(0, None), Dot::Page);
    }

    #[test]
    fn tiles_fit_and_hit() {
        assert_eq!(parts(0), (0, 0));
        assert_eq!(parts(4), (4, 0));
        assert_eq!(parts(5), (3, 2));
        assert_eq!(parts(40), (3, 4));
        for (w, h) in [(1920.0, 1048.0), (1366.0, 666.0), (1024.0, 560.0), (1080.0, 1888.0)] {
            let g = grid::grid(w, h, grid::LABEL_H);
            let area = grid::regions(w, h, false).grid;
            for n in 1..=8 {
                let t = tiles(n, &g, &area);
                assert!(t.cols <= MAX_COLS && t.cols * t.rows >= n, "{w}x{h} {n}: {t:?}");
                assert!(t.side >= TILE_MIN - 0.01 && t.side <= TILE_MAX + 0.01);
                for k in 0..n {
                    let r = t.item(k, false);
                    assert!(r.l >= -0.01 && r.r <= w + 0.01, "{w}x{h} {n}: tile {k} {r:?}");
                }
                // Nothing overlaps.
                for a in 0..n {
                    for b in a + 1..n {
                        let (p, q) = (t.item(a, false), t.item(b, false));
                        assert!(p.r <= q.l + 0.01 || q.r <= p.l + 0.01 || p.b <= q.t + 0.01 || q.b <= p.t + 0.01);
                    }
                }
            }
        }
        let g = grid::grid(1920.0, 1048.0, grid::LABEL_H);
        let t = tiles(8, &g, &grid::regions(1920.0, 1048.0, false).grid);
        assert_eq!((t.cols, t.rows), (4, 2));
        assert!((t.side - TILE_MAX).abs() < 0.01, "1080p: full size");
        // Hits: a big icon, the mini grid, the name.
        let counts = [10, 2, 1, 1, 1, 1, 1, 1];
        let r = t.tile(0, false);
        let b1 = quadrant(1, t.side, false).offset(r.l, r.t);
        assert_eq!(t.hit(b1.cx(), b1.cy(), false, &counts), Some((0, Part::Big(1))));
        let m = quadrant(3, t.side, false).offset(r.l, r.t);
        assert_eq!(t.hit(m.cx(), m.cy(), false, &counts), Some((0, Part::Rest)), "the mini grid opens the category");
        let n = t.name(0, false);
        assert_eq!(t.hit(n.cx(), n.cy() - 2.0, false, &counts), Some((0, Part::Rest)));
        let r1 = t.tile(1, false);
        let q3 = quadrant(3, t.side, false).offset(r1.l, r1.t);
        assert_eq!(t.hit(q3.cx(), q3.cy(), false, &counts), Some((1, Part::Rest)), "two apps: no third icon");
        assert_eq!(t.hit(0.0, 0.0, false, &counts), None);
        // Right to left: mirrored.
        assert_eq!(t.tile(0, true), t.tile(3, false));
        assert_eq!(quadrant(0, 100.0, true), quadrant(1, 100.0, false));
        let (a, b) = (mini_icon(1, 100.0, true), mini_icon(1, 100.0, false));
        assert!((a.l - (100.0 - b.r)).abs() < 0.01 && a.t == b.t, "mirrored across the tile");
        // Big icons never grow past the grid's icons; mini icons sit in the last quadrant.
        assert!(big_icon(0, TILE_MAX, false).w() <= grid::ICON + 0.01);
        let q = quadrant(3, 150.0, false);
        let mi = mini_icon(3, 150.0, false);
        assert!(mi.l >= q.l && mi.r <= q.r + 0.01 && mi.b <= q.b + 0.01);
    }

    /// Every tile of `t` (of a work area's regions `r`) where a click finds
    /// it: the tile, its name, its big and mini icons inside the grid's
    /// region, apart; each big icon's middle hits that icon, the mini grid
    /// and the name the tile.
    fn tiles_in_the_grid(t: &Tiles, r: &grid::Regions, counts: &[usize], what: &str) {
        let what = format!("{what}: {t:?}");
        let area = r.grid;
        assert!(t.cols * t.rows >= t.n && t.side > 0.0 && t.side <= TILE_MAX + 0.01, "{what}");
        assert!(t.name_h == NAME_H || (t.name_h == 0.0 && t.side >= TILE_NAMED - 0.01) || t.side < TILE_NAMED, "{what}");
        for k in 0..t.n {
            let (tile, name, item) = (t.tile(k, r.rtl), t.name(k, r.rtl), t.item(k, r.rtl));
            for x in [tile, name, item] {
                assert!(near_holds(&area, &x), "{what}: tile {k} {x:?} out of the grid's region {area:?}");
            }
            for b in k + 1..t.n {
                assert!(!item.overlaps(&t.item(b, r.rtl)), "{what}: tiles {k} and {b}");
            }
            let (big, mini) = parts(counts[k]);
            for q in 0..big {
                let i = big_icon(q, t.edge(), r.rtl).offset(tile.l, tile.t);
                assert!(near_holds(&area, &i), "{what}: tile {k} icon {q}");
                assert_eq!(r.at(i.cx(), i.cy()), Some(grid::Area::Grid), "{what}");
                assert_eq!(t.hit(i.cx(), i.cy(), r.rtl, counts), Some((k, Part::Big(q))), "{what}: tile {k} icon {q}");
            }
            for m in 0..mini {
                let i = mini_icon(m, t.edge(), r.rtl).offset(tile.l, tile.t);
                assert!(near_holds(&area, &i), "{what}");
                assert_eq!(t.hit(i.cx(), i.cy(), r.rtl, counts), Some((k, Part::Rest)), "{what}: tile {k} mini {m}");
            }
            if t.name_h > 0.0 {
                assert_eq!(t.hit(name.cx(), name.cy(), r.rtl, counts), Some((k, Part::Rest)), "{what}: tile {k}'s name");
            }
        }
    }

    /// `b` inside `a`, to a hair (f32 noise: DIP to device pixels and back).
    fn near_holds(a: &Rect, b: &Rect) -> bool {
        const HAIR: f32 = 1e-3;
        b.l >= a.l - HAIR && b.t >= a.t - HAIR && b.r <= a.r + HAIR && b.b <= a.b + HAIR
    }

    /// Tiles `t` (`laid` on pixels at scale `s`, the work area at `origin`
    /// monitor DIP) as drawn: one edge of whole device pixels, under two
    /// pixels smaller than laid out; each tile on whole pixels, inside the
    /// one laid out (so in the grid's region), its corner pixels and its
    /// bottom edge's (the border drawn out to the square's edge) found by
    /// a click on it; the rest as tiles_in_the_grid.
    fn tiles_on_pixels(t: &Tiles, laid: &Tiles, r: &grid::Regions, s: f32, origin: (f32, f32), counts: &[usize], what: &str) {
        let what = format!("{what} at {s} from {origin:?}");
        let on_px = |v: f32| (v * s - (v * s).round()).abs() < 2e-3;
        let e = t.edge();
        assert!(on_px(e) && e > 0.0 && e <= laid.side + 1e-3 && e * s > laid.side * s - 2.0, "{what}: edge {e} for {}", laid.side);
        for k in 0..t.n {
            let d = t.tile(k, r.rtl);
            assert!((d.w() - e).abs() < 1e-3 && (d.h() - e).abs() < 1e-3, "{what}: tile {k} {d:?}");
            assert!(on_px(d.l + origin.0) && on_px(d.t + origin.1), "{what}: tile {k} {d:?} off the pixels");
            assert!(near_holds(&laid.tile(k, r.rtl), &d), "{what}: tile {k} {d:?} outside {:?}", laid.tile(k, r.rtl));
            let p = 0.5 / s;
            for (x, y) in [(d.l + p, d.t + p), (d.r - p, d.t + p), (d.l + p, d.b - p), (d.r - p, d.b - p), (d.cx(), d.b - p)] {
                assert_eq!(r.at(x, y), Some(grid::Area::Grid), "{what}: tile {k} at ({x}, {y})");
                assert_eq!(t.hit(x, y, r.rtl, counts).map(|h| h.0), Some(k), "{what}: tile {k} at ({x}, {y})");
            }
        }
        tiles_in_the_grid(t, r, counts, &what);
    }

    #[test]
    fn tiles_drawn_on_pixels_stay_in_the_grids_region() {
        // The fix-lp5 review's case: 640 x 304 DIP at 100%, 8 categories:
        // laid out 7 x 2, the last tile's bottom on the region's (y 234).
        // Drawn from (82, 183) 52 pixels across, its border went down to
        // y 234.37, where a click closed Launchpad.
        let counts = [6, 2, 1, 4, 3, 1, 1, 9];
        for rtl in [false, true] {
            let r = grid::regions(640.0, 304.0, rtl);
            assert_eq!(r.grid.b, 234.0);
            let laid = tiles(8, &grid::grid_in(&r, grid::LABEL_H), &r.grid);
            assert_eq!((laid.cols, laid.rows, laid.name_h), (7, 2, 0.0));
            assert!(laid.tile(7, rtl).b > 233.99, "{laid:?}");
            let t = laid.on_pixels(1.0, (0.0, 0.0));
            // The biggest whole-pixel square inside every tile: the first
            // column (82.42 .. 133.79) holds pixels 83 .. 133.
            assert_eq!(t.edge(), 50.0, "{t:?}");
            // The last tile on pixels 183 .. 233 down: its bottom border's
            // pixels a click on it, nothing drawn at the review's point.
            let last = t.tile(7, rtl);
            assert_eq!((last.t, last.b), (183.0, 233.0), "{last:?}");
            let x = if rtl { 532.0 } else { 108.0 };
            assert!(last.l < x && x < last.r, "{last:?}");
            assert_eq!(r.at(x, 232.5), Some(grid::Area::Grid));
            assert_eq!(t.hit(x, 232.5, rtl, &counts).map(|h| h.0), Some(7));
            assert!((0..8).all(|k| !t.tile(k, rtl).contains(x, 234.0) && !t.tile(k, rtl).contains(x, 233.5)));
            for s in [1.0, 1.25, 1.5, 1.75] {
                tiles_on_pixels(&laid.on_pixels(s, (0.0, 0.0)), &laid, &r, s, (0.0, 0.0), &counts, &format!("640x304 rtl={rtl}"));
            }
        }
        // Not on pixels: as laid out.
        let r = grid::regions(640.0, 304.0, false);
        let laid = tiles(8, &grid::grid_in(&r, grid::LABEL_H), &r.grid);
        assert_eq!((laid.edge(), laid.tile(7, false)), (laid.side, laid.on_pixels(f32::NAN, (0.0, 0.0)).tile(7, false)));
        // Every scale, a work area at the monitor's corner or not, small
        // screens and usual, one to twelve categories, both ways, every
        // name box.
        let counts = [12, 3, 5, 1, 2, 8, 4, 4, 7, 1, 2, 3];
        for s in [1.0f32, 1.25, 1.5, 1.75, 2.0] {
            for (px, py) in [(0.0, 0.0), (7.0, 3.0), (-1920.0, 41.0)] {
                let origin = (px / s, py / s);
                for (w, h) in [(320.0, 500.0), (640.0, 304.0), (640.0, 400.0), (640.0, 220.0), (800.0, 333.0), (1024.0, 560.0), (1366.0, 666.0), (1920.0, 1048.0)] {
                    // (A work area of whole device pixels.)
                    let (w, h) = ((w * s).round() / s, (h * s).round() / s);
                    for rtl in [false, true] {
                        let r = grid::regions(w, h, rtl);
                        for two in [grid::LABEL_H, 64.94] {
                            let g = grid::grid_in(&r, two);
                            for n in 1..=12 {
                                let laid = tiles(n, &g, &r.grid);
                                let what = format!("{w}x{h} rtl={rtl} {two} n={n}");
                                tiles_on_pixels(&laid.on_pixels(s, origin), &laid, &r, s, origin, &counts, &what);
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn the_library_stays_in_the_grids_region_on_a_small_screen() {
        // The review's case: 640 x 400 DIP, 8 categories, the fifth one
        // (the second row's first) with 3 apps. TILE_MIN tiles reached
        // y = 370 under a grid region ending at 330: its third app's icon
        // was drawn where a click closed Launchpad.
        let counts = [6, 2, 1, 4, 3, 1, 1, 9];
        for rtl in [false, true] {
            let r = grid::regions(640.0, 400.0, rtl);
            assert_eq!(r.grid.b, 330.0);
            let g = grid::grid_in(&r, grid::LABEL_H);
            assert_eq!((g.cols, g.rows), (3, 1));
            let t = tiles(8, &g, &r.grid);
            tiles_in_the_grid(&t, &r, &counts, &format!("640x400 rtl={rtl}"));
            assert!(t.side < TILE_MIN && t.name_h == NAME_H, "smaller, names kept: {t:?}");
            let tile = t.tile(4, rtl);
            let i = big_icon(2, t.side, rtl).offset(tile.l, tile.t);
            assert!(i.b <= 330.0);
            assert_eq!(t.hit(i.cx(), i.cy(), rtl, &counts), Some((4, Part::Big(2))));
            // The old tile's point is below the region: nothing drawn there.
            assert!((0..8).all(|k| !t.item(k, rtl).contains(100.0, 350.0)));
        }
        // Every small screen, one to twelve categories, both ways, every
        // name box: inside, and found where drawn.
        let counts = [12, 3, 5, 1, 2, 8, 4, 4, 7, 1, 2, 3];
        for w in [320.0, 400.0, 480.0, 640.0, 800.0, 1024.0, 1366.0, 1920.0] {
            for h in (150..=1200).step_by(23) {
                let h = h as f32;
                for rtl in [false, true] {
                    let r = grid::regions(w, h, rtl);
                    for two in [grid::LABEL_H, 64.94] {
                        let g = grid::grid_in(&r, two);
                        for n in 1..=12 {
                            let t = tiles(n, &g, &r.grid);
                            tiles_in_the_grid(&t, &r, &counts, &format!("{w}x{h} rtl={rtl} {two} n={n}"));
                        }
                    }
                }
            }
        }
        // The shortest layout: names hidden, the tiles as big as fit.
        let r = grid::regions(640.0, 220.0, false);
        let t = tiles(8, &grid::grid_in(&r, grid::LABEL_H), &r.grid);
        assert_eq!(t.name_h, 0.0, "{t:?}");
        assert!(t.side > 0.0 && t.side <= r.grid.h() + 0.01);
        // Shrinking: the tiles and their gaps alike.
        assert!((t.gap / t.side - TILE_GAP / TILE_MIN).abs() < 0.001, "{t:?}");
    }

    #[test]
    fn a_usual_screen_keeps_its_library_as_before() {
        // Where TILE_MIN tiles fit the grid's own box, the box it always
        // was: the same tiles, the same gaps, the names.
        for (w, h) in [(1920.0, 1048.0), (1366.0, 666.0), (2560.0, 1408.0), (1080.0, 1888.0)] {
            let r = grid::regions(w, h, false);
            let g = grid::grid_in(&r, grid::LABEL_H);
            for n in 1..=8 {
                let t = tiles(n, &g, &r.grid);
                assert!(t.side >= TILE_MIN && t.gap == TILE_GAP && t.name_h == NAME_H, "{w}x{h} {n}: {t:?}");
                let (aw, ah) = (g.cell_w * g.cols as f32, g.cell_h * g.rows as f32);
                assert!(t.left >= g.left - 0.01 && t.left + t.cols as f32 * (t.side + t.gap) - t.gap <= g.left + aw + 0.01, "{w}x{h} {n}");
                assert!(t.top >= g.top - 0.01 && t.top + t.rows as f32 * (t.side + t.name_h + t.gap) - t.gap <= g.top + ah + 0.01, "{w}x{h} {n}");
            }
        }
    }

    #[test]
    fn arrows_walk_the_tiles() {
        let g = grid::grid(1920.0, 1048.0, grid::LABEL_H);
        let t = tiles(7, &g, &grid::regions(1920.0, 1048.0, false).grid);
        assert_eq!((t.cols, t.rows), (4, 2));
        assert_eq!(step(0, &t, Arrow::Right, false), Some(1));
        assert_eq!(step(3, &t, Arrow::Right, false), Some(4), "on to the next row");
        assert_eq!(step(1, &t, Arrow::Down, false), Some(5));
        assert_eq!(step(3, &t, Arrow::Down, false), Some(6), "into the short row's last");
        assert_eq!(step(6, &t, Arrow::Right, false), None);
        assert_eq!(step(0, &t, Arrow::Left, false), None, "the edge: Home is the caller's business");
        assert_eq!(step(1, &t, Arrow::Left, true), Some(2), "mirrored");
    }
}
