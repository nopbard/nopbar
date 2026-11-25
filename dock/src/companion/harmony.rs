//! Only offscreen targets. Never creates the companion's HWNDs or starts
//! NopBar, reads desktop pixels, or injects input.
use super::*;
use crate::gfx::promo_kit as kit;
use windows::Win32::Graphics::Direct2D::D2D1_DEVICE_CONTEXT_OPTIONS_NONE;
use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};

#[path = "harmony_polish.rs"]
mod polish;

#[test]
#[ignore = "offscreen focus reactions at actual/large sizes; no HWND or desktop capture"]
fn native_review_focus_feedback() {
    unsafe { let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED); }
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/ui-snapshots/native-review-focus");
    for scale in [0.28, 0.56] { for light in [true, false] {
        let cv = kit::Canvas::at(960., 320., 1.);
        unsafe { cv.dc.Clear(Some(&kit::col(kit::harmony_backdrop(light)))); }
        let mut painter = render::Painter::new();
        for (i, cue) in [Cue::FocusStart, Cue::BreakStart, Cue::BreakEnd].into_iter().enumerate() {
            let now = Instant::now();
            let mut reactions = cues::Reactions::default();
            reactions.calm(Calm::Focus, true); reactions.calm(Calm::Meeting, true);
            reactions.suppress(true); reactions.accept(cue, now, true);
            let reaction = reactions.tick(now, 0.2, State::Idle, false);
            assert!(reaction.active && !reaction.quiet);
            let x = 160. + i as f32 * 320.; let y = 280.;
            let mut motion = Motion::new(x); motion.set_state(reaction.state);
            let pose = motion.step(0.2, x - 80., x + 80.);
            let mut fx = fx::Engine::default(); let mut rng = fx::Rng(42);
            fx.pose(&pose, x, y, scale);
            for effect in reaction.effects { fx.burst(effect, &mut rng, x, y, false); }
            fx.tick(0.1, reaction.state, &pose.frame, x, y, &mut rng, true, false);
            unsafe {
                render::shadow(&cv.dc, &pose, x, y, scale, light).unwrap();
                painter.paint(&cv.dc, &pose, x, y, scale, false, 0.2, 1.).unwrap();
                painter.particles(&cv.dc, &fx.parts, x, y, scale, (0., 0.), light).unwrap();
            }
        }
        let (pixels, w, h) = cv.finish();
        kit::save_png(&pixels, w, h, &root.join(format!("{}-{}.png", if light {"light"} else {"dark"}, (scale * 100.) as u32)));
    }}
}

#[test]
#[ignore = "renders all 40 life frames to offscreen targets; no desktop capture"]
fn harmony_companion_life_frames() {
    unsafe {let _=CoInitializeEx(None,COINIT_APARTMENTTHREADED);}
    let frames:Vec<_>=[("look",8),("pet",4),("poke",4),("read",8),("stretch",4),("yawn",4),("carry",8)]
        .into_iter().flat_map(|(name,count)|(0..count).map(move|i|format!("{name}_{i}"))).collect();
    assert_eq!(frames.len(),40);
    for dpi in [1.,1.5] {for light in [true,false] {
        let folder=format!("companion/{}-{}",if light{"light"}else{"dark"},(dpi*100.) as u32);
        let contact=kit::Canvas::at(360.*8.,300.*5.,dpi);
        let mut contact_painter=render::Painter::new();
        unsafe {contact.dc.Clear(Some(&kit::col(kit::harmony_backdrop(light))));}
        for (i,frame) in frames.iter().enumerate() {
            let mut p=Motion::new(180.).pose();p.frame=frame.clone();
            let cv=kit::Canvas::at(360.,300.,dpi);
            let mut painter=render::Painter::new();
            unsafe {
                cv.dc.Clear(Some(&kit::col(kit::harmony_backdrop(light))));
                render::shadow(&cv.dc,&p,180.,260.,0.56,light).unwrap();
                painter.paint(&cv.dc,&p,180.,260.,0.56,false,0.,dpi).unwrap();
                let (x,y)=(180.+(i%8) as f32*360.,260.+(i/8) as f32*300.);
                render::shadow(&contact.dc,&p,x,y,0.56,light).unwrap();
                contact_painter.paint(&contact.dc,&p,x,y,0.56,false,0.,dpi).unwrap();
            }
            let (px,w,h)=cv.finish();
            assert!(px.chunks_exact(4).any(|p|p[2] as u16>p[1] as u16+10),"{frame}: hat visible");
            kit::save_png(&px,w,h,&kit::harmony_dir().join(&folder).join(format!("{frame}.png")));
        }
        let (px,w,h)=contact.finish();
        kit::save_png(&px,w,h,&kit::harmony_dir().join(&folder).join("life-contact.png"));
    }}
}

