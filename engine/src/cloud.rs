//! Live "cloud size" of folders: the real length of their files, including
//! files whose contents are online only (iCloud Drive, OneDrive, Google
//! Drive…), next to what they take on this disk.
//!
//! Read straight from the disk on demand and never stored: scans keep
//! recording size on disk only. Placeholder folders whose listing isn't on
//! this Mac are not opened (that would fetch it from the network), so a total
//! that includes one is a lower bound, marked `partial`.

use crate::listing::{Kind, list_dir};
use rayon::prelude::*;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CloudSize {
    /// Total length of the files.
    pub logical: u64,
    /// What they occupy on this disk.
    pub on_disk: u64,
    /// Some contents are online-only folders that were not listed, so
    /// `logical` is at least this much.
    pub partial: bool,
}

impl CloudSize {
    fn add(&mut self, o: CloudSize) {
        self.logical += o.logical;
        self.on_disk += o.on_disk;
        self.partial |= o.partial;
    }

    /// Bytes that live online only.
    pub fn online_only(&self) -> u64 {
        self.logical.saturating_sub(self.on_disk)
    }
}

/// Folders where cloud providers keep their files.
#[cfg(target_os = "macos")]
pub fn cloud_roots() -> Vec<PathBuf> {
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else { return Vec::new() };
    vec![home.join("Library/CloudStorage"), home.join("Library/Mobile Documents")]
}

/// Folders where cloud providers keep their files: OneDrive (personal and
/// work accounts set these variables) and iCloud for Windows. Google Drive
/// mounts as its own drive letter, which a scan of `C:\` never enters.
#[cfg(windows)]
pub fn cloud_roots() -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = ["OneDrive", "OneDriveConsumer", "OneDriveCommercial"]
        .into_iter()
        .filter_map(std::env::var_os)
        .map(PathBuf::from)
        .collect();
    if let Some(profile) = std::env::var_os("USERPROFILE") {
        roots.push(PathBuf::from(profile).join("iCloudDrive"));
    }
    roots.sort();
    roots.dedup();
    roots
}

/// Linux has no system-wide placeholder files; sync clients keep full copies.
#[cfg(all(unix, not(target_os = "macos")))]
pub fn cloud_roots() -> Vec<PathBuf> {
    Vec::new()
}

/// Whether `path` is inside a cloud provider's folder.
pub fn is_cloud_path(path: &Path) -> bool {
    cloud_roots().iter().any(|r| path.starts_with(r))
}

/// Measures every entry directly inside `dir`, by name. Sub-folders are
/// walked in parallel. Returns what was measured before `cancel` turned true.
pub fn measure_children(dir: &Path, cancel: &AtomicBool) -> HashMap<String, CloudSize> {
    crate::platform::never_fetch_cloud_files();
    let Ok(entries) = list_dir(dir) else { return HashMap::new() };
    entries
        .into_par_iter()
        .filter_map(|e| {
            let size = match e.kind {
                Kind::File => CloudSize { logical: e.logical, on_disk: e.size, partial: false },
                Kind::Dir if e.cloud => CloudSize { partial: true, ..Default::default() },
                Kind::Dir => walk(&dir.join(&e.raw_name), cancel),
                Kind::Other => return None,
            };
            Some((e.name, size))
        })
        .collect()
}

/// Total of one folder.
pub fn measure(dir: &Path, cancel: &AtomicBool) -> CloudSize {
    crate::platform::never_fetch_cloud_files();
    walk(dir, cancel)
}

fn walk(dir: &Path, cancel: &AtomicBool) -> CloudSize {
    if cancel.load(Relaxed) {
        return CloudSize::default();
    }
    let Ok(entries) = list_dir(dir) else { return CloudSize { partial: true, ..Default::default() } };
    let mut total = CloudSize::default();
    let mut dirs = Vec::new();
    for e in entries {
        match e.kind {
            Kind::File => total.add(CloudSize { logical: e.logical, on_disk: e.size, partial: false }),
            Kind::Dir if e.cloud => total.partial = true,
            Kind::Dir => dirs.push(dir.join(&e.raw_name)),
            Kind::Other => {}
        }
    }
    let sub: Vec<CloudSize> = dirs.par_iter().map(|d| walk(d, cancel)).collect();
    for s in sub {
        total.add(s);
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sums_lengths_of_a_tree() {
        let tmp = tempfile::tempdir().unwrap();
        let d = tmp.path();
        std::fs::create_dir_all(d.join("a/b")).unwrap();
        std::fs::write(d.join("a/one.bin"), vec![1u8; 10_000]).unwrap();
        std::fs::write(d.join("a/b/two.bin"), vec![1u8; 5_000]).unwrap();
        std::fs::write(d.join("top.txt"), b"hi").unwrap();
        let never = AtomicBool::new(false);
        let kids = measure_children(d, &never);
        assert_eq!(kids["a"].logical, 15_000);
        assert!(kids["a"].on_disk >= 15_000);
        assert!(!kids["a"].partial);
        assert_eq!(kids["top.txt"].logical, 2);
        assert_eq!(measure(d, &never).logical, 15_002);
    }
}
