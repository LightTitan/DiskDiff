//! Reading one directory: names, kinds and allocated sizes in as few system
//! calls as the platform allows.
//!
//! macOS uses `getattrlistbulk`, which returns a whole batch of entries with
//! their attributes per call instead of one `lstat` per entry. Windows gets
//! sizes for free from `FindNextFile` through `std::fs::read_dir`. Elsewhere we
//! fall back to `read_dir` + `symlink_metadata`.
//!
//! Rules every platform keeps, so a walk can never loop, double count, hang
//! or download anything:
//! - Symlinks are never followed (Unix: `lstat`/`d_type`; Windows: every
//!   name-surrogate reparse point — symlinks, junctions, volume mount points —
//!   is a symlink to std and comes back as [`Kind::Other`]).
//! - Nothing is ever opened for reading. FIFOs, sockets and device files are
//!   only seen through their directory entry and skipped.
//! - Cloud placeholders are reported, never opened.

use std::ffi::{OsStr, OsString};
use std::io;
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    File,
    Dir,
    /// Symlinks, sockets, devices… listed as zero-size files are not useful, so
    /// they are skipped.
    Other,
}

#[derive(Debug)]
pub struct RawEntry {
    /// The name as text, for storing and showing. Names that aren't valid
    /// Unicode (possible on Linux) are converted lossily here.
    pub name: String,
    /// The exact name on disk, for building the paths the walk opens, so a
    /// folder with a non-Unicode name is still walked.
    pub raw_name: OsString,
    pub kind: Kind,
    /// Allocated bytes on disk (files only).
    pub size: u64,
    /// The file's length (files only). Larger than `size` for online-only
    /// cloud files, compressed and sparse files. Not stored in scans.
    pub logical: u64,
    /// Device id, to stop at mount points.
    pub dev: u64,
    /// Inode / file id, only meaningful when `nlink > 1`.
    pub ino: u64,
    pub nlink: u32,
    /// A cloud placeholder (iCloud "dataless", OneDrive/Google Drive
    /// recall-on-open): its contents are not on this disk, and opening it
    /// would make the OS fetch them from the network.
    pub cloud: bool,
    /// A folder whose details couldn't be read (Unix: `lstat` failed, e.g. a
    /// network mount that died after the scan started). The walk marks it
    /// unreadable without trying to enter it.
    pub unreadable: bool,
}

/// Extra choices for [`list_dir_with`].
#[derive(Clone, Copy, Debug, Default)]
pub struct ListOptions<'a> {
    /// Names in this folder to leave out without looking at them at all, not
    /// even an `lstat`: mount points of other filesystems, where a dead
    /// network share would hang the call and an autofs trigger would mount.
    pub skip: &'a [OsString],
    /// Read each file's exact hard-link count even where that costs a file
    /// open (Windows; see [`crate::platform::wants_link_counts`]). On Unix
    /// the count always comes with the entry's metadata.
    pub link_counts: bool,
    /// Windows: the volume's cluster size, what sizes on disk round up to
    /// (0 means the NTFS default, 4 KiB). Unix and macOS report allocated
    /// sizes directly.
    pub cluster: u64,
    /// Windows: the folder is on a network share. Asking a network
    /// redirector for compressed sizes can block indefinitely, so it isn't.
    pub remote: bool,
}

/// Lists `path` with default options.
pub fn list_dir(path: &Path) -> io::Result<Vec<RawEntry>> {
    list_dir_with(path, &ListOptions::default())
}

#[cfg(target_os = "macos")]
pub use macos::list_dir_with;

#[cfg(not(target_os = "macos"))]
pub use portable::list_dir_with;

fn skipped(opts: &ListOptions, name: &OsStr) -> bool {
    !opts.skip.is_empty() && opts.skip.iter().any(|s| s == name)
}

#[cfg(target_os = "macos")]
mod macos {
    use super::*;
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    #[repr(C)]
    struct AttrList {
        bitmapcount: u16,
        reserved: u16,
        commonattr: u32,
        volattr: u32,
        dirattr: u32,
        fileattr: u32,
        forkattr: u32,
    }

