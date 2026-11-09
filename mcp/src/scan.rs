//! Starting scans: in the DiskDiff app when there is one, else in-process.
//!
//! The app is preferred because on macOS Full Disk Access is granted per app:
//! a scan run here only sees what the agent's host (Claude, a terminal…) may
//! read. Only one process may write a scan at a time, so before scanning here
//! the server checks once more that no app answers.

use crate::format::{bytes, iso};
use crate::tools::{Ctx, scan_full};
use anyhow::{Result, anyhow, bail};
use diskdiff_engine::{Progress, ScanOutcome, Store, scan};
use diskdiff_ipc::{self as ipc, FinishedJob, Request, Response, RunningJob};
use serde_json::{Map, Value, json};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Longest `wait_seconds` honoured.
pub const MAX_WAIT: u64 = 600;
/// How long a just-launched app gets to start answering.
const LAUNCH_TIMEOUT: Duration = Duration::from_secs(15);
const POLL: Duration = Duration::from_millis(250);

static NEXT_JOB: AtomicU64 = AtomicU64::new(1);

/// A scan running (or run) inside this server.
pub(crate) struct LocalJob {
    job: u64,
    root: PathBuf,
    temporary: bool,

    progress: Arc<Progress>,
    handle: Option<JoinHandle<Result<ScanOutcome>>>,
    outcome: Option<Result<ScanOutcome, String>>,
}

impl LocalJob {
    /// Collects the thread's result once it has finished.
    fn poll(&mut self) {
        if self.handle.as_ref().is_some_and(|h| h.is_finished()) {
            let h = self.handle.take().expect("checked");
            self.outcome = Some(match h.join() {
                Ok(r) => r.map_err(|e| format!("{e:#}")),
                Err(_) => Err("the scan thread panicked".into()),
            });
        }
    }

