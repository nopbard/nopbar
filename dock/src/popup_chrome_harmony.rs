//! popup::chrome_at's own visuals read back for the design audit (tests
//! only; see crate::gfx::promo_kit, "harmony"). The other audit pictures
//! draw a plate the way chrome_at stacks it (promo_kit::draw_plate); this
//! one builds the real thing (the contact ring, the DropShadows with their
//! converted blur, the tint of the elevation's material, the gradient rim)
//! and reads that visual tree alone off screen
//! (GraphicsCaptureItem::CreateFromVisual: nothing of the screen, no
//! window shown). A menu (E2) beside a panel (E3) at 150 %, the panel at
//! full strength and with its light half up (as motion::open has it part
//! of the way), in both palettes.

use windows::core::Interface;
use windows::Graphics::Capture::{Direct3D11CaptureFramePool, GraphicsCaptureItem};
use windows::Graphics::DirectX::Direct3D11::IDirect3DDevice;
use windows::Graphics::DirectX::DirectXPixelFormat;
use windows::Graphics::SizeInt32;
use windows::UI::Composition::{ContainerVisual, Visual};
use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE, D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP};
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D, D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAPPED_SUBRESOURCE, D3D11_MAP_READ, D3D11_SDK_VERSION,
    D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
};
use windows::Win32::Graphics::Dxgi::IDXGIDevice;
use windows::Win32::System::WinRT::Composition::ICompositorDesktopInterop;
use windows::Win32::System::WinRT::Direct3D11::{CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess};
use windows_numerics::{Vector2, Vector3};

use super::chrome_at;
use crate::gfx::promo_kit as kit;
use crate::theme::Elevation;

/// The last frame of visual `v` (`w` x `h` px) once it is still: BGRA.
pub(crate) unsafe fn read_back(v: &ContainerVisual, w: i32, h: i32) -> Option<Vec<u8>> {
    let make = |kind: D3D_DRIVER_TYPE| -> windows::core::Result<(ID3D11Device, ID3D11DeviceContext)> {
        let (mut dev, mut ctx) = (None, None);
        D3D11CreateDevice(None, kind, HMODULE::default(), D3D11_CREATE_DEVICE_BGRA_SUPPORT, None, D3D11_SDK_VERSION, Some(&mut dev), None, Some(&mut ctx))?;
        Ok((dev.unwrap(), ctx.unwrap()))
    };
    let (device, ctx) = make(D3D_DRIVER_TYPE_HARDWARE).or_else(|_| make(D3D_DRIVER_TYPE_WARP)).ok()?;
    let dxgi: IDXGIDevice = device.cast().ok()?;
    let d3d: IDirect3DDevice = CreateDirect3D11DeviceFromDXGIDevice(&dxgi).ok()?.cast().ok()?;
    let item = GraphicsCaptureItem::CreateFromVisual(&v.cast::<Visual>().ok()?).ok()?;
    let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(&d3d, DirectXPixelFormat::B8G8R8A8UIntNormalized, 2, SizeInt32 { Width: w, Height: h }).ok()?;
    let session = pool.CreateCaptureSession(&item).ok()?;
    session.StartCapture().ok()?;
    let mut last = None;
    let end = std::time::Instant::now() + std::time::Duration::from_millis(500);
    while std::time::Instant::now() < end {
        // The compositor commits from this thread's queue: let it run.
        let mut msg = windows::Win32::UI::WindowsAndMessaging::MSG::default();
        while windows::Win32::UI::WindowsAndMessaging::PeekMessageW(&mut msg, None, 0, 0, windows::Win32::UI::WindowsAndMessaging::PM_REMOVE).as_bool() {
            windows::Win32::UI::WindowsAndMessaging::DispatchMessageW(&msg);
        }
        let Ok(frame) = pool.TryGetNextFrame() else {
            std::thread::sleep(std::time::Duration::from_millis(4));
            continue;
        };
        let px = (|| -> windows::core::Result<Vec<u8>> {
            let access: IDirect3DDxgiInterfaceAccess = frame.Surface()?.cast()?;
            let tex: ID3D11Texture2D = access.GetInterface()?;
            let mut desc = D3D11_TEXTURE2D_DESC::default();
            tex.GetDesc(&mut desc);
            desc.Usage = D3D11_USAGE_STAGING;
            desc.BindFlags = 0;
            desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ.0 as u32;
            desc.MiscFlags = 0;
            let mut staging = None;
            device.CreateTexture2D(&desc, None, Some(&mut staging))?;
            let staging = staging.unwrap();
            ctx.CopyResource(&staging, &tex);
            let mut map = D3D11_MAPPED_SUBRESOURCE::default();
            ctx.Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut map))?;
            let mut out = Vec::with_capacity((w * h * 4) as usize);
            for y in 0..h as usize {
                out.extend_from_slice(std::slice::from_raw_parts((map.pData as *const u8).add(y * map.RowPitch as usize), (w * 4) as usize));
            }
            ctx.Unmap(&staging, 0);
            Ok(out)
        })();
        let _ = frame.Close();
        if let Ok(px) = px {
            last = Some(px);
        }
    }
    let _ = session.Close();
    let _ = pool.Close();
    last
}

