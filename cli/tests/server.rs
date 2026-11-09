//! The tools and the JSON-RPC loop against a store built from two scans of a
//! small generic tree.

use diskdiff_engine::{Progress, Store, scan};
use diskdiff_ipc as ipc;
use diskdiff_cli::{CallError, Config, Ctx, call, protocol};
use serde_json::{Value, json};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

/// Sizes are allocated blocks, so the fixture uses block multiples.
const MB: usize = 1 << 20;
const PHOTO: usize = 75 * 4096;

struct Tmp(PathBuf);

impl Tmp {
    fn new(name: &str) -> Tmp {
        static N: AtomicUsize = AtomicUsize::new(0);
        let p = std::env::temp_dir()
            .join(format!("diskdiff-cli-{name}-{}-{}", std::process::id(), N.fetch_add(1, Ordering::SeqCst)));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        Tmp(diskdiff_engine::platform::canonical_root(&p))
    }
}

impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn write(p: &Path, n: usize) {
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    // Varied bytes so no file system compresses them away.
    fs::write(p, (0..n).map(|i| (i * 31 % 251) as u8).collect::<Vec<_>>()).unwrap();
}

/// A tree scanned twice, saved: between the scans film.mp4 moved from
/// Downloads to Movies, a zip arrived, a photo was deleted and
/// node_modules grew.
struct Fixture {
    _tmp: Tmp,
    root: PathBuf,
    store: PathBuf,
}

fn fixture(name: &str) -> Fixture {
    let tmp = Tmp::new(name);
    let root = tmp.0.join("Disk");
    let store = tmp.0.join("store/snapshots.db");
    for i in 0..4 {
        write(&root.join(format!("Project/node_modules/pkg{i}/index.js")), MB / 2);
    }
    write(&root.join("Project/src/main.rs"), 2000);
    for p in ["a", "b", "c", "d", "e"] {
        write(&root.join(format!("Photos/{p}.jpg")), PHOTO);
    }
    write(&root.join("Downloads/film.mp4"), 3 * MB);
    write(&root.join("Downloads/notes.txt"), 1000);
    let s = Store::open_shared(&store).unwrap();
    scan::scan(&s, &root, false, &Progress::default()).unwrap();

    fs::create_dir_all(root.join("Movies")).unwrap();
    fs::rename(root.join("Downloads/film.mp4"), root.join("Movies/film.mp4")).unwrap();
    write(&root.join("Downloads/archive.zip"), 1_536_000);
    fs::remove_file(root.join("Photos/a.jpg")).unwrap();
    write(&root.join("Project/node_modules/pkg9/index.js"), MB);
    // Scans are ordered by start second; keep them apart.
    std::thread::sleep(std::time::Duration::from_millis(1100));
    scan::scan(&s, &root, false, &Progress::default()).unwrap();
    Fixture { _tmp: tmp, root, store }
}

fn ctx(store: &Path) -> Ctx {
    Ctx::new(Config { store: store.to_owned(), use_app: false, launch_app: false })
}

fn ok(ctx: &Ctx, tool: &str, args: Value) -> Value {
    match call(ctx, tool, &args) {
        Ok(v) => v,
        Err(CallError::Failed(m)) => panic!("{tool} failed: {m}"),
        Err(CallError::Unknown(n)) => panic!("unknown tool {n}"),
    }
}

fn err(ctx: &Ctx, tool: &str, args: Value) -> String {
    match call(ctx, tool, &args) {
        Ok(v) => panic!("{tool} should fail, got {v:#}"),
        Err(CallError::Failed(m)) => m,
        Err(CallError::Unknown(n)) => panic!("unknown tool {n}"),
    }
}

fn find<'a>(items: &'a Value, key: &str, suffix: &str) -> &'a Value {
    items
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i[key].as_str().is_some_and(|p| p.ends_with(suffix)))
        .unwrap_or_else(|| panic!("no {suffix} in {items:#}"))
}

