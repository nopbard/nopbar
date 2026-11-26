//! High-resolution icon extraction through the shell (IShellItemImageFactory).

use windows::core::HSTRING;
use windows::Win32::Foundation::SIZE;
use windows::Win32::Graphics::Gdi::{
    DeleteObject, GetDC, GetDIBits, GetObjectW, ReleaseDC, BITMAP, BITMAPINFO, BITMAPINFOHEADER,
    BI_RGB, DIB_RGB_COLORS, HDC, HGDIOBJ,
};
use windows::Win32::UI::Shell::{
    IShellItemImageFactory, SHCreateItemFromParsingName, SIIGBF_BIGGERSIZEOK, SIIGBF_ICONONLY, SIIGBF_THUMBNAILONLY,
};

pub struct Icon {
    pub width: u32,
    pub height: u32,
    /// Premultiplied BGRA, top-down.
    pub pixels: Vec<u8>,
}

/// Largest icon / thumbnail side accepted from the shell (a picture chosen
/// as an icon can come back as a 2560 px thumbnail; anything far beyond is
/// a broken or hostile shell extension).
pub const MAX_ICON_SIDE: u32 = 4096;

/// Bytes of a `w`x`h` 32-bit image, or None if a side is not in
/// 1..=MAX_ICON_SIDE (sizes come from shell extensions and cache files).
pub fn icon_bytes(w: i64, h: i64) -> Option<usize> {
    let side = 1..=MAX_ICON_SIDE as i64;
    if !side.contains(&w) || !side.contains(&h) {
        return None;
    }
    (w as usize).checked_mul(h as usize)?.checked_mul(4)
}

/// The side the dock asks the shell for, whatever size it draws an icon at:
/// the largest frame icon files carry. Asked for 96 or 192 (sizes few
/// files have), the shell made the frame itself; asked for more than the
/// largest frame, it hands over that frame unscaled in the middle of a
/// framed canvas of the asked size (measured, see `looks_upscaled`). The
/// dock draws the source down (or up) to each size with a high-quality
/// filter (Gfx::surface_fit).
pub const SOURCE_PX: i32 = 256;

/// Smaller frames tried, largest first, when the shell had no real
/// SOURCE_PX one (`looks_upscaled`).
const FALLBACK_PX: [i32; 2] = [48, 32];

/// A `w`x`h` picture fitted into a `side` px square, aspect kept: its
/// drawn width and height (px). Square icons fill it exactly.
pub fn fit_box(w: u32, h: u32, side: u32) -> (f32, f32) {
    let (w, h, s) = (w.max(1) as f32, h.max(1) as f32, side as f32);
    let k = (s / w).min(s / h);
    ((w * k).min(s), (h * k).min(s))
}

/// Deletes a GDI object on every way out.
struct GdiObject(HGDIOBJ);
impl Drop for GdiObject {
    fn drop(&mut self) {
        unsafe {
            let _ = DeleteObject(self.0);
        }
    }
}

/// The screen DC, released on every way out.
struct ScreenDc(HDC);
impl Drop for ScreenDc {
    fn drop(&mut self) {
        unsafe {
            ReleaseDC(None, self.0);
        }
    }
}

pub fn load(path: &str, size: i32) -> Option<Icon> {
    if is_own_app_icon(path) {
        return own_app_icon(size.max(1) as u32);
    }
    // The dock's Launchpad pin has no file: its icon is drawn.
    if crate::launchpad::is_pin(path) {
        return crate::launchpad::glyph::pin_icon(path, size, Appearance::system(crate::syslook::current().light));
    }
    // A picture chosen as an app icon: its own pixels (WIC, premultiplied
    // by the decoder), not the icon of its file type.
    if is_picture(path) {
        return picture(path, size);
    }
    if let Some((file, index)) = resource_key(path) {
        return resource_icon(file, index, size);
    }
    image(path, size, SIIGBF_ICONONLY | SIIGBF_BIGGERSIZEOK)
}

/// Pure: the file and index of a "file,index" icon key (webapps.rs: a
/// window's icon that is not its file's first; a negative index is a
/// resource id). None for any other key (the file part must have an
/// extension, the index be a whole number other than 0).
pub fn icon_location(key: &str) -> Option<(&str, i32)> {
    let (file, index) = key.rsplit_once(',')?;
    let index: i32 = index.parse().ok()?;
    let named = Path::new(file).extension().is_some_and(|e| !e.is_empty()) && Path::new(file).is_absolute();
    (index != 0 && named).then_some((file, index))
}

/// icon_location for a key that is not itself a file (a file may have a
/// comma and digits at the end of its name).
fn resource_key(key: &str) -> Option<(&str, i32)> {
    icon_location(key).filter(|_| !Path::new(key).exists())
}

/// Icon `index` of `file` (negative: the one with that resource id), drawn
/// `px` a side (the shell picks the frame: SHDefExtractIcon). STA thread.
fn resource_icon(file: &str, index: i32, px: i32) -> Option<Icon> {
    use windows::Win32::UI::Shell::SHDefExtractIconW;
    use windows::Win32::UI::WindowsAndMessaging::{DestroyIcon, HICON};
    let side = px.clamp(16, SOURCE_PX) as u32;
    let mut large = HICON::default();
    unsafe {
        // (The large icon's size in the low word; no small one asked.)
        let r = SHDefExtractIconW(&HSTRING::from(file), index, 0, Some(&mut large), None, side);
        if r.is_err() || large.is_invalid() {
            return None;
        }
        let pixels = crate::tray::draw_icon(large, side);
        let _ = DestroyIcon(large);
        Some(Icon { width: side, height: side, pixels: pixels? })
    }
}

/// A picture file at most `size` px a side: decoded by WIC, else the
/// shell's thumbnail of it (straight alpha, measured).
fn picture(path: &str, size: i32) -> Option<Icon> {
    decode_image_fit(Path::new(path), size.clamp(1, MAX_ICON_SIDE as i32) as u32)
        .or_else(|| image(path, size, SIIGBF_THUMBNAILONLY | SIIGBF_BIGGERSIZEOK))
}

/// One GetImage at a time in this process. Two threads asking at once (an
/// .ico at 256 px on one, another at 48 on the other) made the 256 px one
/// come back as the small frame boxed, now and then: measured in this
/// crate's tests (the dock's icon thread runs beside Launchpad's, Search's
/// and stacks').
static SHELL_IMAGE: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn image(path: &str, size: i32, flags: windows::Win32::UI::Shell::SIIGBF) -> Option<Icon> {
    let _one = SHELL_IMAGE.lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        let factory: IShellItemImageFactory =
            SHCreateItemFromParsingName(&HSTRING::from(path), None).ok()?;
        let hbm = factory.GetImage(SIZE { cx: size, cy: size }, flags).ok()?;
        let _owned = GdiObject(HGDIOBJ(hbm.0));

        let mut bm = BITMAP::default();
        let got = GetObjectW(
            HGDIOBJ(hbm.0),
            std::mem::size_of::<BITMAP>() as i32,
            Some(&mut bm as *mut _ as *mut _),
        );
        if got == 0 {
            return None;
        }
        // The size the extension returned, not the one asked for: checked
        // before any buffer is sized from it (audit A12).
        let (w, h) = (bm.bmWidth, bm.bmHeight.abs());
        let Some(len) = icon_bytes(w as i64, h as i64) else {
            crate::log!("icons: {path}: refused a {}x{} image", bm.bmWidth, bm.bmHeight);
            return None;
        };

        let mut info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h, // top-down
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        // GetDIBits writes exactly h rows of w * 4 bytes (32 bpp rows need
        // no padding) into this buffer of that size.
        let mut pixels = vec![0u8; len];
        let hdc = ScreenDc(GetDC(None));
        if hdc.0.is_invalid() {
            return None;
        }
        let lines = GetDIBits(
            hdc.0,
            hbm,
            0,
            h as u32,
            Some(pixels.as_mut_ptr() as *mut _),
            &mut info,
            DIB_RGB_COLORS,
        );
        if lines <= 0 {
            return None;
        }

        // Some icons come back without alpha; treat those as opaque.
        if pixels.chunks_exact(4).all(|p| p[3] == 0) {
            for p in pixels.chunks_exact_mut(4) {
                p[3] = 255;
            }
        }
        // Everything that draws icons takes them as premultiplied; the
        // shell hands icons and picture thumbnails over straight.
        premultiply(&mut pixels);

        Some(Icon { width: w as u32, height: h as u32, pixels })
    }
}

/// Straight BGRA to premultiplied, in place (what every D2D bitmap the
/// dock draws is). Straight alpha drawn as premultiplied keeps an edge
/// pixel at alpha 64 at its full color: the antialiased rim becomes a hard,
/// bright stair-stepped one. Transparent pixels lose any color they carry.
pub fn premultiply(px: &mut [u8]) {
    for p in px.chunks_exact_mut(4) {
        let a = p[3] as u32;
        if a < 255 {
            for c in &mut p[..3] {
                *c = ((*c as u32 * a + 127) / 255) as u8;
            }
        }
    }
}

// ---- the dock's source pictures ---------------------------------------------

/// Where an icon's source pixels came from (logged once per icon).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    /// NopBar's embedded app icon.
    Bundled,
    /// The shell's SOURCE_PX frame.
    Shell,
    /// A smaller real frame: the shell had no SOURCE_PX one.
    Frame(i32),
    /// The largest logo file of a packaged app (its file name).
    StoreLogo(String),
    /// A picture chosen as the icon, as a thumbnail of this side.
    Picture(i32),
    /// Drawn by the dock (the Launchpad pin, the trash, folder stacks).
    Drawn,
}

// ---- the dock's own tiles ---------------------------------------------------------
//
// The Recycle Bin and folder stacks are drawn in NopBar's glyph language
// (crate::glyphs) on a neutral tile, instead of the shell's blue bin and
// yellow folder. Drawn on the icon thread, never cached (the trash's state,
// light / dark and high contrast all change them).

/// The glyph a stack of `folder` shows: a known folder's own (Downloads, ...)
/// from `known` (path, glyph), else a folder.
pub fn stack_glyph(folder: &str, known: &[(String, crate::glyphs::Glyph)]) -> crate::glyphs::Glyph {
    let norm = |p: &str| p.trim_end_matches(['\\', '/']).to_lowercase();
    let f = norm(folder);
    known.iter().find(|(p, _)| !p.is_empty() && norm(p) == f).map(|(_, g)| *g).unwrap_or(crate::glyphs::Glyph::Folder)
}

/// A folder the dock shows as a stack: a real directory, not a drive root
/// or a shell location. Touches the disk: icon thread only.
pub fn is_stack_folder(key: &str) -> bool {
    !key.starts_with("::") && !key.to_ascii_lowercase().starts_with("shell:") && Path::new(key).parent().is_some() && Path::new(key).is_dir()
}

/// The keys whose dock icon is drawn rather than read (and never cached).
/// Touches the disk: icon thread only.
pub fn drawn_key(key: &str) -> bool {
    key.eq_ignore_ascii_case(crate::dock::TRASH) || crate::launchpad::is_pin(key) || is_stack_folder(key)
}

pub const NOPBAR_ICON_KEY: &str = "shell:AppsFolder\\NopBar.NopBar";

/// The running executable or the installed Start entry, as an AUMID or
/// a path relative to the Programs known folder or absolute on disk.
pub fn is_own_app_icon(key: &str) -> bool {
    const START_ENTRY: &str = "{A77F5D77-2E2B-44C3-A6A2-ABA601054A51}\\NopBar.lnk";
    let entry = apps_folder(key).unwrap_or(key);
    if entry.eq_ignore_ascii_case(crate::updates::AUMID) || entry.eq_ignore_ascii_case(START_ENTRY) {
        return true;
    }
    let Some(name) = Path::new(entry).file_name() else { return false };
    let matches = |path: PathBuf| Path::new(entry) == path || entry.eq_ignore_ascii_case(&path.to_string_lossy());
    if name.eq_ignore_ascii_case("nopbar.exe") {
        std::env::current_exe().is_ok_and(matches)
    } else {
        name.eq_ignore_ascii_case("NopBar.lnk")
            && std::env::var_os("APPDATA").is_some_and(|root| matches(PathBuf::from(root).join(crate::release::SHORTCUT)))
    }
}

/// The Recycle Bin holds something (on any drive).
fn recycle_bin_full() -> bool {
    use windows::Win32::UI::Shell::{SHQueryRecycleBinW, SHQUERYRBINFO};
    let mut info = SHQUERYRBINFO { cbSize: std::mem::size_of::<SHQUERYRBINFO>() as u32, ..Default::default() };
    unsafe { SHQueryRecycleBinW(windows::core::PCWSTR::null(), &mut info) }.is_ok() && info.i64NumItems > 0
}

/// Known folders with a glyph of their own, looked up once (registry
/// only: KF_FLAG_DONT_VERIFY never touches the folders, which may sit on a
/// slow NAS) and again after `forget_known_folders`.
static KNOWN: std::sync::Mutex<Option<Vec<(String, crate::glyphs::Glyph)>>> = std::sync::Mutex::new(None);

