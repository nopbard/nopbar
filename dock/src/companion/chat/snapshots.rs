//! Offscreen-only companion bubble acceptance images.
use super::{bubbles::{Bubbles,Phase},paint::{self,Input},history::History,history_view};
use crate::{companion::{anchors::Anchors,motion::Motion,render},gfx::promo_kit as kit};
use windows::Win32::Foundation::RECT;
use windows_numerics::Vector2;
unsafe extern "system" fn hidden_proc(h:windows::Win32::Foundation::HWND,m:u32,w:windows::Win32::Foundation::WPARAM,l:windows::Win32::Foundation::LPARAM)->windows::Win32::Foundation::LRESULT {
    windows::Win32::UI::WindowsAndMessaging::DefWindowProcW(h,m,w,l)
}

pub(crate) fn gnome_fixes_history(stage: &kit::Stage, root: &std::path::Path, system_light: bool, wallpaper_light: bool) {
    let bg = kit::harmony_backdrop(wallpaper_light);
    let _wallpaper = crate::glass::StandIn::new(bg);
    let mut history = History::default();
    history.push("오늘 일정 알려줘".into(), "오전 9시에 제품 검토가 있어.".into());
    let input = Input {text:"",caret:0,selection:(0,0),composition:None};
    let work = RECT {left:0,top:0,right:900,bottom:760};
    let mut view = unsafe {history_view::View::new(stage.gfx.clone(), kit::theme(system_light), 1., work, Some(hidden_proc)).unwrap()};
    let w = (history_view::WIDTH + 2. * history_view::MARGIN) as i32;
    let h = (history_view::HEIGHT + 2. * history_view::MARGIN) as i32;
    for (editing, name) in [(Some(&input), "history"), (None, "history-read-only")] {
        view.draw(&history, None, editing, false, false).unwrap();
        let mut px = unsafe {crate::popup::chrome_harmony::read_back(&view.root, w, h).expect("offscreen history visual")};
        for p in px.as_chunks_mut::<4>().0 {
            let outside = u16::from(255 - p[3]);
            for (channel, backdrop) in p[..3].iter_mut().zip([bg.b, bg.g, bg.r]) {
                *channel = (u16::from(*channel) + (u16::from(backdrop) * outside + 127) / 255).min(255) as u8;
            }
            p[3] = 255;
        }
        kit::save_png(&px, w as u32, h as u32, &root.join(format!("{name}.png")));
    }
}

