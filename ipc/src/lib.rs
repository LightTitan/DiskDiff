//! How other processes ask the running DiskDiff app to do something.
//!
//! The app listens on a loopback TCP port and writes the port and a random
//! token to an endpoint file next to the store (see [`endpoint_path`]). A
//! client reads that file, connects, and sends one request per connection as a
//! line of JSON; the app answers with one line of JSON and closes.
//!
//! Loopback TCP rather than a Unix socket or named pipe keeps this identical
//! on macOS, Windows and Linux with only the standard library. The token stops
//! other users' processes and web pages from driving the app: only someone who
//! can read the endpoint file (the same user) can send requests.

use serde::{Deserialize, Serialize};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// Bumped when a request or response changes incompatibly.
pub const PROTOCOL: u32 = 1;

/// Longest request line the server reads; anything larger is rejected.
const MAX_LINE: u64 = 64 * 1024;

/// The endpoint file for a store: `snapshots.db` → `snapshots.control.json`.
pub fn endpoint_path(store: &Path) -> PathBuf {
    store.with_extension("control.json")
}

/// What the endpoint file holds.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Endpoint {
    pub port: u16,
    pub token: String,
    pub pid: u32,
    pub protocol: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Request {
    /// Is the app there? Answered with [`Response::Pong`].
    Ping,
    /// Start scanning `path`. A temporary scan replaces the previous temporary
    /// one and is discarded when the app quits; a saved one joins that
    /// location's history. Answered with `Started`, `Busy` or `Error`.
    Scan { path: PathBuf, temporary: bool },
    /// The running scan, if any, and the last finished one.
    Status,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Response {
    Pong { version: String, protocol: u32 },
    /// The scan started; `job` identifies it in later [`JobStatus`] answers.
    Started { job: u64 },
    /// Another scan is running; try again once it has finished.
    Busy { running: RunningJob },
    Status(JobStatus),
    Error { message: String },
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct JobStatus {
    pub running: Option<RunningJob>,
    /// The most recent scan that finished, failed or was cancelled since the
    /// app started.
    pub last: Option<FinishedJob>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RunningJob {
    pub job: u64,
    pub root: PathBuf,
    pub temporary: bool,
    pub files: u64,
    pub dirs: u64,
    pub bytes: u64,
    /// Bytes in use on the volume: the denominator for a progress estimate.
    pub expected: u64,
    /// The walk is over and the scan is being saved.
    pub saving: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FinishedJob {
    pub job: u64,
    pub root: PathBuf,
    pub temporary: bool,
    /// The saved scan's id in the store; `None` if it failed or was cancelled.
    pub scan_id: Option<i64>,
    pub cancelled: bool,
    pub error: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct Envelope<T> {
    token: String,
    #[serde(flatten)]
    body: T,
}

// ---- server -------------------------------------------------------------

/// A listening endpoint. Dropping it stops answering and removes the
/// endpoint file (if it still names this process).
pub struct Server {
    file: PathBuf,
    stop: Arc<AtomicBool>,
    addr: SocketAddr,
}

impl Server {
    pub fn port(&self) -> u16 {
        self.addr.port()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // Wake the accept loop so its thread ends.
        let _ = TcpStream::connect_timeout(&self.addr, Duration::from_millis(200));
        if let Ok(ep) = read_endpoint(&self.file)
            && ep.pid == std::process::id()
        {
            let _ = std::fs::remove_file(&self.file);
        }
    }
}

/// Starts answering requests for the store at `store`. `handler` runs on a
/// background thread, once per request.
pub fn serve(store: &Path, handler: impl Fn(Request) -> Response + Send + Sync + 'static) -> io::Result<Server> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
    let addr = listener.local_addr()?;
    let token = random_token();
    let file = endpoint_path(store);
    let ep = Endpoint { port: addr.port(), token: token.clone(), pid: std::process::id(), protocol: PROTOCOL };
    write_private(&file, &serde_json::to_vec_pretty(&ep).map_err(io::Error::other)?)?;

    let stop = Arc::new(AtomicBool::new(false));
    let stop2 = stop.clone();
    let handler = Arc::new(handler);
    std::thread::Builder::new().name("diskdiff-ipc".into()).spawn(move || {
        for conn in listener.incoming() {
            if stop2.load(Ordering::SeqCst) {
                break;
            }
            let Ok(conn) = conn else { continue };
            let handler = handler.clone();
            let token = token.clone();
            let _ = std::thread::Builder::new().name("diskdiff-ipc-conn".into()).spawn(move || {
                let _ = answer(conn, &token, &*handler);
            });
        }
    })?;
    Ok(Server { file, stop, addr })
}

fn answer(conn: TcpStream, token: &str, handler: &dyn Fn(Request) -> Response) -> io::Result<()> {
    conn.set_read_timeout(Some(Duration::from_secs(5)))?;
    conn.set_write_timeout(Some(Duration::from_secs(5)))?;
    let mut line = String::new();
    BufReader::new((&conn).take(MAX_LINE)).read_line(&mut line)?;
    let response = match serde_json::from_str::<Envelope<Request>>(&line) {
        Ok(env) if constant_time_eq(env.token.as_bytes(), token.as_bytes()) => handler(env.body),
        Ok(_) => Response::Error { message: "wrong token".into() },
        Err(e) => Response::Error { message: format!("bad request: {e}") },
    };
    let mut out = serde_json::to_vec(&response).map_err(io::Error::other)?;
    out.push(b'\n');
    (&conn).write_all(&out)
}

// ---- client -------------------------------------------------------------

pub fn read_endpoint(file: &Path) -> io::Result<Endpoint> {
    let bytes = std::fs::read(file)?;
    serde_json::from_slice(&bytes).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

/// Sends one request to the app serving the store at `store`. Fails with
/// `NotFound` when no app has written an endpoint, and `ConnectionRefused`
/// when the file is stale (the app quit without removing it).
pub fn request(store: &Path, req: &Request) -> io::Result<Response> {
    let ep = read_endpoint(&endpoint_path(store))?;
    if ep.protocol != PROTOCOL {
        return Err(io::Error::other(format!(
            "the running DiskDiff speaks protocol {}, this client {}; update both to the same version",
            ep.protocol, PROTOCOL
        )));
    }
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, ep.port));
    let conn = TcpStream::connect_timeout(&addr, Duration::from_secs(2))?;
    conn.set_read_timeout(Some(Duration::from_secs(10)))?;
    conn.set_write_timeout(Some(Duration::from_secs(5)))?;
    let mut line = serde_json::to_vec(&Envelope { token: ep.token, body: req }).map_err(io::Error::other)?;
    line.push(b'\n');
    (&conn).write_all(&line)?;
    let mut answer = String::new();
    BufReader::new((&conn).take(16 * 1024 * 1024)).read_line(&mut answer)?;
    serde_json::from_str(&answer).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

// ---- helpers ------------------------------------------------------------

/// 128 random bits as hex. `RandomState` is seeded from the OS's random
/// source, which is enough for a token only readable by the same user.
fn random_token() -> String {
    use std::hash::{BuildHasher, Hasher};
    let mut out = String::new();
    for i in 0..2u64 {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u64(i);
        h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos()));
        out.push_str(&format!("{:016x}", h.finish()));
    }
    out
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Writes `bytes` to `file` atomically, readable only by the owner on Unix.
fn write_private(file: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = file.with_extension("json.tmp");
    let _ = std::fs::remove_file(&tmp);
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(&tmp)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    drop(f);
    std::fs::rename(&tmp, file)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let dir = std::env::temp_dir().join(format!("diskdiff-ipc-test-{}", std::process::id()));
        let store = dir.join("snapshots.db");
        let server = serve(&store, |req| match req {
            Request::Ping => Response::Pong { version: "test".into(), protocol: PROTOCOL },
            Request::Scan { path, .. } if path == Path::new("/nope") => Response::Error { message: "no".into() },
            Request::Scan { .. } => Response::Started { job: 7 },
            Request::Status => Response::Status(JobStatus::default()),
        })
        .unwrap();
        assert!(endpoint_path(&store).exists());
        assert_eq!(
            request(&store, &Request::Ping).unwrap(),
            Response::Pong { version: "test".into(), protocol: PROTOCOL }
        );
        assert_eq!(
            request(&store, &Request::Scan { path: "/Project".into(), temporary: true }).unwrap(),
            Response::Started { job: 7 }
        );
        assert_eq!(request(&store, &Request::Status).unwrap(), Response::Status(JobStatus::default()));

        // A request with the wrong token is refused.
        let ep = read_endpoint(&endpoint_path(&store)).unwrap();
        let conn = TcpStream::connect(("127.0.0.1", ep.port)).unwrap();
        (&conn).write_all(b"{\"token\":\"bad\",\"cmd\":\"ping\"}\n").unwrap();
        let mut line = String::new();
        BufReader::new(&conn).read_line(&mut line).unwrap();
        assert!(line.contains("wrong token"), "{line}");

        drop(server);
        assert!(!endpoint_path(&store).exists());
        assert_eq!(request(&store, &Request::Ping).unwrap_err().kind(), io::ErrorKind::NotFound);
        let _ = std::fs::remove_dir_all(dir);
    }
}
