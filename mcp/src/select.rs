//! Which scans and paths a tool call means: ids or "latest"/"previous",
//! and paths as agents type them (`~/Downloads`, trailing slashes, relative).

use anyhow::{Result, anyhow, bail};
use diskdiff_engine::ScanInfo;
use diskdiff_engine::query::rel_in;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScanRef {
    Id(i64),
    Latest,
    Previous,
    /// No comparison (only where a comparison is optional).
    None,
}

/// Whether `s` holds `abs`.
pub fn covers(s: &ScanInfo, abs: &Path) -> bool {
    rel_in(s, abs).is_some()
}

fn order(s: &ScanInfo) -> (i64, i64) {
    (s.started_at, s.id)
}

/// The scan a tool should read. `scans` is in store order (oldest first).
/// Without an id: the latest scan covering `abs` (or the latest of all).
pub fn pick<'a>(scans: &'a [ScanInfo], spec: Option<ScanRef>, abs: Option<&Path>) -> Result<&'a ScanInfo> {
    if scans.is_empty() {
        bail!("no scans yet; scan a folder first (run_scan)");
    }
    let latest = || {
        scans.iter().filter(|s| abs.is_none_or(|p| covers(s, p))).max_by_key(|s| order(s)).ok_or_else(|| {
            let p = abs.map_or_else(String::new, |p| p.display().to_string());
            anyhow!("no scan covers {p}; scanned locations: {}. Use run_scan to scan it.", locations(scans))
        })
    };
    match spec.unwrap_or(ScanRef::Latest) {
        ScanRef::Id(id) => {
            let s = by_id(scans, id)?;
            if let Some(p) = abs
                && !covers(s, p)
            {
                bail!("scan {id} is of {}, which does not contain {}", s.root, p.display());
            }
            Ok(s)
        }
        ScanRef::Latest | ScanRef::None => latest(),
        ScanRef::Previous => {
            let cur = latest()?;
            default_compare(scans, cur)
                .ok_or_else(|| anyhow!("there is no saved scan of {} before scan {}", cur.root, cur.id))
        }
    }
}

/// The scan to compare `cur` with. Defaults to [`default_compare`]; `None`
/// when asked for no comparison or there is nothing earlier.
pub fn compare<'a>(scans: &'a [ScanInfo], cur: &ScanInfo, spec: Option<ScanRef>) -> Result<Option<&'a ScanInfo>> {
    match spec {
        None => Ok(default_compare(scans, cur)),
        Some(ScanRef::None) => Ok(None),
        Some(ScanRef::Previous) => default_compare(scans, cur)
            .map(Some)
            .ok_or_else(|| anyhow!("there is no saved scan of {} before scan {}", cur.root, cur.id)),
        Some(ScanRef::Latest) => {
            let s = scans.iter().filter(|s| s.id != cur.id).max_by_key(|s| order(s));
            s.map(Some).ok_or_else(|| anyhow!("there is no other scan to compare with"))
        }
        Some(ScanRef::Id(id)) => {
            let s = by_id(scans, id)?;
            if s.id == cur.id {
                bail!("cannot compare scan {id} with itself");
            }
            Ok(Some(s))
        }
    }
}

/// The saved scan before `cur`: of the same folder if there is one, else of
/// a folder containing it (a full-disk scan still has the numbers). Unlike
/// the app this also answers for a temporary `cur`: an agent's quick scan is
/// most useful compared with the last saved one.
pub fn default_compare<'a>(scans: &'a [ScanInfo], cur: &ScanInfo) -> Option<&'a ScanInfo> {
    let earlier = || scans.iter().filter(|s| !s.temporary && s.id != cur.id && order(s) < order(cur));
    earlier()
        .filter(|s| s.root == cur.root)
        .max_by_key(|s| order(s))
        .or_else(|| earlier().filter(|s| covers(s, Path::new(&cur.root))).max_by_key(|s| order(s)))
}

pub fn by_id(scans: &[ScanInfo], id: i64) -> Result<&ScanInfo> {
    scans.iter().find(|s| s.id == id).ok_or_else(|| anyhow!("no scan with id {id}; call list_scans for the ids"))
}

/// Distinct scanned roots, for error messages.
pub fn locations(scans: &[ScanInfo]) -> String {
    let mut roots: Vec<&str> = scans.iter().map(|s| s.root.as_str()).collect();
    roots.sort_unstable();
    roots.dedup();
    if roots.len() > 10 {
        let n = roots.len() - 10;
        roots.truncate(10);
        return format!("{} and {n} more", roots.join(", "));
    }
    roots.join(", ")
}

/// A path argument: absolute after `~` expansion, or relative to a scan root.
pub enum PathArg {
    Abs(PathBuf),
    Rel(PathBuf),
}

pub fn parse_path(raw: &str) -> Result<PathArg> {
    let raw = raw.trim();
    if raw.is_empty() {
        bail!("path is empty");
    }
    let p = expand_home(raw);
    Ok(if p.is_absolute() { PathArg::Abs(normalize(&p)) } else { PathArg::Rel(p) })
}

fn expand_home(raw: &str) -> PathBuf {
    let home = || std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(PathBuf::from);
    if raw == "~" {
        return home().unwrap_or_else(|| raw.into());
    }
    if let Some(rest) = raw.strip_prefix("~/").or_else(|| raw.strip_prefix("~\\"))
        && let Some(h) = home()
    {
        return h.join(rest);
    }
    PathBuf::from(raw)
}

/// Spells a path the way scans store roots: canonical (symlinks such as
/// /tmp → /private/tmp resolved, `\\?\` on Windows). A path that no longer
/// exists is resolved through its nearest existing ancestor, so a deleted
/// folder still matches its old scans.
pub fn normalize(p: &Path) -> PathBuf {
    let mut rest = Vec::new();
    let mut cur = p;
    loop {
        if let Ok(c) = cur.canonicalize() {
            return rest.iter().rev().fold(c, |acc, n| acc.join(n));
        }
        match (cur.parent(), cur.file_name()) {
            (Some(parent), Some(name)) => {
                rest.push(name.to_owned());
                cur = parent;
            }
            _ => return p.components().collect(),
        }
    }
}

/// The scan and absolute path a call means. A relative path is taken inside
/// the chosen scan's root; no path means the root itself.
pub fn locate<'a>(scans: &'a [ScanInfo], spec: Option<ScanRef>, path: Option<&str>) -> Result<(&'a ScanInfo, PathBuf)> {
    match path.map(parse_path).transpose()? {
        Some(PathArg::Abs(abs)) => Ok((pick(scans, spec, Some(&abs))?, abs)),
        Some(PathArg::Rel(rel)) => {
            let s = pick(scans, spec, None)?;
            Ok((s, Path::new(&s.root).join(rel)))
        }
        None => {
            let s = pick(scans, spec, None)?;
            Ok((s, PathBuf::from(&s.root)))
        }
    }
}