#[test]
#[ignore="offscreen bubble acceptance PNGs; no desktop capture, input or provider calls"]
fn harmony_companion_bubble() {
    let _language=crate::i18n::test_lock("ko");
    let stage=kit::stage();let gfx=&stage.gfx;
    let root=std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/ui-snapshots/companion-bubble");
    std::fs::create_dir_all(&root).unwrap();
    let mut index=String::from("# Companion bubble offscreen PNGs\n\nShared runtime D2D/DirectWrite renderer. No live AI, screen capture or input injection.\n\n");
    let mut geometry=String::from("file,tip_x,tip_y,bottom,gap,spinner_x,spinner_y\n");
    for scale in [0.28,0.56] {for dpi in [1.,1.5] {for light in [true,false] {
        let folder=format!("{}-size{}-dpi{}",if light{"light"}else{"dark"},(scale*100.) as u32,(dpi*100.) as u32);
        for state in ["empty","short-input","wrapped-input","think-2","think-6","short-reply","long-reply","stack","right-edge","mcp-say"] {
            let cv=kit::Canvas::at(400.,480.,dpi);let mut pose=Motion::new(200.).pose();
            pose.frame=match state {"think-2"=>"think_2","think-6"=>"think_6","empty"|"short-input"|"wrapped-input"=>"look_4",_=>"wait_0"}.into();
            let x=if state=="right-edge"{380.}else{200.};let foot=(x,450.);
            let a=Anchors::load(&pose.frame).posed(&pose,1.);let tip=(x+a.tip.0*scale,450.+a.tip.1*scale);
            let mut b=Bubbles::default();let mut input=String::new();
            match state {
                "empty"=>b.input(0.),
                "short-input"=>{b.input(0.);input="오늘 뭐 할까".into();},
                "wrapped-input"=>{b.input(0.);input="오늘은 노움과 함께 아주 긴 이야기를 입력하고 있어. 폭이 280을 넘으면 다음 줄로 이어져. 네 줄 이상에서도 커서는 보여야 해.".into();},
                "think-2"|"think-6"=>{b.input(0.);b.submit("오늘 뭐 할까".into(),0.);},
                "short-reply"=>{b.delta("안녕!",0.);b.finish(0.);},
                "long-reply"=>{b.delta("첫 번째 줄\n두 번째 줄\n세 번째 줄\n네 번째 줄\n다섯 번째 줄\n마지막 네 줄이 보이는 긴 답변이야.",0.);b.finish(0.);},
                "stack"=>{b.say("안녕! 오늘은 좀 한가해 보이네.".into(),0.);b.input(0.);b.submit("오늘 뭐 할까".into(),0.);b.delta("어제 하다 만 정리부터 끝내자. 그다음에 산책 한 바퀴 어때?",0.);b.finish(0.);},
                "right-edge"=>{b.delta("화면 끝에서도 말풍선은 안쪽에 있어.",0.);b.finish(0.);},
                "mcp-say"=>b.say("작업을 마쳤어.".into(),0.),_=>unreachable!(),
            }
            let editing=Input{text:&input,caret:input.encode_utf16().count() as u32,selection:(0,0),composition:None};
            let items=paint::arrange(gfx,&b,&editing,tip,paint::rect(0.,0.,400.,480.),1.,true,dpi).unwrap();
            unsafe {
                cv.dc.Clear(Some(&kit::col(kit::harmony_backdrop(light))));
                render::shadow(&cv.dc,&pose,x,450.,scale,light).unwrap();
                render::Painter::new().paint(&cv.dc,&pose,x,450.,scale,false,0.,dpi).unwrap();
                for item in &items {paint::item(&cv.dc,item,(b.phase==Phase::Input && item.depth==0).then_some(&editing),true,&kit::theme(light)).unwrap();}
                if b.phase==Phase::Thinking {paint::spinner(&cv.dc,foot,scale,0.45,true,&kit::theme(light)).unwrap();}
            }
            let path=format!("{folder}/{state}.png");let(px,w,h)=cv.finish();kit::save_png(&px,w,h,&root.join(&path));
            index+=&format!("- [{folder} / {state}]({path})\n");
            let bottom=items.first().map_or(tip.1-8.,|i|i.bounds.bottom);let(sp,_,_)=super::bubbles::spinner(foot,scale);
            geometry+=&format!("{path},{:.3},{:.3},{bottom:.3},{:.3},{:.3},{:.3}\n",tip.0,tip.1,tip.1-bottom,sp.0,sp.1);
            if b.phase!=Phase::Thinking {assert!((tip.1-bottom-8.).abs()<=1./dpi+0.01,"{state}");}
        }
    }}}
    for dpi in [1.,1.5] {for light in [true,false] {
        let theme=kit::theme(light);let mut h=History::default();h.push("안녕 노움".into(),"안녕! 오늘은 좀 한가해 보이네.".into());h.push("오늘 뭐 할까".into(),"어제 하다 만 정리부터 끝내자. 그다음에 산책 한 바퀴 어때?".into());
        let input=Input{text:"",caret:0,selection:(0,0),composition:None};
        // Build the actual glass/chrome tree in an unshown HWND. Read only this visual.
        let _stand=crate::glass::StandIn::new(kit::harmony_backdrop(light));
        let mut view=unsafe{history_view::View::new(gfx.clone(),theme,dpi,RECT{left:0,top:0,right:1000,bottom:1000},Some(hidden_proc)).unwrap()};
        view.draw(&h,None,Some(&input),false,false).unwrap();
        view.root.SetSize(Vector2{X:(history_view::WIDTH+48.)*dpi,Y:(history_view::HEIGHT+48.)*dpi}).unwrap();
        let(w,h)=(((history_view::WIDTH+48.)*dpi) as u32,((history_view::HEIGHT+48.)*dpi) as u32);
        let px=unsafe{crate::popup::chrome_harmony::read_back(&view.root,w as i32,h as i32)}.expect("history composition offscreen readback");
        let name=format!("history-{}-dpi{}.png",if light{"light"}else{"dark"},(dpi*100.) as u32);
        kit::save_png(&px,w,h,&root.join(&name));index+=&format!("- [{name}]({name})\n");
        let long=(1..=36).map(|i|format!("{i:02}번째 기록 줄 · 가나다 ABC 123")).collect::<Vec<_>>().join("\n");
        let mut b=Bubbles::default();b.say(long.clone(),0.);let message=b.current.clone().unwrap();
        // Separate history variable: the pixel dimensions above intentionally shadowed h.
        let mut archive=History::default();archive.push("긴 답을 읽기".into(),long);archive.link_last(None,Some(message.id));
        view.motion=false;view.reveal(message);
        for state in ["top","middle","bottom"] {
            if state=="middle" {view.wheel(-3.);}else if state=="bottom" {view.wheel(-1000.);}
            view.draw(&archive,None,Some(&input),false,false).unwrap();
            let px=unsafe{crate::popup::chrome_harmony::read_back(&view.root,w as i32,h as i32)}.expect("history scroll offscreen readback");
            let name=format!("history-scroll-{state}-{}-dpi{}.png",if light{"light"}else{"dark"},(dpi*100.) as u32);
            kit::save_png(&px,w,h,&root.join(&name));index+=&format!("- [{name}]({name})\n");
        }
    }}
    std::fs::write(root.join("INDEX.md"),index).unwrap();std::fs::write(root.join("geometry.csv"),geometry).unwrap();
}

