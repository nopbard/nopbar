//! Pictures of the notes on two virtual desktops, drawn off screen (tests
//! only): the real note windows, made hidden and drawn offscreen, placed
//! on a desktop by the hub's own rule (crate::notes::desks), and the note
//! menu drawn by its own paint. Saved to `target/notes-render/`; nothing is
//! shown and the screen is never read.

use windows::core::{GUID, HSTRING};
use windows::Win32::Foundation::{GENERIC_WRITE, HWND, RECT};
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, CreateSolidBrush, DeleteDC, DeleteObject, DrawTextW, FillRect, SelectObject, SetBkMode, SetTextColor,
    BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, DT_LEFT, DT_NOPREFIX, DT_SINGLELINE, HDC, TRANSPARENT,
};
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_ContainerFormatPng, GUID_WICPixelFormat32bppPBGRA, IWICBitmapFrameEncode, IWICImagingFactory, WICBitmapEncoderNoCache,
};
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED};
use windows::Win32::UI::WindowsAndMessaging::DestroyWindow;

use super::desks::Desks;
use super::{check, note, Look};
use crate::theme::Rgba;
use crate::vdesk::{guid_text, Sure};

const NOTE_W: i32 = 230;
const NOTE_H: i32 = 150;
const GAP: i32 = 24;
const HEAD: i32 = 40;

#[test]
#[ignore = "first note acceptance: fake desktop grid and production face painter only"]
fn notes_entry_first_desktop() {
    let _language = crate::i18n::test_lock("ko");
    unsafe { let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED); }
    use crate::desklayer::{grid, place};
    let bounds = RECT { left: 0, top: 0, right: 960, bottom: 540 };
    let mon = crate::monitors::Monitor { handle: 0, rect: bounds, work: bounds, primary: true, dpi: 96 };
    let desk = grid::Desk { cell: (80, 90), shown: true, ..Default::default() };
    let fit = grid::Fit { min: (note::MIN_W as i32, note::MIN_H as i32), inset: 4. };
    let r = grid::auto_place(&desk, &[mon], 0, (240, 200), &fit, &[]).unwrap();
    let p = place::from_px(r, &[mon]).unwrap();
    assert!(r.left > 600 && r.top < 30, "first card in the top-right grid cells: {r:?}");
    let mut disk = super::store::mem::MemDisk::default();
    super::store::load(&mut disk, 1000);
    assert!(super::store::seed_first(&mut disk, p, 1000));
    let loaded = super::store::load(&mut disk, 1001);
    assert_eq!(loaded.notes.len(), 1);
    let root = crate::gfx::promo_kit::harmony_dir().join("notes-entry");
    std::fs::create_dir_all(&root).unwrap();
    for light in [true, false] {
        let wall = crate::gfx::promo_kit::harmony_backdrop(light);
        let look = Look::of(&crate::gfx::promo_kit::theme(light));
        let sheet = Sheet::new(960, 540);
        sheet.fill(bounds, wall);
        let (w, h) = (r.right - r.left, r.bottom - r.top);
        let px = super::face::picture(w, h, &super::face::Face {
            text: &loaded.notes[0].1, date: None, look: &look, scale: 1.,
        }).expect("a face");
        sheet.blend(r.left, r.top, w, h, &px);
        sheet.save(&root.join(format!("desktop-{}.png", if light { "light" } else { "dark" })));
    }
}

fn save_png(path: &std::path::Path, w: u32, h: u32, px: &[u8]) {
    unsafe {
        let f: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).unwrap();
        let bmp = f.CreateBitmapFromMemory(w, h, &GUID_WICPixelFormat32bppPBGRA, w * 4, px).unwrap();
        let stream = f.CreateStream().unwrap();
        stream.InitializeFromFilename(&HSTRING::from(path.as_os_str()), GENERIC_WRITE.0).unwrap();
        let enc = f.CreateEncoder(&GUID_ContainerFormatPng, std::ptr::null::<GUID>()).unwrap();
        enc.Initialize(&stream, WICBitmapEncoderNoCache).unwrap();
        let mut fe: Option<IWICBitmapFrameEncode> = None;
        enc.CreateNewFrame(&mut fe, std::ptr::null_mut()).unwrap();
        let fe = fe.unwrap();
        fe.Initialize(None).unwrap();
        fe.SetSize(w, h).unwrap();
        let mut fmt = GUID_WICPixelFormat32bppPBGRA;
        fe.SetPixelFormat(&mut fmt).unwrap();
        fe.WriteSource(&bmp, std::ptr::null()).unwrap();
        fe.Commit().unwrap();
        enc.Commit().unwrap();
    }
}