#[test]
#[ignore = "measures isolated offscreen life CPU; never starts or attaches to NopBar"]
fn harmony_companion_life_cpu() {
    use windows::Win32::System::Threading::{GetCurrentProcess,GetProcessTimes};
    fn cpu()->f64 {unsafe{
        let(mut c,mut e,mut k,mut u)=(FILETIME::default(),FILETIME::default(),FILETIME::default(),FILETIME::default());
        GetProcessTimes(GetCurrentProcess(),&mut c,&mut e,&mut k,&mut u).unwrap();
        let ticks=|f:FILETIME|((f.dwHighDateTime as u64)<<32)|f.dwLowDateTime as u64;
        (ticks(k)+ticks(u)) as f64/10_000_000.
    }}
    unsafe {let _=CoInitializeEx(None,COINIT_APARTMENTTHREADED);}
    let cv=kit::Canvas::at(340.,340.,1.);
    let dc=unsafe{cv.dc.GetDevice().unwrap().CreateDeviceContext(D2D1_DEVICE_CONTEXT_OPTIONS_NONE).unwrap()};
    let mut raster=unsafe{render::Raster::new(dc,340,340).unwrap()};
    let mut pixels=vec![0;340*340*4];
    let mut report=String::from("Offscreen WARP, 340x340, boil=false, warmed frames, shared life model + D2D raster/readback, stamp gating. 10s per sample; one logical core percent (GetProcessTimes accounting is quantized; 0.000% is below measurement resolution, not proof of zero CPU cost). No HWND, UpdateLayeredWindow, desktop capture, live process or input injection. Idle includes GetCursorPos + gaze sampling. Read starts after sitting. Hidden skips all sampling/render work.\n");
    for battery in [false,true] {for (name,kind,visible) in [("Idle",None,true),("read",Some("read"),true),("carry",Some("carry"),true),("Hidden",None,false)] {
        for frame in ["idle","read","read_doze","carry"] {for f in &clips::catalog().clip(frame).frames {
            let mut p=Motion::new(170.).pose();p.frame=f.clone();
            unsafe {raster.draw(&p,170.,300.,0.56,1.,false,0.,false,true,&mut pixels).unwrap();}
        }}
        let mut gaze=gestures::Gaze::default();
        let mut motion=Motion::new(170.);let mut stamp=None;let mut n=0;let mut samples=0;
        let interval=model::interval(State::Idle,visible,false,false,battery);
        let begin=Instant::now();let before=cpu();let mut deadline=begin;
        while begin.elapsed().as_secs_f64()<10. {
            if let Some(ms)=interval {
                samples+=1;let seconds=begin.elapsed().as_secs_f64();
                let mut p=motion.step(ms as f32/1000.,80.,280.);
                if let Some(clip)=kind {
                    p.frame=clips::catalog().clip(clip).frame(seconds as f32).into();
                } else {
                    let mut pt=POINT::default();
                    if unsafe{GetCursorPos(&mut pt)}.is_ok() {gaze.sample(seconds,grab::Point::new(pt.x as f32,pt.y as f32),grab::Point::new(-10000.,-10000.));}
                }
                let next=render::Stamp::new(&p,0.56,1.,false,true);
                if stamp.as_ref()!=Some(&next) {
                    unsafe {raster.draw(&p,170.,300.,0.56,1.,false,seconds as f32,false,true,&mut pixels).unwrap();}
                    stamp=Some(next);n+=1;
                }
                deadline+=std::time::Duration::from_millis(ms as u64);
                std::thread::sleep(deadline.saturating_duration_since(Instant::now()));
            }else {std::thread::sleep(std::time::Duration::from_secs(10));}
        }
        let used=cpu()-before;let wall=begin.elapsed().as_secs_f64();
        if !visible {assert_eq!(samples,0);assert_eq!(n,0);}
        report+=&format!("{name}, battery={battery}: callbacks={samples}, raster_frames={n}, wall={wall:.3}s, cpu={used:.5}s, core={:.3}%\n",used/wall*100.);
    }}
    println!("{report}");let path=kit::harmony_dir().join("companion/life-cpu.txt");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();std::fs::write(path,report).unwrap();
}

