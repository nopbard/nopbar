//! Pure decoding (from spike S1) of Shell_NotifyIcon traffic and encoding of icon callbacks.
//!
//! shell32 sends `WM_COPYDATA` (dwData == 1) to the window of class
//! `Shell_TrayWnd`. The payload is a fixed, 32-bit-handle layout regardless of
//! the sender's bitness:
//!
//! ```text
//! 0   u32 signature (0x34753423)
//! 4   u32 dwMessage (NIM_*)
//! 8   u32 cbSize           \
//! 12  u32 hWnd              |
//! 16  u32 uID               |
//! 20  u32 uFlags            |  NOTIFYICONDATAW with 32-bit handles
//! 24  u32 uCallbackMessage  |
//! 28  u32 hIcon             |
//! 32  [u16;128] szTip       |
//! 288 u32 dwState, 292 u32 dwStateMask
//! 296 [u16;256] szInfo
//! 808 u32 uTimeout/uVersion
//! ...                      /
//! ```
//! Older senders use a smaller cbSize, so every field past hIcon is optional.

pub const NIM_ADD: u32 = 0;
pub const NIM_MODIFY: u32 = 1;
pub const NIM_DELETE: u32 = 2;
pub const NIM_SETFOCUS: u32 = 3;
pub const NIM_SETVERSION: u32 = 4;

pub const NIF_MESSAGE: u32 = 0x1;
pub const NIF_ICON: u32 = 0x2;
pub const NIF_TIP: u32 = 0x4;
pub const NIF_STATE: u32 = 0x8;
pub const NIF_GUID: u32 = 0x20;
/// Valid only for NOTIFYICON_VERSION_4 icons.
pub const NIF_SHOWTIP: u32 = 0x80;
pub const NIS_HIDDEN: u32 = 0x1;

const OFF_TIP: usize = 32;
const TIP_LEN: usize = 128;
const OFF_STATE: usize = 288;
const OFF_STATE_MASK: usize = 292;
const OFF_VERSION: usize = 808;
const OFF_GUID: usize = 944;
const SIGNATURE: u32 = 0x3475_3423;
const HEADER_LEN: usize = 8;
const NID_V1: usize = 152;
const NID_V2: usize = 936;
const NID_V3: usize = 952;
const NID_FULL: usize = 956;
/// The longest declared payload (header + NOTIFYICONDATAW). Senders may
/// append more (1484 bytes seen on 26100): only the declared size is read.
#[cfg(test)]
pub const MAX_TRAY_LEN: usize = HEADER_LEN + NID_FULL;

#[derive(Clone, Debug, PartialEq)]
pub struct TrayMsg {
    pub signature: u32,
    pub message: u32,
    pub hwnd: u32,
    pub uid: u32,
    pub flags: u32,
    pub callback_msg: u32,
    pub hicon: u32,
    pub tip: String,
    /// Explicit on SETVERSION, a compatibility hint on SHOWTIP registrations.
    pub version: u32,
    pub state: u32,
    pub state_mask: u32,
    /// guidItem, when NIF_GUID is set (identifies the icon instead of hWnd+uID).
    pub guid: Option<[u8; 16]>,
}