/// Motion check: frames at 30 fps through push-up, eviction, thinking, reply and exit.
#[test]
#[ignore="offscreen bubble motion frames; no desktop capture, input or provider calls"]
fn harmony_companion_bubble_motion() {
    let _language=crate::i18n::test_lock("ko");
    let stage=kit::stage();let gfx=&stage.gfx;
    let root=std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/ui-snapshots/companion-bubble-motion");
    std::fs::create_dir_all(&root).unwrap();
    let (scale,dpi,light)=(0.56,1.,false);let x=200.;let foot=(x,450.);
    // Timeline: g1 says (0), u1 sent (2), g2 answers (3), u2 typed (6), sent (8), g3 answers (9), finished (9.2).
    let build=|upto:f64|->(Bubbles,String){
        let mut b=Bubbles::default();let mut typed=String::new();
        b.say("안녕! 나는 이 PC에 사는 노움이야.".into(),0.);
        if upto>=1.{b.input(1.);}
        if upto>=2.{b.submit("오늘 뭐 할까".into(),2.);}
        if upto>=3.{b.delta("어제 하다 만 정리부터 끝내자.".into(),3.);b.finish(3.2);}
        if upto>=6.{b.input(6.);typed="그다음엔?".into();}
        if upto>=8.{b.submit("그다음엔?".into(),8.);typed.clear();}
        if upto>=9.{b.delta("산책 한 바퀴 어때? 바람 쐬면 머리가 맑아져.".into(),9.);b.finish(9.2);}
        (b,typed)
    };
    let mut n=0;
    let exit=build(10.).0.deadline().unwrap();
    for (from,to) in [(5.95,6.5),(7.95,8.6),(8.95,9.6),(exit-0.1,exit+0.7)] {
        let mut t=from;
        while t<to {
            let (b,typed)=build(t);
            let cv=kit::Canvas::at(400.,480.,dpi);let mut pose=Motion::new(x).pose();
            pose.frame=if b.phase==Phase::Thinking {format!("think_{}",((t*1000./150.) as usize)%8)} else {"wait_0".into()};
            let a=Anchors::load(&pose.frame).posed(&pose,1.);let tip=(x+a.tip.0*scale,450.+a.tip.1*scale);
            let editing=Input{text:&typed,caret:typed.encode_utf16().count() as u32,selection:(0,0),composition:None};
            let items=paint::arrange(gfx,&b,&editing,tip,paint::rect(0.,0.,400.,480.),t,true,dpi).unwrap();
            unsafe {
                cv.dc.Clear(Some(&kit::col(kit::harmony_backdrop(light))));
                render::shadow(&cv.dc,&pose,x,450.,scale,light).unwrap();
                render::Painter::new().paint(&cv.dc,&pose,x,450.,scale,false,0.,dpi).unwrap();
                for item in items.iter().rev() {paint::item(&cv.dc,item,(b.phase==Phase::Input && item.depth==0).then_some(&editing),true,&kit::theme(light)).unwrap();}
                if b.phase==Phase::Thinking {paint::spinner(&cv.dc,foot,scale,t,true,&kit::theme(light)).unwrap();}
            }
            let(px,w,h)=cv.finish();kit::save_png(&px,w,h,&root.join(format!("f{n:03}.png")));
            n+=1;t+=1./30.;
        }
    }
    scroll_frames(gfx,&root);
}