fn p(root: &Path, rel: &str) -> String {
    root.join(rel).to_string_lossy().into_owned()
}

#[test]
fn read_tools() {
    let f = fixture("read");
    let c = ctx(&f.store);

    let scans = ok(&c, "list_scans", json!({}));
    assert_eq!(scans["count"], 2);
    let ids: Vec<i64> = scans["scans"].as_array().unwrap().iter().map(|s| s["id"].as_i64().unwrap()).collect();
    assert!(ids[0] > ids[1], "newest first: {ids:?}");
    assert_eq!(scans["locations"][0]["scans"], 2);
    assert!(scans["scans"][0]["finished"].as_str().unwrap().ends_with('Z'));
    assert_eq!(ok(&c, "list_scans", json!({"root": p(&f.root, "Photos")}))["count"], 2);
    assert_eq!(ok(&c, "list_scans", json!({"root": "/elsewhere"}))["count"], 0);

    // The root, compared with the previous scan by default.
    let b = ok(&c, "browse", json!({"path": f.root.to_string_lossy()}));
    assert_eq!(b["scan"]["id"], ids[0]);
    assert_eq!(b["compare_to"]["id"], ids[1]);
    let items = &b["items"];
    assert_eq!(find(items, "name", "Project")["status"], "changed");
    assert_eq!(find(items, "name", "Project")["delta"], MB);
    assert_eq!(find(items, "name", "Movies")["status"], "added");
    assert_eq!(find(items, "name", "Movies")["moved_bytes"], 3 * MB);
    assert_eq!(items[0]["name"], "Project", "biggest first");

    // Inside a folder, the moved file says where it went; relative paths work.
    let d = ok(&c, "browse", json!({"path": "Downloads"}));
    let film = find(&d["items"], "name", "film.mp4");
    assert_eq!(film["status"], "moved");
    assert_eq!(film["moved_to"], p(&f.root, "Movies/film.mp4"));
    assert_eq!(find(&d["items"], "name", "archive.zip")["status"], "added");

    // No comparison, and a cap with a "more" count.
    let ph = ok(&c, "browse", json!({"path": p(&f.root, "Photos"), "compare_to": "none", "limit": 2}));
    assert!(ph["compare_to"].is_null());
    assert_eq!(ph["items"].as_array().unwrap().len(), 2);
    assert!(ph["items"][0].get("status").is_none());
    assert_eq!(ph["more"]["count"], 2);
    // The older scan still has the deleted photo.
    let old = ok(&c, "browse", json!({"path": p(&f.root, "Photos"), "scan": "previous"}));
    assert_eq!(old["items"].as_array().unwrap().len(), 5);
    assert_eq!(old["scan"]["id"], ids[1]);

    let lf = ok(&c, "largest_files", json!({"limit": 3}));
    assert!(lf["files"][0]["path"].as_str().unwrap().ends_with("film.mp4"));
    assert_eq!(lf["files"][0]["size_human"], "3.15 MB");
    assert_eq!(lf["files"].as_array().unwrap().len(), 3);
    let lf = ok(&c, "largest_files", json!({"path": p(&f.root, "Photos")}));
    assert_eq!(lf["files"].as_array().unwrap().len(), 4);

    let fo = ok(&c, "largest_folders", json!({}));
    assert!(fo["folders"].as_array().unwrap().iter().any(|x| x["path"].as_str().unwrap().ends_with("node_modules")));

    // The whole scan: the move is one row, not an add and a delete.
    let w = ok(&c, "what_changed", json!({}));
    assert_eq!(w["from"]["id"], ids[1]);
    assert_eq!(w["moves"]["count"], 1);
    assert_eq!(w["moves"]["largest"][0]["from"], p(&f.root, "Downloads/film.mp4"));
    let mv = find(&w["changes"], "path", "Movies/film.mp4");
    assert_eq!(mv["status"], "moved");
    assert!(w["summary"].as_str().unwrap().contains("moved"));
    assert_eq!(w["moved"], 3 * MB);

    // Under one folder, with every change listed.
    let w = ok(&c, "what_changed", json!({"path": p(&f.root, "Photos"), "min_size": 0}));
    assert_eq!(w["total_delta"], -(PHOTO as i64));
    assert_eq!(find(&w["changes"], "path", "a.jpg")["status"], "removed");
    let w = ok(&c, "what_changed", json!({"path": p(&f.root, "Downloads"), "min_size": 0}));
    assert_eq!(find(&w["changes"], "path", "film.mp4")["status"], "moved");

    let s = ok(&c, "cleanup_suggestions", json!({}));
    let nm = find(&s["suggestions"], "path", "node_modules");
    assert_eq!(nm["kind"], "regenerable");
    assert_eq!(nm["category"], "Package dependencies");
    assert!(nm["hint"].as_str().unwrap().contains("npm"));
    assert!(s["note"].as_str().unwrap().contains("not instructions"));

    let g = ok(&c, "growth", json!({"path": p(&f.root, "Project")}));
    let pts = g["points"].as_array().unwrap();
    assert_eq!(pts.len(), 2);
    assert_eq!(pts[1]["delta"], MB);
    assert_eq!(g["change"], MB);
    assert_eq!(find(&g["churn_since_previous"], "path", "node_modules")["churn"], MB);

    assert_eq!(ok(&c, "scan_status", json!({}))["state"], "idle");
}

