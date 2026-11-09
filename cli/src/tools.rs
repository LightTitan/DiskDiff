//! The tools: definitions (name, description, schema, annotations) and the
//! handlers, each a thin wrapper over an engine function.
//!
//! Results are JSON objects with exact byte counts plus `*_human` strings,
//! and every list is capped so one call cannot flood an agent's context.

use crate::format::{bytes, delta, iso};
use crate::scan;
use crate::select::{self, ScanRef};
use anyhow::{Context as _, Result, anyhow, bail};
use diskdiff_engine::insights::{self, FindingKind};
use diskdiff_engine::moves::{self, Moves};
use diskdiff_engine::{Item, ScanInfo, Status, Store, query};
use serde_json::{Map, Value, json};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// How the server finds the store and whether it may involve the app.
#[derive(Clone, Debug)]
pub struct Config {
    pub store: PathBuf,
    /// Ask a running DiskDiff app to scan (it has Full Disk Access; the
    /// server only has whatever the agent's app has).
    pub use_app: bool,
    /// Start the app when it is not running.
    pub launch_app: bool,
}

impl Config {
    /// From the environment: `DISKDIFF_DB` for the store,
    /// `DISKDIFF_MCP_NO_APP=1` to never talk to or start the app.
    pub fn from_env() -> Config {
        let no_app = std::env::var("DISKDIFF_MCP_NO_APP").is_ok_and(|v| !v.is_empty() && v != "0");
        Config { store: diskdiff_engine::platform::store_path(), use_app: !no_app, launch_app: !no_app }
    }
}

/// Server state shared by every call.
pub struct Ctx {
    pub config: Config,
    store: Mutex<Option<Arc<Store>>>,
    /// The scan this server runs itself when no app is available.
    pub(crate) local: Mutex<Option<scan::LocalJob>>,
}

impl Ctx {
    pub fn new(config: Config) -> Ctx {
        Ctx { config, store: Mutex::new(None), local: Mutex::new(None) }
    }

    /// The store, opened once with `open_shared` so temporary scans the app
    /// shows are left alone. Never creates a store just to read it.
    pub fn store(&self) -> Result<Arc<Store>> {
        let mut slot = self.store.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(s) = &*slot {
            return Ok(s.clone());
        }
        if !self.config.store.exists() {
            bail!("no scans yet; scan a folder first (run_scan)");
        }
        let s = Arc::new(
            Store::open_shared(&self.config.store)
                .with_context(|| format!("opening the DiskDiff store at {}", self.config.store.display()))?,
        );
        *slot = Some(s.clone());
        Ok(s)
    }
}

pub enum CallError {
    /// No such tool: a protocol error, not a tool result.
    Unknown(String),
    /// The tool ran and failed; the message is for the model.
    Failed(String),
}

pub fn call(ctx: &Ctx, name: &str, args: &Value) -> Result<Value, CallError> {
    let f: fn(&Ctx, &Args) -> Result<Value> = match name {
        "list_scans" => list_scans,
        "browse" => browse,
        "largest_files" => largest_files,
        "largest_folders" => largest_folders,
        "what_changed" => what_changed,
        "cleanup_suggestions" => cleanup_suggestions,
        "growth" => growth,
        "run_scan" => run_scan,
        "scan_status" => scan_status,
        _ => return Err(CallError::Unknown(name.into())),
    };
    let def = definitions().into_iter().find(|d| d["name"] == name).expect("every tool has a definition");
    let allowed: Vec<String> =
        def["inputSchema"]["properties"].as_object().map(|p| p.keys().cloned().collect()).unwrap_or_default();
    let run = || -> Result<Value> {
        let a = Args::parse(args, &allowed)?;
        if let Some(req) = def["inputSchema"]["required"].as_array() {
            for r in req.iter().filter_map(Value::as_str) {
                if a.get(r).is_none() {
                    bail!("missing required argument `{r}`");
                }
            }
        }
        f(ctx, &a)
    };
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(run)) {
        Ok(Ok(v)) => Ok(v),
        Ok(Err(e)) => Err(CallError::Failed(format!("{e:#}"))),
        Err(_) => Err(CallError::Failed(format!("{name} failed unexpectedly (see the server's log)"))),
    }
}

// ---- arguments ----------------------------------------------------------

/// A call's arguments, checked against the schema's property names.
pub struct Args(Map<String, Value>);

