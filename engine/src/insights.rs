//! Insights over stored scans: cleanup suggestions, the largest folders, and
//! the biggest changes between two scans.
//!
//! Everything here reads the Merkle tree and leans on its hashes:
//!
//! * two folders with the same hash hold the same names at the same sizes, so
//!   duplicated folders are just repeated hashes;
//! * a folder whose hash at one path is the same in consecutive scans has not
//!   changed in between, so "untouched" needs no per-file history;
//! * a subtree with the same hash in two scans is identical and never opened
//!   when diffing.
//!
//! Walks only enter folders above a size threshold and stop after a fixed
//! number of visits, so they stay fast on a full-disk scan. Nothing here
//! deletes anything.

use crate::node::{DirChild, FLAG_PACKAGE, FLAG_UNREADABLE, Hash, Node};
use crate::query::{Change, Status};
use crate::store::{ScanInfo, Store};
use anyhow::Result;
use std::collections::{BinaryHeap, HashMap};
use std::cmp::Reverse;
use std::path::{Path, PathBuf};
use std::sync::Arc;

const DAY: i64 = 86_400;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FindingKind {
    /// A cache or dependency folder that is rebuilt on demand.
    Regenerable,
    /// The same folder in more than one place.
    Duplicate,
    /// A large folder that has not changed across recent scans.
    Untouched,
}

impl FindingKind {
    pub const ALL: [FindingKind; 3] = [FindingKind::Regenerable, FindingKind::Duplicate, FindingKind::Untouched];

