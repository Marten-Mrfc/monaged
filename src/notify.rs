//! Windows notifications, without registering anything: Monaged keeps a
//! tray icon, and the icon's balloon messages show as Windows notifications
//! (in the notification centre too). Clicking one brings Monaged forward on
//! what it was about. The taskbar button flashes while something waits for you.
//!
//! Plain Win32 through FFI; on other systems everything here does nothing.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};

/// What the last notification was about: (subproject, agent).
static TARGET: Mutex<Option<(i64, Option<i64>)>> = Mutex::new(None);
static CLICKED: AtomicBool = AtomicBool::new(false);
static MAIN_HWND: AtomicIsize = AtomicIsize::new(0);
static TRAY_HWND: AtomicIsize = AtomicIsize::new(0);

/// A notification was clicked: where to go.
pub fn take_click() -> Option<(i64, Option<i64>)> {
    if CLICKED.swap(false, Ordering::Relaxed) { TARGET.lock().ok().and_then(|t| *t) } else { None }
}

#[cfg(windows)]
mod win {
    use super::*;

    const WM_APP_TRAY: u32 = 0x8000 + 1;
    const NIM_ADD: u32 = 0;
    const NIM_MODIFY: u32 = 1;
    const NIM_DELETE: u32 = 2;
    const NIM_SETVERSION: u32 = 4;
    const NIF_MESSAGE: u32 = 0x1;
    const NIF_ICON: u32 = 0x2;
    const NIF_TIP: u32 = 0x4;
    const NIF_INFO: u32 = 0x10;
    const NIF_SHOWTIP: u32 = 0x80;
    const NIIF_INFO: u32 = 0x1;
    const NIIF_RESPECT_QUIET_TIME: u32 = 0x80;
    const NIN_SELECT: u32 = 0x400;
    const NIN_BALLOONUSERCLICK: u32 = 0x405;
    const WM_LBUTTONUP: u32 = 0x202;
    const SW_RESTORE: i32 = 9;
    const FLASHW_TRAY: u32 = 0x2;
    const FLASHW_TIMERNOFG: u32 = 0xC;
    const FLASHW_STOP: u32 = 0;
    const IDI_APPLICATION: usize = 32512;

    #[repr(C)]
    struct NotifyIconData {
        cb_size: u32,
        hwnd: isize,
        id: u32,
        flags: u32,
        callback_message: u32,
        icon: isize,
        tip: [u16; 128],
        state: u32,
        state_mask: u32,
        info: [u16; 256],
        timeout_or_version: u32,
        info_title: [u16; 64],
        info_flags: u32,
        guid: [u8; 16],
        balloon_icon: isize,
    }

    #[repr(C)]
    struct WndClassEx {
        cb_size: u32,
        style: u32,
        wnd_proc: unsafe extern "system" fn(isize, u32, usize, isize) -> isize,
        cls_extra: i32,
        wnd_extra: i32,
        instance: isize,
        icon: isize,
        cursor: isize,
        background: isize,
        menu_name: *const u16,
        class_name: *const u16,
        icon_small: isize,
    }

    #[repr(C)]
    struct FlashInfo {
        cb_size: u32,
        hwnd: isize,
        flags: u32,
        count: u32,
        timeout: u32,
    }