#[test]
#[ignore = "writes offscreen companion pictures"]
fn harmony_companion_states() {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    }
    let samples = [
        ("idle", State::Idle, 0.6),
        ("sit", State::Sit, 0.6),
        ("walk-0", State::Walk, 0.15),
        ("walk-1", State::Walk, 0.5),
        ("notify", State::Notify, 0.15),
        ("wait", State::Wait, 0.6),
        ("done", State::Done, 0.15),
        ("error", State::Error, 0.6),
        ("sleep", State::Sleep, 0.6),
        ("peek", State::Peek, 0.6),
    ];
    for dpi in [1.0, 1.5] {
        for light in [true, false] {
            let folder = format!(
                "companion/{}-{}",
                if light { "light" } else { "dark" },
                (dpi * 100.0) as u32
            );
            let contact = kit::Canvas::at(360.0 * 4.0, 300.0 * 8.0, 2.0 * dpi);
            let mut contact_painter = render::Painter::new();
            unsafe {
                contact
                    .dc
                    .Clear(Some(&kit::col(kit::harmony_backdrop(light))));
            }
            for (index, (name, state, time)) in samples.into_iter().enumerate() {
                let mut m = Motion::new(180.0);
                m.set_state(state);
                for _ in 0..(time * 120.0) as u32 {
                    m.step(1.0 / 120.0, 80.0, 280.0);
                }
                let p = m.pose();
                let cv = kit::Canvas::at(360.0, 300.0, 2.0 * dpi);
                let mut painter = render::Painter::new();
                unsafe {
                    let cx = 180.0 + (index % 4) as f32 * 360.0;
                    let cy = 260.0 + (index / 4) as f32 * 300.0;
                    render::shadow(&contact.dc, &p, cx, cy, 0.56, light).unwrap();
                    contact_painter
                        .paint(&contact.dc, &p, cx, cy, 0.56, true, time, dpi)
                        .unwrap();
                    cv.dc.Clear(Some(&kit::col(kit::harmony_backdrop(light))));
                    render::shadow(&cv.dc, &p, 180.0, 260.0, 0.56, light).unwrap();
                    painter
                        .paint(&cv.dc, &p, 180.0, 260.0, 0.56, true, time, dpi)
                        .unwrap();
                }
                assert!(
                    !painter.boil_unavailable,
                    "required D2D effects unavailable"
                );
                let (px, w, h) = cv.finish();
                assert!(
                    px.chunks_exact(4).any(|p| p[2] as u16 > p[1] as u16 + 10),
                    "gnome's red hat is present"
                );
                kit::save_png(
                    &px,
                    w,
                    h,
                    &kit::harmony_dir().join(&folder).join(format!("{name}.png")),
                );
            }
            for (index,(name,state,time)) in samples.into_iter().enumerate() {
                let mut m=Motion::new(180.);m.set_state(state);let mut fx=fx::Engine::default();let mut rng=fx::Rng(42);
                let time=time.max(0.32);
                for _ in 0..(time*120.) as u32 {let p=m.step(1./120.,80.,280.);fx.tick(1./120.,state,&p.frame,180.,260.,&mut rng,true,false);}
                let p=m.pose();let cv=kit::Canvas::at(360.,300.,2.*dpi);let mut painter=render::Painter::new();
                unsafe {
                    cv.dc.Clear(Some(&kit::col(kit::harmony_backdrop(light))));
                    render::shadow(&cv.dc,&p,180.,260.,0.56,light).unwrap();painter.paint(&cv.dc,&p,180.,260.,0.56,false,time,dpi).unwrap();
                    painter.particles(&cv.dc,&fx.parts,180.,260.,0.56,(0.,0.),light).unwrap();
                    let x=(index%4) as f32*360.;let y=(4+index/4) as f32*300.;
                    contact_painter.paint(&contact.dc,&p,x+180.,y+260.,0.56,false,time,dpi).unwrap();
                    contact_painter.particles(&contact.dc,&fx.parts,180.,260.,0.56,(-x,-y),light).unwrap();
                }
                let (px,w,h)=cv.finish();kit::save_png(&px,w,h,&kit::harmony_dir().join(&folder).join(format!("{name}-fx.png")));
            }
            for (index, (name, bounce)) in [("slot-hidden", 0.0), ("slot-bounce", -2.5)]
                .into_iter()
                .enumerate()
            {
                let cv = kit::Canvas::at(180.0, 44.0, 2.0 * dpi);
                let mut painter = render::Painter::new();
                unsafe {
                    contact_painter
                        .slot(
                            &contact.dc,
                            180.0 + index as f32 * 360.0,
                            1040.0 + bounce,
                            24.0,
                            1.0,
                        )
                        .unwrap();
                    cv.dc.Clear(Some(&kit::col(kit::harmony_backdrop(light))));
                    painter
                        .slot(&cv.dc, 90.0, 34.0 + bounce, 24.0, 1.0)
                        .unwrap();
                }
                let (px, w, h) = cv.finish();
                kit::save_png(
                    &px,
                    w,
                    h,
                    &kit::harmony_dir().join(&folder).join(format!("{name}.png")),
                );
            }
            // Also review the inexpensive no-boil path on both backgrounds.
            let pose = Motion::new(900.0).pose();
            unsafe {
                render::shadow(&contact.dc, &pose, 900.0, 1160.0, 0.56, light).unwrap();
                contact_painter
                    .paint(&contact.dc, &pose, 900.0, 1160.0, 0.56, false, 0.0, dpi)
                    .unwrap();
            }
            for (i,(name,y,angle)) in [("grab-hat",0.04,0.2),("grab-body",0.58,0.06),("grab-foot",0.97,std::f32::consts::PI)].into_iter().enumerate(){
                let f=&clips::catalog().frames["jump_3"];let mut g=grab::Grab::default();g.down(1,grab::Kind::Mouse,grab::Point::new(180.,150.),grab::Point::new(f.ax/f.w as f32,y),0.56);g.moved(1,grab::Point::new(185.,150.));g.theta=angle;
                let p=g.pose();let cv=kit::Canvas::at(360.,300.,2.*dpi);let mut painter=render::Painter::new();
                unsafe{cv.dc.Clear(Some(&kit::col(kit::harmony_backdrop(light))));painter.paint(&cv.dc,&p,180.,if i==1{150.}else{60.},0.56,false,0.,dpi).unwrap();contact_painter.paint(&contact.dc,&p,180.+i as f32*360.,2100.+if i==1{150.}else{60.},0.56,false,0.,dpi).unwrap();}
                let (px,w,h)=cv.finish();kit::save_png(&px,w,h,&kit::harmony_dir().join(&folder).join(format!("{name}.png")));
            }
            let (px, w, h) = contact.finish();
            kit::save_png(
                &px,
                w,
                h,
                &kit::harmony_dir().join(&folder).join("contact.png"),
            );
        }
    }
}