fn known_folders() -> Vec<(String, crate::glyphs::Glyph)> {
    use crate::glyphs::Glyph;
    use windows::Win32::UI::Shell::{
        FOLDERID_Desktop, FOLDERID_Documents, FOLDERID_Downloads, FOLDERID_Music, FOLDERID_Pictures, FOLDERID_Videos, KF_FLAG_DONT_VERIFY, SHGetKnownFolderPath,
    };
    let mut known = KNOWN.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(k) = known.as_ref() {
        return k.clone();
    }
    let ids = [
        (FOLDERID_Downloads, Glyph::Download),
        (FOLDERID_Documents, Glyph::Document),
        (FOLDERID_Pictures, Glyph::Image),
        (FOLDERID_Music, Glyph::Music),
        (FOLDERID_Videos, Glyph::Play),
        (FOLDERID_Desktop, Glyph::Monitor),
    ];
    let k: Vec<(String, Glyph)> = ids
        .iter()
        .filter_map(|(id, g)| unsafe {
            let p = SHGetKnownFolderPath(id, KF_FLAG_DONT_VERIFY, None).ok()?;
            let s = p.to_string().ok();
            windows::Win32::System::Com::CoTaskMemFree(Some(p.0 as *const _));
            Some((s?, *g))
        })
        .collect();
    *known = Some(k.clone());
    k
}

/// A setting changed (a known folder may have moved): look them up again
/// next time.
pub fn forget_known_folders() {
    *KNOWN.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

/// What a drawn tile's colors depend on: the dock's light / dark theme and
/// the high-contrast palette (window, text). Sent with every icon request
/// and back with its reply, and kept with drawn pictures, so a tile drawn
/// for another look is never taken for the current one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Appearance {
    pub light: bool,
    pub hc: Option<([u8; 3], [u8; 3])>,
    /// The finish in force is Flat: its matte pictures (else Glass's).
    pub flat: bool,
}

/// The finish this process draws its own tiles in outside the dock
/// (Appearance::system: Search, Launchpad, the settings' pin list): the
/// dock's and the settings window's look set it as they resolve.
static FLAT_TILES: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// This process's finish is Flat (`flat`) or Glass (FLAT_TILES).
pub fn set_flat_tiles(flat: bool) {
    FLAT_TILES.store(flat, std::sync::atomic::Ordering::Relaxed);
}

impl Appearance {
    /// The system's high contrast on a `light` or dark surface (outside
    /// the dock, which passes its theme's), in this process's finish
    /// (set_flat_tiles).
    pub fn system(light: bool) -> Self {
        let hc = crate::syslook::current().high_contrast.map(|h| ([h.window.r, h.window.g, h.window.b], [h.text.r, h.text.g, h.text.b]));
        Appearance { light, hc, flat: FLAT_TILES.load(std::sync::atomic::Ordering::Relaxed) }
    }

    /// The tile colors for this look.
    pub fn tile(&self) -> crate::glyphs::TileLook {
        crate::glyphs::TileLook::new(self.light, self.hc).finish(self.flat)
    }
}

/// Whether an icon reply drawn for `got` is stale on a dock that looks
/// `now`: a drawn tile for any other look; a themed source (packaged apps'
/// logos) for the other light / dark. Stale replies are dropped and asked
/// for again.
pub fn reply_stale(got: Appearance, drawn: bool, themed: bool, now: Appearance) -> bool {
    if drawn {
        got != now
    } else {
        themed && got.light != now.light
    }
}

/// The glyph of the dock's drawn icon for `key`, if it has one: the
/// Launchpad pin, the trash (full or empty) or a folder stack. Touches the
/// disk: icon thread.
pub fn drawn_glyph(key: &str) -> Option<crate::glyphs::Glyph> {
    if crate::launchpad::is_pin(key) {
        Some(crate::glyphs::Glyph::Launchpad)
    } else if key.eq_ignore_ascii_case(crate::dock::TRASH) {
        Some(if recycle_bin_full() { crate::glyphs::Glyph::TrashFull } else { crate::glyphs::Glyph::Trash })
    } else if is_stack_folder(key) {
        Some(stack_glyph(key, &known_folders()))
    } else {
        None
    }
}

/// Which of `sides` (pictures the dock makes of an icon whose source is
/// `px`) a drawn tile is drawn at by itself: the smaller ones a tile of
/// another kind (glyphs::tile_kind), which the one at `px` shrunk would not
/// show. Ascending, each once.
pub fn own_sides(px: u32, sides: &[u32], look: Appearance) -> Vec<u32> {
    let t = look.tile();
    let big = crate::glyphs::tile_kind(px, &t);
    let mut v: Vec<u32> = sides.iter().copied().filter(|&s| s < px && crate::glyphs::tile_kind(s, &t) != big).collect();
    v.sort_unstable();
    v.dedup();
    v
}

/// The dock's drawn icon for `key` at `px` if it has one, with its
/// pictures at those of `sides` it is drawn at by itself (own_sides): a
/// dock resting at 32 px and magnified to 45 shows the 32 px tile without
/// its fine details, not the 45 px one shrunk. One glyph for all (the
/// trash's state read once). Icon thread.
pub fn drawn_pictures(key: &str, px: i32, sides: &[u32], look: Appearance) -> Option<(Icon, Vec<Icon>)> {
    let g = drawn_glyph(key)?;
    let px = px.clamp(8, 1024) as u32;
    let small = own_sides(px, sides, look).into_iter().map(|s| crate::glyphs::tile(s, g, look.tile())).collect();
    Some((crate::glyphs::tile(px, g, look.tile()), small))
}

/// The side a picture chosen as an icon is asked for (it can be any size;
/// the shell shrinks it well).
fn picture_px(target: i32) -> i32 {
    target.clamp(SOURCE_PX, MAX_ICON_SIDE as i32)
}

fn is_picture(path: &str) -> bool {
    let lower = path.to_lowercase();
    [".png", ".jpg", ".jpeg", ".bmp"].iter().any(|e| lower.ends_with(e))
}

/// The AUMID of a packaged app's key (`shell:AppsFolder\<AUMID>`).
fn apps_folder(key: &str) -> Option<&str> {
    const APPS: &str = "shell:AppsFolder\\";
    key.get(..APPS.len()).filter(|p| p.eq_ignore_ascii_case(APPS)).map(|_| &key[APPS.len()..])
}

/// `key`'s source depends on the dock's light or dark theme (packaged apps
/// ship a logo for each: `pick_logo`).
pub fn themed(key: &str) -> bool {
    // (Drawn tiles say so in their reply: Source::Drawn, Appearance.)
    apps_folder(key).is_some() && !is_own_app_icon(key)
}

/// The disk cache tag of `key`'s source: the side asked of the shell (so a
/// change of what is asked never reads an older result back), and the
/// theme it was chosen for when that matters (`themed`).
pub fn source_tag(key: &str, target: i32, light: bool) -> String {
    if is_picture(key) {
        format!("pic{}", picture_px(target))
    } else if plain_ico(key) {
        // Not src256: what older versions cached for an .ico (the shell's
        // boxed 48 px rendition, on 26200) is never read back.
        format!("ico{SOURCE_PX}")
    } else if themed(key) && light {
        format!("src{SOURCE_PX}-light")
    } else {
        format!("src{SOURCE_PX}")
    }
}

/// The best source pixels for the dock's icon of `key`, drawn at `target`
/// px at most (the Launchpad pin is drawn at that size; other sources are
/// whatever real frame is largest), for a dock that looks `look`. STA
/// thread with COM.
pub fn load_source(key: &str, target: i32, look: Appearance) -> Option<(Icon, Source)> {
    if is_own_app_icon(key) {
        return own_app_icon(target.max(1) as u32).map(|i| (i, Source::Bundled));
    }
    let light = look.light;
    if let Some((i, _)) = drawn_pictures(key, target, &[], look) {
        return Some((i, Source::Drawn));
    }
    if is_picture(key) {
        let px = picture_px(target);
        return picture(key, px).map(|i| (i, Source::Picture(px)));
    }
    if let Some(aumid) = apps_folder(key) {
        if let Some((i, name)) = store_logo(aumid, light) {
            return Some((i, Source::StoreLogo(name)));
        }
    }
    if let Some((file, index)) = resource_key(key) {
        return resource_icon(file, index, SOURCE_PX).map(|i| (i, Source::Shell));
    }
    // An .ico file with a SOURCE_PX frame: that frame, read from the file
    // (SHDefExtractIcon picks it as it is). The shell's GetImage cannot be
    // trusted with it: on Windows 11 26200 an .ico without a 16 px frame
    // (256 + 48 + 32, or a lone 256 one, as icon converters write) comes
    // back as its 48 px rendition boxed in the 256 canvas, its 256 frame
    // never used (measured: 256 + 16 is handed over whole; 26300 handed
    // over all of them). Exes and shortcuts were not affected (0 of 147
    // with a real 256 frame, icons::tests::survey_real_icons).
    if ico_frames(key).is_some_and(|sides| sides.contains(&(SOURCE_PX as u32))) {
        if let Some(i) = resource_icon(key, 0, SOURCE_PX) {
            return Some((i, Source::Shell));
        }
    }
    let flags = SIIGBF_ICONONLY | SIIGBF_BIGGERSIZEOK;
    let big = image(key, SOURCE_PX, flags)?;
    let was_boxed = boxed(&big);
    if !was_boxed && !blocky(&big) {
        return Some((big, Source::Shell));
    }
    // A real smaller frame (GetImage hands frames up to its largest over
    // 1:1).
    let mut first = None;
    for px in FALLBACK_PX {
        let Some(i) = image(key, px, flags) else { continue };
        if !looks_upscaled(&i) {
            return Some((i, Source::Frame(px)));
        }
        first.get_or_insert((i, Source::Frame(px)));
    }
    // Every one suspicious: the large one, unless the shell boxed it (the
    // box holds its 48 px rendition, pixel for pixel: measured).
    match first {
        Some(f) if was_boxed => Some(f),
        _ => Some((big, Source::Shell)),
    }
}

/// The frame sides an .ico file's directory lists (None: not an .ico, or
/// not readable as one). Only the directory is read.
fn ico_frames(key: &str) -> Option<Vec<u32>> {
    use std::io::Read;
    if !plain_ico(key) {
        return None;
    }
    let mut file = std::fs::File::open(key).ok()?;
    // The header first, then exactly the directory its count says (at most
    // 6 + 16 * 65535 bytes, about 1 MiB); never the frames.
    let mut head = vec![0u8; 6];
    file.read_exact(&mut head).ok()?;
    let len = ico_dir_len(&head)?;
    head.resize(len, 0);
    file.read_exact(&mut head[6..]).ok()?;
    ico_dir_sides(&head)
}

/// An .ico file's own key (not a "file,index" one): its icon is read from
/// the file (`ico_frames`), and cached under its own tag (`source_tag`).
/// An app's ID that happens to end in ".ico" is an app (its themed logo),
/// not a file.
fn plain_ico(key: &str) -> bool {
    key.to_ascii_lowercase().ends_with(".ico") && resource_key(key).is_none() && apps_folder(key).is_none()
}

/// Pure: the bytes of the ICONDIR that `head` (at least the file's first 6
/// bytes) starts: 6 + 16 per entry. None if it is not an icon directory or
/// lists no frame.
pub fn ico_dir_len(head: &[u8]) -> Option<usize> {
    if head.get(..4)? != [0, 0, 1, 0] {
        return None;
    }
    let n = u16::from_le_bytes(head.get(4..6)?.try_into().ok()?) as usize;
    (n > 0).then_some(6 + 16 * n)
}

/// Pure: the frame sides of an ICONDIR (`head`: the file's start, the
/// whole directory or more); a width byte of 0 is 256. None if it is not
/// an icon directory or is cut short.
pub fn ico_dir_sides(head: &[u8]) -> Option<Vec<u32>> {
    let dir = head.get(6..ico_dir_len(head)?)?;
    Some(dir.chunks_exact(16).map(|e| if e[0] == 0 { 256 } else { e[0] as u32 }).collect())
}

/// The shell did not hand over a real frame of this size. Measured with
/// GetImage(ICONONLY | BIGGERSIZEOK) on this PC (54 of 400 exes in Program
/// Files and System32, and test .ico files with only 16/32/48 frames):
/// asked for more than its largest frame, the shell returns that frame
/// unscaled in the middle of the asked-for canvas, with a faint frame
/// (alpha ~40-64) around the canvas's edge ("boxed"). It was never seen
/// stretching a frame up; a nearest-neighbor stretch (2x2 blocks of equal
/// pixels along the edges) is caught too. A smooth stretch cannot be told
/// from soft art (drop shadows, glass) by its pixels alone: not judged.
pub fn looks_upscaled(icon: &Icon) -> bool {
    boxed(icon) || blocky(icon)
}

