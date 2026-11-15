//! Windows' own APIs for things Monaged used to ask helper programs to do:
//! PowerShell for shortcuts and for running as administrator, cmd.exe for
//! removing the program folder, reg.exe for the registry.
//!
//! Endpoint security rightly distrusts an app that starts script hosts and
//! shells (Sophos' "Lockdown" stops it and reports the app as malicious), so
//! Monaged starts none of them: only wsl.exe, git and the tools you asked for.

use std::path::Path;

use anyhow::Result;

#[derive(Clone, Copy, Debug)]
pub enum Hive {
    CurrentUser,
    LocalMachine,
}

/// Where WSL lists the distros registered for this user (under HKCU).
pub const LXSS: &str = r"Software\Microsoft\Windows\CurrentVersion\Lxss";

/// One distro WSL has registered for this user (`HKCU\…\Lxss\{guid}`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Distro {
    /// Its key, relative to HKCU.
    pub key: String,
    pub name: String,
    /// Where its disk is, as stored (may start with `\\?\` or hold %VARIABLES%).
    pub base: Option<String>,
    pub default_uid: Option<u32>,
}

/// Distros registered for this user, read from the registry (no wsl.exe).
pub fn wsl_distros() -> Vec<Distro> {
    reg_subkeys(Hive::CurrentUser, LXSS)
        .into_iter()
        .filter_map(|sub| {
            let key = format!(r"{LXSS}\{sub}");
            let name = reg_string(Hive::CurrentUser, &key, "DistributionName")?;
            Some(Distro { base: reg_string(Hive::CurrentUser, &key, "BasePath"), default_uid: reg_dword(Hive::CurrentUser, &key, "DefaultUid"), name, key })
        })
        .collect()
}

#[cfg(windows)]
mod imp {
    use std::path::Path;

    use anyhow::{Result, bail};
    use windows::Win32::Foundation::{CloseHandle, ERROR_CANCELLED, ERROR_FILE_NOT_FOUND, ERROR_SUCCESS, WIN32_ERROR};
    use windows::Win32::System::Registry::{
        HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, KEY_SET_VALUE, REG_DWORD, REG_OPTION_NON_VOLATILE, REG_ROUTINE_FLAGS, REG_SZ, RRF_NOEXPAND, RRF_RT_REG_DWORD,
        RRF_RT_REG_EXPAND_SZ, RRF_RT_REG_SZ, RegCloseKey, RegCreateKeyExW, RegDeleteKeyW, RegDeleteTreeW, RegDeleteValueW, RegEnumKeyExW, RegGetValueW, RegOpenKeyExW,
        RegSetValueExW,
    };
    use windows::core::{HSTRING, Interface, PCWSTR, PWSTR};

    use super::Hive;

    fn root(hive: Hive) -> HKEY {
        match hive {
            Hive::CurrentUser => HKEY_CURRENT_USER,
            Hive::LocalMachine => HKEY_LOCAL_MACHINE,
        }
    }

    fn check(result: WIN32_ERROR, what: &str) -> Result<()> {
        if result != ERROR_SUCCESS {
            bail!("{what} failed (Windows error {})", result.0);
        }
        Ok(())
    }

    pub fn reg_key_exists(hive: Hive, path: &str) -> bool {
        let mut key = HKEY::default();
        // SAFETY: plain registry calls; the key is closed right away.
        unsafe {
            let ok = RegOpenKeyExW(root(hive), &HSTRING::from(path), None, KEY_READ, &mut key) == ERROR_SUCCESS;
            if ok {
                let _ = RegCloseKey(key);
            }
            ok
        }
    }

    /// A REG_SZ or REG_EXPAND_SZ value as stored (%VARIABLES% not expanded).
    pub fn reg_string(hive: Hive, path: &str, name: &str) -> Option<String> {
        let (path, name) = (HSTRING::from(path), HSTRING::from(name));
        let flags = REG_ROUTINE_FLAGS(RRF_RT_REG_SZ.0 | RRF_RT_REG_EXPAND_SZ.0 | RRF_NOEXPAND.0);
        let mut size = 0u32;
        // SAFETY: the first call only asks for the size; the second writes at
        // most `size` bytes into a buffer at least that big.
        unsafe {
            if RegGetValueW(root(hive), &path, &name, flags, None, None, Some(&mut size)) != ERROR_SUCCESS {
                return None;
            }
            let mut buf = vec![0u16; (size as usize).div_ceil(2) + 1];
            let mut size = (buf.len() * 2) as u32;
            if RegGetValueW(root(hive), &path, &name, flags, None, Some(buf.as_mut_ptr().cast()), Some(&mut size)) != ERROR_SUCCESS {
                return None;
            }
            let len = buf.iter().position(|c| *c == 0).unwrap_or(buf.len());
            Some(String::from_utf16_lossy(&buf[..len]))
        }
    }

