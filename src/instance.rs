//! One Monaged at a time. Several copies would share one database and each
//! run its own agents; starting Monaged again brings the open window forward.
//! A restart after an update passes `--restarted` and waits for the old copy
//! to close first.

/// True when another Monaged is running (its window was brought forward).
#[cfg(windows)]
pub fn already_running(waiting_for_restart: bool) -> bool {
    use std::ffi::c_void;
    use std::time::{Duration, Instant};

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn CreateMutexW(attributes: *const c_void, initial_owner: i32, name: *const u16) -> isize;
        fn GetLastError() -> u32;
        fn CloseHandle(handle: isize) -> i32;
    }
    #[link(name = "user32")]
    unsafe extern "system" {
        fn FindWindowW(class: *const u16, title: *const u16) -> isize;
        fn IsIconic(hwnd: isize) -> i32;
        fn ShowWindow(hwnd: isize, cmd: i32) -> i32;
        fn SetForegroundWindow(hwnd: isize) -> i32;
    }
    const ERROR_ALREADY_EXISTS: u32 = 183;
    const SW_RESTORE: i32 = 9;

    let name: Vec<u16> = "Local\\MonagedApp\0".encode_utf16().collect();
    let deadline = Instant::now() + Duration::from_secs(if waiting_for_restart { 15 } else { 0 });
    loop {
        // SAFETY: plain Win32 calls with a NUL-terminated name; the handle is
        // kept open for the life of the process (that's the lock).
        let handle = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
        let exists = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
        if handle != 0 && !exists {
            return false;
        }
        if handle != 0 {
            unsafe { CloseHandle(handle) };
        }
        if Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    let title: Vec<u16> = "Monaged\0".encode_utf16().collect();
    // SAFETY: as above; a missing window (0) is skipped.
    unsafe {
        let hwnd = FindWindowW(std::ptr::null(), title.as_ptr());
        if hwnd != 0 {
            if IsIconic(hwnd) != 0 {
                ShowWindow(hwnd, SW_RESTORE);
            }
            SetForegroundWindow(hwnd);
        }
    }
    true
}

#[cfg(not(windows))]
pub fn already_running(_: bool) -> bool {
    false
}

/// True while a Monaged window runs (its lock exists). For the uninstaller,
/// which must not remove data under a running copy.
#[cfg(windows)]
pub fn running() -> bool {
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn OpenMutexW(access: u32, inherit: i32, name: *const u16) -> isize;
        fn CloseHandle(handle: isize) -> i32;
    }
    const SYNCHRONIZE: u32 = 0x0010_0000;
    let name: Vec<u16> = "Local\\MonagedApp\0".encode_utf16().collect();
    // SAFETY: a plain Win32 call with a NUL-terminated name; the handle is closed right away.
    let handle = unsafe { OpenMutexW(SYNCHRONIZE, 0, name.as_ptr()) };
    if handle == 0 {
        return false;
    }
    unsafe { CloseHandle(handle) };
    true
}

#[cfg(not(windows))]
pub fn running() -> bool {
    false
}

/// Asks the open Monaged window to close, as its close button does (its
/// agents stop with it). True when there was a window to ask.
#[cfg(windows)]
pub fn close_running() -> bool {
    #[link(name = "user32")]
    unsafe extern "system" {
        fn FindWindowW(class: *const u16, title: *const u16) -> isize;
        fn PostMessageW(hwnd: isize, msg: u32, wparam: usize, lparam: isize) -> i32;
    }
    const WM_CLOSE: u32 = 0x0010;
    // GPUI's window class and the main window's title (the setup windows are titled otherwise).
    let class: Vec<u16> = "Zed::Window\0".encode_utf16().collect();
    let title: Vec<u16> = "Monaged\0".encode_utf16().collect();
    // SAFETY: plain Win32 calls with NUL-terminated strings; a missing window (0) is skipped.
    unsafe {
        let hwnd = FindWindowW(class.as_ptr(), title.as_ptr());
        hwnd != 0 && PostMessageW(hwnd, WM_CLOSE, 0, 0) != 0
    }
}

#[cfg(not(windows))]
pub fn close_running() -> bool {
    false
}