impl Args {
    fn parse(v: &Value, allowed: &[String]) -> Result<Args> {
        let map = match v {
            Value::Null => Map::new(),
            Value::Object(m) => m.clone(),
            _ => bail!("arguments must be a JSON object"),
        };
        if let Some(k) = map.keys().find(|k| !allowed.contains(k)) {
            bail!("unknown argument `{k}`; this tool takes: {}", allowed.join(", "));
        }
        Ok(Args(map))
    }

    /// `null` counts as absent: some clients send every optional field.
    fn get(&self, k: &str) -> Option<&Value> {
        self.0.get(k).filter(|v| !v.is_null())
    }

    fn str(&self, k: &str) -> Result<Option<&str>> {
        self.get(k).map(|v| v.as_str().ok_or_else(|| anyhow!("`{k}` must be a string"))).transpose()
    }

    fn u64(&self, k: &str) -> Result<Option<u64>> {
        self.get(k)
            .map(|v| {
                v.as_u64()
                    .or_else(|| v.as_f64().filter(|f| *f >= 0.0 && f.fract() == 0.0).map(|f| f as u64))
                    .ok_or_else(|| anyhow!("`{k}` must be a non-negative integer"))
            })
            .transpose()
    }

    fn bool(&self, k: &str) -> Result<Option<bool>> {
        self.get(k).map(|v| v.as_bool().ok_or_else(|| anyhow!("`{k}` must be true or false"))).transpose()
    }

    fn limit(&self, k: &str, default: usize, max: usize) -> Result<usize> {
        Ok(self.u64(k)?.map_or(default, |n| (n as usize).clamp(1, max)))
    }

    /// A scan id, or "latest" / "previous" (and "none" where `allow_none`).
    fn scan(&self, k: &str, allow_none: bool) -> Result<Option<ScanRef>> {
        let Some(v) = self.get(k) else { return Ok(None) };
        let r = if let Some(id) = v.as_i64() {
            ScanRef::Id(id)
        } else {
            match v.as_str().map(str::trim) {
                Some("latest") => ScanRef::Latest,
                Some("previous") => ScanRef::Previous,
                Some("none") if allow_none => ScanRef::None,
                Some(s) if s.parse::<i64>().is_ok() => ScanRef::Id(s.parse()?),
                _ => {
                    let none = if allow_none { ", \"none\"" } else { "" };
                    bail!("`{k}` must be a scan id (see list_scans), \"latest\" or \"previous\"{none}")
                }
            }
        };
        Ok(Some(r))
    }
}

// ---- shared JSON shapes -------------------------------------------------

/// Every field of a scan, for list_scans and finished scans.
pub fn scan_full(s: &ScanInfo) -> Value {
    json!({
        "id": s.id,
        "root": s.root,
        "started": iso(s.started_at),
        "started_unix": s.started_at,
        "finished": iso(s.finished_at),
        "finished_unix": s.finished_at,
        "duration_seconds": (s.finished_at - s.started_at).max(0),
        "size": s.size,
        "size_human": bytes(s.size),
        "files": s.files,
        "dirs": s.dirs,
        "disk_total": s.disk_total,
        "disk_total_human": bytes(s.disk_total),
        "disk_free": s.disk_free,
        "disk_free_human": bytes(s.disk_free),
        "unreadable": s.unreadable,
        "full_access": s.full_access,
        "temporary": s.temporary,
        "note": s.note,
    })
}

/// Which scan a result came from, without the totals.
fn scan_brief(s: &ScanInfo) -> Value {
    json!({ "id": s.id, "root": s.root, "finished": iso(s.finished_at), "temporary": s.temporary })
}

fn status_str(s: Status) -> &'static str {
    match s {
        Status::Same => "unchanged",
        Status::Changed => "changed",
        Status::Added => "added",
        Status::Removed => "removed",
        Status::NoHistory => "no_history",
        Status::Unreadable => "unreadable",
    }
}

fn kind_of(is_dir: bool) -> &'static str {
    if is_dir { "folder" } else { "file" }
}

