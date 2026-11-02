//! Directory nodes: the unit of storage.
//!
//! A scan is stored as a Merkle tree, like a git tree object. Every directory
//! becomes one node listing its children (sub-directories by hash, files by
//! name and size), and the node is keyed by the hash of its encoding. A
//! directory whose contents did not change between two scans therefore has the
//! same hash in both, so it is stored once and shared, together with its whole
//! subtree. A new scan only adds the directories that changed (and their
//! ancestors), and comparing two scans can skip every subtree whose hashes
//! match.

use anyhow::{Context, Result, bail};
use xxhash_rust::xxh3::xxh3_128;

pub type Hash = [u8; 16];

/// The directory could not be read (permission denied, vanished mid-scan…), so
/// its size is a lower bound.
pub const FLAG_UNREADABLE: u8 = 1;
/// The directory is a package / bundle (`.app`, `.photoslibrary`…) that macOS
/// shows as a single file.
pub const FLAG_PACKAGE: u8 = 2;
/// A cloud placeholder folder: its contents live online, not on this disk,
/// so it was deliberately not opened.
pub const FLAG_CLOUD: u8 = 4;
/// The node carries bytes/files that are not itemized: a folder total without
/// a listing of every file.
pub const FLAG_OTHER: u8 = 0x40;
/// Storage-internal: this node is an index of chunk nodes (see [`chunk`]).
pub const FLAG_INDEX: u8 = 0x80;

/// Directories with more entries than this are stored as chunks.
pub const CHUNK_THRESHOLD: usize = 1024;
const CHUNK_MIN: usize = 64;
const CHUNK_MAX: usize = 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirChild {
    pub name: Box<str>,
    pub hash: Hash,
    /// Recursive allocated size in bytes.
    pub size: u64,
    /// Recursive number of files.
    pub files: u64,
    /// Recursive number of directories, not counting itself.
    pub dirs: u64,
    pub flags: u8,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileChild {
    pub name: Box<str>,
    /// Allocated size in bytes (hard links counted once per scan).
    pub size: u64,
}

/// One directory's listing. Both vectors are sorted by name (byte order), which
/// makes the encoding, and so the hash, independent of read order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Node {
    pub dirs: Vec<DirChild>,
    pub files: Vec<FileChild>,
    pub flags: u8,
    /// Bytes and files inside this directory (not below it) that are counted
    /// but not listed individually.
    pub other_size: u64,
    pub other_files: u64,
}

/// Totals of a node, what its parent records about it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Summary {
    pub hash: Hash,
    pub size: u64,
    pub files: u64,
    pub dirs: u64,
    pub flags: u8,
}

impl Node {
    pub fn sort(&mut self) {
        self.dirs.sort_unstable_by(|a, b| a.name.cmp(&b.name));
        self.files.sort_unstable_by(|a, b| a.name.cmp(&b.name));
    }

    /// Encodes the node; returns the bytes and the summary its parent stores.
    /// Call [`Node::sort`] first.
    pub fn encode(&self) -> (Vec<u8>, Summary) {
        let mut out = Vec::with_capacity(16 + self.dirs.len() * 40 + self.files.len() * 24);
        let other = self.other_size > 0 || self.other_files > 0;
        let flags = if other { self.flags | FLAG_OTHER } else { self.flags & !FLAG_OTHER };
        out.push(flags);
        if other {
            put_varint(&mut out, self.other_size);
            put_varint(&mut out, self.other_files);
        }
        put_varint(&mut out, self.dirs.len() as u64);
        let (mut size, mut files, mut dirs) =
            (self.other_size, self.files.len() as u64 + self.other_files, self.dirs.len() as u64);
        for d in &self.dirs {
            put_str(&mut out, &d.name);
            out.extend_from_slice(&d.hash);
            put_varint(&mut out, d.size);
            put_varint(&mut out, d.files);
            put_varint(&mut out, d.dirs);
            out.push(d.flags);
            size += d.size;
            files += d.files;
            dirs += d.dirs;
        }
        put_varint(&mut out, self.files.len() as u64);
        for f in &self.files {
            put_str(&mut out, &f.name);
            put_varint(&mut out, f.size);
            size += f.size;
        }
        let hash = xxh3_128(&out).to_le_bytes();
        (out, Summary { hash, size, files, dirs, flags: self.flags & !FLAG_OTHER })
    }

