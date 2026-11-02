//! Moves: folders and files that vanished in one place and turned up in
//! another between two scans.
//!
//! A node's hash covers only its contents, not its own name or location, so a
//! folder moved (or renamed) without changes inside has the same hash before
//! and after. Files carry only a name and a size, so a file counts as moved
//! when one with the same name and size vanished elsewhere; only files of at
//! least [`MIN_SIZE`] whose name occurs once on each side are matched, where
//! that is very unlikely to be chance. Sizes are allocated blocks, so a folder
//! of fewer than [`THIN`] files says little more than that, and is matched only
//! when it kept its name the same way, or was renamed where it stood.
//!
//! Packages, regenerable folders and where apps keep their data (`Library`,
//! hidden folders, the system's own) are sealed: an app moved to Applications
//! is a move, but what an updater, an installer, a package manager or an app
//! shuffles around inside such folders is not one anybody made, so they are
//! matched only whole and never opened.
//!
//! A folder that changed on the way has a new hash, so at first only what is
//! unchanged inside it pairs up. When those moves make up most of both ends,
//! the folder itself counts as moved, and the difference as real change.
//!
//! Last, a move somebody made starts or ends in a folder that is there in both
//! scans, or is most of what vanished or appeared around it; a component that
//! is identical in two unrelated installs is neither, and is dropped.
//! Nothing extra is stored: moves are worked out from the two scans.

use crate::node::{DirChild, Hash};
use crate::store::{ScanInfo, Store};
use anyhow::Result;
use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// Smaller folders and files are not matched: empty and near-empty folders
/// share hashes all over the disk.
pub const MIN_SIZE: u64 = 1_000_000;

/// Folders holding fewer files are matched only by name (see the module docs):
/// two `xx.lproj` folders of one `locale.pak` each share a hash by chance.
pub const THIN: u64 = 3;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Move {
    /// Absolute paths in the compare scan and the current scan.
    pub from: PathBuf,
    pub to: PathBuf,
    pub is_dir: bool,
    /// Size at `from`: the bytes that moved.
    pub size: u64,
    /// Size at `to`. It differs from `size` when the folder changed on the
    /// way; the difference is real growth or shrinkage.
    pub after: u64,
}

impl Move {
    pub fn change(&self) -> i64 {
        self.after as i64 - self.size as i64
    }
}

/// How to name the other end of a move seen from this end: its folder
/// ("from Downloads"), or its own name when it was renamed in place.
pub fn label(other: &Path, this: &Path) -> String {
    let name = |p: &Path| p.file_name().map_or_else(|| p.display().to_string(), |n| n.to_string_lossy().into_owned());
    match (other.parent(), this.parent()) {
        (Some(a), Some(b)) if a == b => name(other),
        (Some(a), _) => name(a),
        _ => name(other),
    }
}

#[derive(Clone, Debug, Default)]
pub struct Moves {
    pub list: Vec<Move>,
    by_from: HashMap<PathBuf, usize>,
    by_to: HashMap<PathBuf, usize>,
}

