//! The snapshot store: content-addressed directory nodes in SQLite.
//!
//! * `nodes` holds each distinct directory listing once, keyed by its hash,
//!   with a reference count: how many stored parents (and scans, for roots)
//!   point at it.
//! * `scans` holds one row per scan with its root hash and totals.
//!
//! Saving a scan inserts only nodes whose hash is not stored yet. Deleting a
//! scan releases its root; a node whose count drops to zero is deleted and
//! releases its children in turn, so the work is proportional to what that one
//! scan owned exclusively, never to the size of the tree.

use crate::node::{self, Hash, Node};
use anyhow::{Context, Result, bail};
use parking_lot::Mutex;
use rusqlite::{Connection, OptionalExtension, params};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

const SCHEMA_VERSION: i64 = 1;

#[derive(Clone, Debug, Default)]
pub struct ScanInfo {
    pub id: i64,
    /// Absolute path that was scanned.
    pub root: String,
    /// Unix seconds.
    pub started_at: i64,
    pub finished_at: i64,
    pub root_hash: Hash,
    pub size: u64,
    pub files: u64,
    pub dirs: u64,
    /// Capacity and free space of the volume holding `root`, at scan time.
    pub disk_total: u64,
    pub disk_free: u64,
    /// Directories that could not be read.
    pub unreadable: u64,
    /// Bytes this scan added to the store when it was saved.
    pub added_bytes: u64,
    /// Whether the scan ran with full disk access (macOS) / elevated (Windows).
    pub full_access: bool,
    /// A one-off scan, not part of any history; deleted on the next launch.
    pub temporary: bool,
    pub note: String,
}

impl ScanInfo {
    pub fn disk_used(&self) -> u64 {
        self.disk_total.saturating_sub(self.disk_free)
    }
}

#[derive(Clone, Debug)]
pub struct LargeFile {
    /// Path relative to the scan root, `/`-separated.
    pub path: String,
    pub size: u64,
}

#[derive(Clone, Debug, Default)]
pub struct StoreStats {
    pub file_bytes: u64,
    pub nodes: u64,
    pub node_bytes: u64,
    pub scans: u64,
}

pub struct Store {
    path: PathBuf,
    conn: Mutex<Connection>,
    cache: Mutex<NodeCache>,
}

