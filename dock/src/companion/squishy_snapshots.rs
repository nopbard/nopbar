//! 30fps acceptance movies from the production model and painter; no HWND/IO.
use super::{
    clips,
    grab::{Grab, Kind, Point},
    peek::{self, Edge, Runtime},
    render,
};
use crate::gfx::promo_kit as kit;
use windows::Win32::{
    Foundation::RECT,
    Graphics::Direct2D::D2D1_DEVICE_CONTEXT_OPTIONS_NONE,
    System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED},
};

/// Identical injected inputs on both revisions. Baseline is captured before
/// changing production code, not a recreation of the old appearance.
#[test]
#[ignore = "offline all-action acceptance films; no HWND or desktop input"]
fn harmony_squishy_all() {
    use super::{comfort, fx, model::State, motion::Motion};
    unsafe { let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED); }
    let stage = std::env::var("SQUISHY_STAGE").unwrap_or_else(|_| "after".into());
    let root = std::env::var_os("SQUISHY_OUTPUT").map(std::path::PathBuf::from)
        .unwrap_or_else(|| kit::harmony_dir().join("companion-squishy-all"));
    let gfx_stage=kit::stage();
    let scenes = ["walk-stop", "turn", "grab-land", "done", "error", "notify", "poke",
        "pet", "sit-stand", "stretch", "doze-wake", "think", "show-hide", "carry", "yawn", "wait-look", "sleep"];
    for (scale, dpi) in [(0.56, 1.5), (0.28, 1.)] {
        for light in [false, true] {
            for name in scenes {
                if std::env::var("SQUISHY_FILTER").is_ok_and(|filter| !filter.split(',').any(|s|s==name)) {continue;}
                let folder = root.join(format!("{}-{}-{}-{name}", if light {"light"} else {"dark"}, (scale*100.) as u32, (dpi*100.) as u32)).join(&stage);
                std::fs::create_dir_all(&folder).unwrap();
                let cv = kit::Canvas::at(360., 340., dpi);
                let dc = unsafe {cv.dc.GetDevice().unwrap().CreateDeviceContext(D2D1_DEVICE_CONTEXT_OPTIONS_NONE).unwrap()};
                let mut raster = unsafe {render::Raster::new(dc, cv.w, cv.h).unwrap()};
                let mut pixels = vec![0; (cv.w*cv.h*4) as usize];
                let mut motion = Motion::new(145.);
                let mut comfort = comfort::Layer::default();
                let mut squishy = super::squishy::Layer::default();
                let mut grab = Grab::default();
                let mut effects = fx::Engine::default();
                let mut rng = fx::Rng(42);
                let mut trace = String::from("frame,time,clip,x,y,sx,sy,hat,foot_y,spinner_x,spinner_y,bubble_x,bubble_y\n");
                let mut last = String::new();
                let mut render_us=Vec::new();
                let mut last_stamp:Option<render::Stamp>=None;
                let mut bitmap_changes=0;
                for i in 0..120 {
                    let t = i as f32/30.;
                    let active = t >= 0.3;
                    let u = (t-0.3).max(0.);
                    let hidden=name=="show-hide" && u>=2.33;
                    let (clip, ct) = if !active {("idle",t)} else {match name {
                        "walk-stop" => (if u<1.3 {"walk"}else{"idle"},u),
                        "turn" => (if u<0.6 {"turn_left"}else if u<1.2 {"turn_right"}else{"idle"},if u<0.6 {u}else{u-0.6}),
                        "grab-land" => ("idle",u),
                        "pet" => (if u<1.2 {"pet"}else if u<1.35 {"pet_end"}else{"idle"},if u<1.2 {u}else{u-1.2}),
                        "sit-stand" => (if u<0.9 {"sit"}else if u<1.6 {"stand"}else{"idle"},if u<0.9 {u}else{u-0.9}),
                        "doze-wake" => (if u<2. {"read_doze"}else{"read"},if u<2. {u}else{u-2.}),
                        "think" => (if u<0.4 {"walk"}else if u<1.8 {"think"}else{"idle"},if u<0.4 {u}else{u-0.4}),
                        "show-hide" => (if u<0.6 {"show_air"}else if u<0.9 {"land"}else if u<1.6 {"idle"}else if u<1.68 {"hide_launch"}else if u<2.33 {"hide_air"}else{"idle"},if u<0.6 {u}else if u<0.9 {u-0.6}else if u<1.6 {u-0.9}else if u<1.68 {u-1.6}else{u-1.68}),
                        "wait-look" => (if u<1.2 {"wait"}else if u<1.7 {"look"}else{"idle"},if u<1.2 {u}else{u-1.2}),
                        "carry" => (if u<1.3 {"carry"}else{"idle"},u),
                        "sleep" => ("sleep",u),
                        other => (if u<clips::catalog().clip(other).duration() {other}else{"idle"},u),
                    }};
                    let state = match clip {"walk"|"carry"=>State::Walk,"sit"|"read"|"read_doze"=>State::Sit,"sleep"=>State::Sleep,"wait"=>State::Wait,"done"=>State::Done,"error"=>State::Error,"notify"=>State::Notify,_=>State::Idle};
                    motion.set_state(state);
                    let mut p = motion.step(1./30.,100.,280.);
                    p.frame = clips::catalog().clip(clip).frame(ct).into();
                    p.clip=clip.into();p.clip_time=ct;
                    p.blend = clips::catalog().clip(clip).transition(ct).map(|(f,a)|(f.into(),a));
                    let (mut x, mut y) = (motion.x,300.);
                    let mut draw_scale = scale;
                    let bob = comfort.apply(&mut p,state,t as f64,scale,dpi,true);
                    y += super::squishy::ground_bob(bob,stage!="before");
                    if name=="show-hide" && active && (clip=="show_air"||clip=="hide_air") {
                        (x,y,draw_scale)=(if stage=="before" {super::motion::travel}else{super::motion::spring_travel})(ct/if clip=="show_air"{0.6}else{0.65},clip=="show_air",(145.,300.),(260.,45.),scale,0.06);
                    }
                    if name=="grab-land" && active {
                        if i==9 {grab.down(1,Kind::Mouse,Point::new(145.*dpi,150.*dpi),Point::new(0.5,0.25),scale*dpi);grab.moved(1,Point::new(153.*dpi,150.*dpi));}
                        if u<0.7 {grab.moved(1,Point::new((153.+u*45.)*dpi,(150.-u*30.)*dpi));}
                        if i==30 {grab.up(1,grab.point,false,300.*dpi,80.*dpi,280.*dpi);}
                        let event=grab.step(1./30.);
                        if event.finished {
                            // Same hand-off as App::draw: the release position
                            // becomes the new home, not the fixture's old x.
                            motion.x=grab.point.x/dpi;
                            p.x=motion.x;
                            x=motion.x;
                        }
                        if grab.busy() {p=grab.pose();x=grab.point.x/dpi;y=grab.point.y/dpi;}
                    }
                    let direction=if clip=="pet" {if (ct/0.3) as i32%2==0 {1.}else{-1.}}else if clip=="turn_left" {-1.}else{1.};
                    if stage!="before" {
                        let selected=p.clip.clone();let time=p.clip_time;
                        x=squishy.apply(&mut p,(x,y,draw_scale),1./30.,super::squishy::Sample{clip:&selected,time,direction},true);
                    }
                    effects.motion(true);
                    effects.pose(&p,x,y,draw_scale);
                    effects.tick(1./30.,State::Idle,&p.frame,x,y,&mut rng,true,false);
                    if clip=="notify" && last!="notify" {effects.burst("bang",&mut rng,x,y,false);}
                    let stamp=render::Stamp::new(&p,draw_scale,dpi,false,light);
                    if last_stamp.as_ref().is_none_or(|old|!old.same_bitmap(&stamp)) {bitmap_changes+=1;last_stamp=Some(stamp);}
                    let render_start=std::time::Instant::now();
                    unsafe {raster.draw(&p,x,y,draw_scale,dpi,false,t,false,light,&mut pixels).unwrap();}
                    render_us.push(render_start.elapsed().as_micros() as u64);
                    if hidden {pixels.fill(0);effects.clear();}
                    if !effects.parts.is_empty() {
                        let mut overlay=vec![0;pixels.len()];
                        unsafe {raster.draw_particles(&effects.parts,x,y,draw_scale,dpi,(0.,0.),light,&mut overlay).unwrap();}
                        for (dst,src) in pixels.as_chunks_mut::<4>().0.iter_mut().zip(overlay.as_chunks::<4>().0) {
                            let inv=255-src[3] as u16;
                            for c in 0..4 {dst[c]=(src[c] as u16+(dst[c] as u16*inv+127)/255).min(255) as u8;}
                        }
                    }
                    let a=super::anchors::Anchors::load(&p.frame).posed(&p,1.);
                    if stage!="before" && matches!(name,"walk-stop"|"carry"|"think") {
                        assert!((y+a.feet.1*draw_scale-300.).abs()<0.001,"{name} frame {i}: walking/stop lifted the feet");
                    }
                    let bubble=if stage=="before" {a.tip}else{super::anchors::Anchors::body_tip(&p)};
                    let spinner_foot=if stage=="before" {(x,y)}else{super::anchors::Anchors::spinner_foot(&p,x,y,draw_scale)};
                    if name=="think"||name=="wait-look" {
                        let mut overlay=vec![0;pixels.len()];
                        unsafe {raster.custom(dpi,&mut overlay,|dc| {
                            use super::chat::{bubbles::Bubbles,paint};
                            let mut b=Bubbles::default();b.say("확인".into(),0.);
                            let input=paint::Input{text:"",caret:0,selection:(0,0),composition:None};
                            let tip=(x+bubble.0*draw_scale,y+bubble.1*draw_scale);
                            for item in paint::arrange(&gfx_stage.gfx,&b,&input,tip,paint::rect(0.,0.,360.,340.),2.,false,dpi)? {paint::item(dc,&item,None,false,&kit::theme(light))?;}
                            if clip=="think" {paint::spinner(dc,spinner_foot,draw_scale,t as f64,true,&kit::theme(light))?;}
                            Ok(())
                        }).unwrap();}
                        for (dst,src) in pixels.as_chunks_mut::<4>().0.iter_mut().zip(overlay.as_chunks::<4>().0) {
                            let inv=255-src[3] as u16;
                            for c in 0..4 {dst[c]=(src[c] as u16+(dst[c] as u16*inv+127)/255).min(255) as u8;}
                        }
                    }
                    let spinner=super::chat::bubbles::spinner(spinner_foot,draw_scale).0;
                    trace+=&format!("{i},{t:.6},{clip},{x:.6},{y:.6},{:.6},{:.6},{:.6},{:.6},{:.6},{:.6},{:.6},{:.6}\n",p.sx,p.sy,p.hat_tilt,y+a.feet.1*draw_scale,spinner.0,spinner.1,x+bubble.0*draw_scale,y+bubble.1*draw_scale);
                    for pixel in pixels.as_chunks_mut::<4>().0 {
                        let bg=if light {224u16}else{32};let inv=255-pixel[3] as u16;
                        for c in &mut pixel[..3] {*c=(*c as u16+(bg*inv+127)/255).min(255) as u8;}pixel[3]=255;
                    }
                    kit::save_png(&pixels,cv.w,cv.h,&folder.join(format!("{i:04}.png")));
                    last=clip.into();
                }
                std::fs::write(folder.join("trace.csv"),trace).unwrap();
                render_us.sort_unstable();
                std::fs::write(folder.join("raster.json"),serde_json::json!({"frames":120,"bitmap_changes":bitmap_changes,"body_raster_median_us":render_us[60],"body_raster_p95_us":render_us[114],"body_raster_max_us":render_us[119]}).to_string()).unwrap();
                println!("{}",folder.display());
            }
        }
    }
}

