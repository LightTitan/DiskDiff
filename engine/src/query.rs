//! Reading scans back: folder listings with deltas, and tree-wide changes.
//!
//! Paths are absolute, so two scans with different roots (a full-disk scan and
//! a scan of ~/Documents) still compare wherever they overlap. Subtrees whose
//! hashes match in both scans are identical and are never opened.

use crate::node::{FLAG_CLOUD, FLAG_PACKAGE, FLAG_UNREADABLE, Hash, Node};
use crate::store::{ScanInfo, Store};
use anyhow::Result;
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    /// Same size in both scans.
    Same,
    Changed,
    /// Only in the current scan.
    Added,
    /// Only in the compare scan; `size` is 0.
    Removed,
    /// No compare scan covers this place.
    NoHistory,
    /// The folder could not be read in one of the scans, so its change is unknown.
    Unreadable,
}

#[derive(Clone, Debug)]
pub struct Item {
    pub name: Arc<str>,
    pub is_dir: bool,
    pub package: bool,
    pub unreadable: bool,
    /// A cloud placeholder folder that was not opened.
    pub cloud: bool,
    /// Size in the current scan (0 when removed).
    pub size: u64,
    /// Size in the compare scan, when it has the item.
    pub prev: Option<u64>,
    pub files: u64,
    pub dirs: u64,
    pub status: Status,
    /// Not a real entry: the bytes a scan counted in this folder without
    /// listing the files.
    pub placeholder: bool,
    /// The part of the delta that was moved here from elsewhere (+) or away
    /// (−) rather than added or freed. Set by [`apply_moves`].
    pub moved_bytes: i64,
    /// The item itself was moved: where it came from or went.
    pub moved: Option<Moved>,
}

#[derive(Clone, Debug)]
pub struct Moved {
    /// The other end, absolute.
    pub other: PathBuf,
    /// How to name it: its folder ("Downloads"), or its old or new name when
    /// renamed in place. Empty inside a moved folder, whose row says it.
    pub label: Arc<str>,
    /// True at the new place, false at the old one.
    pub arrived: bool,
}

/// Display name of the placeholder row.
pub const UNITEMIZED: &str = "Files not itemized";

impl Item {
    pub fn delta(&self) -> i64 {
        self.size as i64 - self.prev.unwrap_or(0) as i64
    }
    pub fn has_delta(&self) -> bool {
        !matches!(self.status, Status::NoHistory | Status::Unreadable)
    }
    /// The delta without moved bytes: what was really added or freed.
    pub fn change(&self) -> i64 {
        self.delta() - self.moved_bytes
    }
}