    unsafe extern "C" {
        fn getattrlistbulk(dirfd: i32, attrlist: *mut AttrList, buf: *mut u8, size: usize, options: u64) -> i32;
    }

    const ATTR_BIT_MAP_COUNT: u16 = 5;
    const ATTR_CMN_NAME: u32 = 0x0000_0001;
    const ATTR_CMN_DEVID: u32 = 0x0000_0002;
    const ATTR_CMN_OBJTYPE: u32 = 0x0000_0008;
    const ATTR_CMN_FLAGS: u32 = 0x0004_0000;
    /// `SF_DATALESS` from sys/stat.h: an evicted iCloud/File Provider item.
    const SF_DATALESS: u32 = 0x4000_0000;
    const ATTR_CMN_FILEID: u32 = 0x0200_0000;
    const ATTR_CMN_ERROR: u32 = 0x2000_0000;
    const ATTR_CMN_RETURNED_ATTRS: u32 = 0x8000_0000;
    const ATTR_DIR_MOUNTSTATUS: u32 = 0x0000_0004;
    /// Something is mounted on this folder.
    const DIR_MNTSTATUS_MNTPOINT: u32 = 0x0000_0001;
    /// An autofs trigger: entering it mounts something (often over the network).
    const DIR_MNTSTATUS_TRIGGER: u32 = 0x0000_0002;
    const ATTR_FILE_LINKCOUNT: u32 = 0x0000_0001;
    const ATTR_FILE_ALLOCSIZE: u32 = 0x0000_0004;
    const ATTR_FILE_DATALENGTH: u32 = 0x0000_0200;
    const VREG: u32 = 1;
    const VDIR: u32 = 2;

