//! MCP over stdio: newline-delimited JSON-RPC 2.0, hand-rolled.
//!
//! Requests are answered one at a time in order. Only protocol messages go to
//! the writer; anything for people goes to stderr.

use crate::tools::{self, CallError, Ctx};
use serde_json::{Value, json};
use std::io::{self, BufRead, Write};

pub const LATEST: &str = "2025-06-18";
pub const SUPPORTED: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];

/// A result larger than this is refused rather than sent into a context.
const MAX_RESULT: usize = 512 * 1024;

pub const INSTRUCTIONS: &str = "\
DiskDiff keeps a history of scans: each one records every folder's and file's size at that moment, so two scans show what grew, shrank, appeared, vanished or moved.
Suggested flow: list_scans to see the scanned locations, then what_changed (what grew since the previous scan) and cleanup_suggestions (what could be reclaimed). Verify candidates with browse or growth before recommending anything.
Cleanup suggestions are candidates to check and explain, not instructions. This server never deletes or moves files: tell the person what they could remove, where it is, how big it is and why it is safe or not, and let them do it.
To get fresh numbers call run_scan; keep temporary=true (the default) unless the person wants the scan kept in the folder's history. Poll scan_status for long scans.
Sizes are decimal like Finder (1 GB = 10^9 bytes); every size comes as exact bytes plus a *_human string.";

struct Session {
    version: &'static str,
}

/// Runs the server until `input` ends.
pub fn serve(ctx: &Ctx, mut input: impl BufRead, mut output: impl Write) -> io::Result<()> {
    let mut session = Session { version: LATEST };
    let mut buf = Vec::new();
    loop {
        buf.clear();
        if input.read_until(b'\n', &mut buf)? == 0 {
            return Ok(());
        }
        let line = String::from_utf8_lossy(&buf);
        if line.trim().is_empty() {
            continue;
        }
        if let Some(reply) = handle(ctx, &mut session, &line) {
            let mut out = serde_json::to_vec(&reply).map_err(io::Error::other)?;
            out.push(b'\n');
            output.write_all(&out)?;
            output.flush()?;
        }
    }
}

fn error(id: Value, code: i64, message: impl Into<String>) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message.into() } })
}

/// Answers one message; `None` for notifications and stray responses.
fn handle(ctx: &Ctx, session: &mut Session, line: &str) -> Option<Value> {
    let msg: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(e) => return Some(error(Value::Null, -32700, format!("parse error: {e}"))),
    };
    let Some(obj) = msg.as_object() else {
        return Some(error(Value::Null, -32600, "expected a single JSON-RPC message object"));
    };
    let id = obj.get("id").cloned();
    let Some(method) = obj.get("method").and_then(Value::as_str) else {
        // A response to something we never send, or garbage.
        return id.filter(|_| !obj.contains_key("result") && !obj.contains_key("error")).map(|id| {
            error(id, -32600, "missing method")
        });
    };
    let Some(id) = id else {
        // Notifications (initialized, cancelled…) need no answer.
        return None;
    };
    let params = obj.get("params").cloned().unwrap_or(Value::Null);
    let result = match method {
        "initialize" => {
            let asked = params["protocolVersion"].as_str().unwrap_or(LATEST);
            session.version = SUPPORTED.iter().copied().find(|v| *v == asked).unwrap_or(LATEST);
            json!({
                "protocolVersion": session.version,
                "capabilities": { "tools": { "listChanged": false }, "prompts": { "listChanged": false } },
                "serverInfo": { "name": "diskdiff", "title": "DiskDiff", "version": env!("CARGO_PKG_VERSION") },
                "instructions": INSTRUCTIONS,
            })
        }
        "ping" => json!({}),
        "tools/list" => json!({ "tools": tools::definitions() }),
        "tools/call" => {
            let Some(name) = params["name"].as_str() else {
                return Some(error(id, -32602, "tools/call needs a tool `name`"));
            };
            match tools::call(ctx, name, &params["arguments"]) {
                Ok(v) => tool_result(session, v),
                Err(CallError::Unknown(n)) => return Some(error(id, -32602, format!("unknown tool: {n}"))),
                Err(CallError::Failed(m)) => json!({ "content": [{ "type": "text", "text": m }], "isError": true }),
            }
        }
        "prompts/list" => json!({ "prompts": [prompt_definition()] }),
        "prompts/get" => match params["name"].as_str() {
            Some("disk_checkup") => checkup_prompt(params["arguments"]["path"].as_str()),
            Some(n) => return Some(error(id, -32602, format!("unknown prompt: {n}"))),
            None => return Some(error(id, -32602, "prompts/get needs a prompt `name`")),
        },
        _ => return Some(error(id, -32601, format!("method not found: {method}"))),
    };
    Some(json!({ "jsonrpc": "2.0", "id": id, "result": result }))
}

fn tool_result(session: &Session, v: Value) -> Value {
    let text = serde_json::to_string_pretty(&v).unwrap_or_default();
    if text.len() > MAX_RESULT {
        return json!({
            "content": [{ "type": "text", "text": "The result is too large to return; ask for fewer items with `limit` or a narrower `path`." }],
            "isError": true,
        });
    }
    let mut r = json!({ "content": [{ "type": "text", "text": text }], "isError": false });
    // structuredContent arrived in 2025-06-18; older clients get text only.
    if session.version >= "2025-06-18" {
        r["structuredContent"] = v;
    }
    r
}

fn prompt_definition() -> Value {
    json!({
        "name": "disk_checkup",
        "title": "Disk checkup",
        "description": "Find what grew since the previous scan and what could be reclaimed, as a short prioritized list.",
        "arguments": [{ "name": "path", "description": "Folder to check (defaults to the most recently scanned location).", "required": false }],
    })
}

fn checkup_prompt(path: Option<&str>) -> Value {
    let place = match path.map(str::trim).filter(|p| !p.is_empty()) {
        Some(p) => format!("the folder {p}"),
        None => "the most recently scanned location".into(),
    };
    let text = format!(
        "Give me a checkup of {place} using DiskDiff.\n\
         1. Find what grew since the previous scan (what_changed), telling real growth apart from files that only moved.\n\
         2. Find what could be reclaimed (cleanup_suggestions, largest_folders).\n\
         3. Verify each candidate before recommending it: look inside with browse, and check its history with growth.\n\
         Then give me a short prioritized list: for each item its path, size, what it is, and why it is or is not safe to remove. \
         Do not delete anything; I'll decide what to remove."
    );
    json!({
        "description": format!("Disk checkup of {place}"),
        "messages": [{ "role": "user", "content": { "type": "text", "text": text } }],
    })
}
