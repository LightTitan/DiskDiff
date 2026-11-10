//! `diskdiff-cli export`: a whole scan, or every change between two scans,
//! as tab-separated lines with no limit, for agents that can pipe the output
//! into grep, sort or a file.

use crate::select::{self, ScanRef};
use crate::tools::Ctx;
use anyhow::{Result, anyhow, bail};
use diskdiff_engine::{Status, query};
use std::io::{self, Write};

/// Command-line options of `export`.
#[derive(Default)]
pub struct Options {
    pub path: Option<String>,
    pub scan: Option<String>,
    /// Export the changes since this scan instead of the scan's contents.
    pub from: Option<String>,
}

impl Options {
    pub fn parse(args: &[String]) -> Result<Options> {
        let mut o = Options::default();
        let mut it = args.iter();
        while let Some(a) = it.next() {
            let mut value = |name: &str| it.next().cloned().ok_or_else(|| anyhow!("{name} needs a value"));
            match a.as_str() {
                "--scan" => o.scan = Some(value("--scan")?),
                "--from" | "--changes" => o.from = Some(value(a)?),
                s if s.starts_with("--") => bail!("unknown option {s}"),
                s if o.path.is_none() => o.path = Some(s.to_owned()),
                s => bail!("unexpected argument {s}"),
            }
        }
        Ok(o)
    }
}

fn scan_ref(raw: &str) -> Result<ScanRef> {
    Ok(match raw {
        "latest" => ScanRef::Latest,
        "previous" => ScanRef::Previous,
        s => ScanRef::Id(s.parse().map_err(|_| anyhow!("a scan is an id from list_scans, \"latest\" or \"previous\", not {s:?}"))?),
    })
}

/// Writes the export. Stops quietly when the reader goes away (`| head`).
pub fn run(ctx: &Ctx, o: &Options, out: &mut dyn Write) -> Result<()> {
    match write(ctx, o, out) {
        Err(e) if e.downcast_ref::<io::Error>().is_some_and(|e| e.kind() == io::ErrorKind::BrokenPipe) => Ok(()),
        r => r,
    }
}

fn write(ctx: &Ctx, o: &Options, out: &mut dyn Write) -> Result<()> {
    let store = ctx.store()?;
    let scans = store.scans()?;
    let (cur, abs) = select::locate(&scans, o.scan.as_deref().map(scan_ref).transpose()?, o.path.as_deref())?;
    let mut out = io::BufWriter::new(out);
    if let Some(from) = &o.from {
        let cmp = select::compare(&scans, cur, Some(scan_ref(from)?))?
            .ok_or_else(|| anyhow!("no scan to compare scan {} with", cur.id))?;
        writeln!(out, "# changes under {} from scan {} to scan {}, largest first", abs.display(), cmp.id, cur.id)?;
        writeln!(out, "delta\tsize\tprev_size\tstatus\tkind\tpath")?;
        for c in query::changes(&store, cur, cmp, &abs, usize::MAX)? {
            let status = match c.status {
                Status::Added => "added",
                Status::Removed => "removed",
                _ => "changed",
            };
            let kind = if c.is_dir { "folder" } else { "file" };
            writeln!(out, "{}\t{}\t{}\t{status}\t{kind}\t{}", c.delta(), c.size, c.prev, c.path.display())?;
        }
    } else {
        writeln!(out, "# every folder and file under {} in scan {}", abs.display(), cur.id)?;
        writeln!(out, "size\tkind\tfiles\tpath")?;
        let mut err = None;
        let found = query::walk(&store, cur, &abs, &mut |e| {
            let kind = if e.is_dir { "folder" } else { "file" };
            match writeln!(out, "{}\t{kind}\t{}\t{}", e.size, e.files, e.path.display()) {
                Ok(()) => true,
                Err(x) => {
                    err = Some(x);
                    false
                }
            }
        })?;
        if let Some(e) = err {
            return Err(e.into());
        }
        if !found {
            bail!("{} is not a folder in scan {} (of {})", abs.display(), cur.id, cur.root);
        }
    }
    out.flush()?;
    Ok(())
}