impl Moves {
    fn new(list: Vec<Move>) -> Moves {
        let by_from = list.iter().enumerate().map(|(i, m)| (m.from.clone(), i)).collect();
        let by_to = list.iter().enumerate().map(|(i, m)| (m.to.clone(), i)).collect();
        Moves { list, by_from, by_to }
    }

    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }

    /// The move that left `abs` (it is gone from the current scan).
    pub fn from(&self, abs: &Path) -> Option<&Move> {
        self.by_from.get(abs).map(|&i| &self.list[i])
    }

    /// The move that arrived at `abs`.
    pub fn to(&self, abs: &Path) -> Option<&Move> {
        self.by_to.get(abs).map(|&i| &self.list[i])
    }

    /// Where `abs` was in the compare scan, when it is (inside) a folder that
    /// arrived by a move.
    pub fn before(&self, abs: &Path) -> Option<PathBuf> {
        abs.ancestors().find_map(|a| Some(self.to(a)?.from.join(abs.strip_prefix(a).ok()?)))
    }

    /// Where `abs` is in the current scan, when it is (inside) a folder that
    /// was moved away.
    pub fn after(&self, abs: &Path) -> Option<PathBuf> {
        abs.ancestors().find_map(|a| Some(self.from(a)?.to.join(abs.strip_prefix(a).ok()?)))
    }

    /// Bytes moved into (+) or out of (−) each child of `dir`, by child name.
    /// Moves within one child cancel out and are left out.
    pub fn net_by_child(&self, dir: &Path) -> HashMap<String, i64> {
        let child = |p: &Path| -> Option<String> {
            let rest = p.strip_prefix(dir).ok()?;
            rest.components().next().map(|c| c.as_os_str().to_string_lossy().into_owned())
        };
        let mut out: HashMap<String, i64> = HashMap::new();
        for m in &self.list {
            let (a, b) = (child(&m.from), child(&m.to));
            if a == b {
                continue;
            }
            if let Some(a) = a {
                *out.entry(a).or_default() -= m.size as i64;
            }
            if let Some(b) = b {
                *out.entry(b).or_default() += m.size as i64;
            }
        }
        out.retain(|_, v| *v != 0);
        out
    }

    /// Total bytes moved, counted once per move.
    pub fn total(&self) -> u64 {
        self.list.iter().map(|m| m.size).sum()
    }
}

struct Entry {
    path: PathBuf,
    size: u64,
    /// Recursive number of files (1 for a file).
    files: u64,
}

#[derive(Default)]
struct Side {
    /// Every folder of at least MIN_SIZE inside what appeared (or vanished).
    dirs: Vec<(Hash, Entry)>,
    files: Vec<Entry>,
}

/// The system's own folders at the top of a disk.
const SYSTEM: &[&str] = &["System", "private", "usr", "opt", "bin", "sbin", "cores", "dev", "Windows", "ProgramData"];

/// See the module docs. `at` is the folder holding `d`.
fn sealed(d: &DirChild, at: &Path) -> bool {
    let n = &*d.name;
    crate::insights::is_package(d)
        || crate::insights::junk_rule(n).is_some()
        || n.starts_with('.')
        || n == "Library"
        || at.parent().is_none() && SYSTEM.contains(&n)
}

/// Finds the moves between `cmp` (before) and `cur` (after), where the two
/// scans overlap. Only subtrees whose hashes differ are opened.
pub fn detect(store: &Store, cur: &ScanInfo, cmp: &ScanInfo, cancel: &dyn Fn() -> bool) -> Result<Moves> {
    let (cr, pr) = (Path::new(&cur.root), Path::new(&cmp.root));
    let base = if cr.starts_with(pr) { cr } else if pr.starts_with(cr) { pr } else { return Ok(Moves::default()) };
    let open = |scan: &ScanInfo| -> Result<Option<Hash>> {
        let Some(rel) = crate::query::rel_in(scan, base) else { return Ok(None) };
        Ok(store.resolve(&scan.root_hash, &rel)?.map(|x| x.0))
    };
    let (Some(a), Some(b)) = (open(cur)?, open(cmp)?) else { return Ok(Moves::default()) };

    // What appeared and what vanished, each with its large folders and files.
    let (mut added, mut removed) = (Side::default(), Side::default());
    let mut stack = vec![(base.to_owned(), a, b)];
    while let Some((path, ha, hb)) = stack.pop() {
        if ha == hb {
            continue;
        }
        if cancel() {
            return Ok(Moves::default());
        }
        let (an, bn) = (store.node(&ha)?, store.node(&hb)?);
        for d in &an.dirs {
            match bn.dir(&d.name) {
                Some(o) if o.hash != d.hash && !sealed(d, &path) => stack.push((path.join(&*d.name), d.hash, o.hash)),
                Some(_) => {}
                None => collect(store, &mut added, &path, d, cancel)?,
            }
        }
        for d in bn.dirs.iter().filter(|d| an.dir(&d.name).is_none()) {
            collect(store, &mut removed, &path, d, cancel)?;
        }
        for f in an.files.iter().filter(|f| f.size >= MIN_SIZE && bn.file(&f.name).is_none()) {
            added.files.push(Entry { path: path.join(&*f.name), size: f.size, files: 1 });
        }
        for f in bn.files.iter().filter(|f| f.size >= MIN_SIZE && an.file(&f.name).is_none()) {
            removed.files.push(Entry { path: path.join(&*f.name), size: f.size, files: 1 });
        }
    }
    Ok(Moves::new(pair(added, removed)))
}