#[test]
fn errors_are_helpful() {
    let f = fixture("errors");
    let c = ctx(&f.store);
    assert!(err(&c, "browse", json!({"pth": "/x"})).contains("unknown argument `pth`"));
    assert!(err(&c, "browse", json!({"scan": 999})).contains("no scan with id 999"));
    assert!(err(&c, "browse", json!({"scan": "oldest"})).contains("\"latest\""));
    assert!(err(&c, "browse", json!({"path": "/nowhere/else"})).contains("no scan covers"));
    assert!(err(&c, "browse", json!({"path": p(&f.root, "Movies/film.mp4")})).contains("is a file"));
    assert!(err(&c, "growth", json!({})).contains("missing required argument `path`"));
    assert!(err(&c, "run_scan", json!({"path": p(&f.root, "Downloads/archive.zip")})).contains("not a folder"));
    assert!(err(&c, "run_scan", json!({"path": "relative/dir"})).contains("absolute"));
    // Comparing the first scan: nothing earlier.
    let first = ok(&c, "list_scans", json!({}))["scans"][1]["id"].as_i64().unwrap();
    assert!(err(&c, "what_changed", json!({"to": first})).contains("no earlier saved scan"));
    assert!(matches!(call(&c, "delete_files", &json!({})), Err(CallError::Unknown(_))));

    // No store at all: say so instead of creating one.
    let tmp = Tmp::new("nostore");
    let missing = tmp.0.join("none/snapshots.db");
    assert!(err(&ctx(&missing), "list_scans", json!({})).contains("no scans yet"));
    assert!(!missing.exists());
}

/// Feeds lines through the server loop; returns the replies.
fn rpc(c: &Ctx, lines: &[Value]) -> Vec<Value> {
    let mut input = String::new();
    for l in lines {
        input.push_str(&l.to_string());
        input.push('\n');
    }
    input.push_str("\n{not json\n");
    let mut out = Vec::new();
    protocol::serve(c, input.as_bytes(), &mut out).unwrap();
    String::from_utf8(out).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect()
}

