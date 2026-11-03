//! Application state shared by every view: the store, scan locations, which
//! scans are selected, the running scan, and transient notices.
//!
//! Views hold an `Entity<AppState>`, read it in `render`, and call its methods
//! to change it; every change ends in `cx.notify()` so observers re-render.

use crate::settings::Settings;
use diskdiff_engine::{Progress, ScanInfo, Store, query};
use gpui_kit::*;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SortKey {
    Size,
    /// By |delta|.
    Delta,
    /// Growers first, then shrinkers, each by |delta|.
    DeltaSigned,
    Name,
}

impl SortKey {
    pub const ALL: [SortKey; 4] = [SortKey::Size, SortKey::Delta, SortKey::DeltaSigned, SortKey::Name];
    pub fn label(self) -> &'static str {
        match self {
            SortKey::Size => "Size",
            SortKey::Delta => "Change",
            SortKey::DeltaSigned => "Change +/−",
            SortKey::Name => "Name",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InsightsTab {
    Suggestions,
    Largest,
    Changes,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Screen {
    Files,
    Insights(InsightsTab),
}

pub struct ScanJob {
    pub root: PathBuf,
    pub progress: Arc<Progress>,
}

#[derive(Clone, Debug)]
pub struct Toast {
    pub title: SharedString,
    pub detail: Option<SharedString>,
    pub id: u64,
}

pub struct AppState {
    pub store: Arc<Store>,
    pub settings: Settings,
    /// Every scan in the store, oldest first.
    pub scans: Vec<ScanInfo>,
    /// The location whose history is shown.
    pub location: Option<PathBuf>,
    pub current: Option<i64>,
    pub compare: Option<i64>,
    pub job: Option<ScanJob>,
    pub sort: SortKey,
    pub descending: bool,
    pub screen: Screen,
    pub toast: Option<Toast>,
    /// A request for the browser to reveal a path; the counter makes repeated
    /// requests for the same path distinct.
    pub reveal: Option<(PathBuf, u64)>,
    /// Bumped whenever stored data changes, so views drop cached listings.
    pub data_version: u64,
    pub banner: Option<SharedString>,
    pub show_manage: bool,
    /// Full Disk Access as last checked; `None` until the first check.
    pub full_access: Option<bool>,
    pub access_prompt_dismissed: bool,
    counter: u64,
}

impl AppState {
    pub fn new(store: Arc<Store>, cx: &mut Context<Self>) -> Self {
        let settings = Settings::load();
        let mut s = AppState {
            store,
            location: None,
            settings,
            scans: Vec::new(),
            current: None,
            compare: None,
            job: None,
            sort: SortKey::Size,
            descending: true,
            screen: Screen::Files,
            toast: None,
            reveal: None,
            data_version: 0,
            banner: None,
            show_manage: false,
            full_access: None,
            access_prompt_dismissed: false,
            counter: 0,
        };
        s.reload_scans();
        // First launch with existing scans: start at the newest scan's folder.
        if s.settings.location.is_none() {
            if let Some(newest) = s.scans.iter().rev().find(|x| !x.temporary) {
                let root = PathBuf::from(&newest.root);
                if !s.settings.locations.contains(&root) {
                    s.settings.locations.push(root.clone());
                }
                s.settings.location = Some(root);
            }
        }
        s.location = s
            .settings
            .location
            .clone()
            .filter(|l| s.settings.locations.contains(l))
            .or_else(|| s.settings.locations.first().cloned());
        s.select_newest();
        s.check_access(cx);
        s.seed_favorites();
        s
    }

    /// First run with the sidebar: start from Finder's usual favorites.
    fn seed_favorites(&mut self) {
        if self.settings.favorites.is_some() {
            return;
        }
        let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
        let favorites = vec![home.join("Desktop"), home.join("Documents"), home.join("Downloads"), PathBuf::from("/Applications"), home];
        self.settings.favorites = Some(favorites);
        self.settings.save();
    }

    pub fn favorites(&self) -> &[PathBuf] {
        self.settings.favorites.as_deref().unwrap_or_default()
    }

    pub fn is_favorite(&self, path: &Path) -> bool {
        self.favorites().iter().any(|f| f == path)
    }

    pub fn toggle_favorite(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let list = self.settings.favorites.get_or_insert_with(Vec::new);
        if let Some(ix) = list.iter().position(|f| *f == path) {
            list.remove(ix);
        } else {
            list.push(path);
        }
        self.settings.save();
        cx.notify();
    }

    /// Moves favorite `from` into the place of favorite `to`.
    pub fn move_favorite(&mut self, from: usize, to: usize, cx: &mut Context<Self>) {
        let Some(list) = self.settings.favorites.as_mut() else { return };
        if from >= list.len() || to >= list.len() || from == to {
            return;
        }
        let item = list.remove(from);
        list.insert(to, item);
        self.settings.save();
        cx.notify();
    }

    /// Sets the sidebar width; `save` writes it to disk (at the end of a drag).
    pub fn set_sidebar_width(&mut self, width: f32, save: bool, cx: &mut Context<Self>) {
        let width = width.clamp(crate::sidebar::MIN_WIDTH, crate::sidebar::MAX_WIDTH);
        if (self.settings.sidebar_width - width).abs() > 0.5 {
            self.settings.sidebar_width = width;
            cx.notify();
        }
        if save {
            self.settings.save();
        }
    }

    pub fn set_sidebar(&mut self, show: bool, cx: &mut Context<Self>) {
        self.settings.show_sidebar = show;
        self.settings.save();
        cx.notify();
    }

    fn reload_scans(&mut self) {
        match self.store.scans() {
            Ok(v) => self.scans = v,
            Err(e) => self.banner = Some(format!("Could not read the scan history: {e}").into()),
        }
    }

    pub fn scan(&self, id: i64) -> Option<&ScanInfo> {
        self.scans.iter().find(|s| s.id == id)
    }

    pub fn current_scan(&self) -> Option<&ScanInfo> {
        self.current.and_then(|id| self.scan(id))
    }

    pub fn compare_scan(&self) -> Option<&ScanInfo> {
        self.compare.and_then(|id| self.scan(id))
    }

    /// Scans of the selected location, oldest first, including a temporary
    /// scan of exactly that folder.
    pub fn history(&self) -> Vec<&ScanInfo> {
        let Some(loc) = &self.location else { return Vec::new() };
        self.scans.iter().filter(|s| Path::new(&s.root) == loc).collect()
    }

    /// The root the browser shows: the current scan's.
    pub fn root(&self) -> Option<PathBuf> {
        self.current_scan().map(|s| PathBuf::from(&s.root))
    }

    /// The running scan, when the browser shows it live: a scan of the shown
    /// location, or any scan while there is nothing else to show.
    pub fn live_job(&self) -> Option<&ScanJob> {
        let job = self.job.as_ref()?;
        let root = self.root();
        (root.is_none() || root.as_ref() == Some(&job.root) || self.location.as_ref() == Some(&job.root)).then_some(job)
    }

    /// Whether the browser shows deltas.
    pub fn comparing(&self) -> bool {
        self.compare_scan().is_some() && self.current_scan().is_some_and(|s| !s.temporary)
    }

    fn select_newest(&mut self) {
        let newest = self.history().last().map(|s| s.id);
        self.current = newest;
        self.compare = self.default_compare();
    }

    /// The saved scan just before the current one, or none.
    fn default_compare(&self) -> Option<i64> {
        let cur = self.current_scan()?;
        if cur.temporary {
            return None;
        }
        let hist = self.history();
        let ix = hist.iter().position(|s| s.id == cur.id)?;
        hist[..ix].iter().rev().find(|s| !s.temporary).map(|s| s.id)
    }

    pub fn set_current(&mut self, id: i64, cx: &mut Context<Self>) {
        if self.current == Some(id) {
            return;
        }
        self.current = Some(id);
        self.compare = self.default_compare();
        cx.notify();
    }

    pub fn set_compare(&mut self, id: Option<i64>, cx: &mut Context<Self>) {
        if self.current_scan().is_some_and(|s| s.temporary) || id == self.current {
            return;
        }
        if self.compare != id {
            self.compare = id;
            cx.notify();
        }
    }

    pub fn set_location(&mut self, loc: PathBuf, cx: &mut Context<Self>) {
        if !self.settings.locations.contains(&loc) {
            self.settings.locations.push(loc.clone());
        }
        self.settings.location = Some(loc.clone());
        self.settings.save();
        self.location = Some(loc);
        self.select_newest();
        cx.notify();
    }

    pub fn forget_location(&mut self, loc: &Path, cx: &mut Context<Self>) {
        self.settings.locations.retain(|l| l != loc);
        if self.location.as_deref() == Some(loc) {
            self.location = self.settings.locations.first().cloned();
            self.settings.location = self.location.clone();
            self.select_newest();
        }
        self.settings.save();
        cx.notify();
    }

    pub fn set_sort(&mut self, sort: SortKey, cx: &mut Context<Self>) {
        self.sort = sort;
        cx.notify();
    }

    pub fn toggle_order(&mut self, cx: &mut Context<Self>) {
        self.descending = !self.descending;
        cx.notify();
    }

    pub fn set_graph_free(&mut self, free: bool, cx: &mut Context<Self>) {
        self.settings.graph_free = free;
        self.settings.save();
        cx.notify();
    }

    pub fn set_deep_activity(&mut self, on: bool, cx: &mut Context<Self>) {
        self.settings.deep_activity = on;
        self.settings.save();
        cx.notify();
    }

    pub fn set_screen(&mut self, screen: Screen, cx: &mut Context<Self>) {
        self.screen = screen;
        cx.notify();
    }

    /// Switches to the file list and asks the browser to reveal `path`.
    pub fn go_to(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.counter += 1;
        self.reveal = Some((path, self.counter));
        self.screen = Screen::Files;
        cx.notify();
    }

    pub fn notify_toast(&mut self, title: impl Into<SharedString>, detail: Option<SharedString>, cx: &mut Context<Self>) {
        self.counter += 1;
        self.toast = Some(Toast { title: title.into(), detail, id: self.counter });
        cx.notify();
    }

    pub fn dismiss_toast(&mut self, cx: &mut Context<Self>) {
        self.toast = None;
        cx.notify();
    }

    // ---- scanning -------------------------------------------------------

    /// Scans `root`. A saved scan joins that location's history; a temporary
    /// one replaces the previous temporary scan and is discarded on quit.
    pub fn start_scan(&mut self, root: PathBuf, temporary: bool, cx: &mut Context<Self>) {
        if self.job.is_some() {
            self.notify_toast("A scan is already running.", None, cx);
            return;
        }
        self.banner = None;
        let progress = Arc::new(Progress::default());
        let handle = diskdiff_engine::scan::spawn_scan(self.store.clone(), root.clone(), temporary, progress.clone());
        self.job = Some(ScanJob { root: root.clone(), progress: progress.clone() });
        cx.notify();
        cx.spawn(async move |this, cx| {
            while !handle.is_finished() {
                cx.background_executor().timer(Duration::from_millis(150)).await;
                if this.update(cx, |_, cx| cx.notify()).is_err() {
                    return;
                }
            }
            let result = handle.join().unwrap_or_else(|_| Err(anyhow::anyhow!("the scanner crashed")));
            let _ = this.update(cx, |this, cx| this.finish_scan(root, temporary, result, cx));
        })
        .detach();
    }

    fn finish_scan(
        &mut self,
        root: PathBuf,
        temporary: bool,
        result: anyhow::Result<diskdiff_engine::ScanOutcome>,
        cx: &mut Context<Self>,
    ) {
        let cancelled = self.job.as_ref().is_some_and(|j| j.progress.cancelled());
        self.job = None;
        match result {
            Ok(out) => {
                if temporary {
                    // Only one temporary scan at a time.
                    let old: Vec<i64> =
                        self.scans.iter().filter(|s| s.temporary && s.id != out.scan_id).map(|s| s.id).collect();
                    if !old.is_empty() {
                        let _ = self.store.delete_scans(&old);
                    }
                } else if !self.settings.locations.contains(&root) {
                    self.settings.locations.push(root.clone());
                }
                self.reload_scans();
                self.data_version += 1;
                self.location = Some(root.clone());
                if !temporary {
                    self.settings.location = Some(root);
                    self.settings.save();
                }
                self.current = Some(out.scan_id);
                self.compare = self.default_compare();
            }
            Err(_) if cancelled => {}
            Err(e) => self.banner = Some(format!("Scan failed: {e}").into()),
        }
        cx.notify();
    }

    pub fn cancel_scan(&mut self, cx: &mut Context<Self>) {
        if let Some(job) = &self.job {
            job.progress.cancel();
            cx.notify();
        }
    }

    // ---- managing scans --------------------------------------------------

    /// Deletes scans in the background; the dedup store only touches the
    /// nodes those scans owned, so this is quick even for big scans.
    pub fn delete_scans(&mut self, ids: Vec<i64>, cx: &mut Context<Self>) -> Task<()> {
        let store = self.store.clone();
        let task = cx.background_executor().spawn(async move { store.delete_scans(&ids) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                if let Err(e) = result {
                    this.notify_toast("Couldn’t delete scans", Some(e.to_string().into()), cx);
                }
                this.after_store_change(cx);
            });
        })
    }

    pub fn after_store_change(&mut self, cx: &mut Context<Self>) {
        self.reload_scans();
        self.data_version += 1;
        if self.current.is_some_and(|id| self.scan(id).is_none()) {
            self.select_newest();
        } else if self.compare.is_some_and(|id| self.scan(id).is_none()) {
            self.compare = self.default_compare();
        }
        cx.notify();
    }

    // ---- reading ---------------------------------------------------------

    /// Lists `abs` comparing with folder `prev` of the compare scan.
    pub fn listing_against(&self, abs: &Path, prev: &Path) -> anyhow::Result<diskdiff_engine::Listing> {
        let cur = self.current_scan().ok_or_else(|| anyhow::anyhow!("no scan selected"))?;
        let cmp = if self.comparing() { self.compare_scan() } else { None };
        query::list_against(&self.store, cur, cmp, abs, prev)
    }

    pub fn listing(&self, abs: &Path) -> anyhow::Result<diskdiff_engine::Listing> {
        let cur = self.current_scan().ok_or_else(|| anyhow::anyhow!("no scan selected"))?;
        let cmp = if self.comparing() { self.compare_scan() } else { None };
        query::list(&self.store, cur, cmp, abs)
    }

    /// Re-checks Full Disk Access (macOS). Cheap; called on launch and focus.
    pub fn check_access(&mut self, cx: &mut Context<Self>) {
        let ok = diskdiff_engine::platform::has_full_access();
        if self.full_access != Some(ok) {
            self.full_access = Some(ok);
            cx.notify();
        }
    }
}

/// Bytes in use on the scanned volume at scan time, as System Settings shows
/// it; scans without volume info fall back to what the scanner counted.
pub fn used_bytes(s: &ScanInfo) -> u64 {
    if s.disk_total > 0 { s.disk_used() } else { s.size }
}

/// "Root" for `/`, else the last path segment.
pub fn location_name(p: &Path) -> String {
    if p.parent().is_none() {
        return if cfg!(windows) { p.display().to_string() } else { "Startup Disk".into() };
    }
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| p.display().to_string())
}

/// Parent directory shown as secondary detail, empty for roots.
pub fn location_detail(p: &Path) -> String {
    match p.parent() {
        Some(parent) => parent.display().to_string(),
        None => String::new(),
    }
}

/// "Finder" or "File Explorer".
pub fn file_manager() -> &'static str {
    if cfg!(windows) { "File Explorer" } else { "Finder" }
}
