use serde::Deserialize;
use std::collections::BTreeMap;
use std::sync::OnceLock;
include!(concat!(env!("OUT_DIR"), "/companion_assets.rs"));

#[derive(Debug, Deserialize)]
pub struct Frame {
    pub w: u32,
    pub h: u32,
    pub ax: f32,
    pub ground: f32,
    pub hx: f32,
    pub hy: f32,
    pub hatw: f32,
    #[serde(default)]
    pub edge: Option<f32>,
}
#[derive(Debug, Deserialize)]
pub struct Clip {
    pub frames: Vec<String>,
    pub frame_ms: f32,
    pub repeat: bool,
    #[serde(default)]
    pub hold: BTreeMap<usize, f32>,
    #[serde(default)]
    pub next: Option<String>,
    #[serde(default)]
    pub travel_ms: Option<f32>,
}
impl Clip {
    pub fn transition(&self,seconds:f32)->Option<(&str,f32)> {
        if self.frame_ms<300. {return None;}
        let mut t=if self.repeat {seconds.max(0.)%self.duration()}else{seconds.max(0.)};
        for (i,_) in self.frames.iter().enumerate() {
            let d=self.frame_ms/1000.*self.hold.get(&i).copied().unwrap_or(1.);
            if t<d {
                let next=if i+1<self.frames.len(){i+1}else if self.repeat {0}else{return None;};
                let u=((t-(d-super::timing::FADE))/super::timing::FADE).clamp(0.,1.);
                return (u>0.&&self.frames[i]!=self.frames[next]).then(||(self.frames[next].as_str(),1.-(1.-u).powi(2)));
            }
            t-=d;
        }
        None
    }
    pub fn frame(&self, seconds: f32) -> &str {
        let duration = self.duration();
        let mut t = if self.repeat {
            seconds.max(0.0) % duration
        } else {
            seconds.max(0.0)
        };
        for (i, name) in self.frames.iter().enumerate() {
            let d = self.frame_ms / 1000.0 * self.hold.get(&i).copied().unwrap_or(1.0);
            if t + 0.000001 < d {
                return name;
            }
            t -= d;
        }
        self.frames.last().unwrap()
    }
    pub fn duration(&self) -> f32 {
        (0..self.frames.len())
            .map(|i| self.hold.get(&i).copied().unwrap_or(1.0))
            .sum::<f32>()
            * self.frame_ms
            / 1000.0
    }
}
pub struct Catalog {
    pub frames: BTreeMap<String, Frame>,
    pub clips: BTreeMap<String, Clip>,
    pub anchors: BTreeMap<String, super::anchors::Anchors>,
}
impl Catalog {
    fn parse() -> Self {
        let frames = serde_json::from_str(include_str!("../../../assets/companion/frames.json"))
            .expect("companion frames.json");
        let clips = serde_json::from_str(include_str!("../../../assets/companion/clips.json"))
            .expect("companion clips.json");
        let mut c = Self { frames, clips, anchors:BTreeMap::new() };
        // Catalog construction is the only decode phase for landmarks. Ensure
        // COM also exists when pure-model tests initialize the catalog first.
        unsafe {
            use windows::Win32::System::Com::*;
            let initialized=CoInitializeEx(None,COINIT_MULTITHREADED).is_ok();
            for (name,frame) in &c.frames {c.anchors.insert(name.clone(),super::anchors::Anchors::calculate(frame,png(name,"full")));}
            if initialized {CoUninitialize();}
        }
        c.validate().expect("valid companion asset catalog");
        c
    }
    fn validate(&self) -> Result<(), String> {
        for key in [
            "idle",
            "sit",
            "walk",
            "turn_left",
            "turn_right",
            "hide_launch",
            "hide_air",
            "show_air",
            "land",
            "notify",
            "wait",
            "done",
            "error",
            "sleep",
            "peek",
            "sidepeek",
            "slot",
            "look", "pet", "pet_end", "poke", "read", "read_doze",
            "stand", "stretch", "yawn", "carry",
        ] {
            if !self.clips.contains_key(key) {
                return Err(format!("missing clip {key}"));
            }
        }
        for key in ["hide_air", "show_air"] {
            if self.clips[key].travel_ms.is_none() {
                return Err(format!("missing travel duration {key}"));
            }
        }
        for (key, c) in &self.clips {
            if c.frames.is_empty() || !c.frame_ms.is_finite() || c.frame_ms <= 0.0 {
                return Err(format!("empty/zero length clip {key}"));
            }
            if c.hold
                .iter()
                .any(|(i, m)| *i >= c.frames.len() || !m.is_finite() || *m <= 0.0)
            {
                return Err(format!("invalid hold {key}"));
            }
            if c.next.as_ref().is_some_and(|n| !self.clips.contains_key(n)) {
                return Err(format!("missing next clip {key}"));
            }
            if c.travel_ms.is_some_and(|d| !d.is_finite() || d <= 0.0) {
                return Err(format!("invalid travel {key}"));
            }
            for name in &c.frames {
                let f = self
                    .frames
                    .get(name)
                    .ok_or_else(|| format!("missing frame {name}"))?;
                if f.w == 0
                    || f.h == 0
                    || ![f.ax, f.ground, f.hx, f.hy].iter().all(|x| x.is_finite())
                {
                    return Err(format!("invalid frame {name}"));
                }
                for part in ["full"] {
                    if png(name, part).is_none() {
                        return Err(format!("missing {name}_{part}.png"));
                    }
                }
            }
        }
        Ok(())
    }
    pub fn clip(&self, name: &str) -> &Clip {
        &self.clips[name]
    }
}
pub fn catalog() -> &'static Catalog {
    static C: OnceLock<Catalog> = OnceLock::new();
    C.get_or_init(Catalog::parse)
}
pub fn png(name: &str, part: &str) -> Option<&'static [u8]> {
    let key = format!("{name}_{part}.png");
    PNGS.iter()
        .find(|(n, _)| *n == key)
        .map(|(_, bytes)| *bytes)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sidepeek_approved_cell_has_three_layers_and_no_cyan() {
        unsafe {
            use windows::Win32::System::Com::*;
            CoInitializeEx(None, COINIT_MULTITHREADED).ok().unwrap();
        }
        let c = catalog();
        assert_eq!(c.clip("sidepeek").frames, ["sidepeek_0"]);
        let f = &c.frames["sidepeek_0"];
        assert_eq!(f.edge, Some(18.234));
        for part in ["body", "hat", "full"] {
            let image = crate::icons::decode_bytes_fit(png("sidepeek_0", part).unwrap(), 4096).unwrap();
            assert_eq!((image.width,image.height),(f.w,f.h));
            for p in image.pixels.as_chunks::<4>().0.iter().filter(|p|p[3]>0) {
                // BGRA premultiplied: green AND blue above red is cyan spill.
                assert!(!(p[0] > p[2] && p[1] > p[2]));
            }
        }
        unsafe { windows::Win32::System::Com::CoUninitialize(); }
    }
    #[test]
    fn life_clips_hold_the_approved_frames() {
        let c = catalog();
        assert_eq!(c.clip("look").frames.len(), 8);
        assert_eq!(c.clip("pet").frames, ["pet_0", "pet_1", "pet_2", "pet_1"]);
        assert_eq!(c.clip("pet_end").next.as_deref(), Some("idle"));
        for (name, duration) in [("poke", 0.73), ("stretch", 0.98), ("yawn", 1.14)] {
            assert!((c.clip(name).duration() - duration).abs() < 0.0001);
            assert!(!c.clip(name).repeat);
        }
        assert_eq!(c.clip("poke").frame(0.61), "poke_2");
        assert_eq!(c.clip("poke").frame(0.63), "poke_3");
        assert_eq!(c.clip("stand").frames, ["sit_4", "sit_3", "sit_2", "sit_1", "sit_0"]);
        for name in ["pet", "read", "carry"] { assert!(c.clip(name).repeat); }
    }
    #[test]
    fn all_clips_have_embedded_parts() {
        assert!(catalog().validate().is_ok());
    }
    #[test]
    fn approved_clip_timing_and_holds() {
        let c = catalog();
        assert_eq!(c.clip("idle").frames.len(), 6);
        assert_eq!(c.clip("walk").frames.len(), 8);
        assert_eq!(c.clip("idle").frame_ms, 200.0);
        assert_eq!(c.clip("walk").frame_ms, 100.0);
        assert_eq!(c.clip("sleep").frame_ms, 400.0);
        assert_eq!(c.clip("notify").frame(0.4), "notify_1");
        assert!((c.clip("notify").duration() - 0.75).abs() < 0.0001);
        assert_eq!(c.clip("peek").frame(0.7), "peek_1");
        assert_eq!(c.clip("peek").frame(1.4), "peek_2");
        assert!((c.clip("peek").duration() - 10.0 / 6.0).abs() < 0.0001);
        assert_eq!(
            c.clip("turn_left").frames,
            ["turn_2", "turn_3", "turn_4", "turn_5", "turn_6"]
        );
        assert_eq!(
            c.clip("turn_right").frames,
            ["turn_6", "turn_7", "turn_0", "turn_1", "turn_2"]
        );
        assert_eq!(
            c.clip("sit").frames,
            ["sit_0", "sit_1", "sit_2", "sit_3", "sit_4"]
        );
        for (name, clip) in &c.clips {
            if name != "slot" {
                assert!(
                    clip.frames.iter().all(|f| f.contains('_')),
                    "legacy still frame in {name}"
                );
            }
        }
    }
    #[test]
    fn arbitrary_frame_lists_repeat_or_hold() {
        let mut c = Clip {
            frames: (0..8).map(|i| format!("walk_{i}")).collect(),
            frame_ms: 80.0,
            hold: BTreeMap::new(),
            next: None,
            travel_ms: None,
            repeat: true,
        };
        assert_eq!(c.frame(0.56), "walk_7");
        assert_eq!(c.frame(0.64), "walk_0");
        c.repeat = false;
        assert_eq!(c.frame(10.0), "walk_7");
        assert_eq!(c.frame(-1.0), "walk_0");
    }
    #[test]
    fn ping_pong_and_slow_dissolve_avoid_the_three_to_zero_cut() {
        for name in ["idle","wait","sleep"] {
            assert_eq!(catalog().clip(name).frames,[0,1,2,3,2,1].map(|i|format!("{name}_{i}")));
        }
        let sleep=catalog().clip("sleep");
        assert_eq!(sleep.transition(0.24),None);
        let (name,alpha)=sleep.transition(0.325).unwrap();
        assert_eq!(name,"sleep_1");assert!((alpha-0.75).abs()<0.0001);
        assert_eq!(sleep.frame(0.4),"sleep_1");assert_eq!(sleep.transition(0.4),None);
        assert_eq!(catalog().clip("idle").transition(0.15),None);
    }
}

pub fn fx_png(name:&str)->Option<&'static [u8]>{let key=format!("fx/{name}.png");PNGS.iter().find(|(n,_)|*n==key).map(|(_,b)|*b)}
