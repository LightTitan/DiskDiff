//! Reading one directory: names, kinds and allocated sizes in as few system
//! calls as the platform allows.
//!
//! macOS uses `getattrlistbulk`, which returns a whole batch of entries with
//! their attributes per call instead of one `lstat` per entry. Windows gets
//! sizes for free from `FindNextFile` through `std::fs::read_dir`. Elsewhere we
//! fall back to `read_dir` + `symlink_metadata`.

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
    pub name: String,
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
}

#[cfg(target_os = "macos")]
pub use macos::list_dir;

#[cfg(not(target_os = "macos"))]
pub use portable::list_dir;

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
    const ATTR_FILE_LINKCOUNT: u32 = 0x0000_0001;
    const ATTR_FILE_ALLOCSIZE: u32 = 0x0000_0004;
    const ATTR_FILE_DATALENGTH: u32 = 0x0000_0200;
    const VREG: u32 = 1;
    const VDIR: u32 = 2;

    pub fn list_dir(path: &Path) -> io::Result<Vec<RawEntry>> {
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
            dirattr: 0,
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
                if let Some(e) = parse(entry) {
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
    /// set and the error code.
    fn parse(e: &[u8]) -> Option<RawEntry> {
        let mut p = 4;
        let common = rd_u32(e, p);
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
        if common & ATTR_CMN_NAME != 0 {
            let off = i32::from_ne_bytes(e[p..p + 4].try_into().unwrap()) as isize;
            let len = rd_u32(e, p + 4) as usize;
            let start = (p as isize + off) as usize;
            let raw = e.get(start..start + len.saturating_sub(1))?;
            name = String::from_utf8_lossy(raw).into_owned();
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
        Some(RawEntry { name, kind, size, logical, dev, ino, nlink, cloud: flags & SF_DATALESS != 0 })
    }
}

#[cfg_attr(target_os = "macos", allow(dead_code))]
pub mod portable {
    use super::*;

    pub fn list_dir(path: &Path) -> io::Result<Vec<RawEntry>> {
        let mut out = Vec::new();
        for entry in std::fs::read_dir(path)? {
            let Ok(entry) = entry else { continue };
            let Ok(ft) = entry.file_type() else { continue };
            let name = entry.file_name().to_string_lossy().into_owned();
            if ft.is_dir() {
                let dev = dev_of(&entry);
                let cloud = is_cloud(&entry);
                out.push(RawEntry { name, kind: Kind::Dir, size: 0, logical: 0, dev, ino: 0, nlink: 1, cloud });
            } else if ft.is_file() {
                let Ok(md) = entry.metadata() else { continue };
                let (size, dev, ino, nlink) = file_facts(&md);
                out.push(RawEntry { name, kind: Kind::File, size, logical: md.len(), dev, ino, nlink, cloud: false });
            }
        }
        Ok(out)
    }

    #[cfg(unix)]
    fn dev_of(entry: &std::fs::DirEntry) -> u64 {
        use std::os::unix::fs::MetadataExt;
        entry.metadata().map(|m| m.dev()).unwrap_or(0)
    }

    #[cfg(not(unix))]
    fn dev_of(_: &std::fs::DirEntry) -> u64 {
        0
    }

    /// Windows cloud files (OneDrive, Google Drive…): the find data already
    /// carries the attributes, so asking costs no file open.
    #[cfg(windows)]
    fn is_cloud(entry: &std::fs::DirEntry) -> bool {
        use std::os::windows::fs::MetadataExt;
        const RECALL_ON_OPEN: u32 = 0x0004_0000;
        const RECALL_ON_DATA_ACCESS: u32 = 0x0040_0000;
        entry.metadata().is_ok_and(|m| m.file_attributes() & (RECALL_ON_OPEN | RECALL_ON_DATA_ACCESS) != 0)
    }

    #[cfg(not(windows))]
    fn is_cloud(_: &std::fs::DirEntry) -> bool {
        false
    }

    #[cfg(unix)]
    fn file_facts(md: &std::fs::Metadata) -> (u64, u64, u64, u32) {
        use std::os::unix::fs::MetadataExt;
        (md.blocks() * 512, md.dev(), md.ino(), md.nlink() as u32)
    }

    /// Windows: allocated size is not in the directory listing; round the
    /// logical size up to the usual 4 KiB cluster.
    #[cfg(not(unix))]
    fn file_facts(md: &std::fs::Metadata) -> (u64, u64, u64, u32) {
        let len = md.len();
        (len.div_ceil(4096) * 4096, 0, 0, 1)
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
    }
}