/// Records a folder that appeared or vanished, and every large folder and
/// file inside it: a folder moved into a new folder is only found below it.
fn collect(store: &Store, side: &mut Side, at: &Path, d: &DirChild, cancel: &dyn Fn() -> bool) -> Result<()> {
    if d.size < MIN_SIZE {
        return Ok(());
    }
    let mut stack = vec![(at.join(&*d.name), d.hash, d.size, d.files, sealed(d, at))];
    while let Some((path, hash, size, files, closed)) = stack.pop() {
        if cancel() {
            return Ok(());
        }
        if !closed {
            let n = store.node(&hash)?;
            for f in n.files.iter().filter(|f| f.size >= MIN_SIZE) {
                side.files.push(Entry { path: path.join(&*f.name), size: f.size, files: 1 });
            }
            for d in n.dirs.iter().filter(|d| d.size >= MIN_SIZE) {
                stack.push((path.join(&*d.name), d.hash, d.size, d.files, sealed(d, &path)));
            }
        }
        side.dirs.push((hash, Entry { path, size, files }));
    }
    Ok(())
}

/// Pairs what appeared with what vanished, largest first, one to one. A
/// matched folder claims everything inside it on both sides.
fn pair(added: Side, removed: Side) -> Vec<Move> {
    let inside = |claimed: &HashSet<PathBuf>, p: &Path| p.ancestors().any(|a| claimed.contains(a));
    let name = |p: &Path| p.file_name().map(|n| n.to_owned());
    // How often each name occurs on one side: a name seen more than once (all
    // the `locale.pak`s) does not tell its owners apart.
    let count = |paths: &mut dyn Iterator<Item = &PathBuf>| {
        let mut n: HashMap<Option<OsString>, usize> = HashMap::new();
        for p in paths {
            *n.entry(name(p)).or_default() += 1;
        }
        n
    };
    let once = |n: &HashMap<Option<OsString>, usize>, p: &Path| n.get(&name(p)) == Some(&1);
    let (dirs_a, dirs_b) = (count(&mut added.dirs.iter().map(|d| &d.1.path)), count(&mut removed.dirs.iter().map(|d| &d.1.path)));
    let (files_a, files_b) = (count(&mut added.files.iter().map(|e| &e.path)), count(&mut removed.files.iter().map(|e| &e.path)));
    let (mut took_a, mut took_b) = (HashSet::new(), HashSet::new());
    let (mut above_a, mut above_b) = (HashSet::new(), HashSet::new());
    let mut out = Vec::new();

    let mut by_hash: HashMap<Hash, Vec<&Entry>> = HashMap::new();
    for (h, e) in &removed.dirs {
        by_hash.entry(*h).or_default().push(e);
    }
    let mut cands = Vec::new();
    for (h, e) in &added.dirs {
        for &c in by_hash.get(h).into_iter().flatten() {
            let same_name = name(&c.path) == name(&e.path);
            // A thin folder needs more than its hash: its own name, unique on
            // both sides, or the same parent (renamed in place).
            if e.files >= THIN || c.path.parent() == e.path.parent() || (same_name && once(&dirs_a, &e.path) && once(&dirs_b, &c.path)) {
                cands.push((e, c, same_name, kinship(&e.path, &c.path)));
            }
        }
    }
    // Largest first; then the same name; then the most alike surroundings, so
    // a tree moved as a whole pairs with itself rather than with a copy
    // installed elsewhere; then the outer folder.
    cands.sort_by(|x, y| {
        (y.0.size, y.2, y.3).cmp(&(x.0.size, x.2, x.3)).then(x.0.path.components().count().cmp(&y.0.path.components().count()))
    });
    for (e, c, ..) in cands {
        if inside(&took_a, &e.path) || inside(&took_b, &c.path) || above_a.contains(&e.path) || above_b.contains(&c.path) {
            continue;
        }
        took_a.insert(e.path.clone());
        took_b.insert(c.path.clone());
        above_a.extend(e.path.ancestors().skip(1).map(Path::to_path_buf));
        above_b.extend(c.path.ancestors().skip(1).map(Path::to_path_buf));
        out.push(Move { from: c.path.clone(), to: e.path.clone(), is_dir: true, size: e.size, after: e.size });
    }

    let mut by_key: HashMap<(Option<OsString>, u64), Vec<&Entry>> = HashMap::new();
    for e in removed.files.iter().filter(|e| !inside(&took_b, &e.path) && once(&files_b, &e.path)) {
        by_key.entry((name(&e.path), e.size)).or_default().push(e);
    }
    let mut files: Vec<_> = added.files.iter().filter(|e| !inside(&took_a, &e.path) && once(&files_a, &e.path)).collect();
    files.sort_by(|x, y| y.size.cmp(&x.size).then_with(|| x.path.cmp(&y.path)));
    for e in files {
        let Some(cands) = by_key.get_mut(&(name(&e.path), e.size)) else { continue };
        if cands.is_empty() {
            continue;
        }
        let c = cands.remove(0);
        out.push(Move { from: c.path.clone(), to: e.path.clone(), is_dir: false, size: e.size, after: e.size });
    }
    let size_a: HashMap<&Path, u64> = added.dirs.iter().map(|(_, e)| (e.path.as_path(), e.size)).collect();
    let size_b: HashMap<&Path, u64> = removed.dirs.iter().map(|(_, e)| (e.path.as_path(), e.size)).collect();
    let mut out = roll_up(out, &size_a, &size_b);
    // The outermost new folder around `p`, if `p`'s own folder is new.
    let top = |sizes: &HashMap<&Path, u64>, p: &Path| p.ancestors().skip(1).take_while(|a| sizes.contains_key(a)).last().map(|a| sizes[a]);
    out.retain(|m| top(&size_b, &m.from).is_none_or(|s| m.size * 2 > s) || top(&size_a, &m.to).is_none_or(|s| m.after * 2 > s));
    out
}

