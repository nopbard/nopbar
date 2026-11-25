//! Continuous scale, including the three strings written by earlier versions.
use serde::{Deserialize, Deserializer, Serialize};
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
#[serde(transparent)]
pub struct Size(f32);
impl Default for Size {
    fn default() -> Self {
        Self(0.56)
    }
}
impl Size {
    pub fn new(value: f32) -> Self {
        Self(if value.is_finite() {
            value.clamp(0.28, 0.80)
        } else {
            0.56
        })
    }
    pub fn scale(self) -> f32 {
        self.0
    }
    pub fn ratio(self) -> f32 {
        self.0 / 0.56
    }
    pub fn wheel(self, steps: i32) -> Self {
        Self::new(((self.0 + steps as f32 * 0.04) * 1000.).round() / 1000.)
    }
    pub fn margin(self) -> f32 {
        (190. * 0.56 + 16.) * self.ratio()
    }
    // Finite 0.02 scale buckets, shared by adjacent slider/wheel values.
    #[cfg(test)]
    pub fn bitmap_bucket(self, dpi: f32) -> Option<u32> {
        (self.0 < 0.45).then(|| ((self.0 * dpi.clamp(0.5, 4.) / 0.02).round() as u32).max(1))
    }
}
impl<'de> Deserialize<'de> for Size {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum OldOrNew {
            Number(f32),
            Name(String),
        }
        Ok(match OldOrNew::deserialize(d)? {
            OldOrNew::Number(v) => Self::new(v),
            OldOrNew::Name(s) => Self::new(match s.as_str() {
                "small" => 0.42,
                "large" => 0.70,
                _ => 0.56,
            }),
        })
    }
}
// Flattened config fields: old versions still see their enum, while this
// version prefers the exact continuous scale. Also accept interim numeric size.
pub(super) mod fields {
    use super::*;
    pub fn serialize<S:serde::Serializer>(value:&Size,s:S)->Result<S::Ok,S::Error>{
        #[derive(Serialize)] struct Stored {size:&'static str,size_scale:f32}
        let scale=value.scale();
        Stored{size:if scale<0.49{"small"}else if scale<0.63{"normal"}else{"large"},size_scale:scale}.serialize(s)
    }
    pub fn deserialize<'de,D:Deserializer<'de>>(d:D)->Result<Size,D::Error>{
        #[derive(Deserialize,Default)] #[serde(default)] struct Stored {size:Size,size_scale:Option<f32>}
        let value=Stored::deserialize(d)?;Ok(value.size_scale.map_or(value.size,Size::new))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn config_keeps_legacy_enum_and_prefers_exact_scale(){
        use super::super::model::Config;
        for (v,name) in [(0.28,"small"),(0.48,"small"),(0.56,"normal"),(0.64,"large"),(0.80,"large")] {
            let c=Config{size:Size::new(v),..Default::default()};let json=serde_json::to_value(c).unwrap();
            assert_eq!(json["size"],name);assert!((json["size_scale"].as_f64().unwrap()-v as f64).abs()<1e-6);
            assert_eq!(serde_json::from_value::<Config>(json).unwrap().size,c.size);
        }
        for (json,v) in [(r#"{"size":"small"}"#,0.42),(r#"{"size":"large","size_scale":0.34}"#,0.34),(r#"{"size":0.60}"#,0.60),("{}",0.56)] {assert_eq!(serde_json::from_str::<Config>(json).unwrap().size.scale(),v);}
    }
    #[test]
    fn migrates_strings_clamps_and_serializes_as_number() {
        for (json, value) in [
            ("\"small\"", 0.42),
            ("\"normal\"", 0.56),
            ("\"large\"", 0.70),
            ("0.1", 0.28),
            ("2.0", 0.80),
        ] {
            let s: Size = serde_json::from_str(json).unwrap();
            assert_eq!(s.scale(), value);
            assert!(serde_json::to_value(s).unwrap().is_number());
        }
        assert_eq!(Size::new(f32::NAN), Size::default());
        assert_eq!(Size::new(0.28).wheel(-1).scale(), 0.28);
        assert_eq!(Size::new(0.8).wheel(1).scale(), 0.8);
        assert_eq!(Size::default().wheel(1).scale(), 0.60);
        assert_eq!(Size::default().wheel(-1).scale(), 0.52);
    }
    #[test]
    fn geometry_and_cache_follow_scale() {
        let small = Size::new(0.28);
        let normal = Size::default();
        assert_eq!(small.margin() * 2., normal.margin());
        assert_eq!(normal.bitmap_bucket(1.), None);
        assert_eq!(Size::new(0.34).bitmap_bucket(1.), Some(17));
        assert_eq!(Size::new(0.34).bitmap_bucket(2.), Some(34));
    }
}
