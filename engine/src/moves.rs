//! Moves: folders and files that vanished in one place and turned up in
//! another between two scans.
//!
//! A node's hash covers only its contents, not its own name or location, so a
//! folder moved (or renamed) without changes inside has the same hash before
//! and after. Files carry only a name and a size, so a file counts as moved
//! when one with the same name and size vanished elsewhere; only files of at
//! least [`MIN_SIZE`] are matched, where that is very unlikely to be chance.
//! Nothing extra is stored: moves are worked out from the two scans.

use crate::node::Hash;
use crate::store::{ScanInfo, Store};
use anyhow::Result;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// Smaller folders and files are not matched: empty and near-empty folders
/// share hashes all over the disk.
pub const MIN_SIZE: u64 = 1_000_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Move {
    /// Absolute paths in the compare scan and the current scan.
    pub from: PathBuf,
    pub to: PathBuf,
    pub is_dir: bool,
    pub size: u64,
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
}

#[derive(Default)]
struct Side {
    /// Every folder of at least MIN_SIZE inside what appeared (or vanished).
    dirs: Vec<(Hash, Entry)>,
    files: Vec<Entry>,
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
                Some(o) if o.hash != d.hash => stack.push((path.join(&*d.name), d.hash, o.hash)),
                Some(_) => {}
                None => collect(store, &mut added, path.join(&*d.name), d.hash, d.size, cancel)?,
            }
        }
        for d in bn.dirs.iter().filter(|d| an.dir(&d.name).is_none()) {
            collect(store, &mut removed, path.join(&*d.name), d.hash, d.size, cancel)?;
        }
        for f in an.files.iter().filter(|f| f.size >= MIN_SIZE && bn.file(&f.name).is_none()) {
            added.files.push(Entry { path: path.join(&*f.name), size: f.size });
        }
        for f in bn.files.iter().filter(|f| f.size >= MIN_SIZE && an.file(&f.name).is_none()) {
            removed.files.push(Entry { path: path.join(&*f.name), size: f.size });
        }
    }
    Ok(Moves::new(pair(added, removed)))
}

/// Records a folder that appeared or vanished, and every large folder and
/// file inside it: a folder moved into a new folder is only found below it.
fn collect(store: &Store, side: &mut Side, path: PathBuf, hash: Hash, size: u64, cancel: &dyn Fn() -> bool) -> Result<()> {
    if size < MIN_SIZE {
        return Ok(());
    }
    let mut stack = vec![(path, hash, size)];
    while let Some((path, hash, size)) = stack.pop() {
        if cancel() {
            return Ok(());
        }
        let n = store.node(&hash)?;
        for f in n.files.iter().filter(|f| f.size >= MIN_SIZE) {
            side.files.push(Entry { path: path.join(&*f.name), size: f.size });
        }
        for d in n.dirs.iter().filter(|d| d.size >= MIN_SIZE) {
            stack.push((path.join(&*d.name), d.hash, d.size));
        }
        side.dirs.push((hash, Entry { path, size }));
    }
    Ok(())
}

/// Pairs what appeared with what vanished, largest first, one to one. A
/// matched folder claims everything inside it on both sides.
fn pair(added: Side, removed: Side) -> Vec<Move> {
    let inside = |claimed: &HashSet<PathBuf>, p: &Path| p.ancestors().any(|a| claimed.contains(a));
    let name = |p: &Path| p.file_name().map(|n| n.to_owned());
    let (mut took_a, mut took_b) = (HashSet::new(), HashSet::new());
    let mut out = Vec::new();

    let mut by_hash: HashMap<Hash, Vec<&Entry>> = HashMap::new();
    for (h, e) in &removed.dirs {
        by_hash.entry(*h).or_default().push(e);
    }
    let mut dirs: Vec<_> = added.dirs.iter().collect();
    // Largest first; at equal size the outer folder comes first.
    dirs.sort_by(|x, y| y.1.size.cmp(&x.1.size).then(x.1.path.components().count().cmp(&y.1.path.components().count())));
    for (h, e) in dirs {
        if inside(&took_a, &e.path) {
            continue;
        }
        let Some(cands) = by_hash.get(h) else { continue };
        let free = |c: &&&Entry| !inside(&took_b, &c.path) && !took_b.iter().any(|t: &PathBuf| t.starts_with(&c.path));
        let pick = cands.iter().filter(free).find(|c| name(&c.path) == name(&e.path)).or_else(|| cands.iter().find(free));
        if let Some(c) = pick {
            took_a.insert(e.path.clone());
            took_b.insert(c.path.clone());
            out.push(Move { from: c.path.clone(), to: e.path.clone(), is_dir: true, size: e.size });
        }
    }

    let mut by_key: HashMap<(Option<std::ffi::OsString>, u64), Vec<&Entry>> = HashMap::new();
    for e in removed.files.iter().filter(|e| !inside(&took_b, &e.path)) {
        by_key.entry((name(&e.path), e.size)).or_default().push(e);
    }
    let mut files: Vec<_> = added.files.iter().filter(|e| !inside(&took_a, &e.path)).collect();
    files.sort_by(|x, y| y.size.cmp(&x.size).then_with(|| x.path.cmp(&y.path)));
    for e in files {
        let Some(cands) = by_key.get_mut(&(name(&e.path), e.size)) else { continue };
        if cands.is_empty() {
            continue;
        }
        let c = cands.remove(0);
        out.push(Move { from: c.path.clone(), to: e.path.clone(), is_dir: false, size: e.size });
    }
    out
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
    fn copy_and_changed_folder_are_not_moves() {
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
        // The changed folder's large file still matches by name and size.
        assert_eq!(m.list.len(), 1, "{:?}", m.list);
        assert!(!m.list[0].is_dir);
        assert!(m.list[0].to.ends_with("C/edited/y.bin"));
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
