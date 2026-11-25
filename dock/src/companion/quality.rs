//! qcheck thresholds, applied to the shipped full images (WIC returns PBGRA).
use super::clips;
fn check(bytes: &[u8], components:usize, light_ink:bool, ink_tip:Option<(usize,usize)>) -> [usize; 4] {
    check_with_wall(bytes, components, light_ink, ink_tip, None)
}
fn check_with_wall(bytes: &[u8], components:usize, light_ink:bool, ink_tip:Option<(usize,usize)>, wall:Option<usize>) -> [usize; 4] {
    let im = crate::icons::decode_bytes_fit(bytes, 4096).unwrap();
    let (w, h) = (im.width as usize, im.height as usize);
    let rgba: Vec<_> = im.pixels.chunks_exact(4).map(|p| {
        let a = p[3] as f32;
        let rgb = [p[2],p[1],p[0]].map(|c| if a == 0.0 {0.0} else { (c as f32 * 255.0 / a).min(255.0) });
        let l = if light_ink {0.}else{(rgb[0]*0.299+rgb[1]*0.587+rgb[2]*0.114).round()};
        let max = rgb.into_iter().fold(0.0f32, f32::max);
        let min = rgb.into_iter().fold(255.0f32, f32::min);
        let sat = if max == 0.0 {0.0} else {(max-min)/max*255.0};
        (a, l, sat, rgb)
    }).collect();
    let at = |x: isize,y: isize| y.clamp(0,h as isize-1) as usize*w+x.clamp(0,w as isize-1) as usize;
    let mut out = [0;4];
    for y in 0..h { for x in 0..w {
        let (a,l,_,_) = rgba[y*w+x];
        // The sidepeek beard meets the occluding wall without an ink outline.
        // Only that exact source column may expose white; all other checks apply.
        if wall!=Some(x) && a>120.0 && l>190.0 && (-1..=1).any(|dy|(-1..=1).any(|dx|rgba[at(x as isize+dx,y as isize+dy)].0<40.0)) {out[0]+=1;}
        if l<135.0 {
            let n = (-3..=3).flat_map(|dy|(-3..=3).map(move |dx|(dx,dy))).filter(|&(dx,dy)| {
                let (a,l,s,_) = rgba[at(x as isize+dx,y as isize+dy)]; a>200.0 && l>150.0 && l<205.0 && s<40.0
            }).count();
            if (n as f32*255.0/49.0).round()>170.0 && ink_tip != Some((x,y)) {out[1]+=1;}
        }
    }}
    // qcheck's hat alpha comes from cutout.py: red dilated by one pixel
    // OR dark (<170), intersected with near-red. For white (>200), only
    // that one-pixel red dilation can contribute. Reconstruct it from full.
    for y in 0..h {for x in 0..w {
        let (a,l,_,_)=rgba[y*w+x];
        if a>120.0 && l>200.0 && (-1..=1).any(|dy|(-1..=1).any(|dx| {
            let (_,_,_,r)=rgba[at(x as isize+dx,y as isize+dy)]; r[0]-r[1]>85.0 && r[0]>120.0
        })) {out[2]+=1;}
    }}
    let mut seen=vec![false;w*h]; let mut sizes=Vec::new();
    for i in 0..w*h {if seen[i]||rgba[i].0<=24.0 {continue;} let mut stack=vec![i];seen[i]=true;let mut n=0;
        while let Some(p)=stack.pop(){n+=1;let x=p%w;let y=p/w;for (xx,yy) in [(x as isize-1,y as isize),(x as isize+1,y as isize),(x as isize,y as isize-1),(x as isize,y as isize+1)] {if xx<0||yy<0||xx>=w as isize||yy>=h as isize {continue;}let j=yy as usize*w+xx as usize;if !seen[j]&&rgba[j].0>24.0 {seen[j]=true;stack.push(j);}}}
        sizes.push(n);
    }
    sizes.sort_unstable_by(|a,b|b.cmp(a));
    out[3]=sizes.iter().skip(components).sum();
    out
}
#[test]
fn approved_full_frames_pass_four_quality_checks() {
    unsafe {let _=windows::Win32::System::Com::CoInitializeEx(None,windows::Win32::System::Com::COINIT_APARTMENTTHREADED);}
    // The approved v3 manifest also retains unused legacy still-pose metadata.
    let used: std::collections::BTreeSet<_> = clips::catalog().clips.values().flat_map(|c| &c.frames).collect();
    let failures: Vec<_> = used.into_iter().filter_map(|n| {
        let wall = (n == "sidepeek_0").then(|| clips::catalog().frames[n].edge.unwrap().floor() as usize);
        let counts = check_with_wall(clips::png(n,"full").unwrap(),1,false,approved_ink_tip(n),wall);
        (counts != [0;4]).then_some((n,counts))
    }).collect();
    assert!(failures.is_empty(), "edge/speck/hatwhite/stray: {failures:?}");
}

