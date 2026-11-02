//! DiskDiff's engine: scanning, the deduplicating snapshot store, and diffs.

pub mod insights;
pub mod cloud;
pub mod listing;
pub mod moves;
pub mod node;
pub mod platform;
pub mod query;
pub mod scan;
pub mod store;

pub use query::{Change, Item, Listing, Moved, Status};
pub use scan::{Progress, ScanOutcome};
pub use store::{LargeFile, ScanInfo, Store, StoreStats};