/// Marks the items of folder `abs` with the moves between the two scans.
pub fn apply_moves(items: &mut [Item], abs: &Path, moves: &crate::moves::Moves) {
    let net = moves.net_by_child(abs);
    // Inside a folder that was moved away, everything left with it.
    let away = moves.after(abs);
    for it in items {
        let path = abs.join(&*it.name);
        it.moved = None;
        if let Some(to) = away.as_ref().filter(|_| it.status == Status::Removed) {
            it.moved_bytes = it.delta();
            it.moved = Some(Moved { other: to.join(&*it.name), label: Arc::from(""), arrived: false });
            continue;
        }
        it.moved_bytes = if it.has_delta() { net.get(&*it.name).copied().unwrap_or(0) } else { 0 };
        let found = match it.status {
            Status::Added => moves.to(&path).map(|m| (&m.from, true)),
            Status::Removed => moves.from(&path).map(|m| (&m.to, false)),
            _ => None,
        };
        if let Some((other, arrived)) = found {
            it.moved_bytes = it.delta();
            it.moved = Some(Moved { other: other.clone(), label: Arc::from(crate::moves::label(other, &path)), arrived });
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Listing {
    pub items: Vec<Item>,
    /// Totals of the folder itself.
    pub size: u64,
    pub prev: Option<u64>,
    pub exists: bool,
    pub existed: bool,
}

/// Path of `abs` inside `scan`, `""` for its root, `None` if outside.
pub fn rel_in(scan: &ScanInfo, abs: &Path) -> Option<String> {
    let rel = abs.strip_prefix(&scan.root).ok()?;
    Some(rel.components().map(|c| c.as_os_str().to_string_lossy()).collect::<Vec<_>>().join("/"))
}

fn open(store: &Store, scan: Option<&ScanInfo>, abs: &Path) -> Result<Option<(Hash, Arc<Node>)>> {
    let Some(scan) = scan else { return Ok(None) };
    let Some(rel) = rel_in(scan, abs) else { return Ok(None) };
    store.resolve(&scan.root_hash, &rel)
}

/// Lists folder `abs` in `cur`, with deltas against `cmp`. Items only in
/// `cmp` are included as [`Status::Removed`]. Unsorted.
pub fn list(store: &Store, cur: &ScanInfo, cmp: Option<&ScanInfo>, abs: &Path) -> Result<Listing> {
    list_against(store, cur, cmp, abs, abs)
}

/// Like [`list`], comparing with folder `prev_abs` of `cmp`: where a folder
/// that was moved used to be.
pub fn list_against(store: &Store, cur: &ScanInfo, cmp: Option<&ScanInfo>, abs: &Path, prev_abs: &Path) -> Result<Listing> {
    let a = open(store, Some(cur), abs)?;
    let b = open(store, cmp, prev_abs)?;
    let history = b.is_some() || cmp.is_some_and(|c| rel_in(c, prev_abs).is_some());
    let mut out = Listing { exists: a.is_some(), existed: b.is_some(), ..Default::default() };
    let empty = Arc::new(Node::default());
    let an = a.as_ref().map_or(&empty, |x| &x.1);
    let bn = b.as_ref().map(|x| &x.1);

    for d in &an.dirs {
        let prev = bn.and_then(|n| n.dir(&d.name));
        out.items.push(Item {
            name: Arc::from(&*d.name),
            is_dir: true,
            package: d.flags & FLAG_PACKAGE != 0,
            unreadable: d.flags & FLAG_UNREADABLE != 0,
            cloud: d.flags & FLAG_CLOUD != 0,
            size: d.size,
            prev: prev.map(|p| p.size),
            files: d.files,
            dirs: d.dirs,
            status: if history && (d.flags | prev.map_or(0, |p| p.flags)) & FLAG_UNREADABLE != 0 {
                Status::Unreadable
            } else {
                status(history, Some(d.size), prev.map(|p| p.size))
            },
            placeholder: false,
            moved_bytes: 0,
            moved: None,
        });
    }
    for f in &an.files {
        let prev = bn.and_then(|n| n.file(&f.name));
        out.items.push(Item {
            name: Arc::from(&*f.name),
            is_dir: false,
            package: false,
            unreadable: false,
            cloud: false,
            size: f.size,
            prev: prev.map(|p| p.size),
            files: 1,
            dirs: 0,
            status: status(history, Some(f.size), prev.map(|p| p.size)),
            placeholder: false,
            moved_bytes: 0,
            moved: None,
        });
    }
    if let Some(bn) = bn {
        for d in bn.dirs.iter().filter(|d| an.dir(&d.name).is_none()) {
            out.items.push(Item {
                name: Arc::from(&*d.name),
                is_dir: true,
                package: d.flags & FLAG_PACKAGE != 0,
                unreadable: false,
                cloud: d.flags & FLAG_CLOUD != 0,
                size: 0,
                prev: Some(d.size),
                files: d.files,
                dirs: d.dirs,
                status: Status::Removed,
                placeholder: false,
                moved_bytes: 0,
                moved: None,
            });
        }
        for f in bn.files.iter().filter(|f| an.file(&f.name).is_none()) {
            out.items.push(Item {
                name: Arc::from(&*f.name),
                is_dir: false,
                package: false,
                unreadable: false,
                cloud: false,
                size: 0,
                prev: Some(f.size),
                files: 1,
                dirs: 0,
                status: Status::Removed,
                placeholder: false,
                moved_bytes: 0,
                moved: None,
            });
        }
    }
    let prev_other = bn.map(|n| n.other_size).filter(|&o| o > 0);
    if an.other_size > 0 || prev_other.is_some() {
        out.items.push(Item {
            name: Arc::from(UNITEMIZED),
            is_dir: false,
            package: false,
            unreadable: false,
            cloud: false,
            size: an.other_size,
            prev: prev_other,
            files: an.other_files,
            dirs: 0,
            status: status(history, Some(an.other_size), prev_other),
            placeholder: true,
            moved_bytes: 0,
            moved: None,
        });
    }
    out.size = out.items.iter().map(|i| i.size).sum();
    out.prev = b.as_ref().map(|_| out.items.iter().filter_map(|i| i.prev).sum());
    Ok(out)
}

fn status(history: bool, cur: Option<u64>, prev: Option<u64>) -> Status {
    match (history, cur, prev) {
        (false, ..) => Status::NoHistory,
        (true, Some(_), None) => Status::Added,
        (true, Some(a), Some(b)) if a == b => Status::Same,
        (true, Some(_), Some(_)) => Status::Changed,
        (true, None, _) => Status::Removed,
    }
}

/// Size of `abs` in a scan, if the scan covers it.
pub fn size_at(store: &Store, scan: &ScanInfo, abs: &Path) -> Result<Option<u64>> {
    let Some(rel) = rel_in(scan, abs) else { return Ok(None) };
    if rel.is_empty() {
        return Ok(Some(scan.size));
    }
    let (parent, name) = match rel.rsplit_once('/') {
        Some((p, n)) => (p, n),
        None => ("", rel.as_str()),
    };
    let Some((_, n)) = store.resolve(&scan.root_hash, parent)? else { return Ok(Some(0)) };
    Ok(Some(n.dir(name).map(|d| d.size).or_else(|| n.file(name).map(|f| f.size)).unwrap_or(0)))
}

#[derive(Clone, Debug)]
pub struct Change {
    /// Absolute path.
    pub path: PathBuf,
    pub is_dir: bool,
    pub size: u64,
    pub prev: u64,
    pub status: Status,
    /// The part of the delta moved in from (+) or out to (−) other places.
    pub moved_bytes: i64,
    /// The item itself was moved here from this path.
    pub moved_from: Option<PathBuf>,
}

impl Change {
    pub fn delta(&self) -> i64 {
        self.size as i64 - self.prev as i64
    }
    /// The delta without moved bytes: what was really added or freed.
    pub fn change(&self) -> i64 {
        self.delta() - self.moved_bytes
    }
}

/// The biggest individual changes between two scans under `abs`: files that
/// grew, shrank, appeared or vanished, and folders that appeared or vanished
/// as a whole (reported once, not file by file). Sorted by |delta|.
pub fn changes(store: &Store, cur: &ScanInfo, cmp: &ScanInfo, abs: &Path, limit: usize) -> Result<Vec<Change>> {
    let mut out = Vec::new();
    let (Some(a), Some(b)) = (open(store, Some(cur), abs)?, open(store, Some(cmp), abs)?) else {
        return Ok(out);
    };
    let mut stack = vec![(abs.to_owned(), a.0, b.0)];
    while let Some((path, ha, hb)) = stack.pop() {
        if ha == hb {
            continue;
        }
        let (an, bn) = (store.node(&ha)?, store.node(&hb)?);
        for d in &an.dirs {
            match bn.dir(&d.name) {
                Some(p) if p.hash != d.hash => stack.push((path.join(&*d.name), d.hash, p.hash)),
                Some(_) => {}
                None => out.push(Change {
                    path: path.join(&*d.name),
                    is_dir: true,
                    size: d.size,
                    prev: 0,
                    status: Status::Added, moved_bytes: 0, moved_from: None 
                }),
            }
        }
        for d in bn.dirs.iter().filter(|d| an.dir(&d.name).is_none()) {
            out.push(Change { path: path.join(&*d.name), is_dir: true, size: 0, prev: d.size, status: Status::Removed, moved_bytes: 0, moved_from: None });
        }
        for f in &an.files {
            match bn.file(&f.name) {
                Some(p) if p.size == f.size => {}
                p => out.push(Change {
                    path: path.join(&*f.name),
                    is_dir: false,
                    size: f.size,
                    prev: p.map_or(0, |p| p.size),
                    status: if p.is_some() { Status::Changed } else { Status::Added }, moved_bytes: 0, moved_from: None 
                }),
            }
        }
        for f in bn.files.iter().filter(|f| an.file(&f.name).is_none()) {
            out.push(Change { path: path.join(&*f.name), is_dir: false, size: 0, prev: f.size, status: Status::Removed, moved_bytes: 0, moved_from: None });
        }
        // Keep memory bounded on huge diffs.
        if out.len() > limit * 8 {
            trim(&mut out, limit);
        }
    }
    trim(&mut out, limit);
    Ok(out)
}

fn trim(v: &mut Vec<Change>, limit: usize) {
    v.sort_unstable_by_key(|c| std::cmp::Reverse(c.delta().unsigned_abs()));
    v.truncate(limit);
}

/// Every file under `abs` in one scan, largest first, up to `limit`.
pub fn largest_files(store: &Store, scan: &ScanInfo, abs: &Path, limit: usize) -> Result<Vec<(PathBuf, u64)>> {
    let Some((h, _)) = open(store, Some(scan), abs)? else { return Ok(Vec::new()) };
    let mut heap = std::collections::BinaryHeap::new();
    let mut stack = vec![(abs.to_owned(), h)];
    while let Some((path, h)) = stack.pop() {
        let n = store.node(&h)?;
        for f in &n.files {
            heap.push(std::cmp::Reverse((f.size, path.join(&*f.name))));
            if heap.len() > limit {
                heap.pop();
            }
        }
        let floor = if heap.len() >= limit { heap.peek().map_or(0, |r| r.0.0) } else { 0 };
        for d in &n.dirs {
            // A folder smaller than the current floor cannot contain a winner.
            if d.size > floor {
                stack.push((path.join(&*d.name), d.hash));
            }
        }
    }
    let mut v: Vec<_> = heap.into_iter().map(|r| (r.0.1, r.0.0)).collect();
    v.sort_by(|a, b| b.1.cmp(&a.1));
    Ok(v)
}

/// Churn: the total bytes that changed inside each child folder of `abs`
/// (files grown + shrunk + added + removed, each counted by |delta|). It can be
/// large even when a folder's net change is small. Only subtrees whose hashes
/// differ are opened.
pub fn activity(
    store: &Store,
    cur: &ScanInfo,
    cmp: &ScanInfo,
    abs: &Path,
    cancel: &dyn Fn() -> bool,
) -> Result<std::collections::HashMap<Arc<str>, u64>> {
    let mut out = std::collections::HashMap::new();
    let (Some(a), Some(b)) = (open(store, Some(cur), abs)?, open(store, Some(cmp), abs)?) else {
        return Ok(out);
    };
    for d in &a.1.dirs {
        let Some(p) = b.1.dir(&d.name) else { continue };
        if p.hash == d.hash {
            continue;
        }
        let mut total = 0u64;
        let mut stack = vec![(d.hash, p.hash)];
        while let Some((ha, hb)) = stack.pop() {
            if cancel() {
                return Ok(out);
            }
            let (an, bn) = (store.node(&ha)?, store.node(&hb)?);
            for x in &an.dirs {
                match bn.dir(&x.name) {
                    Some(y) if y.hash != x.hash => stack.push((x.hash, y.hash)),
                    Some(_) => {}
                    None => total += x.size,
                }
            }
            total += bn.dirs.iter().filter(|y| an.dir(&y.name).is_none()).map(|y| y.size).sum::<u64>();
            for f in &an.files {
                total += match bn.file(&f.name) {
                    Some(g) => f.size.abs_diff(g.size),
                    None => f.size,
                };
            }
            total += bn.files.iter().filter(|g| an.file(&g.name).is_none()).map(|g| g.size).sum::<u64>();
        }
        out.insert(Arc::from(&*d.name), total);
    }
    Ok(out)
}