    /// Stable key for persisting hidden suggestions.
    pub fn key(self) -> &'static str {
        match self {
            FindingKind::Regenerable => "junk",
            FindingKind::Duplicate => "duplicate",
            FindingKind::Untouched => "stale",
        }
    }

    /// How sure we are that removing it is fine, in percent; ranks findings.
    fn confidence(self) -> u64 {
        match self {
            FindingKind::Regenerable => 100,
            FindingKind::Duplicate => 85,
            FindingKind::Untouched => 55,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Finding {
    pub kind: FindingKind,
    /// Absolute path. For duplicates, the first copy.
    pub path: PathBuf,
    pub size: u64,
    pub files: u64,
    /// What acting on it could free: the size, or size × (copies − 1).
    pub reclaimable: u64,
    /// Short label ("Package dependencies"); empty for non-cache findings.
    pub category: &'static str,
    /// Why it is safe to remove (regenerable findings).
    pub hint: &'static str,
    /// Every copy of a duplicated folder, sorted, including `path`.
    pub copies: Vec<PathBuf>,
    /// Unix time of the oldest earlier scan in which this folder was
    /// already identical; `None` when it changed since the previous scan.
    pub unchanged_since: Option<i64>,
    /// How many earlier scans in a row had it identical.
    pub unchanged_scans: usize,
    /// It is identical in every earlier scan of the location.
    pub since_first_scan: bool,
    /// For a rolled-up untouched folder: how many untouched subfolders it
    /// holds, and the largest of their names.
    pub untouched_children: usize,
    pub examples: Vec<String>,
}

impl Finding {
    pub fn name(&self) -> String {
        self.path.file_name().map_or_else(|| self.path.display().to_string(), |n| n.to_string_lossy().into_owned())
    }

    fn score(&self) -> u64 {
        self.reclaimable / 100 * self.kind.confidence()
    }

    fn paths(&self) -> &[PathBuf] {
        if self.copies.is_empty() { std::slice::from_ref(&self.path) } else { &self.copies }
    }
}

#[derive(Clone, Debug)]
pub struct Options {
    /// Smallest regenerable or untouched folder worth suggesting.
    pub min_size: u64,
    /// Smallest duplicated folder worth suggesting.
    pub dup_min_size: u64,
    /// Fewer files than this and an equal hash is too likely a coincidence
    /// (one file of the same name and size).
    pub dup_min_files: u64,
    /// How long a folder must have stayed identical to count as untouched.
    pub stale_after: i64,
    /// Earlier scans consulted, newest first.
    pub history_limit: usize,
    pub max_findings: usize,
    pub max_visits: usize,
}

impl Options {
    /// Thresholds scaled to the scan: 1 % of it, between 1 MB and 200 MB, so
    /// a scan of one project folder still gets useful suggestions while a
    /// full disk only hears about things that matter.
    pub fn for_scan(scan: &ScanInfo) -> Options {
        let min_size = (scan.size / 100).clamp(1_000_000, 200_000_000);
        Options {
            min_size,
            dup_min_size: min_size / 2,
            dup_min_files: 8,
            stale_after: 30 * DAY,
            history_limit: 24,
            max_findings: 30,
            max_visits: 200_000,
        }
    }
}

pub(crate) struct JunkRule {
    name: &'static str,
    category: &'static str,
    hint: &'static str,
}

/// Only names whose meaning is unambiguous. Generic names like "build",
/// "dist" and "target" are left out: they are ordinary project folders often
/// enough that flagging them would train people to ignore the screen.
const JUNK: &[JunkRule] = &[
    JunkRule { name: "node_modules", category: "Package dependencies", hint: "Reinstalled by npm install; safe to delete when the project is idle." },
    JunkRule { name: "_cacache", category: "npm cache", hint: "npm’s download cache. Rebuilds itself on the next install." },
    JunkRule { name: ".venv", category: "Python environment", hint: "Recreated from requirements.txt or pyproject.toml." },
    JunkRule { name: "venv", category: "Python environment", hint: "Recreated from requirements.txt or pyproject.toml." },
    JunkRule { name: "__pycache__", category: "Python bytecode", hint: "Regenerated automatically the next time the code runs." },
    JunkRule { name: "deriveddata", category: "Xcode build cache", hint: "Xcode rebuilds this on the next build." },
    JunkRule { name: "ios devicesupport", category: "Xcode device symbols", hint: "Re-downloaded when you next attach that device." },
    JunkRule { name: "watchos devicesupport", category: "Xcode device symbols", hint: "Re-downloaded when you next attach that device." },
    JunkRule { name: ".gradle", category: "Gradle cache", hint: "Re-downloaded on the next Gradle build." },
    JunkRule { name: ".m2", category: "Maven cache", hint: "Re-downloaded on the next Maven build." },
    JunkRule { name: "caches", category: "Cache", hint: "Apps rebuild their caches as you use them." },
    JunkRule { name: ".cache", category: "Cache", hint: "Rebuilt automatically by whatever wrote it." },
    JunkRule { name: ".trash", category: "Trash", hint: "Already deleted — emptying the Trash frees this space." },
    JunkRule { name: "$recycle.bin", category: "Recycle Bin", hint: "Already deleted — emptying it frees this space." },
    JunkRule { name: "cargo", category: "Rust toolchain cache", hint: "Re-downloaded by cargo on the next build." },
];

/// Top-level folders skipped when the scan root is a whole volume: they belong
/// to the OS, package managers or installers, and a suggestion to clear
/// anything there is useless at best.
const PROTECTED: &[&str] = &[
    "System", "Library", "Applications", "private", "usr", "bin", "sbin", "opt", "cores", "dev", "Network",
    "Volumes", "Windows", "Program Files", "Program Files (x86)", "ProgramData",
];

pub(crate) fn junk_rule(name: &str) -> Option<&'static JunkRule> {
    JUNK.iter().find(|r| r.name.eq_ignore_ascii_case(name))
}

/// Packages are one opaque unit: nothing inside a signed bundle or a media
/// library may be cleaned up piecemeal.
pub(crate) fn is_package(d: &DirChild) -> bool {
    d.flags & FLAG_PACKAGE != 0 || crate::platform::is_package(&d.name) || d.name.to_ascii_lowercase().ends_with(".xpc")
}

fn is_volume_root(root: &str) -> bool {
    let p = Path::new(root);
    p.parent().is_none() || (root.len() <= 3 && root.as_bytes().get(1) == Some(&b':'))
}

/// Earlier saved scans of the same root, newest first.
fn history<'a>(scans: &'a [ScanInfo], current: &ScanInfo, limit: usize) -> Vec<&'a ScanInfo> {
    let mut h: Vec<&ScanInfo> = scans
        .iter()
        .filter(|s| {
            s.id != current.id
                && !s.temporary
                && s.root == current.root
                && (s.started_at, s.id) < (current.started_at, current.id)
        })
        .collect();
    h.sort_by_key(|s| Reverse((s.started_at, s.id)));
    h.truncate(limit);
    h
}

struct Candidate {
    child: DirChild,
    path: PathBuf,
    /// This path's hashes in the history, newest first, up to the first scan
    /// that lacks it.
    hist: Vec<Hash>,
    streak: usize,
    stale: bool,
}

/// Cleanup suggestions for `current`, using the other scans of its location
/// (`scans` may hold any scans; only saved ones of the same root are used).
pub fn suggestions(store: &Store, scans: &[ScanInfo], current: &ScanInfo) -> Result<Vec<Finding>> {
    suggestions_with(store, scans, current, &Options::for_scan(current))
}

