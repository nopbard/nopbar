//! Shared runtime/offscreen drawing. Geometry is in DIP, never screen pixels.
use super::{inbox::Row, time::Found};
use crate::companion::chat::paint::{color, rect, rounded};
use crate::{gfx::Gfx, i18n::t, theme::Theme};
use windows::{
    core::Result,
    Win32::Graphics::{
        Direct2D::{Common::*, *},
        DirectWrite::*,
    },
};
use windows_numerics::Vector2;
pub const WIDTH: f32 = 560.;
pub const HEIGHT: f32 = 56.;

#[allow(
    clippy::too_many_arguments,
    reason = "D2D text primitive: context, font, position, style and clipping width"
)]
pub unsafe fn text(
    dc: &ID2D1DeviceContext,
    gfx: &Gfx,
    s: &str,
    x: f32,
    y: f32,
    size: f32,
    bold: bool,
    ink: D2D1_COLOR_F,
    max: f32,
) -> Result<f32> {
    let (l, w, _) = gfx.text_layout(
        s,
        crate::fonts::THEME_DEFAULT,
        size,
        if bold {
            DWRITE_FONT_WEIGHT_SEMI_BOLD
        } else {
            DWRITE_FONT_WEIGHT_NORMAL
        },
    )?;
    l.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
    l.SetMaxWidth(max.max(1.))?;
    let sign=gfx.dwrite().CreateEllipsisTrimmingSign(&l)?;
    l.SetTrimming(&DWRITE_TRIMMING{granularity:DWRITE_TRIMMING_GRANULARITY_CHARACTER,..Default::default()},&sign)?;
    let b = dc.CreateSolidColorBrush(&ink, None)?;
    dc.PushAxisAlignedClip(
        &rect(x, y, max, size * 1.6),
        D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
    );
    dc.DrawTextLayout(Vector2 { X: x, Y: y }, &l, &b, D2D1_DRAW_TEXT_OPTIONS_NONE);
    dc.PopAxisAlignedClip();
    Ok(w)
}
pub unsafe fn pill(
    dc: &ID2D1DeviceContext,
    gfx: &Gfx,
    s: &str,
    r: D2D_RECT_F,
    on: bool,
    light: bool,
) -> Result<()> {
    let ink = if on {
        color(0.09, 1.)
    } else {
        color(if light { 0. } else { 1. }, 0.90)
    };
    rounded(
        dc,
        r,
        (r.bottom - r.top) / 2.,
        if on {
            color(0.969, 1.)
        } else {
            color(if light { 0. } else { 1. }, 0.08)
        },
    )?;
    let (layout,width,height)=gfx.text_layout(s,crate::fonts::THEME_DEFAULT,12.,DWRITE_FONT_WEIGHT_SEMI_BOLD)?;
    let width=width.min((r.right-r.left-12.).max(1.));
    layout.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
    layout.SetMaxWidth(width)?;layout.SetMaxHeight(height)?;
    let sign=gfx.dwrite().CreateEllipsisTrimmingSign(&layout)?;
    layout.SetTrimming(&DWRITE_TRIMMING{granularity:DWRITE_TRIMMING_GRANULARITY_CHARACTER,..Default::default()},&sign)?;
    let (x,y)=label_origin(r,width,height);let brush=dc.CreateSolidColorBrush(&ink,None)?;
    dc.PushAxisAlignedClip(&r,D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
    dc.DrawTextLayout(Vector2{X:x,Y:y},&layout,&brush,D2D1_DRAW_TEXT_OPTIONS_NONE);
    dc.PopAxisAlignedClip();
    Ok(())
}
pub struct Capture<'a> {
    pub text: &'a str,
    pub target: usize,
    pub tabs: bool,
    pub found: Option<&'a Found>,
    pub now: i64,
    pub caret: bool,
    pub caret_at: Option<u32>,
    pub selection: (u32, u32),
    pub popup: bool,
    pub lead: u8,
    pub changing: bool,
}
/// Same text origin and clipping edge for painting, hit testing and IME placement.
pub fn input_edges(
    gfx: &Gfx,
    target: usize,
    tabs: bool,
    found: Option<&Found>,
    now: i64,
) -> Result<(f32, f32)> {
    let (_, width, _) = gfx.text_layout(
        t(if target == 0 {
            "capture.inbox"
        } else {
            "notes.new"
        }),
        crate::fonts::THEME_DEFAULT,
        12.,
        DWRITE_FONT_WEIGHT_SEMI_BOLD,
    )?;
    let right = if let Some(f) = found {
        let (_, width, _) = gfx.text_layout(
            &super::time::label(f.at, now, crate::i18n::current()),
            crate::fonts::THEME_DEFAULT,
            12.,
            DWRITE_FONT_WEIGHT_NORMAL,
        )?;
        WIDTH - 62. - width
    } else {
        WIDTH - if tabs { 12. } else { 106. }
    };
    Ok((46. + width, right))
}
pub unsafe fn capture(
    dc: &ID2D1DeviceContext,
    gfx: &Gfx,
    theme: &Theme,
    c: &Capture<'_>,
) -> Result<()> {
    let light = theme.is_light;
    let v = if light { 0. } else { 1. };
    let dest = t(if c.target == 0 {
        "capture.inbox"
    } else {
        "notes.new"
    });
    let (_, dw, _) = gfx.text_layout(
        dest,
        crate::fonts::THEME_DEFAULT,
        12.,
        DWRITE_FONT_WEIGHT_SEMI_BOLD,
    )?;
    let pw = dw + 20.;
    pill(dc, gfx, dest, rect(14., 14., pw, 28.), c.tabs, light)?;
    let x = 14. + pw + 12.;
    let right = if let Some(f) = c.found {
        let label = super::time::label(f.at, c.now, crate::i18n::current());
        let (_, w, _) = gfx.text_layout(
            &label,
            crate::fonts::THEME_DEFAULT,
            12.,
            DWRITE_FONT_WEIGHT_NORMAL,
        )?;
        let left = WIDTH - 12. - w - 38.;
        rounded(dc, rect(left, 14., w + 38., 28.), 14., color(v, 0.08))?;
        crate::glyphs::draw(
            dc,
            crate::glyphs::Glyph::Bell,
            left + 14.,
            28.,
            12.,
            color(v, 0.75),
        )?;
        text(
            dc,
            gfx,
            &label,
            left + 27.,
            19.,
            12.,
            false,
            color(v, 0.75),
            w + 1.,
        )?;
        left - 12.
    } else {
        if !c.tabs {
            text(
                dc,
                gfx,
                t("capture.destination"),
                WIDTH - 94.,
                21.,
                11.,
                false,
                color(v, 0.42),
                82.,
            )?;
        }
        WIDTH - if c.tabs { 12. } else { 106. }
    };
    let s = if c.text.is_empty() {
        t("capture.placeholder")
    } else {
        c.text
    };
    let (l, _, _) = gfx.text_layout(
        s,
        crate::fonts::THEME_DEFAULT,
        20.,
        DWRITE_FONT_WEIGHT_NORMAL,
    )?;
    l.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
    let (mut caret_x, mut caret_y, mut caret_hit) = (0., 0., DWRITE_HIT_TEST_METRICS::default());
    if !c.text.is_empty() {
        l.HitTestTextPosition(
            c.caret_at.unwrap_or(c.text.encode_utf16().count() as u32),
            false,
            &mut caret_x,
            &mut caret_y,
            &mut caret_hit,
        )?;
    }
    let scroll = (caret_x - (right - x - 3.)).max(0.);
    let tx = x - scroll;
    dc.PushAxisAlignedClip(
        &rect(x, 12., (right - x).max(1.), 32.),
        D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
    );
    if c.selection.0 < c.selection.1 {
        let (mut a, mut ay, mut z, mut zy, mut hit) =
            (0., 0., 0., 0., DWRITE_HIT_TEST_METRICS::default());
        l.HitTestTextPosition(c.selection.0, false, &mut a, &mut ay, &mut hit)?;
        l.HitTestTextPosition(c.selection.1, false, &mut z, &mut zy, &mut hit)?;
        rounded(dc, rect(tx + a, 15., z - a, 26.), 2., color(v, 0.18))?;
    }
    let b =
        dc.CreateSolidColorBrush(&color(v, if c.text.is_empty() { 0.42 } else { 0.90 }), None)?;
    dc.DrawTextLayout(
        Vector2 {
            X: tx + if c.text.is_empty() { 5. } else { 0. },
            Y: 14.,
        },
        &l,
        &b,
        D2D1_DRAW_TEXT_OPTIONS_NONE,
    );
    if let Some(f) = c.found {
        let a = c.text[..f.span.start.min(c.text.len())]
            .encode_utf16()
            .count() as u32;
        let z = c.text[..f.span.end.min(c.text.len())]
            .encode_utf16()
            .count() as u32;
        let (mut ax, mut ay, mut bx, mut by, mut hit) =
            (0., 0., 0., 0., DWRITE_HIT_TEST_METRICS::default());
        l.HitTestTextPosition(a, false, &mut ax, &mut ay, &mut hit)?;
        l.HitTestTextPosition(z, false, &mut bx, &mut by, &mut hit)?;
        let b = dc.CreateSolidColorBrush(&color(v, 0.55), None)?;
        dc.DrawLine(
            Vector2 { X: tx + ax, Y: 40. },
            Vector2 { X: tx + bx, Y: 40. },
            &b,
            2.,
            None,
        );
    }
    if c.caret {
        let b = dc.CreateSolidColorBrush(&color(v, 0.90), None)?;
        dc.DrawLine(
            Vector2 {
                X: tx + caret_x,
                Y: 17.,
            },
            Vector2 {
                X: tx + caret_x,
                Y: 39.,
            },
            &b,
            2.,
            None,
        );
    }
    dc.PopAxisAlignedClip();
    if c.tabs {
        let mut left = 0.;
        for (i, key) in ["capture.inbox", "notes.new"].iter().enumerate() {
            let (_, w, _) = gfx.text_layout(
                t(key),
                crate::fonts::THEME_DEFAULT,
                12.,
                DWRITE_FONT_WEIGHT_SEMI_BOLD,
            )?;
            pill(
                dc,
                gfx,
                t(key),
                rect(left, 64., w + 20., 24.),
                i == c.target,
                light,
            )?;
            left += w + 24.;
        }
    }
    if c.popup {
        popover(dc, gfx, theme, c.found, c.now, c.lead, c.changing)?;
    }
    Ok(())
}
pub unsafe fn popover(
    dc: &ID2D1DeviceContext,
    gfx: &Gfx,
    theme: &Theme,
    found: Option<&Found>,
    now: i64,
    lead: u8,
    changing: bool,
) -> Result<()> {
    let v = if theme.is_light { 0. } else { 1. };
    let x = WIDTH - 252.;
    let y = 64.;
    rounded(
        dc,
        rect(x, y, 252., 150.),
        12.,
        color(if theme.is_light { 0.96 } else { 0.15 }, 0.98),
    )?;
    if let Some(f) = found {
        text(
            dc,
            gfx,
            &super::time::label(f.at, now, crate::i18n::current()),
            x + 12.,
            y + 12.,
            13.,
            true,
            color(v, 0.90),
            228.,
        )?;
    }
    text(
        dc,
        gfx,
        t("widgets.notes_lead"),
        x + 12.,
        y + 40.,
        11.,
        false,
        color(v, 0.42),
        228.,
    )?;
    for (i, (key, n)) in [
        ("remind.on_time", 0),
        ("remind.five_before", 5),
        ("remind.ten_before", 10),
    ]
    .into_iter()
    .enumerate()
    {
        pill(
            dc,
            gfx,
            t(key),
            rect(x + 12. + i as f32 * 76., y + 60., 72., 28.),
            lead == n,
            theme.is_light,
        )?;
    }
    text(
        dc,
        gfx,
        t(if changing {
            "capture.change_placeholder"
        } else {
            "remind.change"
        }),
        x + 12.,
        y + 100.,
        13.,
        false,
        color(v, 0.90),
        125.,
    )?;
    text(
        dc,
        gfx,
        t("remind.disable"),
        x + 150.,
        y + 100.,
        13.,
        false,
        color(v, 0.62),
        90.,
    )?;
    Ok(())
}
pub struct Inbox<'a> {
    pub rows: &'a [Row],
    pub remaining: usize,
    pub visible: &'a [usize],
    pub hover: Option<usize>,
    pub flash: Option<(usize, f32)>,
}
#[cfg(test)]
pub fn inbox_height(_i:&Inbox<'_>)->f32 { crate::desklayer::widget::Size::MEDIUM.dimensions().1 }
#[cfg(test)]
pub unsafe fn inbox(dc:&ID2D1DeviceContext,gfx:&Gfx,theme:&Theme,i:&Inbox<'_>)->Result<()> {
    inbox_sized(dc,gfx,theme,i,crate::desklayer::widget::Size::MEDIUM.dimensions())
}
#[cfg(test)]
pub unsafe fn inbox_sized(dc:&ID2D1DeviceContext,gfx:&Gfx,theme:&Theme,i:&Inbox<'_>,(w,h):(f32,f32))->Result<()> {
    use crate::desklayer::card;
    card::plate(dc,theme,w,h,false,None)?;
    inbox_content(dc,gfx,theme,i,(w,h),0.)
}
/// The inbox's rows: from under the header, 30 DIP each.
pub const ROW_TOP: f32 = 42.;
pub const ROW_H: f32 = 30.;
/// Pure: the rows a card `h` tall shows.
pub fn rows_fit(h: f32) -> usize {
    ((h - crate::desklayer::card::PAD - ROW_TOP) / ROW_H).floor().max(0.) as usize
}
/// An item: 14 on 30; its time 12/600 in a pill.
const ITEM: crate::desklayer::card::Line = crate::desklayer::card::Line::new(14., 400, ROW_H);
const TIME: crate::desklayer::card::Line = crate::desklayer::card::Line { tabular: true, ..crate::desklayer::card::Line::new(12., 600, 20.) };
/// mock4.html's inbox: round checks (18, 1.6 rim in the third grey), the
/// time in a pill, done items in the third grey struck through with the
/// check filled in the second.
pub unsafe fn inbox_content(dc:&ID2D1DeviceContext,gfx:&Gfx,theme:&Theme,i:&Inbox<'_>,(w,h):(f32,f32),menu_alpha:f32)->Result<()> {
    use crate::desklayer::card::{self, ink, PAD};
    card::header(dc,gfx,theme,card::Kind::Inbox,crate::i18n::t("widgets.inbox"),&inbox_meta(i.remaining,menu_alpha),w)?;
    let v=if theme.is_light {0.}else{1.};
    if i.visible.is_empty() { card::line(dc,gfx,crate::i18n::t("dock.stack.empty"),PAD+28.,ROW_TOP,ITEM,ink(theme,3),w-2.*PAD-28.)?; }
    for (r,&idx) in i.visible.iter().take(rows_fit(h)).enumerate() {
        let row=&i.rows[idx];let y=ROW_TOP+r as f32*ROW_H;let cy=y+ROW_H/2.;let done=row.completed.is_some();
        let alpha=if i.hover==Some(idx) {0.06}else{i.flash.filter(|(j,_)|*j==idx).map_or(0.,|(_,t)|0.08*(1.-t/0.9).clamp(0.,1.))};
        if alpha>0. {rounded(dc,rect(8.,y,w-16.,ROW_H),8.,color(v,alpha))?;}
        let check=D2D1_ELLIPSE{point:Vector2{X:PAD+9.,Y:cy},radiusX:9.,radiusY:9.};
        if done {
            dc.FillEllipse(&check,&dc.CreateSolidColorBrush(&card::rgba(ink(theme,2)),None)?);
        } else {
            let rim=D2D1_ELLIPSE{radiusX:8.2,radiusY:8.2,..check};
            dc.DrawEllipse(&rim,&dc.CreateSolidColorBrush(&card::rgba(ink(theme,3)),None)?,1.6,None);
        }
        let x=PAD+18.+10.;
        let mut right=w-PAD;
        if let (false,Some(at))=(done,row.remind) {
            let label=super::time::clock(at);
            let pw=card::width(gfx,&label,TIME)+16.;
            let pill=if theme.is_light {color(0.,0.06)} else {color(1.,0.10)};
            rounded(dc,rect(right-pw,cy-10.,pw,20.),6.,pill)?;
            card::line(dc,gfx,&label,right-pw+8.,cy-10.,TIME,ink(theme,1),pw-8.)?;
            right-=pw+8.;
        }
        let tw=card::line(dc,gfx,&row.text,x,y,ITEM,ink(theme,if done{3}else{1}),(right-x).max(1.))?;
        if done {
            let b=dc.CreateSolidColorBrush(&card::rgba(ink(theme,3)),None)?;
            dc.DrawLine(Vector2{X:x,Y:cy+0.5},Vector2{X:x+tw,Y:cy+0.5},&b,1.,None);
        }
    }
    Ok(())
}


fn label_origin(r:D2D_RECT_F,width:f32,height:f32)->(f32,f32){(r.left+(r.right-r.left-width)/2.,r.top+(r.bottom-r.top-height)/2.)}
fn inbox_meta(remaining:usize,menu_alpha:f32)->String{if menu_alpha>0. {String::new()}else{remaining.to_string()}}
#[cfg(test)] mod review_tests {
 use super::*;
 #[test] fn review_10_pills_chips_and_arrows_center_both_axes(){
  for (w,h,tw,th) in [(110.,36.,36.,16.),(70.,28.,24.,16.),(28.,28.,6.,16.),(332.,36.,52.,16.)] {
   let r=rect(20.,40.,w,h);let(x,y)=label_origin(r,tw,th);
   assert_eq!(x+tw/2.,r.left+w/2.);assert_eq!(y+th/2.,r.top+h/2.);
  }
 }
 #[test] fn review_12_inbox_count_hides_for_entire_menu_fade(){assert_eq!(inbox_meta(2,0.),"2");for a in [0.001,0.5,1.] {assert_eq!(inbox_meta(2,a),"");}}
}