#[test]
#[ignore = "writes the design audit's pictures: cargo test -p dock harmony_ -- --ignored"]
fn harmony_chrome_composition() {
    let stage = kit::stage();
    let gfx = &stage.gfx;
    let c = &gfx.compositor;
    let s = 1.5f32;
    let m = crate::theme::metrics::SHADOW_M;
    // (menu, panel, panel with its light half up), each in SHADOW_M of room.
    let plates = [(160.0f32, 120.0f32, Elevation::E2, 1.0f32), (203.0, 117.0, Elevation::E3, 1.0), (203.0, 117.0, Elevation::E3, 0.5)];
    let (w, h) = (plates.iter().map(|p| p.0 + m * 2.0).sum::<f32>(), 120.0 + m * 2.0);
    let (w_px, h_px) = ((w * s).ceil() as i32, (h * s).ceil() as i32);
    unsafe {
        let interop: ICompositorDesktopInterop = c.cast().unwrap();
        // The stage's window is never shown: only what the visuals hang on.
        let target = interop.CreateDesktopWindowTarget(stage.hwnd, false).unwrap();
        let root = c.CreateContainerVisual().unwrap();
        target.SetRoot(&root).unwrap();
        for (folder, light) in kit::LOOKS {
            let theme = kit::theme(light);
            // DWM's blur stands in as the picture's backdrop (glass::StandIn).
            let _ground = crate::glass::StandIn::new(kit::harmony_backdrop(light));
            let r = theme.radii();
            root.Children().unwrap().RemoveAll().unwrap();
            let scene = c.CreateContainerVisual().unwrap();
            scene.SetSize(Vector2 { X: w_px as f32, Y: h_px as f32 }).unwrap();
            root.Children().unwrap().InsertAtTop(&scene).unwrap();
            let back = c.CreateSpriteVisual().unwrap();
            let bg = kit::harmony_backdrop(light);
            back.SetBrush(&c.CreateColorBrushWithColor(windows::UI::Color { A: 255, R: bg.r, G: bg.g, B: bg.b }).unwrap()).unwrap();
            back.SetSize(Vector2 { X: w_px as f32, Y: h_px as f32 }).unwrap();
            scene.Children().unwrap().InsertAtTop(&back).unwrap();
            let mut x = 0.0f32;
            for (pw, ph, e, strength) in plates {
                // As every panel does: outer scaled, offset by the margin.
                let outer = c.CreateContainerVisual().unwrap();
                outer.SetScale(Vector3 { X: s, Y: s, Z: 1.0 }).unwrap();
                outer.SetOffset(Vector3 { X: ((x + m) * s).round(), Y: (m * s).round(), Z: 0.0 }).unwrap();
                scene.Children().unwrap().InsertAtTop(&outer).unwrap();
                let radius = if e == Elevation::E2 { r.menu } else { r.panel };
                let (plate_root, rim) = chrome_at(gfx, &theme, &outer, pw, ph, s, radius, e).expect("the plate");
                // The light's strength: the marked visuals' opacity.
                if strength < 1.0 {
                    rim.SetOpacity(strength).unwrap();
                    for k in outer.Children().unwrap() {
                        if k.Comment().is_ok_and(|t| t == crate::motion::TAG_SHADE) {
                            k.SetOpacity(strength).unwrap();
                        }
                    }
                }
                let _ = plate_root;
                x += pw + m * 2.0;
            }
            let px = read_back(&scene, w_px, h_px).unwrap_or_else(|| panic!("chrome-live: the visual could not be read back: not drawn"));
            kit::save_png(&px, w_px as u32, h_px as u32, &kit::harmony_dir().join(folder).join("chrome-live.png"));
            // The menu's rim row is lighter than its tint; the row above
            // it (the contact ring over the backdrop) darker than the
            // backdrop far from any plate.
            let at = |xx: i32, yy: i32| px[((yy * w_px + xx) * 4 + 1) as usize] as i32;
            let (mx, top) = (((m + 80.0) * s) as i32, (m * s).round() as i32);
            let tint = at(mx, top + 20);
            // (No rim on either finish since glass A, 2026-11-12: the top
            // row is the plate's own, a glass's tone at most a level or two
            // over the row under it.)
            assert!((at(mx, top) - tint).abs() <= 4, "no lit rim on the top row: {} vs {}", at(mx, top), tint);
            assert!(at(mx, top - 1) < at(2, 2), "the contact ring is darker than the backdrop: {} vs {}", at(mx, top - 1), at(2, 2));
            // Half strength: the plate as it is, less light under it.
            let (p_full, p_half) = ((((160.0 + m * 2.0) + m + 100.0) * s) as i32, (((160.0 + m * 2.0) + (203.0 + m * 2.0) + m + 100.0) * s) as i32);
            let under = ((m + 117.0 + 10.0) * s) as i32;
            // (Flat v2's grain differs a code value or two from one pixel to
            // the next.)
            assert!((at(p_full, top + 30) - at(p_half, top + 30)).abs() <= 3, "the plate does not fade with its light: {} vs {}", at(p_full, top + 30), at(p_half, top + 30));
            assert!(at(p_half, under) > at(p_full, under), "half the shadow: {} vs {}", at(p_half, under), at(p_full, under));
        }
        drop(target);
    }
}

