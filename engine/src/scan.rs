//! The scanner: a parallel, bottom-up walk that streams finished directory
//! nodes to a writer thread.
//!
//! Each directory is listed, its sub-directories are scanned in parallel on a
//! work-stealing pool, and once they return the directory's own node is encoded
//! and sent to the writer. Only the directories currently being walked are held
//! in memory, so a full-disk scan needs a few megabytes rather than a copy of
//! the whole tree.

use crate::listing::{Kind, ListOptions, RawEntry, list_dir_with};
use crate::node::{DirChild, FLAG_CLOUD, FLAG_PACKAGE, FLAG_UNREADABLE, FileChild, Node, Summary};
use crate::platform;
use crate::store::{LargeFile, NewScan, Store};
use anyhow::{Result, bail};
use parking_lot::Mutex;
use rayon::prelude::*;
use std::collections::{BinaryHeap, HashMap, HashSet};
use std::cmp::Reverse;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
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
    /// The top of the tree as walked so far, for showing the scan live.
    pub live: LiveDir,
}

/// How deep the live tree goes: folders down to this many levels below the
/// root keep a running size, and the folders above them their listing.
/// Deeper folders only add to their ancestors' sizes, which keeps the
/// tree small however big the disk is.
pub const LIVE_DEPTH: usize = 3;

/// A folder as the walk has seen it so far.
#[derive(Default)]
pub struct LiveDir {
    pub name: Arc<str>,
    /// Bytes found inside so far; the folder's size once `done`.
    pub bytes: AtomicU64,
    pub done: AtomicBool,
    pub unreadable: AtomicBool,
    pub cloud: bool,
    pub package: bool,
    /// Its entries, once listed. Only folders above [`LIVE_DEPTH`] get them.
    pub contents: OnceLock<LiveContents>,
}

pub struct LiveContents {
    pub dirs: Vec<LiveDir>,
    pub files: Vec<(Arc<str>, u64)>,
}

impl LiveDir {
    /// The folder at `rel` below this one, once the walk has listed it.
    pub fn find(&self, rel: &Path) -> Option<&LiveDir> {
        let mut node = self;
        for part in rel.components() {
            let name = part.as_os_str().to_str()?;
            node = node.contents.get()?.dirs.iter().find(|d| &*d.name == name)?;
        }
        Some(node)
    }
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
    /// Names not to look at, by the folder they're in (from the plan's
    /// absolute skip paths), so a skipped mount point is never even stat'ed.
    skip: HashMap<PathBuf, Vec<OsString>>,
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
    let root = platform::canonical_root(root);
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

