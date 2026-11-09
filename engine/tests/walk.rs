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
    junction(&root.join("app/loop"), &root);
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