#[test]
fn json_rpc() {
    let f = fixture("rpc");
    let c = ctx(&f.store);
    let replies = rpc(
        &c,
        &[
            json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"test","version":"1"}}}),
            json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
            json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
            json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"list_scans","arguments":{}}}),
            json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"browse","arguments":{"scan":12345}}}),
            json!({"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"nope","arguments":{}}}),
            json!({"jsonrpc":"2.0","id":6,"method":"resources/list"}),
            json!({"jsonrpc":"2.0","id":7,"method":"ping"}),
            json!({"jsonrpc":"2.0","id":8,"method":"prompts/list"}),
            json!({"jsonrpc":"2.0","id":9,"method":"prompts/get","params":{"name":"disk_checkup","arguments":{"path":"~/Projects"}}}),
            json!({"jsonrpc":"2.0","id":10,"method":"prompts/get","params":{"name":"disk_checkup"}}),
            json!({"jsonrpc":"2.0","id":11,"method":"prompts/get","params":{"name":"other"}}),
        ],
    );
    // One reply per request, none for the notification, one for the bad line.
    assert_eq!(replies.len(), 12, "{replies:#?}");
    let init = &replies[0]["result"];
    assert_eq!(init["protocolVersion"], "2025-06-18");
    assert_eq!(init["serverInfo"]["name"], "diskdiff");
    assert!(init["capabilities"]["tools"].is_object());
    assert!(init["capabilities"]["prompts"].is_object());
    let instr = init["instructions"].as_str().unwrap();
    assert!(instr.contains("never deletes") && instr.contains("temporary=true") && instr.contains("10^9"));

    let tools = replies[1]["result"]["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 9);
    for t in tools {
        assert_eq!(t["inputSchema"]["additionalProperties"], false, "{}", t["name"]);
        let ro = t["annotations"]["readOnlyHint"].as_bool().unwrap();
        assert_eq!(ro, t["name"] != "run_scan");
        assert_eq!(t["annotations"]["destructiveHint"], false);
    }

    let res = &replies[2]["result"];
    assert_eq!(res["isError"], false);
    assert_eq!(res["structuredContent"]["count"], 2);
    let text: Value = serde_json::from_str(res["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(text, res["structuredContent"]);

    assert_eq!(replies[3]["result"]["isError"], true);
    assert!(replies[3]["result"]["content"][0]["text"].as_str().unwrap().contains("12345"));
    assert_eq!(replies[4]["error"]["code"], -32602);
    assert_eq!(replies[5]["error"]["code"], -32601);
    assert_eq!(replies[6]["result"], json!({}));
    assert_eq!(replies[6]["id"], 7);

    let prompts = replies[7]["result"]["prompts"].as_array().unwrap();
    assert_eq!(prompts.len(), 1);
    assert_eq!(prompts[0]["name"], "disk_checkup");
    assert_eq!(prompts[0]["arguments"][0]["required"], false);
    let msg = &replies[8]["result"]["messages"][0];
    assert_eq!(msg["role"], "user");
    let text = msg["content"]["text"].as_str().unwrap();
    assert!(text.contains("~/Projects") && text.contains("what_changed") && text.contains("Do not delete"));
    assert!(replies[9]["result"]["messages"][0]["content"]["text"].as_str().unwrap().contains("most recently scanned"));
    assert_eq!(replies[10]["error"]["code"], -32602);
    assert_eq!(replies[11]["error"]["code"], -32700);
    assert!(replies[11]["id"].is_null());
}

#[test]
fn older_clients_get_their_version() {
    let tmp = Tmp::new("versions");
    let c = ctx(&tmp.0.join("snapshots.db"));
    for (asked, got) in [("2025-03-26", "2025-03-26"), ("2024-11-05", "2024-11-05"), ("2099-01-01", "2025-06-18")] {
        let r = rpc(
            &c,
            &[
                json!({"jsonrpc":"2.0","id":"a","method":"initialize","params":{"protocolVersion":asked}}),
                json!({"jsonrpc":"2.0","id":"b","method":"tools/call","params":{"name":"list_scans"}}),
            ],
        );
        assert_eq!(r[0]["id"], "a");
        assert_eq!(r[0]["result"]["protocolVersion"], got);
        // Missing store: a tool error, not a protocol error.
        assert_eq!(r[1]["result"]["isError"], true);
        assert!(r[1]["result"].get("structuredContent").is_none());
    }
}

#[test]
fn run_scan_in_process() {
    let f = fixture("local");
    let c = ctx(&f.store);
    write(&f.root.join("Downloads/second.zip"), 2 * MB);
    let r = ok(&c, "run_scan", json!({"path": p(&f.root, "Downloads"), "wait_seconds": 60}));
    assert_eq!(r["state"], "finished", "{r:#}");
    assert_eq!(r["runner"], "server");
    assert!(r["note"].as_str().unwrap().contains("inside the MCP server"));
    assert_eq!(r["scan"]["temporary"], true);
    assert!(r["scan"]["full_access"].is_boolean());
    let id = r["scan_id"].as_i64().unwrap();
    assert!(r["next"].as_str().unwrap().contains(&format!("to={id}")));
    assert_eq!(ok(&c, "scan_status", json!({}))["scan_id"], id);

    // A temporary scan of Downloads compares with the saved full scan.
    let w = ok(&c, "what_changed", json!({"to": id, "min_size": 0}));
    assert_eq!(w["to"]["id"], id);
    assert!(find(&w["changes"], "path", "second.zip")["status"] == "added");

    // A second temporary scan replaces the first.
    let r2 = ok(&c, "run_scan", json!({"path": p(&f.root, "Photos"), "wait_seconds": 60}));
    assert_eq!(r2["state"], "finished");
    let scans = ok(&c, "list_scans", json!({}));
    assert_eq!(scans["count"], 3);
    assert!(scans["scans"].as_array().unwrap().iter().all(|s| s["id"] != id));

    // Without waiting it returns a job to follow.
    let r3 = ok(&c, "run_scan", json!({"path": p(&f.root, "Project"), "temporary": false}));
    let job = r3["job"].as_u64().unwrap();
    let mut st = ok(&c, "scan_status", json!({"job": job}));
    for _ in 0..200 {
        if st["state"] != "running" {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
        st = ok(&c, "scan_status", json!({"job": job}));
    }
    assert_eq!(st["state"], "finished", "{st:#}");
    assert_eq!(st["scan"]["temporary"], false);
}

#[test]
fn run_scan_in_process_creates_the_store() {
    let tmp = Tmp::new("fresh");
    write(&tmp.0.join("Photos/a.jpg"), 1000);
    let store = tmp.0.join("store/snapshots.db");
    let c = ctx(&store);
    let r = ok(&c, "run_scan", json!({"path": p(&tmp.0, "Photos"), "wait_seconds": 60}));
    assert_eq!(r["state"], "finished", "{r:#}");
    assert!(store.exists());
    assert_eq!(ok(&c, "list_scans", json!({}))["count"], 1);
}

#[test]
fn run_scan_through_the_app() {
    let f = fixture("app");
    let c = Ctx::new(Config { store: f.store.clone(), use_app: true, launch_app: false });
    let scans = ok(&c, "list_scans", json!({}));
    let latest = scans["scans"][0]["id"].as_i64().unwrap();

    // A stand-in for the app: the first status poll says running, then done.
    let asked: Arc<Mutex<Vec<ipc::Request>>> = Arc::default();
    let polls = Arc::new(AtomicU64::new(0));
    let root = f.root.join("Photos");
    let (asked2, polls2, root2) = (asked.clone(), polls.clone(), root.clone());
    let server = ipc::serve(&f.store, move |req| {
        asked2.lock().unwrap().push(req.clone());
        let running = || ipc::RunningJob {
            job: 7,
            root: root2.clone(),
            temporary: true,
            files: 3,
            dirs: 1,
            bytes: 900_000,
            expected: 10 * MB as u64,
            saving: false,
        };
        match req {
            ipc::Request::Ping => ipc::Response::Pong { version: "test".into(), protocol: ipc::PROTOCOL },
            ipc::Request::Scan { path, .. } if path.ends_with("Downloads") => ipc::Response::Busy { running: running() },
            ipc::Request::Scan { .. } => ipc::Response::Started { job: 7 },
            ipc::Request::Status if polls2.fetch_add(1, Ordering::SeqCst) == 0 => {
                ipc::Response::Status(ipc::JobStatus { running: Some(running()), last: None })
            }
            ipc::Request::Status => ipc::Response::Status(ipc::JobStatus {
                running: None,
                last: Some(ipc::FinishedJob {
                    job: 7,
                    root: root2.clone(),
                    temporary: true,
                    scan_id: Some(latest),
                    cancelled: false,
                    error: None,
                }),
            }),
        }
    })
    .unwrap();

    let r = ok(&c, "run_scan", json!({"path": root.to_string_lossy()}));
    assert_eq!(r["runner"], "app");
    assert_eq!(r["job"], 7);
    assert_eq!(r["state"], "running");
    assert_eq!(r["bytes_human"], "900 KB");
    assert!(r["hint"].as_str().unwrap().contains("scan_status"));
    assert_eq!(
        asked.lock().unwrap()[0],
        ipc::Request::Scan { path: diskdiff_engine::platform::canonical_root(&root), temporary: true }
    );

    let st = ok(&c, "scan_status", json!({}));
    assert_eq!(st["state"], "finished", "{st:#}");
    assert_eq!(st["runner"], "app");
    assert_eq!(st["scan_id"], latest);
    assert_eq!(st["scan"]["id"], latest);

    let busy = err(&c, "run_scan", json!({"path": p(&f.root, "Downloads")}));
    assert!(busy.contains("already running") && busy.contains("Photos"), "{busy}");

    // The app gone and its endpoint file left behind: scan here instead.
    let endpoint = ipc::endpoint_path(&f.store);
    let stale = fs::read(&endpoint).unwrap();
    drop(server);
    fs::write(&endpoint, stale).unwrap();
    let r = ok(&c, "run_scan", json!({"path": root.to_string_lossy(), "wait_seconds": 60}));
    assert_eq!(r["runner"], "server", "{r:#}");
    assert_eq!(r["state"], "finished");
}

#[test]
fn report_for_pasting_into_a_chat() {
    let f = fixture("report");
    let s = Store::open_shared(&f.store).unwrap();
    let scans = s.scans().unwrap();
    let (prev, cur) = (&scans[0], &scans[1]);
    let text = diskdiff_cli::report::markdown(&s, &scans, cur, Some(prev), &|_| false).unwrap();
    for heading in ["# Disk report for", "## What changed since", "## Cleanup suggestions", "## Largest folders", "## Largest files"] {
        assert!(text.contains(heading), "{heading} missing:\n{text}");
    }
    // Paths are relative to the scan root; the move is a move, not new space.
    assert!(text.contains("`Downloads/archive.zip` (file, new"), "{text}");
    assert!(text.contains("`Movies/film.mp4` (file, 3.15 MB, moved here from `Downloads/film.mp4`)"), "{text}");
    assert!(text.contains("`Project/node_modules`"), "the cache should be a suggestion:\n{text}");

    // Suggestions dismissed in the app stay out; no comparison drops the changes.
    let text = diskdiff_cli::report::markdown(&s, &scans, cur, None, &|_| true).unwrap();
    assert!(!text.contains("## What changed"), "{text}");
    assert!(text.contains("No large caches"), "{text}");
}

#[test]
fn guide_and_skill_name_the_command() {
    let guide = diskdiff_cli::report::guide("\"/opt/app/diskdiff-cli\"");
    assert!(guide.contains("\"/opt/app/diskdiff-cli\" call what_changed '{\"path\": \"~/Downloads\"}'"), "{guide}");
    assert!(guide.contains("never deletes or moves files"), "{guide}");
    let skill = diskdiff_cli::report::skill("diskdiff-cli");
    assert!(skill.starts_with("---\nname: diskdiff\ndescription: \""), "{skill}");
    assert!(skill.contains("\n---\n\n# DiskDiff"), "{skill}");
}