    let mut skip: HashMap<PathBuf, Vec<OsString>> = HashMap::new();
    for s in &plan.skip {
        if let (Some(parent), Some(name)) = (s.parent(), s.file_name()) {
            skip.entry(parent.to_owned()).or_default().push(name.to_owned());
        }
    }
    let ctx = Ctx {
        root_dev: platform::dev_of(&root),
        allowed_devs: plan.extra_devs.clone(),
        skip,
        progress,
        tx,
        seen_dirs: (0..64).map(|_| Mutex::new(HashSet::new())).collect(),
        largest: Mutex::new(BinaryHeap::new()),
        largest_floor: AtomicU64::new(0),
    };
    // The root counts as walked, so a bind mount or btrfs mount of the same
    // folder further down isn't walked (and counted) a second time.
    #[cfg(unix)]
    if let Ok(md) = std::fs::metadata(&root) {
        use std::os::unix::fs::MetadataExt;
        ctx.seen_dirs[(md.ino() % 64) as usize].lock().insert((md.dev(), md.ino()));
    }
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(std::thread::available_parallelism().map_or(8, |n| n.get()).clamp(4, 12) + 4)
        .stack_size(8 << 20)
        .thread_name(|i| format!("scan-{i}"))
        .build()?;
    let summary = pool.install(|| scan_dir(&ctx, &root, String::new(), &[&progress.live], 0));
    mark_done(&progress.live, &summary);
    let Ctx { tx, largest, .. } = ctx;
    drop(tx);
    let writer = write_thread.join().map_err(|_| anyhow::anyhow!("writer thread panicked"))??;
    if progress.cancelled() {
        bail!("scan cancelled");
    }
    progress.saving.store(true, Relaxed);
    let mut largest: Vec<LargeFile> =
        largest.into_inner().into_iter().map(|Reverse((size, path))| LargeFile { path, size }).collect();
    largest.sort_by_key(|f| Reverse(f.size));
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

/// `live` holds the live folders this one counts towards: its tracked
/// ancestors, and itself while `depth` is within [`LIVE_DEPTH`].
fn scan_dir<'a>(ctx: &Ctx<'a>, path: &Path, rel: String, live: &[&'a LiveDir], depth: usize) -> Summary {
    let mut node = Node::default();
    if ctx.progress.cancelled() {
        return finish(ctx, node);
    }
    let opts = ListOptions {
        skip: ctx.skip.get(path).map_or(&[], Vec::as_slice),
        link_counts: platform::wants_link_counts(path),
    };
    // A folder that can't be listed (permissions, I/O errors) is kept and
    // marked unreadable; the scan goes on.
    let entries = match list_dir_with(path, &opts) {
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
    for d in live {
        d.bytes.fetch_add(bytes, Relaxed);
    }

    // Filtered up front so a live listing only holds folders that get walked.
    let subdirs: Vec<(RawEntry, PathBuf)> = subdirs
        .into_iter()
        .filter_map(|e| {
            if e.dev != 0 && e.dev != ctx.root_dev && !ctx.allowed_devs.contains(&e.dev) {
                return None; // another volume mounted here
            }
            // The on-disk name, so a non-Unicode folder name still opens.
            let child = path.join(&e.raw_name);
            // Same (device, inode) as a folder already walked: a bind mount,
            // or a loop through an odd mount.
            if e.ino != 0 && !ctx.seen_dirs[(e.ino % 64) as usize].lock().insert((e.dev, e.ino)) {
                return None;
            }
            Some((e, child))
        })
        .collect();
    let contents = (depth < LIVE_DEPTH).then(|| {
        let listing = LiveContents {
            dirs: subdirs
                .iter()
                .map(|(e, _)| LiveDir {
                    name: e.name.as_str().into(),
                    cloud: e.cloud,
                    package: platform::is_package(&e.name),
                    ..Default::default()
                })
                .collect(),
            files: node.files.iter().map(|f| (Arc::from(&*f.name), f.size)).collect(),
        };
        let this = live.last().expect("a folder within LIVE_DEPTH is live");
        let _ = this.contents.set(listing);
        this.contents.get().expect("just set")
    });

    node.dirs = subdirs
        .into_par_iter()
        .enumerate()
        .map(|(i, (e, child))| {
            let child_live = contents.map(|c| &c.dirs[i]);
            let deeper;
            let live = match child_live {
                Some(d) => {
                    deeper = [live, &[d]].concat();
                    &deeper[..]
                }
                None => live,
            };
            let child_rel = if rel.is_empty() { e.name.clone() } else { format!("{rel}/{}", e.name) };
            // Never open a cloud placeholder: that would download its listing.
            // Its contents are not on this disk, so it takes ~0 bytes here.
            let s = if e.cloud {
                finish(ctx, Node { flags: FLAG_CLOUD, ..Default::default() })
            } else {
                scan_dir(ctx, &child, child_rel, live, depth + 1)
            };
            if let Some(d) = child_live {
                mark_done(d, &s);
            }
            let mut flags = s.flags;
            if platform::is_package(&e.name) {
                flags |= FLAG_PACKAGE;
            }
            DirChild { name: e.name.into(), hash: s.hash, size: s.size, files: s.files, dirs: s.dirs, flags }
        })
        .collect();
    finish(ctx, node)
}

fn mark_done(d: &LiveDir, s: &Summary) {
    d.bytes.store(s.size, Relaxed);
    d.unreadable.store(s.flags & FLAG_UNREADABLE != 0, Relaxed);
    d.done.store(true, Relaxed);
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
