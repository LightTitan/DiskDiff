//! Windows: embeds the app icon and version details into diskdiff.exe.
//!
//! GPUI loads icon resource 1 of the executable for its windows and taskbar
//! button, and Explorer shows the same icon for the file. Other platforms
//! need nothing here (macOS takes the icon from the bundle, Linux from the
//! .desktop file).

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=icon/DiskDiff.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        windows_resources();
    }
}

#[cfg(windows)]
fn windows_resources() {
    use std::path::PathBuf;
    let dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let icon = dir.join("icon").join("DiskDiff.ico");
    let version = std::env::var("CARGO_PKG_VERSION").unwrap();
    let mut parts: Vec<u32> = version.split(['.', '-']).filter_map(|p| p.parse().ok()).collect();
    parts.resize(4, 0);
    let comma = parts.iter().map(u32::to_string).collect::<Vec<_>>().join(",");
    // Written to OUT_DIR so the icon can be referenced by absolute path.
    let rc = format!(
        r#"1 ICON "{icon}"

1 VERSIONINFO
FILEVERSION {comma}
PRODUCTVERSION {comma}
FILEOS 0x40004
FILETYPE 0x1
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904B0"
    BEGIN
      VALUE "CompanyName", "DiskDiff"
      VALUE "FileDescription", "DiskDiff"
      VALUE "FileVersion", "{version}"
      VALUE "InternalName", "diskdiff"
      VALUE "OriginalFilename", "diskdiff.exe"
      VALUE "ProductName", "DiskDiff"
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
    );
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("diskdiff.rc");
    std::fs::write(&out, rc).unwrap();
    embed_resource::compile(&out, embed_resource::NONE).manifest_optional().unwrap();
}

/// Cross-building for Windows from another OS: no resource compiler to rely
/// on, so the executable goes without an icon (release builds run on Windows).
#[cfg(not(windows))]
fn windows_resources() {
    println!("cargo:warning=building for Windows from another OS: diskdiff.exe gets no icon");
}
