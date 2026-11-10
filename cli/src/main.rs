//! `diskdiff-cli`: a command-line tool for agents that work through a shell,
//! and (`diskdiff-cli mcp`) DiskDiff's MCP server.

use diskdiff_cli::{CallError, Config, Ctx, export, protocol, report, select};
use std::process::ExitCode;

const HELP: &str = "\
diskdiff-cli: DiskDiff's scan history for AI agents

Usage:
  diskdiff-cli call <tool> [<json>]  run one tool and print its JSON result
  diskdiff-cli tools                 list the tools and their argument schemas
  diskdiff-cli report [<path>]       Markdown report to paste into any AI chat
  diskdiff-cli export [<path>]       every folder and file in a scan, tab-separated
      [--scan <id>]                  which scan (default: the latest holding <path>)
      [--from <id>]                  every change since that scan instead
  diskdiff-cli guide                 how an agent with a terminal uses this tool
  diskdiff-cli skill                 the same as an agent skill (SKILL.md)
  diskdiff-cli mcp                   run the MCP server on stdin/stdout
  diskdiff-cli --help | --version

Examples:
  claude mcp add diskdiff -- /path/to/diskdiff-cli mcp
  diskdiff-cli call list_scans
  diskdiff-cli report ~/Downloads
  diskdiff-cli export ~ > scan.tsv
  diskdiff-cli export ~ --from previous | head -50
  diskdiff-cli skill > ~/.claude/skills/diskdiff/SKILL.md
  diskdiff-cli call what_changed '{\"path\": \"~/Downloads\"}'
  diskdiff-cli call run_scan '{\"path\": \"~/Projects\", \"wait_seconds\": 60}'

Environment:
  DISKDIFF_DB=<file>      use this store instead of the app's
  DISKDIFF_MCP_NO_APP=1   never ask or start the DiskDiff app; scan in-process

Nothing here deletes or moves files.";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let ctx = || Ctx::new(Config::from_env());
    match args.first().map(String::as_str) {
        None => {
            println!("{HELP}");
            ExitCode::SUCCESS
        }
        // `serve` was the command before the rename to diskdiff-cli.
        Some("mcp" | "serve") => {
            let ctx = ctx();
            eprintln!("diskdiff-cli {}: serving {}", env!("CARGO_PKG_VERSION"), ctx.config.store.display());
            let stdin = std::io::stdin().lock();
            let stdout = std::io::stdout().lock();
            match protocol::serve(&ctx, stdin, stdout) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("diskdiff-cli: {e}");
                    ExitCode::FAILURE
                }
            }
        }
        Some("tools") => print(&serde_json::Value::Array(diskdiff_cli::definitions())),
        Some("guide") => {
            print!("{}", report::guide(&report::own_command()));
            ExitCode::SUCCESS
        }
        Some("skill") => {
            print!("{}", report::skill(&report::own_command()));
            ExitCode::SUCCESS
        }
        Some("report") => match run_report(&ctx(), args.get(1).map(String::as_str)) {
            Ok(text) => {
                print!("{text}");
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("diskdiff-cli: {e:#}");
                ExitCode::FAILURE
            }
        },
        Some("export") => {
            let run = || -> anyhow::Result<()> {
                let o = export::Options::parse(&args[1..])?;
                export::run(&ctx(), &o, &mut std::io::stdout().lock())
            };
            match run() {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("diskdiff-cli: {e:#}");
                    ExitCode::FAILURE
                }
            }
        }
        Some("call") => {
            let Some(tool) = args.get(1) else {
                eprintln!("usage: diskdiff-cli call <tool> [<json arguments>]");
                return ExitCode::from(2);
            };
            let json = match args.get(2).map(|s| serde_json::from_str(s)).transpose() {
                Ok(v) => v.unwrap_or(serde_json::Value::Null),
                Err(e) => {
                    eprintln!("diskdiff-cli: the arguments are not valid JSON: {e}");
                    return ExitCode::from(2);
                }
            };
            match diskdiff_cli::call(&ctx(), tool, &json) {
                Ok(v) => print(&v),
                Err(CallError::Unknown(n)) => {
                    eprintln!("diskdiff-cli: unknown tool {n}; see `diskdiff-cli tools`");
                    ExitCode::FAILURE
                }
                Err(CallError::Failed(m)) => {
                    eprintln!("diskdiff-cli: {m}");
                    ExitCode::FAILURE
                }
            }
        }
        Some("-h" | "--help" | "help") => {
            println!("{HELP}");
            ExitCode::SUCCESS
        }
        Some("-V" | "--version") => {
            println!("diskdiff-cli {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Some(other) => {
            eprintln!("diskdiff-cli: unknown command {other}\n\n{HELP}");
            ExitCode::from(2)
        }
    }
}

fn print(v: &serde_json::Value) -> ExitCode {
    // Compact: indentation would only cost an agent tokens.
    println!("{}", serde_json::to_string(v).unwrap_or_default());
    ExitCode::SUCCESS
}

/// The report for the latest saved scan holding `path` (or the latest scan),
/// compared with the scan before it.
fn run_report(ctx: &Ctx, path: Option<&str>) -> anyhow::Result<String> {
    let store = ctx.store()?;
    let scans = store.scans()?;
    let (cur, _) = select::locate(&scans, None, path)?;
    let cmp = select::default_compare(&scans, cur);
    report::markdown(&store, &scans, cur, cmp, &|_| false)
}