#[test]
#[ignore = "measures offscreen idle CPU only; no windows shown"]
fn harmony_companion_idle_cpu() { companion_cpu(false); }

#[test]
#[ignore = "20 seconds x 3 CPU samples per state/power; offscreen only"]
fn harmony_companion_cpu_recheck() { companion_cpu(true); }

fn companion_cpu(recheck:bool) {
    use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};
    fn cpu() -> f64 {
        unsafe {
            let (mut c, mut e, mut k, mut u) = (
                FILETIME::default(),
                FILETIME::default(),
                FILETIME::default(),
                FILETIME::default(),
            );
            GetProcessTimes(GetCurrentProcess(), &mut c, &mut e, &mut k, &mut u).unwrap();
            let ticks = |f: FILETIME| ((f.dwHighDateTime as u64) << 32) | f.dwLowDateTime as u64;
            (ticks(k) + ticks(u)) as f64 / 10_000_000.0
        }
    }
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    }
    // Match the app's threading flags; Canvas intentionally uses plain WARP
    // for picture tests and does not model the production device's CPU cost.
    use windows::core::Interface;
    use windows::Win32::Graphics::{Direct2D::*,Direct3D::D3D_DRIVER_TYPE_WARP,Dxgi::IDXGIDevice};
    let factory:ID2D1Factory1=unsafe{D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED,None).unwrap()};
    let device=crate::gfx::create_device(D3D_DRIVER_TYPE_WARP,None).unwrap();
    let dxgi:IDXGIDevice=device.cast().unwrap();let d2d=unsafe{factory.CreateDevice(&dxgi).unwrap()};
    let dc=unsafe{d2d.CreateDeviceContext(D2D1_DEVICE_CONTEXT_OPTIONS_NONE).unwrap()};
    let mut raster=unsafe{render::Raster::new(dc,340,340).unwrap()};
    let fxdc=unsafe{d2d.CreateDeviceContext(D2D1_DEVICE_CONTEXT_OPTIONS_NONE).unwrap()};
    let mut fxraster=unsafe{render::Raster::new(fxdc,340,340).unwrap()};
    let mut fxpixels=vec![0u8;340*340*4];
    let mut pixels = vec![0u8; 340 * 340 * 4];
    let mut report=String::from("Offscreen WARP with production Gfx::DEVICE_FLAGS (prevents driver threading optimizations), 340x340 DIP at stated DPI, warmed PNGs + frame anchors + actual deformation shader path, continuous motion + stamp gating, persistent pixel buffer, D2D + CPU readback. Separate FX target included (fast FX 30fps, slow FX 12fps, half on battery). No UpdateLayeredWindow/desktop present. Percent is one logical core; not a live NopBar measurement.\n");
    let seconds=if recheck{20.0}else{5.0};
    let repeats=if recheck{3}else{1};
    let cases=if recheck {vec![
        (State::Idle,false,false,true,1.0),(State::Idle,false,true,true,1.0),
        (State::Idle,false,false,true,2.0),(State::Idle,false,true,true,2.0),
        (State::Walk,false,false,true,1.0),(State::Walk,false,true,true,1.0),
        (State::Sleep,false,false,true,1.0),(State::Sleep,false,true,true,1.0),
    ]}else{vec![
        (State::Idle, true, false, true, 1.0),
        (State::Idle, false, false, true, 1.0),
        (State::Idle, true, true, true, 1.0),
        (State::Sleep, false, false, true, 1.0),
        (State::Walk, false, false, true, 1.0),
        (State::Idle, false, true, true, 1.0),
        (State::Sleep, false, true, true, 1.0),
        (State::Walk, false, true, true, 1.0),
        (State::Idle, true, false, false, 1.0),
        (State::Idle, false, false, true, 2.0),
        (State::Idle, false, true, true, 2.0),
    ]};
    let path=kit::harmony_dir().join(if recheck{"companion/cpu-recheck.txt"}else{"companion/cross-review-cpu.txt"});
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    report+=&format!("Each case: {seconds}s x {repeats}; rounds interleaved; boil=false for recheck.\n");
    let mut samples=vec![Vec::<f64>::new();cases.len()];
    for round in 1..=repeats {for (case,&(state,boil,battery,visible,dpi)) in cases.iter().enumerate() {
        let side=(340.0*dpi) as u32;
        unsafe{raster.resize(side,side).unwrap();fxraster.resize(side,side).unwrap();}
        pixels.resize((side*side*4) as usize,0);fxpixels.resize((side*side*4) as usize,0);
        let mut layer=comfort::Layer::default();
        let mut motion = Motion::new(170.0);
        motion.set_state(state);
        let interval = model::interval(state, visible, false, false, battery);
        let mut fx=fx::Engine::default();let mut rng=fx::Rng(42);
        // Warm all clip frames before measuring steady-state idle.
        for i in 0..240 {
            let mut p = motion.step(1.0 / 60.0, 80.0, 280.0);
            layer.apply(&mut p,state,i as f64/60.,0.56,dpi,true);
            fx.pose(&p,170.,300.,0.56);
            unsafe {
                raster
                    .draw(
                        &p,
                        170.0,
                        300.0,
                        0.56,
                        dpi,
                        boil,
                        0.0,
                        false,
                        true,
                        &mut pixels,
                    )
                    .unwrap();
            }
        }
        // Warm FX bitmaps and their independent target before the measurement.
        for name in fx::NAMES.into_iter().chain(["ticks_right"]) {
            fx.clear();fx.burst(name,&mut rng,170.,300.,false);
            unsafe{fxraster.draw_particles(&fx.parts,170.,300.,0.56,dpi,(0.,0.),true,&mut fxpixels).unwrap();}
        }
        fx.clear();
        layer=comfort::Layer::default();
        let mut stamp = None;
        let begin = Instant::now();
        let before = cpu();
        let mut n = 0;
        let mut fx_frames = 0;
        let mut deadline = begin;
        while begin.elapsed().as_secs_f32() < seconds {
            if let Some(ms) = fx::interval(interval,&fx.parts,battery) {
                let mut p = motion.step(ms as f32 / 1000.0, 80.0, 280.0);
                layer.apply(&mut p,state,begin.elapsed().as_secs_f64(),0.56,dpi,true);
                fx.pose(&p,170.,300.,0.56);
                fx.tick(ms as f32/1000.,state,&p.frame,170.,300.,&mut rng,true,false);
                if !fx.parts.is_empty(){fx_frames+=1;unsafe{fxraster.draw_particles(&fx.parts,170.,300.,0.56,dpi,(0.,0.),true,&mut fxpixels).unwrap();}}
                let next = render::Stamp::new(&p, 0.56, dpi, boil, true);
                if stamp.as_ref() != Some(&next) {
                    unsafe {
                        raster
                            .draw(
                                &p,
                                170.0,
                                300.0,
                                0.56,
                                dpi,
                                boil,
                                begin.elapsed().as_secs_f32(),
                                false,
                                true,
                                &mut pixels,
                            )
                            .unwrap();
                    }
                    stamp = Some(next);
                    n += 1;
                }
                deadline += std::time::Duration::from_millis(ms as u64);
                std::thread::sleep(deadline.saturating_duration_since(Instant::now()));
            } else {
                std::thread::sleep(std::time::Duration::from_secs_f32(seconds));
            }
        }
        let wall = begin.elapsed().as_secs_f64();
        let used = cpu() - before;
        let percent=used/wall*100.0;samples[case].push(percent);
        let line=format!("round={round}, {state:?}, dpi={dpi}, size=0.56, boil={boil}, battery={battery}, visible={visible}: frames={n}, fx_frames={fx_frames}, wall={wall:.3}s, cpu={used:.4}s, core={percent:.4}%\n");
        print!("{line}");report+=&line;std::fs::write(&path,&report).unwrap();
    }}
    if recheck {for (case,&(state,_,battery,_,dpi)) in cases.iter().enumerate(){
        let mut sorted=samples[case].clone();sorted.sort_by(f64::total_cmp);
        let line=format!("MEDIAN {state:?}, dpi={dpi}, battery={battery}: {:.4}% (samples={:?})\n",sorted[sorted.len()/2],samples[case]);
        print!("{line}");report+=&line;
    }}
    std::fs::write(path,report).unwrap();
}

