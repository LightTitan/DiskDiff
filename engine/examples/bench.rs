//! cargo run --release -p diskdiff-engine --example bench -- <dir> [db]
use diskdiff_engine::{Progress, Store, scan};
use std::sync::Arc;

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let root = args.next().unwrap_or_else(|| std::env::var("HOME").unwrap());
    let db = args.next().unwrap_or_else(|| "/tmp/diskdiff-bench.db".into());
    let store = Arc::new(Store::open(db.as_ref())?);
    let progress = Arc::new(Progress::default());
    let h = scan::spawn_scan(store.clone(), root.into(), false, progress.clone());
    while !h.is_finished() {
        std::thread::sleep(std::time::Duration::from_millis(500));
        eprintln!(
            "{} files  {} dirs  {:.1} GB",
            progress.files.load(std::sync::atomic::Ordering::Relaxed),
            progress.dirs.load(std::sync::atomic::Ordering::Relaxed),
            progress.bytes.load(std::sync::atomic::Ordering::Relaxed) as f64 / 1e9
        );
    }
    let out = h.join().unwrap()?;
    let st = store.stats()?;
    println!("{out:?}");
    println!("store: {} nodes, {:.1} MB node data, {:.1} MB file", st.nodes, st.node_bytes as f64 / 1e6, st.file_bytes as f64 / 1e6);
    let s = store.scans()?.pop().unwrap();
    println!("scan: {} files, {} dirs, {:.2} GB, unreadable {}", s.files, s.dirs, s.size as f64 / 1e9, s.unreadable);
    Ok(())
}
