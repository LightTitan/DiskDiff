//! `diskdiff-mcp`: the MCP server binary, plus a one-shot CLI for agents that
//! work through a shell.

use diskdiff_mcp::{CallError, Config, Ctx, protocol};
use std::process::ExitCode;

const HELP: &str = "\
diskdiff-mcp: DiskDiff's scan history for AI agents (Model Context Protocol)

Usage:
  diskdiff-mcp [serve]               run the MCP server on stdin/stdout
  diskdiff-mcp call <tool> [<json>]  run one tool and print its JSON result
  diskdiff-mcp tools                 list the tools and their argument schemas
  diskdiff-mcp --help | --version

Examples:
  claude mcp add diskdiff -- /path/to/diskdiff-mcp
  diskdiff-mcp call list_scans
  diskdiff-mcp call what_changed '{\"path\": \"~/Downloads\"}'
  diskdiff-mcp call run_scan '{\"path\": \"~/Projects\", \"wait_seconds\": 60}'

Environment:
  DISKDIFF_DB=<file>      use this store instead of the app's
  DISKDIFF_MCP_NO_APP=1   never ask or start the DiskDiff app; scan in-process

Nothing here deletes or moves files.";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let ctx = || Ctx::new(Config::from_env());
    match args.first().map(String::as_str) {
        None | Some("serve") => {
            let ctx = ctx();
            eprintln!("diskdiff-mcp {}: serving {}", env!("CARGO_PKG_VERSION"), ctx.config.store.display());
            let stdin = std::io::stdin().lock();
            let stdout = std::io::stdout().lock();
            match protocol::serve(&ctx, stdin, stdout) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("diskdiff-mcp: {e}");
                    ExitCode::FAILURE
                }
            }
        }
        Some("tools") => print(&serde_json::Value::Array(diskdiff_mcp::definitions())),
        Some("call") => {
            let Some(tool) = args.get(1) else {
                eprintln!("usage: diskdiff-mcp call <tool> [<json arguments>]");
                return ExitCode::from(2);
            };
            let json = match args.get(2).map(|s| serde_json::from_str(s)).transpose() {
                Ok(v) => v.unwrap_or(serde_json::Value::Null),
                Err(e) => {
                    eprintln!("diskdiff-mcp: the arguments are not valid JSON: {e}");
                    return ExitCode::from(2);
                }
            };
            match diskdiff_mcp::call(&ctx(), tool, &json) {
                Ok(v) => print(&v),
                Err(CallError::Unknown(n)) => {
                    eprintln!("diskdiff-mcp: unknown tool {n}; see `diskdiff-mcp tools`");
                    ExitCode::FAILURE
                }
                Err(CallError::Failed(m)) => {
                    eprintln!("diskdiff-mcp: {m}");
                    ExitCode::FAILURE
                }
            }
        }
        Some("-h" | "--help" | "help") => {
            println!("{HELP}");
            ExitCode::SUCCESS
        }
        Some("-V" | "--version") => {
            println!("diskdiff-mcp {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Some(other) => {
            eprintln!("diskdiff-mcp: unknown command {other}\n\n{HELP}");
            ExitCode::from(2)
        }
    }
}

fn print(v: &serde_json::Value) -> ExitCode {
    println!("{}", serde_json::to_string_pretty(v).unwrap_or_default());
    ExitCode::SUCCESS
}
