//! Lets an AI agent's MCP server (`diskdiff-mcp`) ask the running app to scan.
//!
//! The scan then runs here, with the app's Full Disk Access, its live
//! progress in the window, the one-temporary-scan rule and the sidebar,
//! rather than the server writing to the store behind the app's back.
//!
//! `diskdiff_ipc` answers each request on a background thread, so the handler
//! never touches GPUI entities: status comes from [`Jobs`], which `AppState`
//! keeps up to date, and scan requests travel to the main thread over a
//! channel, like the Finder service in `services`.

use crate::state::AppState;
use diskdiff_engine::Progress;
use diskdiff_ipc::{FinishedJob, JobStatus, Request, Response, RunningJob, Server};
use gpui_kit::*;
use parking_lot::Mutex;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering::Relaxed;
use std::time::Duration;

/// How long a scan request waits for the main thread before giving up; well
/// under the client's read timeout.
const REPLY_TIMEOUT: Duration = Duration::from_secs(5);

/// The listening server; dropped on quit so the endpoint file goes away.
static SERVER: Mutex<Option<Server>> = Mutex::new(None);

/// The running scan and the last finished one, shared with the server thread.
#[derive(Clone, Default)]
pub struct Jobs(Arc<Mutex<JobsInner>>);

#[derive(Default)]
struct JobsInner {
    running: Option<Running>,
    last: Option<FinishedJob>,
}

struct Running {
    job: u64,
    root: PathBuf,
    temporary: bool,
    progress: Arc<Progress>,
}

impl Jobs {
    pub fn started(&self, job: u64, root: PathBuf, temporary: bool, progress: Arc<Progress>) {
        self.0.lock().running = Some(Running { job, root, temporary, progress });
    }

    pub fn finished(&self, done: FinishedJob) {
        let mut inner = self.0.lock();
        inner.running = None;
        inner.last = Some(done);
    }

    pub fn status(&self) -> JobStatus {
        let inner = self.0.lock();
        JobStatus { running: inner.running.as_ref().map(running_job), last: inner.last.clone() }
    }
}

fn running_job(r: &Running) -> RunningJob {
    let p = &r.progress;
    RunningJob {
        job: r.job,
        root: r.root.clone(),
        temporary: r.temporary,
        files: p.files.load(Relaxed),
        dirs: p.dirs.load(Relaxed),
        bytes: p.bytes.load(Relaxed),
        expected: p.expected.load(Relaxed),
        saving: p.saving.load(Relaxed),
    }
}

/// Why a path can't be scanned, as told to the agent.
fn check_path(path: &Path) -> Result<(), String> {
    if !path.is_absolute() {
        return Err(format!("{} is not an absolute path", path.display()));
    }
    match std::fs::metadata(path) {
        Ok(m) if m.is_dir() => Ok(()),
        Ok(_) => Err(format!("{} is not a folder", path.display())),
        Err(e) => Err(format!("cannot open {}: {e}", path.display())),
    }
}

struct ScanRequest {
    path: PathBuf,
    temporary: bool,
    reply: std::sync::mpsc::SyncSender<Response>,
}

/// Starts answering agents. Failing to listen only costs the agent feature,
/// so it is logged rather than fatal.
pub fn init(state: Entity<AppState>, cx: &mut App) {
    let jobs = state.read(cx).jobs.clone();
    let store = state.read(cx).store.path().to_path_buf();
    let (tx, rx) = futures::channel::mpsc::unbounded::<ScanRequest>();

    let server = diskdiff_ipc::serve(&store, move |req| match req {
        Request::Ping => {
            Response::Pong { version: env!("CARGO_PKG_VERSION").into(), protocol: diskdiff_ipc::PROTOCOL }
        }
        Request::Status => Response::Status(jobs.status()),
        Request::Scan { path, temporary } => {
            if let Err(message) = check_path(&path) {
                return Response::Error { message };
            }
            let (reply, answer) = std::sync::mpsc::sync_channel(1);
            if tx.unbounded_send(ScanRequest { path, temporary, reply }).is_err() {
                return Response::Error { message: "DiskDiff is quitting".into() };
            }
            answer.recv_timeout(REPLY_TIMEOUT).unwrap_or_else(|_| Response::Error {
                message: "DiskDiff did not respond in time; try again".into(),
            })
        }
    });
    match server {
        Ok(server) => *SERVER.lock() = Some(server),
        Err(e) => {
            eprintln!("could not start the agent endpoint: {e}");
            return;
        }
    }

    cx.spawn(async move |cx| {
        use futures::StreamExt;
        let mut rx = rx;
        while let Some(req) = rx.next().await {
            let response = cx.update(|cx| start(&state, req.path, req.temporary, cx));
            let _ = req.reply.send(response);
        }
    })
    .detach();
}