/// A small frame in the middle of a framed canvas: the ring one pixel in
/// from the edge is (nearly) all faintly visible, the ring eight pixels in
/// (nearly) all transparent (the shell's frame was 3 px wide on a 128 px
/// canvas, 5 px on a 256 one; a real frame with a translucent background
/// is visible there), and what is clearly visible is at most 60 % of the
/// side across.
fn boxed(icon: &Icon) -> bool {
    let (w, h) = (icon.width as usize, icon.height as usize);
    if w < 64 || h < 64 || icon.pixels.len() != w * h * 4 {
        return false;
    }
    let a = |x: usize, y: usize| icon.pixels[(y * w + x) * 4 + 3];
    let ring = |i: usize| -> Vec<u8> { (i..w - i).flat_map(|x| [a(x, i), a(x, h - 1 - i)]).chain((i..h - i).flat_map(|y| [a(i, y), a(w - 1 - i, y)])).collect() };
    let (frame, gap) = (ring(1), ring(8));
    if frame.iter().filter(|&&v| v > 0).count() * 10 < frame.len() * 9 || gap.iter().filter(|&&v| v == 0).count() * 10 < gap.len() * 9 {
        return false;
    }
    let (mut x0, mut y0, mut x1, mut y1) = (usize::MAX, usize::MAX, 0, 0);
    for y in 0..h {
        for x in 0..w {
            if a(x, y) > 96 {
                (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
            }
        }
    }
    if x0 == usize::MAX {
        return false;
    }
    let side = (x1 - x0).max(y1 - y0) + 1;
    side * 10 <= w.min(h) * 6
}

/// How far (px) a pixel looks for the two sides of the edge it is on: a
/// nearest-neighbor stretch up to 4x keeps an edge pixel's block within it.
const RAMP_REACH: usize = 4;

/// Share of the edge pixels that equal their right, lower and lower-right
/// neighbors (the 2x2 blocks of a nearest-neighbor stretch): about 0 for
/// real art, 0.25 for a 2x stretch, more for larger ones. Edge pixels are
/// partly transparent ones on a real alpha transition: something clearly
/// more and something clearly less opaque within RAMP_REACH px. Flat
/// translucent areas (glass, a constant shadow) are not edges, however
/// alike their pixels. 0 (no evidence) with fewer than 64 edge pixels.
pub fn block_share(icon: &Icon) -> f32 {
    let (w, h) = (icon.width as usize, icon.height as usize);
    if w == 0 || h == 0 || icon.pixels.len() != w * h * 4 {
        return 0.0;
    }
    let a: Vec<u8> = icon.pixels.iter().skip(3).step_by(4).copied().collect();
    // Least and most alpha within RAMP_REACH (rows, then columns).
    let reach = |v: &[u8], rows: bool, pick: fn(u8, u8) -> u8| -> Vec<u8> {
        let (len, count) = if rows { (w, h) } else { (h, w) };
        let mut out = v.to_vec();
        for line in 0..count {
            let at = |i: usize| if rows { line * w + i } else { i * w + line };
            for i in 0..len {
                let (lo, hi) = (i.saturating_sub(RAMP_REACH), (i + RAMP_REACH).min(len - 1));
                out[at(i)] = (lo..=hi).map(|j| v[at(j)]).reduce(pick).unwrap_or(v[at(i)]);
            }
        }
        out
    };
    let lo = reach(&reach(&a, true, u8::min), false, u8::min);
    let hi = reach(&reach(&a, true, u8::max), false, u8::max);
    let px = |x: usize, y: usize| &icon.pixels[(y * w + x) * 4..(y * w + x) * 4 + 4];
    let (mut edges, mut blocks) = (0u32, 0u32);
    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            let v = a[i];
            if !(17..=239).contains(&v) || lo[i] as u32 + 32 >= v as u32 || hi[i] as u32 <= v as u32 + 32 {
                continue;
            }
            edges += 1;
            let p = px(x, y);
            if x + 1 < w && y + 1 < h && p == px(x + 1, y) && p == px(x, y + 1) && p == px(x + 1, y + 1) {
                blocks += 1;
            }
        }
    }
    if edges < 64 {
        0.0
    } else {
        blocks as f32 / edges as f32
    }
}

fn blocky(icon: &Icon) -> bool {
    block_share(icon) >= 0.12
}

// ---- packaged apps' logos ---------------------------------------------------

/// The `Square44x44Logo` (path relative to the package) and
/// `BackgroundColor` of application `app_id` in an AppxManifest.xml: the
/// `Package/Applications/Application` with that Id and its
/// `VisualElements` (any namespace prefix), read by an XML parser
/// (comments, entities, quotes and spacing as XML has them; no DTD, no
/// external entities).
pub fn manifest_logo(xml: &str, app_id: &str) -> Option<(String, Option<String>)> {
    use windows::Data::Xml::Dom::{XmlDocument, XmlLoadSettings};
    let settings = XmlLoadSettings::new().ok()?;
    settings.SetProhibitDtd(true).ok()?;
    settings.SetResolveExternals(false).ok()?;
    settings.SetValidateOnParse(false).ok()?;
    let doc = XmlDocument::new().ok()?;
    doc.LoadXmlWithSettings(&HSTRING::from(xml), &settings).ok()?;
    let root = doc.DocumentElement().ok()?;
    if local_name(&root.NodeName().ok()?) != "Package" {
        return None;
    }
    let attr = |e: &windows::Data::Xml::Dom::XmlElement, n: &str| e.GetAttribute(&HSTRING::from(n)).ok().map(|v| v.to_string()).filter(|v| !v.is_empty());
    for apps in child_elements(&root, "Applications") {
        for app in child_elements(&apps, "Application") {
            if !attr(&app, "Id").is_some_and(|id| id.eq_ignore_ascii_case(app_id)) {
                continue;
            }
            let ve = child_elements(&app, "VisualElements").into_iter().next()?;
            return Some((attr(&ve, "Square44x44Logo")?, attr(&ve, "BackgroundColor")));
        }
    }
    None
}

/// `uap:VisualElements` -> `VisualElements`.
fn local_name(n: &HSTRING) -> String {
    let n = n.to_string();
    n.rsplit(':').next().unwrap_or_default().to_string()
}

/// The child elements of `e` named `name` (local name).
fn child_elements(e: &windows::Data::Xml::Dom::XmlElement, name: &str) -> Vec<windows::Data::Xml::Dom::XmlElement> {
    use windows::core::Interface;
    use windows::Data::Xml::Dom::{NodeType, XmlElement};
    let Ok(list) = e.ChildNodes() else { return Vec::new() };
    let n = list.Length().unwrap_or(0);
    (0..n)
        .filter_map(|i| list.Item(i).ok())
        .filter(|c| c.NodeType().ok() == Some(NodeType::ElementNode))
        .filter(|c| c.NodeName().is_ok_and(|v| local_name(&v) == name))
        .filter_map(|c| c.cast::<XmlElement>().ok())
        .collect()
}

/// Which form of a logo a file is (its `altform-` qualifier).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LogoForm {
    /// For a tile of the manifest's BackgroundColor (no altform).
    Plated,
    /// Without a tile, for a dark taskbar (`altform-unplated`).
    Unplated,
    /// Without a tile, for a light taskbar (`altform-lightunplated`).
    LightUnplated,
}

/// Of the files in a logo's folder (`names`), the largest real one for logo
/// file stem `stem` (`AppList` of `Assets\AppList.png`) on a `light` or
/// dark dock: (name, px, plated). The unplated form made for the dock's
/// theme first, else the plated one (it brings its own tile). On a light
/// dock a package with an unplated form only for dark gives None (the
/// shell chooses). Contrast and theme- variants are left out; a
/// `targetsize-N` file is N px, a `scale-N` one 44 x N / 100.
pub fn pick_logo(stem: &str, names: &[String], light: bool) -> Option<(String, u32, bool)> {
    let stem = stem.to_lowercase();
    let mut files: Vec<(String, u32, LogoForm)> = Vec::new();
    for n in names {
        let lower = n.to_lowercase();
        let Some(q) = lower.strip_prefix(&stem).and_then(|r| r.strip_suffix(".png")) else { continue };
        let q = if q.is_empty() { Some("") } else { q.strip_prefix('.') };
        let Some(q) = q else { continue };
        let (mut px, mut form, mut skip) = (44u32, LogoForm::Plated, false);
        for part in q.split('_').filter(|p| !p.is_empty()) {
            if let Some(v) = part.strip_prefix("targetsize-") {
                px = v.parse().unwrap_or(0);
            } else if let Some(v) = part.strip_prefix("scale-") {
                px = v.parse::<u32>().map(|s| (44 * s + 50) / 100).unwrap_or(0);
            } else if part == "altform-unplated" {
                form = LogoForm::Unplated;
            } else if part == "altform-lightunplated" {
                form = LogoForm::LightUnplated;
            } else if part.starts_with("contrast-") || part.starts_with("theme-") || part.starts_with("altform-") {
                skip = true;
            }
        }
        if !skip && px > 0 && px <= MAX_ICON_SIDE {
            files.push((n.clone(), px, form));
        }
    }
    let has = |f: LogoForm| files.iter().any(|x| x.2 == f);
    let want = match (light, has(LogoForm::LightUnplated), has(LogoForm::Unplated)) {
        (true, true, _) => LogoForm::LightUnplated,
        (true, false, true) => return None,
        (false, _, true) => LogoForm::Unplated,
        _ => LogoForm::Plated,
    };
    files.into_iter().filter(|x| x.2 == want).max_by_key(|x| x.1).map(|(n, px, f)| (n, px, f == LogoForm::Plated))
}

/// A path from a manifest that stays inside its package: relative, made
/// of plain names only (no drive or UNC prefix, no root, no "..").
pub fn package_relative(rel: &Path) -> bool {
    use std::path::Component;
    rel.components().next().is_some() && rel.components().all(|c| matches!(c, Component::Normal(_) | Component::CurDir))
}

/// `path` is inside `root`, once both are resolved (links followed).
pub fn inside(root: &Path, path: &Path) -> bool {
    match (std::fs::canonicalize(root), std::fs::canonicalize(path)) {
        (Ok(r), Ok(p)) => p.starts_with(&r) && p != r,
        _ => false,
    }
}

/// `#RRGGBB` (or `#AARRGGBB`) as (r, g, b); None for "transparent", system
/// color names and anything else.
pub fn parse_hex_color(s: &str) -> Option<(u8, u8, u8)> {
    let h = s.trim().strip_prefix('#')?;
    let h = match h.len() {
        6 => h,
        8 if h[..2].eq_ignore_ascii_case("ff") => &h[2..],
        _ => return None,
    };
    let v = u32::from_str_radix(h, 16).ok()?;
    Some(((v >> 16) as u8, (v >> 8) as u8, v as u8))
}

/// Premultiplied pixels over a solid color (the tile a plated logo is
/// drawn on).
fn over_color(px: &mut [u8], (r, g, b): (u8, u8, u8)) {
    for p in px.chunks_exact_mut(4) {
        let k = 255 - p[3] as u32;
        for (c, bg) in p[..3].iter_mut().zip([b, g, r]) {
            *c = (*c as u32 + (bg as u32 * k + 127) / 255).min(255) as u8;
        }
        p[3] = 255;
    }
}

/// Logo files smaller than this are not used (the shell is asked instead).
const STORE_LOGO_MIN: u32 = 128;

/// The install folder of a package of `family` (the first one listed).
fn package_dir(family: &str) -> Option<std::path::PathBuf> {
    use windows::core::PWSTR;
    use windows::Win32::Foundation::{ERROR_INSUFFICIENT_BUFFER, ERROR_SUCCESS};
    use windows::Win32::Storage::Packaging::Appx::{GetPackagePathByFullName, GetPackagesByPackageFamily};
    let f = HSTRING::from(family);
    unsafe {
        let (mut count, mut len) = (0u32, 0u32);
        let r = GetPackagesByPackageFamily(&f, &mut count, None, &mut len, None);
        if r != ERROR_INSUFFICIENT_BUFFER || count == 0 || count > 64 || len == 0 || len > 1 << 16 {
            return None;
        }
        let mut names = vec![PWSTR::null(); count as usize];
        let mut buf = vec![0u16; len as usize];
        if GetPackagesByPackageFamily(&f, &mut count, Some(names.as_mut_ptr()), &mut len, Some(PWSTR(buf.as_mut_ptr()))) != ERROR_SUCCESS {
            return None;
        }
        let full = names.first().filter(|p| !p.is_null())?.to_string().ok()?;
        let mut plen = 0u32;
        let full = HSTRING::from(full);
        if GetPackagePathByFullName(&full, &mut plen, None) != ERROR_INSUFFICIENT_BUFFER || plen == 0 || plen > 4096 {
            return None;
        }
        let mut path = vec![0u16; plen as usize];
        if GetPackagePathByFullName(&full, &mut plen, Some(PWSTR(path.as_mut_ptr()))) != ERROR_SUCCESS {
            return None;
        }
        let end = path.iter().position(|&c| c == 0).unwrap_or(path.len());
        Some(std::path::PathBuf::from(String::from_utf16_lossy(&path[..end])))
    }
}