    #[link(name = "user32")]
    unsafe extern "system" {
        fn RegisterClassExW(class: *const WndClassEx) -> u16;
        fn CreateWindowExW(
            ex_style: u32,
            class: *const u16,
            name: *const u16,
            style: u32,
            x: i32,
            y: i32,
            w: i32,
            h: i32,
            parent: isize,
            menu: isize,
            instance: isize,
            param: *mut core::ffi::c_void,
        ) -> isize;
        fn DefWindowProcW(hwnd: isize, msg: u32, wparam: usize, lparam: isize) -> isize;
        fn LoadIconW(instance: isize, name: usize) -> isize;
        fn SetForegroundWindow(hwnd: isize) -> i32;
        fn ShowWindow(hwnd: isize, cmd: i32) -> i32;
        fn IsIconic(hwnd: isize) -> i32;
        fn FlashWindowEx(info: *const FlashInfo) -> i32;
        fn EnumThreadWindows(thread: u32, callback: unsafe extern "system" fn(isize, isize) -> i32, lparam: isize) -> i32;
        fn GetWindowTextW(hwnd: isize, text: *mut u16, max: i32) -> i32;
        fn IsWindowVisible(hwnd: isize) -> i32;
        fn RegisterWindowMessageW(name: *const u16) -> u32;
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetModuleHandleW(name: *const u16) -> isize;
        fn GetCurrentThreadId() -> u32;
    }
    #[link(name = "shell32")]
    unsafe extern "system" {
        fn Shell_NotifyIconW(message: u32, data: *mut NotifyIconData) -> i32;
    }

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn copy_into(dst: &mut [u16], text: &str) {
        let mut units: Vec<u16> = text.encode_utf16().take(dst.len() - 1).collect();
        units.push(0);
        dst[..units.len()].copy_from_slice(&units);
    }

    static TASKBAR_CREATED: AtomicIsize = AtomicIsize::new(0);

    unsafe extern "system" fn wnd_proc(hwnd: isize, msg: u32, wparam: usize, lparam: isize) -> isize {
        if msg == WM_APP_TRAY {
            let event = (lparam as u32) & 0xffff;
            if matches!(event, NIN_BALLOONUSERCLICK | NIN_SELECT | WM_LBUTTONUP) {
                CLICKED.store(true, Ordering::Relaxed);
                bring_forward();
            }
            return 0;
        }
        // Explorer restarted: the tray is new, so add the icon again.
        if msg as isize == TASKBAR_CREATED.load(Ordering::Relaxed) && msg != 0 {
            add_icon(hwnd);
            return 0;
        }
        unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
    }

    fn icon_data(hwnd: isize) -> NotifyIconData {
        NotifyIconData {
            cb_size: std::mem::size_of::<NotifyIconData>() as u32,
            hwnd,
            id: 1,
            flags: 0,
            callback_message: WM_APP_TRAY,
            icon: 0,
            tip: [0; 128],
            state: 0,
            state_mask: 0,
            info: [0; 256],
            timeout_or_version: 0,
            info_title: [0; 64],
            info_flags: 0,
            guid: [0; 16],
            balloon_icon: 0,
        }
    }

    fn add_icon(hwnd: isize) {
        unsafe {
            let instance = GetModuleHandleW(std::ptr::null());
            // The exe's own icon when it has one, else Windows' default.
            let mut icon = LoadIconW(instance, 1);
            if icon == 0 {
                icon = LoadIconW(0, IDI_APPLICATION);
            }
            let mut data = icon_data(hwnd);
            data.flags = NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_SHOWTIP;
            data.icon = icon;
            copy_into(&mut data.tip, "Monaged");
            Shell_NotifyIconW(NIM_ADD, &mut data);
            data.timeout_or_version = 4;
            Shell_NotifyIconW(NIM_SETVERSION, &mut data);
        }
    }

    /// Creates the hidden window and the tray icon (once, on the UI thread).
    pub fn init() {
        if TRAY_HWND.load(Ordering::Relaxed) != 0 {
            return;
        }
        unsafe {
            let instance = GetModuleHandleW(std::ptr::null());
            let class_name = wide("MonagedTray");
            let class = WndClassEx {
                cb_size: std::mem::size_of::<WndClassEx>() as u32,
                style: 0,
                wnd_proc,
                cls_extra: 0,
                wnd_extra: 0,
                instance,
                icon: 0,
                cursor: 0,
                background: 0,
                menu_name: std::ptr::null(),
                class_name: class_name.as_ptr(),
                icon_small: 0,
            };
            RegisterClassExW(&class);
            let title = wide("Monaged tray");
            // WS_POPUP, never shown.
            let hwnd = CreateWindowExW(0, class_name.as_ptr(), title.as_ptr(), 0x8000_0000, 0, 0, 0, 0, 0, 0, instance, std::ptr::null_mut());
            if hwnd == 0 {
                return;
            }
            TASKBAR_CREATED.store(RegisterWindowMessageW(wide("TaskbarCreated").as_ptr()) as isize, Ordering::Relaxed);
            TRAY_HWND.store(hwnd, Ordering::Relaxed);
            add_icon(hwnd);
        }
    }