#[test]
#[ignore = "offscreen emotion and comfort sheets; no HWND or screen capture"]
fn harmony_companion_emote_comfort() { emote_comfort(&[0.56],false); }
#[test]
#[ignore = "small-size emotion sheets; offscreen only"]
fn harmony_companion_polish_emotes() { emote_comfort(&[0.28,0.34,0.56],true); }
fn emote_comfort(sizes:&[f32],polish:bool) {
 unsafe{let _=CoInitializeEx(None,COINIT_APARTMENTTHREADED);}
 use windows::Win32::Graphics::{DirectWrite::*,Direct2D::{Common::*,D2D1_DRAW_TEXT_OPTIONS_NONE}};
 let dw:IDWriteFactory=unsafe{DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED).unwrap()};
 let fmt=unsafe{dw.CreateTextFormat(windows::core::w!("Segoe UI"),None,DWRITE_FONT_WEIGHT_NORMAL,DWRITE_FONT_STYLE_NORMAL,DWRITE_FONT_STRETCH_NORMAL,13.,windows::core::w!("en-US")).unwrap()};
 let label=|cv:&kit::Canvas,text:&str,x:f32,y:f32,light:bool|unsafe{let brush=cv.dc.CreateSolidColorBrush(&D2D1_COLOR_F{r:if light{0.15}else{0.85},g:if light{0.15}else{0.85},b:if light{0.15}else{0.85},a:1.},None).unwrap();cv.dc.DrawText(&text.encode_utf16().collect::<Vec<_>>(),&fmt,&D2D_RECT_F{left:x,top:y,right:x+290.,bottom:y+24.},&brush,D2D1_DRAW_TEXT_OPTIONS_NONE,DWRITE_MEASURING_MODE_NATURAL);};
 let cases=[("notify","notify_1",State::Notify,"",0.2),("wait","wait_2",State::Wait,"",0.65),("done","done_1",State::Done,"",0.45),("error","error_1",State::Error,"",0.7),("sleep","sleep_2",State::Sleep,"",1.65),("land","sit_3",State::Idle,"dust",0.2),("morning","wait_2",State::Idle,"morning",0.4),("carry","carry_3",State::Idle,"",0.5),("read","read_3",State::Idle,"",0.5),("focus","peek_0",State::Idle,"ticks_right",0.15)];
 for dpi in [1.,1.5] {for light in [true,false] {
  let folder=kit::harmony_dir().join(format!("companion/{}-{}",if light{"light"}else{"dark"},(dpi*100.) as u32));
  for &size in sizes {for flip in [false,true] {
   let cv=kit::Canvas::at(320.*4.,300.*4.,dpi);unsafe{cv.dc.Clear(Some(&kit::col(kit::harmony_backdrop(light))));}
   let mut painter=render::Painter::new();
   for (i,(name,frame,state,burst,t)) in cases.iter().enumerate(){
    let x=160.+(i%4) as f32*320.;let y=267.+(i/4) as f32*300.;
    let mut p=Motion::new(x).pose();p.frame=(*frame).into();p.flip=flip;
    let mut e=fx::Engine::default();let mut rng=fx::Rng(42);e.pose(&p,x,y,size);e.tick(0.,*state,frame,x,y,&mut rng,true,false);if !burst.is_empty(){e.burst(if flip&&*burst=="ticks_right"{"ticks_left"}else{burst},&mut rng,x,y,false);}
    for _ in 0..(*t*120.) as u32 {e.pose(&p,x,y,size);e.tick(1./120.,*state,frame,x,y,&mut rng,true,false);}
    unsafe{render::shadow(&cv.dc,&p,x,y,size,light).unwrap();painter.paint(&cv.dc,&p,x,y,size,false,*t,dpi).unwrap();painter.particles(&cv.dc,&e.parts,x,y,size,(0.,0.),light).unwrap();}
    label(&cv,name,x-145.,y-257.,light);
   }
   let(px,w,h)=cv.finish();kit::save_png(&px,w,h,&folder.join(if polish{format!("emote-{:03}-{}.png",(size*100.).round() as u32,if flip{"left"}else{"right"})}else{format!("emote-{}.png",if flip{"left"}else{"right"})}));
  }
  }
  let cv=kit::Canvas::at(360.*3.,320.*2.,dpi);unsafe{cv.dc.Clear(Some(&kit::col(kit::harmony_backdrop(light))));}let mut painter=render::Painter::new();
  for (i,size) in [0.34,0.56,0.80].into_iter().enumerate(){let p=Motion::new(180.).pose();let x=180.+i as f32*360.;unsafe{painter.paint(&cv.dc,&p,x,300.,size,false,0.,dpi).unwrap();}label(&cv,&format!("size {size}"),x-160.,12.,light);}
  for (i,t) in [0.,0.7,1.4,2.1].into_iter().enumerate(){let x=135.+i as f32*270.;let mut p=Motion::new(x).pose();let mut layer=comfort::Layer::default();layer.apply(&mut p,State::Idle,-0.2,0.56,dpi,true);p=Motion::new(x).pose();layer.apply(&mut p,State::Idle,t,0.56,dpi,true);unsafe{painter.paint(&cv.dc,&p,x,620.,0.56,false,t as f32,dpi).unwrap();}label(&cv,&format!("breath/sway {t:.1}s"),x-120.,332.,light);}
  let(px,w,h)=cv.finish();kit::save_png(&px,w,h,&folder.join("comfort-sizes-motion.png"));
 }}
}

