//! The scanner: a parallel, bottom-up walk that streams finished directory
//! nodes to a writer thread.
//!
//! Each directory is listed, its sub-directories are scanned in parallel on a
//! work-stealing pool, and once they return the directory's own node is encoded
//! and sent to the writer. Only the directories currently being walked are held
//! in memory, so a full-disk scan needs a few megabytes rather than a copy of
//! the whole tree.

use crate::listing::{Kind, RawEntry, list_dir};
use crate::node::{DirChild, FLAG_CLOUD, FLAG_PACKAGE, FLAG_UNREADABLE, FileChild, Node, Summary};
use crate::platform;
use crate::store::{LargeFile, NewScan, Store};
use anyhow::{Result, bail};
use parking_lot::Mutex;
use rayon::prelude::*;
use std::collections::{BinaryHeap, HashSet};
use std::cmp::Reverse;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
use std::time::{SystemTime, UNIX_EPOCH};

const LARGEST_KEEP: usize = 1000;

/// Live counters a UI can poll while a scan runs.
#[derive(Default)]
pub struct Progress {
    pub files: AtomicU64,
    pub dirs: AtomicU64,
    pub bytes: AtomicU64,
    pub unreadable: AtomicU64,
    /// Total bytes in use on the volume, the denominator for a progress bar.
    pub expected: AtomicU64,
    pub current: Mutex<String>,
    cancel: AtomicBool,
    /// Set once the walk is over and the store is committing.
    pub saving: AtomicBool,
}

impl Progress {
    pub fn cancel(&self) {
        self.cancel.store(true, Relaxed);
    }
    pub fn cancelled(&self) -> bool {
        self.cancel.load(Relaxed)
    }
}

#[derive(Debug)]
pub struct ScanOutcome {
    pub scan_id: i64,
    pub nodes_added: u64,
    pub bytes_added: u64,
    pub seconds: f64,
}

struct Ctx<'a> {
    root_dev: u64,
    allowed_devs: Vec<u64>,
    skip: &'a [PathBuf],
    progress: &'a Progress,
    tx: crossbeam_channel::Sender<Vec<crate::node::Piece>>,
    /// Directories already walked, by (device, inode), sharded to keep lock
    /// contention low. Guards against loops through odd mounts.
    seen_dirs: Vec<Mutex<HashSet<(u64, u64)>>>,
    largest: Mutex<BinaryHeap<Reverse<(u64, String)>>>,
    largest_floor: AtomicU64,
}

/// Scans `root` and saves it as a new scan. Blocks until done; run it on a
/// background thread and poll `progress`.
pub fn scan(store: &Store, root: &Path, temporary: bool, progress: &Progress) -> Result<ScanOutcome> {
    let t0 = std::time::Instant::now();
    let started_at = now();
    let root = root.canonicalize().unwrap_or_else(|_| root.to_owned());
    platform::never_fetch_cloud_files();
    let plan = platform::plan(&root);
    let disk = platform::disk_space(&root).unwrap_or_default();
    progress.expected.store(disk.total.saturating_sub(disk.free), Relaxed);

    let (tx, rx) = crossbeam_channel::bounded::<Vec<crate::node::Piece>>(4096);
    let mut writer = store.writer()?;
    let write_thread = std::thread::Builder::new().name("scan-writer".into()).spawn(move || {
        let mut res = Ok(());
        for pieces in rx {
            for p in pieces {
                if res.is_ok() {
                    res = writer.put(&p.hash, &p.raw);
                }
            }
        }
        res.map(|_| writer)
    })?;

    let ctx = Ctx {
        root_dev: platform::dev_of(&root),
        allowed_devs: plan.extra_devs.clone(),
        skip: &plan.skip,
        progress,
        tx,
        seen_dirs: (0..64).map(|_| Mutex::new(HashSet::new())).collect(),
        largest: Mutex::new(BinaryHeap::new()),
        largest_floor: AtomicU64::new(0),
    };
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(std::thread::available_parallelism().map_or(8, |n| n.get()).clamp(4, 12) + 4)
        .stack_size(8 << 20)
        .thread_name(|i| format!("scan-{i}"))
        .build()?;
    let summary = pool.install(|| scan_dir(&ctx, &root, String::new()));
    let Ctx { tx, largest, .. } = ctx;
    drop(tx);
    let writer = write_thread.join().map_err(|_| anyhow::anyhow!("writer thread panicked"))??;
    if progress.cancelled() {
        bail!("scan cancelled");
    }
    progress.saving.store(true, Relaxed);
    let mut largest: Vec<LargeFile> =
        largest.into_inner().into_iter().map(|Reverse((size, path))| LargeFile { path, size }).collect();
    largest.sort_by(|a, b| b.size.cmp(&a.size));
    let (nodes_added, bytes_added) = (writer.inserted, writer.added);
    let scan_id = writer.finish(NewScan {
        root: root.to_string_lossy().into_owned(),
        started_at,
        finished_at: now(),
        summary,
        disk_total: disk.total,
        disk_free: disk.free,
        unreadable: progress.unreadable.load(Relaxed),
        full_access: platform::has_full_access(),
        temporary,
        largest,
    })?;
    Ok(ScanOutcome { scan_id, nodes_added, bytes_added, seconds: t0.elapsed().as_secs_f64() })
}

