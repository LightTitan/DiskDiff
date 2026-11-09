//! Text for people and agents who don't use MCP.
//!
//! * [`markdown`]: the Insights screens (what changed, cleanup suggestions,
//!   largest items) as one Markdown report to paste into any AI chat. The app's
//!   "Copy for AI" and `diskdiff-cli report` both produce it.
//! * [`guide`] / [`skill`]: how an agent with a terminal (Claude Code, Codex,
//!   Cursor…) uses `diskdiff-cli` as a plain command-line tool.

use crate::format::{bytes, delta};
use diskdiff_engine::insights::{self, Finding, FindingKind};
use diskdiff_engine::{ScanInfo, Status, Store};
use std::fmt::Write as _;
use std::path::Path;

/// Items per section: enough to talk about, small enough to paste.
const TOP: usize = 15;

/// The report for `cur`, with changes since `cmp` when given. `hidden` leaves
/// out suggestions the person dismissed in the app.
pub fn markdown(
    store: &Store,
    scans: &[ScanInfo],
    cur: &ScanInfo,
    cmp: Option<&ScanInfo>,
    hidden: &dyn Fn(&Finding) -> bool,
) -> anyhow::Result<String> {
    let root = Path::new(&cur.root);
    let rel = |p: &Path| -> String {
        match p.strip_prefix(root) {
            Ok(r) if r.as_os_str().is_empty() => ".".into(),
            Ok(r) => r.display().to_string(),
            Err(_) => p.display().to_string(),
        }
    };
    let mut out = String::new();
    let w = &mut out;

    let _ = writeln!(w, "# Disk report for {}\n", cur.root);
    let _ = writeln!(
        w,
        "From DiskDiff, which keeps a history of scans of this folder. Scanned {}: {} in {} files and {} folders. The disk holds {} with {} free.",
        day(cur.finished_at),
        bytes(cur.size),
        cur.files,
        cur.dirs,
        bytes(cur.disk_total),
        bytes(cur.disk_free),
    );
    if cur.unreadable > 0 {
        let _ = writeln!(w, "{} folders could not be read, so the real total may be higher.", cur.unreadable);
    }
    let _ = writeln!(
        w,
        "\nPaths below are relative to {}. Sizes are decimal (1 GB = 1,000,000,000 bytes). Nothing has been deleted: the cleanup suggestions are candidates to check, not instructions.",
        cur.root
    );

    if let Some(cmp) = cmp {
        let min = (cur.size / 10_000).max(1_000_000);
        let ch = insights::changes(store, cur, cmp, min, TOP)?;
        let _ = writeln!(w, "\n## What changed since {}\n", day(cmp.finished_at));
        let _ = writeln!(
            w,
            "In total {}: {} added, {} removed, {} moved within the folder.\n",
            delta(ch.total_delta),
            bytes(ch.added),
            bytes(ch.removed),
            bytes(ch.moved)
        );
        if ch.changes.is_empty() {
            let _ = writeln!(w, "Nothing of {} or more changed.", bytes(min));
        }
        for c in ch.changes.iter().take(TOP) {
            let kind = if c.is_dir { "folder" } else { "file" };
            if let Some(from) = &c.moved_from {
                // Moved, not new: the space was already used before.
                let _ = writeln!(w, "- `{}` ({kind}, {}, moved here from `{}`)", rel(&c.path), bytes(c.size), rel(from));
                continue;
            }
            let what = match c.status {
                Status::Added => "new",
                Status::Removed => "deleted",
                _ if c.delta() > 0 => "grew",
                _ => "shrank",
            };
            let _ = writeln!(w, "- {} `{}` ({kind}, {what}, now {})", delta(c.delta()), rel(&c.path), bytes(c.size));
        }
    }

    let findings = insights::suggestions(store, scans, cur)?;
    let shown: Vec<&Finding> = findings.iter().filter(|f| !hidden(f)).collect();
    let _ = writeln!(w, "\n## Cleanup suggestions\n");
    if shown.is_empty() {
        let _ = writeln!(w, "No large caches, duplicated folders or long-untouched folders turned up.");
    } else {
        let total: u64 = shown.iter().map(|f| f.reclaimable).sum();
        let _ = writeln!(w, "Up to {} could be reclaimed, if each one checks out:\n", bytes(total));
    }
    for f in shown.iter().take(TOP) {
        let _ = writeln!(w, "- {} `{}`: {}", bytes(f.reclaimable), rel(&f.path), finding_reason(f, &rel));
    }

    let folders = insights::largest_folders(store, cur, TOP)?;
    let _ = writeln!(w, "\n## Largest folders\n");
    for f in &folders {
        let files = if f.files == 1 { "1 file".to_owned() } else { format!("{} files", f.files) };
        let _ = writeln!(w, "- {} `{}` ({files})", bytes(f.size), rel(&f.path));
    }
    let mut files = store.largest_files(cur.id)?;
    files.sort_by_key(|f| std::cmp::Reverse(f.size));
    let _ = writeln!(w, "\n## Largest files\n");
    for f in files.iter().take(TOP) {
        let _ = writeln!(w, "- {} `{}`", bytes(f.size), rel(&f.abs(root)));
    }

    let _ = writeln!(
        w,
        "\n---\nPlease explain what takes up the most space, what grew and why it might have, and which of these I could safely remove (and how), ordered by how much each would free. Say when something is risky or needs a closer look first."
    );
    Ok(out)
}