    pub fn decode(bytes: &[u8]) -> Result<Node> {
        let mut r = Reader { b: bytes, i: 0 };
        let flags = r.byte()?;
        let (mut other_size, mut other_files) = (0, 0);
        if flags & FLAG_OTHER != 0 {
            other_size = r.varint()?;
            other_files = r.varint()?;
        }
        let nd = r.varint()? as usize;
        let mut dirs = Vec::with_capacity(nd.min(1 << 16));
        for _ in 0..nd {
            let name = r.str()?;
            let hash: Hash = r.take(16)?.try_into().unwrap();
            let size = r.varint()?;
            let files = r.varint()?;
            let ndirs = r.varint()?;
            let flags = r.byte()?;
            dirs.push(DirChild { name, hash, size, files, dirs: ndirs, flags });
        }
        let nf = r.varint()? as usize;
        let mut files = Vec::with_capacity(nf.min(1 << 16));
        for _ in 0..nf {
            let name = r.str()?;
            let size = r.varint()?;
            files.push(FileChild { name, size });
        }
        Ok(Node { dirs, files, flags: flags & !FLAG_OTHER, other_size, other_files })
    }

    /// Child hashes a stored node references: sub-directories, or the chunks
    /// of an index node. Names are skipped, not decoded.
    pub fn child_hashes(bytes: &[u8]) -> Result<Vec<Hash>> {
        if let Some(chunks) = index_chunks(bytes)? {
            return Ok(chunks);
        }
        let mut r = Reader { b: bytes, i: 0 };
        if r.byte()? & FLAG_OTHER != 0 {
            r.varint()?;
            r.varint()?;
        }
        let nd = r.varint()? as usize;
        let mut out = Vec::with_capacity(nd.min(1 << 16));
        for _ in 0..nd {
            let n = r.varint()? as usize;
            r.take(n)?;
            out.push(r.take(16)?.try_into().unwrap());
            r.varint()?;
            r.varint()?;
            r.varint()?;
            r.byte()?;
        }
        Ok(out)
    }

    pub fn dir(&self, name: &str) -> Option<&DirChild> {
        self.dirs.binary_search_by(|d| (*d.name).cmp(name)).ok().map(|i| &self.dirs[i])
    }

    pub fn file(&self, name: &str) -> Option<&FileChild> {
        self.files.binary_search_by(|f| (*f.name).cmp(name)).ok().map(|i| &self.files[i])
    }
}

/// An index node's chunk hashes, or `None` for an ordinary node.
pub fn index_chunks(bytes: &[u8]) -> Result<Option<Vec<Hash>>> {
    if bytes.first().is_none_or(|f| f & FLAG_INDEX == 0) {
        return Ok(None);
    }
    let mut r = Reader { b: bytes, i: 1 };
    let n = r.varint()? as usize;
    let mut out = Vec::with_capacity(n.min(1 << 16));
    for _ in 0..n {
        out.push(r.take(16)?.try_into().unwrap());
    }
    Ok(Some(out))
}

/// One stored piece of a large directory.
pub struct Piece {
    pub hash: Hash,
    pub raw: Vec<u8>,
}