fn scroll_frames(gfx:&crate::gfx::Gfx,root:&std::path::Path) {
    let text=(1..=12).map(|i|format!("{i:02}번째 줄 · 가나다 ABC 123")).collect::<Vec<_>>().join("\n");
    let input=Input{text:"",caret:0,selection:(0,0),composition:None};
    let mut evidence=String::from("file,scroll,max_scroll,dots,lifetime,alpha\n");
    for scale in [0.28,0.56] {for dpi in [1.,1.5] {for light in [true,false] {
        let folder=root.join(format!("scroll-{}-size{}-dpi{}",if light{"light"}else{"dark"},(scale*100.) as u32,(dpi*100.) as u32));
        std::fs::create_dir_all(&folder).unwrap();
        let mut b=Bubbles::default();
        let mut pose=Motion::new(200.).pose();pose.frame="idle_0".into();
        let anchors=Anchors::load(&pose.frame).posed(&pose,1.);
        let tip=(200.+anchors.tip.0*scale,450.+anchors.tip.1*scale);
        let mut frame=|name:&str,b:&Bubbles,now:f64| {
            let cv=kit::Canvas::at(400.,480.,dpi);
            let items=paint::arrange(gfx,b,&input,tip,paint::rect(0.,0.,400.,480.),now,true,dpi).unwrap();
            unsafe {
                cv.dc.Clear(Some(&kit::col(kit::harmony_backdrop(light))));
                render::shadow(&cv.dc,&pose,200.,450.,scale,light).unwrap();
                render::Painter::new().paint(&cv.dc,&pose,200.,450.,scale,false,0.,dpi).unwrap();
                for it in &items {paint::item(&cv.dc,it,None,false,&kit::theme(light)).unwrap();}
            }
            if let Some(it)=items.first() {evidence+=&format!("{}/{name}.png,{},{},{},{},{}\n",folder.file_name().unwrap().to_string_lossy(),it.text.scroll,it.text.max_scroll,it.overflow_done,b.lifetime,it.alpha);}
            let(px,w,h)=cv.finish();kit::save_png(&px,w,h,&folder.join(format!("{name}.png")));
        };
        for (n,name) in [(4,"01-stream-4"),(8,"02-stream-8"),(12,"03-stream-12")] {
            b=Bubbles::default();b.delta(&text.lines().take(n).collect::<Vec<_>>().join("\n"),0.);frame(name,&b,1.);
        }
        b.finish(1.);frame("04-finished-bottom",&b,1.5);
        let id=b.current.as_ref().unwrap().id;
        b.message_mut(id).unwrap().wheel(4.,160.,160.,2.,true);
        frame("05-wheel-middle-40ms",&b,2.04);frame("06-wheel-middle-80ms",&b,2.081);
        b.message_mut(id).unwrap().wheel(100.,80.,160.,3.,true);frame("07-wheel-top",&b,3.081);
        frame("08-three-dots",&b,3.2);
        // A real working signal passed through the model still renders the idle clip.
        let now=std::time::Instant::now();let mut model=crate::companion::model::Model::new(now);
        model.observe(crate::companion::model::Signal{working:true,..Default::default()},now);
        assert_eq!(model.update(now,crate::companion::model::Config::default()),crate::companion::model::State::Idle);
        let mut motion=Motion::new(200.);motion.set_state(model.state);assert!(motion.pose().frame.starts_with("idle_"));
        frame("09-agent-working-idle",&Bubbles::default(),1.);
        for (kind,content) in [("8sec","짧".to_string()),("22sec","긴".repeat(233))] {
            let mut timed=Bubbles::default();timed.delta(&content,0.);timed.finish(0.);
            for now in [8.,9.,22.,23.] {frame(&format!("lifetime-{kind}-t{now:02}"),&timed,now);}
        }
    }}}
    std::fs::write(root.join("scroll-geometry.csv"),evidence).unwrap();
}
