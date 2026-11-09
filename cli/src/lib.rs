//! DiskDiff's MCP server: lets AI agents read the scan history and start
//! scans, over stdio (`protocol`) or one call at a time from a shell.
//!
//! Nothing here deletes or moves files; the only tool that writes anything
//! is `run_scan`, and it only adds a scan to the store.

pub mod format;
pub mod protocol;
mod scan;
pub mod select;
pub mod tools;

pub use tools::{CallError, Config, Ctx, call, definitions};