#[test]
#[ignore = "offscreen shared tool icons and popup material; no native window"]
fn harmony_companion_tools() { harmony_companion_fan(); }

#[test]
#[ignore = "notes entry acceptance: production fan/character raster, no visible windows"]
fn harmony_notes_entry() {
 unsafe { let _=CoInitializeEx(None,COINIT_APARTMENTTHREADED); }
 let _language=crate::i18n::test_lock("ko");
 let stage=kit::stage();
 let root=kit::harmony_dir().join("notes-entry");
 for size in [0.28,0.56] {for dpi in [1.,1.5] {for light in [true,false] {
  let theme=kit::theme(light);
  let cv=kit::Canvas::at(1440.,420.,dpi);
  unsafe {cv.dc.Clear(Some(&kit::col(kit::harmony_backdrop(light))));}
  for (col,x) in [240.,55.,425.].into_iter().enumerate() {
   let mut p=Motion::new(x).pose();p.frame="look_4".into();
   let y=380.;let a=anchors::Anchors::load(&p.frame).posed(&p,1.);
   let tip=fan::Point{x:x+a.tip.0*size,y:y+a.tip.1*size};
   let shape=bubble::silhouette(&p,x,y,size);
   let work=fan::Rect{left:8.,top:30.,right:472.,bottom:412.};
   for kind in [grab::Kind::Mouse,grab::Kind::Touch,grab::Kind::Pen] {
    for mode in [fan::Expanded::None,fan::Expanded::Size,fan::Expanded::Rest] {
     let layout=fan::place_tools(tip,&shape,size<=0.4,work,&[],kind,mode,true,false).unwrap_or_else(|| panic!("size={size} x={x} kind={kind:?} mode={mode:?}"));
     for (i,b) in layout.buttons.iter().enumerate() {
      assert!(b.center.x-b.radius>=work.left+8. && b.center.x+b.radius<=work.right-8.);
      assert!(b.center.y-b.radius>=work.top+8. && b.center.y+b.radius<=work.bottom-8.);
      assert!(layout.buttons[..i].iter().all(|a| a.center.distance(b.center)+0.001>=a.radius+b.radius+10.));
     }
    }
   }
   let layout=fan::place_tools(tip,&shape,size<=0.4,work,&[],grab::Kind::Mouse,fan::Expanded::None,true,false).unwrap();
   unsafe {
    cv.dc.SetTransform(&(render::translate(col as f32*480.,0.)*render::scale(dpi,dpi)));
    render::shadow(&cv.dc,&p,x,y,size,light).unwrap();
    render::Painter::new().paint(&cv.dc,&p,x,y,size,false,0.,dpi).unwrap();
    bubble::paint(&cv.dc,&layout,&theme,Some(fan::Id::NewNote),1.,1.,None,false,dpi).unwrap();
    let text=bubble::tooltip(fan::Id::NewNote);
    let (w,h)=crate::topbar::wrapped_tip_size(&stage.gfx,&theme,dpi,&text,200.*dpi).unwrap();
    let r=layout.tooltip_rect(fan::Id::NewNote,w as f32/dpi,h as f32/dpi,work).unwrap();
    cv.dc.SetTransform(&render::translate(col as f32*480.*dpi,0.));
    crate::topbar::paint_tip_offscreen(&stage.gfx,&theme,&cv.dc,dpi,&text,r.left*dpi,r.top*dpi).unwrap();
   }
  }
  let (px,w,h)=cv.finish();
  kit::save_png(&px,w,h,&root.join(format!("fan-{}-size{}-dpi{}.png",if light{"light"}else{"dark"},(size*100.) as u32,(dpi*100.) as u32)));
 }}}
}
#[test]
#[ignore = "compact v4 fan scenarios, offscreen only"]
fn harmony_companion_fan() {
 unsafe{let _=CoInitializeEx(None,COINIT_APARTMENTTHREADED);}
 use windows::Win32::Graphics::{DirectWrite::*,Direct2D::{Common::*,D2D1_DRAW_TEXT_OPTIONS_NONE}};
 let dw:IDWriteFactory=unsafe{DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED).unwrap()};
 let fmt=unsafe{dw.CreateTextFormat(windows::core::w!("Segoe UI"),None,DWRITE_FONT_WEIGHT_NORMAL,DWRITE_FONT_STYLE_NORMAL,DWRITE_FONT_STRETCH_NORMAL,13.,windows::core::w!("en-US")).unwrap()};
 let mut failures=Vec::new();
 for dpi in [1.,1.5] {for light in [true,false] {
  let theme=kit::theme(light);let cv=kit::Canvas::at(480.*4.,420.*2.,dpi);unsafe{cv.dc.Clear(Some(&kit::col(kit::harmony_backdrop(light))));}
  for (i,(name,x,mode,edge,blocked)) in [("center",240.,fan::Expanded::None,false,false),("right edge",425.,fan::Expanded::None,false,false),("left edge",55.,fan::Expanded::None,false,false),("above blocked",240.,fan::Expanded::None,false,true),("size satellites",240.,fan::Expanded::Size,false,false),("rest satellites",240.,fan::Expanded::Rest,false,false),("detach",240.,fan::Expanded::None,true,false)].into_iter().enumerate() {
   let ox=(i%4) as f32*480.;let oy=(i/4) as f32*420.;let y=if blocked{240.}else{380.};let size=if blocked{0.28}else{0.56};
   let mut p=Motion::new(x).pose();p.frame="look_4".into();
   let a=anchors::Anchors::load(&p.frame).posed(&p,1.);let tip=fan::Point{x:x+a.tip.0*size,y:y+a.tip.1*size};
   let work=fan::Rect{left:8.,top:30.,right:472.,bottom:412.};
   let forbidden=if blocked{vec![fan::Rect{left:8.,top:30.,right:472.,bottom:tip.y-10.}]}else{vec![]};
   let shape=bubble::silhouette(&p,x,y,size);
   let Some(layout)=fan::place_tools(tip,&shape,size<=0.4,work,&forbidden,grab::Kind::Mouse,mode,false,edge)else{failures.push(format!("{name}: {tip:?}"));continue;};
   if matches!(name,"right edge"|"left edge") {
    assert!((layout.center.x-tip.x).abs()<=layout.radius*0.9+0.001);
    assert!(layout.arcs.iter().all(|a|a.bounds().bottom-tip.y<=layout.radius*0.3+0.001),"{name}: arc extends down the body: {layout:?}");
    if light&&dpi==1. {println!("{name}: R={}, shift_x={:.2}, rotation={:.1}, arc_drop={:.2}",layout.radius,layout.center.x-tip.x,layout.arcs[0].middle-90.,layout.arcs[0].bounds().bottom-tip.y);}
   }
   unsafe {
    cv.dc.SetTransform(&(render::translate(ox,oy)*render::scale(dpi,dpi)));
    for r in &forbidden{let brush=cv.dc.CreateSolidColorBrush(&D2D1_COLOR_F{r:0.4,g:0.4,b:0.4,a:0.2},None).unwrap();cv.dc.FillRectangle(&D2D_RECT_F{left:r.left,top:r.top,right:r.right,bottom:r.bottom},&brush);}
    render::shadow(&cv.dc,&p,x,y,size,light).unwrap();render::Painter::new().paint(&cv.dc,&p,x,y,size,false,0.,dpi).unwrap();
    bubble::paint(&cv.dc,&layout,&theme,None,1.,1.,None,true,dpi).unwrap();
    let brush=cv.dc.CreateSolidColorBrush(&kit::col(theme.ink()),None).unwrap();cv.dc.DrawText(&name.encode_utf16().collect::<Vec<_>>(),&fmt,&D2D_RECT_F{left:16.,top:8.,right:460.,bottom:29.},&brush,D2D1_DRAW_TEXT_OPTIONS_NONE,DWRITE_MEASURING_MODE_NATURAL);
   }
  }
  let(px,w,h)=cv.finish();kit::save_png(&px,w,h,&kit::harmony_dir().join(format!("companion/{}-{}/fan.png",if light{"light"}else{"dark"},(dpi*100.) as u32)));
 }}
 assert!(failures.is_empty(),"{failures:?}");
}

