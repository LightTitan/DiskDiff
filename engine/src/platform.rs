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

/// Windows: the volume serial number of the volume holding `path` (0 if it
/// can't be read, which the walk treats as "same volume").
///
/// Only the scan root's volume is looked up. Entries inside the walk get 0:
/// on Windows another volume can only appear inside a folder tree through a
/// reparse point (a volume mount point is a junction-style reparse point),
/// and the walk never enters reparse points, so per-entry volume ids would
/// cost a file open each and add nothing.
#[cfg(windows)]
pub fn dev_of(path: &Path) -> u64 {
    win::open_info(path, true).map_or(0, |i| u64::from(i.dwVolumeSerialNumber))
}

#[cfg(not(any(unix, windows)))]
pub fn dev_of(_: &Path) -> u64 {
    0
}

/// The canonical absolute form of a scan root. On Windows,
/// `canonicalize` returns verbatim paths (`\\?\C:\…`); the plain form
/// (`C:\…`) is what users type and what saved locations hold, so it's used
/// instead. std adds the `\\?\` prefix again by itself whenever a path is
/// too long for the legacy API, so long paths keep working.
pub fn canonical_root(root: &Path) -> PathBuf {
    let c = root.canonicalize().unwrap_or_else(|_| root.to_owned());
    strip_verbatim(c)
}

fn strip_verbatim(p: PathBuf) -> PathBuf {
    let Some(s) = p.to_str() else { return p };
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{rest}"));
    }
    if let Some(rest) = s.strip_prefix(r"\\?\") {
        let b = rest.as_bytes();
        if b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':' {
            return PathBuf::from(rest);
        }
    }
    p
}

/// Whether the walk should read exact hard-link counts for the files in
/// `dir`, which costs one file open per file on Windows (a directory listing
/// doesn't carry the link count there; on Unix `lstat` does, for free).
///
/// Windows: only inside the Windows folder. The component store
/// (`C:\Windows\WinSxS`) hard-links most of System32 and friends, so without
/// link counts several GB would be counted twice. Hard links elsewhere are
/// rare on Windows, and opening every file on the disk would make a scan
/// several times slower, so files outside it count in full at every link.
#[cfg(windows)]
pub fn wants_link_counts(dir: &Path) -> bool {
    use std::sync::OnceLock;
    static WINDIR: OnceLock<String> = OnceLock::new();
    let windir = WINDIR.get_or_init(|| {
        let w = std::env::var_os("SystemRoot").or_else(|| std::env::var_os("windir"));
        let w = w.map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
        let mut s = w.to_string_lossy().to_lowercase();
        if !s.ends_with('\\') {
            s.push('\\');
        }
        s
    });
    let mut d = dir.to_string_lossy().to_lowercase();
    if !d.ends_with('\\') {
        d.push('\\');
    }
    d.starts_with(windir.as_str())
}

#[cfg(not(windows))]
pub fn wants_link_counts(_: &Path) -> bool {
    false
}

