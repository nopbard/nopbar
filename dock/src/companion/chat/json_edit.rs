//! Change one JSON object member without reserializing neighbouring members.
use serde::de::{MapAccess, Visitor};
use serde::Deserializer;
use serde_json::value::RawValue;
struct Members<'a>(Vec<(String, &'a RawValue)>);
impl<'de> Visitor<'de> for Members<'de> {
    type Value = Self;
    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("an object")
    }
    fn visit_map<A: MapAccess<'de>>(mut self, mut map: A) -> Result<Self, A::Error> {
        while let Some((key, value)) = map.next_entry::<String, &'de RawValue>()? {
            if self.0.iter().any(|(old, _)| old == &key) {
                return Err(serde::de::Error::custom("duplicate key"));
            }
            self.0.push((key, value));
        }
        Ok(self)
    }
}
pub fn set(text: &str, key: &str, value: Option<&str>) -> Result<String, String> {
    let mut de = serde_json::Deserializer::from_str(text);
    let members = de
        .deserialize_map(Members(Vec::new()))
        .map_err(|_| "Invalid JSON object")?
        .0;
    de.end().map_err(|_| "Invalid JSON object")?;
    let offset = |v: &RawValue| v.get().as_ptr() as usize - text.as_ptr() as usize;
    let open = text.find('{').ok_or("Invalid JSON object")? + 1;
    let mut out = text.to_string();
    if let Some(i) = members.iter().position(|(k, _)| k == key) {
        let current = members[i].1;
        let start = offset(current);
        let end = start + current.get().len();
        if let Some(value) = value {
            out.replace_range(start..end, value);
        } else if i > 0 {
            let previous = members[i - 1].1;
            out.replace_range(offset(previous) + previous.get().len()..end, "");
        } else {
            let end = if members.len() > 1 {
                end + text[end..].find(',').ok_or("Invalid JSON object")? + 1
            } else {
                end
            };
            out.replace_range(open..end, "");
        }
    } else if let Some(value) = value {
        let (at, comma) = members
            .last()
            .map_or((open, ""), |(_, v)| (offset(v) + v.get().len(), ","));
        out.insert_str(
            at,
            &format!(
                "{comma}{}:{value}",
                serde_json::to_string(key).map_err(|_| "Invalid key")?
            ),
        );
    }
    Ok(out)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn edits_only_the_requested_member_and_keeps_order_and_spacing() {
        let original = "{\n  \"z\": [1, {\"n\":3}], \"a\": 2\n}";
        let added = set(original, "ours", Some("{\"ok\":true}")).unwrap();
        assert!(added.starts_with("{\n  \"z\": [1, {\"n\":3}], \"a\": 2,"));
        assert_eq!(set(&added, "ours", None).unwrap(), original);
        assert_eq!(
            set("{\"ours\":1, \"z\":2}", "ours", None).unwrap(),
            "{ \"z\":2}"
        );
        assert!(set("{\"a\":1,\"a\":2}", "a", None).is_err());
    }
}