fn finding_reason(f: &Finding, rel: &dyn Fn(&Path) -> String) -> String {
    match f.kind {
        FindingKind::Regenerable => {
            let mut s = f.category.to_owned();
            if !f.hint.is_empty() {
                s.push_str(". ");
                s.push_str(f.hint);
            }
            s
        }
        FindingKind::Duplicate => {
            let copies: Vec<String> = f.copies.iter().map(|c| format!("`{}`", rel(c))).collect();
            format!("the same {} folder is in {} places: {}", bytes(f.size), f.copies.len(), copies.join(", "))
        }
        FindingKind::Untouched => match f.unchanged_since {
            Some(t) if f.since_first_scan => {
                format!("unchanged in every scan since {} ({} scans)", day(t), f.unchanged_scans + 1)
            }
            Some(t) => format!("unchanged since {} ({} scans in a row)", day(t), f.unchanged_scans + 1),
            None => "large and not changed recently".into(),
        },
    }
}

fn day(unix: i64) -> String {
    crate::format::iso(unix).get(..10).unwrap_or_default().to_owned()
}

/// How `command` (the quoted path to `diskdiff-cli`, or an AppImage with `cli`)
/// is used from a terminal, for an agent's instructions.
pub fn guide(command: &str) -> String {
    let c = command;
    format!(
        "\
# DiskDiff

DiskDiff keeps a history of disk scans: every folder's and file's size at each scan, so two scans show what grew, shrank, appeared, vanished or moved. Query it with this command-line tool (`call` prints JSON):

- Overview of changes, cleanup suggestions and largest items: `{c} report [<path>]`
- The scanned locations and their scans: `{c} call list_scans`
- What grew, shrank, appeared or moved since the previous scan: `{c} call what_changed '{{\"path\": \"~/Downloads\"}}'`
- Caches, duplicated folders and long-untouched folders: `{c} call cleanup_suggestions`
- A folder's contents with sizes and changes: `{c} call browse '{{\"path\": \"~/Library\"}}'`
- The largest files: `{c} call largest_files '{{\"limit\": 20}}'`
- A folder's size over the scan history: `{c} call growth '{{\"path\": \"~/Library/Caches\"}}'`
- A fresh scan: `{c} call run_scan '{{\"path\": \"~\", \"wait_seconds\": 120}}'`
- Every tool with its arguments: `{c} tools`

Suggested flow: `report` or `list_scans` first, then `what_changed` and `cleanup_suggestions`; check candidates with `browse` or `growth` before recommending anything.
Cleanup suggestions are candidates to check and explain, not instructions. The tool never deletes or moves files: tell the person what they could remove, where it is, how big it is and why it is safe or not, and let them decide.
For fresh numbers use `run_scan` (temporary by default, so it doesn't join the folder's history). The DiskDiff app runs the scan if it is installed.
Sizes are decimal (1 GB = 10^9 bytes); each comes as exact bytes plus a *_human string.
Run these commands directly: the tool's `mcp` command starts an MCP server for MCP clients and isn't needed here.
"
    )
}

/// [`guide`] as a Claude Code skill (`~/.claude/skills/diskdiff/SKILL.md`).
pub fn skill(command: &str) -> String {
    format!(
        "---\nname: diskdiff\ndescription: \"Answer questions about disk space with DiskDiff's scan history: what takes up space, what grew since an earlier scan, and what could safely be cleaned up. Use when the person asks why their disk is full, where space went, or what to delete.\"\n---\n\n{}",
        guide(command)
    )
}

/// How to run this tool from a shell: its own path, quoted, or the AppImage
/// it was started from plus `cli` (the AppImage's mount is temporary).
pub fn own_command() -> String {
    if let Some(image) = std::env::var_os("APPIMAGE") {
        return format!("\"{}\" cli", Path::new(&image).display());
    }
    match std::env::current_exe() {
        Ok(exe) => format!("\"{}\"", exe.display()),
        Err(_) => "diskdiff-cli".into(),
    }
}
