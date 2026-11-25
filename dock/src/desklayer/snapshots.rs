//! Acceptance images, synthetic fixtures and hidden Composition targets only.
use super::{
    card,
    widget::{self, Size},
};
use crate::{
    device_battery::{
        model::{Desktop, Device, Kind},
        view::View,
    },
    gfx::promo_kit as kit,
    notes::{inbox::Row, widgets},
};
use windows::{
    Win32::{Foundation::*, UI::WindowsAndMessaging::DefWindowProcW},
    UI::Color,
};
use windows_numerics::Vector2;
unsafe extern "system" fn hidden(h: HWND, m: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    DefWindowProcW(h, m, w, l)
}
fn devices() -> Vec<Device> {
    [
        ("키보드", 50, Kind::Keyboard),
        ("마우스", 95, Kind::Mouse),
        ("이어폰", 18, Kind::Headphones),
        ("게임패드", 80, Kind::Gamepad),
        ("키보드 2", 75, Kind::Keyboard),
        ("마우스 2", 40, Kind::Mouse),
        ("헤드폰", 60, Kind::Headphones),
        ("컨트롤러", 100, Kind::Gamepad),
    ]
    .into_iter()
    .enumerate()
    .map(|(i, (name, p, kind))| Device {
        id: i.to_string(),
        name: name.into(),
        kind,
        percent: Some(p),
        connected: true,
        charging: i == 4,
        ..Default::default()
    })
    .collect()
}
fn widget_matrix() {
    let _lang = crate::i18n::test_lock("ko");
    let stage = kit::stage();
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../target/ui-snapshots/widget-system/stage1");
    std::fs::create_dir_all(&root).unwrap();
    let mut rows: Vec<_> = [
        "회의 자료 출력",
        "택배 반품 신청",
        "세금계산서 확인",
        "원두 주문",
        "예약 확인",
        "장보기",
        "회신",
        "산책",
    ]
    .into_iter()
    .map(|s| Row::new(s.into(), 0, None, 0, None))
    .collect();
    rows[0].remind = Some(54000);
    rows[2].completed = Some(0);
    let devices = devices();
    for light in [false, true] {
        for dpi in [1., 1.5] {
            for size in Size::ALL {
                for kind in ["note", "inbox", "battery"] {
                    for state in if kind == "battery" && size == Size::SMALL {
                        vec!["normal", "empty", "hover", "single"]
                    } else {
                        vec!["normal", "empty", "hover"]
                    } {
                        let theme = kit::theme(light);
                        // Non-square Explorer spacing, scaled with the monitor.
                        let g = super::grid::Grid {
                            ox: 0,
                            oy: 0,
                            cw: (76. * dpi) as i32,
                            ch: (96. * dpi) as i32,
                            cols: 20,
                            rows: 12,
                            ..Default::default()
                        };
                        let px = size.pixels(&g, dpi);
                        let dimensions = (px.0 as f32 / dpi, px.1 as f32 / dpi);
                        let backdrop = kit::harmony_backdrop(light);
                        let _stand = crate::glass::StandIn::new(backdrop);
                        let view = unsafe {
                            View::sized(
                                stage.gfx.clone(),
                                &theme,
                                size,
                                dimensions,
                                dpi,
                                Some(hidden),
                            )
                            .unwrap()
                        };
                        let m = 0;
                        let (pw, ph) = (view.glass.card.0 + 2 * m, view.glass.card.1 + 2 * m);
                        let bg = stage.gfx.compositor.CreateSpriteVisual().unwrap();
                        bg.SetSize(Vector2 {
                            X: pw as f32,
                            Y: ph as f32,
                        })
                        .unwrap();
                        bg.SetBrush(
                            &stage
                                .gfx
                                .compositor
                                .CreateColorBrushWithColor(Color {
                                    A: 255,
                                    R: backdrop.r,
                                    G: backdrop.g,
                                    B: backdrop.b,
                                })
                                .unwrap(),
                        )
                        .unwrap();
                        view.glass.root.Children().unwrap().InsertAtBottom(&bg).unwrap();
                        let visible: Vec<_> = if state == "empty" {
                            vec![]
                        } else {
                            (0..rows.len()).collect()
                        };
                        let inbox = widgets::Inbox {
                            rows: &rows,
                            remaining: 7,
                            visible: &visible,
                            hover: None,
                            flash: None,
                        };
                        let prefs = Desktop {
                            size,
                            ..Default::default()
                        };
                        let list: Vec<_> = if state == "empty" {
                            vec![]
                        } else {
                            devices
                                .iter()
                                .take(if state == "single" {
                                    1
                                } else if size == Size::SMALL {
                                    3
                                } else {
                                    8
                                })
                                .collect()
                        };
                        let ck=match kind {"note"=>card::Kind::Note,"inbox"=>card::Kind::Inbox,_=>card::Kind::Battery};
                        view.frame_at(&theme,ck,if state=="hover"{1.}else{0.},|dc,gfx,theme|unsafe {
            match kind {
                "note"=>card::note(dc,gfx,theme,if state=="empty"{""}else{"로그 레벨
낮추기 전에 팀에 공유
빌드 전에 서버 주소 운영으로 변경
원두, 두부, 대파, 계란
다음 주 일정 확인"},if state=="hover"{""}else{"11.16"},dimensions.0,dimensions.1)?,
                "inbox"=>widgets::inbox_content(dc,gfx,theme,&inbox,dimensions,if state=="hover"{1.}else{0.})?,
                _=>crate::device_battery::paint::content_sized(dc,gfx,theme,&prefs,&list,dimensions)?,
            }
            Ok(())
        }).unwrap();
                        let pixels =
                            unsafe { crate::popup::chrome_harmony::read_back(&view.glass.root, pw, ph) }
                                .unwrap();
                        kit::save_png(
                            &pixels,
                            pw as u32,
                            ph as u32,
                            &root.join(format!(
                                "{kind}-{size:?}-{state}-{}-{}.png",
                                if light { "light" } else { "dark" },
                                (dpi * 100.) as u32
                            )),
                        );
                    }
                }
            }
        }
    }
    println!("widget PNGs: {}", root.display());
}

fn widget_overlap() {
    let _lang = crate::i18n::test_lock("ko");
    let stage = kit::stage();
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../target/ui-snapshots/widget-system/stage1");
    for light in [false, true] {
        for after in [false, true] {
            let cv = kit::Canvas::of(stage.gfx.d2d_factory().clone(), 700., 480., 1.);
            let theme = kit::theme(light);
            let g = super::grid::Grid {
                ox: 24,
                oy: 24,
                cw: 76,
                ch: 96,
                cols: 8,
                rows: 4,
                ..Default::default()
            };
            let a = g.rect(
                super::grid::Span {
                    c: 4,
                    r: 0,
                    w: 2,
                    h: 2,
                },
                8,
            );
            let b = if after {
                widget::nearest_free(&g, a, (2, 2), 8, &[a]).unwrap()
            } else {
                a
            };
            for (r, label) in [(a, "배터리"), (b, "메모")] {
                unsafe {
                    cv.dc.SetTransform(&crate::companion::render::translate(
                        r.left as f32,
                        r.top as f32,
                    ));
                    card::plate(
                        &cv.dc,
                        &theme,
                        (r.right - r.left) as f32,
                        (r.bottom - r.top) as f32,
                        false,
                        None,
                    )
                    .unwrap();
                    card::note(
                        &cv.dc,
                        &stage.gfx,
                        &theme,
                        label,
                        "",
                        (r.right - r.left) as f32,
                        (r.bottom - r.top) as f32,
                    )
                    .unwrap();
                }
            }
            let (p, w, h) = cv.finish();
            kit::save_png(
                &p,
                w,
                h,
                &root.join(format!(
                    "overlap-{}-{}.png",
                    if after { "after" } else { "before" },
                    if light { "light" } else { "dark" }
                )),
            );
        }
    }
}

fn widget_editor(){
    use super::{edit::{Kind as WidgetKind,Preview},edit_view::{Scene,content}};
    use windows_numerics::Vector3;
    let _lang=crate::i18n::test_lock("ko");let stage=kit::stage();
    let root=std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/ui-snapshots/widget-system/stage2");std::fs::create_dir_all(&root).unwrap();
    let rows=vec![Row::new("회의 자료 출력".into(),0,None,0,None),Row::new("원두 주문".into(),0,None,0,None)];
    let note=Preview::Note("로그 레벨\n배포 전에 설정 확인\n다음 주 일정 정리".into(),"11.16".into());
    let battery=Preview::Battery(Desktop::default(),devices());
    let inbox=Preview::Inbox(rows,vec![0,1],2);
    for light in [false,true]{for dpi in [1.,1.5]{for kind in WidgetKind::ALL{for size in Size::ALL{
        let theme=kit::theme(light);let backdrop=kit::harmony_backdrop(light);let _stand=crate::glass::StandIn::new(backdrop);
        let (pw,ph)=((1280.*dpi) as i32,(900.*dpi) as i32);
        let monitor=crate::monitors::Monitor{handle:0,rect:RECT{left:0,top:0,right:pw,bottom:ph},work:RECT{left:0,top:0,right:pw,bottom:ph},primary:true,dpi:(96.*dpi) as u32};
        let surface=unsafe{super::edit_surface::Surface::new(stage.gfx.clone(),&theme,monitor,Some(hidden)).unwrap()};
        let bg=stage.gfx.compositor.CreateSpriteVisual().unwrap();bg.SetSize(Vector2{X:pw as f32,Y:ph as f32}).unwrap();bg.SetBrush(&stage.gfx.compositor.CreateColorBrushWithColor(Color{A:255,R:backdrop.r,G:backdrop.g,B:backdrop.b}).unwrap()).unwrap();surface._root.Children().unwrap().InsertAtBottom(&bg).unwrap();
        let cardrect=|x:f32,y:f32,w:f32,h:f32|RECT{left:(x*dpi) as i32,top:(y*dpi) as i32,right:((x+w)*dpi) as i32,bottom:((y+h)*dpi) as i32};
        let frames=[(1,cardrect(84.,104.,288.,176.)),(2,cardrect(84.,344.,136.,176.)),(3,cardrect(400.,104.,288.,176.))];
        // Fixture cards are real Composition cards inserted under the editor surface.
        let mut views=vec![];
        for ((_,r),preview) in frames.iter().zip([&note,&battery,&inbox]){
            let dims=((r.right-r.left) as f32/dpi,(r.bottom-r.top) as f32/dpi);let size=if dims.0<200.{Size::SMALL}else{Size::MEDIUM};
            let view=unsafe{View::sized(stage.gfx.clone(),&theme,size,dims,dpi,Some(hidden)).unwrap()};
            let ck=match preview.kind(){WidgetKind::Note=>card::Kind::Note,WidgetKind::Inbox=>card::Kind::Inbox,WidgetKind::Battery=>card::Kind::Battery};
            view.frame_at(&theme,ck,0.,|dc,gfx,theme|unsafe{content(dc,gfx,theme,preview,size,dims)}).unwrap();
            // Detach its visual from its hidden HWND, then use it in the synthetic scene.
            view.glass.detach();let m=0 as f32;
            view.glass.root.SetOffset(Vector3{X:r.left as f32-m,Y:r.top as f32-m,Z:0.}).unwrap();
            surface._root.Children().unwrap().InsertAbove(&view.glass.root,&bg).unwrap();views.push(view);
        }
        let preview=match kind{WidgetKind::Note=>&note,WidgetKind::Inbox=>&inbox,WidgetKind::Battery=>&battery};
        let notes=vec![("saved1".into(),"주말 장보기".into()),("saved2".into(),"여행 준비".into())];
        let mut scene=Scene{monitor,frames:&frames,slots:&[],ghost:Some(cardrect(388.,392.,288.,176.)),lift:Some(super::edit_view::Lifted{preview:&note,at:cardrect(430.,350.,288.,176.),size:Size::MEDIUM,dpi:monitor.scale(),t:1.}),gallery:true,kind,size,preview,preview_hwnd:Default::default(),notes:&notes,selected:None,chosen:None,quiet:false,places:&[]};
        surface.draw(&theme,&mut scene).unwrap();
        let pixels=unsafe{crate::popup::chrome_harmony::read_back(&surface._root,pw,ph)}.unwrap();
        kit::save_png(&pixels,pw as u32,ph as u32,&root.join(format!("editor-{kind:?}-{size:?}-{}-{}.png",if light{"light"}else{"dark"},(dpi*100.) as u32)));
        if kind==WidgetKind::Battery&&size==Size::SMALL{
            scene.gallery=false;scene.ghost=None;scene.lift=None;surface.draw(&theme,&mut scene).unwrap();
            let pixels=unsafe{crate::popup::chrome_harmony::read_back(&surface._root,pw,ph)}.unwrap();
            kit::save_png(&pixels,pw as u32,ph as u32,&root.join(format!("editor-grid-{}-{}.png",if light{"light"}else{"dark"},(dpi*100.) as u32)));
            // Shell entry must work even with no existing cards on this monitor.
            for view in &views {view.glass.root.SetIsVisible(false).unwrap();}
            scene.gallery=true;scene.frames=&[];surface.draw(&theme,&mut scene).unwrap();
            let pixels=unsafe{crate::popup::chrome_harmony::read_back(&surface._root,pw,ph)}.unwrap();
            kit::save_png(&pixels,pw as u32,ph as u32,&root.join(format!("editor-empty-gallery-{}-{}.png",if light{"light"}else{"dark"},(dpi*100.) as u32)));

        }
        drop(views);
    }}}}
}

/// Native Composition resources stay on one dispatcher/COM apartment for the whole audit.
#[test]
#[ignore="offscreen widget matrix, collision repair and editor on one native harness"]
fn widget_system(){
    widget_matrix();
    widget_overlap();
    widget_editor();
}