/// The panels' confirm review (2026-11-11, P2): a band that is off showed
/// the rows twice (a masked copy under the plain one), so a see-through
/// fill under it came out stronger than in the middle. The real
/// popup::parts::Edges over a see-through fill (search's selection, 0.14)
/// is read back off screen: with the bands off every row of it has the
/// middle's alpha, the bottom band included; with them on the alpha
/// climbs from the edge to the middle's and is never over it; and turned
/// off again (the menus' fade, then still) it is whole once more.
#[test]
#[ignore = "reads the compositor back: cargo test -p dock harmony_ -- --ignored"]
fn harmony_edges_composite_once() {
    use crate::popup::parts::Edges;
    let stage = kit::stage();
    let gfx = &stage.gfx;
    let c = &gfx.compositor;
    let (w, h, band, s) = (200.0f32, 120.0f32, crate::theme::metrics::size::SCROLL_FADE, 2.0f32);
    let (w_px, h_px) = ((w * s).ceil() as i32, (h * s).ceil() as i32);
    unsafe {
        let interop: ICompositorDesktopInterop = c.cast().unwrap();
        let target = interop.CreateDesktopWindowTarget(stage.hwnd, false).unwrap();
        let root = c.CreateContainerVisual().unwrap();
        root.SetSize(Vector2 { X: w_px as f32, Y: h_px as f32 }).unwrap();
        target.SetRoot(&root).unwrap();
        for (_, light) in kit::LOOKS {
            let theme = kit::theme(light);
            root.Children().unwrap().RemoveAll().unwrap();
            let holder = c.CreateContainerVisual().unwrap();
            holder.SetScale(Vector3 { X: s, Y: s, Z: 1.0 }).unwrap();
            root.Children().unwrap().InsertAtTop(&holder).unwrap();
            let view = c.CreateContainerVisual().unwrap();
            view.SetSize(Vector2 { X: w, Y: h }).unwrap();
            let fill = c.CreateSpriteVisual().unwrap();
            fill.SetSize(Vector2 { X: w, Y: h }).unwrap();
            fill.SetBrush(&c.CreateColorBrushWithColor(windows::UI::Color { A: 36, R: 0, G: 0, B: 0 }).unwrap()).unwrap();
            view.Children().unwrap().InsertAtTop(&fill).unwrap();
            let mut e = Edges::new(gfx, &theme, &view, (w, h, band), s, (false, false)).expect("the edges");
            holder.Children().unwrap().InsertAtTop(&e.stage).unwrap();
            let alpha = |px: &[u8], dip: f32| px[(((dip * s) as i32).clamp(0, h_px - 1) * w_px + w_px / 2) as usize * 4 + 3] as i32;
            let read = || read_back(&root, w_px, h_px).expect("read back");
            let px = read();
            let mid = alpha(&px, h / 2.0);
            assert!((30..=42).contains(&mid), "{light}: the fill's own alpha: {mid}");
            for dip in [0.25, 2.0, 6.0, band - 0.5, h - band + 0.5, h - 6.0, h - 2.0, h - 0.25] {
                assert_eq!(alpha(&px, dip), mid, "{light}: bands off, at {dip} DIP: the middle's alpha");
            }
            e.jump((true, true), theme.high_contrast);
            let px = read();
            for (edge, inner) in [(1.0, band - 2.0), (h - 1.0, h - band + 2.0)] {
                let (a0, a1) = (alpha(&px, edge), alpha(&px, inner));
                assert!(a0 < a1 && a1 <= mid, "{light}: across the band {a0} < {a1} <= {mid}");
            }
            assert!((0..h_px).all(|y| alpha(&px, y as f32 / s) <= mid), "{light}: never stronger than the middle");
            // The menus turn them with Edges::set (a fade): once it is
            // over, as at first.
            e.set(c, (false, false), theme.high_contrast);
            let px = read();
            for dip in [0.25, 6.0, h - 6.0, h - 0.25] {
                assert_eq!(alpha(&px, dip), mid, "{light}: off again, at {dip} DIP");
            }
        }
        drop(target);
    }
}