fn path_str(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

fn size_obj(m: &mut Map<String, Value>, key: &str, n: u64) {
    m.insert(key.into(), json!(n));
    m.insert(format!("{key}_human"), json!(bytes(n)));
}

fn delta_obj(m: &mut Map<String, Value>, key: &str, d: i64) {
    m.insert(key.into(), json!(d));
    m.insert(format!("{key}_human"), json!(delta(d)));
}

/// The size from which a change is worth listing: 0.5 % of the scan,
/// rounded to 1/2/5 MB steps between 1 MB and 50 MB, as the app does.
fn change_threshold(cur: &ScanInfo) -> u64 {
    let raw = (cur.size / 200).clamp(1_000_000, 50_000_000);
    let mut step = 1_000_000u64;
    let mut best = step;
    while step <= raw {
        for m in [1, 2, 5] {
            if step * m <= raw {
                best = step * m;
            }
        }
        step *= 10;
    }
    best
}

fn detect_moves(store: &Store, cur: &ScanInfo, cmp: Option<&ScanInfo>) -> Result<Moves> {
    match cmp {
        Some(c) => moves::detect(store, cur, c, &|| false),
        None => Ok(Moves::default()),
    }
}

// ---- handlers -----------------------------------------------------------

fn list_scans(ctx: &Ctx, a: &Args) -> Result<Value> {
    let store = ctx.store()?;
    let mut scans = store.scans()?;
    let limit = a.limit("limit", 50, 500)?;
    if let Some(root) = a.str("root")? {
        let abs = match select::parse_path(root)? {
            select::PathArg::Abs(p) => p,
            select::PathArg::Rel(_) => bail!("`root` must be an absolute path"),
        };
        scans.retain(|s| select::covers(s, &abs));
    }
    // Locations first: the overview an agent usually wants.
    let mut locations: Vec<(String, usize, i64)> = Vec::new();
    for s in &scans {
        match locations.iter_mut().find(|l| l.0 == s.root) {
            Some(l) => {
                l.1 += 1;
                l.2 = l.2.max(s.finished_at);
            }
            None => locations.push((s.root.clone(), 1, s.finished_at)),
        }
    }
    let total = scans.len();
    scans.reverse();
    scans.truncate(limit);
    Ok(json!({
        "count": total,
        "shown": scans.len(),
        "locations": locations.iter().map(|(root, n, last)| json!({"root": root, "scans": n, "last_scan": iso(*last)})).collect::<Vec<_>>(),
        "scans": scans.iter().map(scan_full).collect::<Vec<_>>(),
    }))
}

fn browse(ctx: &Ctx, a: &Args) -> Result<Value> {
    let store = ctx.store()?;
    let scans = store.scans()?;
    let (cur, abs) = select::locate(&scans, a.scan("scan", false)?, a.str("path")?)?;
    let cmp = select::compare(&scans, cur, a.scan("compare_to", true)?)?;
    let limit = a.limit("limit", 50, 500)?;
    let moves = detect_moves(&store, cur, cmp)?;
    // A folder that arrived by a move is compared with where it was.
    let prev_abs = moves.before(&abs).unwrap_or_else(|| abs.clone());
    let mut l = query::list_against(&store, cur, cmp, &abs, &prev_abs)?;
    if !l.exists && !l.existed {
        if let Some(n) = query::size_at(&store, cur, &abs)?.filter(|&n| n > 0) {
            bail!("{} is a file of {} in scan {}, not a folder; browse its parent", abs.display(), bytes(n), cur.id);
        }
        bail!("{} is not in scan {} (of {}); browse a folder inside it", abs.display(), cur.id, cur.root);
    }
    if cmp.is_some() {
        query::apply_moves(&mut l.items, &abs, &moves);
    }
    // Biggest first, removed items by what they used to take.
    l.items.sort_by_key(|i| std::cmp::Reverse(if i.status == Status::Removed { i.prev.unwrap_or(0) } else { i.size }));
    let comparing = cmp.is_some();
    let total = l.items.len();
    let rest = l.items.split_off(total.min(limit));

    let mut out = Map::new();
    out.insert("path".into(), json!(path_str(&abs)));
    out.insert("scan".into(), scan_brief(cur));
    out.insert("compare_to".into(), cmp.map_or(Value::Null, scan_brief));
    if prev_abs != abs {
        out.insert("previously_at".into(), json!(path_str(&prev_abs)));
    }
    out.insert("exists".into(), json!(l.exists));
    size_obj(&mut out, "size", l.size);
    if let Some(p) = l.prev.filter(|_| comparing) {
        size_obj(&mut out, "prev_size", p);
        delta_obj(&mut out, "delta", l.size as i64 - p as i64);
    }
    out.insert("items".into(), json!(l.items.iter().map(|i| item_json(i, &abs, comparing)).collect::<Vec<_>>()));
    if !rest.is_empty() {
        let mut more = Map::new();
        more.insert("count".into(), json!(rest.len()));
        size_obj(&mut more, "size", rest.iter().map(|i| i.size).sum());
        out.insert("more".into(), Value::Object(more));
    }
    Ok(Value::Object(out))
}

fn item_json(it: &Item, dir: &Path, comparing: bool) -> Value {
    let mut m = Map::new();
    m.insert("name".into(), json!(&*it.name));
    let kind = if it.placeholder {
        "unitemized"
    } else if it.package {
        "package"
    } else {
        kind_of(it.is_dir)
    };
    m.insert("kind".into(), json!(kind));
    if !it.placeholder {
        m.insert("path".into(), json!(path_str(&dir.join(&*it.name))));
    }
    size_obj(&mut m, "size", it.size);
    if comparing {
        if it.has_delta() {
            if let Some(p) = it.prev {
                m.insert("prev_size".into(), json!(p));
            }
            delta_obj(&mut m, "delta", it.delta());
            if it.moved_bytes != 0 {
                delta_obj(&mut m, "moved_bytes", it.moved_bytes);
                delta_obj(&mut m, "change", it.change());
            }
        }
        let status = if it.moved.is_some() { "moved" } else { status_str(it.status) };
        m.insert("status".into(), json!(status));
        if let Some(mv) = &it.moved {
            let key = if mv.arrived { "moved_from" } else { "moved_to" };
            m.insert(key.into(), json!(path_str(&mv.other)));
        }
    }
    if it.is_dir {
        m.insert("files".into(), json!(it.files));
        m.insert("dirs".into(), json!(it.dirs));
    }
    if it.unreadable {
        m.insert("unreadable".into(), json!(true));
    }
    if it.cloud {
        m.insert("cloud_placeholder".into(), json!(true));
    }
    Value::Object(m)
}

fn largest_files(ctx: &Ctx, a: &Args) -> Result<Value> {
    let store = ctx.store()?;
    let scans = store.scans()?;
    let (cur, abs) = select::locate(&scans, a.scan("scan", false)?, a.str("path")?)?;
    let limit = a.limit("limit", 25, 200)?;
    let files = query::largest_files(&store, cur, &abs, limit)?;
    if files.is_empty() && store.resolve(&cur.root_hash, &query::rel_in(cur, &abs).unwrap_or_default())?.is_none() {
        bail!("{} is not a folder in scan {} (of {})", abs.display(), cur.id, cur.root);
    }
    Ok(json!({
        "path": path_str(&abs),
        "scan": scan_brief(cur),
        "files": files.iter().map(|(p, n)| json!({"path": path_str(p), "size": n, "size_human": bytes(*n)})).collect::<Vec<_>>(),
    }))
}

fn largest_folders(ctx: &Ctx, a: &Args) -> Result<Value> {
    let store = ctx.store()?;
    let scans = store.scans()?;
    let cur = select::pick(&scans, a.scan("scan", false)?, None)?;
    let limit = a.limit("limit", 25, 200)?;
    let folders = insights::largest_folders(&store, cur, limit)?;
    Ok(json!({
        "scan": scan_brief(cur),
        "note": "A folder that is 90% or more one subfolder is left out in favour of that subfolder.",
        "folders": folders.iter().map(|f| json!({
            "path": path_str(&f.path), "size": f.size, "size_human": bytes(f.size), "files": f.files, "dirs": f.dirs,
        })).collect::<Vec<_>>(),
    }))
}

fn what_changed(ctx: &Ctx, a: &Args) -> Result<Value> {
    let store = ctx.store()?;
    let scans = store.scans()?;
    let path = a.str("path")?;
    let (cur, abs) = select::locate(&scans, a.scan("to", false)?, path)?;
    let cmp = select::compare(&scans, cur, a.scan("from", false)?)?.ok_or_else(|| {
        anyhow!(
            "there is no earlier saved scan of {} to compare scan {} with; pass `from` with a scan id from list_scans, or scan again later with temporary=false to start a history",
            cur.root,
            cur.id
        )
    })?;
    let min = a.u64("min_size")?.unwrap_or_else(|| change_threshold(cur));
    let limit = a.limit("limit", 30, 200)?;

    let mut out = Map::new();
    out.insert("from".into(), scan_brief(cmp));
    out.insert("to".into(), scan_brief(cur));
    size_obj(&mut out, "min_size", min);

    let (cr, pr) = (Path::new(&cur.root), Path::new(&cmp.root));
    let base = if cr.starts_with(pr) { cr } else { pr };
    let whole = path.is_none() || abs == base || base.starts_with(&abs);
    let moves = detect_moves(&store, cur, Some(cmp))?;
    let (rows, moved_list): (Vec<Value>, Vec<&moves::Move>) = if whole {
        if !cr.starts_with(pr) && !pr.starts_with(cr) {
            bail!("scans {} ({}) and {} ({}) do not overlap", cur.id, cur.root, cmp.id, cmp.root);
        }
        let sum = insights::changes(&store, cur, cmp, min, limit)?;
        out.insert("path".into(), json!(path_str(base)));
        delta_obj(&mut out, "total_delta", sum.total_delta);
        size_obj(&mut out, "added", sum.added);
        size_obj(&mut out, "removed", sum.removed);
        size_obj(&mut out, "moved", sum.moved);
        out.insert(
            "summary".into(),
            json!(format!(
                "Between {} and {}, {} changed by {}: {} added, {} removed, {} moved within it.",
                iso(cmp.finished_at),
                iso(cur.finished_at),
                base.display(),
                delta(sum.total_delta),
                bytes(sum.added),
                bytes(sum.removed),
                bytes(sum.moved)
            )),
        );
        (sum.changes.iter().map(change_json).collect(), moves.list.iter().collect())
    } else {
        let (now, then) = (query::size_at(&store, cur, &abs)?, query::size_at(&store, cmp, &abs)?);
        let (Some(now), Some(then)) = (now, then) else {
            bail!("{} is not covered by both scan {} ({}) and scan {} ({})", abs.display(), cur.id, cur.root, cmp.id, cmp.root);
        };
        out.insert("path".into(), json!(path_str(&abs)));
        delta_obj(&mut out, "total_delta", now as i64 - then as i64);
        out.insert(
            "summary".into(),
            json!(format!(
                "Between {} and {}, {} went from {} to {} ({}).",
                iso(cmp.finished_at),
                iso(cur.finished_at),
                abs.display(),
                bytes(then),
                bytes(now),
                delta(now as i64 - then as i64)
            )),
        );
        let under: Vec<&moves::Move> =
            moves.list.iter().filter(|m| m.from.starts_with(&abs) || m.to.starts_with(&abs)).collect();
        let mut v = query::changes(&store, cur, cmp, &abs, limit * 2)?;
        v.retain(|c| c.delta().unsigned_abs() >= min);
        v.truncate(limit);
        for c in &mut v {
            if c.status == Status::Added
                && let Some(m) = moves.to(&c.path)
            {
                c.moved_from = Some(m.from.clone());
                c.moved_bytes = m.size as i64;
            }
        }
        let rows = v
            .iter()
            .map(|c| {
                let mut row = change_json(c);
                if c.status == Status::Removed
                    && let Some(m) = moves.from(&c.path)
                {
                    row["status"] = json!("moved");
                    row["moved_to"] = json!(path_str(&m.to));
                }
                row
            })
            .collect();
        (rows, under)
    };
    out.insert("changes".into(), json!(rows));
    let mut by_size = moved_list;
    by_size.sort_by_key(|m| std::cmp::Reverse(m.size));
    let mut mv = Map::new();
    mv.insert("count".into(), json!(by_size.len()));
    size_obj(&mut mv, "bytes", by_size.iter().map(|m| m.size).sum());
    mv.insert(
        "largest".into(),
        json!(
            by_size
                .iter()
                .take(10)
                .map(|m| json!({
                    "from": path_str(&m.from), "to": path_str(&m.to), "kind": kind_of(m.is_dir),
                    "size": m.size, "size_human": bytes(m.size), "size_after": m.after,
                }))
                .collect::<Vec<_>>()
        ),
    );
    out.insert("moves".into(), Value::Object(mv));
    Ok(Value::Object(out))
}

fn change_json(c: &query::Change) -> Value {
    let mut m = Map::new();
    m.insert("path".into(), json!(path_str(&c.path)));
    m.insert("kind".into(), json!(kind_of(c.is_dir)));
    m.insert("status".into(), json!(if c.moved_from.is_some() { "moved" } else { status_str(c.status) }));
    size_obj(&mut m, "size", c.size);
    m.insert("prev_size".into(), json!(c.prev));
    delta_obj(&mut m, "delta", c.delta());
    if let Some(from) = &c.moved_from {
        m.insert("moved_from".into(), json!(path_str(from)));
    }
    if c.moved_bytes != 0 {
        delta_obj(&mut m, "moved_bytes", c.moved_bytes);
        delta_obj(&mut m, "change", c.change());
    }
    Value::Object(m)
}

fn cleanup_suggestions(ctx: &Ctx, a: &Args) -> Result<Value> {
    let store = ctx.store()?;
    let scans = store.scans()?;
    let cur = select::pick(&scans, a.scan("scan", false)?, None)?;
    let limit = a.limit("limit", 15, 30)?;
    let mut findings = insights::suggestions(&store, &scans, cur)?;
    let total = findings.len();
    findings.truncate(limit);
    let rows: Vec<Value> = findings
        .iter()
        .map(|f| {
            let mut m = Map::new();
            let (kind, why) = match f.kind {
                FindingKind::Regenerable => ("regenerable", "A cache or dependency folder that its tool rebuilds on demand."),
                FindingKind::Duplicate => (
                    "duplicate",
                    "The same folder (same file names and sizes, not compared byte for byte) in several places.",
                ),
                FindingKind::Untouched => ("untouched", "A large folder that has not changed across recent scans."),
            };
            m.insert("kind".into(), json!(kind));
            m.insert("why".into(), json!(why));
            m.insert("path".into(), json!(path_str(&f.path)));
            size_obj(&mut m, "size", f.size);
            size_obj(&mut m, "reclaimable", f.reclaimable);
            m.insert("files".into(), json!(f.files));
            if !f.category.is_empty() {
                m.insert("category".into(), json!(f.category));
            }
            if !f.hint.is_empty() {
                m.insert("hint".into(), json!(f.hint));
            }
            if !f.copies.is_empty() {
                m.insert("copies".into(), json!(f.copies.iter().take(10).map(|p| path_str(p)).collect::<Vec<_>>()));
                if f.copies.len() > 10 {
                    m.insert("more_copies".into(), json!(f.copies.len() - 10));
                }
            }
            if let Some(t) = f.unchanged_since {
                m.insert("unchanged_since".into(), json!(iso(t)));
            }
            if f.unchanged_scans > 0 {
                m.insert("unchanged_scans".into(), json!(f.unchanged_scans));
                m.insert("unchanged_since_first_scan".into(), json!(f.since_first_scan));
            }
            if f.untouched_children > 0 {
                m.insert("untouched_subfolders".into(), json!(f.untouched_children));
            }
            if !f.examples.is_empty() {
                m.insert("examples".into(), json!(f.examples));
            }
            Value::Object(m)
        })
        .collect();
    let mut out = Map::new();
    out.insert("scan".into(), scan_brief(cur));
    out.insert(
        "note".into(),
        json!(
            "Candidates to check, not instructions. Verify each with browse or growth, consider whether the person still needs it, and explain what removing it would mean. This server never deletes anything."
        ),
    );
    size_obj(&mut out, "total_reclaimable", findings.iter().map(|f| f.reclaimable).sum());
    out.insert("count".into(), json!(total));
    out.insert("suggestions".into(), json!(rows));
    Ok(Value::Object(out))
}

fn growth(ctx: &Ctx, a: &Args) -> Result<Value> {
    let store = ctx.store()?;
    let scans = store.scans()?;
    let (cur, abs) = select::locate(&scans, a.scan("scan", false)?, a.str("path")?)?;
    let limit = a.limit("limit", 30, 200)?;
    // Every saved scan up to `cur` that covers the path, whatever its root.
    let mut points: Vec<(&ScanInfo, u64)> = Vec::new();
    for s in scans.iter().filter(|s| {
        (s.id == cur.id || !s.temporary) && (s.started_at, s.id) <= (cur.started_at, cur.id) && select::covers(s, &abs)
    }) {
        if let Some(n) = query::size_at(&store, s, &abs)? {
            points.push((s, n));
        }
    }
    let total = points.len();
    let points = &points[total.saturating_sub(limit)..];
    let rows: Vec<Value> = points
        .iter()
        .enumerate()
        .map(|(i, (s, n))| {
            let mut m = Map::new();
            m.insert("scan".into(), json!(s.id));
            m.insert("finished".into(), json!(iso(s.finished_at)));
            size_obj(&mut m, "size", *n);
            if i > 0 {
                delta_obj(&mut m, "delta", *n as i64 - points[i - 1].1 as i64);
            }
            Value::Object(m)
        })
        .collect();

    let mut out = Map::new();
    out.insert("path".into(), json!(path_str(&abs)));
    out.insert("scans_covering".into(), json!(total));
    out.insert("points".into(), json!(rows));
    if let (Some(first), Some(last)) = (points.first(), points.last())
        && points.len() > 1
    {
        let d = last.1 as i64 - first.1 as i64;
        delta_obj(&mut out, "change", d);
        let days = (last.0.finished_at - first.0.finished_at) as f64 / 86_400.0;
        out.insert("days".into(), json!((days * 10.0).round() / 10.0));
        if days >= 1.0 {
            delta_obj(&mut out, "change_per_day", (d as f64 / days) as i64);
        }
        // Churn shows busy folders even when their net change is small.
        let prev = points[points.len() - 2].0;
        let churn = query::activity(&store, last.0, prev, &abs, &|| false)?;
        let net: std::collections::HashMap<_, _> = query::list(&store, last.0, Some(prev), &abs)?
            .items
            .into_iter()
            .map(|i| (i.name.clone(), i.delta()))
            .collect();
        let mut churn: Vec<_> = churn.into_iter().filter(|(_, c)| *c > 0).collect();
        churn.sort_by_key(|(_, c)| std::cmp::Reverse(*c));
        out.insert(
            "churn_since_previous".into(),
            json!(
                churn
                    .iter()
                    .take(10)
                    .map(|(name, c)| {
                        let mut m = Map::new();
                        m.insert("path".into(), json!(path_str(&abs.join(&**name))));
                        size_obj(&mut m, "churn", *c);
                        delta_obj(&mut m, "net", net.get(name).copied().unwrap_or(0));
                        Value::Object(m)
                    })
                    .collect::<Vec<_>>()
            ),
        );
    }
    Ok(Value::Object(out))
}

fn run_scan(ctx: &Ctx, a: &Args) -> Result<Value> {
    let raw = a.str("path")?.unwrap_or_default();
    let path = match select::parse_path(raw)? {
        select::PathArg::Abs(p) => p,
        select::PathArg::Rel(_) => bail!("`path` must be absolute (or start with ~)"),
    };
    let temporary = a.bool("temporary")?.unwrap_or(true);
    let wait = a.u64("wait_seconds")?.unwrap_or(0).min(scan::MAX_WAIT);
    scan::run(ctx, &path, temporary, wait)
}

fn scan_status(ctx: &Ctx, a: &Args) -> Result<Value> {
    scan::status(ctx, a.u64("job")?)
}

// ---- definitions --------------------------------------------------------

fn scan_prop(desc: &str, none: bool) -> Value {
    let mut words = vec!["latest", "previous"];
    if none {
        words.push("none");
    }
    json!({ "anyOf": [{"type": "integer"}, {"type": "string", "enum": words}], "description": desc })
}

fn limit_prop(default: u64, max: u64) -> Value {
    json!({ "type": "integer", "minimum": 1, "maximum": max, "description": format!("How many to return (default {default}, at most {max}).") })
}

fn tool(name: &str, title: &str, description: &str, props: Value, required: &[&str], read_only: bool) -> Value {
    json!({
        "name": name,
        "title": title,
        "description": description,
        "inputSchema": { "type": "object", "properties": props, "required": required, "additionalProperties": false },
        "annotations": {
            "title": title,
            "readOnlyHint": read_only,
            "destructiveHint": false,
            "idempotentHint": read_only,
            "openWorldHint": false,
        },
    })
}

pub fn definitions() -> Vec<Value> {
    const PATH: &str = "Absolute folder path (~ allowed), or a path relative to the scan's root.";
    let scan = || scan_prop("Scan id from list_scans, or \"latest\" (default) / \"previous\".", false);
    vec![
        tool(
            "list_scans",
            "List scans",
            "Lists DiskDiff's stored scans, newest first: when each ran, which folder, its total size, file and folder counts, the volume's capacity and free space, unreadable folders, whether it had full disk access, and whether it is temporary. Start here to see which locations have a history.",
            json!({
                "root": {"type": "string", "description": "Only scans that include this absolute path."},
                "limit": limit_prop(50, 500),
            }),
            &[],
            true,
        ),
        tool(
            "browse",
            "Browse folder",
            "Lists a folder's contents as of a scan, biggest first, with each item's change since an earlier scan: status added, removed, changed, unchanged or moved (with where from/to). By default compares the latest scan containing the path with the previous saved scan of the same location. Results are capped; `more` counts the rest.",
            json!({
                "path": {"type": "string", "description": "Folder to list. Defaults to the scan's root. Absolute (~ allowed) or relative to the scan's root."},
                "scan": scan(),
                "compare_to": scan_prop("Scan to compare with: an id, \"previous\" (default: the saved scan before `scan`), \"latest\", or \"none\" for no comparison.", true),
                "limit": limit_prop(50, 500),
            }),
            &[],
            true,
        ),
        tool(
            "largest_files",
            "Largest files",
            "The largest individual files under a folder in one scan, biggest first.",
            json!({
                "path": {"type": "string", "description": format!("{PATH} Defaults to the scan's root.")},
                "scan": scan(),
                "limit": limit_prop(25, 200),
            }),
            &[],
            true,
        ),
        tool(
            "largest_folders",
            "Largest folders",
            "The largest folders anywhere in a scan, biggest first. A folder that is almost entirely one subfolder is skipped in favour of that subfolder, so the list is not a chain of parents.",
            json!({ "scan": scan(), "limit": limit_prop(25, 200) }),
            &[],
            true,
        ),
        tool(
            "what_changed",
            "What changed",
            "What grew, shrank, appeared, disappeared or moved between two scans: totals (added, removed, moved bytes) and the biggest individual changes, folders that appeared or vanished as a whole reported once. Moves are told apart from real growth. Defaults: `to` is the latest scan, `from` the previous saved scan of the same location. Pass `path` to look only under one folder.",
            json!({
                "from": scan_prop("Earlier scan: an id or \"previous\" (default).", false),
                "to": scan_prop("Later scan: an id, \"latest\" (default) or \"previous\".", false),
                "path": {"type": "string", "description": format!("Only changes under this folder. {PATH}")},
                "min_size": {"type": "integer", "minimum": 0, "description": "Smallest change in bytes to list (default scales with the scan: 0.5% of it, 1 MB to 50 MB)."},
                "limit": limit_prop(30, 200),
            }),
            &[],
            true,
        ),
        tool(
            "cleanup_suggestions",
            "Cleanup suggestions",
            "Candidates for freeing space in a scan, ranked by what they could free: regenerable caches and dependency folders (with why they are rebuildable), duplicated folders (matched by names and sizes, not content), and large folders untouched across recent scans. These are leads to verify and explain to the person, not instructions to delete: check each with browse or growth, say what it is and what removing it would cost, and let the person decide. Nothing here deletes anything.",
            json!({ "scan": scan(), "limit": limit_prop(15, 30) }),
            &[],
            true,
        ),
        tool(
            "growth",
            "Folder growth",
            "How one folder's size evolved across every saved scan that includes it (any scan whose root contains it), with the change per scan and per day, plus which of its subfolders churned most since the previous scan (bytes written and deleted, even when the net change is small).",
            json!({
                "path": {"type": "string", "description": PATH},
                "scan": scan_prop("Last scan to include: an id or \"latest\" (default).", false),
                "limit": limit_prop(30, 200),
            }),
            &["path"],
            true,
        ),
        tool(
            "run_scan",
            "Scan a folder",
            "Scans a folder and saves the result in DiskDiff's history. Runs in the DiskDiff app when it is installed (starting it in the background if needed, so the scan has the app's disk permissions), otherwise inside this server. Only reads the disk. Returns at once with a job id unless `wait_seconds` is set; follow it with scan_status, then call what_changed. Scanning a whole disk can take minutes, and some clients time out tool calls after about a minute, so prefer short waits and polling.",
            json!({
                "path": {"type": "string", "description": "Absolute folder path to scan (~ allowed)."},
                "temporary": {"type": "boolean", "description": "true (default): a one-off scan that replaces the previous temporary one and is discarded when DiskDiff quits. false: keep it in the folder's history for later comparisons."},
                "wait_seconds": {"type": "integer", "minimum": 0, "maximum": scan::MAX_WAIT, "description": "Wait up to this many seconds for the scan to finish (default 0)."},
            }),
            &["path"],
            false,
        ),
        tool(
            "scan_status",
            "Scan status",
            "Progress of the running scan (files, folders and bytes so far, the volume's used bytes as an estimate of the total, whether it is saving), or the result of the last one.",
            json!({ "job": {"type": "integer", "minimum": 0, "description": "Job id from run_scan. Defaults to the running or last scan."} }),
            &[],
            true,
        ),
    ]
}