#[derive(Clone, Copy)]
enum Scene {
    Grab(f32),
    Enter(Edge),
    Idle(Edge),
    Notify(Edge),
    Detach(Edge),
}
impl Scene {
    fn duration(self) -> f64 {
        match self {
            Self::Grab(_) => 3.,
            Self::Enter(_) | Self::Detach(_) => 1.5,
            Self::Idle(Edge::Bottom) => 20.,
            Self::Idle(_) => 15.,
            Self::Notify(_) => 4.,
        }
    }
    fn edge(self) -> Option<Edge> {
        match self {
            Self::Grab(_) => None,
            Self::Enter(e) | Self::Idle(e) | Self::Notify(e) | Self::Detach(e) => Some(e),
        }
    }
}

#[test]
#[ignore = "WARP hat hinge coverage at actual sizes, no desktop capture"]
fn harmony_hat_joint_has_no_alpha_crack() {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    }
    for (s, dpi) in [(0.28, 1.), (0.56, 1.5)] {
        let cv = kit::Canvas::at(300., 360., dpi);
        let dc = unsafe {
            cv.dc
                .GetDevice()
                .unwrap()
                .CreateDeviceContext(D2D1_DEVICE_CONTEXT_OPTIONS_NONE)
                .unwrap()
        };
        let mut raster = unsafe { render::Raster::new(dc, cv.w, cv.h).unwrap() };
        let mut pixels = vec![0u8; (cv.w * cv.h * 4) as usize];
          let mut opaque_pixels = pixels.clone();
          for (frame,angle,blending) in ["peek_0", "sidepeek_0", "idle_0", "walk_1", "think_0", "jump_5", "stretch_2"]
              .into_iter().flat_map(|frame|[-14.,14.].into_iter().flat_map(move |angle|[false,true].map(move |blending|(frame,angle,blending)))) {
            let mut p = super::motion::Motion::new(0.).pose();
            p.frame = frame.into();
            p.hat_tilt = angle;
            p.continuous = true;
              if blending { p.blend = Some((frame.into(), 0.5)); }
            unsafe {
                raster
                    .draw(&p, 150., 320., s, dpi, false, 0., false, false, &mut pixels)
                    .unwrap();
            }
            let f = &clips::catalog().frames[frame];
            let m = render::body_matrix(&p, 150., 320., s);
            let x = ((f.hx * m.M11 + m.M31) * dpi).round() as usize;
            let y = ((render::hat_hinge(&p) * m.M22 + m.M32) * dpi).round() as usize;
            for row in y - 1..=y + 1 {
                let alpha = pixels[(row * cv.w as usize + x) * 4 + 3];
                assert!(
                    alpha >= 250,
                      "{frame} size{s}/dpi{dpi} blend{blending} hinge row{row}: alpha={alpha}"
                );
                  let red = pixels[(row * cv.w as usize + x) * 4 + 2];
                  if blending {
                      let reference = opaque_pixels[(row * cv.w as usize + x) * 4 + 2];
                      assert!(red.abs_diff(reference) <= 3,
                          "{frame}: blended hinge red={red}, opaque={reference}");
                  }
            }
              if !blending {opaque_pixels.copy_from_slice(&pixels);}
        }
    }
}

