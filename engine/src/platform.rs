//! Platform facts the scanner needs: volume ids, free space, what to skip.

use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, Default)]
pub struct DiskSpace {
    pub total: u64,
    pub free: u64,
}

impl DiskSpace {
    pub fn used(&self) -> u64 {
        self.total.saturating_sub(self.free)
    }
}

/// How to walk a given root.
#[derive(Clone, Debug, Default)]
pub struct Plan {
    /// Volumes besides the root's own that the walk may enter.
    pub extra_devs: Vec<u64>,
    /// Absolute paths not to descend into.
    pub skip: Vec<PathBuf>,
}

/// Packages macOS shows as single files.
pub fn is_package(name: &str) -> bool {
    const EXT: &[&str] = &[
        "app", "bundle", "framework", "photoslibrary", "musiclibrary", "tvlibrary", "plugin", "kext",
        "xcarchive", "pkg", "rtfd", "playground", "xcodeproj", "xcworkspace", "imovielibrary", "fcpbundle",
        "logicx", "band", "sparsebundle", "photolibrary", "aplibrary", "theater", "appex", "docset", "qlgenerator",
    ];
    match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => EXT.iter().any(|e| e.eq_ignore_ascii_case(ext)),
        _ => false,
    }
}

#[cfg(unix)]
pub fn dev_of(path: &Path) -> u64 {
    use std::os::unix::fs::MetadataExt;
    std::fs::symlink_metadata(path).map(|m| m.dev()).unwrap_or(0)
}

#[cfg(not(unix))]
pub fn dev_of(_: &Path) -> u64 {
    0
}

#[cfg(unix)]
pub fn disk_space(path: &Path) -> Option<DiskSpace> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let c = CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut s: libc::statfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statfs(c.as_ptr(), &mut s) } != 0 {
        return None;
    }
    let bs = s.f_bsize as u64;
    Some(DiskSpace { total: s.f_blocks as u64 * bs, free: s.f_bavail as u64 * bs })
}

#[cfg(windows)]
pub fn disk_space(path: &Path) -> Option<DiskSpace> {
    use std::os::windows::ffi::OsStrExt;
    unsafe extern "system" {
        fn GetDiskFreeSpaceExW(dir: *const u16, avail: *mut u64, total: *mut u64, free: *mut u64) -> i32;
    }
    let w: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let (mut avail, mut total, mut free) = (0u64, 0u64, 0u64);
    if unsafe { GetDiskFreeSpaceExW(w.as_ptr(), &mut avail, &mut total, &mut free) } == 0 {
        return None;
    }
    Some(DiskSpace { total, free: avail })
}

#[cfg(target_os = "macos")]
pub fn plan(root: &Path) -> Plan {
    // On macOS the root volume is the sealed system volume; user data lives on
    // the Data volume, firmlinked into place (/Users, /Applications, /private…).
    // Walking "/" therefore has to enter the Data volume through those
    // firmlinks, but must not walk /System/Volumes/Data itself as well, or
    // everything would be counted twice.
    let data = Path::new("/System/Volumes/Data");
    let mut p = Plan::default();
    if root == Path::new("/") {
        p.extra_devs.push(dev_of(data));
        p.skip.push(data.to_owned());
        // Virtual or regenerated trees that are not "used space" a user can act on.
        p.skip.push("/dev".into());
        p.skip.push("/System/Volumes/VM".into());
        p.skip.push("/System/Volumes/Preboot".into());
        p.skip.push("/System/Volumes/Update".into());
        p.skip.push("/System/Volumes/xarts".into());
        p.skip.push("/System/Volumes/iSCPreboot".into());
        p.skip.push("/System/Volumes/Hardware".into());
        p.skip.push("/Volumes".into());
    } else if root.starts_with(data) || root.starts_with("/Users") {
        p.extra_devs.push(dev_of(data));
    }
    p
}

#[cfg(not(target_os = "macos"))]
pub fn plan(root: &Path) -> Plan {
    let mut p = Plan::default();
    if root == Path::new("/") {
        for s in ["/proc", "/sys", "/dev", "/run"] {
            p.skip.push(s.into());
        }
    }
    p
}

/// macOS: tells the kernel never to materialise "dataless" (evicted iCloud /
/// File Provider) files or folders for this process. Touching one then fails
/// instead of silently downloading from the network. Process-wide and
/// permanent, which is what a disk analyzer wants.
#[cfg(target_os = "macos")]
pub fn never_fetch_cloud_files() {
    unsafe extern "C" {
        fn setiopolicy_np(iotype: i32, scope: i32, policy: i32) -> i32;
    }
    const IOPOL_TYPE_VFS_MATERIALIZE_DATALESS_FILES: i32 = 3;
    const IOPOL_SCOPE_PROCESS: i32 = 0;
    const IOPOL_MATERIALIZE_DATALESS_FILES_OFF: i32 = 1;
    unsafe {
        setiopolicy_np(IOPOL_TYPE_VFS_MATERIALIZE_DATALESS_FILES, IOPOL_SCOPE_PROCESS, IOPOL_MATERIALIZE_DATALESS_FILES_OFF);
    }
}

#[cfg(not(target_os = "macos"))]
pub fn never_fetch_cloud_files() {}

/// macOS: whether this process has Full Disk Access. Probed by reading a file
/// only readable with it. Elsewhere: true.
#[cfg(target_os = "macos")]
pub fn has_full_access() -> bool {
    let Some(home) = std::env::var_os("HOME") else { return true };
    let probe = Path::new(&home).join("Library/Application Support/com.apple.TCC/TCC.db");
    match std::fs::File::open(&probe) {
        Ok(_) => true,
        Err(e) => e.kind() == std::io::ErrorKind::NotFound,
    }
}

#[cfg(not(target_os = "macos"))]
pub fn has_full_access() -> bool {
    true
}

/// Shows a path in Finder / Explorer, selected.
pub fn reveal(path: &Path) {
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg("-R").arg(path).spawn();
    #[cfg(windows)]
    let _ = std::process::Command::new("explorer").arg(format!("/select,{}", path.display())).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let _ = std::process::Command::new("xdg-open").arg(path.parent().unwrap_or(path)).spawn();
}

/// Opens a file or folder with its default application.
pub fn open(path: &Path) {
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg(path).spawn();
    #[cfg(windows)]
    let _ = std::process::Command::new("explorer").arg(path).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let _ = std::process::Command::new("xdg-open").arg(path).spawn();
}

/// Where DiskDiff keeps its store and settings.
pub fn data_dir() -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        home().join("Library/Application Support/app.diskdiff.native")
    }
    #[cfg(windows)]
    {
        appdata().join("DiskDiff Native")
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home().join(".local/share"))
            .join("diskdiff")
    }
}

#[cfg(unix)]
fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| ".".into())
}

#[cfg(windows)]
fn home() -> PathBuf {
    std::env::var_os("USERPROFILE").map(PathBuf::from).unwrap_or_else(|| ".".into())
}

#[cfg(windows)]
fn appdata() -> PathBuf {
    std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(|| ".".into())
}