pub fn suggestions_with(store: &Store, scans: &[ScanInfo], current: &ScanInfo, opts: &Options) -> Result<Vec<Finding>> {
    let hist = history(scans, current, opts.history_limit);
    let volume_root = is_volume_root(&current.root);
    let walk_min = opts.min_size.min(opts.dup_min_size);
    let mut findings: Vec<Finding> = Vec::new();
    // First path of each duplicate candidate, then the further copies.
    let mut seen: HashMap<Hash, (PathBuf, u64, u64)> = HashMap::new();
    let mut groups: HashMap<Hash, Vec<PathBuf>> = HashMap::new();
    let mut visits = 0usize;

    // (path, hash, history hashes, report untouched folders here, is root).
    // Below a reported untouched folder the walk goes on for caches and
    // duplicates, which are better advice than the folder as a whole.
    let root_hist: Vec<Hash> = hist.iter().map(|s| s.root_hash).collect();
    let mut stack: Vec<(PathBuf, Hash, Vec<Hash>, bool, bool)> =
        vec![(PathBuf::from(&current.root), current.root_hash, root_hist, true, true)];

    while let Some((path, hash, phist, report_stale, is_root)) = stack.pop() {
        visits += 1;
        if visits > opts.max_visits {
            break;
        }
        let node = store.node(&hash)?;
        // The history nodes at this path, loaded once for all children.
        // `None` where the folder is identical to the current one.
        let hist_nodes: Vec<Option<Arc<Node>>> =
            phist.iter().map(|h| if *h == hash { Ok(None) } else { store.node(h).map(Some) }).collect::<Result<_>>()?;

        let mut candidates: Vec<Candidate> = Vec::new();
        for d in node.dirs.iter().filter(|d| d.size >= walk_min) {
            if is_root && volume_root && PROTECTED.contains(&&*d.name) {
                continue;
            }
            if d.flags & FLAG_UNREADABLE != 0 {
                continue;
            }
            let cpath = path.join(&*d.name);
            let package = is_package(d);

            let (chist, streak) = if package { (Vec::new(), 0) } else { child_history(d, &hist_nodes) };
            let unchanged_since = (streak > 0).then(|| hist[streak - 1].finished_at);

            if !package && d.size >= opts.min_size {
                if let Some(rule) = junk_rule(&d.name) {
                    findings.push(Finding {
                        kind: FindingKind::Regenerable,
                        path: cpath,
                        size: d.size,
                        files: d.files,
                        reclaimable: d.size,
                        category: rule.category,
                        hint: rule.hint,
                        copies: Vec::new(),
                        unchanged_since,
                        unchanged_scans: streak,
                        since_first_scan: streak > 0 && streak == hist.len(),
                        untouched_children: 0,
                        examples: Vec::new(),
                    });
                    continue;
                }
            } else if junk_rule(&d.name).is_some() {
                continue;
            }

            // Duplicates: a repeated hash is a copy; its insides are the same
            // as the first copy's, so it is never entered.
            if d.size >= opts.dup_min_size && d.files >= opts.dup_min_files {
                if seen.contains_key(&d.hash) {
                    groups.entry(d.hash).or_default().push(cpath);
                    continue;
                }
                seen.insert(d.hash, (cpath.clone(), d.size, d.files));
            }
            if package {
                continue;
            }
            let stale = report_stale
                && d.size >= opts.min_size
                && unchanged_since.is_some_and(|t| current.finished_at - t >= opts.stale_after);
            candidates.push(Candidate { child: d.clone(), path: cpath, hist: chist, streak, stale });
        }

        // Many untouched subfolders that make up most of this folder: suggest
        // the folder once instead of each of them.
        let stale: Vec<&Candidate> = candidates.iter().filter(|c| c.stale).collect();
        let stale_bytes: u64 = stale.iter().map(|c| c.child.size).sum();
        let rolled_up = !is_root
            && stale.len() >= 3
            && stale_bytes as u128 * 100 >= node_size(&node) as u128 * 70;
        if rolled_up {
            let mut by_size = stale.clone();
            by_size.sort_by_key(|c| Reverse(c.child.size));
            let streak = stale.iter().map(|c| c.streak).min().unwrap_or(0);
            findings.push(Finding {
                kind: FindingKind::Untouched,
                path: path.clone(),
                size: node_size(&node),
                files: stale.iter().map(|c| c.child.files).sum(),
                reclaimable: stale_bytes,
                category: "",
                hint: "",
                copies: Vec::new(),
                // The newest of the children's dates: all of them are at least this old.
                unchanged_since: stale.iter().filter(|c| c.streak > 0).map(|c| hist[c.streak - 1].finished_at).max(),
                unchanged_scans: streak,
                since_first_scan: streak > 0 && streak == hist.len(),
                untouched_children: stale.len(),
                examples: by_size.iter().take(4).map(|c| c.child.name.to_string()).collect(),
            });
        }

        for c in candidates.into_iter().rev() {
            let report = c.stale && !rolled_up;
            if report {
                findings.push(Finding {
                    kind: FindingKind::Untouched,
                    path: c.path.clone(),
                    size: c.child.size,
                    files: c.child.files,
                    reclaimable: c.child.size,
                    category: "",
                    hint: "",
                    copies: Vec::new(),
                    unchanged_since: Some(hist[c.streak - 1].finished_at),
                    unchanged_scans: c.streak,
                    since_first_scan: c.streak == hist.len(),
                    untouched_children: 0,
                    examples: Vec::new(),
                });
            }
            let keep_stale = report_stale && !c.stale && !rolled_up;
            stack.push((c.path, c.child.hash, c.hist, keep_stale, false));
        }
    }

    // Duplicate groups, largest first; a group entirely inside an already
    // reported group is the same duplication seen again.
    let mut dups: Vec<(u64, u64, Vec<PathBuf>)> = groups
        .into_iter()
        .map(|(h, more)| {
            let (first, size, files) = seen.remove(&h).expect("first copy recorded");
            let mut copies = vec![first];
            copies.extend(more);
            copies.sort();
            (size, files, copies)
        })
        .collect();
    dups.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.2.cmp(&b.2)));
    let mut kept: Vec<PathBuf> = Vec::new();
    for (size, files, copies) in dups {
        if copies.iter().all(|p| kept.iter().any(|k| p.starts_with(k))) {
            continue;
        }
        kept.extend(copies.iter().cloned());
        findings.push(Finding {
            kind: FindingKind::Duplicate,
            path: copies[0].clone(),
            size,
            files,
            reclaimable: size * (copies.len() as u64 - 1),
            category: "",
            hint: "",
            copies,
            unchanged_since: None,
            unchanged_scans: 0,
            since_first_scan: false,
            untouched_children: 0,
            examples: Vec::new(),
        });
    }

    // Rank, then drop findings that sit entirely inside a better one. A
    // folder holding better findings only offers what is left besides them
    // (an untouched folder that is mostly one duplicate is that duplicate),
    // so no byte is counted twice.
    findings.sort_by(|a, b| b.score().cmp(&a.score()).then_with(|| a.path.cmp(&b.path)));
    let mut out: Vec<Finding> = Vec::new();
    for mut f in findings {
        let nested = f.paths().iter().all(|p| out.iter().any(|o| o.paths().iter().any(|q| p.starts_with(q))));
        if nested {
            continue;
        }
        if f.kind != FindingKind::Duplicate {
            let inner: u64 = out
                .iter()
                .flat_map(|o| o.paths().iter().map(move |q| (q, o.size)))
                .filter(|(q, _)| q.starts_with(&f.path) && **q != f.path)
                .map(|(_, size)| size)
                .sum();
            if inner > 0 {
                f.reclaimable = f.reclaimable.saturating_sub(inner);
                if f.reclaimable < opts.min_size {
                    continue;
                }
            }
        }
        out.push(f);
        if out.len() >= opts.max_findings {
            break;
        }
    }
    Ok(out)
}