/// A packaged app's largest Square44x44Logo file for a `light` or dark
/// dock, decoded (on its tile color when it is a plated one with a solid
/// color): what the taskbar shows, at the largest size the package has,
/// instead of the shell's rendition at one size. None: not found (the
/// shell is asked instead).
fn store_logo(aumid: &str, light: bool) -> Option<(Icon, String)> {
    let (family, app) = aumid.split_once('!')?;
    let dir = package_dir(family)?;
    std::fs::metadata(dir.join("AppxManifest.xml")).ok().filter(|m| m.len() < 4 << 20)?;
    let xml = std::fs::read_to_string(dir.join("AppxManifest.xml")).ok()?;
    let (logo, bg) = manifest_logo(&xml, app)?;
    let rel = Path::new(&logo);
    if !package_relative(rel) {
        return None;
    }
    let folder = dir.join(rel.parent()?);
    let stem = rel.file_stem()?.to_str()?;
    let names: Vec<String> = std::fs::read_dir(&folder).ok()?.flatten().filter_map(|e| e.file_name().into_string().ok()).collect();
    let (name, px, plated) = pick_logo(stem, &names, light)?;
    // A small logo only (WinDbg's is 44 px): the shell may draw a larger
    // tile logo instead.
    if px < STORE_LOGO_MIN {
        return None;
    }
    let file = folder.join(&name);
    if !inside(&dir, &file) {
        return None;
    }
    let mut icon = decode_image(&file)?;
    if plated {
        if let Some(c) = bg.as_deref().and_then(parse_hex_color) {
            over_color(&mut icon.pixels, c);
        }
    }
    Some((icon, name))
}

/// An image file at its own size, premultiplied BGRA (WIC; needs COM).
pub fn decode_image(path: &Path) -> Option<Icon> {
    decode_image_fit(path, MAX_ICON_SIDE)
}

/// An image file shrunk (aspect kept) to at most `max_side` px a side,
/// premultiplied BGRA by WIC's converter (WIC; needs COM).
pub fn decode_image_fit(path: &Path, max_side: u32) -> Option<Icon> {
    use windows::Win32::Foundation::GENERIC_READ;
    use windows::Win32::Graphics::Imaging::{CLSID_WICImagingFactory, IWICImagingFactory, WICDecodeMetadataCacheOnDemand};
    use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};
    unsafe {
        let factory: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).ok()?;
        let decoder = factory.CreateDecoderFromFilename(&HSTRING::from(path.as_os_str()), None, GENERIC_READ, WICDecodeMetadataCacheOnDemand).ok()?;
        decode_first_frame(&factory, &decoder, max_side)
    }
}

/// An image in memory (an embedded PNG), as decode_image_fit (WIC; needs
/// COM).
pub fn decode_bytes_fit(bytes: &[u8], max_side: u32) -> Option<Icon> {
    with_image_decoder(bytes, |factory, decoder| unsafe { decode_first_frame(factory, decoder, max_side) })
}

fn with_image_decoder<T>(
    bytes: &[u8],
    read: impl FnOnce(&windows::Win32::Graphics::Imaging::IWICImagingFactory, &windows::Win32::Graphics::Imaging::IWICBitmapDecoder) -> Option<T>,
) -> Option<T> {
    use windows::Win32::Graphics::Imaging::{CLSID_WICImagingFactory, IWICImagingFactory, WICDecodeMetadataCacheOnLoad};
    use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};
    unsafe {
        let factory: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).ok()?;
        let stream = factory.CreateStream().ok()?;
        stream.InitializeFromMemory(bytes).ok()?;
        // Every pixel is copied out before this returns: `bytes` need not
        // outlive the call.
        let decoder = factory.CreateDecoderFromStream(&stream, std::ptr::null(), WICDecodeMetadataCacheOnLoad).ok()?;
        read(&factory, &decoder)
    }
}

/// NopBar's bundled icon fitted to an in-app slot (WIC; needs COM).
pub fn own_app_icon(side: u32) -> Option<Icon> {
    own_app_pictures(side, &[]).map(|(icon, _)| icon)
}

/// The peak and smaller dock pictures, each read from the closest frame
/// that can cover its size. Keep the artwork's transparent margin and
/// centered placement at every scale.
pub fn own_app_pictures(side: u32, smaller: &[u32]) -> Option<(Icon, Vec<Icon>)> {
    use windows::core::Interface;
    use windows::Win32::Graphics::Imaging::{IWICBitmapSource, WICBitmapInterpolationModeFant, WICBitmapInterpolationModeHighQualityCubic};

    const ICO: &[u8] = include_bytes!("../res/nopbar.ico");
    with_image_decoder(ICO, |factory, decoder| unsafe {
        let mut frames = (0..decoder.GetFrameCount().ok()?).filter_map(|index| {
            let frame = decoder.GetFrame(index).ok()?;
            let (mut w, mut h) = (0, 0);
            frame.GetSize(&mut w, &mut h).ok()?;
            (w == h && w > 0).then_some((w, frame))
        }).collect::<Vec<_>>();
        frames.sort_unstable_by_key(|(w, _)| *w);

        let picture = |requested: u32| {
            let target = requested.clamp(1, 1024);
            let (w, frame) = frames.iter().find(|(w, _)| *w >= target).or_else(|| frames.last())?;
            let input: IWICBitmapSource = premultiplied_frame(factory, frame)?.cast().ok()?;
            let input = if *w != target {
                let scaler = factory.CreateBitmapScaler().ok()?;
                let filter = if *w > target { WICBitmapInterpolationModeFant } else { WICBitmapInterpolationModeHighQualityCubic };
                scaler.Initialize(&input, target, target, filter).ok()?;
                scaler.cast().ok()?
            } else {
                input
            };
            bitmap_pixels(&input, target, target)
        };
        let icon = picture(side)?;
        let small = smaller.iter().copied().filter(|&s| s < side).filter_map(picture).collect();
        Some((icon, small))
    })
}

unsafe fn premultiplied_frame(
    factory: &windows::Win32::Graphics::Imaging::IWICImagingFactory,
    frame: &windows::Win32::Graphics::Imaging::IWICBitmapFrameDecode,
) -> Option<windows::Win32::Graphics::Imaging::IWICFormatConverter> {
    use windows::Win32::Graphics::Imaging::{GUID_WICPixelFormat32bppPBGRA, WICBitmapDitherTypeNone, WICBitmapPaletteTypeCustom};
    let conv = factory.CreateFormatConverter().ok()?;
    conv.Initialize(frame, &GUID_WICPixelFormat32bppPBGRA, WICBitmapDitherTypeNone, None, 0.0, WICBitmapPaletteTypeCustom).ok()?;
    Some(conv)
}

/// The first frame, converted to premultiplied BGRA, then shrunk to
/// `max_side`.
unsafe fn decode_first_frame(
    factory: &windows::Win32::Graphics::Imaging::IWICImagingFactory,
    decoder: &windows::Win32::Graphics::Imaging::IWICBitmapDecoder,
    max_side: u32,
) -> Option<Icon> {
    use windows::core::Interface;
    use windows::Win32::Graphics::Imaging::{IWICBitmapSource, WICBitmapInterpolationModeFant};
    let frame = decoder.GetFrame(0).ok()?;
    let (mut w, mut h) = (0u32, 0u32);
    frame.GetSize(&mut w, &mut h).ok()?;
    if w == 0 || h == 0 {
        return None;
    }
    let conv = premultiplied_frame(factory, &frame)?;
    let max = max_side.clamp(1, MAX_ICON_SIDE);
    let (src, w, h): (IWICBitmapSource, u32, u32) = if w.max(h) > max {
        let (fw, fh) = fit_box(w, h, max);
        let (nw, nh) = ((fw.round() as u32).max(1), (fh.round() as u32).max(1));
        // Scaled after the conversion: filtered premultiplied, no dark
        // fringe from transparent pixels' colors.
        let scaler = factory.CreateBitmapScaler().ok()?;
        scaler.Initialize(&conv, nw, nh, WICBitmapInterpolationModeFant).ok()?;
        (scaler.cast().ok()?, nw, nh)
    } else {
        (conv.cast().ok()?, w, h)
    };
    bitmap_pixels(&src, w, h)
}

unsafe fn bitmap_pixels(src: &windows::Win32::Graphics::Imaging::IWICBitmapSource, w: u32, h: u32) -> Option<Icon> {
    let len = icon_bytes(w as i64, h as i64)?;
    let mut pixels = vec![0u8; len];
    src.CopyPixels(std::ptr::null(), w * 4, &mut pixels).ok()?;
    Some(Icon { width: w, height: h, pixels })
}

/// Logs where each icon's source came from, once per key and run.
pub fn log_source(key: &str, icon: &Icon, src: &str) {
    static SEEN: std::sync::Mutex<Option<std::collections::HashSet<String>>> = std::sync::Mutex::new(None);
    let Ok(mut seen) = SEEN.lock() else { return };
    if seen.get_or_insert_with(Default::default).insert(key.to_lowercase()) {
        crate::log!("icons: {key}: source {}x{} ({src})", icon.width, icon.height);
    }
}
// ---- analysis ---------------------------------------------------------------

/// What the dock needs to know to dress an icon.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct IconInfo {
    /// Share of pixels that are (nearly) opaque: ~1.0 for full-bleed square
    /// icons, much less for shaped ones.
    pub coverage: f32,
    /// Average un-premultiplied color of the visible pixels.
    pub avg: (u8, u8, u8),
}

pub fn analyze(icon: &Icon) -> IconInfo {
    let (mut opaque, mut n, mut r, mut g, mut b) = (0u64, 0u64, 0u64, 0u64, 0u64);
    for px in icon.pixels.chunks_exact(4) {
        let a = px[3] as u64;
        if a >= 200 {
            opaque += 1;
        }
        if a >= 128 {
            // premultiplied BGRA -> straight RGB
            b += px[0] as u64 * 255 / a;
            g += px[1] as u64 * 255 / a;
            r += px[2] as u64 * 255 / a;
            n += 1;
        }
    }
    let total = (icon.pixels.len() / 4).max(1) as f32;
    let avg = if n == 0 { (128, 128, 128) } else { ((r / n) as u8, (g / n) as u8, (b / n) as u8) };
    IconInfo { coverage: opaque as f32 / total, avg }
}

/// Backplate color for an icon: its average color with most of the
/// saturation taken out, settled at a quiet mid-dark tone, so a yellow
/// folder gets a warm grey plate instead of a muddy olive one.
/// Backplate color for an icon: its average color, mostly desaturated,
/// pushed to a dark mid-tone on a dark dock and a light one on a light dock.
pub fn plate_color(info: IconInfo, light: bool) -> (u8, u8, u8) {
    let (r, g, b) = (info.avg.0 as f32, info.avg.1 as f32, info.avg.2 as f32);
    let lum = 0.299 * r + 0.587 * g + 0.114 * b;
    let f = |c: f32| {
        let desat = c * 0.35 + lum * 0.65;
        let v = if light { desat * 0.22 + 196.0 } else { desat * 0.38 + 34.0 };
        v.round().clamp(0.0, 255.0) as u8
    };
    (f(r), f(g), f(b))
}

// ---- disk cache -------------------------------------------------------------
//
// %LOCALAPPDATA%\NopBar\icons\{fnv64(key + mtime)}-{tag}-pm.bgra
// File: u32 width, u32 height (LE), then width*height*4 bytes of
// premultiplied BGRA. The source file's modification time is part of the
// key, so an app update that changes its icon is picked up automatically.
// `tag` says what was asked of the shell (source_tag for the dock, the
// size for Search and Launchpad). Files of the older format (no "-pm":
// straight alpha, asked at the dock's peak size) are never read and are
// deleted by the next sweep.

use std::path::{Path, PathBuf};