/// A sheet to draw on: a 32-bit DIB in a memory DC.
struct Sheet {
    dc: HDC,
    bits: *mut u8,
    w: i32,
    h: i32,
    old: windows::Win32::Graphics::Gdi::HGDIOBJ,
    bmp: windows::Win32::Graphics::Gdi::HBITMAP,
}

impl Sheet {
    fn new(w: i32, h: i32) -> Sheet {
        unsafe {
            let dc = CreateCompatibleDC(None);
            let bi = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER { biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32, biWidth: w, biHeight: -h, biPlanes: 1, biBitCount: 32, biCompression: BI_RGB.0, ..Default::default() },
                ..Default::default()
            };
            let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
            let bmp = CreateDIBSection(Some(dc), &bi, DIB_RGB_COLORS, &mut bits, None, 0).unwrap();
            let old = SelectObject(dc, bmp.into());
            Sheet { dc, bits: bits as *mut u8, w, h, old, bmp }
        }
    }

    fn fill(&self, r: RECT, c: Rgba) {
        unsafe {
            let b = CreateSolidBrush(note::colorref(c));
            FillRect(self.dc, &r, b);
            let _ = DeleteObject(b.into());
        }
    }

    fn text(&self, x: i32, y: i32, t: &str, c: Rgba) {
        unsafe {
            SetBkMode(self.dc, TRANSPARENT);
            SetTextColor(self.dc, note::colorref(c));
            let mut w: Vec<u16> = t.encode_utf16().collect();
            let mut r = RECT { left: x, top: y, right: x + 600, bottom: y + 30 };
            DrawTextW(self.dc, &mut w, &mut r, DT_LEFT | DT_SINGLELINE | DT_NOPREFIX);
        }
    }

    /// Premultiplied BGRA `px` (`w` x `h`) over the sheet at (x, y).
    fn blend(&self, x: i32, y: i32, w: i32, h: i32, px: &[u8]) {
        for j in 0..h {
            for i in 0..w {
                let (sx, sy) = (x + i, y + j);
                if sx < 0 || sy < 0 || sx >= self.w || sy >= self.h {
                    continue;
                }
                let s = ((j * w + i) * 4) as usize;
                let d = ((sy * self.w + sx) * 4) as usize;
                let a = px[s + 3] as u32;
                for c in 0..3 {
                    unsafe {
                        let dst = self.bits.add(d + c);
                        *dst = (px[s + c] as u32 + *dst as u32 * (255 - a) / 255).min(255) as u8;
                    }
                }
            }
        }
    }

    /// (B, G, R) at (x, y).
    fn at(&self, x: i32, y: i32) -> [u8; 3] {
        let i = ((y * self.w + x) * 4) as usize;
        unsafe { [*self.bits.add(i), *self.bits.add(i + 1), *self.bits.add(i + 2)] }
    }

    /// Opaque (GDI leaves alpha at 0), then saved.
    fn save(&self, path: &std::path::Path) -> Vec<u8> {
        let n = (self.w * self.h * 4) as usize;
        let px = unsafe { std::slice::from_raw_parts_mut(self.bits, n) };
        for a in px.iter_mut().skip(3).step_by(4) {
            *a = 255;
        }
        save_png(path, self.w as u32, self.h as u32, px);
        px.to_vec()
    }
}

impl Drop for Sheet {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.dc, self.old);
            let _ = DeleteObject(self.bmp.into());
            let _ = DeleteDC(self.dc);
        }
    }
}