fn node_size(n: &Node) -> u64 {
    n.dirs.iter().map(|d| d.size).sum::<u64>() + n.files.iter().map(|f| f.size).sum::<u64>()
}

/// A child's hashes in the history (newest first, until a scan lacks it) and
/// how many scans in a row it was identical.
fn child_history(d: &DirChild, hist_nodes: &[Option<Arc<Node>>]) -> (Vec<Hash>, usize) {
    let mut out = Vec::with_capacity(hist_nodes.len());
    let mut streak = 0;
    let mut open = true;
    for n in hist_nodes {
        // `None`: the parent was identical there, so the child is too.
        let h = match n {
            None => Some(d.hash),
            Some(n) => n.dir(&d.name).map(|c| c.hash),
        };
        let Some(h) = h else { break };
        if open && h == d.hash {
            streak += 1;
        } else {
            open = false;
        }
        out.push(h);
    }
    (out, streak)
}

#[derive(Clone, Debug)]
pub struct LargeFolder {
    pub path: PathBuf,
    pub size: u64,
    pub files: u64,
    pub dirs: u64,
}

/// The largest folders of a scan. A folder that is mostly one subfolder (90 %
/// or more) is skipped in favour of that subfolder, so the list is not a chain
/// of parents all showing the same bytes. Packages count as one item.
pub fn largest_folders(store: &Store, scan: &ScanInfo, limit: usize) -> Result<Vec<LargeFolder>> {
    const MAX_VISITS: usize = 400_000;
    let mut heap: BinaryHeap<Reverse<(u64, PathBuf, u64, u64)>> = BinaryHeap::new();
    let mut stack = vec![(PathBuf::from(&scan.root), scan.root_hash)];
    let mut visits = 0;
    while let Some((path, hash)) = stack.pop() {
        visits += 1;
        if visits > MAX_VISITS {
            break;
        }
        let n = store.node(&hash)?;
        for d in &n.dirs {
            let floor = if heap.len() >= limit { heap.peek().map_or(0, |r| r.0.0) } else { 0 };
            if d.size == 0 || d.size <= floor {
                continue;
            }
            let child = path.join(&*d.name);
            let package = is_package(d);
            let dominated = if package {
                false
            } else {
                let cn = store.node(&d.hash)?;
                cn.dirs.iter().map(|x| x.size).max().unwrap_or(0) as u128 * 100 >= d.size as u128 * 90
            };
            if !dominated {
                heap.push(Reverse((d.size, child.clone(), d.files, d.dirs)));
                if heap.len() > limit {
                    heap.pop();
                }
            }
            if !package {
                stack.push((child, d.hash));
            }
        }
    }
    let mut v: Vec<LargeFolder> =
        heap.into_iter().map(|Reverse((size, path, files, dirs))| LargeFolder { path, size, files, dirs }).collect();
    v.sort_by(|a, b| b.size.cmp(&a.size).then_with(|| a.path.cmp(&b.path)));
    Ok(v)
}

