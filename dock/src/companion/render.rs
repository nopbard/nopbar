//! Shared by the layered window and harmony: approved PNGs, no recoloring.
use super::{clips, motion::Pose};
use std::collections::HashMap;
use windows::core::{Error, Result};
use windows::Win32::Foundation::E_FAIL;
use windows::Win32::Graphics::Direct2D::{Common::*, *};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows_numerics::{Matrix3x2, Vector2};

fn particle_name(name: &'static str, light: bool) -> &'static str {
    if light { return name; }
    match name {
        "tick" => "tick_light", "z" => "z_light", "note" => "note_light",
        "squiggle" => "squiggle_light", "dots" => "dots_light", _ => name,
    }
}

/// Rasterize only a new clip frame or a changed rendering configuration.
/// Position and entry springs alone never invalidate the bitmap.
#[derive(Clone, Debug, PartialEq)]
pub struct Stamp {
    frame: String,
    scale: u32,
    dpi: u32,
    flip: bool,
    boil: bool,
    light: bool,
    deform:Option<[u32;4]>,
    angle:f32,
    hat_tilt: u32,
    pivot:Option<(u32,u32)>,
    blend:Option<(String,u32)>,
}
impl Stamp {
    pub fn new(p: &Pose, scale: f32, dpi: f32, boil: bool, light: bool) -> Self {
        Self {
            frame: p.frame.clone(),
            scale: scale.to_bits(),
            dpi: dpi.to_bits(),
            flip: p.flip,
            boil,
            light,
            deform:p.continuous.then(||[p.sx.to_bits(),p.sy.to_bits(),p.skew.to_bits(),p.jy.to_bits()]),
            angle:p.tilt,
            hat_tilt:p.hat_tilt.to_bits(),
            pivot:p.pivot.map(|(x,y)|(x.to_bits(),y.to_bits())),
            blend:p.blend.as_ref().map(|(f,a)|(f.clone(),a.to_bits())),
        }
    }
    /// Compare with the last raster, not the last requested pose: small angle
    /// deltas accumulate until they actually require new pixels.
    pub fn same_bitmap(&self,other:&Self)->bool {
        self.frame==other.frame&&self.scale==other.scale&&self.dpi==other.dpi&&self.flip==other.flip
            &&self.boil==other.boil&&self.light==other.light&&self.deform==other.deform
            &&self.blend==other.blend&&self.pivot==other.pivot&&self.hat_tilt==other.hat_tilt
            &&((self.angle-other.angle+180.).rem_euclid(360.)-180.).abs()<0.25
    }

}

pub fn boil_parameters(dpi: f32) -> (f32, f32) {
    (0.035 * dpi, 2.2 / dpi)
}

/// Particle surfaces also move without readback while their local pixels match.
#[derive(PartialEq)]
pub struct ParticleStamp {scale:u32,dpi:u32,light:bool,parts:Vec<(&'static str,[u32;5],Option<[u32;8]>)>}
impl ParticleStamp {
    pub fn new(parts:&[super::fx::Particle],gx:f32,gy:f32,s:f32,dpi:f32,origin:(f32,f32),light:bool)->Self {
        let coordinate=|v:f32|((v*1024.).round()/1024.).to_bits();
        let parts=parts.iter().map(|p|{
            let (x,y,scale,angle,alpha)=p.visual(gx,gy,s);
            let line=p.gloom_line(gx,gy,s).map(|l|[coordinate(l.from.0-origin.0),coordinate(l.from.1-origin.1),coordinate(l.to.0-origin.0),coordinate(l.to.1-origin.1),coordinate(l.fade_end.0-origin.0),coordinate(l.fade_end.1-origin.1),l.width.to_bits(),l.opacity.to_bits()]);
            (p.name,[coordinate(x-origin.0),coordinate(y-origin.1),scale.to_bits(),angle.to_bits(),alpha.to_bits()],line)
        }).collect();
        Self{scale:s.to_bits(),dpi:dpi.to_bits(),light,parts}
    }
}

/// The cached bitmap is reused; only the two source rectangles are transformed.
unsafe fn bent_bitmap(dc:&ID2D1DeviceContext,bitmap:&ID2D1Bitmap1,p:&Pose,world:Matrix3x2,s:f32,dpi:f32,alpha:f32) {
    let f=&clips::catalog().frames[&p.frame];
    let hinge=hat_hinge(p);
    let overlap=1./(s*dpi);
    let lower=D2D_RECT_F{left:0.,top:hinge-overlap,right:f.w as f32,bottom:f.h as f32};
    let upper=D2D_RECT_F{left:0.,top:0.,right:f.w as f32,bottom:hinge};
    dc.SetTransform(&world);
    let blend=dc.GetPrimitiveBlend();
    if alpha<1. {
        // Composite the overlapping halves at full opacity first. Applying
        // crossfade opacity to each half creates a bright additive seam.
        dc.PushLayer(&D2D1_LAYER_PARAMETERS1 {
            contentBounds:D2D_RECT_F{left:-10000.,top:-10000.,right:10000.,bottom:10000.},
            geometricMask:std::mem::ManuallyDrop::new(None),maskAntialiasMode:D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
            maskTransform:Matrix3x2::identity(),opacity:alpha,opacityBrush:std::mem::ManuallyDrop::new(None),
            layerOptions:D2D1_LAYER_OPTIONS1_NONE,
        },None);
        dc.SetPrimitiveBlend(D2D1_PRIMITIVE_BLEND_SOURCE_OVER);
    }
    dc.DrawBitmap(bitmap,Some(&lower),1.,D2D1_INTERPOLATION_MODE_LINEAR,Some(&lower),None);
    dc.SetTransform(&(hat_matrix(p)*world));
    dc.DrawBitmap(bitmap,Some(&upper),1.,D2D1_INTERPOLATION_MODE_LINEAR,Some(&upper),None);
    if alpha<1. {dc.SetPrimitiveBlend(blend);dc.PopLayer();}
}

#[cfg(test)]
mod review_tests {
    use super::*;
    #[test]
    fn unchanged_particle_pixels_move_without_raster_but_visual_changes_invalidate() {
        let mut fx=crate::companion::fx::Engine::default();let mut rng=crate::companion::fx::Rng(42);
        fx.burst("question",&mut rng,100.,200.,false);
        for p in &mut fx.parts {p.t=0.2;}
        assert!(!fx.parts.is_empty());
        let a=ParticleStamp::new(&fx.parts,100.,200.,0.56,1.,(50.,100.),true);
        assert!(a==ParticleStamp::new(&fx.parts,150.,230.,0.56,1.,(100.,130.),true));
        for p in &mut fx.parts {p.t+=0.1;}
        assert!(a!=ParticleStamp::new(&fx.parts,100.,200.,0.56,1.,(50.,100.),true));
    }
    #[test]
    fn position_only_presents_without_raster_and_small_rotation_accumulates() {
        let mut grab=crate::companion::grab::Grab::default();
        grab.down(1,crate::companion::grab::Kind::Mouse,crate::companion::grab::Point::new(100.,100.),crate::companion::grab::Point::new(0.5,0.1),0.56);
        grab.moved(1,crate::companion::grab::Point::new(120.,100.));
        let mut p=grab.pose();let old=Stamp::new(&p,0.56,1.,false,true);let angle=p.tilt;
        let mut rasters=0;let mut presents=0;
        for delta in [0.,0.1,0.2] {
            p.x+=50.;p.tilt=angle+delta;
            if !old.same_bitmap(&Stamp::new(&p,0.56,1.,false,true)){rasters+=1;}
            presents+=1;
        }
        assert_eq!(rasters,0);assert_eq!(presents,3);
        p.tilt=angle+0.26;assert!(!old.same_bitmap(&Stamp::new(&p,0.56,1.,false,true)));
        p.tilt=angle;p.pivot=Some((0.4,0.1));assert!(!old.same_bitmap(&Stamp::new(&p,0.56,1.,false,true)));
        p.pivot=Some((0.5,0.1));p.sy+=0.01;assert!(!old.same_bitmap(&Stamp::new(&p,0.56,1.,false,true)));
    }
    #[test]
    #[ignore = "offscreen WARP resize and decoded bitmap retention; no HWND"]
    fn harmony_companion_resize_cache() {
        use crate::companion::{fx,motion::Motion};
        use windows::Win32::System::Com::{CoInitializeEx,COINIT_APARTMENTTHREADED};
        unsafe {
            let _=CoInitializeEx(None,COINIT_APARTMENTTHREADED);
            let cv=crate::gfx::promo_kit::Canvas::at(340.,340.,1.);
            let dc=cv.dc.GetDevice().unwrap().CreateDeviceContext(D2D1_DEVICE_CONTEXT_OPTIONS_NONE).unwrap();
            let mut raster=Raster::new(dc.clone(),340,340).unwrap();
            let pose=Motion::new(170.).pose();
            let mut fx=fx::Engine::default();let mut rng=fx::Rng(42);
            let mut pixels=vec![0;340*340*4];
            raster.draw(&pose,170.,300.,0.55,1.,true,0.,false,true,&mut pixels).unwrap();
            for name in fx::NAMES.into_iter().chain(["ticks_right"]) {
                fx.clear();fx.burst(name,&mut rng,170.,300.,false);
                for p in &mut fx.parts {p.t=0.2;}
                for light in [true,false] {raster.draw_particles(&fx.parts,170.,300.,0.55,1.,(0.,0.),light,&mut pixels).unwrap();}
            }
            let body=raster.painter.bitmaps.clone();let particles=raster.painter.fx_bitmaps.clone();
            for angle in [-14.,-7.,0.,7.,14.] {
                let mut bent=pose.clone();bent.hat_tilt=angle;
                raster.draw(&bent,170.,300.,0.55,1.,false,0.,false,true,&mut pixels).unwrap();
                assert_eq!(body,raster.painter.bitmaps,"hat bending must reuse decoded/scaled bitmaps");
            }
            let noise=raster.painter.boil.as_ref().unwrap().noise.clone();
            assert!(particles.contains_key("z_light") && !particles.contains_key("tick_light"));
            for side in [600,340,340,510] {
                let old=raster.target.clone();
                let unchanged=raster.w==side;
                raster.resize(side,side).unwrap();
                if unchanged {assert_eq!(old,raster.target);} else {assert_ne!(old,raster.target);}
                pixels.resize((side*side*4) as usize,0);
                raster.draw(&pose,170.,300.,0.55,1.,true,0.,false,true,&mut pixels).unwrap();
                assert!(pixels.chunks_exact(4).any(|p|p[3]>0));
                raster.draw_particles(&fx.parts,170.,300.,0.55,1.,(0.,0.),false,&mut pixels).unwrap();
                assert!(pixels.chunks_exact(4).any(|p|p[3]>0));
                assert_eq!(dc,raster.dc);
                assert_eq!(body,raster.painter.bitmaps);
                assert_eq!(particles,raster.painter.fx_bitmaps);
                assert_eq!(noise,raster.painter.boil.as_ref().unwrap().noise);
            }
            let b=raster.painter.scaled_bitmap(&dc,"idle_0",0.56,2.).unwrap();
            let f=&clips::catalog().frames["idle_0"];
            assert_eq!(b.GetPixelSize().width,(f.w as f32*1.12).round() as u32);
            assert_eq!(b,raster.painter.scaled_bitmap(&dc,"idle_0",0.56,2.).unwrap());
            assert!(raster.painter.bitmaps.keys().filter(|k|k.contains("_small_")).all(|k|k.ends_with("_56")));
            raster.painter.scaled_bitmap(&dc,"idle_0",0.34,1.).unwrap();
            assert!(raster.painter.bitmaps.keys().filter(|k|k.contains("_small_")).all(|k|k.ends_with("_17")));
        }
    }
    #[test]
    fn stationary_clip_skips_springs_and_walking_moves_without_rasterizing() {
        let mut p = crate::companion::motion::Motion::new(10.0).pose();
        let before = Stamp::new(&p, 0.56, 1.0, false, false);
        p.x += 40.0;
        p.sx = 0.98;
        assert_eq!(before, Stamp::new(&p, 0.56, 1.0, false, false));
        p.frame = "idle_1".into();
        assert_ne!(before, Stamp::new(&p, 0.56, 1.0, false, false));
        p.frame = "idle_0".into();
        assert_ne!(before, Stamp::new(&p, 0.56, 1.5, false, false));
    }
    #[test]
    fn grabbed_pixel_stays_on_axis_and_inverse_handles_walking_flip(){
        let mut p=crate::companion::motion::Motion::new(0.).pose();p.frame="jump_3".into();p.pivot=Some((0.4,0.9));p.tilt=176.;p.sy=1.06;
        let f=&clips::catalog().frames[&p.frame];let m=body_matrix(&p,150.,230.,0.56);
        let (u,v)=(f.w as f32*0.4,f.h as f32*0.9);
        assert!((u*m.M11+v*m.M21+m.M31-150.).abs()<0.001);
        assert!((u*m.M12+v*m.M22+m.M32-230.).abs()<0.001);
        for flip in [false,true]{p.flip=flip;let m=body_matrix(&p,150.,230.,0.56);let pixel=super::super::grab::Point::new(u*m.M11+v*m.M21+m.M31,u*m.M12+v*m.M22+m.M32);let found=grab_point(&p,150.,230.,0.56,pixel);assert!((found.x-0.4).abs()<0.001&&(found.y-0.9).abs()<0.001);}
    }
    #[test]
    fn boil_strength_and_frequency_are_screen_pixel_stable() {
        for dpi in [1.0, 1.5, 2.0] {
            let (frequency, amplitude) = boil_parameters(dpi);
            assert!((amplitude * dpi - 2.2).abs() < 0.0001);
            assert!((frequency / dpi - 0.035).abs() < 0.0001);
        }
    }
}

pub fn translate(x: f32, y: f32) -> Matrix3x2 {
    Matrix3x2 {
        M31: x,
        M32: y,
        ..Matrix3x2::identity()
    }
}
pub fn scale(x: f32, y: f32) -> Matrix3x2 {
    Matrix3x2 {
        M11: x,
        M22: y,
        ..Matrix3x2::identity()
    }
}
pub fn body_matrix(p: &Pose, x: f32, y: f32, s: f32) -> Matrix3x2 {
    let f = &clips::catalog().frames[&p.frame];
    let r = p.tilt.to_radians();
    let rot = Matrix3x2 {
        M11: r.cos(),
        M12: r.sin(),
        M21: -r.sin(),
        M22: r.cos(),
        ..Matrix3x2::identity()
    };
    let (px,py)=p.pivot.map_or((f.ax,f.ground),|(x,y)|(x*f.w as f32,y*f.h as f32));
    translate(-px, -py)
        * scale(if p.flip { -p.sx * s } else { p.sx * s }, p.sy * s)
        * Matrix3x2 {M21:p.skew.to_radians().tan(),..Matrix3x2::identity()}
        * rot
        * translate(x, y + p.jy * s)
}
/// Bend the upper cone above the nose. The exported body layer still contains
/// the old hat outline, so moving that layer would leave a second silhouette.
pub fn hat_hinge(p: &Pose) -> f32 {
    let f = &clips::catalog().frames[&p.frame];
    f.hy - f.hatw * 0.25
}
pub fn hat_matrix(p: &Pose) -> Matrix3x2 {
    let f = &clips::catalog().frames[&p.frame];
    let hinge = hat_hinge(p);
    translate(-f.hx,-hinge)
        * Matrix3x2 {M21:p.hat_tilt.to_radians().tan(),M22:1.-p.hat_tilt.abs()*0.002,..Matrix3x2::identity()}
        * translate(f.hx,hinge)
}
pub fn grab_point(p:&Pose,x:f32,y:f32,s:f32,point:super::grab::Point)->super::grab::Point {
    let m=body_matrix(p,x,y,s);let dx=point.x-m.M31;let dy=point.y-m.M32;let det=m.M11*m.M22-m.M12*m.M21;
    let f=&clips::catalog().frames[&p.frame];
    super::grab::Point::new(((dx*m.M22-dy*m.M21)/det)/f.w as f32,((dy*m.M11-dx*m.M12)/det)/f.h as f32)
}
fn props(flags: D2D1_BITMAP_OPTIONS) -> D2D1_BITMAP_PROPERTIES1 {
    D2D1_BITMAP_PROPERTIES1 {
        pixelFormat: D2D1_PIXEL_FORMAT {
            format: DXGI_FORMAT_B8G8R8A8_UNORM,
            alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
        },
        dpiX: 96.0,
        dpiY: 96.0,
        bitmapOptions: flags,
        ..Default::default()
    }
}
unsafe fn value<T: Copy>(
    fx: &ID2D1Effect,
    prop: i32,
    kind: D2D1_PROPERTY_TYPE,
    v: &T,
) -> Result<()> {
    let result = fx.SetValue(
        prop as u32,
        kind,
        std::slice::from_raw_parts(v as *const T as *const u8, std::mem::size_of::<T>()),
    );
    if let Err(e) = &result {
        crate::log!(
            "companion effect property {prop}, type {:?}, bytes {}: {e}",
            kind,
            std::mem::size_of::<T>()
        );
        #[cfg(test)]
        eprintln!(
            "effect property {prop}, type {:?}, bytes {}, declared {:?}: {e}",
            kind,
            std::mem::size_of::<T>(),
            fx.GetType(prop as u32)
        );
    }
    result
}
struct Boil {
    noise: ID2D1Effect,
    displacement: ID2D1Effect,
}
impl Boil {
    unsafe fn new(dc: &ID2D1DeviceContext) -> Result<Self> {
        let noise = dc.CreateEffect(&CLSID_D2D1Turbulence)?;
        let displacement = dc.CreateEffect(&CLSID_D2D1DisplacementMap)?;
        value(
            &noise,
            D2D1_TURBULENCE_PROP_BASE_FREQUENCY.0,
            D2D1_PROPERTY_TYPE_VECTOR2,
            &Vector2 { X: 0.035, Y: 0.035 },
        )?;
        value(
            &noise,
            D2D1_TURBULENCE_PROP_NUM_OCTAVES.0,
            D2D1_PROPERTY_TYPE_UINT32,
            &1u32,
        )?;
        value(
            &noise,
            D2D1_TURBULENCE_PROP_NOISE.0,
            D2D1_PROPERTY_TYPE_ENUM,
            &D2D1_TURBULENCE_NOISE_FRACTAL_SUM.0,
        )?;
        value(
            &displacement,
            D2D1_DISPLACEMENTMAP_PROP_SCALE.0,
            D2D1_PROPERTY_TYPE_FLOAT,
            &2.2f32,
        )?;
        value(
            &displacement,
            D2D1_DISPLACEMENTMAP_PROP_X_CHANNEL_SELECT.0,
            D2D1_PROPERTY_TYPE_ENUM,
            &D2D1_CHANNEL_SELECTOR_R.0,
        )?;
        value(
            &displacement,
            D2D1_DISPLACEMENTMAP_PROP_Y_CHANNEL_SELECT.0,
            D2D1_PROPERTY_TYPE_ENUM,
            &D2D1_CHANNEL_SELECTOR_G.0,
        )?;
        displacement.SetInput(1, &noise.GetOutput()?, true);
        Ok(Self {
            noise,
            displacement,
        })
    }
}
pub struct Painter {
    bitmaps: HashMap<String, ID2D1Bitmap1>,
    bucket: Option<u32>,
    fx_bitmaps: HashMap<&'static str, ID2D1Bitmap1>,
    boil: Option<Boil>,
    pub boil_unavailable: bool,
}
impl Painter {
    pub unsafe fn warm_inputs(&mut self,dc:&ID2D1DeviceContext,s:f32,dpi:f32)->Result<()> {
        for frame in ["look_4","jump_0"] {self.scaled_bitmap(dc,frame,s,dpi)?;super::anchors::Anchors::load(frame);}
        Ok(())
    }
    unsafe fn scaled_bitmap(&mut self,dc:&ID2D1DeviceContext,frame:&str,s:f32,dpi:f32)->Result<ID2D1Bitmap1>{
        // Resample once at every output density, including >100% physical size.
        // Keep only the current density's frames when size/DPI changes.
        let bucket=((s*dpi/0.02).round() as u32).max(1);
        if self.bucket!=Some(bucket){self.bitmaps.retain(|key,_|!key.contains("_small_"));self.bucket=Some(bucket);}
        let key=format!("{frame}_small_{bucket}");
        if let Some(b)=self.bitmaps.get(&key){return Ok(b.clone());}
        let f=&clips::catalog().frames[frame];
        let mut i=crate::icons::decode_bytes_fit(clips::png(frame,"full").ok_or_else(||Error::from(E_FAIL))?,4096).ok_or_else(||Error::from(E_FAIL))?;
        let width=(f.w as f32*bucket as f32*0.02).round().max(1.) as u32;
        let height=(f.h as f32*bucket as f32*0.02).round().max(1.) as u32;
        if (width,height)!=(i.width,i.height) {
            use windows::Win32::{System::Com::*,Graphics::Imaging::*};
            let factory:IWICImagingFactory=CoCreateInstance(&CLSID_WICImagingFactory,None,CLSCTX_INPROC_SERVER)?;
            let source=factory.CreateBitmapFromMemory(i.width,i.height,&GUID_WICPixelFormat32bppPBGRA,i.width*4,&i.pixels)?;
            let scaler=factory.CreateBitmapScaler()?;
            scaler.Initialize(&source,width,height,if bucket>50{WICBitmapInterpolationModeHighQualityCubic}else{WICBitmapInterpolationModeFant})?;
            let mut pixels=vec![0; width as usize*height as usize*4];scaler.CopyPixels(std::ptr::null(),width*4,&mut pixels)?;
            i=crate::icons::Icon{width,height,pixels};
        }
        let mut properties=props(D2D1_BITMAP_OPTIONS_NONE);
        // The cached pixels are small; their logical size remains source-art size.
        properties.dpiX=96.*i.width as f32/f.w as f32;properties.dpiY=96.*i.height as f32/f.h as f32;
        let b=dc.CreateBitmap(D2D_SIZE_U{width:i.width,height:i.height},Some(i.pixels.as_ptr() as _),i.width*4,&properties)?;
        self.bitmaps.insert(key,b.clone());Ok(b)
    }
    pub fn new() -> Self {
        Self {
            bitmaps: HashMap::new(),
            bucket: None,
            fx_bitmaps: HashMap::new(),
            boil: None,
            boil_unavailable: false,
        }
    }
    unsafe fn fx_bitmap(&mut self, dc: &ID2D1DeviceContext, name: &'static str) -> Result<ID2D1Bitmap1> {
        if let Some(bitmap) = self.fx_bitmaps.get(name) { return Ok(bitmap.clone()); }
        let bytes = clips::fx_png(name).ok_or_else(|| Error::from(E_FAIL))?;
        let mut i = crate::icons::decode_bytes_fit(bytes, 4096).ok_or_else(|| Error::from(E_FAIL))?;
        for p in i.pixels.as_chunks_mut::<4>().0 {
            let grey = ((u16::from(p[2])*54 + u16::from(p[1])*183 + u16::from(p[0])*19) >> 8) as u8;
            p[..3].fill(grey);
        }
        let bitmap = dc.CreateBitmap(D2D_SIZE_U { width:i.width, height:i.height },
            Some(i.pixels.as_ptr() as _), i.width*4, &props(D2D1_BITMAP_OPTIONS_NONE))?;
        self.fx_bitmaps.insert(name, bitmap.clone());
        Ok(bitmap)
    }
    unsafe fn bitmap(
        &mut self,
        dc: &ID2D1DeviceContext,
        frame: &str,
        part: &str,
    ) -> Result<ID2D1Bitmap1> {
        let key = format!("{frame}_{part}");
        if let Some(b) = self.bitmaps.get(&key) {
            return Ok(b.clone());
        }
        let bytes = if part == "fx" {clips::fx_png(frame)} else {clips::png(frame, part)}.ok_or_else(|| Error::from(E_FAIL))?;
        let i = crate::icons::decode_bytes_fit(bytes, 4096).ok_or_else(|| Error::from(E_FAIL))?;
        let b = dc.CreateBitmap(
            D2D_SIZE_U {
                width: i.width,
                height: i.height,
            },
            Some(i.pixels.as_ptr() as _),
            i.width * 4,
            &props(D2D1_BITMAP_OPTIONS_NONE),
        )?;
        self.bitmaps.insert(key, b.clone());
        Ok(b)
    }
    /// Caller owns BeginDraw/EndDraw and the DPI transform.
    pub unsafe fn paint(
        &mut self,
        dc: &ID2D1DeviceContext,
        p: &Pose,
        x: f32,
        y: f32,
        s: f32,
        boil: bool,
        seconds: f32,
        screen_dpi: f32,
    ) -> Result<()> {
        let mut old = Matrix3x2::identity();
        dc.GetTransform(&mut old);
        let body = self.scaled_bitmap(dc, &p.frame, s, screen_dpi)?;
        if boil && self.boil.is_none() && !self.boil_unavailable {
            match Boil::new(dc) {
                Ok(b) => self.boil = Some(b),
                Err(e) => {
                    self.boil_unavailable = true;
                    crate::log!("companion: line boil disabled: {e}");
                }
            }
        }
        let world = body_matrix(p, x, y, s) * old;
        let result = (|| -> Result<()> {
            dc.SetTransform(&world);
            if p.hat_tilt != 0. {
                let alpha=p.blend.as_ref().map_or(0.,|(_,a)|*a);
                bent_bitmap(dc,&body,p,world,s,screen_dpi,1.-alpha);
                if let Some((next,alpha))=&p.blend {
                    let bitmap=self.scaled_bitmap(dc,next,s,screen_dpi)?;
                    let mut next_pose=p.clone();next_pose.frame=next.clone();
                    let blend=dc.GetPrimitiveBlend();dc.SetPrimitiveBlend(D2D1_PRIMITIVE_BLEND_ADD);
                    bent_bitmap(dc,&bitmap,&next_pose,body_matrix(&next_pose,x,y,s)*old,s,screen_dpi,*alpha);
                    dc.SetPrimitiveBlend(blend);
                }
            } else if let Some(b) = self.boil.as_ref().filter(|_| boil && p.blend.is_none()) {
                let f = &clips::catalog().frames[&p.frame];
                // SVG reference: 0.035 / 2.2 at 96 DPI. Keep screen-pixel
                // frequency/amplitude stable when the monitor scale changes.
                let (frequency, amplitude) = boil_parameters(screen_dpi);
                value(
                    &b.noise,
                    D2D1_TURBULENCE_PROP_BASE_FREQUENCY.0,
                    D2D1_PROPERTY_TYPE_VECTOR2,
                    &Vector2 {
                        X: frequency,
                        Y: frequency,
                    },
                )?;
                value(
                    &b.displacement,
                    D2D1_DISPLACEMENTMAP_PROP_SCALE.0,
                    D2D1_PROPERTY_TYPE_FLOAT,
                    &amplitude,
                )?;
                b.displacement.SetInput(0, &body, true);
                value(
                    &b.noise,
                    D2D1_TURBULENCE_PROP_SIZE.0,
                    D2D1_PROPERTY_TYPE_VECTOR2,
                    &Vector2 {
                        X: f.w as f32,
                        Y: f.h as f32,
                    },
                )?;
                value(
                    &b.noise,
                    D2D1_TURBULENCE_PROP_SEED.0,
                    // Some Windows builds declare this INT32 although the
                    // docs say UINT32. 0..96 has the same representation;
                    // use the effect's actual property type.
                    b.noise.GetType(D2D1_TURBULENCE_PROP_SEED.0 as u32),
                    &((seconds * 8.0) as u32 % 97),
                )?;
                dc.DrawImage(
                    &b.displacement.GetOutput()?,
                    None,
                    None,
                    D2D1_INTERPOLATION_MODE_LINEAR,
                    D2D1_COMPOSITE_MODE_SOURCE_OVER,
                );
            } else {
                // Fant-filtered pixels already match the output density;
                // linear subpixel sampling only handles the tiny deformation.
                let interpolation=D2D1_INTERPOLATION_MODE_LINEAR;
                let alpha=p.blend.as_ref().map_or(0.,|(_,a)|*a);
                dc.DrawBitmap(&body, None, 1.-alpha, interpolation, None, None);
                if let Some((next,alpha))=&p.blend {
                    let bitmap=self.scaled_bitmap(dc,next,s,screen_dpi)?;
                    let mut next_pose=p.clone();next_pose.frame=next.clone();
                    dc.SetTransform(&(body_matrix(&next_pose,x,y,s)*old));
                    let blend=dc.GetPrimitiveBlend();dc.SetPrimitiveBlend(D2D1_PRIMITIVE_BLEND_ADD);
                    dc.DrawBitmap(&bitmap,None,*alpha,interpolation,None,None);
                    dc.SetPrimitiveBlend(blend);
                }
            }
            Ok(())
        })();
        dc.SetTransform(&old);
        result
    }
    pub unsafe fn particles(&mut self, dc:&ID2D1DeviceContext, parts:&[super::fx::Particle], gx:f32,gy:f32,s:f32,origin:(f32,f32),light:bool)->Result<()> {
        let mut old=Matrix3x2::identity();dc.GetTransform(&mut old);
        let dpi=old.M11.hypot(old.M12).max(0.1);
        let result=(||->Result<()> {for p in parts {
            if p.name=="tick" {
                let (x,y,sc,angle,alpha)=p.visual(gx,gy,s);let r=angle.to_radians();
                let (dx,dy)=(-r.sin()*17.5*sc,r.cos()*17.5*sc);
                let ink=if light{super::fx::CONTOUR_INK}else{0.88};
                let brush=dc.CreateSolidColorBrush(&D2D1_COLOR_F{r:ink,g:ink,b:ink,a:alpha},None)?;
                dc.SetTransform(&old);
                dc.DrawLine(Vector2{X:x-dx-origin.0,Y:y-dy-origin.1},Vector2{X:x+dx-origin.0,Y:y+dy-origin.1},&brush,super::fx::stroke_width(s,dpi),None);
                continue;
            }
            if let Some(line)=p.gloom_line(gx,gy,s) {
                if line.to.1<=line.from.1||line.opacity<=0. {continue;}
                // These marks sit on the red hat: keep contour ink in both
                // themes. A short transparent start replaces the steam glyph.
                let ink=super::fx::CONTOUR_INK;
                let stops=[D2D1_GRADIENT_STOP{position:0.,color:D2D1_COLOR_F{r:ink,g:ink,b:ink,a:0.}},D2D1_GRADIENT_STOP{position:1.,color:D2D1_COLOR_F{r:ink,g:ink,b:ink,a:line.opacity}}];
                use windows::core::Interface;
                let target:ID2D1RenderTarget=dc.cast()?;
                let stops=target.CreateGradientStopCollection(&stops,D2D1_GAMMA_2_2,D2D1_EXTEND_MODE_CLAMP)?;
                let point=|p:(f32,f32)|Vector2{X:p.0-origin.0,Y:p.1-origin.1};
                let brush=dc.CreateLinearGradientBrush(&D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES{startPoint:point(line.from),endPoint:point(line.fade_end)},None,&stops)?;
                dc.SetTransform(&old);dc.DrawLine(point(line.from),point(line.to),&brush,line.width.max(1./dpi),None);
                continue;
            }
            let b=self.fx_bitmap(dc,particle_name(p.name,light))?;let size=b.GetSize();
            let (x,y,sc,angle,alpha)=p.visual(gx,gy,s);let r=angle.to_radians();
            let rot=Matrix3x2{M11:r.cos(),M12:r.sin(),M21:-r.sin(),M22:r.cos(),..Matrix3x2::identity()};
            dc.SetTransform(&(translate(-size.width/2.,-size.height/2.)*scale(sc*0.5,sc*0.5)*rot*translate(x-origin.0,y-origin.1)*old));
            dc.DrawBitmap(&b,None,alpha,D2D1_INTERPOLATION_MODE_LINEAR,None,None);
        }Ok(())})();dc.SetTransform(&old);result
    }
    pub unsafe fn slot(
        &mut self,
        dc: &ID2D1DeviceContext,
        x: f32,
        y: f32,
        h: f32,
        alpha: f32,
    ) -> Result<()> {
        let frame = clips::catalog().clip("slot").frame(0.0);
        let f = &clips::catalog().frames[frame];
        let b = self.bitmap(dc, frame, "full")?;
        let w = h * f.w as f32 / f.h as f32;
        dc.DrawBitmap(
            &b,
            Some(&D2D_RECT_F {
                left: x - w / 2.0,
                top: y - h,
                right: x + w / 2.0,
                bottom: y,
            }),
            alpha,
            D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC,
            None,
            None,
        );
        Ok(())
    }
}
pub unsafe fn shadow(
    dc: &ID2D1DeviceContext,
    p: &Pose,
    x: f32,
    y: f32,
    s: f32,
    light: bool,
) -> Result<()> {
    let f = &clips::catalog().frames[clips::catalog().clip("idle").frame(0.0)];
    let r = (1.0 + p.jy.min(0.0) / 90.0).max(0.45);
    let color = if light {
        D2D1_COLOR_F {
            r: 40.0 / 255.0,
            g: 30.0 / 255.0,
            b: 25.0 / 255.0,
            a: 0.13,
        }
    } else {
        D2D1_COLOR_F {
            r: 0.0,
            g: 0.0,
            b: 0.0,
            a: 0.35,
        }
    };
    let brush = dc.CreateSolidColorBrush(&color, None)?;
    dc.FillEllipse(
        &D2D1_ELLIPSE {
            point: Vector2 { X: x, Y: y },
            radiusX: f.w as f32 * 0.36 * s * r,
            radiusY: 6.0 * r,
        },
        &brush,
    );
    Ok(())
}

/// Reused target + staging bitmap. No per-frame device/bitmap allocation.
pub struct Raster {
    pub dc: ID2D1DeviceContext,
    target: ID2D1Bitmap1,
    cpu: ID2D1Bitmap1,
    pub w: u32,
    pub h: u32,
    pub painter: Painter,
}
impl Raster {
    /// Targets depend on size; decoded artwork and device effects do not.
    pub unsafe fn resize(&mut self, w:u32, h:u32)->Result<()> {
        if (self.w,self.h)==(w,h) { return Ok(()); }
        let size=D2D_SIZE_U{width:w,height:h};
        let target=self.dc.CreateBitmap(size,None,0,&props(D2D1_BITMAP_OPTIONS_TARGET))?;
        let cpu=self.dc.CreateBitmap(size,None,0,&props(D2D1_BITMAP_OPTIONS_CPU_READ|D2D1_BITMAP_OPTIONS_CANNOT_DRAW))?;
        self.dc.SetTarget(None);
        self.target=target;self.cpu=cpu;self.w=w;self.h=h;
        Ok(())
    }
    pub unsafe fn new(dc: ID2D1DeviceContext, w: u32, h: u32) -> Result<Self> {
        let size = D2D_SIZE_U {
            width: w,
            height: h,
        };
        let target = dc.CreateBitmap(size, None, 0, &props(D2D1_BITMAP_OPTIONS_TARGET))?;
        let cpu = dc.CreateBitmap(
            size,
            None,
            0,
            &props(D2D1_BITMAP_OPTIONS_CPU_READ | D2D1_BITMAP_OPTIONS_CANNOT_DRAW),
        )?;
        Ok(Self {
            dc,
            target,
            cpu,
            w,
            h,
            painter: Painter::new(),
        })
    }
    pub(crate) unsafe fn custom(&mut self,dpi:f32,pixels:&mut[u8],paint:impl FnOnce(&ID2D1DeviceContext)->Result<()>)->Result<()> {
        self.dc.SetTarget(&self.target);self.dc.SetTransform(&scale(dpi,dpi));self.dc.BeginDraw();self.dc.Clear(Some(&D2D1_COLOR_F::default()));
        let r=paint(&self.dc);let end=self.dc.EndDraw(None,None);r?;end?;self.readback(pixels)
    }
    pub unsafe fn draw_particles(&mut self,parts:&[super::fx::Particle],gx:f32,gy:f32,s:f32,dpi:f32,origin:(f32,f32),light:bool,pixels:&mut[u8])->Result<()> {
        self.dc.SetTarget(&self.target);self.dc.SetTransform(&scale(dpi,dpi));self.dc.BeginDraw();self.dc.Clear(Some(&D2D1_COLOR_F::default()));
        let r=self.painter.particles(&self.dc,parts,gx,gy,s,origin,light);let end=self.dc.EndDraw(None,None);r?;end?;self.readback(pixels)
    }
    unsafe fn readback(&self,pixels:&mut[u8])->Result<()> {
        assert_eq!(pixels.len(),(self.w*self.h*4) as usize);
        self.cpu.CopyFromBitmap(None,&self.target,None)?;let m=self.cpu.Map(D2D1_MAP_OPTIONS_READ)?;
        for row in 0..self.h as usize {std::ptr::copy_nonoverlapping(m.bits.add(row*m.pitch as usize),pixels.as_mut_ptr().add(row*self.w as usize*4),self.w as usize*4);}
        self.cpu.Unmap()?;Ok(())
    }
    pub unsafe fn draw(
        &mut self,
        p: &Pose,
        x: f32,
        y: f32,
        s: f32,
        dpi: f32,
        boil: bool,
        t: f32,
        shadow_only: bool,
        light: bool,
        pixels: &mut [u8],
    ) -> Result<()> {
        assert_eq!(pixels.len(), (self.w * self.h * 4) as usize);
        self.dc.SetTarget(&self.target);
        self.dc.SetTransform(&scale(dpi, dpi));
        self.dc.BeginDraw();
        self.dc.Clear(Some(&D2D1_COLOR_F::default()));
        let r = if shadow_only {
            shadow(&self.dc, p, x, y, s, light)
        } else {
            self.painter.paint(&self.dc, p, x, y, s, boil, t, dpi)
        };
        let end = self.dc.EndDraw(None, None);
        r?;
        end?;
        self.cpu.CopyFromBitmap(None, &self.target, None)?;
        let m = self.cpu.Map(D2D1_MAP_OPTIONS_READ)?;
        // Copy mapped rows straight into the caller's persistent DIB.
        for row in 0..self.h as usize {
            std::ptr::copy_nonoverlapping(
                m.bits.add(row * m.pitch as usize),
                pixels.as_mut_ptr().add(row * self.w as usize * 4),
                self.w as usize * 4,
            );
        }
        self.cpu.Unmap()?;
        Ok(())
    }
}
