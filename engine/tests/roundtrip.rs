use diskdiff_engine::{Progress, Status, Store, query, scan};
use std::fs;
use std::path::Path;
use std::sync::atomic::Ordering::Relaxed;

fn write(p: &Path, n: usize) {
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, vec![7u8; n]).unwrap();
}

#[test]
fn scan_dedup_diff_and_delete() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("root");
    for i in 0..20 {
        write(&root.join(format!("stable/d{i}/f.bin")), 5000);
    }
    write(&root.join("grow/a.bin"), 10_000);
    write(&root.join("gone/b.bin"), 20_000);
    let store = Store::open(&tmp.path().join("db.sqlite")).unwrap();

    let s1 = scan::scan(&store, &root, false, &Progress::default()).unwrap();
    let first_nodes = store.stats().unwrap().nodes;

    write(&root.join("grow/a.bin"), 50_000);
    write(&root.join("grow/new.bin"), 8000);
    fs::remove_dir_all(root.join("gone")).unwrap();
    let s2 = scan::scan(&store, &root, false, &Progress::default()).unwrap();
    // Only root and grow/ changed; the 21 stable dirs are shared.
    assert_eq!(s2.nodes_added, 2, "{s2:?}");
    assert!(store.stats().unwrap().nodes == first_nodes + 2);

    let scans = store.scans().unwrap();
    let (a, b) = (&scans[1], &scans[0]);
    let root = root.canonicalize().unwrap();
    let l = query::list(&store, a, Some(b), &root).unwrap();
    let get = |n: &str| l.items.iter().find(|i| &*i.name == n).unwrap().clone();
    assert_eq!(get("stable").status, Status::Same);
    assert_eq!(get("grow").status, Status::Changed);
    assert!(get("grow").delta() > 0);
    assert_eq!(get("gone").status, Status::Removed);

    let ch = query::changes(&store, a, b, &root, 10).unwrap();
    assert_eq!(ch.len(), 3, "{ch:?}");
    assert!(ch.iter().any(|c| c.path.ends_with("gone") && c.status == Status::Removed && c.is_dir));

    // Deleting the first scan frees only what it owned: old root, old grow/, gone/.
    assert!(store.exclusive_bytes(s1.scan_id).unwrap() > 0);
    let freed = store.delete_scans(&[s1.scan_id]).unwrap();
    assert_eq!(freed, 3);
    let l2 = query::list(&store, &store.scans().unwrap()[0], None, &root).unwrap();
    assert_eq!(l2.items.len(), 2);
    // Deleting the last scan empties the store.
    store.delete_scans(&[s2.scan_id]).unwrap();
    assert_eq!(store.stats().unwrap().nodes, 0);
}

#[test]
fn cancelled_scan_leaves_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    write(&tmp.path().join("r/x/y.bin"), 100);
    let store = Store::open(&tmp.path().join("db.sqlite")).unwrap();
    let p = Progress::default();
    p.cancel();
    assert!(scan::scan(&store, &tmp.path().join("r"), false, &p).is_err());
    assert_eq!(store.stats().unwrap().nodes, 0);
    assert!(store.scans().unwrap().is_empty());
}

#[test]
fn big_folder_rescan_writes_little() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("big");
    for i in 0..6000 {
        write(&root.join(format!("f{i:05}.log")), 10);
    }
    let store = Store::open(&tmp.path().join("db.sqlite")).unwrap();
    let a = scan::scan(&store, &root, false, &Progress::default()).unwrap();
    write(&root.join("f03000.log"), 50_000);
    let b = scan::scan(&store, &root, false, &Progress::default()).unwrap();
    assert!(b.bytes_added * 5 < a.bytes_added, "first {} then {}", a.bytes_added, b.bytes_added);
    let scans = store.scans().unwrap();
    let root = root.canonicalize().unwrap();
    let l = query::list(&store, &scans[1], Some(&scans[0]), &root).unwrap();
    assert_eq!(l.items.len(), 6000);
    let changed: Vec<_> = l.items.iter().filter(|i| i.status == Status::Changed).collect();
    assert_eq!(changed.len(), 1);
    assert_eq!(&*changed[0].name, "f03000.log");
    store.delete_scans(&[a.scan_id, b.scan_id]).unwrap();
    assert_eq!(store.stats().unwrap().nodes, 0);
}

#[test]
fn live_tree_ends_at_the_saved_sizes() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("root");
    write(&root.join("Photos/2024/a/b/deep.jpg"), 30_000);
    write(&root.join("Photos/2024/top.jpg"), 4000);
    write(&root.join("Downloads/film.mp4"), 90_000);
    write(&root.join("notes.txt"), 500);
    let store = Store::open(&tmp.path().join("db.sqlite")).unwrap();
    let progress = Progress::default();
    scan::scan(&store, &root, false, &progress).unwrap();

    let scan = &store.scans().unwrap()[0];
    let live = &progress.live;
    assert!(live.done.load(Relaxed));
    assert_eq!(live.bytes.load(Relaxed), scan.size);
    let files = &live.contents.get().unwrap().files;
    assert_eq!(files.iter().map(|(n, _)| &**n).collect::<Vec<_>>(), ["notes.txt"]);

    let root = root.canonicalize().unwrap();
    for item in query::list(&store, scan, None, &root).unwrap().items.iter().filter(|i| i.is_dir) {
        let d = live.find(Path::new(&*item.name)).unwrap();
        assert!(d.done.load(Relaxed));
        assert_eq!(d.bytes.load(Relaxed), item.size, "{}", item.name);
    }
    // Sizes reach the deepest live level; below it there are no listings.
    let level3 = live.find(Path::new("Photos/2024/a")).unwrap();
    assert!(level3.bytes.load(Relaxed) > 0 && level3.contents.get().is_none());
    assert!(live.find(Path::new("Photos/2024/a/b")).is_none());
}