/// Note window `w` (hidden) drawn onto `sheet` at (x, y): its offscreen
/// picture (the same card it shows), its open editor's text included.
fn print(sheet: &Sheet, w: HWND, x: i32, y: i32) {
    let (pw, ph, px) = note::picture(w).expect("a note picture");
    sheet.blend(x, y, pw, ph, &px);
}

fn rgb(c: Rgba) -> [u8; 3] {
    [c.b, c.g, c.r]
}

fn draw(name: &str, l: &Look, wall: Rgba) {
    note::set_look(l.clone());
    let g = |b: u8| [b; 16];
    let (a, b) = (guid_text(&g(1)), guid_text(&g(2)));
    // The notes: one per desktop, one on every desktop.
    let notes = [
        ("s1", "업무\n- [ ] 주간 보고서\n- [x] 3시 회의", Some(a.clone())),
        ("s2", "게임\n- [ ] 레이드 9시\n- [x] 길드 출석", Some(b.clone())),
        ("s3", "공용: 비밀번호 바꾸기", None),
    ];
    let off = RECT { left: -32000, top: -32000, right: -32000 + NOTE_W, bottom: -32000 + NOTE_H };
    let wins: Vec<HWND> = notes.iter().map(|(id, md, _)| note::create(id, off, &check::to_display(md)).expect("a note window")).collect();
    let pane_w = 3 * NOTE_W + 4 * GAP;
    let pane_h = HEAD + NOTE_H + 2 * GAP + 60;
    let sheet = Sheet::new(2 * pane_w + GAP, pane_h);
    sheet.fill(RECT { left: 0, top: 0, right: sheet.w, bottom: sheet.h }, l.bg);
    let mut slots = Vec::new();
    for (k, (label, cur)) in [("데스크톱 1 (업무)", 0usize), ("데스크톱 2 (게임)", 1usize)].into_iter().enumerate() {
        let x0 = k as i32 * (pane_w + GAP);
        sheet.fill(RECT { left: x0, top: HEAD, right: x0 + pane_w, bottom: pane_h }, wall);
        sheet.text(x0 + GAP, 10, label, l.text);
        let d = Desks::of(Some(&Sure { ids: vec![g(1), g(2)], current: cur }), true);
        for (i, (_, _, home)) in notes.iter().enumerate() {
            let (x, y) = (x0 + GAP + i as i32 * (NOTE_W + GAP), HEAD + GAP);
            let shows = d.shows(home.as_deref());
            if shows {
                print(&sheet, wins[i], x, y);
            }
            slots.push((x, y, shows));
        }

    }
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("target").join("notes-render");
    let _ = std::fs::create_dir_all(&dir);
    sheet.save(&dir.join(format!("{name}.png")));
    // Each desktop: its own note and the shared one, not the other's.
    for (k, &(x, y, shows)) in slots.iter().enumerate() {
        let px = sheet.at(x + NOTE_W / 2, y + NOTE_H - 6);
        if shows {
            assert_ne!(px,rgb(wall),"{name}: note {k} drawn");
            let spread = px.iter().max().unwrap() - px.iter().min().unwrap();
            assert!(spread <= 3, "{name}: neutral glass {px:?}");
        } else {
            assert_eq!(px, rgb(wall), "{name}: note {k} hidden");
        }
    }
    // The printed notes carry their text (not just the card).
    let (x, y, _) = slots[0];
    let inked = (y + 24..y + 60).any(|yy| (x + 12..x + 120).any(|xx| sheet.at(xx, yy) != rgb(l.bg)));
    assert!(inked, "{name}: the note's text is printed");
    for w in wins {
        note::destroy(w);
        unsafe {
            let _ = DestroyWindow(w);
        }
    }
}

fn resolved(light: bool) -> Look {
    let sys = crate::syslook::SysLook { light, accent: Rgba { r: 0, g: 120, b: 215, a: 255 }, transparency: true, high_contrast: None, animations: true, ui_font: 0 };
    Look::of(&crate::theme::Theme::graphite().resolve_fx(&sys, false))
}

