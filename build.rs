//! Embeds Monaged's icon and version info in the Windows executable (the
//! taskbar, Explorer, the tray icon and Windows notifications use them).
//! Needs `rc.exe` from the Windows SDK; without it the build still works,
//! just without an icon.

use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=assets/monaged.ico");
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let Some(rc) = find_rc() else {
        println!("cargo:warning=rc.exe not found (Windows SDK); building without an icon");
        return;
    };
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let icon = manifest.join("assets").join("monaged.ico");
    let version = std::env::var("CARGO_PKG_VERSION").unwrap();
    let parts: Vec<&str> = version.split('.').collect();
    let (major, minor, patch) = (parts.first().unwrap_or(&"0"), parts.get(1).unwrap_or(&"0"), parts.get(2).unwrap_or(&"0"));
    let rc_file = out.join("monaged.rc");
    std::fs::write(
        &rc_file,
        format!(
            r#"1 ICON "{icon}"
1 VERSIONINFO
FILEVERSION {major},{minor},{patch},0
PRODUCTVERSION {major},{minor},{patch},0
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904b0"
    BEGIN
      VALUE "CompanyName", "Monaged"
      VALUE "FileDescription", "Monaged"
      VALUE "FileVersion", "{version}"
      VALUE "InternalName", "monaged"
      VALUE "OriginalFilename", "monaged.exe"
      VALUE "ProductName", "Monaged"
      VALUE "ProductVersion", "{version}"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x409, 1200
  END
END
"#,
            icon = icon.display().to_string().replace('\\', "\\\\"),
        ),
    )
    .unwrap();
    let res = out.join("monaged.res");
    let status = Command::new(&rc).args(["/nologo", "/fo"]).arg(&res).arg(&rc_file).status();
    match status {
        Ok(s) if s.success() => println!("cargo:rustc-link-arg-bins={}", res.display()),
        _ => println!("cargo:warning=rc.exe failed; building without an icon"),
    }
}

/// The newest x64 rc.exe in the Windows SDK.
fn find_rc() -> Option<PathBuf> {
    if let Ok(rc) = std::env::var("RC") {
        return Some(PathBuf::from(rc));
    }
    let kits = Path::new(r"C:\Program Files (x86)\Windows Kits\10\bin");
    let mut versions: Vec<PathBuf> = std::fs::read_dir(kits).ok()?.filter_map(|e| e.ok().map(|e| e.path())).filter(|p| p.join("x64").join("rc.exe").exists()).collect();
    versions.sort();
    versions.pop().map(|p| p.join("x64").join("rc.exe"))
}