pub fn fnv64(data: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in data {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

fn cache_dir() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA").map(|d| PathBuf::from(d).join("NopBar").join("icons"))
}

/// The end of every current cache file's name.
const CACHE_SUFFIX: &str = "-pm.bgra";

/// Where the icon for `key` asked as `tag` is cached (None if no LOCALAPPDATA).
pub fn cache_path(key: &str, tag: &str) -> Option<PathBuf> {
    // A "file,index" key: its file's time (the index is in the key).
    let file = resource_key(key).map_or(key, |(f, _)| f);
    let mtime = std::fs::metadata(file)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let id = fnv64(format!("{}|{mtime}", key.to_lowercase()).as_bytes());
    cache_dir().map(|d| d.join(format!("{id:016x}-{tag}{CACHE_SUFFIX}")))
}

/// How long a cached icon is trusted when the key has no file of its own to
/// compare against (shortcuts point elsewhere; shell:AppsFolder has no file).
const NON_EXE_MAX_AGE: std::time::Duration = std::time::Duration::from_secs(7 * 24 * 3600);

/// Plain .exe keys are versioned by their mtime in the cache name. For
/// .lnk and AUMID keys the real image source can change underneath, so
/// those entries are re-extracted once they are a week old.
pub fn cache_fresh(key: &str, path: &Path) -> bool {
    // (A file's own icons: versioned by the file's time, as an exe's.)
    if key.to_lowercase().ends_with(".exe") || resource_key(key).is_some() {
        return true;
    }
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .is_some_and(|age| age < NON_EXE_MAX_AGE)
}

pub fn read_cache(path: &Path) -> Option<Icon> {
    // Never read more than the largest icon could be (a damaged or foreign
    // file must not be pulled into memory whole).
    let max = 8 + icon_bytes(MAX_ICON_SIDE as i64, MAX_ICON_SIDE as i64)?;
    if std::fs::metadata(path).ok()?.len() > max as u64 {
        return None;
    }
    let data = std::fs::read(path).ok()?;
    if data.len() < 8 {
        return None;
    }
    let w = u32::from_le_bytes(data[0..4].try_into().ok()?);
    let h = u32::from_le_bytes(data[4..8].try_into().ok()?);
    let len = icon_bytes(w as i64, h as i64)?;
    if data.len() != 8 + len {
        return None;
    }
    Some(Icon { width: w, height: h, pixels: data[8..].to_vec() })
}

/// Cached icons not used (read or written) for this long are deleted.
const CACHE_MAX_AGE: std::time::Duration = std::time::Duration::from_secs(30 * 24 * 3600);
/// The icon cache folder is kept under this size (oldest files go first).
const CACHE_MAX_BYTES: u64 = 64 << 20;

/// Which cache files to delete (audit A16: an app updated or a one-off exe
/// left its old icon files behind for good). `files`: (name, size, age).
/// Too old, or the oldest beyond the size budget; leftover temp files and
/// files of an older format too.
pub fn cache_sweep(mut files: Vec<(String, u64, std::time::Duration)>, max_age: std::time::Duration, max_bytes: u64) -> Vec<String> {
    let mut gone = Vec::new();
    files.retain(|(name, _, age)| {
        let old_format = name.ends_with(".bgra") && !name.ends_with(CACHE_SUFFIX);
        let drop = *age > max_age || name.ends_with(".tmp") || old_format;
        if drop {
            gone.push(name.clone());
        }
        !drop
    });
    // Newest first; whatever does not fit the budget goes.
    files.sort_by_key(|(_, _, age)| *age);
    let mut total = 0u64;
    for (name, size, _) in files {
        total = total.saturating_add(size);
        if total > max_bytes {
            gone.push(name);
        }
    }
    gone
}

/// Deletes stale and excess files from the icon cache (worker thread, once
/// per run). Reading a cached icon does not change its time, so "age" is
/// since it was written: a live icon is simply written again when deleted.
pub fn sweep_cache() {
    let Some(dir) = cache_dir() else { return };
    let Ok(rd) = std::fs::read_dir(&dir) else { return };
    let now = std::time::SystemTime::now();
    let files: Vec<(String, u64, std::time::Duration)> = rd
        .flatten()
        .filter_map(|e| {
            let md = e.metadata().ok().filter(|m| m.is_file())?;
            let age = md.modified().ok().and_then(|t| now.duration_since(t).ok()).unwrap_or_default();
            Some((e.file_name().to_string_lossy().into_owned(), md.len(), age))
        })
        .filter(|(n, _, _)| n.ends_with(".bgra") || n.ends_with(".tmp"))
        .collect();
    let gone = cache_sweep(files, CACHE_MAX_AGE, CACHE_MAX_BYTES);
    for name in &gone {
        let _ = std::fs::remove_file(dir.join(name));
    }
    if !gone.is_empty() {
        crate::log!("icons: removed {} old cache file(s)", gone.len());
    }
}

pub fn write_cache(path: &Path, icon: &Icon) {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let mut data = Vec::with_capacity(8 + icon.pixels.len());
    data.extend_from_slice(&icon.width.to_le_bytes());
    data.extend_from_slice(&icon.height.to_le_bytes());
    data.extend_from_slice(&icon.pixels);
    let tmp = path.with_extension("tmp");
    if std::fs::write(&tmp, data).is_ok() {
        let _ = std::fs::rename(&tmp, path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nopbar_resolves_to_the_bundled_art_at_each_size() {
        crate::gfx::promo_kit::keep_com();
        let key = NOPBAR_ICON_KEY;
        assert!(is_own_app_icon(key));
        assert!(is_own_app_icon(r"shell:AppsFolder\{A77F5D77-2E2B-44C3-A6A2-ABA601054A51}\NopBar.lnk"));
        assert!(!is_own_app_icon(r"C:\elsewhere\nopbar.exe"));
        assert!(!is_own_app_icon(r"shell:AppsFolder\Other.NopBar"));
        assert!(drawn_glyph(key).is_none());

        for px in [16, 20, 24, 32, 40, 44, 48, 64, 66, 88, 128, 256] {
            let (icon, source) = load_source(key, px, Appearance::default()).unwrap();
            assert_eq!(source, Source::Bundled);
            assert_eq!((icon.width, icon.height), (px as u32, px as u32));
            assert_eq!(icon.pixels, load(key, px).unwrap().pixels);
            assert_eq!(icon.pixels[3], 0, "transparent corner at {px} px");
            let pixels = icon.pixels.as_chunks::<4>().0;
            assert!(pixels.iter().any(|p| p[2].saturating_sub(p[1]) > 40 && p[3] == 255), "red hat at {px} px");
            let width = pixels.chunks_exact(px as usize)
                .map(|row| row.iter().filter(|p| p[3] >= 200).count())
                .max().unwrap();
            assert!(width >= px as usize * 80 / 100, "artwork takes {width} of {px} px");
        }
    }

    #[test]
    fn the_cache_sweep_drops_old_and_excess_files() {
        let d = |days: u64| std::time::Duration::from_secs(days * 24 * 3600);
        let files = vec![
            ("new-pm.bgra".to_string(), 40, d(0)),
            ("mid-pm.bgra".to_string(), 40, d(2)),
            ("old-pm.bgra".to_string(), 40, d(5)),
            ("ancient-pm.bgra".to_string(), 1, d(40)),
            ("x.tmp".to_string(), 1, d(0)),
            // The older format (straight alpha, sized names): gone at once.
            ("0123456789abcdef-128.bgra".to_string(), 1, d(0)),
        ];
        let mut gone = cache_sweep(files, d(30), 100);
        gone.sort();
        assert_eq!(gone, ["0123456789abcdef-128.bgra", "ancient-pm.bgra", "old-pm.bgra", "x.tmp"]);
        assert!(cache_sweep(vec![("a-pm.bgra".into(), 5, d(1))], d(30), 100).is_empty());
    }

    fn tmp(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("nopbar-test-{}-{name}", std::process::id()))
    }

    #[test]
    fn cache_roundtrip() {
        let p = tmp("rt.bgra");
        let icon = Icon { width: 4, height: 2, pixels: (0..32).collect() };
        write_cache(&p, &icon);
        let back = read_cache(&p).unwrap();
        assert_eq!((back.width, back.height), (4, 2));
        assert_eq!(back.pixels, icon.pixels);
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn cache_rejects_truncated() {
        let p = tmp("trunc.bgra");
        let icon = Icon { width: 4, height: 2, pixels: (0..32).collect() };
        write_cache(&p, &icon);
        let data = std::fs::read(&p).unwrap();
        std::fs::write(&p, &data[..data.len() - 1]).unwrap();
        assert!(read_cache(&p).is_none());
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn exe_cache_always_fresh_lnk_needs_recent_file() {
        let p = tmp("fresh.bgra");
        let _ = std::fs::remove_file(&p);
        assert!(cache_fresh(r"C:\Apps\a.exe", &p));
        assert!(!cache_fresh(r"C:\Links\a.lnk", &p)); // no file yet
        std::fs::write(&p, b"x").unwrap();
        assert!(cache_fresh(r"C:\Links\a.lnk", &p));
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn analyze_full_square_vs_shape() {
        // 4x4 fully opaque red -> coverage 1, avg red.
        let full = Icon { width: 4, height: 4, pixels: [0u8, 0, 255, 255].repeat(16) };
        let i = analyze(&full);
        assert_eq!(i.coverage, 1.0);
        assert_eq!(i.avg, (255, 0, 0));
        // Same but only 4 of 16 pixels visible -> coverage 0.25.
        let mut px = vec![0u8; 64];
        for k in 0..4 {
            px[k * 4..k * 4 + 4].copy_from_slice(&[255, 0, 0, 255]);
        }
        let shaped = Icon { width: 4, height: 4, pixels: px };
        let i = analyze(&shaped);
        assert_eq!(i.coverage, 0.25);
        assert_eq!(i.avg, (0, 0, 255));
    }

    #[test]
    fn analyze_unpremultiplies() {
        // 50% alpha white stored premultiplied as 128.
        let icon = Icon { width: 1, height: 1, pixels: vec![128, 128, 128, 128] };
        assert_eq!(analyze(&icon).avg, (255, 255, 255));
    }

    #[test]
    fn plate_is_muted_mid_dark() {
        let p = plate_color(IconInfo { coverage: 0.5, avg: (240, 200, 40) }, false); // yellow folder
        let spread = p.0.max(p.1).max(p.2) - p.0.min(p.1).min(p.2);
        assert!(spread < 40, "plate should be close to grey: {p:?}");
        assert!(p.0 > 34 && p.0 < 140, "mid-dark: {p:?}");
        let black = plate_color(IconInfo { coverage: 0.5, avg: (0, 0, 0) }, false);
        assert_eq!(black, (34, 34, 34));
    }

    #[test]
    fn icon_sizes_are_bounded() {
        assert_eq!(icon_bytes(1, 1), Some(4));
        assert_eq!(icon_bytes(256, 256), Some(262_144));
        assert_eq!(icon_bytes(MAX_ICON_SIDE as i64, MAX_ICON_SIDE as i64), Some(4096 * 4096 * 4));
        for (w, h) in [(0, 5), (5, 0), (-1, 5), (5, -1), (MAX_ICON_SIDE as i64 + 1, 1), (1, i32::MAX as i64), (i64::MAX, i64::MAX)] {
            assert_eq!(icon_bytes(w, h), None, "{w}x{h}");
        }
    }

    #[test]
    fn cache_rejects_oversized_headers() {
        let p = tmp("huge.bgra");
        // Claims 65536 x 65536: refused before any allocation of that size.
        let mut data = 65_536u32.to_le_bytes().to_vec();
        data.extend_from_slice(&65_536u32.to_le_bytes());
        data.extend_from_slice(&[0u8; 16]);
        std::fs::write(&p, &data).unwrap();
        assert!(read_cache(&p).is_none());
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn the_shell_is_asked_for_its_largest_frame_whatever_the_size() {
        // One source for every dock size and scale (it used to be the
        // nearest "usual" side above the peak: 96 / 128 / 192).
        for t in [24, 77, 116, 154, 256, 384] {
            for light in [false, true] {
                assert_eq!(source_tag(r"C:\Apps\a.exe", t, light), "src256");
            }
            // Packaged apps: one per dock theme (their logos come in both).
            assert_eq!(source_tag("shell:AppsFolder\\A_x!App", t, false), "src256");
            assert_eq!(source_tag("shell:appsfolder\\A_x!App", t, true), "src256-light");
        }
        assert!(themed("shell:AppsFolder\\A_x!App") && !themed(r"C:\Apps\a.exe"));
        // A picture: at least SOURCE_PX, more when drawn larger.
        assert_eq!(source_tag(r"C:\Pics\a.PNG", 77, false), "pic256");
        assert_eq!(source_tag(r"C:\Pics\a.png", 384, true), "pic384");
        // An .ico file: its own tag (review of 4d3be53f, 2); shortcuts and
        // an icon of a file by index keep theirs.
        for light in [false, true] {
            assert_eq!(source_tag(r"C:\Icons\a.ICO", 77, light), "ico256");
            assert_eq!(source_tag(r"C:\Links\a.lnk", 77, light), "src256");
        }
        assert_eq!(source_tag(r"C:\NoSuch\a.ico,2", 77, false), "src256");
        // An app whose ID ends in ".ico" keeps its themed tags (confirm
        // review of f819ab54).
        let app = "shell:AppsFolder\\A_x!Viewer.ico";
        assert!(!plain_ico(app));
        assert_eq!((source_tag(app, 77, false), source_tag(app, 77, true)), ("src256".into(), "src256-light".into()));
    }

    #[test]
    fn an_older_ico_cache_does_not_shadow_its_own_frame() {
        // Review of 4d3be53f, 2: the boxed 48 px result an older version
        // cached for an .ico (tag src256) is not what the dock reads now.
        let _com = Com::new();
        let p = tmp("cached.ico");
        std::fs::write(&p, ico(&[(256, png(256, 256, &glass_straight(256))), (48, dib(48, &glass_straight(48)))])).unwrap();
        let key = p.to_str().unwrap();
        let old = cache_path(key, "src256").unwrap();
        std::fs::create_dir_all(old.parent().unwrap()).unwrap();
        write_cache(&old, &disc(48, 22.0, false));
        let tag = source_tag(key, 73, false);
        let new = cache_path(key, &tag).unwrap();
        assert_ne!(new, old);
        // What the icon thread does (worker.rs): no fresh cache under the
        // tag asked for, so load_source, whose answer is the 256 frame.
        assert!(!cache_fresh(key, &new) || read_cache(&new).is_none());
        let (i, src) = load_source(key, 73, Appearance::default()).unwrap();
        assert_eq!((i.width, src), (256, Source::Shell));
        let _ = std::fs::remove_file(old);
        let _ = std::fs::remove_file(new);
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn straight_alpha_is_premultiplied_once() {
        // White at alpha 64, straight: becomes 64 (it used to be drawn at
        // full white: a hard bright rim).
        let mut px = vec![255, 255, 255, 64, 10, 20, 30, 255, 0, 0, 0, 0];
        premultiply(&mut px);
        assert_eq!(px, [64, 64, 64, 64, 10, 20, 30, 255, 0, 0, 0, 0]);
        // Review of 6841fcc, 1: a dark translucent straight pixel has no
        // channel above its alpha, yet is straight: converted all the same.
        let mut dark = vec![32, 32, 32, 128];
        premultiply(&mut dark);
        assert_eq!(dark, [16, 16, 16, 128]);
    }

    /// A white disc of radius `r` (antialiased, premultiplied) centered on
    /// an `n` px canvas; `frame`: the shell's faint frame around the canvas.
    fn disc(n: u32, r: f32, frame: bool) -> Icon {
        let mut pixels = vec![0u8; (n * n * 4) as usize];
        let c = n as f32 / 2.0;
        for y in 0..n {
            for x in 0..n {
                let d = ((x as f32 + 0.5 - c).powi(2) + (y as f32 + 0.5 - c).powi(2)).sqrt();
                let mut a = ((r - d + 0.5).clamp(0.0, 1.0) * 255.0).round() as u8;
                let edge = x.min(y).min(n - 1 - x).min(n - 1 - y);
                if frame && edge < 3 && a == 0 {
                    a = [38, 64, 13][edge as usize];
                }
                let i = ((y * n + x) * 4) as usize;
                pixels[i..i + 4].copy_from_slice(&[a, a, a, a]);
            }
        }
        Icon { width: n, height: n, pixels }
    }

    /// `i` stretched `k` times with nearest neighbor.
    fn stretch(i: &Icon, k: u32) -> Icon {
        let n = i.width * k;
        let mut pixels = Vec::with_capacity((n * n * 4) as usize);
        for y in 0..n {
            for x in 0..n {
                let o = (((y / k) * i.width + x / k) * 4) as usize;
                pixels.extend_from_slice(&i.pixels[o..o + 4]);
            }
        }
        Icon { width: n, height: n, pixels }
    }

    #[test]
    fn a_boxed_or_blocky_frame_is_not_taken_for_a_real_one() {
        // Real art at its own size.
        assert!(!looks_upscaled(&disc(256, 120.0, false)));
        assert!(!looks_upscaled(&disc(48, 22.0, false)));
        assert!(!looks_upscaled(&disc(32, 15.0, false)));
        // A full-bleed square (its edge is opaque, not a faint frame).
        let full = Icon { width: 256, height: 256, pixels: [200u8, 100, 50, 255].repeat(256 * 256) };
        assert!(!looks_upscaled(&full));
        // Small art with margins but no frame (wabmig.exe's 256 frame).
        assert!(!looks_upscaled(&disc(256, 60.0, false)));
        // What GetImage(256) gives for an exe whose largest frame is 48.
        assert!(looks_upscaled(&disc(256, 23.0, true)));
        assert!(looks_upscaled(&disc(128, 23.0, true)));
        // A frame that big in its canvas is a real one with a soft edge.
        assert!(!looks_upscaled(&disc(256, 110.0, true)));
        // Nearest-neighbor stretched 2x and 4x.
        assert!(looks_upscaled(&stretch(&disc(64, 30.0, false), 2)));
        assert!(looks_upscaled(&stretch(&disc(64, 30.0, false), 4)));
        assert!(block_share(&disc(256, 120.0, false)) < 0.05);
        // Review of 6841fcc, 2: real art with a flat translucent
        // background (glass) and a crisp opaque middle is neither: its
        // flat pixels are no edge, and its background shows where the
        // shell's frame leaves a gap.
        for n in [256, 48, 32] {
            let glass = glass(n);
            assert!(!looks_upscaled(&glass), "{n}");
            assert!(block_share(&glass) < 0.05, "{n}");
        }
    }

    /// A translucent grey background over the whole canvas with an opaque
    /// square in the middle (premultiplied).
    fn glass(n: u32) -> Icon {
        let mut pixels = Vec::with_capacity((n * n * 4) as usize);
        for y in 0..n {
            for x in 0..n {
                let inner = (n / 4..n - n / 4).contains(&x) && (n / 4..n - n / 4).contains(&y);
                pixels.extend_from_slice(if inner { &[40, 90, 200, 255] } else { &[48, 48, 48, 96] });
            }
        }
        Icon { width: n, height: n, pixels }
    }

    const NS: &str = r#"xmlns="http://schemas.microsoft.com/appx/manifest/foundation/windows10" xmlns:uap="http://schemas.microsoft.com/appx/manifest/uap/windows10""#;

    #[test]
    fn the_logo_of_the_right_application_is_read() {
        let xml = format!(
            r##"<?xml version="1.0" encoding="utf-8"?><Package {NS}><Applications>
            <Application Id="Helper" Executable="h.exe"><uap:VisualElements Square44x44Logo="Assets\Helper.png" BackgroundColor="#102030"/></Application>
            <Application Id="App" Executable="a.exe">
              <uap:VisualElements DisplayName="A" Square150x150Logo="Assets\Med.png" Square44x44Logo="Assets\App&amp;List.png" BackgroundColor="transparent">
              </uap:VisualElements>
            </Application></Applications></Package>"##
        );
        assert_eq!(manifest_logo(&xml, "app"), Some((r"Assets\App&List.png".into(), Some("transparent".into()))));
        assert_eq!(manifest_logo(&xml, "Helper"), Some((r"Assets\Helper.png".into(), Some("#102030".into()))));
        assert_eq!(manifest_logo(&xml, "Other"), None);
        assert_eq!(manifest_logo("not xml <", "App"), None);
    }

    #[test]
    fn the_manifest_is_read_as_xml() {
        // Review of 6841fcc, 3: a line break after the element name, and
        // spaces with single quotes around an attribute's value.
        let broken = format!(
            "<Package {NS}><Applications><Application\n  Id=\"App\"><uap:VisualElements\n Square44x44Logo=\"Assets\\A.png\"/></Application></Applications></Package>"
        );
        assert_eq!(manifest_logo(&broken, "App").map(|l| l.0), Some(r"Assets\A.png".into()));
        let quoted = format!("<Package {NS}><Applications><Application Id = 'App'><uap:VisualElements Square44x44Logo = 'Assets\\B.png' BackgroundColor = '#FFFFFF' /></Application></Applications></Package>");
        assert_eq!(manifest_logo(&quoted, "App"), Some((r"Assets\B.png".into(), Some("#FFFFFF".into()))));
        // An example in a comment before the real one is not read.
        let commented = format!(
            r#"<Package {NS}><Applications>
            <!-- <Application Id="App"><uap:VisualElements Square44x44Logo="..\..\outside\Logo.png"/></Application> -->
            <Application Id="App"><uap:VisualElements Square44x44Logo="Assets\Real.png"/></Application>
            </Applications></Package>"#
        );
        assert_eq!(manifest_logo(&commented, "App").map(|l| l.0), Some(r"Assets\Real.png".into()));
        // Only Package/Applications/Application counts.
        let nested = format!(r#"<Package {NS}><Extensions><Application Id="App"><uap:VisualElements Square44x44Logo="No.png"/></Application></Extensions></Package>"#);
        assert_eq!(manifest_logo(&nested, "App"), None);
    }

    #[test]
    fn manifest_paths_stay_in_the_package() {
        // Review of 6841fcc, 5.
        for bad in [r"\outside\Logo.png", r"D:outside\Logo.png", r"C:\outside\Logo.png", r"..\Logo.png", r"Assets\..\..\Logo.png", r"\\server\share\Logo.png", ""] {
            assert!(!package_relative(Path::new(bad)), "{bad}");
        }
        for good in [r"Assets\Logo.png", "Assets/Logo.png", r".\Assets\Logo.png", "Logo.png"] {
            assert!(package_relative(Path::new(good)), "{good}");
        }
        let root = tmp("pkg");
        let side = tmp("pkg-side");
        std::fs::create_dir_all(root.join("Assets")).unwrap();
        std::fs::create_dir_all(&side).unwrap();
        std::fs::write(root.join("Assets").join("L.png"), b"x").unwrap();
        std::fs::write(side.join("L.png"), b"x").unwrap();
        assert!(inside(&root, &root.join("Assets").join("L.png")));
        assert!(!inside(&root, &side.join("L.png")));
        assert!(!inside(&root, &root));
        assert!(!inside(&root, &root.join("missing.png")));
        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_dir_all(side);
    }

    #[test]
    fn the_largest_real_logo_file_is_picked() {
        let names: Vec<String> = [
            "AppList.png",
            "AppList.scale-200.png",
            "AppList.scale-400.png",
            "AppList.targetsize-48.png",
            "AppList.targetsize-256_altform-unplated.png",
            "AppList.targetsize-256_altform-lightunplated.png",
            "AppList.targetsize-256_contrast-black.png",
            "AppListOther.targetsize-512.png",
            "Med.scale-400.png",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        // (name, px, plated)
        assert_eq!(pick_logo("AppList", &names, false), Some(("AppList.targetsize-256_altform-unplated.png".into(), 256, false)));
        // Review of 6841fcc, 4: a light dock takes the light form.
        assert_eq!(pick_logo("AppList", &names, true), Some(("AppList.targetsize-256_altform-lightunplated.png".into(), 256, false)));
        // Only scale files (plated): scale-400 of a 44 px logo is 176 px,
        // for either theme (it brings its own tile).
        let scaled: Vec<String> = ["applist.scale-100.png", "applist.scale-400.png"].iter().map(|s| s.to_string()).collect();
        for light in [false, true] {
            assert_eq!(pick_logo("AppList", &scaled, light), Some(("applist.scale-400.png".into(), 176, true)));
        }
        // An unplated form for dark only: a light dock leaves it to the
        // shell; a dark one takes it over a larger plated file.
        let dark_only: Vec<String> = ["A.targetsize-64_altform-unplated.png", "A.targetsize-256.png"].iter().map(|s| s.to_string()).collect();
        assert_eq!(pick_logo("A", &dark_only, true), None);
        assert_eq!(pick_logo("A", &dark_only, false), Some(("A.targetsize-64_altform-unplated.png".into(), 64, false)));
        assert_eq!(pick_logo("Missing", &names, false), None);
    }

    // ---- with the real shell and WIC (files made here) ------------------

    struct Com;
    impl Com {
        fn new() -> Self {
            use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
            unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok().unwrap() };
            Com
        }
    }
    impl Drop for Com {
        fn drop(&mut self) {
            unsafe { windows::Win32::System::Com::CoUninitialize() };
        }
    }

    /// An .ico of frames (n px, the entry's bytes: `dib` or `png`).
    fn ico(frames: &[(u32, Vec<u8>)]) -> Vec<u8> {
        let mut dir = Vec::new();
        let mut data = Vec::new();
        let start = 6 + 16 * frames.len();
        for (n, body) in frames {
            dir.extend_from_slice(&[(n % 256) as u8, (n % 256) as u8, 0, 0]);
            dir.extend_from_slice(&1u16.to_le_bytes());
            dir.extend_from_slice(&32u16.to_le_bytes());
            dir.extend_from_slice(&(body.len() as u32).to_le_bytes());
            dir.extend_from_slice(&((start + data.len()) as u32).to_le_bytes());
            data.extend_from_slice(body);
        }
        let mut out = vec![0, 0, 1, 0];
        out.extend_from_slice(&(frames.len() as u16).to_le_bytes());
        out.extend(dir);
        out.extend(data);
        out
    }

    /// A 32 bpp DIB icon frame from straight BGRA (top-down) pixels.
    fn dib(n: u32, px: &[u8]) -> Vec<u8> {
        let mut body = Vec::new();
        for v in [40u32, n, n * 2] {
            body.extend_from_slice(&v.to_le_bytes());
        }
        body.extend_from_slice(&1u16.to_le_bytes());
        body.extend_from_slice(&32u16.to_le_bytes());
        body.extend_from_slice(&[0u8; 24]);
        for y in (0..n).rev() {
            body.extend_from_slice(&px[(y * n * 4) as usize..((y + 1) * n * 4) as usize]);
        }
        body.extend(std::iter::repeat_n(0u8, (n.div_ceil(32) * 4 * n) as usize));
        body
    }

    /// A PNG (RGBA, stored deflate) of straight BGRA (top-down) pixels.
    fn png(w: u32, h: u32, bgra: &[u8]) -> Vec<u8> {
        fn crc(data: &[u8]) -> u32 {
            let mut c = 0xFFFF_FFFFu32;
            for b in data {
                c ^= *b as u32;
                for _ in 0..8 {
                    c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
                }
            }
            !c
        }
        let chunk = |out: &mut Vec<u8>, kind: &[u8], body: &[u8]| {
            out.extend_from_slice(&(body.len() as u32).to_be_bytes());
            let mut t = kind.to_vec();
            t.extend_from_slice(body);
            out.extend_from_slice(&t);
            out.extend_from_slice(&crc(&t).to_be_bytes());
        };
        let mut raw = Vec::new();
        for row in bgra.chunks_exact((w * 4) as usize).take(h as usize) {
            raw.push(0);
            for p in row.chunks_exact(4) {
                raw.extend_from_slice(&[p[2], p[1], p[0], p[3]]);
            }
        }
        let mut z = vec![0x78, 0x01];
        for (i, part) in raw.chunks(65_535).enumerate() {
            let last = (i + 1) * 65_535 >= raw.len();
            z.push(last as u8);
            z.extend_from_slice(&(part.len() as u16).to_le_bytes());
            z.extend_from_slice(&(!(part.len() as u16)).to_le_bytes());
            z.extend_from_slice(part);
        }
        let (mut a, mut b) = (1u32, 0u32);
        for x in &raw {
            a = (a + *x as u32) % 65_521;
            b = (b + a) % 65_521;
        }
        z.extend_from_slice(&((b << 16) | a).to_be_bytes());
        let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
        let mut ihdr = Vec::new();
        ihdr.extend_from_slice(&w.to_be_bytes());
        ihdr.extend_from_slice(&h.to_be_bytes());
        ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
        chunk(&mut out, b"IHDR", &ihdr);
        chunk(&mut out, b"IDAT", &z);
        chunk(&mut out, b"IEND", &[]);
        out
    }

    /// `glass(n)` as straight BGRA (what an .ico holds).
    fn glass_straight(n: u32) -> Vec<u8> {
        let mut g = glass(n).pixels;
        for p in g.chunks_exact_mut(4) {
            let a = p[3] as u32;
            for c in &mut p[..3] {
                *c = (*c as u32 * 255 / a.max(1)).min(255) as u8;
            }
        }
        g
    }

    #[test]
    fn real_shell_and_wic_pictures_come_out_premultiplied() {
        let _com = Com::new();
        // Review of 6841fcc, 1: a dark translucent straight PNG chosen as
        // an icon (RGB 32 at alpha 128) is 16 premultiplied.
        let p = tmp("dark.png");
        std::fs::write(&p, png(256, 256, &[32u8, 32, 32, 128].repeat(256 * 256))).unwrap();
        let i = load(p.to_str().unwrap(), 256).unwrap();
        assert_eq!((i.width, &i.pixels[..4]), (256, &[16u8, 16, 16, 128][..]));
        // A larger picture is shrunk to the size asked (by WIC).
        let small = load(p.to_str().unwrap(), 64).unwrap();
        assert_eq!((small.width, small.height), (64, 64));
        let _ = std::fs::remove_file(p);
        // The same pixel in an icon frame, through the shell.
        let q = tmp("dark.ico");
        std::fs::write(&q, ico(&[(48, dib(48, &[32u8, 32, 32, 128].repeat(48 * 48)))])).unwrap();
        let i = load(q.to_str().unwrap(), 48).unwrap();
        assert_eq!((i.width, &i.pixels[..4]), (48, &[16u8, 16, 16, 128][..]));
        let _ = std::fs::remove_file(q);
    }

    #[test]
    fn real_shell_a_real_256_frame_with_glass_is_kept_and_a_boxed_48_is_not() {
        let _com = Com::new();
        // Review of 6841fcc, 2: 256 / 48 / 32 frames, each drawn with a
        // translucent background (the 256 one as PNG, like real icons):
        // the 256 one is the source.
        let p = tmp("glass.ico");
        std::fs::write(&p, ico(&[(256, png(256, 256, &glass_straight(256))), (48, dib(48, &glass_straight(48))), (32, dib(32, &glass_straight(32)))])).unwrap();
        let (i, src) = load_source(p.to_str().unwrap(), 73, Appearance::default()).unwrap();
        assert_eq!((i.width, src), (256, Source::Shell));
        // Its own 256 frame, not a rendition of the 48 one (on 26200 the
        // shell boxes the 48 one for an .ico without a 16 px frame).
        assert_eq!(&i.pixels[..4], &[47, 47, 47, 96], "the glass, premultiplied");
        assert!(!looks_upscaled(&i));
        let _ = std::fs::remove_file(p);
        // A lone 256 frame (what icon converters write): kept.
        let p = tmp("glass256.ico");
        std::fs::write(&p, ico(&[(256, png(256, 256, &glass_straight(256)))])).unwrap();
        let (i, src) = load_source(p.to_str().unwrap(), 73, Appearance::default()).unwrap();
        assert_eq!((i.width, src), (256, Source::Shell));
        assert!(!looks_upscaled(&i));
        let _ = std::fs::remove_file(p);
        // Without its 256 frame, the same art comes back boxed (its glass
        // inside the box): the 48 frame is taken.
        let p = tmp("glass48.ico");
        std::fs::write(&p, ico(&[(48, dib(48, &glass_straight(48))), (32, dib(32, &glass_straight(32)))])).unwrap();
        let (i, src) = load_source(p.to_str().unwrap(), 73, Appearance::default()).unwrap();
        assert_eq!((i.width, src), (48, Source::Frame(48)));
        let _ = std::fs::remove_file(p);
        // Only 48 and 32: the shell boxes the 48 one in a 256 canvas; the
        // 48 frame is taken.
        let q = tmp("small.ico");
        let disc_straight = |n: u32, r: f32| {
            let mut d = disc(n, r, false).pixels;
            for px in d.chunks_exact_mut(4) {
                let a = px[3];
                px[..3].copy_from_slice(&[if a > 0 { 255 } else { 0 }, 128, 0]);
            }
            d
        };
        std::fs::write(&q, ico(&[(48, dib(48, &disc_straight(48, 22.0))), (32, dib(32, &disc_straight(32, 15.0)))])).unwrap();
        let (i, src) = load_source(q.to_str().unwrap(), 73, Appearance::default()).unwrap();
        assert_eq!((i.width, src), (48, Source::Frame(48)));
        let _ = std::fs::remove_file(q);
    }

    /// What the shell hands over for an icon: size, the rings `boxed`
    /// reads, the clearly visible box, `block_share`, a few pixels.
    fn describe(i: &Icon) -> String {
        let (w, h) = (i.width as usize, i.height as usize);
        let a = |x: usize, y: usize| i.pixels[(y * w + x) * 4 + 3];
        let ring = |k: usize| -> (usize, usize, usize) {
            if w <= 2 * k || h <= 2 * k {
                return (0, 0, 0);
            }
            let r: Vec<u8> = (k..w - k).flat_map(|x| [a(x, k), a(x, h - 1 - k)]).chain((k..h - k).flat_map(|y| [a(k, y), a(w - 1 - k, y)])).collect();
            (r.iter().filter(|&&v| v > 0).count(), r.iter().filter(|&&v| v == 0).count(), r.len())
        };
        let (mut x0, mut y0, mut x1, mut y1) = (usize::MAX, usize::MAX, 0, 0);
        for y in 0..h {
            for x in 0..w {
                if a(x, y) > 96 {
                    (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
                }
            }
        }
        let px = |x: usize, y: usize| &i.pixels[(y * w + x) * 4..(y * w + x) * 4 + 4];
        format!(
            "{w}x{h} ring1(vis,clear,n) {:?} ring8 {:?} box({x0},{y0})-({x1},{y1}) block_share {:.3} boxed {} blocky {} px00 {:?} px11 {:?} px44 {:?} mid {:?}",
            ring(1),
            ring(8),
            block_share(i),
            boxed(i),
            blocky(i),
            px(0, 0),
            px(1.min(w - 1), 1.min(h - 1)),
            px(4.min(w - 1), 4.min(h - 1)),
            px(w / 2, h / 2)
        )
    }

    /// What the shell's GetImage hands over for test .ico files (and for
    /// files in NOPBAR_DIAG_ICO, ';'-separated, read where they are),
    /// beside the file's own 256 frame. Measured on 26200: an .ico without
    /// a 16 px frame comes back as its 48 px rendition boxed at 256.
    /// cargo test -p dock icons::tests::diagnose_ico_frames -- --ignored --nocapture
    #[test]
    #[ignore = "prints what the shell returns for .ico files"]
    fn diagnose_ico_frames() {
        let _com = Com::new();
        let flags = SIIGBF_ICONONLY | SIIGBF_BIGGERSIZEOK;
        let g = |n: u32| if n == 256 { png(256, 256, &glass_straight(256)) } else { dib(n, &glass_straight(n)) };
        let made = |sides: &[u32]| ico(&sides.iter().map(|&n| (n, g(n))).collect::<Vec<_>>());
        let mut files: Vec<(String, PathBuf, bool)> = Vec::new();
        for sides in [&[256, 48, 32][..], &[256], &[256, 24], &[256, 16], &[256, 48, 32, 16]] {
            let p = tmp(&format!("diag-{}.ico", sides.iter().map(|n| n.to_string()).collect::<Vec<_>>().join("-")));
            std::fs::write(&p, made(sides)).unwrap();
            files.push((format!("glass {sides:?}"), p, true));
        }
        for real in std::env::var("NOPBAR_DIAG_ICO").unwrap_or_default().split(';').filter(|s| !s.is_empty()) {
            files.push((real.to_string(), PathBuf::from(real), false));
        }
        for (name, p, made) in files {
            let key = p.to_str().unwrap();
            println!("== {name}");
            for px in [256, 48] {
                println!("  GetImage {px}: {:?}", image(key, px, flags).map(|i| describe(&i)));
            }
            if let (Some(a), Some(b)) = (image(key, 256, flags), resource_icon(key, 0, 256)) {
                let n = a.pixels.chunks_exact(4).zip(b.pixels.chunks_exact(4)).filter(|(p, q)| p.iter().zip(q.iter()).any(|(u, v)| u.abs_diff(*v) > 24)).count();
                println!("  GetImage 256 vs own 256 frame: {n} px differ");
            }
            println!("  load_source: {:?}", load_source(key, 73, Appearance::default()).map(|(i, s)| (i.width, s)));
            if made {
                let _ = std::fs::remove_file(p);
            }
        }
    }

    /// Whether the icon file / first icon group of an exe or dll has a
    /// 256 px frame (an ICONDIR / GRPICONDIR entry of width 0). None: not
    /// read.
    fn has_256_frame(file: &str, index: i32) -> Option<bool> {
        use windows::core::PCWSTR;
        use windows::Win32::Foundation::FreeLibrary;
        use windows::Win32::System::LibraryLoader::{
            EnumResourceNamesW, FindResourceW, LoadLibraryExW, LoadResource, LockResource, SizeofResource, LOAD_LIBRARY_AS_DATAFILE, LOAD_LIBRARY_AS_IMAGE_RESOURCE,
        };
        use windows::Win32::UI::WindowsAndMessaging::RT_GROUP_ICON;
        let entries = |dir: &[u8], entry: usize| -> Option<bool> {
            let n = u16::from_le_bytes(dir.get(4..6)?.try_into().ok()?) as usize;
            Some((0..n).any(|k| dir.get(6 + k * entry).is_some_and(|&w| w == 0)))
        };
        let ext = Path::new(file).extension()?.to_str()?.to_ascii_lowercase();
        if ext == "ico" {
            return entries(&std::fs::read(file).ok()?, 16);
        }
        if !["exe", "dll", "cpl", "ocx", "mun"].contains(&ext.as_str()) {
            return None;
        }
        unsafe extern "system" fn collect(_: windows::Win32::Foundation::HMODULE, _: PCWSTR, name: PCWSTR, out: isize) -> windows::core::BOOL {
            let out = &mut *(out as *mut Vec<usize>);
            out.push(name.0 as usize);
            true.into()
        }
        // System files keep their icons in SystemResources\<name>.mun.
        let read = |file: &str| unsafe {
            let m = LoadLibraryExW(&HSTRING::from(file), None, LOAD_LIBRARY_AS_DATAFILE | LOAD_LIBRARY_AS_IMAGE_RESOURCE).ok()?;
            let mut names: Vec<usize> = Vec::new();
            let _ = EnumResourceNamesW(Some(m), RT_GROUP_ICON, Some(collect), &mut names as *mut _ as isize);
            // Index i: the i-th group; negative: resource id.
            let name = if index < 0 {
                Some(PCWSTR((-index) as usize as *const u16))
            } else {
                names.get(index as usize).map(|&n| PCWSTR(n as *const u16))
            };
            let got = name.and_then(|name| {
                let r = FindResourceW(Some(m), name, RT_GROUP_ICON);
                if r.is_invalid() {
                    return None;
                }
                let len = SizeofResource(Some(m), r) as usize;
                let data = LoadResource(Some(m), r).ok()?;
                let ptr = LockResource(data) as *const u8;
                (!ptr.is_null()).then(|| entries(std::slice::from_raw_parts(ptr, len), 14)).flatten()
            });
            let _ = FreeLibrary(m);
            got
        };
        read(file).or_else(|| {
            let name = Path::new(file).file_name()?.to_str()?;
            read(&format!(r"{}\SystemResources\{name}.mun", std::env::var("SystemRoot").ok()?))
        })
    }

    /// Where a file's icon comes from: a shortcut's icon location (or
    /// target), anything else itself, index 0.
    fn icon_file(path: &Path) -> Option<(String, i32)> {
        use windows::Win32::System::Com::{CoCreateInstance, IPersistFile, CLSCTX_INPROC_SERVER, STGM_READ};
        use windows::Win32::UI::Shell::{IShellLinkW, ShellLink};
        use windows::core::Interface;
        let s = path.to_str()?.to_string();
        if !s.to_ascii_lowercase().ends_with(".lnk") {
            return Some((s, 0));
        }
        unsafe {
            let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).ok()?;
            link.cast::<IPersistFile>().ok()?.Load(&HSTRING::from(s.as_str()), STGM_READ).ok()?;
            let mut buf = [0u16; 1024];
            let mut idx = 0i32;
            let expand = |b: &[u16]| {
                let raw = String::from_utf16_lossy(&b[..b.iter().position(|&c| c == 0).unwrap_or(b.len())]);
                // %name% (any case: Windows' environment ignores it).
                let parts: Vec<&str> = raw.split('%').collect();
                let mut out = String::new();
                let mut k = 0;
                while k < parts.len() {
                    match (k % 2 == 1 && k + 1 < parts.len()).then(|| std::env::var(parts[k]).ok()).flatten() {
                        Some(v) => out.push_str(&v),
                        None if k % 2 == 1 => {
                            out.push('%');
                            out.push_str(parts[k]);
                        }
                        None => out.push_str(parts[k]),
                    }
                    k += 1;
                }
                out
            };
            if link.GetIconLocation(&mut buf, &mut idx).is_ok() && buf[0] != 0 {
                return Some((expand(&buf), idx));
            }
            let mut buf = [0u16; 1024];
            link.GetPath(&mut buf, std::ptr::null_mut(), 0).ok()?;
            (buf[0] != 0).then(|| (expand(&buf), 0))
        }
    }

    fn walk(dir: &Path, depth: u32, exts: &[&str], out: &mut Vec<PathBuf>, cap: usize) {
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        let mut items: Vec<_> = rd.flatten().map(|e| e.path()).collect();
        items.sort();
        for p in items {
            if out.len() >= cap {
                return;
            }
            if p.is_dir() {
                if depth > 0 {
                    walk(&p, depth - 1, exts, out, cap);
                }
            } else if p.extension().and_then(|e| e.to_str()).is_some_and(|e| exts.contains(&e.to_ascii_lowercase().as_str())) {
                out.push(p);
            }
        }
    }

    /// Real apps on this PC through `load_source`: which ones with a real
    /// 256 px frame come back as a smaller frame (a blurry dock icon).
    /// cargo test -p dock icons::tests::survey_real_icons -- --ignored --nocapture
    #[test]
    #[ignore = "reads the icons of this PC's apps and prints a report"]
    fn survey_real_icons() {
        let _com = Com::new();
        let env = |k: &str| std::env::var(k).unwrap_or_default();
        let mut files = Vec::new();
        for (dir, depth, exts, cap) in [
            (format!(r"{}\Microsoft\Windows\Start Menu\Programs", env("ProgramData")), 3, &["lnk"][..], 250),
            (format!(r"{}\Microsoft\Windows\Start Menu\Programs", env("APPDATA")), 3, &["lnk"][..], 150),
            (env("ProgramFiles"), 2, &["exe"][..], 200),
            (env("ProgramFiles(x86)"), 2, &["exe"][..], 150),
            (format!(r"{}\System32", env("SystemRoot")), 0, &["exe"][..], 150),
            (env("ProgramFiles"), 4, &["ico"][..], 100),
            (env("ProgramFiles(x86)"), 4, &["ico"][..], 100),
            (env("LOCALAPPDATA"), 3, &["ico"][..], 100),
        ] {
            let mut part = Vec::new();
            walk(Path::new(&dir), depth, exts, &mut part, cap);
            files.extend(part);
        }
        let flags = SIIGBF_ICONONLY | SIIGBF_BIGGERSIZEOK;
        let (mut total, mut with256, mut kept, mut down, mut small_ok, mut small_shell, mut unknown) = (0, 0, 0, 0, 0, 0, 0);
        for f in &files {
            let key = f.to_str().unwrap();
            let Some((i, src)) = load_source(key, 73, Appearance::default()) else { continue };
            total += 1;
            let has = icon_file(f).and_then(|(file, idx)| has_256_frame(&file, idx));
            match (has, &src) {
                (Some(true), Source::Shell) => {
                    with256 += 1;
                    kept += 1
                }
                (Some(true), Source::Frame(_)) => {
                    with256 += 1;
                    down += 1;
                    let big = image(key, SOURCE_PX, flags).unwrap();
                    println!("DOWNGRADED {key} -> {}x{} {src:?}\n    256: {}", i.width, i.height, describe(&big));
                }
                (Some(false), Source::Frame(_)) => small_ok += 1,
                (Some(false), Source::Shell) => {
                    small_shell += 1;
                    let big = image(key, SOURCE_PX, flags).unwrap();
                    println!("no-256 kept as Shell {key}\n    256: {}", describe(&big));
                }
                _ => unknown += 1,
            }
        }
        println!("files {} loaded {total}; real 256: {with256} (kept {kept}, downgraded {down}); no 256: frame {small_ok}, shell {small_shell}; unknown {unknown}", files.len());
    }

    #[test]
    fn an_icon_directory_lists_its_frame_sides() {
        let file = ico(&[(256, vec![1, 2, 3]), (48, vec![4]), (16, vec![5])]);
        assert_eq!(ico_dir_sides(&file), Some(vec![256, 48, 16]));
        // Only the directory is needed.
        assert_eq!(ico_dir_sides(&file[..6 + 16 * 3]), Some(vec![256, 48, 16]));
        assert_eq!(ico_dir_sides(&file[..6 + 16]), None, "cut short");
        assert_eq!(ico_dir_sides(b"\x89PNG\r\n\x1a\n"), None);
        assert_eq!(ico_dir_sides(&[0, 0, 1, 0, 0, 0]), None, "no frames");
        assert_eq!(ico_dir_sides(&[0, 0, 2, 0, 1, 0]), None, "a cursor");
        // Review of 4d3be53f, 1: every cut of the header, and of an entry,
        // is refused (a 4-byte file used to panic: panic = abort).
        let one = ico(&[(256, vec![1])]);
        for len in 0..6 + 16 {
            assert_eq!(ico_dir_sides(&one[..len]), None, "{len} bytes");
        }
        for len in 0..6 {
            assert_eq!(ico_dir_len(&one[..len]), None, "{len} bytes");
        }
        assert_eq!(ico_dir_len(&one[..6]), Some(22));
        assert_eq!(ico_dir_sides(&[0, 0, 1, 0, 1, 0, 0]), None);
        assert_eq!(ico_dir_sides(&one[..22]), Some(vec![256]));
        // Review of 4d3be53f, 3: more than 255 entries, up to u16's most.
        for n in [255usize, 256, 65_535] {
            let mut dir = vec![0, 0, 1, 0];
            dir.extend_from_slice(&(n as u16).to_le_bytes());
            for k in 0..n {
                dir.extend_from_slice(&[if k == n - 1 { 0 } else { 48 }; 16]);
            }
            assert_eq!(ico_dir_len(&dir), Some(6 + 16 * n));
            let sides = ico_dir_sides(&dir).unwrap();
            assert_eq!((sides.len(), sides[n - 1], sides[0]), (n, 256, 48), "{n}");
            assert_eq!(ico_dir_sides(&dir[..dir.len() - 1]), None, "{n}, cut short");
        }
    }

    #[test]
    fn an_ico_file_is_read_up_to_its_directory_only() {
        let case = |name: &str, bytes: &[u8]| {
            let p = tmp(name);
            std::fs::write(&p, bytes).unwrap();
            let got = ico_frames(p.to_str().unwrap());
            let _ = std::fs::remove_file(p);
            got
        };
        // Cut short anywhere in the directory: None, no panic.
        let one = ico(&[(256, vec![1, 2, 3])]);
        for len in [0, 4, 5, 6, 7, 21] {
            assert_eq!(case(&format!("cut{len}.ico"), &one[..len]), None, "{len} bytes");
        }
        assert_eq!(case("whole.ico", &one), Some(vec![256]));
        // 65535 entries (about 1 MiB), the 256 one last.
        let n = 65_535usize;
        let mut big = vec![0, 0, 1, 0];
        big.extend_from_slice(&(n as u16).to_le_bytes());
        for k in 0..n {
            big.extend_from_slice(&[if k == n - 1 { 0 } else { 32 }; 16]);
        }
        let sides = case("many.ico", &big).unwrap();
        assert_eq!((sides.len(), sides[n - 1]), (n, 256));
        assert_eq!(case("manycut.ico", &big[..big.len() - 8]), None);
        // Not an .ico key: not read.
        assert_eq!(ico_frames(r"C:\Apps\a.exe"), None);
    }

    #[test]
    fn tile_colors() {
        assert_eq!(parse_hex_color("#102030"), Some((0x10, 0x20, 0x30)));
        assert_eq!(parse_hex_color("#FF102030"), Some((0x10, 0x20, 0x30)));
        assert_eq!(parse_hex_color("transparent"), None);
        assert_eq!(parse_hex_color("#80102030"), None);
        let mut px = vec![0, 0, 128, 128, 0, 0, 0, 0];
        over_color(&mut px, (0, 0, 255));
        assert_eq!(px, [127, 0, 128, 255, 255, 0, 0, 255]);
    }

    #[test]
    fn pictures_fit_their_square() {
        assert_eq!(fit_box(96, 96, 48), (48.0, 48.0));
        assert_eq!(fit_box(200, 100, 48), (48.0, 24.0));
        assert_eq!(fit_box(100, 200, 48), (24.0, 48.0));
        assert_eq!(fit_box(16, 16, 77), (77.0, 77.0));
        assert_eq!(fit_box(0, 0, 10), (10.0, 10.0));
    }

    #[test]
    fn fnv64_stable() {
        assert_eq!(fnv64(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv64(b"a"), 0xaf63_dc4c_8601_ec8c);
    }

    #[test]
    fn cache_key_ignores_case() {
        assert_eq!(cache_path(r"C:\X\No.exe", "src256"), cache_path(r"c:\x\no.EXE", "src256"));
        let p = cache_path(r"C:\X\No.exe", "src256").unwrap();
        assert!(p.to_string_lossy().ends_with("-src256-pm.bgra"), "{p:?}");
        assert_ne!(cache_path(r"C:\X\No.exe", "src256"), cache_path(r"C:\X\No.exe", "96"));
    }

    #[test]
    fn stack_glyphs_for_known_folders() {
        use crate::glyphs::Glyph;
        let known = vec![("C:/Users/a/Downloads".to_string(), Glyph::Download), (String::new(), Glyph::Music)];
        assert_eq!(stack_glyph("c:/users/A/downloads/", &known), Glyph::Download);
        assert_eq!(stack_glyph("C:/Projects", &known), Glyph::Folder);
        assert_eq!(stack_glyph("", &known), Glyph::Folder, "an empty known path matches nothing");
    }

    #[test]
    fn appearance_colors_the_tiles() {
        let dark = Appearance { light: false, hc: None, flat: false };
        let hc = Appearance { light: false, hc: Some(([1, 2, 3], [250, 251, 252])), flat: false };
        assert_eq!(dark.tile(), crate::glyphs::TileLook::new(false, None));
        assert_ne!(dark.tile(), hc.tile());
        assert!(hc.tile().outline.is_some());
    }
}

#[cfg(test)]
mod resource_icon_tests {
    use super::*;

    #[test]
    fn icon_keys_with_an_index() {
        assert_eq!(icon_location(r"C:\App\resources.dll,-42"), Some((r"C:\App\resources.dll", -42)));
        assert_eq!(icon_location(r"C:\App\app.exe,1"), Some((r"C:\App\app.exe", 1)));
        assert_eq!(icon_location(r"C:\App\app.exe,0"), None, "the first icon is the file's own key");
        assert_eq!(icon_location(r"C:\App\app.exe"), None);
        assert_eq!(icon_location(r"C:\a,b\x.exe"), None, "a comma in a folder name");
        assert_eq!(icon_location(r"C:\Photos,2"), None, "no file with an extension before the comma");
        assert_eq!(icon_location(r"app.exe,1"), None, "not an absolute path");
        assert_eq!(icon_location(r"shell:AppsFolder\A_x!App"), None);
    }

    #[test]
    fn each_icon_of_a_file_is_cached_on_its_own() {
        let dll = std::env::var("SystemRoot").map(|r| format!(r"{r}\System32\shell32.dll")).unwrap_or_else(|_| r"C:\Windows\System32\shell32.dll".into());
        let first = cache_path(&dll, "src256");
        let a = cache_path(&format!("{dll},-21"), "src256");
        let b = cache_path(&format!("{dll},-22"), "src256");
        assert!(a.is_some() && a != b && a != first, "the index is part of the key");
        // The file's time is too: the same key and time, the same file.
        assert_eq!(a, cache_path(&format!("{dll},-21"), "src256"));
        assert!(cache_fresh(&format!("{dll},-21"), Path::new("x")), "versioned by the file's time, like an exe");
    }

    #[test]
    fn icons_by_index_and_by_resource_id_are_read() {
        let _com = unsafe { windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_APARTMENTTHREADED) };
        let dll = std::env::var("SystemRoot").map(|r| format!(r"{r}\System32\shell32.dll")).unwrap_or_else(|_| r"C:\Windows\System32\shell32.dll".into());
        let by_id = resource_icon(&dll, -21, 64).expect("shell32's icon 21");
        let by_index = resource_icon(&dll, 3, 64).expect("shell32's fourth icon");
        assert_eq!((by_id.width, by_id.height), (64, 64));
        assert!(by_id.pixels.iter().skip(3).step_by(4).any(|&a| a > 0), "something drawn");
        assert_ne!(by_id.pixels, by_index.pixels, "two icons, not the first one twice");
        assert!(resource_icon(&dll, -999_999, 64).is_none(), "no such icon");
        // Through the key the dock asks with.
        assert!(load(&format!("{dll},-21"), 48).is_some());
    }
}

/// Same cached known-folder resolution used by the Downloads stack glyph.
#[cfg(not(test))]
pub fn downloads_folder()->Option<String>{known_folders().into_iter().find(|(_,g)|*g==crate::glyphs::Glyph::Download).map(|(p,_)|p)}