    pub fn reg_dword(hive: Hive, path: &str, name: &str) -> Option<u32> {
        let mut value = 0u32;
        let mut size = 4u32;
        // SAFETY: a DWORD read into a u32.
        let result = unsafe { RegGetValueW(root(hive), &HSTRING::from(path), &HSTRING::from(name), RRF_RT_REG_DWORD, None, Some((&mut value as *mut u32).cast()), Some(&mut size)) };
        (result == ERROR_SUCCESS).then_some(value)
    }

    pub fn reg_subkeys(hive: Hive, path: &str) -> Vec<String> {
        let mut key = HKEY::default();
        let mut out = Vec::new();
        // SAFETY: enumerating an opened key into a fixed buffer (registry key
        // names are at most 255 characters); the key is closed afterwards.
        unsafe {
            if RegOpenKeyExW(root(hive), &HSTRING::from(path), None, KEY_READ, &mut key) != ERROR_SUCCESS {
                return out;
            }
            for index in 0.. {
                let mut name = [0u16; 256];
                let mut len = name.len() as u32;
                if RegEnumKeyExW(key, index, Some(PWSTR(name.as_mut_ptr())), &mut len, None, None, None, None) != ERROR_SUCCESS {
                    break;
                }
                out.push(String::from_utf16_lossy(&name[..len as usize]));
            }
            let _ = RegCloseKey(key);
        }
        out
    }

    fn set(hive: Hive, path: &str, name: &str, kind: windows::Win32::System::Registry::REG_VALUE_TYPE, data: &[u8]) -> Result<()> {
        let mut key = HKEY::default();
        // SAFETY: creating (or opening) a key for writing, one value, closing it.
        unsafe {
            check(RegCreateKeyExW(root(hive), &HSTRING::from(path), None, PCWSTR::null(), REG_OPTION_NON_VOLATILE, KEY_SET_VALUE, None, &mut key, None), "Opening a registry key")?;
            let result = RegSetValueExW(key, &HSTRING::from(name), None, kind, Some(data));
            let _ = RegCloseKey(key);
            check(result, "Writing to the registry")
        }
    }

    pub fn reg_set_string(hive: Hive, path: &str, name: &str, value: &str) -> Result<()> {
        let data: Vec<u8> = value.encode_utf16().chain(std::iter::once(0)).flat_map(u16::to_le_bytes).collect();
        set(hive, path, name, REG_SZ, &data)
    }

    pub fn reg_set_dword(hive: Hive, path: &str, name: &str, value: u32) -> Result<()> {
        set(hive, path, name, REG_DWORD, &value.to_le_bytes())
    }

    /// Removes a value; a value or key that isn't there counts as removed.
    pub fn reg_delete_value(hive: Hive, path: &str, name: &str) -> Result<()> {
        let mut key = HKEY::default();
        // SAFETY: opening a key for writing, deleting one value, closing it.
        unsafe {
            let opened = RegOpenKeyExW(root(hive), &HSTRING::from(path), None, KEY_SET_VALUE, &mut key);
            if opened == ERROR_FILE_NOT_FOUND {
                return Ok(());
            }
            check(opened, "Opening a registry key")?;
            let result = RegDeleteValueW(key, &HSTRING::from(name));
            let _ = RegCloseKey(key);
            if result == ERROR_FILE_NOT_FOUND {
                return Ok(());
            }
            check(result, "Removing a registry value")
        }
    }

    /// Removes a key with everything in it; a key that isn't there counts as removed.
    pub fn reg_delete_tree(hive: Hive, path: &str) -> Result<()> {
        let path = HSTRING::from(path);
        // SAFETY: plain registry calls on a path under the given hive.
        unsafe {
            let result = RegDeleteTreeW(root(hive), &path);
            if result != ERROR_SUCCESS && result != ERROR_FILE_NOT_FOUND {
                return check(result, "Removing a registry key");
            }
            let result = RegDeleteKeyW(root(hive), &path);
            if result == ERROR_FILE_NOT_FOUND {
                return Ok(());
            }
            check(result, "Removing a registry key")
        }
    }

