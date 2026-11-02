//! Builds a demo store with a few weeks of fake history:
//! cargo run -p diskdiff-engine --example demo -- <work-dir>
//! Writes <work-dir>/tree (the scanned folder) and <work-dir>/demo.db.
use diskdiff_engine::{Progress, Store, scan};
use std::fs;
use std::path::Path;

fn put(p: &Path, n: usize) {
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, vec![1u8; n]).unwrap();
}

fn main() -> anyhow::Result<()> {
    let work = std::path::PathBuf::from(std::env::args().nth(1).expect("work dir"));
    let tree = work.join("tree");
    let _ = fs::remove_dir_all(&tree);
    for f in ["demo.db", "demo.db-wal", "demo.db-shm"] {
        let _ = fs::remove_file(work.join(f));
    }
    let names = ["Documents", "Downloads", "Library", "Movies", "Music", "Pictures", "Projects", "node_modules"];
    for (i, n) in names.iter().enumerate() {
        for j in 0..(10 + i * 3) {
            put(&tree.join(n).join(format!("sub{}", j % 4)).join(format!("file-{j:03}.dat")), 20_000 * (j + 1) * (i + 1));
        }
    }
    put(&tree.join("Library/Caches/com.apple.Safari/Cache.db"), 3_000_000);
    put(&tree.join("Projects/app/target/debug/app"), 9_000_000);
    put(&tree.join("Projects/web/node_modules/react/index.js"), 400_000);
    let store = Store::open(&work.join("demo.db"))?;
    let day = 86_400i64;
    let start = 1_790_000_000i64;
    let mut free = 210_000_000_000u64;
    for k in 0..9 {
        match k {
            1 => put(&tree.join("Downloads/Xcode_17.xip"), 12_000_000),
            2 => put(&tree.join("Library/Caches/com.apple.Safari/Cache.db"), 7_500_000),
            3 => fs::remove_dir_all(tree.join("Movies/sub1"))?,
            4 => put(&tree.join("Movies/Export Final.mov"), 18_000_000),
            5 => fs::remove_file(tree.join("Downloads/Xcode_17.xip"))?,
            6 => put(&tree.join("Projects/app/target/debug/app"), 26_000_000),
            7 => put(&tree.join("Pictures/sub2/IMG_4410.HEIC"), 5_000_000),
            8 => put(&tree.join("Documents/Taxes 2026.pdf"), 800_000),
            _ => {}
        }
        let out = scan::scan(&store, &tree, false, &Progress::default())?;
        // Fake the timeline and the volume figures.
        free = free.wrapping_sub(((k * 7919) % 11) as u64 * 1_300_000_000).wrapping_add(if k == 5 { 9_000_000_000 } else { 0 });
        let conn = rusqlite::Connection::open(work.join("demo.db"))?;
        let t = start + k as i64 * 4 * day + (k as i64 % 3) * 3600 * 5;
        conn.execute(
            "UPDATE scans SET started_at = ?2, finished_at = ?2 + 40, disk_total = 494384795648, disk_free = ?3 WHERE id = ?1",
            rusqlite::params![out.scan_id, t, free as i64],
        )?;
    }
    println!("{}", tree.canonicalize()?.display());
    Ok(())
}