/// One picture: the faces of a few notes (and one being edited) on a
/// fake wallpaper, in one theme and one state.
fn faces(name: &str, light: bool, quiet: bool, lang: &str) {
    let l = resolved(light);
    note::set_look(l.clone());
    note::set_vivid(!quiet);
    let wall = if light { Rgba { r: 178, g: 196, b: 226, a: 255 } } else { Rgba { r: 34, g: 44, b: 72, a: 255 } };
    let ko = lang == "ko";
    let notes: [(&str, (i32, i32)); 4] = if ko {
        [
            ("장보기\n- [ ] 우유\n- [x] 빵\n- [ ] 달걀 한 판", (144, 144)),
            ("비밀번호 바꾸기", (144, 144)),
            ("회의 메모\n다음 주 화요일까지 디자인 시안 두 개를 보내기로 했어요. 하나는 밝은 버전, 하나는 어두운 버전. 위젯은 애플처럼 조용하게.", (296, 144)),
            ("주간 계획\n월: 보드 크기 3종 마무리\n화: 메모 위젯 날짜 표시\n수: 바탕화면 모드 점검\n목: 번역 42개 확인\n금: 0.1.51 출시 준비\n주말: 쉬기", (296, 296)),
        ]
    } else {
        [
            ("Groceries\n- [ ] Milk\n- [x] Bread\n- [ ] A dozen eggs", (144, 144)),
            ("Change the password", (144, 144)),
            ("Meeting notes\nWe agreed to send two design drafts by next Tuesday: one light, one dark. Widgets stay quiet, like Apple's.", (296, 144)),
            ("This week\nMon: finish the three board sizes\nTue: dates on the note widget\nWed: check the desktop look\nThu: review 42 translations\nFri: get 0.1.51 ready\nWeekend: rest", (296, 296)),
        ]
    };
    let gap = 24;
    let total_w = gap + notes.iter().map(|(_, (w, _))| w + gap).sum::<i32>();
    let sheet = Sheet::new(total_w, 296 + 2 * gap);
    // A soft wallpaper: the tint the faces lean to.
    for y in 0..sheet.h {
        let k = y as f32 / sheet.h as f32;
        let c = Rgba { r: (wall.r as f32 * (1.05 - 0.15 * k)).min(255.0) as u8, g: (wall.g as f32 * (1.05 - 0.15 * k)).min(255.0) as u8, b: (wall.b as f32 * (1.05 - 0.1 * k)).min(255.0) as u8, a: 255 };
        sheet.fill(RECT { left: 0, top: y, right: sheet.w, bottom: y + 1 }, c);
    }
    let mut x = gap;
    // A fixed day for most, today for one (its time shows).
    let day = super::note::days(2026, 11, 8) * 86_400 + 10 * 3600;
    let mut wins = Vec::new();
    for (k, (md, (w, h))) in notes.iter().enumerate() {
        let off = RECT { left: -32000, top: -32000, right: -32000 + w, bottom: -32000 + h };
        let win = note::create(&format!("f{k}"), off, &check::to_display(md)).expect("a note window");
        if k == 1 {
            note::set_date(win, std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64);
        } else {
            note::set_date(win, day);
        }
        // The large one is being typed in (its editor shows), in front only.
        if k == 3 && !quiet {
            note::set_test_active(win, true);
        }
        print(&sheet, win, x, gap);
        x += w + gap;
        wins.push(win);
    }
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("target").join("ui-snapshots").join("notes");
    let _ = std::fs::create_dir_all(&dir);
    sheet.save(&dir.join(format!("{name}-{lang}.png")));
    for w in wins {
        note::destroy(w);
    }
    note::set_vivid(false);
}

#[test]
#[ignore = "writes PNGs to look at: cargo test -p dock notes::shots -- --ignored"]
fn render_note_faces() {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        }
        let r = std::panic::catch_unwind(|| {
            for lang in ["ko", "en"] {
                let _l = crate::i18n::test_lock(lang);
                for light in [true, false] {
                    for quiet in [true, false] {
                        let name = format!("{}-{}", if quiet { "desk" } else { "front" }, if light { "light" } else { "dark" });
                        faces(&name, light, quiet, lang);
                    }
                }
            }
        });
        let _ = tx.send(r.err().map(|e| e.downcast_ref::<String>().cloned().or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string())).unwrap_or_default()));
    });
    if let Some(msg) = rx.recv().expect("the drawing thread answers") {
        panic!("{msg}");
    }
}