/// Splits a large node into chunk nodes plus an index node that lists them,
/// so a change to one entry rewrites one chunk (~200 entries) instead of the
/// whole listing. Chunk boundaries are content-defined (they depend on the
/// names around them, not on positions), so inserting or removing an entry
/// does not shift every later chunk.
///
/// Returns the pieces to store, chunks first and the index last, and the
/// summary of the whole directory. Small nodes come back as a single piece.
pub fn chunk(node: &Node) -> (Vec<Piece>, Summary) {
    let (raw, summary) = node.encode();
    if node.dirs.len() + node.files.len() <= CHUNK_THRESHOLD {
        return (vec![Piece { hash: summary.hash, raw }], summary);
    }
    let mut pieces = Vec::new();
    let push = |dirs: &[DirChild], files: &[FileChild], pieces: &mut Vec<Piece>| {
        let part = Node { dirs: dirs.to_vec(), files: files.to_vec(), ..Default::default() };
        let (raw, s) = part.encode();
        pieces.push(Piece { hash: s.hash, raw });
    };
    for range in boundaries(node.dirs.iter().map(|d| &*d.name)) {
        push(&node.dirs[range], &[], &mut pieces);
    }
    for range in boundaries(node.files.iter().map(|f| &*f.name)) {
        push(&[], &node.files[range], &mut pieces);
    }
    // Unitemized totals ride in the first chunk.
    if node.other_size > 0 || node.other_files > 0 {
        let first = Node { other_size: node.other_size, other_files: node.other_files, ..Default::default() };
        let (raw, s) = first.encode();
        pieces.insert(0, Piece { hash: s.hash, raw });
    }
    let mut index = vec![node.flags | FLAG_INDEX];
    put_varint(&mut index, pieces.len() as u64);
    for p in &pieces {
        index.extend_from_slice(&p.hash);
    }
    // The directory's identity is its index: equal listings give equal
    // chunks, so equal indexes.
    let hash = xxh3_128(&index).to_le_bytes();
    pieces.push(Piece { hash, raw: index });
    (pieces, Summary { hash, ..summary })
}

fn boundaries<'a>(names: impl Iterator<Item = &'a str>) -> Vec<std::ops::Range<usize>> {
    let mut out = Vec::new();
    let (mut start, mut i) = (0, 0);
    for name in names {
        i += 1;
        let len = i - start;
        if len >= CHUNK_MAX || (len >= CHUNK_MIN && xxh3_128(name.as_bytes()) % 128 == 0) {
            out.push(start..i);
            start = i;
        }
    }
    if start < i {
        out.push(start..i);
    }
    out
}

/// Joins chunk nodes back into one listing.
pub fn join(flags: u8, parts: Vec<Node>) -> Node {
    let mut n = Node { flags: flags & !FLAG_INDEX, ..Default::default() };
    for p in parts {
        n.dirs.extend(p.dirs);
        n.files.extend(p.files);
        n.other_size += p.other_size;
        n.other_files += p.other_files;
    }
    n
}

/// Small nodes are stored raw, larger ones zstd-compressed. One tag byte says which.
pub fn compress(raw: &[u8]) -> Vec<u8> {
    if raw.len() < 96 {
        let mut v = Vec::with_capacity(raw.len() + 1);
        v.push(0);
        v.extend_from_slice(raw);
        return v;
    }
    let mut v = vec![1u8];
    v.extend_from_slice(&zstd::bulk::compress(raw, 3).expect("zstd compress"));
    v
}

pub fn decompress(stored: &[u8]) -> Result<Vec<u8>> {
    match stored.first() {
        Some(0) => Ok(stored[1..].to_vec()),
        Some(1) => {
            let mut d = zstd::bulk::Decompressor::new()?;
            let cap = zstd::zstd_safe::get_frame_content_size(&stored[1..])
                .ok()
                .flatten()
                .unwrap_or(1 << 20) as usize;
            Ok(d.decompress(&stored[1..], cap.max(1))?)
        }
        _ => bail!("unknown node encoding"),
    }
}

fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push((v as u8) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

fn put_str(out: &mut Vec<u8>, s: &str) {
    put_varint(out, s.len() as u64);
    out.extend_from_slice(s.as_bytes());
}

struct Reader<'a> {
    b: &'a [u8],
    i: usize,
}