#[test]
#[ignore = "fake input to offscreen raster latency, never injects native input"]
fn harmony_companion_input_latency() {
 unsafe{let _=CoInitializeEx(None,COINIT_APARTMENTTHREADED);}
 let cv=kit::Canvas::at(340.,340.,1.);let dc=unsafe{cv.dc.GetDevice().unwrap().CreateDeviceContext(D2D1_DEVICE_CONTEXT_OPTIONS_NONE).unwrap()};
 let mut r=unsafe{render::Raster::new(dc,340,340).unwrap()};let mut pixels=vec![0;340*340*4];
 let mut base=Motion::new(170.).pose();let mut press=gestures::Press::default();
 unsafe{r.painter.warm_inputs(&r.dc,0.56,1.).unwrap();r.draw(&base,170.,300.,0.56,1.,false,0.,false,true,&mut pixels).unwrap();}
 let before=pixels.clone();let mut samples=vec![];
 for i in 0..100 {let t=i as f64;let begin=Instant::now();press.set(true,t);let mut p=base.clone();press.apply(&mut p,t,true);unsafe{r.draw(&p,170.,300.,0.56,1.,false,0.,false,true,&mut pixels).unwrap();}assert_ne!(pixels,before);samples.push(begin.elapsed().as_secs_f64()*1000.);press.set(false,t+0.1);}
 base.frame="look_4".into();let begin=Instant::now();unsafe{r.draw(&base,170.,300.,0.56,1.,false,0.,false,true,&mut pixels).unwrap();}let release=begin.elapsed().as_secs_f64()*1000.;
 samples.sort_by(f64::total_cmp);let report=format!("Fake press + shared offscreen WARP raster/readback, 100 samples: median={:.3}ms p95={:.3}ms max={:.3}ms. Release look_4={release:.3}ms. No native HWND, bubble creation or screen presentation measured. Same-message dispatch is separately unit-tested.\n",samples[50],samples[95],samples[99]);
 println!("{report}");std::fs::write(kit::harmony_dir().join("companion/input-latency.txt"),report).unwrap();assert!(samples[95]<16.&&release<16.);
}

