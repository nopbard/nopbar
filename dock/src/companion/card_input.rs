//! Nonactivating cards observe dismissal on their own Win32 message thread.
use std::{
    cell::Cell,
    sync::{
        atomic::{AtomicU32, AtomicUsize, Ordering},
        Arc,
    },
};
use windows::Win32::{
    Foundation::*,
    System::{LibraryLoader::GetModuleHandleW, Threading::GetCurrentThreadId},
    UI::{Input::KeyboardAndMouse::VK_ESCAPE, WindowsAndMessaging::*},
};

pub(super) const DISMISS: u32 = WM_APP + 1;
const STOPPED: u32 = u32::MAX;
static NEXT_TOKEN: AtomicUsize = AtomicUsize::new(0);
thread_local! { static TARGET: Cell<(HWND, usize)> = const { Cell::new((HWND(std::ptr::null_mut()), 0)) }; }

pub(super) struct Hooks {
    thread: Arc<AtomicU32>,
    pub token: usize,
}

impl Hooks {
    pub fn start(hwnd: HWND) -> Option<Self> {
        let hwnd = hwnd.0 as isize;
        let thread = Arc::new(AtomicU32::new(0));
        let state = Arc::clone(&thread);
        let token = NEXT_TOKEN.fetch_add(1, Ordering::Relaxed);
        std::thread::Builder::new()
            .name("companion-card-input".into())
            .spawn(move || unsafe { run(HWND(hwnd as *mut _), token, &state) })
            .ok()?;
        Some(Self { thread, token })
    }
}

impl Drop for Hooks {
    fn drop(&mut self) {
        let tid = self.thread.swap(STOPPED, Ordering::AcqRel);
        if tid != 0 && tid != STOPPED {
            unsafe {
                let _ = PostThreadMessageW(tid, WM_QUIT, WPARAM(0), LPARAM(0));
            }
        }
    }
}

unsafe fn run(hwnd: HWND, token: usize, state: &AtomicU32) {
    let Ok(module) = GetModuleHandleW(None) else {
        return;
    };
    let mut msg = MSG::default();
    let _ = PeekMessageW(&mut msg, None, 0, 0, PM_NOREMOVE);
    let thread = GetCurrentThreadId();
    if state
        .compare_exchange(0, thread, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return;
    }
    TARGET.set((hwnd, token));
    let mouse = SetWindowsHookExW(WH_MOUSE_LL, Some(outside_click), Some(module.into()), 0).ok();
    let keyboard = SetWindowsHookExW(WH_KEYBOARD_LL, Some(escape), Some(module.into()), 0).ok();
    if mouse.is_some() || keyboard.is_some() {
        while GetMessageW(&mut msg, None, 0, 0).0 > 0 {
            DispatchMessageW(&msg);
        }
    }
    for hook in [mouse, keyboard].into_iter().flatten() {
        let _ = UnhookWindowsHookEx(hook);
    }
    let _ = state.compare_exchange(thread, STOPPED, Ordering::AcqRel, Ordering::Acquire);
}

unsafe fn dismiss_at(hwnd: HWND, token: usize, at: u32) {
    let _ = PostMessageW(
        Some(hwnd),
        DISMISS,
        WPARAM(at as usize),
        LPARAM(token as isize),
    );
}

unsafe extern "system" fn outside_click(code: i32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if code >= 0
        && lp.0 != 0
        && matches!(
            wp.0 as u32,
            WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN | WM_XBUTTONDOWN
        )
    {
        let (hwnd, token) = TARGET.get();
        let input = &*(lp.0 as *const MSLLHOOKSTRUCT);
        let mut bounds = RECT::default();
        if GetWindowRect(hwnd, &mut bounds).is_ok() && !inside(bounds, input.pt) {
            dismiss_at(hwnd, token, input.time);
        }
    }
    CallNextHookEx(None, code, wp, lp)
}

fn inside(rect: RECT, point: POINT) -> bool {
    (rect.left..rect.right).contains(&point.x) && (rect.top..rect.bottom).contains(&point.y)
}

unsafe extern "system" fn escape(code: i32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if code >= 0 && lp.0 != 0 && matches!(wp.0 as u32, WM_KEYDOWN | WM_SYSKEYDOWN) {
        let input = &*(lp.0 as *const KBDLLHOOKSTRUCT);
        if input.vkCode == u32::from(VK_ESCAPE.0) {
            let (hwnd, token) = TARGET.get();
            dismiss_at(hwnd, token, input.time);
        }
    }
    CallNextHookEx(None, code, wp, lp)
}