    /// A .lnk shortcut, through the Shell's own ShellLink object.
    pub fn create_shortcut(link: &Path, target: &Path, args: &str, description: &str) -> Result<()> {
        use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize, IPersistFile};
        use windows::Win32::UI::Shell::{IShellLinkW, ShellLink};
        let text = |p: &Path| HSTRING::from(p.as_os_str());
        // SAFETY: COM is set up for this thread (and torn down if this call
        // set it up); the interfaces are released when they go out of scope.
        unsafe {
            let init = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            let result = (|| -> windows::core::Result<()> {
                let shell: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
                shell.SetPath(&text(target))?;
                shell.SetArguments(&HSTRING::from(args))?;
                if let Some(dir) = target.parent() {
                    shell.SetWorkingDirectory(&text(dir))?;
                }
                shell.SetIconLocation(&text(target), 0)?;
                shell.SetDescription(&HSTRING::from(description))?;
                let file: IPersistFile = shell.cast()?;
                file.Save(&text(link), true)
            })();
            if init.is_ok() {
                CoUninitialize();
            }
            result.map_err(|e| anyhow::anyhow!("couldn't make the shortcut {}: {e}", link.display()))
        }
    }

    /// Runs `exe` with `params` as administrator (Windows asks first),
    /// without a window, and waits for it. Its exit code; 1223 when the
    /// prompt was declined.
    pub fn run_elevated(exe: &Path, params: &str) -> Result<u32> {
        use windows::Win32::System::Threading::{GetExitCodeProcess, INFINITE, WaitForSingleObject};
        use windows::Win32::UI::Shell::{SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW};
        let (verb, file, params) = (HSTRING::from("runas"), HSTRING::from(exe.as_os_str()), HSTRING::from(params));
        let mut info = SHELLEXECUTEINFOW {
            cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
            fMask: SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC,
            lpVerb: PCWSTR(verb.as_ptr()),
            lpFile: PCWSTR(file.as_ptr()),
            lpParameters: PCWSTR(params.as_ptr()),
            nShow: 0, // SW_HIDE
            ..Default::default()
        };
        // SAFETY: `info` and the strings it points to outlive the call; the
        // process handle it returns is waited on and closed here.
        unsafe {
            if let Err(err) = ShellExecuteExW(&mut info) {
                if err.code() == ERROR_CANCELLED.to_hresult() {
                    return Ok(1223);
                }
                bail!("Windows couldn't start it as administrator: {}", err.message());
            }
            if info.hProcess.is_invalid() {
                bail!("Windows started it as administrator but didn't say how it went");
            }
            WaitForSingleObject(info.hProcess, INFINITE);
            let mut code = 1u32;
            let got = GetExitCodeProcess(info.hProcess, &mut code);
            let _ = CloseHandle(info.hProcess);
            got.map_err(|e| anyhow::anyhow!("couldn't read how it went: {}", e.message()))?;
            Ok(code)
        }
    }

    /// The user's Downloads folder (wherever they moved it).
    pub fn downloads_dir() -> Option<std::path::PathBuf> {
        use windows::Win32::System::Com::CoTaskMemFree;
        use windows::Win32::UI::Shell::{FOLDERID_Downloads, KF_FLAG_DEFAULT, SHGetKnownFolderPath};
        // SAFETY: the returned string is copied, then freed with CoTaskMemFree as documented.
        unsafe {
            let path = SHGetKnownFolderPath(&FOLDERID_Downloads, KF_FLAG_DEFAULT, None).ok()?;
            let text = path.to_string().ok();
            CoTaskMemFree(Some(path.0 as *const _));
            text.map(std::path::PathBuf::from)
        }
    }

    /// Monaged's biggest visible window, drawn into a bitmap (PrintWindow with
    /// full content, so GPU-drawn content is included): (width, height, RGBA).
    pub fn capture_own_window() -> Result<(u32, u32, Vec<u8>)> {
        use windows::Win32::Foundation::{HWND, LPARAM, RECT};
        use windows::Win32::Graphics::Gdi::{
            BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleDC, CreateDIBSection, DIB_RGB_COLORS, DeleteDC, DeleteObject, GetDC, ReleaseDC, SelectObject,
        };
        use windows::Win32::Storage::Xps::{PRINT_WINDOW_FLAGS, PrintWindow};
        use windows::Win32::UI::WindowsAndMessaging::{EnumWindows, GetClientRect, GetWindowThreadProcessId, IsIconic, IsWindowVisible};
        use windows::core::BOOL;

        struct Found {
            pid: u32,
            best: Option<(HWND, i64)>,
        }
        unsafe extern "system" fn each(hwnd: HWND, data: LPARAM) -> BOOL {
            // SAFETY: `data` is the &mut Found passed to EnumWindows below, alive for the call.
            let found = unsafe { &mut *(data.0 as *mut Found) };
            let mut pid = 0u32;
            unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
            if pid == found.pid && unsafe { IsWindowVisible(hwnd) }.as_bool() && !unsafe { IsIconic(hwnd) }.as_bool() {
                let mut rect = RECT::default();
                if unsafe { GetClientRect(hwnd, &mut rect) }.is_ok() {
                    let area = i64::from(rect.right - rect.left) * i64::from(rect.bottom - rect.top);
                    if found.best.is_none_or(|(_, a)| area > a) {
                        found.best = Some((hwnd, area));
                    }
                }
            }
            BOOL(1)
        }
        let mut found = Found { pid: std::process::id(), best: None };
        // SAFETY: the callback only reads window properties and writes `found`, which outlives the call.
        unsafe {
            let _ = EnumWindows(Some(each), LPARAM(&mut found as *mut Found as isize));
        }
        let Some((hwnd, _)) = found.best else { bail!("Monaged's window isn't open (or it's minimized)") };
        // SAFETY: every GDI object made here is selected out and deleted before returning.
        unsafe {
            let mut rect = RECT::default();
            GetClientRect(hwnd, &mut rect)?;
            let (width, height) = ((rect.right - rect.left).max(1), (rect.bottom - rect.top).max(1));
            let screen = GetDC(None);
            let dc = CreateCompatibleDC(Some(screen));
            let info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: width,
                    biHeight: -height, // top-down rows
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
            let bitmap = CreateDIBSection(Some(dc), &info, DIB_RGB_COLORS, &mut bits, None, 0);
            let result = (|| -> Result<Vec<u8>> {
                let bitmap = bitmap.map_err(|e| anyhow::anyhow!("couldn't make a bitmap: {}", e.message()))?;
                let old = SelectObject(dc, bitmap.into());
                // PW_CLIENTONLY | PW_RENDERFULLCONTENT
                let ok = PrintWindow(hwnd, dc, PRINT_WINDOW_FLAGS(1 | 2)).as_bool();
                SelectObject(dc, old);
                let pixels = if ok && !bits.is_null() {
                    let len = (width as usize) * (height as usize) * 4;
                    let bgra = std::slice::from_raw_parts(bits as *const u8, len);
                    let mut rgba = Vec::with_capacity(len);
                    for px in bgra.chunks_exact(4) {
                        rgba.extend_from_slice(&[px[2], px[1], px[0], 255]);
                    }
                    Some(rgba)
                } else {
                    None
                };
                let _ = DeleteObject(bitmap.into());
                pixels.ok_or_else(|| anyhow::anyhow!("Windows couldn't draw the window"))
            })();
            let _ = DeleteDC(dc);
            ReleaseDC(None, screen);
            Ok((width as u32, height as u32, result?))
        }
    }

    /// Opens Explorer with the file selected.
    pub fn reveal_in_explorer(path: &Path) -> Result<()> {
        use std::os::windows::process::CommandExt;
        let windows_dir = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
        std::process::Command::new(Path::new(&windows_dir).join("explorer.exe"))
            .raw_arg(format!("/select,\"{}\"", path.display()))
            .spawn()
            .map(|_| ())
            .map_err(|e| anyhow::anyhow!("couldn't open Explorer: {e}"))
    }
}