#[test]
#[ignore = "30fps production WARP frames, clip audit, no screen capture/input"]
fn harmony_companion_squishy() {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    }
    let root = kit::harmony_dir().join("companion-squishy");
    let scenes = [
        ("grab-hat", Scene::Grab(0.03)),
        ("grab-belly", Scene::Grab(0.5)),
        ("grab-foot", Scene::Grab(0.98)),
        ("enter-bottom", Scene::Enter(Edge::Bottom)),
        ("enter-left", Scene::Enter(Edge::Left)),
        ("enter-right", Scene::Enter(Edge::Right)),
        ("idle-bottom", Scene::Idle(Edge::Bottom)),
        ("idle-left", Scene::Idle(Edge::Left)),
        ("idle-right", Scene::Idle(Edge::Right)),
        ("notify-bottom", Scene::Notify(Edge::Bottom)),
        ("notify-left", Scene::Notify(Edge::Left)),
        ("detach-bottom", Scene::Detach(Edge::Bottom)),
        ("detach-right", Scene::Detach(Edge::Right)),
    ];
    for (s, dpi) in [(0.56, 1.5), (0.28, 1.)] {
        for light in [false, true] {
            let cv = kit::Canvas::at(480., 360., dpi);
            let (w, h) = (cv.w, cv.h);
            let dc = unsafe {
                cv.dc
                    .GetDevice()
                    .unwrap()
                    .CreateDeviceContext(D2D1_DEVICE_CONTEXT_OPTIONS_NONE)
                    .unwrap()
            };
            let mut raster = unsafe { render::Raster::new(dc, w, h).unwrap() };
            let mut pixels = vec![0u8; (w * h * 4) as usize];
            let r = RECT {
                left: (24. * dpi) as i32,
                top: (24. * dpi) as i32,
                right: (456. * dpi) as i32,
                bottom: (336. * dpi) as i32,
            };
            for (name, scene) in scenes {
                let folder = root.join(format!(
                    "{}-size{}-dpi{}-{name}",
                    if light { "light" } else { "dark" },
                    (s * 100.) as u32,
                    (dpi * 100.) as u32
                ));
                std::fs::create_dir_all(folder.join("frames")).unwrap();
                let mut clock = Runtime::default();
                clock.enter(0., 0.);
                if matches!(scene, Scene::Idle(_) | Scene::Notify(_)) {
                    clock.entered = None;
                }
                if matches!(scene, Scene::Notify(_)) {
                    clock.notify(0.);
                }
                let mut g = Grab::default();
                let pivot = match scene {
                    Scene::Grab(y) => y,
                    _ => 0.5,
                };
                let py = if pivot > 0.9 {
                    290.
                } else if pivot > 0.2 {
                    190.
                } else {
                    100.
                };
                g.down(
                    1,
                    Kind::Mouse,
                    Point::new(235. * dpi, py * dpi),
                    Point::new(0.5, pivot),
                    s * dpi,
                );
                g.moved(1, Point::new(240. * dpi, py * dpi));
                let frames = (scene.duration() * 30.) as usize;
                let mut trace = String::from(
                    "frame,time,depth,sx,sy,hat_degrees,tilt_degrees,outside_pixels\n",
                );
                for i in 0..frames {
                    let t = i as f64 / 30.;
                    let (p, x, y) = match scene {
                        Scene::Grab(_) => {
                            g.moved(
                                1,
                                Point::new(
                                    (240. + 80. * (t as f32 * std::f32::consts::TAU).sin()) * dpi,
                                    py * dpi,
                                ),
                            );
                            g.step(1. / 30.);
                            assert!(g.pose().tilt.abs() <= 25.001);
                            (g.pose(), g.point.x / dpi, g.point.y / dpi)
                        }
                        Scene::Detach(e) => {
                            if i == 0 {
                                clock.detach(0., e);
                            }
                            let mut p = g.pose();
                            clock.apply_detach(&mut p, t, true);
                            (p, 240., 190.)
                        }
                        _ => {
                            let e = scene.edge().unwrap();
                            let frame = clock.edge_frame(e, t, true, 0.);
                            let frame = if matches!(scene, Scene::Notify(Edge::Bottom))
                                && clock.active_notification(t)
                            {
                                clips::catalog().clip("notify").frame(t as f32)
                            } else {
                                frame
                            };
                            clock.animated_pose(
                                e,
                                r,
                                if e == Edge::Bottom {
                                    240. * dpi
                                } else {
                                    300. * dpi
                                },
                                frame,
                                s,
                                dpi,
                                t,
                                true,
                            )
                        }
                    };
                    unsafe {
                        raster
                            .draw(&p, x, y, s, dpi, false, t as f32, false, light, &mut pixels)
                            .unwrap();
                    }
                    let clipped = scene.edge().is_some() && !matches!(scene, Scene::Detach(_));
                    if clipped {
                        peek::clip_pixels(&mut pixels, w, (0, 0), r);
                    }
                    let mut outside = 0;
                    let mut ink = 0;
                    for (j, pixel) in pixels.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                        let (px, py) = ((j % w as usize) as i32, (j / w as usize) as i32);
                        let inside = px >= r.left && px < r.right && py >= r.top && py < r.bottom;
                        if pixel[3] > 0 {
                            ink += 1;
                            if !inside {
                                outside += 1;
                            }
                        }
                        let bg = if inside {
                            if light {
                                196u16
                            } else {
                                58u16
                            }
                        } else if light {
                            225
                        } else {
                            28
                        };
                        let inverse = 255 - pixel[3] as u16;
                        for channel in &mut pixel[..3] {
                            *channel =
                                (*channel as u16 + (bg * inverse + 127) / 255).min(255) as u8;
                        }
                        pixel[3] = 255;
                    }
                    assert!(ink > 0, "empty {name} frame {i}");
                    if clipped {
                        assert_eq!(outside, 0, "{name} frame {i}: outside monitor");
                    }
                    trace += &format!(
                        "{i},{t:.6},{:.6},{:.6},{:.6},{:.6},{:.6},{outside}\n",
                        scene.edge().map_or(1., |e| clock.edge_depth(e, t, true)),
                        p.sx,
                        p.sy,
                        p.hat_tilt,
                        p.tilt
                    );
                    kit::save_png(
                        &pixels,
                        w,
                        h,
                        &folder.join("frames").join(format!("{i:04}.png")),
                    );
                }
                std::fs::write(folder.join("trace.csv"), trace).unwrap();
                std::fs::write(folder.join("frames.json"),serde_json::json!({"scenario":name,"frames":frames,"fps":30,"scale":s,"dpi":dpi,"light":light,"clip_checked":scene.edge().is_some()&&!matches!(scene,Scene::Detach(_))}).to_string()).unwrap();
                println!("{}: {frames} production frames", folder.display());
            }
        }
    }
}