/// Small Win32 helpers shared by the platform code and the directory lister.
#[cfg(windows)]
pub(crate) mod win {
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
        FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, GetCompressedFileSizeW,
        GetFileInformationByHandle, INVALID_FILE_SIZE, OPEN_EXISTING,
    };

    /// A NUL-terminated wide path, with the `\\?\` prefix when it's too long
    /// for the legacy MAX_PATH limit (paths here are absolute and normalized,
    /// as the walk builds them from a canonical root).
    pub fn wide(path: &Path) -> Vec<u16> {
        let w: Vec<u16> = path.as_os_str().encode_wide().collect();
        let mut out = Vec::with_capacity(w.len() + 9);
        let already = w.starts_with(&[b'\\' as u16, b'\\' as u16, b'?' as u16, b'\\' as u16]);
        if w.len() >= 248 && !already {
            if w.starts_with(&[b'\\' as u16, b'\\' as u16]) {
                out.extend(r"\\?\UNC\".encode_utf16());
                out.extend_from_slice(&w[2..]);
            } else {
                out.extend(r"\\?\".encode_utf16());
                out.extend_from_slice(&w);
            }
        } else {
            out.extend_from_slice(&w);
        }
        out.push(0);
        out
    }

    /// Opens `path` for attribute reads only and returns its handle
    /// information (volume serial, link count, file index). Never follows a
    /// reparse point and never reads contents, so it doesn't recall cloud
    /// files or traverse junctions. `dir` adds the flag needed to open
    /// directories.
    pub fn open_info(path: &Path, dir: bool) -> Option<BY_HANDLE_FILE_INFORMATION> {
        let w = wide(path);
        let mut flags = FILE_FLAG_OPEN_REPARSE_POINT;
        if dir {
            flags |= FILE_FLAG_BACKUP_SEMANTICS;
        }
        // SAFETY: `w` is NUL-terminated and outlives the call; the handle is
        // closed below.
        unsafe {
            let h = CreateFileW(
                w.as_ptr(),
                FILE_READ_ATTRIBUTES,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                std::ptr::null(),
                OPEN_EXISTING,
                flags,
                std::ptr::null_mut(),
            );
            if h == INVALID_HANDLE_VALUE {
                return None;
            }
            let mut info: BY_HANDLE_FILE_INFORMATION = std::mem::zeroed();
            let ok = GetFileInformationByHandle(h, &mut info) != 0;
            CloseHandle(h);
            ok.then_some(info)
        }
    }

    /// Bytes a compressed (NTFS or CompactOS/WOF), sparse or deduplicated
    /// file takes on disk.
    pub fn compressed_size(path: &Path) -> Option<u64> {
        let w = wide(path);
        let mut high = 0u32;
        // SAFETY: `w` is NUL-terminated; `high` is a valid out pointer.
        let low = unsafe { GetCompressedFileSizeW(w.as_ptr(), &mut high) };
        if low == INVALID_FILE_SIZE && std::io::Error::last_os_error().raw_os_error() != Some(0) {
            return None;
        }
        Some((u64::from(high) << 32) | u64::from(low))
    }
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

/// macOS.
///
/// The root volume is the sealed system volume; user data lives on the Data
/// volume, firmlinked into place (/Users, /Applications, /Library,
/// /private, /usr/local…). Walking "/" therefore has to enter the Data
/// volume through those firmlinks, but must not walk /System/Volumes/Data
/// itself as well, or everything would be counted twice.
///
/// Folders like `.Spotlight-V100`, `.fseventsd` and `.DocumentRevisions-V100`
/// stay in: they're real space on the Data volume (and usually show as
/// unreadable without Full Disk Access). Time Machine local snapshots are
/// APFS snapshots, not folders, so they never appear in the tree; when one is
/// browsed it's mounted under /Volumes, which is skipped.
#[cfg(target_os = "macos")]
pub fn plan(root: &Path) -> Plan {
    let data = Path::new("/System/Volumes/Data");
    let mut p = Plan::default();
    if root.starts_with("/Volumes") {
        // Another disk (or a disk image): its own volume, nothing special.
        return p;
    }
    // Anything not on another disk may reach the Data volume through a
    // firmlink (scanning /usr reaches /usr/local, / reaches everything).
    p.extra_devs.push(dev_of(data));
    let skip: &[&str] = &[
        // The Data volume's own mount point: its contents are already
        // reached through the firmlinks.
        "/System/Volumes/Data",
        // Virtual or regenerated trees that are not "used space" a user
        // can act on.
        "/dev",
        "/System/Volumes/VM",
        "/System/Volumes/Preboot",
        "/System/Volumes/Update",
        "/System/Volumes/xarts",
        "/System/Volumes/iSCPreboot",
        "/System/Volumes/Hardware",
        // Other disks, network shares, mounted disk images and browsed
        // Time Machine snapshots.
        "/Volumes",
        // autofs triggers (auto_home, the network browser): entering one
        // mounts a network share or waits on a server and can hang. They
        // are separate devices too, so the volume check would also stop
        // there, but naming them avoids even looking.
        "/home",
        "/net",
        "/Network/Servers",
        "/System/Volumes/Data/home",
    ];
    for s in skip {
        let s = Path::new(s);
        // Only what lies inside this root, and never the root itself (a
        // scan of /System/Volumes/Data must still walk it).
        if s.starts_with(root) && s != root {
            p.skip.push(s.to_owned());
        }
    }
    p
}

/// Linux: the walk stays on the root's filesystem, using
/// `/proc/self/mountinfo` to tell which mount points under the root belong
/// to it. See [`linux_plan`].
#[cfg(target_os = "linux")]
pub fn plan(root: &Path) -> Plan {
    match std::fs::read_to_string("/proc/self/mountinfo") {
        Ok(text) => linux_plan(root, &parse_mountinfo(&text), dev_of),
        Err(_) => pseudo_fs_plan(root),
    }
}

/// Windows: nothing to plan. Every way into another volume or back into the
/// same tree (junctions such as `C:\Documents and Settings`, directory
/// symlinks, volume mount points) is a reparse point, and the walk never
/// enters reparse points (see `listing`). `System Volume Information`
/// (shadow copies) and `$Recycle.Bin` are real space and stay in;
/// `pagefile.sys`, `hiberfil.sys` and `swapfile.sys` are counted from the
/// directory listing, so being in use doesn't hide them.
#[cfg(windows)]
pub fn plan(_root: &Path) -> Plan {
    Plan::default()
}

#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
pub fn plan(root: &Path) -> Plan {
    pseudo_fs_plan(root)
}

/// Kernel and runtime pseudo filesystems at their usual places. Also caught
/// by the mount table on Linux; listed in case it can't be read.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
const PSEUDO_FS: &[&str] = &["/proc", "/sys", "/dev", "/run"];

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn pseudo_fs_plan(root: &Path) -> Plan {
    let mut p = Plan::default();
    if root == Path::new("/") {
        p.skip.extend(PSEUDO_FS.iter().map(PathBuf::from));
    }
    p
}

/// One line of `/proc/self/mountinfo`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Mount {
    /// Where it's mounted.
    pub mount_point: PathBuf,
    /// The path inside the filesystem that is mounted there (the btrfs
    /// subvolume, or a bind mount's source folder).
    pub root: String,
    pub fstype: String,
    /// The device, pool/dataset or server it comes from.
    pub source: String,
}

/// Parses `/proc/self/mountinfo` (see proc(5)): `id parent major:minor root
/// mount-point options [optional fields…] - fstype source super-options`.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn parse_mountinfo(text: &str) -> Vec<Mount> {
    text.lines()
        .filter_map(|line| {
            let f: Vec<&str> = line.split(' ').collect();
            let dash = f.iter().position(|x| *x == "-")?;
            if dash < 6 || f.len() < dash + 3 {
                return None;
            }
            Some(Mount {
                mount_point: PathBuf::from(unescape_mount(f[4])),
                root: unescape_mount(f[3]),
                fstype: f[dash + 1].to_owned(),
                source: unescape_mount(f[dash + 2]),
            })
        })
        .collect()
}