    fn running(&self) -> bool {
        self.handle.is_some()
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Runner {
    App,
    Server,
}

impl Runner {
    fn name(self) -> &'static str {
        match self {
            Runner::App => "app",
            Runner::Server => "server",
        }
    }
}

pub fn run(ctx: &Ctx, path: &Path, temporary: bool, wait: u64) -> Result<Value> {
    let root = path.canonicalize().map_err(|e| anyhow!("cannot scan {}: {e}", path.display()))?;
    if !root.is_dir() {
        bail!("{} is not a folder", root.display());
    }
    if let Some(busy) = local_busy(ctx) {
        bail!(busy);
    }
    let (runner, job) = match app_scan(ctx, &root, temporary)? {
        Some(job) => (Runner::App, job),
        // The app may have come up since the first attempt: two writers
        // would block each other on the store.
        None if ctx.config.use_app
            && matches!(ipc::request(&ctx.config.store, &Request::Ping), Ok(Response::Pong { .. })) =>
        {
            let req = Request::Scan { path: root.clone(), temporary };
            (Runner::App, started(ipc::request(&ctx.config.store, &req)?)?)
        }
        None => (Runner::Server, start_local(ctx, &root, temporary)?),
    };
    let deadline = Instant::now() + Duration::from_secs(wait);
    let mut st = job_status(ctx, runner, job)?;
    while wait > 0 && st["state"] == "running" && Instant::now() < deadline {
        std::thread::sleep(POLL.min(deadline.saturating_duration_since(Instant::now())));
        st = job_status(ctx, runner, job)?;
    }
    if st["state"] == "running" {
        st["hint"] = json!(format!("Call scan_status with job {job} to follow it; once finished, call what_changed."));
    }
    Ok(st)
}

pub fn status(ctx: &Ctx, job: Option<u64>) -> Result<Value> {
    // A scan here takes precedence: the app cannot be scanning meanwhile.
    let local = {
        let mut slot = ctx.local.lock().unwrap_or_else(|e| e.into_inner());
        slot.as_mut().map(|j| {
            j.poll();
            (j.job, j.running())
        })
    };
    if let Some((id, running)) = local
        && (running || job == Some(id))
    {
        return job_status(ctx, Runner::Server, id);
    }
    if ctx.config.use_app
        && let Ok(Response::Status(s)) = ipc::request(&ctx.config.store, &Request::Status)
    {
        let id = job.or(s.running.as_ref().map(|r| r.job)).or(s.last.as_ref().map(|l| l.job));
        return match id {
            Some(id) => job_status(ctx, Runner::App, id),
            None => Ok(json!({ "state": "idle", "runner": "app", "message": "The DiskDiff app has not run a scan since it started." })),
        };
    }
    match local {
        Some((id, _)) => job_status(ctx, Runner::Server, id),
        None => Ok(json!({ "state": "idle", "message": "No scan has run since this server started; call list_scans for earlier ones." })),
    }
}

// ---- the app ------------------------------------------------------------

/// Asks the app to scan, starting it if needed. `None`: no app to ask.
fn app_scan(ctx: &Ctx, root: &Path, temporary: bool) -> Result<Option<u64>> {
    if !ctx.config.use_app {
        return Ok(None);
    }
    let store = &ctx.config.store;
    let req = Request::Scan { path: root.to_owned(), temporary };
    match ipc::request(store, &req) {
        Ok(r) => return started(r).map(Some),
        Err(e) if absent(&e) => {}
        Err(e) => bail!("could not reach the DiskDiff app: {e}"),
    }
    if !ctx.config.launch_app || !launch_app() {
        return Ok(None);
    }
    let deadline = Instant::now() + LAUNCH_TIMEOUT;
    while Instant::now() < deadline {
        std::thread::sleep(POLL);
        if let Ok(Response::Pong { .. }) = ipc::request(store, &Request::Ping) {
            return started(ipc::request(store, &req)?).map(Some);
        }
    }
    eprintln!("diskdiff-mcp: started DiskDiff but it did not answer within {LAUNCH_TIMEOUT:?}; scanning here");
    Ok(None)
}

fn absent(e: &std::io::Error) -> bool {
    matches!(e.kind(), std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused)
}

fn started(r: Response) -> Result<u64> {
    match r {
        Response::Started { job } => Ok(job),
        Response::Busy { running } => bail!(busy_message(&running)),
        Response::Error { message } => bail!("DiskDiff did not start the scan: {message}"),
        other => bail!("unexpected answer from DiskDiff: {other:?}"),
    }
}

fn busy_message(r: &RunningJob) -> String {
    let saving = if r.saving { ", now saving" } else { "" };
    format!(
        "A scan of {} is already running ({} files, {} so far{saving}). Wait for it with scan_status (job {}), then try again.",
        r.root.display(),
        r.files,
        bytes(r.bytes),
        r.job
    )
}

/// Starts the DiskDiff app in the background. Its output goes nowhere: on
/// stdout it would corrupt the protocol.
fn launch_app() -> bool {
    use std::process::{Command, Stdio};
    let quiet = |c: &mut Command| {
        c.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    };
    let exe = std::env::current_exe().ok();
    #[cfg(target_os = "macos")]
    {
        // Inside DiskDiff.app/Contents/MacOS: open that copy, else ask
        // Launch Services for the app by bundle id.
        let bundle = exe.as_deref().and_then(Path::parent).filter(|d| d.ends_with("Contents/MacOS")).and_then(|d| {
            let app = d.parent()?.parent()?;
            (app.extension().is_some_and(|e| e == "app")).then(|| app.to_owned())
        });
        let mut c = Command::new("open");
        c.arg("-g");
        match &bundle {
            Some(app) => c.arg("-a").arg(app),
            None => c.args(["-b", "app.diskdiff.native"]),
        };
        quiet(&mut c);
        c.status().is_ok_and(|s| s.success())
    }
    #[cfg(windows)]
    {
        let Some(app) = exe.as_deref().and_then(Path::parent).map(|d| d.join("diskdiff.exe")).filter(|p| p.exists())
        else {
            return false;
        };
        let mut c = Command::new(app);
        quiet(&mut c);
        c.spawn().is_ok()
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        if std::env::var_os("DISPLAY").is_none() && std::env::var_os("WAYLAND_DISPLAY").is_none() {
            return false;
        }
        let sibling = exe.as_deref().and_then(Path::parent).map(|d| d.join("diskdiff")).filter(|p| p.exists());
        let mut c = Command::new(sibling.unwrap_or_else(|| "diskdiff".into()));
        quiet(&mut c);
        c.spawn().is_ok()
    }
}

// ---- in-process ---------------------------------------------------------

fn local_busy(ctx: &Ctx) -> Option<String> {
    let mut slot = ctx.local.lock().unwrap_or_else(|e| e.into_inner());
    let j = slot.as_mut()?;
    j.poll();
    j.running().then(|| {
        busy_message(&RunningJob {
            job: j.job,
            root: j.root.clone(),
            temporary: j.temporary,
            files: j.progress.files.load(Relaxed),
            dirs: j.progress.dirs.load(Relaxed),
            bytes: j.progress.bytes.load(Relaxed),
            expected: j.progress.expected.load(Relaxed),
            saving: j.progress.saving.load(Relaxed),
        })
    })
}

fn start_local(ctx: &Ctx, root: &Path, temporary: bool) -> Result<u64> {
    // A fresh store: its own connection and node cache for the writer.
    let store = Arc::new(Store::open_shared(&ctx.config.store)?);
    let progress = Arc::new(Progress::default());
    let job = NEXT_JOB.fetch_add(1, Relaxed);
    let (r, p) = (root.to_owned(), progress.clone());
    let handle = std::thread::Builder::new().name("scan".into()).spawn(move || {
        let out = scan::scan(&store, &r, temporary, &p)?;
        // As in the app, a temporary scan replaces the previous one.
        if temporary {
            let old: Vec<i64> = store.temporary_ids()?.into_iter().filter(|&id| id != out.scan_id).collect();
            if !old.is_empty() {
                store.delete_scans(&old)?;
            }
        }
        Ok(out)
    })?;
    *ctx.local.lock().unwrap_or_else(|e| e.into_inner()) = Some(LocalJob {
        job,
        root: root.to_owned(),
        temporary,

        progress,
        handle: Some(handle),
        outcome: None,
    });
    Ok(job)
}

// ---- status -------------------------------------------------------------

fn job_status(ctx: &Ctx, runner: Runner, job: u64) -> Result<Value> {
    match runner {
        Runner::Server => local_status(ctx, job),
        Runner::App => app_status(ctx, job),
    }
}

fn local_status(ctx: &Ctx, job: u64) -> Result<Value> {
    let mut slot = ctx.local.lock().unwrap_or_else(|e| e.into_inner());
    let Some(j) = slot.as_mut().filter(|j| j.job == job) else {
        bail!("no scan with job id {job} ran in this server; call list_scans for finished scans");
    };
    j.poll();
    let mut v = if j.running() {
        let p = &j.progress;
        progress_json(&RunningJob {
            job,
            root: j.root.clone(),
            temporary: j.temporary,
            files: p.files.load(Relaxed),
            dirs: p.dirs.load(Relaxed),
            bytes: p.bytes.load(Relaxed),
            expected: p.expected.load(Relaxed),
            saving: p.saving.load(Relaxed),
        })
    } else {
        let fin = FinishedJob {
            job,
            root: j.root.clone(),
            temporary: j.temporary,
            scan_id: j.outcome.as_ref().and_then(|o| o.as_ref().ok()).map(|o| o.scan_id),
            cancelled: false,
            error: j.outcome.as_ref().and_then(|o| o.as_ref().err()).cloned(),
        };
        drop(slot);
        finished_json(ctx, &fin)?
    };
    v["runner"] = json!(Runner::Server.name());
    v["note"] = json!(
        "No DiskDiff app was available, so this scan ran inside the MCP server. On macOS it only had the disk permissions of the app that started the server; folders it could not read are counted in `unreadable` and `full_access` says whether it had Full Disk Access."
    );
    Ok(v)
}

fn app_status(ctx: &Ctx, job: u64) -> Result<Value> {
    let s = match ipc::request(&ctx.config.store, &Request::Status)? {
        Response::Status(s) => s,
        Response::Error { message } => bail!("DiskDiff: {message}"),
        other => bail!("unexpected answer from DiskDiff: {other:?}"),
    };
    let mut v = match (&s.running, &s.last) {
        (Some(r), _) if r.job == job => progress_json(r),
        (_, Some(l)) if l.job == job => finished_json(ctx, l)?,
        _ => json!({
            "state": "unknown",
            "job": job,
            "message": "The DiskDiff app does not know this job (it may have restarted). Call list_scans to find the scan.",
        }),
    };
    v["runner"] = json!(Runner::App.name());
    Ok(v)
}

fn progress_json(r: &RunningJob) -> Value {
    let mut m = Map::new();
    m.insert("state".into(), json!("running"));
    m.insert("job".into(), json!(r.job));
    m.insert("root".into(), json!(r.root.to_string_lossy()));
    m.insert("temporary".into(), json!(r.temporary));
    m.insert("files".into(), json!(r.files));
    m.insert("dirs".into(), json!(r.dirs));
    m.insert("bytes".into(), json!(r.bytes));
    m.insert("bytes_human".into(), json!(bytes(r.bytes)));
    m.insert("expected".into(), json!(r.expected));
    m.insert("expected_human".into(), json!(bytes(r.expected)));
    m.insert("expected_note".into(), json!("Bytes in use on the whole volume: an upper bound when scanning one folder."));
    m.insert("saving".into(), json!(r.saving));
    Value::Object(m)
}

fn finished_json(ctx: &Ctx, f: &FinishedJob) -> Result<Value> {
    let mut m = Map::new();
    m.insert("job".into(), json!(f.job));
    m.insert("root".into(), json!(f.root.to_string_lossy()));
    m.insert("temporary".into(), json!(f.temporary));
    match (f.scan_id, &f.error) {
        (Some(id), _) => {
            m.insert("state".into(), json!("finished"));
            m.insert("scan_id".into(), json!(id));
            if let Some(info) = ctx.store().ok().and_then(|s| s.scans().ok()).and_then(|v| v.into_iter().find(|s| s.id == id)) {
                m.insert("duration_seconds".into(), json!((info.finished_at - info.started_at).max(0)));
                m.insert("finished".into(), json!(iso(info.finished_at)));
                m.insert("scan".into(), scan_full(&info));
            }
            let keep = if f.temporary {
                " This scan is temporary: it is discarded when DiskDiff quits; scan with temporary=false to keep it in the history."
            } else {
                ""
            };
            m.insert(
                "next".into(),
                json!(format!("Call what_changed with to={id} to compare it with the previous saved scan of this folder.{keep}")),
            );
        }
        (None, Some(e)) => {
            m.insert("state".into(), json!("failed"));
            m.insert("error".into(), json!(e));
        }
        (None, None) => {
            m.insert("state".into(), json!(if f.cancelled { "cancelled" } else { "failed" }));
        }
    }
    Ok(Value::Object(m))
}