#[test]
fn the_notes_of_two_desktops_off_screen() {
    let _l = crate::i18n::test_lock("ko");
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        }
        let r = std::panic::catch_unwind(|| {
            let dark = Look::default();
            draw("desktops-dark", &dark, Rgba { r: 20, g: 22, b: 26, a: 255 });
            let light = Look {
                bg: Rgba { r: 250, g: 250, b: 250, a: 255 },
                text: Rgba { r: 28, g: 28, b: 30, a: 255 },
                dim: Rgba { r: 120, g: 120, b: 125, a: 255 },
                light: true,
                ..Look::default()
            };
            draw("desktops-light", &light, Rgba { r: 196, g: 200, b: 206, a: 255 });
        });
        let _ = tx.send(r.err().map(|e| e.downcast_ref::<String>().cloned().or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string())).unwrap_or_default()));
    });
    if let Some(msg) = rx.recv().expect("the drawing thread answers") {
        panic!("{msg}");
    }
}

/// The notes of the promo film (see crate::gfx::promo_kit): each note's
/// face painted by face::paint at twice its size, as it shows in front
/// (not leaning to a wallpaper), with a fixed day. The card's corners are
/// DWM's on screen: the film rounds them by `notes.json`'s radius.
#[test]
#[ignore = "writes the promo film's pictures: cargo test -p dock promo_ -- --ignored"]
fn promo_notes() {
    crate::gfx::promo_kit::each_lang(promo_note_pictures);
}

fn promo_note_pictures() {
    use crate::gfx::promo_kit as kit;
    use crate::i18n::date;
    // A note changed today shows the time, an older one its day
    // (note::date_text): a fixed afternoon, the day before the bar's.
    let then = windows::Win32::Foundation::SYSTEMTIME { wYear: 2026, wMonth: 11, wDayOfWeek: 0, wDay: 8, wHour: 14, wMinute: 12, wSecond: 0, wMilliseconds: 0 };
    let (time, day) = (date::time(&then), date::month_day(&then));
    let notes: [(&str, &str, &str, (i32, i32)); 2] = [
        ("list", kit::say("장보기\n- [ ] 우유\n- [x] 빵\n- [ ] 달걀 한 판", "Groceries\n- [ ] Milk\n- [x] Bread\n- [ ] A dozen eggs"), &time, (168, 168)),
        ("week", kit::say("이번 주\n월: 시안 두 개 보내기\n수: 번역 확인\n금: 새 버전 준비", "This week\nMon: send two drafts\nWed: check the translation\nFri: prepare the new version"), &day, (264, 168)),
    ];
    let s = kit::scale();
    let mut looks = serde_json::Map::new();
    for (folder, light) in kit::LOOKS {
        let l = Look::of(&kit::theme(light));
        for (name, md, date, (w, h)) in notes {
            let (pw, ph) = ((w as f32 * s) as i32, (h as f32 * s) as i32);
            let sheet = Sheet::new(pw, ph);
            let text = check::to_display(md);
            let face = super::face::Face { text: &text, date: Some(date.to_string()), look: &l, scale: s };
            sheet.blend(0, 0, pw, ph, &super::face::picture(pw, ph, &face).expect("a face"));
            let path = kit::out_dir().join(folder).join(format!("note-{name}.png"));
            let _ = std::fs::create_dir_all(path.parent().unwrap());
            unsafe {
                let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            }
            sheet.save(&path);
        }
        looks.insert(folder.into(), serde_json::json!({ "radius": l.radius, "bg": kit::css(l.bg), "shadows": kit::shadows_json(&kit::theme(light).elevation(crate::theme::Elevation::E2)) }));
    }
    let sizes: Vec<serde_json::Value> = notes.iter().map(|(name, _, _, (w, h))| serde_json::json!({ "id": name, "w": w, "h": h })).collect();
    kit::save_json("notes.json", &serde_json::json!({ "scale": s, "notes": sizes, "looks": looks }));
    assert!(kit::out_dir().join("light/note-list.png").is_file());
}