#[derive(Clone, Debug, Default)]
pub struct ChangeSummary {
    /// Largest |delta| first.
    pub changes: Vec<Change>,
    /// Scanned size of `cur` minus that of `cmp`, over the part both cover.
    pub total_delta: i64,
    /// Bytes in files and folders that appeared / disappeared.
    pub added: u64,
    pub removed: u64,
    /// Bytes moved from one place to another, not counted in either.
    pub moved: u64,
    pub min_size: u64,
}

/// The changes between two scans that are at least `min_size`: whole folders
/// that appeared or vanished, files that changed, and folders that grew or
/// shrank through many small changes no single item explains. Detail that
/// explains a folder's change replaces the folder; long detail collapses into
/// it. Identical subtrees are skipped by hash.
pub fn changes(store: &Store, cur: &ScanInfo, cmp: &ScanInfo, min_size: u64, limit: usize) -> Result<ChangeSummary> {
    let mut out = ChangeSummary { min_size, ..Default::default() };
    // Compare where both scans overlap: the deeper of the two roots.
    let (cr, pr) = (Path::new(&cur.root), Path::new(&cmp.root));
    let base = if cr.starts_with(pr) { cr } else if pr.starts_with(cr) { pr } else { return Ok(out) };
    let (Some(a), Some(b)) = (open(store, cur, base)?, open(store, cmp, base)?) else { return Ok(out) };
    let (an, bn) = (store.node(&a)?, store.node(&b)?);
    out.total_delta = node_size(&an) as i64 - node_size(&bn) as i64;
    let mut w = ChangeWalk { store, min: min_size, visits: 0, added: 0, removed: 0 };
    let v = w.walk(base, a, b)?;
    let moves = crate::moves::detect(store, cur, cmp, &|| false)?;
    let mut v = fold_moves(v, &moves, min_size);
    // Real changes first by size, then the moves by size.
    v.sort_by_key(|c| (c.moved_from.is_some(), Reverse(c.change().unsigned_abs()), Reverse(c.size)));
    v.truncate(limit);
    out.changes = v;
    out.moved = moves.total();
    // The walk counted each move's new end as added and its old end as
    // removed; only what a folder gained and lost on the way is real.
    let (mut added, mut removed) = (w.added, w.removed);
    for m in &moves.list {
        added = added.saturating_sub(m.after);
        removed = removed.saturating_sub(m.size);
        if m.change() != 0
            && let (Some(a), Some(b)) = (open(store, cur, &m.to)?, open(store, cmp, &m.from)?)
        {
            let mut inner = ChangeWalk { store, min: u64::MAX, visits: 0, added: 0, removed: 0 };
            inner.walk(&m.to, a, b)?;
            added += inner.added;
            removed += inner.removed;
        }
    }
    (out.added, out.removed) = (added, removed);
    Ok(out)
}