impl Store {
    pub fn open(path: &Path) -> Result<Store> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let conn = open_conn(path)?;
        migrate(&conn)?;
        let store = Store { path: path.to_owned(), conn: Mutex::new(conn), cache: Mutex::new(NodeCache::new(4096)) };
        // Temporary scans only live for one session of the app.
        let leftovers = store.temporary_ids()?;
        if !leftovers.is_empty() {
            store.delete_scans(&leftovers)?;
        }
        Ok(store)
    }

    pub fn temporary_ids(&self) -> Result<Vec<i64>> {
        let conn = self.conn.lock();
        let mut st = conn.prepare("SELECT id FROM scans WHERE temporary = 1")?;
        let ids = st.query_map([], |r| r.get(0))?.collect::<rusqlite::Result<Vec<i64>>>()?;
        Ok(ids)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// A second connection for a scan's writer thread.
    pub(crate) fn writer(&self) -> Result<NodeWriter> {
        let conn = open_conn(&self.path)?;
        conn.execute_batch("BEGIN IMMEDIATE")?;
        Ok(NodeWriter { conn, added: 0, inserted: 0, done: false })
    }

    pub fn scans(&self) -> Result<Vec<ScanInfo>> {
        let conn = self.conn.lock();
        let mut st = conn.prepare(
            "SELECT id, root, started_at, finished_at, root_hash, size, files, dirs, disk_total, disk_free,
                    unreadable, added_bytes, full_access, temporary, note FROM scans ORDER BY started_at, id",
        )?;
        let rows = st.query_map([], |r| {
            let h: Vec<u8> = r.get(4)?;
            Ok(ScanInfo {
                id: r.get(0)?,
                root: r.get(1)?,
                started_at: r.get(2)?,
                finished_at: r.get(3)?,
                root_hash: h.try_into().unwrap_or([0; 16]),
                size: r.get::<_, i64>(5)? as u64,
                files: r.get::<_, i64>(6)? as u64,
                dirs: r.get::<_, i64>(7)? as u64,
                disk_total: r.get::<_, i64>(8)? as u64,
                disk_free: r.get::<_, i64>(9)? as u64,
                unreadable: r.get::<_, i64>(10)? as u64,
                added_bytes: r.get::<_, i64>(11)? as u64,
                full_access: r.get(12)?,
                temporary: r.get(13)?,
                note: r.get(14)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn largest_files(&self, scan: i64) -> Result<Vec<LargeFile>> {
        let conn = self.conn.lock();
        let blob: Option<Vec<u8>> =
            conn.query_row("SELECT largest FROM scans WHERE id = ?1", [scan], |r| r.get(0)).optional()?;
        let Some(blob) = blob else { return Ok(Vec::new()) };
        let raw = node::decompress(&blob)?;
        Ok(decode_largest(&raw))
    }

    pub fn set_note(&self, scan: i64, note: &str) -> Result<()> {
        self.conn.lock().execute("UPDATE scans SET note = ?2 WHERE id = ?1", params![scan, note])?;
        Ok(())
    }

    /// Loads a node by hash, through a small LRU cache.
    pub fn node(&self, hash: &Hash) -> Result<Arc<Node>> {
        if let Some(n) = self.cache.lock().get(hash) {
            return Ok(n);
        }
        let raw = self.raw(hash)?;
        let n = match node::index_chunks(&raw)? {
            Some(chunks) => {
                let parts = chunks.iter().map(|c| Node::decode(&self.raw(c)?)).collect::<Result<Vec<_>>>()?;
                Arc::new(node::join(raw[0], parts))
            }
            None => Arc::new(Node::decode(&raw)?),
        };
        self.cache.lock().put(*hash, n.clone());
        Ok(n)
    }

    fn raw(&self, hash: &Hash) -> Result<Vec<u8>> {
        let blob: Vec<u8> = self
            .conn
            .lock()
            .query_row("SELECT data FROM nodes WHERE hash = ?1", [&hash[..]], |r| r.get(0))
            .optional()?
            .context("node missing from store")?;
        node::decompress(&blob)
    }

    /// Resolves a `/`-separated path relative to a root node. `""` is the root.
    /// Returns `None` when the path does not exist in that tree.
    pub fn resolve(&self, root: &Hash, rel: &str) -> Result<Option<(Hash, Arc<Node>)>> {
        let mut hash = *root;
        let mut n = self.node(&hash)?;
        for part in rel.split('/').filter(|p| !p.is_empty()) {
            let Some(d) = n.dir(part) else { return Ok(None) };
            hash = d.hash;
            n = self.node(&hash)?;
        }
        Ok(Some((hash, n)))
    }

    /// Deletes scans and every node no remaining scan references.
    /// Returns the number of nodes freed.
    pub fn delete_scans(&self, ids: &[i64]) -> Result<u64> {
        let mut conn = self.conn.lock();
        let tx = conn.transaction()?;
        let mut freed = 0u64;
        {
            let mut get_root = tx.prepare("SELECT root_hash FROM scans WHERE id = ?1")?;
            let mut del_scan = tx.prepare("DELETE FROM scans WHERE id = ?1")?;
            let mut dec = tx.prepare("UPDATE nodes SET refs = refs - 1 WHERE hash = ?1 RETURNING refs, data")?;
            let mut del_node = tx.prepare("DELETE FROM nodes WHERE hash = ?1")?;
            for &id in ids {
                let Some(root): Option<Vec<u8>> = get_root.query_row([id], |r| r.get(0)).optional()? else {
                    continue;
                };
                del_scan.execute([id])?;
                let mut stack: Vec<Hash> = vec![root.try_into().unwrap_or([0; 16])];
                while let Some(h) = stack.pop() {
                    let row: Option<(i64, Vec<u8>)> =
                        dec.query_row([&h[..]], |r| Ok((r.get(0)?, r.get(1)?))).optional()?;
                    let Some((refs, data)) = row else { continue };
                    if refs <= 0 {
                        stack.extend(Node::child_hashes(&node::decompress(&data)?)?);
                        del_node.execute([&h[..]])?;
                        freed += 1;
                    }
                }
            }
        }
        tx.commit()?;
        self.cache.lock().clear();
        // Hand the freed pages back to the file system a slice at a time; the
        // rest is reused by later scans either way.
        let _ = conn.execute_batch("PRAGMA incremental_vacuum(20000)");
        Ok(freed)
    }

    /// Bytes deleting this scan would free: the nodes reachable only through it.
    pub fn exclusive_bytes(&self, scan: i64) -> Result<u64> {
        let conn = self.conn.lock();
        let root: Vec<u8> = conn.query_row("SELECT root_hash FROM scans WHERE id = ?1", [scan], |r| r.get(0))?;
        let mut st = conn.prepare_cached("SELECT refs, length(data), data FROM nodes WHERE hash = ?1")?;
        // Simulate the release in memory.
        let mut pending: HashMap<Hash, i64> = HashMap::new();
        let mut stack: Vec<Hash> = vec![root.try_into().unwrap_or([0; 16])];
        let mut bytes = 0u64;
        while let Some(h) = stack.pop() {
            let row: Option<(i64, i64, Vec<u8>)> =
                st.query_row([&h[..]], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).optional()?;
            let Some((refs, len, data)) = row else { continue };
            let left = pending.entry(h).or_insert(refs);
            *left -= 1;
            if *left == 0 {
                bytes += len as u64 + 40;
                stack.extend(Node::child_hashes(&node::decompress(&data)?)?);
            }
        }
        Ok(bytes)
    }

    pub fn stats(&self) -> Result<StoreStats> {
        let conn = self.conn.lock();
        let (nodes, node_bytes): (i64, i64) =
            conn.query_row("SELECT count(*), coalesce(sum(length(data)), 0) FROM nodes", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })?;
        let scans: i64 = conn.query_row("SELECT count(*) FROM scans", [], |r| r.get(0))?;
        let mut file_bytes = 0;
        for suffix in ["", "-wal"] {
            let mut p = self.path.clone().into_os_string();
            p.push(suffix);
            file_bytes += std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
        }
        Ok(StoreStats { file_bytes, nodes: nodes as u64, node_bytes: node_bytes as u64, scans: scans as u64 })
    }

    /// Rewrites the file to its minimum size. Slow on big stores; only on request.
    pub fn compact(&self) -> Result<()> {
        self.conn.lock().execute_batch("PRAGMA wal_checkpoint(TRUNCATE); VACUUM;")?;
        Ok(())
    }
}

fn open_conn(path: &Path) -> Result<Connection> {
    let conn = Connection::open(path).with_context(|| format!("open {}", path.display()))?;
    // auto_vacuum only takes effect before the first table is created.
    conn.execute_batch(
        "PRAGMA auto_vacuum = INCREMENTAL;
         PRAGMA journal_mode = WAL;
         PRAGMA synchronous = NORMAL;
         PRAGMA cache_size = -32000;
         PRAGMA busy_timeout = 10000;",
    )?;
    Ok(conn)
}

fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
         CREATE TABLE IF NOT EXISTS nodes (
             id INTEGER PRIMARY KEY,
             hash BLOB NOT NULL UNIQUE,
             refs INTEGER NOT NULL,
             data BLOB NOT NULL
         );
         CREATE TABLE IF NOT EXISTS scans (
             id INTEGER PRIMARY KEY,
             root TEXT NOT NULL,
             started_at INTEGER NOT NULL,
             finished_at INTEGER NOT NULL,
             root_hash BLOB NOT NULL,
             size INTEGER NOT NULL,
             files INTEGER NOT NULL,
             dirs INTEGER NOT NULL,
             disk_total INTEGER NOT NULL,
             disk_free INTEGER NOT NULL,
             unreadable INTEGER NOT NULL DEFAULT 0,
             added_bytes INTEGER NOT NULL DEFAULT 0,
             full_access INTEGER NOT NULL DEFAULT 1,
             temporary INTEGER NOT NULL DEFAULT 0,
             note TEXT NOT NULL DEFAULT '',
             largest BLOB
         );",
    )?;
    let v: Option<String> =
        conn.query_row("SELECT value FROM meta WHERE key = 'schema'", [], |r| r.get(0)).optional()?;
    match v.map(|s| s.parse::<i64>().unwrap_or(0)) {
        None => {
            conn.execute("INSERT INTO meta (key, value) VALUES ('schema', ?1)", [SCHEMA_VERSION.to_string()])?;
        }
        Some(SCHEMA_VERSION) => {}
        Some(other) => bail!("snapshot store has schema {other}, this build understands {SCHEMA_VERSION}"),
    }
    Ok(())
}

/// Writes one scan's nodes inside a single transaction, so a cancelled or
/// failed scan leaves nothing behind.
pub(crate) struct NodeWriter {
    conn: Connection,
    pub added: u64,
    pub inserted: u64,
    done: bool,
}

pub(crate) struct NewScan {
    pub root: String,
    pub started_at: i64,
    pub finished_at: i64,
    pub summary: node::Summary,
    pub disk_total: u64,
    pub disk_free: u64,
    pub unreadable: u64,
    pub full_access: bool,
    pub temporary: bool,
    pub largest: Vec<LargeFile>,
}

impl NodeWriter {
    /// Stores a node unless an identical one exists. Children must be written
    /// before their parent.
    pub fn put(&mut self, hash: &Hash, raw: &[u8]) -> Result<()> {
        let exists = self
            .conn
            .prepare_cached("SELECT 1 FROM nodes WHERE hash = ?1")?
            .exists([&hash[..]])?;
        if exists {
            return Ok(());
        }
        let stored = node::compress(raw);
        self.conn
            .prepare_cached("INSERT INTO nodes (hash, refs, data) VALUES (?1, 0, ?2)")?
            .execute(params![&hash[..], stored])?;
        self.added += stored.len() as u64 + 40;
        self.inserted += 1;
        let mut inc = self.conn.prepare_cached("UPDATE nodes SET refs = refs + 1 WHERE hash = ?1")?;
        for child in Node::child_hashes(raw)? {
            inc.execute([&child[..]])?;
        }
        Ok(())
    }

    pub fn finish(mut self, scan: NewScan) -> Result<i64> {
        let s = &scan.summary;
        self.conn.execute("UPDATE nodes SET refs = refs + 1 WHERE hash = ?1", [&s.hash[..]])?;
        let largest = node::compress(&encode_largest(&scan.largest));
        self.conn.execute(
            "INSERT INTO scans (root, started_at, finished_at, root_hash, size, files, dirs, disk_total,
                                disk_free, unreadable, added_bytes, full_access, largest, temporary)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
            params![
                scan.root,
                scan.started_at,
                scan.finished_at,
                &s.hash[..],
                s.size as i64,
                s.files as i64,
                s.dirs as i64,
                scan.disk_total as i64,
                scan.disk_free as i64,
                scan.unreadable as i64,
                self.added as i64,
                scan.full_access,
                largest,
                scan.temporary,
            ],
        )?;
        let id = self.conn.last_insert_rowid();
        self.conn.execute_batch("COMMIT")?;
        self.done = true;
        Ok(id)
    }
}

impl Drop for NodeWriter {
    fn drop(&mut self) {
        if !self.done {
            let _ = self.conn.execute_batch("ROLLBACK");
        }
    }
}

fn encode_largest(files: &[LargeFile]) -> Vec<u8> {
    let mut out = Vec::new();
    for f in files {
        out.extend_from_slice(&f.size.to_le_bytes());
        out.extend_from_slice(&(f.path.len() as u32).to_le_bytes());
        out.extend_from_slice(f.path.as_bytes());
    }
    out
}

fn decode_largest(mut b: &[u8]) -> Vec<LargeFile> {
    let mut out = Vec::new();
    while b.len() >= 12 {
        let size = u64::from_le_bytes(b[..8].try_into().unwrap());
        let n = u32::from_le_bytes(b[8..12].try_into().unwrap()) as usize;
        let Some(p) = b.get(12..12 + n) else { break };
        out.push(LargeFile { path: String::from_utf8_lossy(p).into_owned(), size });
        b = &b[12 + n..];
    }
    out
}

/// A tiny LRU for decoded nodes; browsing re-reads the same few constantly.
struct NodeCache {
    cap: usize,
    tick: u64,
    map: HashMap<Hash, (u64, Arc<Node>)>,
}

impl NodeCache {
    fn new(cap: usize) -> Self {
        NodeCache { cap, tick: 0, map: HashMap::new() }
    }
    fn get(&mut self, h: &Hash) -> Option<Arc<Node>> {
        self.tick += 1;
        let t = self.tick;
        self.map.get_mut(h).map(|e| {
            e.0 = t;
            e.1.clone()
        })
    }
    fn put(&mut self, h: Hash, n: Arc<Node>) {
        if self.map.len() >= self.cap {
            // Evict the older half in one pass instead of tracking order.
            let mut ticks: Vec<u64> = self.map.values().map(|e| e.0).collect();
            ticks.sort_unstable();
            let cut = ticks[ticks.len() / 2];
            self.map.retain(|_, e| e.0 > cut);
        }
        self.tick += 1;
        self.map.insert(h, (self.tick, n));
    }
    fn clear(&mut self) {
        self.map.clear();
    }
}
