//! Windows accessibility settings shown by the quick settings panel.

use super::*;
use windows::core::PCWSTR;
use windows::Win32::{
    Foundation::{CloseHandle, HANDLE},
    System::{
        Diagnostics::ToolHelp::{
            CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
            TH32CS_SNAPPROCESS,
        },
        Registry::{RegSetKeyValueW, HKEY_CURRENT_USER, REG_DWORD},
        SystemInformation::GetSystemDirectoryW,
        Threading::GetCurrentProcessId,
    },
    UI::{
        Accessibility::{SKF_STICKYKEYSON, STICKYKEYS, STICKYKEYS_FLAGS},
        Input::KeyboardAndMouse::{VK_CONTROL, VK_LWIN},
    },
};

pub(super) const COLOR_KEY: PCWSTR = w!("Software\\Microsoft\\ColorFiltering");
pub(super) const MONO_KEY: PCWSTR = w!("Software\\Microsoft\\Multimedia\\Audio");

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Access {
    Magnifier,
    ColorFilters,
    Narrator,
    MonoAudio,
    Captions,
    StickyKeys,
}

impl Access {
    pub(super) const ALL: [Self; 6] = [
        Self::Magnifier,
        Self::ColorFilters,
        Self::Narrator,
        Self::MonoAudio,
        Self::Captions,
        Self::StickyKeys,
    ];

    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Magnifier => "control.access.magnifier",
            Self::ColorFilters => "control.access.color_filters",
            Self::Narrator => "control.access.narrator",
            Self::MonoAudio => "control.access.mono_audio",
            Self::Captions => "control.access.live_captions",
            Self::StickyKeys => "control.access.sticky_keys",
        }
    }

    pub(super) fn page(self) -> &'static str {
        match self {
            Self::Magnifier => "ms-settings:easeofaccess-magnifier",
            Self::ColorFilters => "ms-settings:easeofaccess-colorfilter",
            Self::Narrator => "ms-settings:easeofaccess-narrator",
            Self::MonoAudio => "ms-settings:easeofaccess-audio",
            Self::Captions => "ms-settings:easeofaccess-closedcaptioning",
            Self::StickyKeys => "ms-settings:easeofaccess-keyboard",
        }
    }

    pub(super) fn glyph(self) -> Glyph {
        match self {
            Self::Magnifier => Glyph::Search,
            Self::ColorFilters => Glyph::Appearance,
            Self::Narrator => Glyph::Speaker,
            Self::MonoAudio => Glyph::Headphones,
            Self::Captions => Glyph::ChatBubble,
            Self::StickyKeys => Glyph::Keyboard,
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub(super) struct AccessView {
    pub(super) on: Option<bool>,
    pub(super) busy: bool,
    pub(super) failed: Option<Instant>,
}

fn sticky() -> Option<STICKYKEYS> {
    let size = std::mem::size_of::<STICKYKEYS>() as u32;
    let mut keys = STICKYKEYS {
        cbSize: size,
        dwFlags: STICKYKEYS_FLAGS(0),
    };
    unsafe {
        SystemParametersInfoW(
            SPI_GETSTICKYKEYS,
            size,
            Some((&raw mut keys).cast()),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
    }
    .ok()?;
    Some(keys)
}

#[derive(Default)]
struct Processes {
    magnifier: Vec<u32>,
    narrator: Vec<u32>,
    captions: Vec<u32>,
}

struct Snapshot(HANDLE);

impl Drop for Snapshot {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

fn processes() -> Option<Processes> {
    use windows::Win32::System::RemoteDesktop::ProcessIdToSessionId;
    unsafe {
        let mut current_session = 0;
        ProcessIdToSessionId(GetCurrentProcessId(), &mut current_session).ok()?;
        let snapshot = Snapshot(CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0).ok()?);
        let mut info = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        Process32FirstW(snapshot.0, &mut info).ok()?;
        let mut out = Processes::default();
        loop {
            let n = info
                .szExeFile
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(info.szExeFile.len());
            let name = String::from_utf16_lossy(&info.szExeFile[..n]);
            let target = match name.to_ascii_lowercase().as_str() {
                "magnify.exe" => Some(&mut out.magnifier),
                "narrator.exe" => Some(&mut out.narrator),
                "livecaptions.exe" => Some(&mut out.captions),
                _ => None,
            };
            if let Some(target) = target {
                let mut session = 0;
                if ProcessIdToSessionId(info.th32ProcessID, &mut session).is_ok()
                    && session == current_session
                {
                    target.push(info.th32ProcessID);
                }
            }
            if Process32NextW(snapshot.0, &mut info).is_err() {
                break;
            }
        }
        Some(out)
    }
}

pub(super) fn read_accessibility() -> [Option<bool>; Access::ALL.len()] {
    let running = processes();
    Access::ALL.map(|a| match a {
        Access::Magnifier => running.as_ref().map(|p| !p.magnifier.is_empty()),
        Access::ColorFilters => {
            Some(crate::syslook::dword(COLOR_KEY, w!("Active")).unwrap_or(0) != 0)
        }
        Access::Narrator => running.as_ref().map(|p| !p.narrator.is_empty()),
        Access::MonoAudio => {
            Some(crate::syslook::dword(MONO_KEY, w!("AccessibilityMonoMixState")).unwrap_or(0) != 0)
        }
        Access::Captions => running.as_ref().map(|p| !p.captions.is_empty()),
        Access::StickyKeys => sticky().map(|k| k.dwFlags.0 & SKF_STICKYKEYSON.0 != 0),
    })
}

fn broker(args: &[&str]) -> bool {
    use std::os::windows::{ffi::OsStringExt, process::CommandExt};
    use windows::Win32::System::Threading::CREATE_NO_WINDOW;
    let mut buffer = [0u16; 32768];
    let n = unsafe { GetSystemDirectoryW(Some(&mut buffer)) } as usize;
    if n == 0 || n >= buffer.len() {
        return false;
    }
    let path =
        std::path::PathBuf::from(std::ffi::OsString::from_wide(&buffer[..n])).join("AtBroker.exe");
    std::process::Command::new(path)
        .args(args)
        .creation_flags(CREATE_NO_WINDOW.0)
        .spawn()
        .is_ok()
}

fn write_color(value: u32) -> bool {
    unsafe {
        RegSetKeyValueW(
            HKEY_CURRENT_USER,
            COLOR_KEY,
            w!("Active"),
            REG_DWORD.0,
            Some((&raw const value).cast()),
            4,
        )
        .is_ok()
    }
}

fn close_captions() -> bool {
    struct Close {
        pids: Vec<u32>,
        sent: bool,
    }
    unsafe extern "system" fn close_window(hwnd: HWND, data: LPARAM) -> BOOL {
        let close = &mut *(data.0 as *mut Close);
        let mut pid = 0;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if close.pids.contains(&pid) && IsWindowVisible(hwnd).as_bool() {
            close.sent |= PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0)).is_ok();
        }
        BOOL(1)
    }
    let Some(p) = processes() else { return false };
    if p.captions.is_empty() {
        return true;
    }
    let mut state = Close {
        pids: p.captions,
        sent: false,
    };
    unsafe {
        let _ = EnumWindows(Some(close_window), LPARAM((&raw mut state) as isize));
    }
    state.sent
}

pub(super) fn set_accessibility(a: Access, on: bool) -> bool {
    if cfg!(test) {
        return false;
    }
    match (a, on) {
        (Access::Magnifier, true) => broker(&["/start", "magnifierpane"]),
        (Access::Narrator, true) => broker(&["/start", "Narrator"]),
        (Access::Captions, true) => broker(&["/start", "LiveCaptions"]),
        (Access::Captions, false) => close_captions(),
        (Access::Magnifier, false) => {
            crate::keys::unmixed(&[VK_LWIN.0, VK_ESCAPE.0]) == crate::keys::Sent::Ok
        }
        (Access::Narrator, false) => {
            crate::keys::unmixed(&[VK_LWIN.0, VK_CONTROL.0, VK_RETURN.0]) == crate::keys::Sent::Ok
        }
        (Access::ColorFilters, _) => {
            let previous = crate::syslook::dword(COLOR_KEY, w!("Active")).unwrap_or(0);
            let wanted = u32::from(on);
            if !write_color(wanted) {
                return false;
            }
            if broker(&["/colorfiltershortcut", "/resettransferkeys"]) {
                true
            } else {
                if crate::syslook::dword(COLOR_KEY, w!("Active")) == Some(wanted) {
                    let _ = write_color(previous);
                }
                false
            }
        }
        (Access::StickyKeys, _) => {
            let Some(mut flags) = sticky() else {
                return false;
            };
            flags.dwFlags.0 = (flags.dwFlags.0 & !SKF_STICKYKEYSON.0) | u32::from(on);
            unsafe {
                SystemParametersInfoW(
                    SPI_SETSTICKYKEYS,
                    flags.cbSize,
                    Some((&raw mut flags).cast()),
                    SPIF_UPDATEINIFILE | SPIF_SENDCHANGE,
                )
                .is_ok()
            }
        }
        (Access::MonoAudio, _) => false,
    }
}
