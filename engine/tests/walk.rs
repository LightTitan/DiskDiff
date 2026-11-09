//! The walk's safety rules on a real file system: links are never followed,
//! hard links count once, special files are never opened, unreadable folders
//! don't stop a scan.

use diskdiff_engine::{Progress, Store, scan};
use std::fs;
use std::path::Path;

fn write(p: &Path, n: usize) {
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, vec![7u8; n]).unwrap();
}

/// Scans `root` into a fresh store and returns (size, files, unreadable).
fn scan_of(root: &Path) -> (u64, u64, u64) {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(&tmp.path().join("db.sqlite")).unwrap();
    scan::scan(&store, root, false, &Progress::default()).unwrap();
    let s = &store.scans().unwrap()[0];
    (s.size, s.files, s.unreadable)
}

#[cfg(unix)]
#[test]
fn symlinks_are_never_followed() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("Project");
    let outside = tmp.path().join("Photos");
    write(&root.join("app/main.bin"), 40_000);
    write(&outside.join("big.jpg"), 400_000);
    let (alone, files, _) = scan_of(&root);
    assert_eq!(files, 1);
    // A loop back to the root, a link to a folder outside and one to a file.
    std::os::unix::fs::symlink(&root, root.join("app/loop")).unwrap();
    std::os::unix::fs::symlink(&outside, root.join("photos")).unwrap();
    std::os::unix::fs::symlink(outside.join("big.jpg"), root.join("big.jpg")).unwrap();
    std::os::unix::fs::symlink(tmp.path().join("missing"), root.join("dangling")).unwrap();
    assert_eq!(scan_of(&root), (alone, 1, 0));
}

#[cfg(unix)]
#[test]
fn hard_links_count_once() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("Project");
    write(&root.join("a/film.mp4"), 300_000);
    let (one, _, _) = scan_of(&root);
    fs::create_dir_all(root.join("b")).unwrap();
    fs::hard_link(root.join("a/film.mp4"), root.join("b/film.mp4")).unwrap();
    fs::hard_link(root.join("a/film.mp4"), root.join("b/copy.mp4")).unwrap();
    let (size, files, _) = scan_of(&root);
    assert_eq!(files, 3);
    // Split evenly over the links; integer division may lose a byte or two.
    assert!(one - size < 3, "one copy {one}, with links {size}");
}

/// A FIFO would block whoever opens it, and a socket can't be read; the walk
/// must only see their directory entries and move on.
#[cfg(unix)]
#[test]
fn fifos_and_sockets_are_not_opened() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("Project");
    write(&root.join("notes.txt"), 1000);
    let status = std::process::Command::new("mkfifo").arg(root.join("pipe")).status().unwrap();
    assert!(status.success());
    let _sock = std::os::unix::net::UnixListener::bind(root.join("sock")).unwrap();
    let (_, files, unreadable) = scan_of(&root);
    assert_eq!((files, unreadable), (1, 0));
}

#[cfg(unix)]
#[test]
fn unreadable_folder_is_marked_not_fatal() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("Project");
    write(&root.join("open/a.bin"), 10_000);
    write(&root.join("locked/b.bin"), 10_000);
    fs::set_permissions(root.join("locked"), fs::Permissions::from_mode(0o000)).unwrap();
    // Root ignores permissions (e.g. in a container); nothing to check then.
    let enforced = fs::read_dir(root.join("locked")).is_err();
    let (size, files, unreadable) = scan_of(&root);
    fs::set_permissions(root.join("locked"), fs::Permissions::from_mode(0o755)).unwrap();
    if enforced {
        assert_eq!((files, unreadable), (1, 1));
        assert!(size >= 10_000);
    }
}

/// Linux file names are bytes; one that isn't UTF-8 must still be walked
/// (macOS file systems refuse such names, Windows names are UTF-16).
#[cfg(target_os = "linux")]
#[test]
fn non_utf8_folder_names_are_walked() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("Project");
    let odd = root.join(OsStr::from_bytes(b"caf\xe9"));
    write(&odd.join("film.mp4"), 50_000);
    let (size, files, unreadable) = scan_of(&root);
    assert_eq!((files, unreadable), (1, 0));
    assert!(size >= 50_000);
}

/// Junctions (like the legacy `Documents and Settings` → `Users`) would loop
/// or count a folder twice if followed.
#[cfg(windows)]
#[test]
fn junctions_and_directory_links_are_never_followed() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("Project");
    let outside = tmp.path().join("Photos");
    write(&root.join("app/main.bin"), 40_000);
    write(&outside.join("big.jpg"), 400_000);
    let (alone, files, _) = scan_of(&root);
    assert_eq!(files, 1);
    // `mklink /J` needs no special privilege, unlike symlinks.
    let junction = |link: &Path, target: &Path| {
        let ok = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(link)
            .arg(target)
            .status()
            .unwrap()
            .success();
        assert!(ok);
    };
    // Joined part by part: a `/` inside the path would reach `mklink` as an
    // option ("/loop").
    junction(&root.join("app").join("loop"), &root);
    junction(&root.join("photos"), &outside);
    assert_eq!(scan_of(&root), (alone, 1, 0));
}