/// One row per move instead of a New and a Deleted row; the moved bytes of
/// other rows are marked, and rows that only changed by moves are dropped.
fn fold_moves(v: Vec<Change>, moves: &crate::moves::Moves, min: u64) -> Vec<Change> {
    let mut out = Vec::with_capacity(v.len());
    for mut c in v {
        if c.status == Status::Removed && moves.from(&c.path).is_some() || c.status == Status::Added && moves.to(&c.path).is_some() {
            continue;
        }
        c.moved_bytes = moves
            .list
            .iter()
            .map(|m| match (m.to.starts_with(&c.path), m.from.starts_with(&c.path)) {
                (true, false) => m.size as i64,
                (false, true) => -(m.size as i64),
                _ => 0,
            })
            .sum();
        if c.change().unsigned_abs() >= min {
            out.push(c);
        }
    }
    for m in moves.list.iter().filter(|m| m.size >= min) {
        out.push(Change {
            path: m.to.clone(),
            is_dir: m.is_dir,
            size: m.after,
            prev: 0,
            status: Status::Added,
            moved_bytes: m.size as i64,
            moved_from: Some(m.from.clone()),
        });
    }
    out
}

fn open(store: &Store, scan: &ScanInfo, abs: &Path) -> Result<Option<Hash>> {
    let Some(rel) = crate::query::rel_in(scan, abs) else { return Ok(None) };
    Ok(store.resolve(&scan.root_hash, &rel)?.map(|x| x.0))
}

struct ChangeWalk<'a> {
    store: &'a Store,
    min: u64,
    visits: usize,
    added: u64,
    removed: u64,
}

