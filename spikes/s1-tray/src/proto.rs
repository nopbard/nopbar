//! Pure decoding of Shell_NotifyIcon traffic and encoding of icon callbacks.
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

const OFF_TIP: usize = 32;
const TIP_LEN: usize = 128;
const OFF_VERSION: usize = 808;
const MIN_LEN: usize = OFF_TIP; // through hIcon

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
    pub version: u32,
}

fn u32_at(b: &[u8], off: usize) -> Option<u32> {
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

/// Decodes a tray `WM_COPYDATA` payload. `None` for other dwData values or
/// buffers too short to hold the fields through `hIcon`.
pub fn parse_copydata(dw_data: usize, bytes: &[u8]) -> Option<TrayMsg> {
    if dw_data != 1 || bytes.len() < MIN_LEN {
        return None;
    }
    Some(TrayMsg {
        signature: u32_at(bytes, 0)?,
        message: u32_at(bytes, 4)?,
        hwnd: u32_at(bytes, 12)?,
        uid: u32_at(bytes, 16)?,
        flags: u32_at(bytes, 20)?,
        callback_msg: u32_at(bytes, 24)?,
        hicon: u32_at(bytes, 28)?,
        tip: wstr_at(bytes, OFF_TIP, TIP_LEN),
        version: u32_at(bytes, OFF_VERSION).unwrap_or(0),
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

#[cfg(test)]
mod tests {
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
        let m = parse_copydata(1, &fixture_add()[..152]).unwrap();
        assert_eq!(m.version, 0);
        assert_eq!(m.tip, "Test Tip");
    }

    #[test]
    fn pack_v4_negative_coords() {
        let (w, l) = pack_callback(4, 7, 0x007B, -1500, -20);
        assert_eq!((w & 0xFFFF) as u16 as i16, -1500);
        assert_eq!(((w >> 16) & 0xFFFF) as u16 as i16, -20);
        assert_eq!(l, 0x007B | (7 << 16));
    }

    #[test]
    fn pack_legacy() {
        assert_eq!(pack_callback(0, 7, 0x0205, 10, 10), (7, 0x0205));
    }
}