#[cfg(windows)]
#[test]
fn windows_link_counts_when_asked() {
    use diskdiff_engine::listing::{ListOptions, list_dir, list_dir_with};
    let tmp = tempfile::tempdir().unwrap();
    write(&tmp.path().join("film.mp4"), 300_000);
    fs::hard_link(tmp.path().join("film.mp4"), tmp.path().join("copy.mp4")).unwrap();
    let exact = list_dir_with(tmp.path(), &ListOptions { link_counts: true, ..Default::default() }).unwrap();
    assert!(exact.iter().all(|e| e.nlink == 2), "{exact:?}");
    assert_eq!(exact[0].ino, exact[1].ino);
    // The cheap listing doesn't open files and assumes one link.
    assert!(list_dir(tmp.path()).unwrap().iter().all(|e| e.nlink == 1));
}

#[test]
fn skipped_names_are_left_out_of_a_listing() {
    use diskdiff_engine::listing::{ListOptions, list_dir_with};
    let tmp = tempfile::tempdir().unwrap();
    write(&tmp.path().join("mnt/share/a.bin"), 10);
    write(&tmp.path().join("Downloads/film.mp4"), 10);
    let skip = vec![std::ffi::OsString::from("mnt")];
    let names: Vec<String> = list_dir_with(tmp.path(), &ListOptions { skip: &skip, ..Default::default() })
        .unwrap()
        .into_iter()
        .map(|e| e.name)
        .collect();
    assert_eq!(names, ["Downloads"]);
}

/// macOS reports a mount point with the parent's device id, so only its mount
/// status keeps a disk image (or another disk, or a network share) mounted
/// inside the scanned folder from being walked.
#[cfg(target_os = "macos")]
#[test]
fn mounted_volumes_inside_the_folder_are_never_entered() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("Project");
    write(&root.join("app/main.bin"), 40_000);
    let (alone, files, _) = scan_of(&root);
    assert_eq!(files, 1);

    let image = tmp.path().join("Photos.dmg");
    let mount = root.join("Photos");
    let hdiutil = |args: &[&std::ffi::OsStr]| std::process::Command::new("hdiutil").args(args).output();
    let created = hdiutil(&["create".as_ref(), "-size".as_ref(), "4m".as_ref(), "-fs".as_ref(), "APFS".as_ref(), "-volname".as_ref(), "Photos".as_ref(), image.as_os_str()]);
    if !created.as_ref().is_ok_and(|o| o.status.success()) {
        eprintln!("skipped: hdiutil can't create disk images here ({created:?})");
        return;
    }
    fs::create_dir_all(&mount).unwrap();
    let attached = hdiutil(&["attach".as_ref(), "-nobrowse".as_ref(), "-mountpoint".as_ref(), mount.as_os_str(), image.as_os_str()]);
    if !attached.as_ref().is_ok_and(|o| o.status.success()) {
        eprintln!("skipped: hdiutil can't attach disk images here ({attached:?})");
        return;
    }
    struct Detach<'a>(&'a Path);
    impl Drop for Detach<'_> {
        fn drop(&mut self) {
            let _ = std::process::Command::new("hdiutil").args(["detach", "-force"]).arg(self.0).output();
        }
    }
    let _detach = Detach(&mount);
    write(&mount.join("big.jpg"), 400_000);
    write(&mount.join("raw/img.raw"), 400_000);
    let (size, files, unreadable) = scan_of(&root);
    assert_eq!((files, unreadable), (1, 0), "the image's files were counted");
    assert!(size < alone + 100_000, "the image's bytes were counted: {size} vs {alone}");
}

/// A folder whose `lstat` fails (here: its parent can be listed but not
/// searched) has an unknown volume: it's marked unreadable, not entered.
#[cfg(target_os = "linux")]
#[test]
fn a_folder_that_cant_be_stated_is_unreadable() {
    use std::os::unix::fs::PermissionsExt;
    if unsafe { libc::geteuid() } == 0 {
        eprintln!("skipped: root can stat anything");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("Project");
    write(&root.join("app/main.bin"), 40_000);
    let locked = root.join("locked");
    write(&locked.join("inner/b.bin"), 40_000);
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o444)).unwrap();
    let (_, files, unreadable) = scan_of(&root);
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(files, 1);
    assert_eq!(unreadable, 1, "locked/inner should be marked unreadable");
}
