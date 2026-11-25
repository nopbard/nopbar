//! IMM input without a child EDIT. Field owns selection in Unicode scalar indexes.
use crate::search::edit::Field;
use windows::Win32::{Foundation::*, UI::Input::{Ime::*, KeyboardAndMouse::*}};

#[derive(Default)]
pub struct Editor {
    pub field: Field,
    pub composing: bool,
    pub composition: Vec<char>,
    pub comp_cursor: usize,
    pub suppress_enter: bool,
    surrogate: Option<u16>,
}
impl Editor {
    pub fn insert(&mut self,s:&str) {self.field.insert_multiline(s);}
    pub fn displayed(&self)->String {self.field.with_composition(&self.composition).0.iter().collect()}
    pub fn caret(&self)->u32 {
        let (v,start)=self.field.with_composition(&self.composition);
        v.iter().take(if self.composition.is_empty(){self.field.caret()}else{start+self.comp_cursor}).map(|c|c.len_utf16() as u32).sum()
    }
    pub fn selection(&self)->(u32,u32) {
        let s=self.field.text();let(a,b)=self.field.selection();
        (s.chars().take(a).map(|c|c.len_utf16() as u32).sum(),s.chars().take(b).map(|c|c.len_utf16() as u32).sum())
    }
    pub fn comp_range(&self)->Option<(u32,u32)> {
        if self.composition.is_empty(){None}else{let a=self.selection().0;Some((a,a+self.composition.iter().map(|c|c.len_utf16() as u32).sum::<u32>()))}
    }
    pub fn click(&mut self,utf16:u32,extend:bool) {
        let mut count=0;let at=self.field.text().chars().take_while(|c|{count+=c.len_utf16() as u32;count<=utf16}).count();self.field.click(at,extend);
    }
    pub fn character(&mut self,u:u16) {
        if u==13 && self.suppress_enter {return;}
        if self.composing {return;}
        if (0xd800..=0xdbff).contains(&u){self.surrogate=Some(u);return;}
        let text=if let Some(high)=self.surrogate.take(){String::from_utf16_lossy(&[high,u])}else{String::from_utf16_lossy(&[u])};
        if u==13 {self.insert("\n");}else if u>=32 {self.insert(&text);}
    }
    pub unsafe fn edit_key(&mut self,h:HWND,vk:u32,ctrl:bool,shift:bool) {
        if self.composing{return;}
        match vk as u16 {
            v if v==VK_BACK.0 => self.field.backspace(ctrl),
            v if v==VK_DELETE.0 => self.field.delete(ctrl),
            v if v==VK_LEFT.0 => self.field.left(ctrl,shift),
            v if v==VK_RIGHT.0 => self.field.right(ctrl,shift),
            v if v==VK_HOME.0 => self.field.home(shift),
            v if v==VK_END.0 => self.field.end(shift),
            v if ctrl && v==b'A' as u16 => self.field.select_all(),
            v if ctrl && v==b'V' as u16 => {if let Some(s)=crate::search::clipboard_text(h){self.insert(&s);}},
            v if ctrl && (v==b'C' as u16 || v==b'X' as u16) => {
                let(a,b)=self.field.selection();let s:String=self.field.text().chars().skip(a).take(b-a).collect();
                if !s.is_empty() && crate::search::copy_text(h,&s) && v==b'X' as u16 {self.field.delete(false);}
            }
            _=>{}
        }
    }
    pub unsafe fn compose(&mut self,h:HWND,flags:u32) {
        let ctx=ImmGetContext(h);if ctx.is_invalid(){return;}
        if flags&GCS_RESULTSTR.0!=0 {let s=ime_string(ctx,GCS_RESULTSTR);self.insert(&s);self.composition.clear();if GetKeyState(VK_RETURN.0 as i32)<0{self.suppress_enter=true;}}
        self.composition=if flags&GCS_COMPSTR.0!=0{ime_string(ctx,GCS_COMPSTR).chars().collect()}else{Vec::new()};
        let pos=ImmGetCompositionStringW(ctx,GCS_CURSORPOS,None,0);
        let mut units=0;self.comp_cursor=if pos<0{self.composition.len()}else{self.composition.iter().take_while(|c|{units+=c.len_utf16() as i32;units<=pos}).count()};
        let _=ImmReleaseContext(h,ctx);
    }
}
unsafe fn ime_string(ctx:HIMC,kind:IME_COMPOSITION_STRING)->String {
    let bytes=ImmGetCompositionStringW(ctx,kind,None,0);
    if bytes<=0 || bytes>65536{return String::new();}
    let mut buf=vec![0u16;bytes as usize/2];
    let got=ImmGetCompositionStringW(ctx,kind,Some(buf.as_mut_ptr() as _),bytes as u32);
    if got<=0 {return String::new();} String::from_utf16_lossy(&buf[..got as usize/2])
}
pub unsafe fn place_ime(h:HWND,x:f32,y:f32,dpi:f32) {
    let ctx=ImmGetContext(h);if ctx.is_invalid(){return;}
    let(x,y)=((x*dpi).round() as i32,(y*dpi).round() as i32);let bottom=y+(super::bubbles::LINE*dpi).ceil() as i32;
    let _=ImmSetCompositionWindow(ctx,&COMPOSITIONFORM{dwStyle:CFS_POINT,ptCurrentPos:POINT{x,y},rcArea:RECT::default()});
    let _=ImmSetCandidateWindow(ctx,&CANDIDATEFORM{dwIndex:0,dwStyle:CFS_EXCLUDE,ptCurrentPos:POINT{x,y:bottom},rcArea:RECT{left:x,top:y,right:x+1,bottom}});
    let _=ImmReleaseContext(h,ctx);
}
#[cfg(test)] mod tests {
    use super::*;
    #[test] fn composition_enter_does_not_send_or_insert_newline() {
        let mut e=Editor{composing:true,..Default::default()};
        assert_eq!(super::super::state::key(13,false,e.composing),super::super::state::Key::Native);
        e.character(13);assert_eq!(e.field.text(),"");
        e.composing=false;e.suppress_enter=true;e.character(13);assert_eq!(e.field.text(),"");
        e.suppress_enter=false;e.character(13);assert_eq!(e.field.text(),"\n");
    }
    #[test] fn multiline_surrogates_and_composition_at_selection() {
        let mut e=Editor::default();e.insert("가\r\n나");assert_eq!(e.field.text(),"가\n나");
        e.character(0xd83d);e.character(0xde00);assert_eq!(e.caret(),5);
        e.field.select_all();e.composition=vec!['한'];e.comp_cursor=1;
        assert_eq!(e.displayed(),"한");assert_eq!(e.caret(),1);assert_eq!(e.comp_range(),Some((0,1)));
    }
}