#[test]
#[ignore="offscreen reason-driven poses, no desktop access"]
fn harmony_assistant_reasons(){
    unsafe {let _=CoInitializeEx(None,COINIT_APARTMENTTHREADED);}
    for light in [false,true] {for dpi in [1.,1.5] {for scale in [0.28,0.56] {
        let cv=kit::Canvas::at(960.,520.,dpi);let mut painter=render::Painter::new();
        unsafe {cv.dc.Clear(Some(&kit::col(kit::harmony_backdrop(light))));}
        for (i,clip) in ["idle","yawn","read_doze","sleep","stretch","read","done","look","walk","sit"].into_iter().enumerate(){
            let mut p=Motion::new(0.).pose();p.frame=clips::catalog().clip(clip).frame(0.45).into();
            let x=96.+(i%5) as f32*192.;let y=240.+(i/5) as f32*260.;
            unsafe {render::shadow(&cv.dc,&p,x,y,scale,light).unwrap();painter.paint(&cv.dc,&p,x,y,scale,false,0.,dpi).unwrap();}
        }
        let(px,w,h)=cv.finish();kit::save_png(&px,w,h,&kit::harmony_dir().join(format!("assistant/reasons-{}-{}-{}.png",if light{"light"}else{"dark"},(dpi*100.)as u32,(scale*100.)as u32)));
    }}}
}