/// Stops answering and removes the endpoint file.
pub fn shutdown() {
    SERVER.lock().take();
}

fn start(state: &Entity<AppState>, path: PathBuf, temporary: bool, cx: &mut App) -> Response {
    let started = state.update(cx, |s, cx| {
        let job = s.try_start_scan(path.clone(), temporary, cx)?;
        // A saved scan adds its folder to the sidebar and shows it, as adding
        // a location and scanning it would; a temporary one is selected when
        // it finishes.
        if !temporary {
            s.set_location(path.clone(), cx);
        }
        s.notify_toast("Scan requested by an AI agent", Some(path.display().to_string().into()), cx);
        Some(job)
    });
    let Some(job) = started else {
        let status = state.read(cx).jobs.status();
        return match status.running {
            Some(running) => Response::Busy { running },
            None => Response::Error { message: "another scan is running".into() },
        };
    };
    // Don't steal focus from what the user is doing; only surface the app
    // when its window can't be seen at all (hidden, minimized, covered).
    // Without ignoring other apps, macOS activates it only if no other app
    // is active.
    let visible = cx
        .windows()
        .iter()
        .any(|w| w.update(cx, |_, window, _| window.is_visible()).unwrap_or(false));
    if !visible {
        cx.activate(false);
    }
    Response::Started { job }
}

#[cfg(test)]
mod tests {
    use super::*;
    // `gpui_kit::*` brings its own `test` macro; use the standard one.
    use core::prelude::v1::test;

    #[test]
    fn status_follows_jobs() {
        let jobs = Jobs::default();
        assert_eq!(jobs.status(), JobStatus::default());

        let progress = Arc::new(Progress::default());
        progress.files.store(3, Relaxed);
        progress.bytes.store(4096, Relaxed);
        jobs.started(1, "/Project".into(), true, progress.clone());
        let running = jobs.status().running.unwrap();
        assert_eq!((running.job, running.files, running.bytes, running.saving), (1, 3, 4096, false));
        assert_eq!(running.root, Path::new("/Project"));

        // Live counts come from the progress atomics, not a copy.
        progress.files.store(5, Relaxed);
        assert_eq!(jobs.status().running.unwrap().files, 5);

        let done = FinishedJob {
            job: 1,
            root: "/Project".into(),
            temporary: true,
            scan_id: Some(9),
            cancelled: false,
            error: None,
        };
        jobs.finished(done.clone());
        assert_eq!(jobs.status(), JobStatus { running: None, last: Some(done) });
    }

    #[test]
    fn checks_paths() {
        let dir = std::env::temp_dir().join(format!("diskdiff-control-test-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("Photos")).unwrap();
        std::fs::write(dir.join("film.mp4"), b"x").unwrap();
        assert!(check_path(&dir.join("Photos")).is_ok());
        assert!(check_path(&dir.join("film.mp4")).unwrap_err().contains("not a folder"));
        assert!(check_path(&dir.join("Missing")).unwrap_err().contains("cannot open"));
        assert!(check_path(Path::new("Photos")).unwrap_err().contains("not an absolute path"));
        let _ = std::fs::remove_dir_all(dir);
    }
}