/// The kernel writes space, tab, newline and backslash as `\040`-style
/// octal escapes.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn unescape_mount(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' && i + 3 < b.len() && b[i + 1..i + 4].iter().all(|c| (b'0'..=b'7').contains(c)) {
            out.push((b[i + 1] - b'0') * 64 + (b[i + 2] - b'0') * 8 + (b[i + 3] - b'0'));
            i += 4;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Linux walk plan from the mount table.
///
/// The walk stays on the device the root is on, which is right for ext4 or
/// xfs but would cut btrfs and ZFS installs in half: on Fedora and openSUSE
/// `/` and `/home` are subvolumes of one btrfs filesystem with different
/// `st_dev`, and Ubuntu's ZFS layout puts them in datasets of one pool. So
/// mounts under the root that are part of the root's filesystem (same btrfs
/// device, same ZFS pool) are allowed, by their `st_dev`; every other mount
/// point under the root — other disks and partitions, network shares (nfs,
/// cifs, sshfs and other FUSE), tmpfs, overlay (Docker), squashfs (snaps),
/// proc/sysfs/cgroup — is skipped by path, so the walk never even `lstat`s
/// it (a dead network mount would hang that call).
///
/// Snapshots are left out even when they're on the same filesystem (snapper's
/// `.snapshots`, Timeshift's), since they share their data with the live
/// files and would count it again. Unmounted nested btrfs subvolumes have
/// their own `st_dev` and aren't in the mount table, so the volume check
/// stops at them (that's how Docker's and snapper's subvolumes look).
/// Bind mounts of the same disk filesystem are allowed; the walk's
/// (device, inode) check keeps a folder reached twice from counting twice.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn linux_plan(root: &Path, mounts: &[Mount], dev: impl Fn(&Path) -> u64) -> Plan {
    let mut p = pseudo_fs_plan(root);
    // A later mount on the same point hides the earlier ones.
    let visible: Vec<&Mount> = mounts
        .iter()
        .enumerate()
        .filter(|(i, m)| !mounts[i + 1..].iter().any(|n| n.mount_point == m.mount_point))
        .map(|(_, m)| m)
        .collect();
    let Some(home) = visible
        .iter()
        .filter(|m| root.starts_with(&m.mount_point))
        .max_by_key(|m| m.mount_point.components().count())
    else {
        return p;
    };
    let root_dev = dev(root);
    let mut zfs_roots = Vec::new();
    if home.fstype == "zfs" {
        zfs_roots.push(home.mount_point.clone());
    }
    for m in &visible {
        if m.mount_point == home.mount_point || !m.mount_point.starts_with(root) {
            continue;
        }
        if same_filesystem(home, m) && !is_snapshot(m) {
            let d = dev(&m.mount_point);
            if d != 0 && d != root_dev && !p.extra_devs.contains(&d) {
                p.extra_devs.push(d);
            }
            if m.fstype == "zfs" {
                zfs_roots.push(m.mount_point.clone());
            }
        } else if !p.skip.contains(&m.mount_point) {
            p.skip.push(m.mount_point.clone());
        }
    }
    // ZFS's `.zfs/snapshot` folders mount a snapshot on access (they're
    // hidden by default, but `snapdir=visible` lists them).
    for z in zfs_roots {
        let s = z.join(".zfs");
        if s.starts_with(root) {
            p.skip.push(s);
        }
    }
    p
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn same_filesystem(a: &Mount, b: &Mount) -> bool {
    if a.fstype != b.fstype {
        return false;
    }
    match a.fstype.as_str() {
        // Subvolumes of one btrfs filesystem share its device.
        "btrfs" => a.source == b.source,
        // Datasets of one pool: `pool/ROOT/ubuntu`, `pool/USERDATA/…`.
        "zfs" => a.source.split('/').next() == b.source.split('/').next(),
        // A bind mount of the same disk filesystem has the same device.
        // Virtual filesystems all report a placeholder source ("tmpfs",
        // "overlay", "none"), so only real devices count.
        _ => a.source.starts_with("/dev/") && a.source == b.source,
    }
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn is_snapshot(m: &Mount) -> bool {
    let snap = |p: &Path| {
        p.components().any(|c| {
            let c = c.as_os_str().to_string_lossy();
            c == ".snapshots" || c == "@snapshots" || c.starts_with("timeshift-btrfs")
        })
    };
    snap(Path::new(&m.root)) || snap(&m.mount_point)
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

/// Elsewhere there's no per-app grant to ask for, so this is always true.
/// That doesn't mean everything is readable: on Linux other users' homes and
/// root-only folders, and on Windows (unelevated) folders such as
/// `System Volume Information` or other users' profiles, can't be listed.
/// The walk marks those folders unreadable and counts them in the scan's
/// unreadable total instead of failing; their size shows as unknown.
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

/// The store DiskDiff and its agent server open: `DISKDIFF_DB` if set (demo
/// data, tests), otherwise `snapshots.db` in [`data_dir`].
pub fn store_path() -> PathBuf {
    std::env::var_os("DISKDIFF_DB").map(PathBuf::from).unwrap_or_else(|| data_dir().join("snapshots.db"))
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
#[allow(dead_code)]
fn home() -> PathBuf {
    std::env::var_os("USERPROFILE").map(PathBuf::from).unwrap_or_else(|| ".".into())
}

#[cfg(windows)]
fn appdata() -> PathBuf {
    std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(|| ".".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(id: u32, root: &str, mount_point: &str, fstype: &str, source: &str) -> String {
        format!("{id} 1 0:{id} {root} {mount_point} rw,relatime shared:{id} - {fstype} {source} rw\n")
    }

    /// Fedora/openSUSE-like: / and /home are subvolumes of one btrfs
    /// filesystem, with everything a desktop usually has mounted around them.
    fn btrfs_desktop() -> String {
        [
            line(1, "/root", "/", "btrfs", "/dev/vda3"),
            line(2, "/", "/proc", "proc", "proc"),
            line(3, "/", "/sys", "sysfs", "sysfs"),
            line(4, "/", "/sys/fs/cgroup", "cgroup2", "cgroup2"),
            line(5, "/", "/dev", "devtmpfs", "devtmpfs"),
            line(6, "/", "/run", "tmpfs", "tmpfs"),
            line(7, "/home", "/home", "btrfs", "/dev/vda3"),
            line(8, "/", "/boot", "ext4", "/dev/vda2"),
            line(9, "/", "/tmp", "tmpfs", "tmpfs"),
            line(10, "/root/.snapshots", "/.snapshots", "btrfs", "/dev/vda3"),
            line(11, "/export", "/mnt/nas", "nfs4", "server:/export"),
            line(12, "/", "/mnt/share", "cifs", "//server/share"),
            line(13, "/", "/home/user/remote", "fuse.sshfs", "user@server:"),
            line(14, "/", "/var/lib/docker/overlay2/abc/merged", "overlay", "overlay"),
            line(15, "/", "/var/lib/snapd/snap/core/1", "squashfs", "/dev/loop0"),
            line(16, "/", "/media/user/USB\\040Disk", "vfat", "/dev/sdb1"),
            line(17, "/", "/data", "xfs", "/dev/sdc1"),
        ]
        .concat()
    }

    fn devs(p: &Path) -> u64 {
        match p.to_str().unwrap() {
            "/" => 30,
            "/home" => 31,
            "/.snapshots" => 32,
            _ => 99,
        }
    }

    #[test]
    fn mountinfo_parses_fields_and_escapes() {
        let m = parse_mountinfo(&btrfs_desktop());
        assert_eq!(m.len(), 17);
        assert_eq!(m[6], Mount { mount_point: "/home".into(), root: "/home".into(), fstype: "btrfs".into(), source: "/dev/vda3".into() });
        assert_eq!(m[15].mount_point, Path::new("/media/user/USB Disk"));
        // No optional fields, or several of them.
        let m = parse_mountinfo("1 0 8:1 / / rw - ext4 /dev/sda1 rw\n2 1 0:5 / /x rw shared:1 master:2 - tmpfs tmpfs rw\nbad line\n");
        assert_eq!(m.len(), 2);
        assert_eq!((m[1].fstype.as_str(), m[1].source.as_str()), ("tmpfs", "tmpfs"));
    }

    #[test]
    fn btrfs_root_includes_home_subvolume_but_no_other_mounts() {
        let p = linux_plan(Path::new("/"), &parse_mountinfo(&btrfs_desktop()), devs);
        // /home is another subvolume of the root's filesystem: walked.
        assert_eq!(p.extra_devs, vec![31]);
        assert!(!p.skip.contains(&PathBuf::from("/home")));
        for s in [
            "/proc",
            "/sys",
            "/sys/fs/cgroup",
            "/dev",
            "/run",
            "/boot",
            "/tmp",
            "/.snapshots",
            "/mnt/nas",
            "/mnt/share",
            "/home/user/remote",
            "/var/lib/docker/overlay2/abc/merged",
            "/var/lib/snapd/snap/core/1",
            "/media/user/USB Disk",
            "/data",
        ] {
            assert!(p.skip.contains(&PathBuf::from(s)), "{s} should be skipped: {:?}", p.skip);
        }
    }

    #[test]
    fn scanning_a_folder_only_plans_mounts_inside_it() {
        let p = linux_plan(Path::new("/home/user"), &parse_mountinfo(&btrfs_desktop()), devs);
        assert!(p.extra_devs.is_empty());
        assert_eq!(p.skip, vec![PathBuf::from("/home/user/remote")]);
        // A folder on another disk: just that disk.
        let p = linux_plan(Path::new("/data/Photos"), &parse_mountinfo(&btrfs_desktop()), devs);
        assert!(p.extra_devs.is_empty() && p.skip.is_empty(), "{p:?}");
    }

    #[test]
    fn ext4_with_separate_home_partition_stays_on_root() {
        let info = [
            line(1, "/", "/", "ext4", "/dev/sda2"),
            line(2, "/", "/home", "ext4", "/dev/sda3"),
            line(3, "/", "/boot/efi", "vfat", "/dev/sda1"),
            // A bind mount of a folder on the root filesystem.
            line(4, "/srv/Project", "/home/Project", "ext4", "/dev/sda2"),
        ]
        .concat();
        let p = linux_plan(Path::new("/"), &parse_mountinfo(&info), |_| 30);
        assert!(p.extra_devs.is_empty());
        assert!(p.skip.contains(&PathBuf::from("/home")));
        assert!(p.skip.contains(&PathBuf::from("/boot/efi")));
        // Same disk filesystem: left to the walk's (device, inode) check.
        assert!(!p.skip.contains(&PathBuf::from("/home/Project")));
    }

    #[test]
    fn zfs_datasets_of_the_root_pool_are_walked() {
        let info = [
            line(1, "/", "/", "zfs", "rpool/ROOT/ubuntu"),
            line(2, "/", "/home/user", "zfs", "rpool/USERDATA/user"),
            line(3, "/", "/boot", "zfs", "bpool/BOOT/ubuntu"),
        ]
        .concat();
        let dev = |p: &Path| if p == Path::new("/home/user") { 41 } else { 40 };
        let p = linux_plan(Path::new("/"), &parse_mountinfo(&info), dev);
        assert_eq!(p.extra_devs, vec![41]);
        assert!(p.skip.contains(&PathBuf::from("/boot")));
        assert!(p.skip.contains(&PathBuf::from("/.zfs")));
        assert!(p.skip.contains(&PathBuf::from("/home/user/.zfs")));
    }

    #[test]
    fn a_later_mount_hides_an_earlier_one_on_the_same_point() {
        let info = [
            line(1, "/root", "/", "btrfs", "/dev/vda3"),
            line(2, "/data", "/mnt/x", "btrfs", "/dev/vda3"),
            line(3, "/", "/mnt/x", "nfs", "server:/x"),
        ]
        .concat();
        let p = linux_plan(Path::new("/"), &parse_mountinfo(&info), |_| 5);
        assert!(p.skip.contains(&PathBuf::from("/mnt/x")));
    }

    #[test]
    fn pseudo_filesystems_are_skipped_without_a_mount_table() {
        let p = linux_plan(Path::new("/"), &[], |_| 1);
        for s in PSEUDO_FS {
            assert!(p.skip.contains(&PathBuf::from(s)));
        }
        assert!(linux_plan(Path::new("/home"), &[], |_| 1).skip.is_empty());
    }

    #[test]
    fn verbatim_prefixes_are_removed() {
        assert_eq!(strip_verbatim(PathBuf::from(r"\\?\C:\Users")), PathBuf::from(r"C:\Users"));
        assert_eq!(strip_verbatim(PathBuf::from(r"\\?\UNC\server\share")), PathBuf::from(r"\\server\share"));
        // Device paths have no plain form; left alone.
        assert_eq!(strip_verbatim(PathBuf::from(r"\\?\Volume{1}\")), PathBuf::from(r"\\?\Volume{1}\"));
        assert_eq!(strip_verbatim(PathBuf::from("/home/user")), PathBuf::from("/home/user"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_root_skips_data_volume_autofs_and_other_disks() {
        let p = plan(Path::new("/"));
        for s in ["/System/Volumes/Data", "/System/Volumes/VM", "/Volumes", "/dev", "/home", "/net", "/System/Volumes/Data/home"] {
            assert!(p.skip.contains(&PathBuf::from(s)), "{s}");
        }
        assert_eq!(p.extra_devs, vec![dev_of(Path::new("/System/Volumes/Data"))]);
        // Scanning the Data volume directly walks it, but not its autofs mount.
        let p = plan(Path::new("/System/Volumes/Data"));
        assert_eq!(p.skip, vec![PathBuf::from("/System/Volumes/Data/home")]);
        // A firmlinked folder may reach the Data volume.
        assert!(plan(Path::new("/usr")).skip.is_empty());
        assert!(!plan(Path::new("/usr")).extra_devs.is_empty());
        // Another disk: nothing special.
        let p = plan(Path::new("/Volumes/Backup/Photos"));
        assert!(p.skip.is_empty() && p.extra_devs.is_empty());
    }

    #[cfg(windows)]
    #[test]
    fn windows_volume_id_is_real() {
        let tmp = std::env::temp_dir();
        assert_ne!(dev_of(&tmp), 0);
        assert_eq!(dev_of(&tmp), dev_of(&tmp.join("..")));
        assert!(wants_link_counts(&PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join("System32")));
        assert!(!wants_link_counts(&tmp));
    }
}