/// How many folder names above two paths agree, from the nearest up.
fn kinship(a: &Path, b: &Path) -> usize {
    match (a.parent(), b.parent()) {
        (Some(a), Some(b)) => a.components().rev().zip(b.components().rev()).take_while(|(x, y)| x == y).count(),
        _ => 0,
    }
}

/// Turns the moves inside a folder that moved and changed on the way into one
/// move of the folder. A vanished and an appeared folder count as one when
/// moves between them make up more than half of each, and next to nothing
/// inside either went to or came from anywhere else: such a stray match (a
/// subfolder identical to one in an unrelated install) is dropped.
/// `size_a` and `size_b` hold the folders that appeared and vanished.
fn roll_up(mut moves: Vec<Move>, size_a: &HashMap<&Path, u64>, size_b: &HashMap<&Path, u64>) -> Vec<Move> {
    // The folders around each move, level by level, while both are new.
    let mut pairs: Vec<(PathBuf, PathBuf)> = Vec::new();
    let mut seen = HashSet::new();
    for m in &moves {
        for (f, t) in m.from.ancestors().skip(1).zip(m.to.ancestors().skip(1)) {
            if !size_b.contains_key(f) || !size_a.contains_key(t) {
                break;
            }
            if seen.insert((f, t)) {
                pairs.push((f.to_owned(), t.to_owned()));
            }
        }
    }
    // Outermost first: it takes in the pairs inside it.
    pairs.sort_by_key(|(f, _)| f.components().count());
    for (f, t) in pairs {
        let (mut bytes, mut stray) = (0, 0);
        for m in &moves {
            match (m.from.starts_with(&f), m.to.starts_with(&t)) {
                (true, true) => bytes += m.size,
                (false, false) => {}
                _ => stray += m.size,
            }
        }
        let (before, after) = (size_b[f.as_path()], size_a[t.as_path()]);
        if stray * 10 >= bytes || bytes * 2 <= before || bytes * 2 <= after {
            continue;
        }
        moves.retain(|m| !m.from.starts_with(&f) && !m.to.starts_with(&t));
        moves.push(Move { from: f, to: t, is_dir: true, size: before, after });
    }
    moves
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

    fn two_scans(setup: impl Fn(&Path), change: impl Fn(&Path)) -> (tempfile::TempDir, Store, ScanInfo, ScanInfo) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        setup(&root);
        let store = Store::open(&dir.path().join("t.db")).unwrap();
        scan(&store, &root, false, &Progress::default()).unwrap();
        change(&root);
        scan(&store, &root, false, &Progress::default()).unwrap();
        let scans = store.scans().unwrap();
        let (cur, cmp) = (scans.iter().max_by_key(|s| s.id).unwrap().clone(), scans.iter().min_by_key(|s| s.id).unwrap().clone());
        (dir, store, cur, cmp)
    }

    #[test]
    fn finds_moved_folder_and_file() {
        let mb = 1_000_000;
        let (_d, store, cur, cmp) = two_scans(
            |r| {
                write(&r.join("Downloads/media/a.mov"), 3 * mb);
                write(&r.join("Downloads/media/b.mov"), 2 * mb);
                write(&r.join("Downloads/film.mp4"), 4 * mb);
                write(&r.join("Downloads/small.txt"), 10);
                fs::create_dir_all(r.join("Projects")).unwrap();
            },
            |r| {
                fs::rename(r.join("Downloads/media"), r.join("Projects/media")).unwrap();
                fs::create_dir_all(r.join("Movies/2026")).unwrap();
                fs::rename(r.join("Downloads/film.mp4"), r.join("Movies/2026/film.mp4")).unwrap();
                fs::remove_file(r.join("Downloads/small.txt")).unwrap();
                write(&r.join("Projects/new.bin"), 2 * mb);
            },
        );
        let m = detect(&store, &cur, &cmp, &|| false).unwrap();
        let root = Path::new(&cur.root);
        assert_eq!(m.list.len(), 2, "{:?}", m.list);
        let media = m.to(&root.join("Projects/media")).expect("folder move");
        assert_eq!(media.from, root.join("Downloads/media"));
        assert!(media.is_dir);
        let film = m.from(&root.join("Downloads/film.mp4")).expect("file move into a new folder");
        assert_eq!(film.to, root.join("Movies/2026/film.mp4"));
        assert_eq!(label(&media.from, &media.to), "Downloads");
        assert_eq!(label(&media.to, &media.from), "Projects");

        assert_eq!(m.before(&root.join("Projects/media")).as_deref(), Some(root.join("Downloads/media").as_path()));
        assert_eq!(m.before(&root.join("Projects/media/x")).as_deref(), Some(root.join("Downloads/media/x").as_path()));
        assert_eq!(m.after(&root.join("Downloads/media")).as_deref(), Some(root.join("Projects/media").as_path()));
        let inside = crate::query::list_against(&store, &cur, Some(&cmp), &root.join("Projects/media"), &m.before(&root.join("Projects/media")).unwrap()).unwrap();
        assert!(inside.items.iter().all(|it| it.status == crate::Status::Same), "{:?}", inside.items);
        let net = m.net_by_child(root);
        // Sizes are allocated bytes, so compare with what the moves carry.
        assert_eq!(net.get("Downloads"), Some(&-((media.size + film.size) as i64)));
        assert_eq!(net.get("Projects"), Some(&(media.size as i64)));
        assert_eq!(net.get("Movies"), Some(&(film.size as i64)));
    }

    #[test]
    fn change_list_shows_one_row_per_move() {
        let mb = 1_000_000;
        let (_d, store, cur, cmp) = two_scans(
            |r| {
                write(&r.join("Downloads/media/a.mov"), 6 * mb);
                fs::create_dir_all(r.join("Projects")).unwrap();
            },
            |r| {
                fs::rename(r.join("Downloads/media"), r.join("Projects/media")).unwrap();
                write(&r.join("Projects/renders/r.bin"), 2 * mb);
            },
        );
        let s = crate::insights::changes(&store, &cur, &cmp, mb as u64, 50).unwrap();
        let moved: Vec<_> = s.changes.iter().filter(|c| c.moved_from.is_some()).collect();
        assert_eq!(moved.len(), 1, "{:?}", s.changes);
        assert!(moved[0].path.ends_with("Projects/media") && moved[0].moved_from.as_ref().unwrap().ends_with("Downloads/media"));
        assert!(!s.changes.iter().any(|c| c.status == crate::Status::Removed), "{:?}", s.changes);
        assert!(s.moved > 0 && s.removed == 0);
        assert!(s.added >= 2 * mb as u64 && s.added < 3 * mb as u64);
    }

    #[test]
    fn copy_is_not_a_move_but_a_changed_folder_is() {
        let mb = 1_000_000;
        let (_d, store, cur, cmp) = two_scans(
            |r| {
                write(&r.join("A/data/x.bin"), 3 * mb);
                write(&r.join("B/edited/y.bin"), 3 * mb);
            },
            |r| {
                // Copied: the original is still there.
                write(&r.join("C/data/x.bin"), 3 * mb);
                // Moved and changed inside.
                fs::rename(r.join("B/edited"), r.join("C/edited")).unwrap();
                write(&r.join("C/edited/z.bin"), 2 * mb);
            },
        );
        let m = detect(&store, &cur, &cmp, &|| false).unwrap();
        // The unchanged file inside pairs up and makes up most of both ends.
        assert_eq!(m.list.len(), 1, "{:?}", m.list);
        let mv = &m.list[0];
        assert!(mv.is_dir && mv.from.ends_with("B/edited") && mv.to.ends_with("C/edited"));
        assert!(mv.change() >= 2 * mb as i64 && mv.change() < 3 * mb as i64, "{mv:?}");
    }

    #[test]
    fn folder_moved_and_changed_is_one_move() {
        let mb = 1_000_000;
        let (_d, store, cur, cmp) = two_scans(
            |r| {
                write(&r.join("Project/media/videos/a.mp4"), 4 * mb);
                write(&r.join("Project/media/images/b.png"), 2 * mb);
                write(&r.join("Project/media/_files/thumbs/t.bin"), 2 * mb);
                write(&r.join("Project/media/loop.webp"), 2 * mb);
                write(&r.join("Project/media/old.log"), mb);
                write(&r.join("Project/main.rs"), 10);
                fs::create_dir_all(r.join("Downloads")).unwrap();
            },
            |r| {
                fs::create_dir_all(r.join("Downloads/work")).unwrap();
                fs::rename(r.join("Project/media"), r.join("Downloads/work/media")).unwrap();
                fs::remove_file(r.join("Downloads/work/media/old.log")).unwrap();
                write(&r.join("Downloads/work/media/new.mp4"), 3 * mb);
            },
        );
        let root = Path::new(&cur.root);
        let m = detect(&store, &cur, &cmp, &|| false).unwrap();
        assert_eq!(m.list.len(), 1, "{:?}", m.list);
        let mv = m.to(&root.join("Downloads/work/media")).expect("the folder itself moved");
        assert_eq!(mv.from, root.join("Project/media"));
        assert!(mv.change() > mb as i64 && mv.change() < 3 * mb as i64, "{mv:?}");

        // In the browser: the folder says where it came from, and only what it
        // gained on the way is coloured; inside, it is compared with where it was.
        let mut work = crate::query::list_against(&store, &cur, Some(&cmp), &root.join("Downloads/work"), &root.join("Downloads/work")).unwrap().items;
        crate::query::apply_moves(&mut work, &root.join("Downloads/work"), &m);
        let media = work.iter().find(|it| &*it.name == "media").unwrap();
        assert!(media.moved.as_ref().is_some_and(|x| x.arrived));
        assert_eq!(media.change(), mv.change());
        let inside = crate::query::list_against(&store, &cur, Some(&cmp), &mv.to, &mv.from).unwrap().items;
        let status = |n: &str| inside.iter().find(|it| &*it.name == n).map(|it| it.status);
        assert_eq!(status("new.mp4"), Some(crate::Status::Added));
        assert_eq!(status("old.log"), Some(crate::Status::Removed));
        assert_eq!(status("videos"), Some(crate::Status::Same));

        // On the Changes screen: one Moved row, and real change only for what
        // was added and deleted inside.
        let s = crate::insights::changes(&store, &cur, &cmp, mb as u64 / 2, 50).unwrap();
        assert_eq!(s.changes.iter().filter(|c| c.moved_from.is_some()).count(), 1, "{:?}", s.changes);
        assert!(s.added >= 3 * mb as u64 && s.added < 4 * mb as u64, "{}", s.added);
        assert!(s.removed >= mb as u64 && s.removed < 2 * mb as u64, "{}", s.removed);
    }

    #[test]
    fn mostly_new_folder_is_not_a_move() {
        let mb = 1_000_000;
        let (_d, store, cur, cmp) = two_scans(
            |r| {
                for f in ["k1.bin", "k2.bin", "k3.bin"] {
                    write(&r.join("A/keep").join(f), 2 * mb);
                }
                write(&r.join("A/big.bin"), 2 * mb);
            },
            |r| {
                // Most of A goes into a new folder that is mostly other things,
                // the rest of A is deleted.
                write(&r.join("X/other.bin"), 8 * mb);
                fs::rename(r.join("A/keep"), r.join("X/keep")).unwrap();
                fs::remove_dir_all(r.join("A")).unwrap();
            },
        );
        let m = detect(&store, &cur, &cmp, &|| false).unwrap();
        assert_eq!(m.list.len(), 1, "{:?}", m.list);
        assert!(m.list[0].to.ends_with("X/keep") && m.list[0].change() == 0);
    }

    #[test]
    fn identical_parts_of_unrelated_installs_are_not_moves() {
        let mb = 1_000_000;
        let (_d, store, cur, cmp) = two_scans(
            |r| {
                for f in ["a.qm", "b.qm", "c.qm"] {
                    write(&r.join("Projects/app/dist/_internal/Qt/translations").join(f), mb);
                }
                write(&r.join("Projects/app/dist/_internal/libpython.dylib"), 9 * mb);
                write(&r.join("Projects/app/main.py"), 10);
            },
            |r| {
                // The build output deleted; another tool bundling the same Qt
                // installed elsewhere.
                fs::remove_dir_all(r.join("Projects/app/dist")).unwrap();
                for f in ["a.qm", "b.qm", "c.qm"] {
                    write(&r.join("Tools/viewer/_internal/Qt6/translations").join(f), mb);
                }
                write(&r.join("Tools/viewer/_internal/libtorch.dylib"), 9 * mb);
            },
        );
        let m = detect(&store, &cur, &cmp, &|| false).unwrap();
        assert!(m.is_empty(), "{:?}", m.list);
    }

    #[test]
    fn look_alike_bundles_are_not_moves() {
        // One app's localisations deleted, another's installed: each `.lproj`
        // holds one `locale.pak`, and sizes are allocated blocks, so folders
        // and files pair up by hash or by name and size alone.
        let mb = 1_000_000;
        let (_d, store, cur, cmp) = two_scans(
            |r| {
                write(&r.join("Old.app/Resources/ta.lproj/locale.pak"), 2 * mb);
                write(&r.join("Old.app/Resources/de.lproj/locale.pak"), 3 * mb + 7);
                write(&r.join("Old.app/Resources/fr.lproj/locale.pak"), 4 * mb);
            },
            |r| {
                fs::remove_dir_all(r.join("Old.app")).unwrap();
                write(&r.join("New.app/Resources/el.lproj/locale.pak"), 2 * mb);
                write(&r.join("New.app/Resources/it.lproj/locale.pak"), 3 * mb + 9);
                write(&r.join("New.app/Resources/nl.lproj/locale.pak"), 5 * mb);
            },
        );
        let m = detect(&store, &cur, &cmp, &|| false).unwrap();
        assert!(m.is_empty(), "{:?}", m.list);
    }

    #[test]
    fn apps_and_dependencies_move_only_whole() {
        let mb = 1_000_000;
        let (_d, store, cur, cmp) = two_scans(
            |r| {
                write(&r.join("releases/Viewer.app/Contents/Frameworks/Codec.framework/Codec"), 3 * mb);
                write(&r.join("releases/Viewer.app/Contents/MacOS/viewer"), 2 * mb);
                write(&r.join("Downloads/Tools.app/Contents/MacOS/tool"), 2 * mb);
                write(&r.join("Downloads/Tools.app/Contents/Resources/data.bin"), 2 * mb);
                for f in ["a.js", "b.js", "c.js"] {
                    write(&r.join("projA/node_modules/typescript/lib").join(f), 2 * mb);
                }
                write(&r.join("projA/main.ts"), 10);
                fs::create_dir_all(r.join("Applications")).unwrap();
            },
            |r| {
                // Installed anew: the app changed, its framework did not.
                fs::remove_dir_all(r.join("releases/Viewer.app")).unwrap();
                write(&r.join("Applications/Viewer.app/Contents/Frameworks/Codec.framework/Codec"), 3 * mb);
                write(&r.join("Applications/Viewer.app/Contents/MacOS/viewer"), 2 * mb + 9000);
                fs::rename(r.join("Downloads/Tools.app"), r.join("Applications/Tools.app")).unwrap();
                fs::remove_dir_all(r.join("projA/node_modules")).unwrap();
                for f in ["a.js", "b.js", "c.js"] {
                    write(&r.join("projB/node_modules/typescript/lib").join(f), 2 * mb);
                }
                write(&r.join("projB/node_modules/vite/vite.js"), 2 * mb);
            },
        );
        let m = detect(&store, &cur, &cmp, &|| false).unwrap();
        assert_eq!(m.list.len(), 1, "{:?}", m.list);
        assert!(m.list[0].to.ends_with("Applications/Tools.app"));
    }

    #[test]
    fn rename_in_place_is_labelled_by_name() {
        let (_d, store, cur, cmp) = two_scans(
            |r| write(&r.join("Docs/old/x.bin"), 2_000_000),
            |r| fs::rename(r.join("Docs/old"), r.join("Docs/new")).unwrap(),
        );
        let m = detect(&store, &cur, &cmp, &|| false).unwrap();
        let mv = &m.list[0];
        assert_eq!(label(&mv.from, &mv.to), "old");
        assert_eq!(label(&mv.to, &mv.from), "new");
        // Seen from above, the rename cancels out.
        assert!(m.net_by_child(Path::new(&cur.root)).is_empty());
    }
}