fn scan_dir(ctx: &Ctx, path: &Path, rel: String) -> Summary {
    let mut node = Node::default();
    if ctx.progress.cancelled() {
        return finish(ctx, node);
    }
    let entries = match list_dir(path) {
        Ok(e) => e,
        Err(_) => {
            ctx.progress.unreadable.fetch_add(1, Relaxed);
            node.flags |= FLAG_UNREADABLE;
            return finish(ctx, node);
        }
    };
    ctx.progress.dirs.fetch_add(1, Relaxed);
    if let Some(mut cur) = ctx.progress.current.try_lock() {
        cur.clear();
        cur.push_str(&path.to_string_lossy());
    }

    let mut subdirs: Vec<RawEntry> = Vec::new();
    let mut bytes = 0u64;
    for e in entries {
        match e.kind {
            Kind::Dir => subdirs.push(e),
            Kind::File => {
                // A hard-linked file's bytes are split evenly over its links,
                // so the attribution is the same every scan whatever order the
                // walk visits them in.
                let size = if e.nlink > 1 { e.size / u64::from(e.nlink) } else { e.size };
                bytes += size;
                if size > ctx.largest_floor.load(Relaxed) {
                    note_large(ctx, &rel, &e.name, size);
                }
                node.files.push(FileChild { name: e.name.into(), size });
            }
            Kind::Other => {}
        }
    }
    ctx.progress.files.fetch_add(node.files.len() as u64, Relaxed);
    ctx.progress.bytes.fetch_add(bytes, Relaxed);

    node.dirs = subdirs
        .into_par_iter()
        .filter_map(|e| {
            if e.dev != 0 && e.dev != ctx.root_dev && !ctx.allowed_devs.contains(&e.dev) {
                return None; // another volume mounted here
            }
            let child = path.join(&e.name);
            if ctx.skip.iter().any(|s| s == &child) {
                return None;
            }
            if e.ino != 0 && !ctx.seen_dirs[(e.ino % 64) as usize].lock().insert((e.dev, e.ino)) {
                return None;
            }
            let child_rel = if rel.is_empty() { e.name.clone() } else { format!("{rel}/{}", e.name) };
            // Never open a cloud placeholder: that would download its listing.
            // Its contents are not on this disk, so it takes ~0 bytes here.
            let s = if e.cloud {
                finish(ctx, Node { flags: FLAG_CLOUD, ..Default::default() })
            } else {
                scan_dir(ctx, &child, child_rel)
            };
            let mut flags = s.flags;
            if platform::is_package(&e.name) {
                flags |= FLAG_PACKAGE;
            }
            Some(DirChild { name: e.name.into(), hash: s.hash, size: s.size, files: s.files, dirs: s.dirs, flags })
        })
        .collect();
    finish(ctx, node)
}

fn finish(ctx: &Ctx, mut node: Node) -> Summary {
    node.sort();
    let (pieces, summary) = crate::node::chunk(&node);
    // A send only fails when the writer stopped on an error; the scan result
    // is discarded then anyway.
    let _ = ctx.tx.send(pieces);
    summary
}

fn note_large(ctx: &Ctx, rel: &str, name: &str, size: u64) {
    let mut heap = ctx.largest.lock();
    let path = if rel.is_empty() { name.to_owned() } else { format!("{rel}/{name}") };
    heap.push(Reverse((size, path)));
    if heap.len() > LARGEST_KEEP {
        heap.pop();
        if let Some(Reverse((min, _))) = heap.peek() {
            ctx.largest_floor.store(*min, Relaxed);
        }
    }
}

fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

/// Runs a scan on its own thread.
pub fn spawn_scan(
    store: Arc<Store>,
    root: PathBuf,
    temporary: bool,
    progress: Arc<Progress>,
) -> std::thread::JoinHandle<Result<ScanOutcome>> {
    std::thread::Builder::new()
        .name("scan".into())
        .spawn(move || scan(&store, &root, temporary, &progress))
        .expect("spawn scan thread")
}
