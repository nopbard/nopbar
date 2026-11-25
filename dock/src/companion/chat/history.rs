//! Opt-in history; the provider receives only its existing 20-turn context.
use serde::{Deserialize,Serialize};
use std::{collections::VecDeque,io,path::{Path,PathBuf}};
#[derive(Default,Serialize,Deserialize)]
pub struct History {
    pub turns: VecDeque<(String,String)>,
    #[serde(skip)]
    pub revision:u64,
    #[serde(skip)]
    pub links:std::collections::HashMap<u64,(usize,bool)>,
}
impl History {
    pub fn push(&mut self,user:String,answer:String) {
        self.turns.push_back((user,answer));self.trim();self.revision+=1;
    }
    pub fn link_last(&mut self,user:Option<u64>,answer:Option<u64>) {
        let Some(index)=self.turns.len().checked_sub(1)else{return;};
        for (id,is_user) in [(user,true),(answer,false)] {if let Some(id)=id {self.links.insert(id,(index,is_user));}}
    }
    fn trim(&mut self){while self.turns.len()>200 {self.turns.pop_front();self.links.retain(|_,(index,_)|{if *index==0 {false}else{*index-=1;true}});}}
    pub fn read(path:&Path)->Self {
        if std::fs::metadata(path).map_or(true,|m|m.len()>64*1024*1024){return Self::default();}
        let mut result:Self=std::fs::read(path).ok().and_then(|b|serde_json::from_slice(&b).ok()).unwrap_or_default();result.trim();result
    }
    pub fn save(&self,path:&Path,enabled:bool)->io::Result<()> {
        if !enabled {return match std::fs::remove_file(path){Err(e) if e.kind()==io::ErrorKind::NotFound=>Ok(()),other=>other};}
        if let Some(parent)=path.parent(){std::fs::create_dir_all(parent)?;}
        let bytes=serde_json::to_vec(self)?;
        // Atomic replacement uses the same per-user filesystem as preferences.
        let temp=path.with_extension("json.tmp");std::fs::write(&temp,bytes)?;
        #[cfg(windows)] unsafe {
            use windows::{core::HSTRING,Win32::Storage::FileSystem::{MoveFileExW,MOVEFILE_REPLACE_EXISTING,MOVEFILE_WRITE_THROUGH}};
            MoveFileExW(&HSTRING::from(temp.as_os_str()),&HSTRING::from(path.as_os_str()),MOVEFILE_REPLACE_EXISTING|MOVEFILE_WRITE_THROUGH).map_err(io::Error::other)?;
        }
        Ok(())
    }
}
pub fn path()->PathBuf { std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_default().join("NopBar/chat-history.json") }

#[cfg(test)] mod tests {
    use super::*;
    #[test] fn duplicate_text_uses_message_ids_and_trimming_moves_links() {
        let mut h=History::default();
        h.push("same".into(),"same".into());h.link_last(Some(10),Some(11));
        h.push("same".into(),"same".into());h.link_last(Some(20),Some(21));
        assert_eq!(h.links[&11],(0,false));assert_eq!(h.links[&21],(1,false));
        for _ in 0..199 {h.push("u".into(),"a".into());}
        assert!(!h.links.contains_key(&11));assert_eq!(h.links[&21],(0,false));
    }
    #[test] fn opt_in_creates_caps_reloads_and_off_deletes() {
        let path=std::env::temp_dir().join(format!("nopbar-bubble-history-test-{}.json",std::process::id()));
        let mut h=History::default();h.save(&path,false).unwrap();assert!(!path.exists());
        for i in 0..205 {h.push(i.to_string(),"답".into());}
        h.save(&path,true).unwrap();let h=History::read(&path);assert_eq!(h.turns.len(),200);assert_eq!(h.turns[0].0,"5");
        h.save(&path,false).unwrap();assert!(!path.exists());
    }
}