pub(super) fn u32_at(b: &[u8], off: usize) -> Option<u32> {
    b.get(off..off + 4).map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

fn wstr_at(b: &[u8], off: usize, max_chars: usize) -> String {
    let end = (off + max_chars * 2).min(b.len());
    if off >= end {
        return String::new();
    }
    let units: Vec<u16> = b[off..end]
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .take_while(|&u| u != 0)
        .collect();
    String::from_utf16_lossy(&units)
}

/// Decodes the declared version of the tray `WM_COPYDATA` payload.
pub fn parse_copydata(dw_data: usize, bytes: &[u8]) -> Option<TrayMsg> {
    if dw_data != 1 || u32_at(bytes, 0)? != SIGNATURE {
        return None;
    }
    let message = u32_at(bytes, 4)?;
    if !matches!(message, NIM_ADD | NIM_MODIFY | NIM_DELETE | NIM_SETFOCUS | NIM_SETVERSION) {
        return None;
    }
    let size = u32_at(bytes, HEADER_LEN)? as usize;
    if !matches!(size, NID_V1 | NID_V2 | NID_V3 | NID_FULL) {
        return None;
    }
    let bytes = bytes.get(..HEADER_LEN + size)?;
    let flags = u32_at(bytes, 20)?;
    // uTimeout shares this slot for balloon notifications. Preserve the
    // legacy SHOWTIP registration hint for senders whose SETVERSION we miss.
    let version = if message == NIM_SETVERSION {
        let version = u32_at(bytes, OFF_VERSION)?;
        if !matches!(version, 0 | 3 | 4) {
            return None;
        }
        version
    } else if message == NIM_ADD && flags & NIF_SHOWTIP != 0 {
        u32_at(bytes, OFF_VERSION).filter(|v| matches!(v, 3 | 4)).unwrap_or(0)
    } else {
        0
    };
    Some(TrayMsg {
        signature: SIGNATURE,
        message,
        hwnd: u32_at(bytes, 12)?,
        uid: u32_at(bytes, 16)?,
        flags,
        callback_msg: u32_at(bytes, 24)?,
        hicon: u32_at(bytes, 28)?,
        tip: wstr_at(bytes, OFF_TIP, TIP_LEN),
        version,
        state: u32_at(bytes, OFF_STATE).unwrap_or(0),
        state_mask: u32_at(bytes, OFF_STATE_MASK).unwrap_or(0),
        guid: if flags & NIF_GUID != 0 {
            bytes.get(OFF_GUID..OFF_GUID + 16).map(|g| {
                let mut a = [0u8; 16];
                a.copy_from_slice(g);
                a
            })
        } else {
            None
        },
    })
}

/// Builds (wParam, lParam) for a callback to the icon's owner window.
/// Version 4: wParam = MAKEWPARAM(x, y) in screen coords, lParam =
/// MAKELPARAM(event, uID). Older: wParam = uID, lParam = event.
pub fn pack_callback(version: u32, uid: u32, event: u32, x: i32, y: i32) -> (usize, isize) {
    if version >= 4 {
        let w = (x as i16 as u16 as usize) | ((y as i16 as u16 as usize) << 16);
        let l = ((event & 0xFFFF) | ((uid & 0xFFFF) << 16)) as isize;
        (w, l)
    } else {
        (uid as usize, event as isize)
    }
}

/// Shell_NotifyIconGetRect's question (`WM_COPYDATA`, dwData == 3): which
/// icon, and which corner. The payload:
///
/// ```text
/// 0  u32 signature (0x34753423)
/// 4  u32 dwMessage (1: the top left corner, 2: the bottom right)
/// 8  u32 cbSize, 12 u32 padding
/// 16 hWnd (32 bits; 64 in a 48-byte payload), then u32 uID, then guidItem
/// ```
///
/// Taskbar replacements answer it with the corner packed as MAKELPARAM.
#[derive(Clone, Debug, PartialEq)]
pub struct RectQuery {
    pub corner: u32,
    pub hwnd: u32,
    pub uid: u32,
    /// guidItem, when not all zero.
    pub guid: Option<[u8; 16]>,
}

pub const RECT_TOP_LEFT: u32 = 1;
pub const RECT_BOTTOM_RIGHT: u32 = 2;

/// Decodes a dwData == 3 payload (both handle widths). None for anything else.
pub fn parse_rect_query(dw_data: usize, b: &[u8]) -> Option<RectQuery> {
    if dw_data != 3 || u32_at(b, 0)? != 0x3475_3423 {
        return None;
    }
    let corner = u32_at(b, 4)?;
    if corner != RECT_TOP_LEFT && corner != RECT_BOTTOM_RIGHT {
        return None;
    }
    // A 64-bit handle field makes the payload 48 bytes (guid at 28).
    let (hwnd, uid_at) = if b.len() >= 48 { (u32_at(b, 16)?, 24) } else { (u32_at(b, 16)?, 20) };
    let uid = u32_at(b, uid_at)?;
    let guid = b.get(uid_at + 4..uid_at + 20).map(|g| {
        let mut a = [0u8; 16];
        a.copy_from_slice(g);
        a
    });
    Some(RectQuery { corner, hwnd, uid, guid: guid.filter(|g| g.iter().any(|&x| x != 0)) })
}

/// The answer to a RectQuery for the icon at `r` (screen px): its corner as
/// MAKELPARAM(x, y).
pub fn pack_corner(corner: u32, r: (i32, i32, i32, i32)) -> isize {
    let (x, y) = if corner == RECT_TOP_LEFT { (r.0, r.1) } else { (r.2, r.3) };
    ((x as i16 as u16 as u32) | ((y as i16 as u16 as u32) << 16)) as i32 as isize
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_icon_rect_question_and_answer() {
        let mut b = vec![0u8; 40];
        b[0..4].copy_from_slice(&0x3475_3423u32.to_le_bytes());
        b[4..8].copy_from_slice(&2u32.to_le_bytes());
        b[16..20].copy_from_slice(&0x1059Eu32.to_le_bytes());
        b[20..24].copy_from_slice(&1u32.to_le_bytes());
        assert_eq!(super::parse_rect_query(3, &b), Some(super::RectQuery { corner: 2, hwnd: 0x1059E, uid: 1, guid: None }));
        assert_eq!(super::parse_rect_query(1, &b), None);
        // 64-bit handle: the uid at 24.
        let mut w = vec![0u8; 48];
        w[..20].copy_from_slice(&b[..20]);
        w[24..28].copy_from_slice(&7u32.to_le_bytes());
        w[30] = 9;
        let q = super::parse_rect_query(3, &w).unwrap();
        assert_eq!((q.hwnd, q.uid, q.guid.map(|g| g[2])), (0x1059E, 7, Some(9)));
        // Corners: low word x, high word y, signed.
        assert_eq!(super::pack_corner(1, (2400, 0, 2433, 26)), 2400);
        assert_eq!(super::pack_corner(2, (2400, 0, 2433, 26)), (26 << 16) | 2433);
        assert_eq!(super::pack_corner(1, (-10, 5, 0, 0)) as u32 & 0xFFFF, 0xFFF6);
    }
}

#[cfg(test)]
mod tests_layout {
    use super::*;

    fn put(b: &mut [u8], off: usize, v: u32) {
        b[off..off + 4].copy_from_slice(&v.to_le_bytes());
    }

    fn fixture_add() -> Vec<u8> {
        let mut b = vec![0u8; 964];
        put(&mut b, 0, 0x3475_3423);
        put(&mut b, 4, NIM_ADD);
        put(&mut b, 8, 956);
        put(&mut b, 12, 0x0012_0456);
        put(&mut b, 16, 7);
        put(&mut b, 20, NIF_MESSAGE | NIF_ICON | NIF_TIP);
        put(&mut b, 24, 0x8001);
        put(&mut b, 28, 0x0009_00AB);
        for (i, u) in "Test Tip".encode_utf16().enumerate() {
            b[OFF_TIP + i * 2..OFF_TIP + i * 2 + 2].copy_from_slice(&u.to_le_bytes());
        }
        b
    }

    #[test]
    fn parse_rejects_other_dwdata() {
        assert!(parse_copydata(2, &fixture_add()).is_none());
    }

    #[test]
    fn parse_rejects_short_buffer() {
        assert!(parse_copydata(1, &fixture_add()[..20]).is_none());
    }

    #[test]
    fn parse_add_uses_32bit_handles() {
        let m = parse_copydata(1, &fixture_add()).unwrap();
        assert_eq!((m.message, m.hwnd, m.uid, m.callback_msg, m.hicon), (NIM_ADD, 0x0012_0456, 7, 0x8001, 0x0009_00AB));
        assert_eq!(m.tip, "Test Tip");
    }

    #[test]
    fn parse_old_cbsize_has_no_version() {
        let mut b = fixture_add();
        put(&mut b, HEADER_LEN, NID_V1 as u32);
        put(&mut b, OFF_VERSION, 4);
        let m = parse_copydata(1, &b).unwrap();
        assert_eq!(m.version, 0);
        assert_eq!(m.tip, "Test Tip");
        assert_eq!(parse_copydata(1, &b[..HEADER_LEN + NID_V1]), Some(m));
    }

    #[test]
    fn parse_reads_the_declared_size_of_a_longer_payload() {
        // Windows 11 26100 sends 1484 bytes for a 956 byte NOTIFYICONDATAW.
        let mut b = fixture_add();
        b.resize(1484, 0xAB);
        let m = parse_copydata(1, &b).expect("trailing bytes are ignored");
        assert_eq!(m, parse_copydata(1, &b[..MAX_TRAY_LEN]).unwrap());
    }

    #[test]
    fn parse_rejects_malformed_tray_messages() {
        let mut b = fixture_add();
        put(&mut b, 4, NIM_DELETE);
        put(&mut b, 8, 0);
        put(&mut b, 16, 1_660_971_264);
        put(&mut b, OFF_VERSION, 855_663_872);
        assert!(parse_copydata(1, &b).is_none());
        put(&mut b, 8, NID_FULL as u32);
        assert!(parse_copydata(1, &b[..MAX_TRAY_LEN - 1]).is_none());
        assert_eq!(parse_copydata(1, &b).unwrap().version, 0);
        put(&mut b, 4, NIM_SETVERSION);
        assert!(parse_copydata(1, &b).is_none());
        put(&mut b, OFF_VERSION, 4);
        assert_eq!(parse_copydata(1, &b).unwrap().version, 4);
        put(&mut b, 0, 0);
        assert!(parse_copydata(1, &b).is_none());
    }

    #[test]
    fn showtip_add_keeps_the_legacy_version_hint() {
        let mut b = fixture_add();
        put(&mut b, 20, NIF_SHOWTIP);
        put(&mut b, OFF_VERSION, 3);
        assert_eq!(parse_copydata(1, &b).unwrap().version, 3);
        put(&mut b, 4, NIM_MODIFY);
        assert_eq!(parse_copydata(1, &b).unwrap().version, 0);
    }

    #[test]
    fn pack_v4_negative_coords() {
        let (w, l) = pack_callback(4, 7, 0x007B, -1500, -20);
        assert_eq!((w & 0xFFFF) as u16 as i16, -1500);
        assert_eq!(((w >> 16) & 0xFFFF) as u16 as i16, -20);
        assert_eq!(l, 0x007B | (7 << 16));
    }

    #[test]
    fn parse_state_fields() {
        let mut b = fixture_add();
        put(&mut b, 288, NIS_HIDDEN);
        put(&mut b, 292, NIS_HIDDEN);
        let m = parse_copydata(1, &b).unwrap();
        assert_eq!((m.state, m.state_mask), (NIS_HIDDEN, NIS_HIDDEN));
    }

    #[test]
    fn pack_legacy() {
        assert_eq!(pack_callback(0, 7, 0x0205, 10, 10), (7, 0x0205));
    }
}