    pub fn shutdown() {
        let hwnd = TRAY_HWND.swap(0, Ordering::Relaxed);
        if hwnd != 0 {
            let mut data = icon_data(hwnd);
            unsafe { Shell_NotifyIconW(NIM_DELETE, &mut data) };
        }
    }

    pub fn show(title: &str, body: &str) {
        let hwnd = TRAY_HWND.load(Ordering::Relaxed);
        if hwnd == 0 {
            return;
        }
        let mut data = icon_data(hwnd);
        data.flags = NIF_INFO;
        data.info_flags = NIIF_INFO | NIIF_RESPECT_QUIET_TIME;
        copy_into(&mut data.info_title, title);
        copy_into(&mut data.info, body);
        unsafe { Shell_NotifyIconW(NIM_MODIFY, &mut data) };
    }

    pub fn set_tip(text: &str) {
        let hwnd = TRAY_HWND.load(Ordering::Relaxed);
        if hwnd == 0 {
            return;
        }
        let mut data = icon_data(hwnd);
        data.flags = NIF_TIP | NIF_SHOWTIP;
        copy_into(&mut data.tip, text);
        unsafe { Shell_NotifyIconW(NIM_MODIFY, &mut data) };
    }

    /// Monaged's main window (the visible one titled "Monaged" on this thread).
    fn main_window() -> isize {
        let known = MAIN_HWND.load(Ordering::Relaxed);
        if known != 0 {
            return known;
        }
        unsafe extern "system" fn each(hwnd: isize, _: isize) -> i32 {
            let mut buf = [0u16; 32];
            let len = unsafe { GetWindowTextW(hwnd, buf.as_mut_ptr(), 32) };
            if len > 0 && String::from_utf16_lossy(&buf[..len as usize]) == "Monaged" && unsafe { IsWindowVisible(hwnd) } != 0 {
                MAIN_HWND.store(hwnd, Ordering::Relaxed);
                return 0;
            }
            1
        }
        unsafe { EnumThreadWindows(GetCurrentThreadId(), each, 0) };
        MAIN_HWND.load(Ordering::Relaxed)
    }

    pub fn bring_forward() {
        let hwnd = main_window();
        if hwnd != 0 {
            unsafe {
                if IsIconic(hwnd) != 0 {
                    ShowWindow(hwnd, SW_RESTORE);
                }
                SetForegroundWindow(hwnd);
            }
        }
    }

    pub fn flash(on: bool) {
        let hwnd = main_window();
        if hwnd == 0 {
            return;
        }
        let info = FlashInfo {
            cb_size: std::mem::size_of::<FlashInfo>() as u32,
            hwnd,
            flags: if on { FLASHW_TRAY | FLASHW_TIMERNOFG } else { FLASHW_STOP },
            count: 0,
            timeout: 0,
        };
        unsafe { FlashWindowEx(&info) };
    }
}

#[cfg(not(windows))]
mod win {
    pub fn init() {}
    pub fn shutdown() {}
    pub fn show(_: &str, _: &str) {}
    pub fn set_tip(_: &str) {}
    pub fn flash(_: bool) {}
}

pub use win::{flash, init, set_tip, shutdown};

/// Shows a Windows notification about a subproject (and agent).
pub fn send(title: &str, body: &str, target: (i64, Option<i64>)) {
    if let Ok(mut t) = TARGET.lock() {
        *t = Some(target);
    }
    win::show(title, body);
}