#[test]
fn sidepeek_open_beard_is_only_allowed_at_the_measured_wall_column() {
    unsafe {let _=windows::Win32::System::Com::CoInitializeEx(None,windows::Win32::System::Com::COINIT_APARTMENTTHREADED);}
    let bytes=clips::png("sidepeek_0","full").unwrap();
    let wall=clips::catalog().frames["sidepeek_0"].edge.unwrap().floor() as usize;
    let raw=check(bytes,1,false,None);
    assert!(raw[0]>0);
    assert_eq!(&raw[1..],&[0,0,0]);
    assert_eq!(check_with_wall(bytes,1,false,None,Some(wall)),[0;4]);
    assert_eq!(check_with_wall(bytes,1,false,None,Some(wall-1)),raw);
    assert_eq!(check_with_wall(bytes,1,false,None,Some(wall+1)),raw);
}

// Approved by the art lead (2026-11-13): these exact pixels are the drawn
// endpoints of the envelope's V-shaped flap, not isolated grey specks.
// Keep the PNGs intact. No other frame or coordinate has an ink-tip exemption.
fn approved_ink_tip(frame:&str)->Option<(usize,usize)> {
    match frame {"carry_1"=>Some((171,61)),"carry_3"=>Some((164,74)),"carry_7"=>Some((184,64)),_=>None}
}
#[test]
fn envelope_ink_exceptions_are_exact_coordinates_only() {
    unsafe {let _=windows::Win32::System::Com::CoInitializeEx(None,windows::Win32::System::Com::COINIT_APARTMENTTHREADED);}
    for name in ["carry_1","carry_3","carry_7"] {
        let bytes=clips::png(name,"full").unwrap();
        let (x,y)=approved_ink_tip(name).unwrap();
        assert_eq!(check(bytes,1,false,None),[0,1,0,0]);
        assert_eq!(check(bytes,1,false,Some((x+1,y))),[0,1,0,0]);
        assert_eq!(check(bytes,1,false,Some((x,y))),[0;4]);
    }
    assert_eq!(approved_ink_tip("carry_0"),None);
}

#[test]
fn particles_pass_four_quality_checks(){
    unsafe {let _=windows::Win32::System::Com::CoInitializeEx(None,windows::Win32::System::Com::COINIT_APARTMENTTHREADED);}
    for n in super::fx::NAMES {assert_eq!(check(clips::fx_png(n).unwrap(),if n=="dots"{3}else{1},false,None),[0;4],"{n}: edge/speck/hatwhite/stray (dots has three intentional glyphs)");}
}

#[test]fn light_ink_preserves_alpha_without_strays(){
    unsafe{let _=windows::Win32::System::Com::CoInitializeEx(None,windows::Win32::System::Com::COINIT_APARTMENTTHREADED);}
    for n in ["tick","z","note","squiggle","dots"]{let key=format!("{n}_light");let bytes=clips::fx_png(&key).unwrap();assert_eq!(check(bytes,if n=="dots"{3}else{1},true,None),[0;4]);let base=crate::icons::decode_bytes_fit(clips::fx_png(n).unwrap(),4096).unwrap();let light=crate::icons::decode_bytes_fit(bytes,4096).unwrap();assert_eq!((base.width,base.height),(light.width,light.height));assert_eq!(base.pixels.chunks_exact(4).map(|p|p[3]).collect::<Vec<_>>(),light.pixels.chunks_exact(4).map(|p|p[3]).collect::<Vec<_>>(),"{n}: intentional bright ink retains exact coverage");}
}