/// popup::chrome_live (the control center's plate, final audit 5): built
/// 240 DIP tall, its "h" set to 140, read back: the plate (tint, rim,
/// contact ring) ends at 140 and the backdrop is under it past that, as
/// the plate built 140 tall has it; set back to 240 it is whole again.
#[test]
#[ignore = "reads the compositor back: cargo test -p dock harmony_ -- --ignored"]
fn harmony_a_live_plate_follows_its_height() {
    use windows::core::HSTRING;
    let stage = kit::stage();
    let gfx = &stage.gfx;
    let c = &gfx.compositor;
    let (s, m) = (1.5f32, crate::theme::metrics::SHADOW_M);
    let (pw, tall, short) = (200.0f32, 240.0f32, 140.0f32);
    let (w_px, h_px) = (((pw + m * 2.0) * s).ceil() as i32, ((tall + m * 2.0) * s).ceil() as i32);
    unsafe {
        let interop: ICompositorDesktopInterop = c.cast().unwrap();
        let target = interop.CreateDesktopWindowTarget(stage.hwnd, false).unwrap();
        let root = c.CreateContainerVisual().unwrap();
        target.SetRoot(&root).unwrap();
        for (_, light) in kit::LOOKS {
            let theme = kit::theme(light);
            let _ground = crate::glass::StandIn::new(kit::harmony_backdrop(light));
            root.Children().unwrap().RemoveAll().unwrap();
            let scene = c.CreateContainerVisual().unwrap();
            scene.SetSize(Vector2 { X: w_px as f32, Y: h_px as f32 }).unwrap();
            root.Children().unwrap().InsertAtTop(&scene).unwrap();
            let back = c.CreateSpriteVisual().unwrap();
            let bg = kit::harmony_backdrop(light);
            back.SetBrush(&c.CreateColorBrushWithColor(windows::UI::Color { A: 255, R: bg.r, G: bg.g, B: bg.b }).unwrap()).unwrap();
            back.SetSize(Vector2 { X: w_px as f32, Y: h_px as f32 }).unwrap();
            scene.Children().unwrap().InsertAtTop(&back).unwrap();
            let outer = c.CreateContainerVisual().unwrap();
            outer.SetScale(Vector3 { X: s, Y: s, Z: 1.0 }).unwrap();
            outer.SetOffset(Vector3 { X: (m * s).round(), Y: (m * s).round(), Z: 0.0 }).unwrap();
            scene.Children().unwrap().InsertAtTop(&outer).unwrap();
            let h = c.CreatePropertySet().unwrap();
            super::chrome_live(gfx, &theme, &outer, pw, tall, s, theme.radii().panel, &h).expect("the plate");
            let at = |px: &[u8], dip_y: f32| px[((((m + dip_y) * s) as i32 * w_px + ((m + pw / 2.0) * s) as i32) * 4 + 1) as usize] as i32;
            // A check run on purpose (--ignored) that cannot read what it
            // checks fails: it never passes having looked at nothing (final
            // review 4).
            let full = read_back(&scene, w_px, h_px).unwrap_or_else(|| panic!("light {light}: the compositor could not be read back: nothing was checked"));
            h.InsertScalar(&HSTRING::from("h"), short).unwrap();
            let cut = read_back(&scene, w_px, h_px).expect("read back");
            // Inside both: the same plate, but for a glass's tone (it falls
            // down the whole of whatever height the plate has: glass::
            // SPEC_LINE). Past the short one's bottom (and its shadows: well
            // under it): the plate in the tall one, not in the short one.
            // The tone's white over the plate `p` differs by `spec * (255 -
            // p)` times the difference in how far down each point lies.
            let spec = theme.glass_fx(crate::theme::Material::Panel).map_or(0.0, |f| f.spec);
            let tone = |p: i32, share: f32| (spec * (255 - p) as f32 * share.abs()).ceil() as i32;
            let inside = 110.0;
            let p = at(&full, inside);
            assert!((p - at(&cut, inside)).abs() <= 3 + tone(p, inside / short - inside / tall), "light {light}: {} vs {}", p, at(&cut, inside));
            let past = short + 60.0;
            assert!((at(&full, past) - p).abs() <= 6 + tone(p, (past - inside) / tall), "light {light}: the tall plate goes on");
            assert!((at(&cut, past) - at(&cut, inside)).abs() > 6, "light {light}: the short plate ends at its height: {} vs {}", at(&cut, past), at(&cut, inside));
            assert!((at(&cut, past) - bg.g as i32).abs() <= 8, "light {light}: the backdrop past it: {} vs {}", at(&cut, past), bg.g);
            h.InsertScalar(&HSTRING::from("h"), tall).unwrap();
            let again = read_back(&scene, w_px, h_px).expect("read back");
            assert!((at(&again, past) - at(&full, past)).abs() <= 3, "light {light}: whole again");
        }
        drop(target);
    }
}