impl<'a> Reader<'a> {
    fn byte(&mut self) -> Result<u8> {
        let v = *self.b.get(self.i).context("truncated node")?;
        self.i += 1;
        Ok(v)
    }
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let s = self.b.get(self.i..self.i + n).context("truncated node")?;
        self.i += n;
        Ok(s)
    }
    fn varint(&mut self) -> Result<u64> {
        let mut v = 0u64;
        let mut shift = 0;
        loop {
            let b = self.byte()?;
            v |= u64::from(b & 0x7f) << shift;
            if b & 0x80 == 0 {
                return Ok(v);
            }
            shift += 7;
            if shift > 63 {
                bail!("bad varint");
            }
        }
    }
    fn str(&mut self) -> Result<Box<str>> {
        let n = self.varint()? as usize;
        let s = self.take(n)?;
        Ok(String::from_utf8_lossy(s).into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_stable_hash() {
        let mut n = Node {
            dirs: vec![DirChild { name: "b".into(), hash: [7; 16], size: 10, files: 2, dirs: 1, flags: 0 }],
            files: vec![
                FileChild { name: "z.txt".into(), size: 4096 },
                FileChild { name: "a.txt".into(), size: 8192 },
            ],
            ..Default::default()
        };
        n.sort();
        let (bytes, s) = n.encode();
        assert_eq!(s.size, 10 + 4096 + 8192);
        assert_eq!(s.files, 4);
        assert_eq!(s.dirs, 2);
        let back = Node::decode(&decompress(&compress(&bytes)).unwrap()).unwrap();
        assert_eq!(back, n);
        assert_eq!(Node::child_hashes(&bytes).unwrap(), vec![[7; 16]]);
        let (_, s2) = back.encode();
        assert_eq!(s.hash, s2.hash);
    }

    #[test]
    fn chunked_nodes_roundtrip_and_localise_changes() {
        let mut n = Node::default();
        for i in 0..5000 {
            n.files.push(FileChild { name: format!("f{i:05}").into(), size: i });
        }
        n.dirs.push(DirChild { name: "d".into(), hash: [3; 16], size: 1, files: 1, dirs: 0, flags: 0 });
        n.sort();
        let (pieces, s) = chunk(&n);
        assert!(pieces.len() > 5);
        assert_eq!(s.files, 5001);
        let index = &pieces.last().unwrap().raw;
        let chunks = index_chunks(index).unwrap().unwrap();
        assert_eq!(Node::child_hashes(index).unwrap(), chunks);
        let parts: Vec<Node> = pieces[..pieces.len() - 1].iter().map(|p| Node::decode(&p.raw).unwrap()).collect();
        assert_eq!(join(index[0], parts), n);
        // Changing one file rewrites one chunk; inserting one shifts at most two.
        let mut m = n.clone();
        m.files[2500].size += 1;
        m.files.push(FileChild { name: "f01234a".into(), size: 9 });
        m.sort();
        let (p2, s2) = chunk(&m);
        assert_ne!(s.hash, s2.hash);
        let old: std::collections::HashSet<_> = pieces.iter().map(|p| p.hash).collect();
        let new = p2.iter().filter(|p| !old.contains(&p.hash)).count();
        assert!(new <= 3, "{new} new pieces");
    }

    #[test]
    fn unitemized_totals_roundtrip() {
        let n = Node { other_size: 5000, other_files: 3, files: vec![FileChild { name: "a".into(), size: 7 }], ..Default::default() };
        let (bytes, s) = n.encode();
        assert_eq!((s.size, s.files), (5007, 4));
        assert_eq!(Node::decode(&bytes).unwrap(), n);
        assert!(Node::child_hashes(&bytes).unwrap().is_empty());
    }

    #[test]
    fn large_nodes_compress() {
        let mut n = Node::default();
        for i in 0..500 {
            n.files.push(FileChild { name: format!("file-{i:05}.log").into(), size: 4096 * i });
        }
        n.sort();
        let (bytes, _) = n.encode();
        let c = compress(&bytes);
        assert!(c.len() < bytes.len() / 2);
        assert_eq!(decompress(&c).unwrap(), bytes);
    }
}