impl ChangeWalk<'_> {
    const MAX_VISITS: usize = 400_000;

    fn walk(&mut self, path: &Path, a: Hash, b: Hash) -> Result<Vec<Change>> {
        let mut out = Vec::new();
        if a == b || self.visits >= Self::MAX_VISITS {
            return Ok(out);
        }
        self.visits += 1;
        let (an, bn) = (self.store.node(&a)?, self.store.node(&b)?);
        let emit = |out: &mut Vec<Change>, c: Change, min: u64| {
            if c.delta().unsigned_abs() >= min {
                out.push(c);
            }
        };
        for d in &an.dirs {
            let p = path.join(&*d.name);
            match bn.dir(&d.name) {
                None => {
                    self.added += d.size;
                    emit(&mut out, Change { path: p, is_dir: true, size: d.size, prev: 0, status: Status::Added, moved_bytes: 0, moved_from: None }, self.min);
                }
                Some(o) if o.hash == d.hash => {}
                // A folder unreadable in either scan has no trustworthy change.
                Some(o) if (o.flags | d.flags) & FLAG_UNREADABLE != 0 => {}
                Some(o) => {
                    let below = self.walk(&p, d.hash, o.hash)?;
                    let delta = d.size as i64 - o.size as i64;
                    if delta.unsigned_abs() < self.min {
                        out.extend(below);
                        continue;
                    }
                    let explained: u64 = below.iter().map(|c| c.delta().unsigned_abs()).sum();
                    let me = Change { path: p, is_dir: true, size: d.size, prev: o.size, status: Status::Changed, moved_bytes: 0, moved_from: None };
                    if (explained as u128) * 100 < delta.unsigned_abs() as u128 * 60 {
                        out.extend(below);
                        out.push(me);
                    } else if below.len() > 3 {
                        out.push(me);
                    } else {
                        out.extend(below);
                    }
                }
            }
        }
        for d in bn.dirs.iter().filter(|d| an.dir(&d.name).is_none()) {
            self.removed += d.size;
            emit(&mut out, Change { path: path.join(&*d.name), is_dir: true, size: 0, prev: d.size, status: Status::Removed, moved_bytes: 0, moved_from: None }, self.min);
        }
        for f in &an.files {
            match bn.file(&f.name) {
                Some(o) if o.size == f.size => {}
                Some(o) => emit(
                    &mut out,
                    Change { path: path.join(&*f.name), is_dir: false, size: f.size, prev: o.size, status: Status::Changed, moved_bytes: 0, moved_from: None },
                    self.min,
                ),
                None => {
                    self.added += f.size;
                    emit(&mut out, Change { path: path.join(&*f.name), is_dir: false, size: f.size, prev: 0, status: Status::Added, moved_bytes: 0, moved_from: None }, self.min);
                }
            }
        }
        for f in bn.files.iter().filter(|f| an.file(&f.name).is_none()) {
            self.removed += f.size;
            emit(&mut out, Change { path: path.join(&*f.name), is_dir: false, size: 0, prev: f.size, status: Status::Removed, moved_bytes: 0, moved_from: None }, self.min);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan::{Progress, scan};
    use std::fs;

    fn write(p: &Path, n: usize) {
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, vec![7u8; n]).unwrap();
    }

    fn opts() -> Options {
        Options {
            min_size: 100_000,
            dup_min_size: 100_000,
            dup_min_files: 2,
            stale_after: 0,
            history_limit: 24,
            max_findings: 50,
            max_visits: 100_000,
        }
    }

    fn scan_at(store: &Store, root: &Path) -> ScanInfo {
        let id = scan(store, root, false, &Progress::default()).unwrap().scan_id;
        store.scans().unwrap().into_iter().find(|s| s.id == id).unwrap()
    }

    fn project(root: &Path, name: &str, salt: usize) {
        for i in 0..6 {
            write(&root.join(format!("{name}/src/f{i}.rs")), 30_000 + salt);
        }
    }

    #[test]
    fn regenerable_and_duplicates() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("r");
        for i in 0..10 {
            write(&root.join(format!("web/node_modules/pkg{i}/index.js")), 20_000);
        }
        // Two identical photo folders, one nested deeper; each holds an
        // identical subfolder that must not be reported again.
        for base in ["Photos", "Backup/Photos copy"] {
            for i in 0..8 {
                write(&root.join(format!("{base}/img{i}.jpg")), 40_000);
                write(&root.join(format!("{base}/raw/r{i}.raw")), 40_000);
            }
        }
        project(&root, "a", 0);
        let store = Store::open(&tmp.path().join("db.sqlite")).unwrap();
        let cur = scan_at(&store, &root);
        let f = suggestions_with(&store, std::slice::from_ref(&cur), &cur, &opts()).unwrap();
        let root = root.canonicalize().unwrap();

        let junk: Vec<_> = f.iter().filter(|x| x.kind == FindingKind::Regenerable).collect();
        assert_eq!(junk.len(), 1, "{f:#?}");
        assert_eq!(junk[0].path, root.join("web/node_modules"));
        assert_eq!(junk[0].category, "Package dependencies");
        assert_eq!(junk[0].reclaimable, junk[0].size);

        let dups: Vec<_> = f.iter().filter(|x| x.kind == FindingKind::Duplicate).collect();
        assert_eq!(dups.len(), 1, "{dups:#?}");
        assert_eq!(dups[0].copies, vec![root.join("Backup/Photos copy"), root.join("Photos")]);
        assert_eq!(dups[0].reclaimable, dups[0].size);
        // No untouched folders without history.
        assert!(f.iter().all(|x| x.kind != FindingKind::Untouched));
    }

    #[test]
    fn untouched_prefers_the_highest_folder() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("r");
        for i in 0..4 {
            write(&root.join(format!("Archive/2019/sub{i}/a.bin")), 60_000);
        }
        write(&root.join("Work/notes.txt"), 200_000);
        let store = Store::open(&tmp.path().join("db.sqlite")).unwrap();
        let s1 = scan_at(&store, &root);
        write(&root.join("Work/notes.txt"), 300_000);
        let s2 = scan_at(&store, &root);
        write(&root.join("Work/more.txt"), 150_000);
        let s3 = scan_at(&store, &root);
        let scans = vec![s1.clone(), s2, s3.clone()];

        let f = suggestions_with(&store, &scans, &s3, &opts()).unwrap();
        let root = root.canonicalize().unwrap();
        let stale: Vec<_> = f.iter().filter(|x| x.kind == FindingKind::Untouched).collect();
        assert_eq!(stale.len(), 1, "{f:#?}");
        assert_eq!(stale[0].path, root.join("Archive"));
        assert_eq!(stale[0].unchanged_scans, 2);
        assert!(stale[0].since_first_scan);
        assert_eq!(stale[0].unchanged_since, Some(s1.finished_at));

        // Too recent to count when the bar is 30 days.
        let mut o = opts();
        o.stale_after = 30 * DAY;
        let f = suggestions_with(&store, &scans, &s3, &o).unwrap();
        assert!(f.iter().all(|x| x.kind != FindingKind::Untouched));
    }

    #[test]
    fn untouched_rolls_up_many_children() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("r");
        for i in 0..4 {
            write(&root.join(format!("Media/Old{i}/v.mov")), 200_000);
        }
        write(&root.join("Media/new.txt"), 10);
        let store = Store::open(&tmp.path().join("db.sqlite")).unwrap();
        let s1 = scan_at(&store, &root);
        write(&root.join("Media/new.txt"), 50_000);
        let s2 = scan_at(&store, &root);
        let f = suggestions_with(&store, &[s1, s2.clone()], &s2, &opts()).unwrap();
        let stale: Vec<_> = f.iter().filter(|x| x.kind == FindingKind::Untouched).collect();
        assert_eq!(stale.len(), 1, "{f:#?}");
        assert!(stale[0].path.ends_with("Media"));
        assert_eq!(stale[0].untouched_children, 4);
        assert_eq!(stale[0].examples.len(), 4);
    }

    #[test]
    fn better_findings_inside_an_untouched_folder_win() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("r");
        // An old project that is all dependencies, and an old backup that is
        // all a copy of another folder.
        for i in 0..10 {
            write(&root.join(format!("OldProject/node_modules/p{i}/x.js")), 40_000);
            write(&root.join(format!("Shots/s{i}.png")), 30_000);
            write(&root.join(format!("OldBackup/Shots/s{i}.png")), 30_000);
        }
        write(&root.join("OldProject/README"), 100);
        write(&root.join("live.txt"), 10);
        let store = Store::open(&tmp.path().join("db.sqlite")).unwrap();
        let s1 = scan_at(&store, &root);
        write(&root.join("live.txt"), 50_000);
        let s2 = scan_at(&store, &root);
        let f = suggestions_with(&store, &[s1, s2.clone()], &s2, &opts()).unwrap();
        let root = root.canonicalize().unwrap();
        let kinds: Vec<_> = f.iter().map(|x| (x.kind, x.path.strip_prefix(&root).unwrap().to_owned())).collect();
        assert!(kinds.contains(&(FindingKind::Regenerable, "OldProject/node_modules".into())), "{kinds:?}");
        assert!(f.iter().any(|x| x.kind == FindingKind::Duplicate), "{kinds:?}");
        // The untouched folders are nothing beyond those, so they are gone.
        assert!(!kinds.contains(&(FindingKind::Untouched, "OldProject".into())), "{kinds:?}");
        assert!(!kinds.contains(&(FindingKind::Untouched, "OldBackup".into())), "{kinds:?}");
    }

    #[test]
    fn largest_folders_skip_wrappers() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("r");
        write(&root.join("wrap/inner/big.bin"), 900_000);
        write(&root.join("wrap/small.txt"), 10_000);
        write(&root.join("mixed/a/x.bin"), 300_000);
        write(&root.join("mixed/b/y.bin"), 300_000);
        let store = Store::open(&tmp.path().join("db.sqlite")).unwrap();
        let s = scan_at(&store, &root);
        let v = largest_folders(&store, &s, 10).unwrap();
        let names: Vec<String> = v.iter().map(|f| f.path.file_name().unwrap().to_string_lossy().into_owned()).collect();
        assert_eq!(names[0], "inner", "{names:?}");
        assert!(!names.contains(&"wrap".to_string()), "{names:?}");
        assert!(names.contains(&"mixed".to_string()));
        let v = largest_folders(&store, &s, 1).unwrap();
        assert_eq!(v.len(), 1);
    }

    #[test]
    fn changes_roll_up_and_totals() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("r");
        write(&root.join("keep/a.bin"), 100_000);
        write(&root.join("gone/b.bin"), 500_000);
        write(&root.join("grow/one.bin"), 100_000);
        for i in 0..10 {
            write(&root.join(format!("cache/c{i}.bin")), 10_000);
        }
        let store = Store::open(&tmp.path().join("db.sqlite")).unwrap();
        let s1 = scan_at(&store, &root);
        fs::remove_dir_all(root.join("gone")).unwrap();
        write(&root.join("grow/one.bin"), 700_000);
        write(&root.join("new/n.bin"), 400_000);
        // Many small changes that only add up at the folder.
        for i in 0..10 {
            write(&root.join(format!("cache/c{i}.bin")), 60_000);
        }
        let s2 = scan_at(&store, &root);
        let ch = changes(&store, &s2, &s1, 300_000, 100).unwrap();
        let root = root.canonicalize().unwrap();
        let find = |p: &str| ch.changes.iter().find(|c| c.path == root.join(p));
        assert_eq!(find("gone").unwrap().status, Status::Removed);
        assert_eq!(find("new").unwrap().status, Status::Added);
        assert_eq!(find("grow/one.bin").unwrap().status, Status::Changed);
        assert!(find("grow").is_none(), "explained by its file");
        let cache = find("cache").expect("rolled up");
        assert!(cache.is_dir && cache.delta() > 0);
        assert!(find("keep").is_none());
        assert_eq!(ch.total_delta, s2.size as i64 - s1.size as i64);
        assert!(ch.added >= 400_000 && ch.removed >= 500_000);
        // Sorted by |delta|.
        assert!(ch.changes.windows(2).all(|w| w[0].delta().unsigned_abs() >= w[1].delta().unsigned_abs()));
    }
}