#[cfg(not(windows))]
mod imp {
    use std::path::Path;

    use anyhow::{Result, bail};

    use super::Hive;

    pub fn reg_key_exists(_: Hive, _: &str) -> bool {
        false
    }
    pub fn reg_string(_: Hive, _: &str, _: &str) -> Option<String> {
        None
    }
    pub fn reg_dword(_: Hive, _: &str, _: &str) -> Option<u32> {
        None
    }
    pub fn reg_subkeys(_: Hive, _: &str) -> Vec<String> {
        Vec::new()
    }
    pub fn reg_set_string(_: Hive, _: &str, _: &str, _: &str) -> Result<()> {
        Ok(())
    }
    pub fn reg_set_dword(_: Hive, _: &str, _: &str, _: u32) -> Result<()> {
        Ok(())
    }
    pub fn reg_delete_value(_: Hive, _: &str, _: &str) -> Result<()> {
        Ok(())
    }
    pub fn reg_delete_tree(_: Hive, _: &str) -> Result<()> {
        Ok(())
    }
    pub fn create_shortcut(_: &Path, _: &Path, _: &str, _: &str) -> Result<()> {
        bail!("shortcuts are made on Windows only")
    }
    pub fn run_elevated(_: &Path, _: &str) -> Result<u32> {
        bail!("running as administrator works on Windows only")
    }

    pub fn downloads_dir() -> Option<std::path::PathBuf> {
        std::env::var("HOME").ok().map(|h| std::path::PathBuf::from(h).join("Downloads"))
    }