    pub fn list_dir_with(path: &Path, opts: &ListOptions) -> io::Result<Vec<RawEntry>> {
        let c = CString::new(path.as_os_str().as_bytes()).map_err(|_| io::ErrorKind::InvalidInput)?;
        let fd = unsafe { libc::open(c.as_ptr(), libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        struct Fd(i32);
        impl Drop for Fd {
            fn drop(&mut self) {
                unsafe { libc::close(self.0) };
            }
        }
        let _guard = Fd(fd);

        let mut attrs = AttrList {
            bitmapcount: ATTR_BIT_MAP_COUNT,
            reserved: 0,
            commonattr: ATTR_CMN_RETURNED_ATTRS
                | ATTR_CMN_NAME
                | ATTR_CMN_ERROR
                | ATTR_CMN_DEVID
                | ATTR_CMN_OBJTYPE
                | ATTR_CMN_FLAGS
                | ATTR_CMN_FILEID,
            volattr: 0,
            dirattr: ATTR_DIR_MOUNTSTATUS,
            fileattr: ATTR_FILE_LINKCOUNT | ATTR_FILE_ALLOCSIZE | ATTR_FILE_DATALENGTH,
            forkattr: 0,
        };
        let mut buf = vec![0u8; 128 * 1024];
        let mut out = Vec::new();
        loop {
            let n = unsafe { getattrlistbulk(fd, &mut attrs, buf.as_mut_ptr(), buf.len(), 0) };
            if n < 0 {
                let e = io::Error::last_os_error();
                if out.is_empty() {
                    return Err(e);
                }
                break; // keep what we got
            }
            if n == 0 {
                break;
            }
            let mut p = 0usize;
            for _ in 0..n {
                let entry = &buf[p..];
                let len = rd_u32(entry, 0) as usize;
                if len == 0 || p + len > buf.len() {
                    break;
                }
                // getattrlistbulk reports an entry's attributes without
                // entering it, so skipped names and mount points cost nothing.
                if let Some(e) = parse(entry).filter(|e| !skipped(opts, &e.raw_name)) {
                    out.push(e);
                }
                p += len;
            }
        }
        Ok(out)
    }

    fn rd_u32(b: &[u8], at: usize) -> u32 {
        u32::from_ne_bytes(b[at..at + 4].try_into().unwrap())
    }

    fn rd_u64(b: &[u8], at: usize) -> u64 {
        u64::from_ne_bytes(b[at..at + 8].try_into().unwrap())
    }

    /// Fields follow the length in bit order, after the returned-attributes
    /// set and the error code: common, then directory, then file attributes.
    ///
    /// Mount points and autofs triggers are left out. For a mount point
    /// getattrlistbulk reports the folder underneath it, on the parent's
    /// device (see getattrlistbulk(2)), so the walk's device check can't tell
    /// it's another volume; only the mount status can. Leaving them out means
    /// another disk, a disk image or a network share mounted inside the
    /// scanned folder is never entered, so it can't hang the walk or add to it.
    fn parse(e: &[u8]) -> Option<RawEntry> {
        let mut p = 4;
        let common = rd_u32(e, p);
        let dir = rd_u32(e, p + 8);
        let file = rd_u32(e, p + 12);
        p += 20; // attribute_set_t: five u32
        if common & ATTR_CMN_ERROR != 0 {
            let err = rd_u32(e, p);
            p += 4;
            if err != 0 {
                return None;
            }
        }
        let mut name = String::new();
        let mut raw_name = OsString::new();
        if common & ATTR_CMN_NAME != 0 {
            let off = i32::from_ne_bytes(e[p..p + 4].try_into().unwrap()) as isize;
            let len = rd_u32(e, p + 4) as usize;
            let start = (p as isize + off) as usize;
            let raw = e.get(start..start + len.saturating_sub(1))?;
            name = String::from_utf8_lossy(raw).into_owned();
            raw_name = OsStr::from_bytes(raw).to_owned();
            p += 8;
        }
        let mut dev = 0;
        if common & ATTR_CMN_DEVID != 0 {
            dev = rd_u32(e, p) as u64;
            p += 4;
        }
        let mut objtype = 0;
        if common & ATTR_CMN_OBJTYPE != 0 {
            objtype = rd_u32(e, p);
            p += 4;
        }
        let mut flags = 0;
        if common & ATTR_CMN_FLAGS != 0 {
            flags = rd_u32(e, p);
            p += 4;
        }
        let mut ino = 0;
        if common & ATTR_CMN_FILEID != 0 {
            ino = rd_u64(e, p);
            p += 8;
        }
        if dir & ATTR_DIR_MOUNTSTATUS != 0 {
            let status = rd_u32(e, p);
            p += 4;
            if status & (DIR_MNTSTATUS_MNTPOINT | DIR_MNTSTATUS_TRIGGER) != 0 {
                return None;
            }
        }
        let mut nlink = 1;
        let mut size = 0;
        if file & ATTR_FILE_LINKCOUNT != 0 {
            nlink = rd_u32(e, p);
            p += 4;
        }
        if file & ATTR_FILE_ALLOCSIZE != 0 {
            size = rd_u64(e, p);
            p += 8;
        }
        let mut logical = size;
        if file & ATTR_FILE_DATALENGTH != 0 {
            logical = rd_u64(e, p);
        }
        let kind = match objtype {
            VREG => Kind::File,
            VDIR => Kind::Dir,
            _ => Kind::Other,
        };
        Some(RawEntry { name, raw_name, kind, size, logical, dev, ino, nlink, cloud: flags & SF_DATALESS != 0, unreadable: false })
    }
}

#[cfg_attr(target_os = "macos", allow(dead_code))]
pub mod portable {
    use super::*;

    pub fn list_dir(path: &Path) -> io::Result<Vec<RawEntry>> {
        list_dir_with(path, &ListOptions::default())
    }

    pub fn list_dir_with(path: &Path, opts: &ListOptions) -> io::Result<Vec<RawEntry>> {
        let mut out = Vec::new();
        for entry in std::fs::read_dir(path)? {
            let Ok(entry) = entry else { continue };
            let raw_name = entry.file_name();
            if skipped(opts, &raw_name) {
                continue;
            }
            // From the directory entry itself (`d_type` on Linux, the find
            // data on Windows); symlinks and reparse points are neither dirs
            // nor files here, so they're never followed.
            let Ok(ft) = entry.file_type() else { continue };
            let name = raw_name.to_string_lossy().into_owned();
            let e = if ft.is_dir() {
                dir_entry(&entry, name, raw_name)
            } else if ft.is_file() {
                file_entry(&entry, name, raw_name, opts)
            } else {
                // Symlinks, FIFOs, sockets, devices: left out untouched.
                None
            };
            out.extend(e);
        }
        Ok(out)
    }

    /// Unix: `DirEntry::metadata` is an `lstat` relative to the open
    /// directory, so it never follows a link. Its device and inode let the
    /// walk stop at mount points and notice a folder reached twice.
    #[cfg(unix)]
    fn dir_entry(entry: &std::fs::DirEntry, name: String, raw_name: OsString) -> Option<RawEntry> {
        use std::os::unix::fs::MetadataExt;
        // If even `lstat` fails, the folder's volume is unknown: it isn't
        // entered (it could be a dead network mount), just marked unreadable.
        let (dev, ino, unreadable) = entry.metadata().map_or((0, 0, true), |m| (m.dev(), m.ino(), false));
        Some(RawEntry { name, raw_name, kind: Kind::Dir, size: 0, logical: 0, dev, ino, nlink: 1, cloud: false, unreadable })
    }

    #[cfg(unix)]
    fn file_entry(entry: &std::fs::DirEntry, name: String, raw_name: OsString, _: &ListOptions) -> Option<RawEntry> {
        use std::os::unix::fs::MetadataExt;
        let md = entry.metadata().ok()?;
        Some(RawEntry {
            name,
            raw_name,
            kind: Kind::File,
            // Allocated blocks: sparse files count what they use, swap files
            // and preallocated files what they reserve.
            size: md.blocks() * 512,
            logical: md.len(),
            dev: md.dev(),
            ino: md.ino(),
            nlink: md.nlink() as u32,
            cloud: false,
            unreadable: false,
        })
    }

    /// Windows attribute bits from the find data.
    #[cfg(windows)]
    mod attr {
        pub const REPARSE_POINT: u32 = 0x0000_0400;
        pub const SPARSE: u32 = 0x0000_0200;
        pub const COMPRESSED: u32 = 0x0000_0800;
        pub const OFFLINE: u32 = 0x0000_1000;
        pub const RECALL_ON_OPEN: u32 = 0x0004_0000;
        pub const RECALL_ON_DATA_ACCESS: u32 = 0x0040_0000;
        /// OneDrive, Google Drive and other cloud-files placeholders: the
        /// contents aren't on this disk, and opening one would recall it.
        pub const CLOUD: u32 = OFFLINE | RECALL_ON_OPEN | RECALL_ON_DATA_ACCESS;
    }

    /// Windows: `DirEntry::metadata` comes from the `FindNextFileW` data that
    /// `read_dir` already has — no file is opened, so it can't fail on files
    /// in use (`pagefile.sys`, `hiberfil.sys`) and can't recall cloud files.
    #[cfg(windows)]
    fn dir_entry(entry: &std::fs::DirEntry, name: String, raw_name: OsString) -> Option<RawEntry> {
        use std::os::windows::fs::MetadataExt;
        let attrs = entry.metadata().map_or(0, |m| m.file_attributes());
        let cloud = attrs & (attr::RECALL_ON_OPEN | attr::RECALL_ON_DATA_ACCESS) != 0;
        Some(RawEntry { name, raw_name, kind: Kind::Dir, size: 0, logical: 0, dev: 0, ino: 0, nlink: 1, cloud, unreadable: false })
    }

    /// Windows: the find data has the length and attributes but not the
    /// allocated size or link count.
    /// - Cloud placeholders take (almost) nothing on this disk: 0.
    /// - Compressed (NTFS, or CompactOS/WOF, which shows as a reparse point),
    ///   sparse and deduplicated files ask for their real on-disk size; only
    ///   those few files pay for the call.
    /// - Everything else: the length rounded up to the volume's cluster.
    /// - On a network share no per-file calls at all (see [`ListOptions::remote`]).
    /// - Link counts only when asked (see [`ListOptions::link_counts`]); the
    ///   open reads attributes only and doesn't follow reparse points.
    #[cfg(windows)]
    fn file_entry(entry: &std::fs::DirEntry, name: String, raw_name: OsString, opts: &ListOptions) -> Option<RawEntry> {
        use crate::platform::win;
        use std::os::windows::fs::MetadataExt;
        let md = entry.metadata().ok()?;
        let attrs = md.file_attributes();
        let logical = md.len();
        let cloud = attrs & attr::CLOUD != 0;
        let cluster = if opts.cluster == 0 { 4096 } else { opts.cluster };
        let rounded = |n: u64| n.div_ceil(cluster) * cluster;
        let size = if cloud {
            0
        } else if !opts.remote && attrs & (attr::COMPRESSED | attr::SPARSE | attr::REPARSE_POINT) != 0 {
            match win::compressed_size(&entry.path()) {
                Some(n) => rounded(n),
                // A reparse point the system can't open (a container or
                // sandbox placeholder with no filter loaded here) holds no
                // data on this disk; counting its length would invent space.
                None if attrs & attr::REPARSE_POINT != 0 => 0,
                None => rounded(logical),
            }
        } else {
            rounded(logical)
        };
        let (mut ino, mut nlink) = (0, 1);
        if opts.link_counts
            && !cloud
            && !opts.remote
            && let Some(i) = win::open_info(&entry.path(), false)
        {
            ino = (u64::from(i.nFileIndexHigh) << 32) | u64::from(i.nFileIndexLow);
            nlink = i.nNumberOfLinks.max(1);
        }
        Some(RawEntry { name, raw_name, kind: Kind::File, size, logical, dev: 0, ino, nlink, cloud, unreadable: false })
    }

    #[cfg(not(any(unix, windows)))]
    fn dir_entry(_: &std::fs::DirEntry, name: String, raw_name: OsString) -> Option<RawEntry> {
        Some(RawEntry { name, raw_name, kind: Kind::Dir, size: 0, logical: 0, dev: 0, ino: 0, nlink: 1, cloud: false, unreadable: false })
    }

    #[cfg(not(any(unix, windows)))]
    fn file_entry(entry: &std::fs::DirEntry, name: String, raw_name: OsString, _: &ListOptions) -> Option<RawEntry> {
        let len = entry.metadata().ok()?.len();
        Some(RawEntry { name, raw_name, kind: Kind::File, size: len, logical: len, dev: 0, ino: 0, nlink: 1, cloud: false, unreadable: false })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_files_and_dirs_with_sizes() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.bin"), vec![1u8; 10_000]).unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink("a.bin", dir.path().join("link")).unwrap();
        let mut v = list_dir(dir.path()).unwrap();
        v.sort_by(|a, b| a.name.cmp(&b.name));
        let a = v.iter().find(|e| e.name == "a.bin").unwrap();
        assert_eq!(a.kind, Kind::File);
        assert!(a.size >= 10_000);
        assert_eq!(v.iter().find(|e| e.name == "sub").unwrap().kind, Kind::Dir);
        if let Some(l) = v.iter().find(|e| e.name == "link") {
            assert_eq!(l.kind, Kind::Other);
        }
        // Same answer as the portable path.
        let mut p = portable::list_dir(dir.path()).unwrap();
        p.sort_by(|a, b| a.name.cmp(&b.name));
        let pa = p.iter().find(|e| e.name == "a.bin").unwrap();
        assert_eq!(pa.size, a.size);
        assert_eq!(a.logical, 10_000);
        assert_eq!(pa.logical, 10_000);
        assert_eq!(a.raw_name, "a.bin");
        // Folders carry their device and inode on Unix, so the walk can stop
        // at mount points and skip a folder reached twice.
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let md = std::fs::symlink_metadata(dir.path().join("sub")).unwrap();
            for list in [&v, &p] {
                let sub = list.iter().find(|e| e.name == "sub").unwrap();
                assert_eq!((sub.dev, sub.ino), (md.dev(), md.ino()));
            }
            assert!(p.iter().all(|e| e.name != "link"), "symlinks are left out");
        }
    }
}