    pub fn capture_own_window() -> Result<(u32, u32, Vec<u8>)> {
        bail!("screenshots are only taken on Windows")
    }

    pub fn reveal_in_explorer(_: &Path) -> Result<()> {
        Ok(())
    }
}

pub use imp::*;

/// Quotes one argument for a Windows command line (the rules programs built
/// on the C runtime, Rust's included, read it back with).
pub fn quote_arg(arg: &str) -> String {
    if !arg.is_empty() && !arg.contains([' ', '\t', '"']) {
        return arg.to_string();
    }
    let mut out = String::from("\"");
    let mut backslashes = 0;
    for c in arg.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                out.push_str(&"\\".repeat(backslashes * 2 + 1));
                out.push('"');
                backslashes = 0;
            }
            _ => {
                out.push_str(&"\\".repeat(backslashes));
                out.push(c);
                backslashes = 0;
            }
        }
    }
    out.push_str(&"\\".repeat(backslashes * 2));
    out.push('"');
    out
}

/// Moves the running program out of `dir` (Windows lets a running exe be
/// renamed, not deleted) into %TEMP%, so `dir` can be deleted right away
/// without a helper process waiting for this one to exit. Returns where it went.
pub fn move_running_exe_out(dir: &Path) -> Result<std::path::PathBuf> {
    use anyhow::Context as _;
    let exe = std::env::current_exe().context("couldn't find the running program")?;
    if !exe.starts_with(dir) {
        return Ok(exe);
    }
    let to = std::env::temp_dir().join(format!("monaged-removed-{}.exe", std::process::id()));
    std::fs::rename(&exe, &to).with_context(|| format!("couldn't move {} out of the way", exe.display()))?;
    Ok(to)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_arguments_like_the_c_runtime_reads_them() {
        assert_eq!(quote_arg("plain"), "plain");
        assert_eq!(quote_arg(r"C:\Users\Sam Rivera\log.txt"), r#""C:\Users\Sam Rivera\log.txt""#);
        assert_eq!(quote_arg(r"C:\dir with space\"), r#""C:\dir with space\\""#);
        assert_eq!(quote_arg(r#"say "hi""#), r#""say \"hi\"""#);
        assert_eq!(quote_arg(""), r#""""#);
    }

    #[cfg(windows)]
    #[test]
    fn the_registry_round_trips() {
        let key = format!(r"Software\MonagedTest\{}", std::process::id());
        let sub = format!(r"{key}\Child");
        reg_set_string(Hive::CurrentUser, &key, "Name", "Ubuntu Ü").unwrap();
        reg_set_dword(Hive::CurrentUser, &key, "Uid", 1000).unwrap();
        reg_set_string(Hive::CurrentUser, &sub, "Empty", "").unwrap();
        assert!(reg_key_exists(Hive::CurrentUser, &key));
        assert_eq!(reg_string(Hive::CurrentUser, &key, "Name").as_deref(), Some("Ubuntu Ü"));
        assert_eq!(reg_dword(Hive::CurrentUser, &key, "Uid"), Some(1000));
        assert_eq!(reg_string(Hive::CurrentUser, &sub, "Empty").as_deref(), Some(""));
        assert_eq!(reg_string(Hive::CurrentUser, &key, "Missing"), None);
        assert_eq!(reg_subkeys(Hive::CurrentUser, &key), vec!["Child".to_string()]);
        reg_delete_value(Hive::CurrentUser, &key, "Name").unwrap();
        reg_delete_value(Hive::CurrentUser, &key, "Name").unwrap();
        assert_eq!(reg_string(Hive::CurrentUser, &key, "Name"), None);
        reg_delete_tree(Hive::CurrentUser, &key).unwrap();
        assert!(!reg_key_exists(Hive::CurrentUser, &key));
        reg_delete_tree(Hive::CurrentUser, &key).unwrap();
        let _ = reg_delete_tree(Hive::CurrentUser, r"Software\MonagedTest");
    }

    #[cfg(windows)]
    #[test]
    fn makes_a_shortcut() {
        let dir = std::env::temp_dir().join(format!("monaged-link-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let link = dir.join("Monaged test.lnk");
        create_shortcut(&link, &std::env::current_exe().unwrap(), "--setup", "A test shortcut").unwrap();
        assert!(std::fs::metadata(&link).unwrap().len() > 0);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_program_outside_the_folder_stays_where_it_is() {
        let here = std::env::current_exe().unwrap();
        let elsewhere = Path::new(if cfg!(windows) { r"C:\definitely\not\here" } else { "/definitely/not/here" });
        assert_eq!(move_running_exe_out(elsewhere).unwrap(), here);
    }
}
