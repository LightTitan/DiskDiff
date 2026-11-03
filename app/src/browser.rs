//! The Finder-style column browser and its status bar.
//!
//! How it behaves:
//!
//! * One column per open folder; files are first-class rows. The selected
//!   path (the "chain") survives scan, compare and sort changes, and each
//!   column keeps its own scroll offset across them.
//! * Navigation never scrolls the viewport except to bring the clicked column
//!   (and a newly opened one) fully into view; when columns close, a trailing
//!   spacer keeps the view from jumping left.
//! * Column widths are per column index, default 340, 150–900. The 5 px
//!   resize grip sits just outside the right border so it never covers the
//!   scrollbar; double-click fits the column to its names.
//! * Scrollbar lanes are always reserved (12 px) and the thumb thickens when
//!   the pointer comes within 18 px.
//! * Keyboard: ↑/↓ move, → enters a folder, ← goes back, ⌘↑ parent, typing
//!   selects by name (Finder type-to-select), ⌘R reveals, ⌥⌘C copies the path.
//! * Arrow cursor everywhere; resize grips use the resize cursor.
//! * While a scan of the shown folder runs, the columns show it live: folders
//!   appear as they are listed, sizes count up, rows glide to their new place
//!   as the order changes. Folders deeper than the scan reports live show
//!   their contents once it finishes.

use crate::format;
use crate::state::{AppState, SortKey, file_manager};
use crate::theme::{ActivePalette, Palette};
use crate::widgets::{icon, icon_button, icon_button_disabled, spinner, truncate_start};
use diskdiff_engine::cloud::{self, CloudSize};
use diskdiff_engine::moves::{self, Moves};
use diskdiff_engine::scan::LIVE_DEPTH;
use diskdiff_engine::{Item, Progress, ScanInfo, Status, platform, query};
use gpui_kit::assets::IconName;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::time::{Duration, Instant};

const ROW_H: f32 = 28.;
const DEFAULT_W: f32 = 340.;
const MIN_W: f32 = 150.;
const MAX_W: f32 = 900.;
const LANE: f32 = 12.;
const NEAR: f32 = 18.;
const GUTTER: f32 = 10.;
/// Room a row's name keeps before its extra figures shrink to "…" pills.
const NAME_MIN: f32 = 72.;
/// Width of a figure shrunk to a "…" pill.
const PILL_W: f32 = 20.;
const PREVIEW_W: f32 = 280.;
/// How often a live scan's numbers are read; rows animate between reads.
const LIVE_TICK: Duration = Duration::from_millis(66);
const MB: u64 = 1_000_000;
const GB: u64 = 1_000_000_000;

actions!(
    browser,
    [SelectPrev, SelectNext, OpenSelected, GoBack, GoParent, SelectFirst, SelectLast, Reveal, CopyPath, OpenItem, AddToSidebar]
);

pub fn bind_keys(cx: &mut App) {
    let ctx = Some("Browser");
    cx.bind_keys([
        KeyBinding::new("up", SelectPrev, ctx),
        KeyBinding::new("down", SelectNext, ctx),
        KeyBinding::new("right", OpenSelected, ctx),
        KeyBinding::new("left", GoBack, ctx),
        KeyBinding::new("cmd-up", GoParent, ctx),
        KeyBinding::new("alt-up", SelectFirst, ctx),
        KeyBinding::new("alt-down", SelectLast, ctx),
        KeyBinding::new("home", SelectFirst, ctx),
        KeyBinding::new("end", SelectLast, ctx),
        KeyBinding::new("cmd-r", Reveal, ctx),
        KeyBinding::new("cmd-alt-c", CopyPath, ctx),
        KeyBinding::new("cmd-down", OpenItem, ctx),
        KeyBinding::new("ctrl-cmd-t", AddToSidebar, ctx),
    ]);
}

/// What a column's listing was computed from; a change means reload.
#[derive(Clone, PartialEq)]
struct Key {
    version: u64,
    current: Option<i64>,
    compare: Option<i64>,
    sort: SortKey,
    descending: bool,
}

struct Column {
    path: PathBuf,
    items: Vec<Item>,
    error: Option<SharedString>,
    key: Option<Key>,
    scroll: UniformListScrollHandle,
    activity: HashMap<Arc<str>, u64>,
    activity_key: Option<Key>,
    activity_pending: bool,
    cancel: Arc<AtomicBool>,
    /// Live cloud sizes of the entries (cloud folders only), by name.
    cloud: HashMap<String, CloudSize>,
    cloud_key: Option<Key>,
    cloud_cancel: Arc<AtomicBool>,
    /// Folders a live scan is still walking, by name.
    counting: HashSet<Arc<str>>,
    /// Where each row is drawn while it animates, by name.
    motion: HashMap<Arc<str>, Motion>,
}

impl Column {
    fn new(path: PathBuf) -> Self {
        Column {
            path,
            items: Vec::new(),
            error: None,
            key: None,
            scroll: UniformListScrollHandle::new(),
            activity: HashMap::new(),
            activity_key: None,
            activity_pending: false,
            cancel: Arc::new(AtomicBool::new(false)),
            cloud: HashMap::new(),
            cloud_key: None,
            cloud_cancel: Arc::new(AtomicBool::new(false)),
            counting: HashSet::new(),
            motion: HashMap::new(),
        }
    }

    /// Moves every animating row one frame closer to its place, size and full
    /// opacity. New rows start animating only during a live scan. Returns
    /// whether anything still moves.
    fn step(&mut self, dt: f32, live: bool) -> bool {
        // Exponential easing: the same feel at any frame rate.
        let ease = |tau: f32| 1. - (-dt / tau).exp();
        let (k_y, k_size, k_alpha) = (ease(0.08), ease(0.12) as f64, ease(0.12));
        let mut moving = false;
        for (ix, it) in self.items.iter().enumerate() {
            let y = ix as f32 * ROW_H;
            let size = it.size as f64;
            let Some(m) = self.motion.get_mut(&it.name) else {
                if live {
                    // Appears in place, fades in and counts up from zero.
                    self.motion.insert(it.name.clone(), Motion { y, size: 0., alpha: 0. });
                    moving = true;
                }
                continue;
            };
            m.y += (y - m.y) * k_y;
            m.size += (size - m.size) * k_size;
            m.alpha += (1. - m.alpha) * k_alpha;
            if (y - m.y).abs() < 0.3 {
                m.y = y;
            }
            if (size - m.size).abs() <= (size * 0.0005).max(1.) {
                m.size = size;
            }
            if m.alpha > 0.995 {
                m.alpha = 1.;
            }
            moving |= m.y != y || m.size != size || m.alpha < 1.;
        }
        if !live && !moving {
            self.motion.clear();
        }
        moving
    }
}

/// A row as drawn mid-animation.
#[derive(Clone, Copy)]
struct Motion {
    y: f32,
    size: f64,
    alpha: f32,
}

impl Drop for Column {
    fn drop(&mut self) {
        self.cancel.store(true, Relaxed);
        self.cloud_cancel.store(true, Relaxed);
    }
}

#[derive(Clone)]
struct ChainItem {
    name: Arc<str>,
    is_dir: bool,
}

enum Drag {
    Resize { col: usize, start_x: Pixels, start_w: f32 },
    VThumb { col: usize, grab: Pixels },
    HThumb { grab: Pixels },
}

pub struct Browser {
    state: Entity<AppState>,
    focus: FocusHandle,
    root: Option<PathBuf>,
    chain: Vec<ChainItem>,
    columns: Vec<Column>,
    widths: Vec<f32>,
    hscroll: ScrollHandle,
    spacer: f32,
    drag: Option<Drag>,
    /// Column whose scrollbar the pointer is near.
    near_v: Option<usize>,
    near_h: bool,
    hover_grip: Option<usize>,
    typed: String,
    typed_at: Instant,
    copied_at: Option<Instant>,
    /// Free space on the disk right now, for the probed path.
    free: Option<(PathBuf, u64)>,
    reveal_seen: u64,
    /// Horizontal scroll to apply after the next layout (columns not laid out yet).
    pending_reveal_col: Option<usize>,
    /// Axis the current scroll gesture is locked to (`true` = horizontal).
    wheel_axis: Option<bool>,
    wheel_acc: (f32, f32),
    wheel_at: Instant,
    /// Moves between the two compared scans, keyed by (data version,
    /// current, compare); worked out in the background.
    moves: Option<((u64, i64, i64), Arc<Moves>)>,
    moves_wanted: Option<(u64, i64, i64)>,
    moves_cancel: Arc<AtomicBool>,
    /// The running scan the columns show live.
    live: Option<Arc<Progress>>,
    live_at: Instant,
    frame_at: Instant,
}

impl Browser {
    pub fn new(state: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        bind_keys(cx);
        cx.observe(&state, |this, _, cx| {
            this.sync(cx);
            cx.notify();
        })
        .detach();
        let mut b = Browser {
            state,
            focus: cx.focus_handle(),
            root: None,
            chain: Vec::new(),
            columns: Vec::new(),
            widths: Vec::new(),
            hscroll: ScrollHandle::new(),
            spacer: 0.,
            drag: None,
            near_v: None,
            near_h: false,
            hover_grip: None,
            typed: String::new(),
            typed_at: Instant::now(),
            copied_at: None,
            free: None,
            reveal_seen: 0,
            pending_reveal_col: None,
            wheel_axis: None,
            wheel_acc: (0., 0.),
            wheel_at: Instant::now(),
            moves: None,
            moves_wanted: None,
            moves_cancel: Arc::new(AtomicBool::new(false)),
            live: None,
            live_at: Instant::now(),
            frame_at: Instant::now(),
        };
        b.sync(cx);
        window.focus(&b.focus, cx);
        b
    }

    fn key(&self, cx: &App) -> Key {
        let s = self.state.read(cx);
        Key {
            version: s.data_version,
            current: s.current,
            compare: if s.comparing() { s.compare } else { None },
            sort: s.sort,
            descending: s.descending,
        }
    }

    fn width(&self, ix: usize) -> f32 {
        self.widths.get(ix).copied().unwrap_or(DEFAULT_W)
    }

    /// Absolute path of the folder column `ix` lists.
    fn column_path(&self, ix: usize) -> Option<PathBuf> {
        let mut p = self.root.clone()?;
        for c in &self.chain[..ix] {
            p.push(&*c.name);
        }
        Some(p)
    }

    fn column_count(&self) -> usize {
        match self.chain.last() {
            Some(c) if !c.is_dir => self.chain.len(),
            _ => self.chain.len() + 1,
        }
    }

    pub fn selected_path(&self) -> Option<PathBuf> {
        let mut p = self.root.clone()?;
        for c in &self.chain {
            p.push(&*c.name);
        }
        Some(p)
    }

    /// Brings columns in line with the state: resets on a new root, handles
    /// reveal requests, reloads listings whose inputs changed.
    fn sync(&mut self, cx: &mut Context<Self>) {
        let (root, reveal, live) = {
            let s = self.state.read(cx);
            let job = s.live_job();
            (job.map(|j| j.root.clone()).or_else(|| s.root()), s.reveal.clone(), job.map(|j| j.progress.clone()))
        };
        if live.is_some() != self.live.is_some() {
            // A live scan starts from nothing; when it ends, the rows glide
            // into the saved scan's order.
            for col in &mut self.columns {
                col.key = None;
                if live.is_some() {
                    col.motion.clear();
                }
            }
        }
        self.live = live;
        if root != self.root {
            self.root = root;
            self.chain.clear();
            self.columns.clear();
            self.spacer = 0.;
            self.hscroll.set_offset(point(px(0.), px(0.)));
        }
        if let Some((path, n)) = reveal {
            if n != self.reveal_seen {
                self.reveal_seen = n;
                self.reveal(&path, cx);
            }
        }
        self.load_columns(cx);
    }

    fn load_columns(&mut self, cx: &mut Context<Self>) {
        let key = self.key(cx);
        let n = self.column_count();
        let mut paths = Vec::with_capacity(n);
        for i in 0..n {
            match self.column_path(i) {
                Some(p) => paths.push(p),
                None => break,
            }
        }
        self.columns.truncate(paths.len());
        for (i, p) in paths.into_iter().enumerate() {
            if i < self.columns.len() && self.columns[i].path != p {
                self.columns.truncate(i);
            }
            if i == self.columns.len() {
                self.columns.push(Column::new(p));
            }
            if self.live.is_some() {
                self.reload_live(i, &key, cx);
            } else if self.columns[i].key.as_ref() != Some(&key) {
                self.reload(i, &key, cx);
            }
        }
        if self.live.is_some() {
            return;
        }
        self.start_moves(&key, cx);
        self.start_activity(&key, cx);
        self.start_cloud(&key, cx);
        self.update_free_space();
    }

    /// Fills column `i` from the running scan.
    fn reload_live(&mut self, i: usize, key: &Key, cx: &mut Context<Self>) {
        let Some(progress) = self.live.clone() else { return };
        let Some(rel) = self.root.as_ref().and_then(|r| self.columns[i].path.strip_prefix(r).ok()).map(Path::to_owned) else { return };
        let col = &mut self.columns[i];
        // Reload from the store once the scan is over.
        col.key = None;
        col.items.clear();
        col.counting.clear();
        col.error = None;
        let dir = progress.live.find(&rel);
        let Some(contents) = dir.and_then(|d| d.contents.get()) else {
            col.error = Some(if rel.components().count() >= LIVE_DEPTH {
                "Its contents show when the scan finishes.".into()
            } else if dir.is_some_and(|d| d.unreadable.load(Relaxed)) {
                "This folder couldn’t be read.".into()
            } else {
                "Scanning…".into()
            });
            return;
        };
        for d in &contents.dirs {
            if !d.done.load(Relaxed) {
                col.counting.insert(d.name.clone());
            }
            col.items.push(Item {
                name: d.name.clone(),
                is_dir: true,
                package: d.package,
                unreadable: d.unreadable.load(Relaxed),
                cloud: d.cloud,
                size: d.bytes.load(Relaxed),
                prev: None,
                files: 0,
                dirs: 0,
                status: Status::NoHistory,
                placeholder: false,
                moved_bytes: 0,
                moved: None,
            });
        }
        col.items.extend(contents.files.iter().map(|(name, size)| Item {
            name: name.clone(),
            is_dir: false,
            package: false,
            unreadable: false,
            cloud: false,
            size: *size,
            prev: None,
            files: 1,
            dirs: 0,
            status: Status::NoHistory,
            placeholder: false,
            moved_bytes: 0,
            moved: None,
        }));
        sort_items(&mut col.items, key.sort, key.descending, false);
        cx.notify();
    }

    fn reload(&mut self, i: usize, key: &Key, cx: &mut Context<Self>) {
        let path = self.columns[i].path.clone();
        let moves = self.moves_for(key);
        // Inside a folder that arrived by a move, compare with where it was.
        let before = moves.as_ref().and_then(|m| m.before(&path));
        let (result, comparing) = {
            let s = self.state.read(cx);
            (before.map_or_else(|| s.listing(&path), |b| s.listing_against(&path, &b)), s.comparing())
        };
        let col = &mut self.columns[i];
        match result {
            Ok(listing) => {
                let mut items = listing.items;
                if !comparing {
                    items.retain(|it| it.status != Status::Removed);
                }
                if let Some(m) = moves {
                    query::apply_moves(&mut items, &path, &m);
                }
                sort_items(&mut items, key.sort, key.descending, comparing);
                col.items = items;
                col.error = None;
                if !listing.exists && !listing.existed {
                    col.error = Some("This folder isn’t in the selected scan.".into());
                }
            }
            Err(e) => {
                col.items.clear();
                col.error = Some(e.to_string().into());
            }
        }
        col.key = Some(key.clone());
    }

    fn moves_for(&self, key: &Key) -> Option<Arc<Moves>> {
        let k = (key.version, key.current?, key.compare?);
        self.moves.as_ref().filter(|(mk, _)| *mk == k).map(|(_, m)| m.clone())
    }

    /// Works out the moves between the compared scans once per pair, then
    /// reloads the columns with them.
    fn start_moves(&mut self, key: &Key, cx: &mut Context<Self>) {
        let (Some(cur_id), Some(cmp_id)) = (key.current, key.compare) else { return };
        let k = (key.version, cur_id, cmp_id);
        if self.moves_wanted == Some(k) {
            return;
        }
        self.moves_wanted = Some(k);
        let (cur, cmp, store) = {
            let s = self.state.read(cx);
            (s.current_scan().cloned(), s.compare_scan().cloned(), s.store.clone())
        };
        let (Some(cur), Some(cmp)) = (cur, cmp) else { return };
        self.moves_cancel.store(true, Relaxed);
        self.moves_cancel = Arc::new(AtomicBool::new(false));
        let cancel = self.moves_cancel.clone();
        let task = {
            let cancel = cancel.clone();
            cx.background_executor().spawn(async move { moves::detect(&store, &cur, &cmp, &|| cancel.load(Relaxed)) })
        };
        cx.spawn(async move |this, cx| {
            let result = task.await;
            if cancel.load(Relaxed) {
                return;
            }
            let _ = this.update(cx, |this, cx| {
                this.moves = Some((k, Arc::new(result.unwrap_or_default())));
                for col in &mut this.columns {
                    col.key = None;
                }
                this.load_columns(cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// Exact recursive churn per child folder, computed in the background.
    fn start_activity(&mut self, key: &Key, cx: &mut Context<Self>) {
        let (cur, cmp, deep, store) = {
            let s = self.state.read(cx);
            let cur = s.current_scan().cloned();
            let cmp = if s.comparing() { s.compare_scan().cloned() } else { None };
            (cur, cmp, s.settings.deep_activity, s.store.clone())
        };
        let (Some(cur), Some(cmp)) = (cur, cmp) else {
            for c in &mut self.columns {
                c.activity.clear();
                c.activity_pending = false;
                c.activity_key = None;
            }
            return;
        };
        for i in 0..self.columns.len() {
            let col = &mut self.columns[i];
            if !deep {
                col.activity.clear();
                col.activity_pending = false;
                continue;
            }
            if col.activity_key.as_ref() == Some(key) || !col.items.iter().any(|it| it.is_dir && it.status == Status::Changed) {
                continue;
            }
            col.cancel.store(true, Relaxed);
            col.cancel = Arc::new(AtomicBool::new(false));
            col.activity_key = Some(key.clone());
            col.activity_pending = true;
            let cancel = col.cancel.clone();
            let path = col.path.clone();
            let (store, cur, cmp) = (store.clone(), cur.clone(), cmp.clone());
            let cancel2 = cancel.clone();
            let task = cx.background_executor().spawn(async move {
                query::activity(&store, &cur, &cmp, &path, &|| cancel2.load(Relaxed)).unwrap_or_default()
            });
            let path = self.columns[i].path.clone();
            let key = key.clone();
            cx.spawn(async move |this, cx| {
                let result = task.await;
                if cancel.load(Relaxed) {
                    return;
                }
                let _ = this.update(cx, |this, cx| {
                    if let Some(col) = this.columns.iter_mut().find(|c| c.path == path && c.activity_key.as_ref() == Some(&key)) {
                        col.activity = result;
                        col.activity_pending = false;
                        cx.notify();
                    }
                });
            })
            .detach();
        }
    }

    /// Real file lengths for columns inside cloud drives, read live from
    /// the disk in the background and kept in memory only. Re-read whenever
    /// the column reloads.
    fn start_cloud(&mut self, key: &Key, cx: &mut Context<Self>) {
        for col in &mut self.columns {
            if col.cloud_key.as_ref() == Some(key) || !cloud::is_cloud_path(&col.path) {
                continue;
            }
            col.cloud_cancel.store(true, Relaxed);
            col.cloud_cancel = Arc::new(AtomicBool::new(false));
            col.cloud_key = Some(key.clone());
            let cancel = col.cloud_cancel.clone();
            let path = col.path.clone();
            let task = {
                let (path, cancel) = (path.clone(), cancel.clone());
                cx.background_executor().spawn(async move { cloud::measure_children(&path, &cancel) })
            };
            let key = key.clone();
            cx.spawn(async move |this, cx| {
                let sizes = task.await;
                if cancel.load(Relaxed) {
                    return;
                }
                let _ = this.update(cx, |this, cx| {
                    if let Some(col) = this.columns.iter_mut().find(|c| c.path == path && c.cloud_key.as_ref() == Some(&key)) {
                        col.cloud = sizes;
                        cx.notify();
                    }
                });
            })
            .detach();
        }
    }

    fn update_free_space(&mut self) {
        let Some(p) = self.selected_path() else { return };
        // The nearest existing ancestor: the selection may be gone from disk.
        let mut probe = p.clone();
        while !probe.exists() {
            if !probe.pop() {
                return;
            }
        }
        if self.free.as_ref().is_some_and(|(q, _)| *q == probe) {
            return;
        }
        if let Some(d) = platform::disk_space(&probe) {
            self.free = Some((probe, d.free));
        }
    }

    // ---- selection -----------------------------------------------------------

    fn select(&mut self, col: usize, ix: usize, cx: &mut Context<Self>) {
        let Some(item) = self.columns.get(col).and_then(|c| c.items.get(ix)) else { return };
        let entry = ChainItem { name: item.name.clone(), is_dir: item.is_dir };
        let old_count = self.column_count();
        let old_total = self.total_width();
        self.chain.truncate(col);
        self.chain.push(entry);
        self.load_columns(cx);
        if let Some(c) = self.columns.get(col) {
            c.scroll.scroll_to_item(ix, ScrollStrategy::Nearest);
        }
        self.ensure_visible(col);
        if self.column_count() > col + 1 {
            self.ensure_visible(col + 1);
        } else if self.chain.last().is_some_and(|c| !c.is_dir) {
            self.ensure_preview_visible();
        }
        if self.column_count() != old_count {
            self.keep_spacer(old_total);
        }
        cx.notify();
    }

    fn selected_index(&self, col: usize) -> Option<usize> {
        let name = &self.chain.get(col)?.name;
        self.columns.get(col)?.items.iter().position(|it| &it.name == name)
    }

    /// The column holding the selection (keyboard focus column).
    fn active_column(&self) -> usize {
        self.chain.len().saturating_sub(1)
    }

    fn move_selection(&mut self, delta: isize, cx: &mut Context<Self>) {
        if self.chain.is_empty() {
            if !self.columns.is_empty() && !self.columns[0].items.is_empty() {
                self.select(0, 0, cx);
            }
            return;
        }
        let col = self.active_column();
        let len = self.columns.get(col).map_or(0, |c| c.items.len());
        if len == 0 {
            return;
        }
        let cur = self.selected_index(col).unwrap_or(0) as isize;
        let next = (cur + delta).clamp(0, len as isize - 1) as usize;
        self.select(col, next, cx);
    }

    fn reveal(&mut self, path: &Path, cx: &mut Context<Self>) {
        let Some(root) = self.root.clone() else { return };
        let Ok(rel) = path.strip_prefix(&root) else {
            self.state.update(cx, |s, cx| s.notify_toast("That item is not part of the current scan.", None, cx));
            return;
        };
        self.chain.clear();
        self.columns.clear();
        self.spacer = 0.;
        let key = self.key(cx);
        let parts: Vec<String> = rel.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect();
        for (i, part) in parts.iter().enumerate() {
            if self.columns.len() <= i {
                let p = self.column_path(i).unwrap();
                self.columns.push(Column::new(p));
                if self.live.is_some() {
                    self.reload_live(i, &key, cx);
                } else {
                    self.reload(i, &key, cx);
                }
            }
            let Some(item) = self.columns[i].items.iter().find(|it| &*it.name == part.as_str()) else { break };
            self.chain.push(ChainItem { name: item.name.clone(), is_dir: item.is_dir });
        }
        self.load_columns(cx);
        for (i, _) in self.chain.iter().enumerate() {
            if let (Some(ix), Some(c)) = (self.selected_index(i), self.columns.get(i)) {
                c.scroll.scroll_to_item(ix, ScrollStrategy::Center);
            }
        }
        self.pending_reveal_col = Some(self.column_count().saturating_sub(1));
        cx.notify();
    }

    // ---- horizontal viewport -------------------------------------------------

    fn column_left(&self, ix: usize) -> f32 {
        (0..ix).map(|i| self.width(i)).sum()
    }

    fn total_width(&self) -> f32 {
        let mut w: f32 = (0..self.column_count()).map(|i| self.width(i)).sum();
        if self.chain.last().is_some_and(|c| !c.is_dir) {
            w += PREVIEW_W;
        }
        w
    }

    fn viewport_width(&self) -> f32 {
        f32::from(self.hscroll.bounds().size.width)
    }

    fn scroll_x(&self) -> f32 {
        -f32::from(self.hscroll.offset().x)
    }

    fn set_scroll_x(&self, x: f32) {
        let y = self.hscroll.offset().y;
        self.hscroll.set_offset(point(px(-x.max(0.)), y));
    }

    fn ensure_span_visible(&self, left: f32, right: f32) {
        let vw = self.viewport_width();
        if vw <= 0. {
            return;
        }
        let sx = self.scroll_x();
        if left < sx {
            self.set_scroll_x(left);
        } else if right > sx + vw {
            self.set_scroll_x((right - vw).min(left));
        }
    }

    fn ensure_visible(&self, col: usize) {
        let left = self.column_left(col);
        self.ensure_span_visible(left, left + self.width(col));
    }

    fn ensure_preview_visible(&self) {
        let left = self.column_left(self.column_count());
        self.ensure_span_visible(left, left + PREVIEW_W);
    }

    /// When content shrinks, pad the end so the view does not jump left.
    fn keep_spacer(&mut self, _old_total: f32) {
        let need = self.scroll_x() + self.viewport_width() - self.total_width();
        self.spacer = need.max(0.);
    }

    fn reset_widths(&mut self, cx: &mut Context<Self>) {
        self.widths.clear();
        self.spacer = 0.;
        cx.notify();
    }

    /// Widens a column to `w` (never narrows it), e.g. so a row's "…" pills
    /// show their figures.
    fn widen(&mut self, col: usize, w: f32, cx: &mut Context<Self>) {
        let w = w.clamp(MIN_W, MAX_W).max(self.width(col));
        if self.widths.len() <= col {
            self.widths.resize(col + 1, DEFAULT_W);
        }
        self.widths[col] = w;
        self.ensure_visible(col);
        cx.notify();
    }

    /// Width that fits the longest name in a column.
    fn fit_width(&self, col: usize, cx: &App) -> f32 {
        let Some(c) = self.columns.get(col) else { return DEFAULT_W };
        let longest = c.items.iter().map(|it| it.name.chars().count()).max().unwrap_or(0) as f32;
        let extra = if self.comparing(cx) { 150. } else { 0. };
        (longest * 7.0 + 140. + extra).clamp(MIN_W, MAX_W)
    }

    // ---- actions ---------------------------------------------------------------

    fn reveal_in_finder(&mut self, cx: &mut Context<Self>) {
        let Some(p) = self.selected_path() else { return };
        if p.exists() {
            platform::reveal(&p);
        } else {
            self.state.update(cx, |s, cx| s.notify_toast("That item no longer exists on disk.", None, cx));
        }
    }

    fn copy_path(&mut self, cx: &mut Context<Self>) {
        let Some(p) = self.selected_path() else { return };
        cx.write_to_clipboard(ClipboardItem::new_string(p.display().to_string()));
        self.copied_at = Some(Instant::now());
        cx.notify();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(1500)).await;
            let _ = this.update(cx, |_, cx| cx.notify());
        })
        .detach();
    }

    /// The selected folder, if a folder is selected.
    fn selected_folder(&self) -> Option<PathBuf> {
        self.chain.last().filter(|c| c.is_dir)?;
        self.selected_path()
    }

    fn toggle_favorite(&mut self, cx: &mut Context<Self>) {
        if let Some(p) = self.selected_folder() {
            self.state.update(cx, |s, cx| s.toggle_favorite(p, cx));
        }
    }

    fn open_item(&mut self, cx: &mut Context<Self>) {
        let Some(p) = self.selected_path() else { return };
        if p.exists() {
            platform::open(&p);
        } else {
            self.state.update(cx, |s, cx| s.notify_toast("That item no longer exists on disk.", None, cx));
        }
    }

    /// Finder type-to-select: letters typed within a second of each other
    /// form a prefix; the first name at or after it (in name order) wins.
    fn type_select(&mut self, ch: &str, cx: &mut Context<Self>) {
        if self.typed_at.elapsed() > Duration::from_millis(1000) {
            self.typed.clear();
        }
        self.typed_at = Instant::now();
        self.typed.push_str(&ch.to_lowercase());
        let col = if self.chain.is_empty() { 0 } else { self.active_column() };
        let Some(c) = self.columns.get(col) else { return };
        let typed = self.typed.clone();
        let best = c
            .items
            .iter()
            .enumerate()
            .filter(|(_, it)| it.name.to_lowercase() >= typed)
            .min_by(|a, b| natural_cmp(&a.1.name, &b.1.name))
            .or_else(|| c.items.iter().enumerate().max_by(|a, b| natural_cmp(&a.1.name, &b.1.name)))
            .map(|(i, _)| i);
        if let Some(ix) = best {
            self.select(col, ix, cx);
        }
    }

    // ---- scrolling -------------------------------------------------------------

    /// Scroll gestures over the columns move along one axis only, like
    /// Finder: the axis is picked from the first few pixels of a gesture and
    /// kept until the next gesture (momentum included). Without this a
    /// slightly diagonal swipe scrolls a column and the column strip at once.
    fn on_wheel(&mut self, e: &ScrollWheelEvent, cx: &mut Context<Self>) {
        let d = e.delta.pixel_delta(px(ROW_H));
        let (mut dx, mut dy) = (f32::from(d.x), f32::from(d.y));
        let horizontal = match e.delta {
            // A mouse wheel: every tick stands alone. Shift+wheel scrolls
            // sideways (macOS usually converts it already; do it if not).
            ScrollDelta::Lines(_) => {
                if e.modifiers.shift && dx == 0. {
                    (dx, dy) = (dy, 0.);
                }
                self.wheel_axis = None;
                dx.abs() > dy.abs()
            }
            // A trackpad: lock each gesture (and its momentum) to one axis.
            ScrollDelta::Pixels(_) => {
                let now = Instant::now();
                if e.touch_phase == TouchPhase::Started || now.duration_since(self.wheel_at) > Duration::from_millis(250) {
                    self.wheel_axis = None;
                    self.wheel_acc = (0., 0.);
                }
                self.wheel_at = now;
                match self.wheel_axis {
                    Some(h) => h,
                    None => {
                        self.wheel_acc.0 += dx.abs();
                        self.wheel_acc.1 += dy.abs();
                        let (ax, ay) = self.wheel_acc;
                        if ax + ay < 4. {
                            return; // too little movement to tell yet
                        }
                        let h = ax > ay;
                        self.wheel_axis = Some(h);
                        h
                    }
                }
            }
        };
        if horizontal {
            self.set_scroll_x(self.scroll_x() - dx);
            if self.spacer > 0. && self.scroll_x() + self.viewport_width() <= self.total_width() + 1. {
                self.spacer = 0.;
            }
        } else {
            let b = self.hscroll.bounds();
            let x = f32::from(e.position.x - b.origin.x) + self.scroll_x();
            let mut left = 0.;
            for (i, c) in self.columns.iter().enumerate() {
                let w = self.width(i);
                if x >= left && x < left + w {
                    let h = c.scroll.0.borrow().base_handle.clone();
                    let max = f32::from(h.max_offset().y).max(0.);
                    let y = (f32::from(h.offset().y) + dy).clamp(-max, 0.);
                    h.set_offset(point(px(0.), px(y)));
                    break;
                }
                left += w;
            }
        }
        cx.notify();
    }

    // ---- mouse drags -------------------------------------------------------------

    fn on_drag_move(&mut self, e: &MouseMoveEvent, cx: &mut Context<Self>) {
        let Some(drag) = &self.drag else { return };
        match *drag {
            Drag::Resize { col, start_x, start_w } => {
                let w = (start_w + f32::from(e.position.x - start_x)).clamp(MIN_W, MAX_W);
                if self.widths.len() <= col {
                    self.widths.resize(col + 1, DEFAULT_W);
                }
                self.widths[col] = w;
                let right = self.column_left(col) + w;
                let vw = self.viewport_width();
                if right > self.scroll_x() + vw {
                    self.set_scroll_x(right - vw);
                }
            }
            Drag::VThumb { col, grab } => {
                if let Some(c) = self.columns.get(col) {
                    let h = c.scroll.0.borrow().base_handle.clone();
                    let b = h.bounds();
                    let content = c.items.len() as f32 * ROW_H + 8.;
                    let (thumb, track) = thumb_geom(f32::from(b.size.height), content);
                    let y = f32::from(e.position.y - b.origin.y - grab);
                    let max = (content - f32::from(b.size.height)).max(0.);
                    let t = if track - thumb > 0. { (y / (track - thumb)).clamp(0., 1.) } else { 0. };
                    h.set_offset(point(px(0.), px(-(t * max))));
                }
            }
            Drag::HThumb { grab } => {
                let b = self.hscroll.bounds();
                let vw = f32::from(b.size.width);
                let content = self.total_width() + self.spacer;
                let (thumb, track) = thumb_geom(vw, content);
                let x = f32::from(e.position.x - b.origin.x - grab);
                let t = if track - thumb > 0. { (x / (track - thumb)).clamp(0., 1.) } else { 0. };
                self.set_scroll_x(t * (content - vw).max(0.));
            }
        }
        cx.notify();
    }

    // ---- rendering -----------------------------------------------------------------

    /// Whether rows show changes. A live scan's rows show only their size:
    /// figures left over from the scans it replaces would be misleading.
    fn comparing(&self, cx: &App) -> bool {
        self.live.is_none() && self.state.read(cx).comparing()
    }

    fn render_rows(&self, col: usize, range: Range<usize>, focused: bool, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let p = cx.palette();
        let comparing = self.comparing(cx);
        let live = self.live.is_some();
        let Some(c) = self.columns.get(col) else { return Vec::new() };
        let selected_ix = self.selected_index(col);
        let is_last = col + 1 == self.chain.len();
        let (width, this) = (self.width(col), cx.entity());
        range
            .filter_map(|ix| {
                let it = c.items.get(ix)?;
                let selected = selected_ix == Some(ix) && is_last;
                let open = selected_ix == Some(ix) && !is_last;
                let activity = c.activity.get(&it.name).copied().filter(|_| !live);
                let pending = !live && c.activity_pending && it.is_dir && it.status == Status::Changed && activity.is_none();
                let cloud = c.cloud.get(&*it.name).copied().filter(|_| !live);
                let motion = c.motion.get(&it.name).map(|m| (m.y - ix as f32 * ROW_H, m.alpha, m.size as u64));
                let counting = live.then(|| c.counting.contains(&it.name));
                Some(
                    row(it, ix, col, width, selected, open, focused, comparing, activity, pending, cloud, motion, counting, &this, &self.state, &p)
                        .on_click(cx.listener(move |this, e: &ClickEvent, window, cx| {
                            window.focus(&this.focus, cx);
                            this.select(col, ix, cx);
                            if e.click_count() == 2 && this.chain.last().is_some_and(|c| !c.is_dir) {
                                this.open_item(cx);
                            }
                        }))
                        .into_any_element(),
                )
            })
            .collect()
    }

    fn render_column(&self, col: usize, focused: bool, p: &Palette, cx: &mut Context<Self>) -> impl IntoElement {
        let c = &self.columns[col];
        let w = self.width(col);
        let count = c.items.len();
        let empty = count == 0 && c.error.is_none();
        let error = c.error.clone();
        let near = self.near_v == Some(col) || matches!(self.drag, Some(Drag::VThumb { col: dc, .. }) if dc == col);
        let grip_on = self.hover_grip == Some(col) || matches!(self.drag, Some(Drag::Resize { col: dc, .. }) if dc == col);
        let scroll = c.scroll.clone();
        let list = uniform_list(
            SharedString::from(format!("col-{col}-{}", c.path.display())),
            count,
            cx.processor(move |this, range: Range<usize>, _, cx| this.render_rows(col, range, focused, cx)),
        )
        .track_scroll(&scroll)
        .size_full()
        .pt(px(4.))
        .pb(px(4.));

        div()
            .id(("column", col))
            .relative()
            .flex_shrink_0()
            .w(px(w))
            .h_full()
            .bg(p.column)
            .border_r_1()
            .border_color(p.border)
            .on_mouse_move(cx.listener(move |this, e: &MouseMoveEvent, _, cx| {
                if this.drag.is_some() {
                    return;
                }
                let right = this.column_left(col) + this.width(col) - this.scroll_x();
                let b = this.hscroll.bounds();
                let x = f32::from(e.position.x - b.origin.x);
                // Move events only reach the hovered column, and the near zone
                // lies inside it, so this column alone decides near_v.
                let new = (right - x <= NEAR).then_some(col);
                if new != this.near_v {
                    this.near_v = new;
                    cx.notify();
                }
            }))
            .child(
                div()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .left(px(GUTTER))
                    .right(px(LANE))
                    .child(list),
            )
            .when(empty, |el| {
                el.child(div().absolute().top(px(10.)).left(px(GUTTER + 4.)).text_size(px(12.)).text_color(p.dim).child("Empty folder"))
            })
            .when_some(error, |el, err| {
                el.child(
                    div()
                        .absolute()
                        .top(px(10.))
                        .left(px(GUTTER + 4.))
                        .right(px(LANE + 4.))
                        .text_size(px(12.))
                        .text_color(p.dim)
                        .child(err),
                )
            })
            .child(self.vscrollbar(col, near, p, cx))
            // Resize grip: just outside the right border, over the next
            // column's empty gutter, never over this column's scrollbar.
            .child(
                div()
                    .id(("grip", col))
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .right(px(-5.))
                    .w(px(5.))
                    .cursor(CursorStyle::ResizeLeftRight)
                    .when(grip_on, |el| el.child(div().absolute().top_0().bottom_0().left_0().w(px(2.)).bg(p.accent)))
                    .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                        let new = if *hovered { Some(col) } else if this.hover_grip == Some(col) { None } else { this.hover_grip };
                        if new != this.hover_grip {
                            this.hover_grip = new;
                            cx.notify();
                        }
                    }))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, e: &MouseDownEvent, _, cx| {
                            cx.stop_propagation();
                            if e.click_count == 2 {
                                let w = this.fit_width(col, cx);
                                if this.widths.len() <= col {
                                    this.widths.resize(col + 1, DEFAULT_W);
                                }
                                this.widths[col] = w;
                                this.ensure_visible(col);
                                this.drag = None;
                            } else {
                                this.drag = Some(Drag::Resize { col, start_x: e.position.x, start_w: this.width(col) });
                            }
                            cx.notify();
                        }),
                    )
                    .tooltip(|window, cx| gpui_kit::component::tooltip::Tooltip::new("Drag to resize · double-click to fit").build(window, cx)),
            )
    }

    fn vscrollbar(&self, col: usize, near: bool, p: &Palette, cx: &mut Context<Self>) -> impl IntoElement {
        let c = &self.columns[col];
        let h = c.scroll.0.borrow().base_handle.clone();
        let vh = f32::from(h.bounds().size.height);
        let content = c.items.len() as f32 * ROW_H + 8.;
        let visible = vh > 0. && content > vh + 1.;
        let (thumb, track) = thumb_geom(vh, content);
        let max = (content - vh).max(1.);
        let t = (-f32::from(h.offset().y) / max).clamp(0., 1.);
        let top = t * (track - thumb);
        let inset = if near { 0. } else { 3. };
        let dragging = matches!(self.drag, Some(Drag::VThumb { col: dc, .. }) if dc == col);
        div()
            .id(("vlane", col))
            .absolute()
            .top_0()
            .bottom_0()
            .right_0()
            .w(px(LANE))
            .when(visible, |el| {
                el.child(
                    div()
                        .absolute()
                        .top(px(top + inset))
                        .h(px((thumb - inset * 2.).max(8.)))
                        .left(px(if near { 0. } else { 3. }))
                        .right(px(if near { 1. } else { 3. }))
                        .rounded_full()
                        .bg(if near || dragging { p.scrollbar_hover } else { p.scrollbar }),
                )
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, e: &MouseDownEvent, _, cx| {
                        cx.stop_propagation();
                        let y = f32::from(e.position.y) - f32::from(h.bounds().origin.y);
                        // Grabbing outside the thumb centres it under the pointer first.
                        let grab = if y >= top && y <= top + thumb { y - top } else { thumb / 2. };
                        this.drag = Some(Drag::VThumb { col, grab: px(grab) });
                        this.on_drag_move(&MouseMoveEvent { position: e.position, pressed_button: Some(MouseButton::Left), modifiers: e.modifiers }, cx);
                    }),
                )
            })
    }

    fn hscrollbar(&self, p: &Palette, cx: &mut Context<Self>) -> impl IntoElement {
        let vw = self.viewport_width();
        let content = self.total_width() + self.spacer;
        let visible = vw > 0. && content > vw + 1.;
        let (thumb, track) = thumb_geom(vw, content);
        let t = (self.scroll_x() / (content - vw).max(1.)).clamp(0., 1.);
        let left = t * (track - thumb);
        let near = self.near_h || matches!(self.drag, Some(Drag::HThumb { .. }));
        let inset = if near { 1. } else { 3. };
        div()
            .id("hlane")
            .absolute()
            .left_0()
            .right_0()
            .bottom_0()
            .h(px(LANE))
            .when(visible, |el| {
                el.child(
                    div()
                        .absolute()
                        .left(px(left + inset))
                        .w(px((thumb - inset * 2.).max(8.)))
                        .top(px(if near { 0. } else { 3. }))
                        .bottom(px(if near { 0. } else { 3. }))
                        .rounded_full()
                        .bg(if near { p.scrollbar_hover } else { p.scrollbar }),
                )
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, e: &MouseDownEvent, _, cx| {
                        cx.stop_propagation();
                        let x = f32::from(e.position.x - this.hscroll.bounds().origin.x);
                        let grab = if x >= left && x <= left + thumb { x - left } else { thumb / 2. };
                        this.drag = Some(Drag::HThumb { grab: px(grab) });
                        this.on_drag_move(&MouseMoveEvent { position: e.position, pressed_button: Some(MouseButton::Left), modifiers: e.modifiers }, cx);
                    }),
                )
            })
    }

    fn preview(&self, p: &Palette, cx: &App) -> Option<AnyElement> {
        let last = self.chain.last()?;
        if last.is_dir {
            return None;
        }
        let col = self.chain.len() - 1;
        let item = self.columns.get(col)?.items.get(self.selected_index(col)?)?.clone();
        let path = self.selected_path()?;
        let comparing = self.comparing(cx);
        if item.placeholder {
            return Some(
                div()
                    .flex_shrink_0()
                    .w(px(PREVIEW_W))
                    .h_full()
                    .bg(p.column)
                    .border_r_1()
                    .border_color(p.border)
                    .p(px(18.))
                    .flex()
                    .flex_col()
                    .gap(px(10.))
                    .text_size(px(12.))
                    .child(div().font_weight(FontWeight::SEMIBOLD).child(format!("{} · {}", query::UNITEMIZED, format::bytes(item.size))))
                    .child(div().text_color(p.dim).child(
                        "This scan recorded the folder's total size but not its individual files.",
                    ))
                    .into_any_element(),
            );
        }
        let meta = std::fs::symlink_metadata(&path).ok();
        let modified = meta.as_ref().and_then(|m| m.modified().ok()).and_then(unix);
        let created = meta.as_ref().and_then(|m| m.created().ok()).and_then(unix);
        let kind = path.extension().map(|e| format!("{} file", e.to_string_lossy().to_uppercase())).unwrap_or("File".into());
        let cloud_size = self.columns.get(col).and_then(|c| c.cloud.get(&*item.name)).copied().filter(|c| c.online_only() > 0);
        let detail = |label: &'static str, value: String| {
            div()
                .flex()
                .justify_between()
                .gap(px(12.))
                .text_size(px(12.))
                .child(div().text_color(p.dim).child(label))
                .child(div().text_color(p.text).overflow_hidden().text_ellipsis().whitespace_nowrap().child(value))
        };
        let delta_text = match (&item.moved, item.status) {
            (Some(m), _) => format!(
                "Moved {} {}{}",
                if m.arrived { "from" } else { "to" },
                m.other.parent().map(crate::format::home_path).unwrap_or_default(),
                if item.change() != 0 { format!(" · {}", format::delta(item.change())) } else { String::new() }
            ),
            (None, status) => match status {
            Status::Removed => format!("Deleted · was {}", format::bytes(item.prev.unwrap_or(0))),
            Status::Added => format!("New · {}", format::delta(item.delta())),
            Status::Changed => format!("{} · was {}", format::delta(item.delta()), format::bytes(item.prev.unwrap_or(0))),
            Status::Same => "No change".into(),
            _ => "—".into(),
            },
        };
        Some(
            div()
                .flex_shrink_0()
                .w(px(PREVIEW_W))
                .h_full()
                .bg(p.column)
                .border_r_1()
                .border_color(p.border)
                .p(px(18.))
                .flex()
                .flex_col()
                .gap(px(14.))
                .child(div().flex().justify_center().pt(px(18.)).child(icon(IconName::File, 64., p.file)))
                .child(
                    div()
                        .text_center()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_size(px(13.))
                        .overflow_hidden()
                        .text_ellipsis()
                        .child(item.name.to_string()),
                )
                .child(div().text_center().text_size(px(12.)).text_color(p.dim).child(format!("{kind} · {}", format::bytes(item.size.max(item.prev.unwrap_or(0))))))
                .child(div().h(px(1.)).bg(p.border))
                .child(detail("Size", format::bytes(item.size)))
                .when_some(cloud_size, |el, c| {
                    el.child(detail("Total size", format::bytes(c.logical)))
                        .child(detail("On this Mac", format::bytes(c.on_disk)))
                })
                .when(comparing, |el| el.child(detail("Change", delta_text)))
                .when_some(modified, |el, t| el.child(detail("Modified", format::long_date(t))))
                .when_some(created, |el, t| el.child(detail("Created", format::long_date(t))))
                .when(meta.is_none(), |el| el.child(div().text_size(px(12.)).text_color(p.dim).child("No longer on disk.")))
                .into_any_element(),
        )
    }

    fn status_bar(&self, p: &Palette, cx: &mut Context<Self>) -> impl IntoElement {
        let path = self.selected_path().or_else(|| self.root.clone());
        let path_text = path.as_ref().map(|p| p.display().to_string()).unwrap_or_default();
        let copied = self.copied_at.is_some_and(|t| t.elapsed() < Duration::from_millis(1500));
        let has_path = path.is_some();
        let (free, free_tip) = free_space_text(self.state.read(cx).current_scan(), self.free.as_ref().map(|f| f.1));
        let this = cx.entity();
        let (t1, t2, t3, t4) = (this.clone(), this.clone(), this.clone(), this);
        let folder = self.selected_folder();
        let starred = folder.as_ref().is_some_and(|f| self.state.read(cx).is_favorite(f));
        div()
            .h(px(32.))
            .flex_shrink_0()
            .pl(px(14.))
            .pr(px(8.))
            .flex()
            .items_center()
            .gap(px(12.))
            .bg(p.elevated)
            .border_t_1()
            .border_color(p.border)
            .text_size(px(12.))
            .text_color(p.dim)
            .child(
                div()
                    .id("status-path")
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .child(truncate_start(path_text.clone(), 140))
                    .tooltip(move |window, cx| gpui_kit::component::tooltip::Tooltip::new(path_text.clone()).build(window, cx)),
            )
            .when_some(free, |el, free| {
                el.child(
                    div()
                        .id("status-free")
                        .flex_shrink_0()
                        .child(free)
                        .tooltip(move |window, cx| gpui_kit::component::tooltip::Tooltip::new(free_tip.clone()).build(window, cx)),
                )
            })
            .child(div().w(px(1.)).h(px(16.)).bg(p.border))
            .child(icon_button_disabled(
                "favorite",
                if starred { IconName::StarOff } else { IconName::Star },
                if starred { "Remove from Sidebar (⌃⌘T)" } else { "Add to Sidebar (⌃⌘T)" },
                folder.is_none(),
                move |_, cx| t4.update(cx, |this, cx| this.toggle_favorite(cx)),
            ))
            .child(icon_button_disabled(
                "reveal",
                IconName::FolderOpen,
                format!("Show in {} (⌘R)", file_manager()),
                !has_path,
                move |_, cx| t1.update(cx, |this, cx| this.reveal_in_finder(cx)),
            ))
            .child(icon_button_disabled(
                "copy",
                if copied { IconName::Check } else { IconName::Copy },
                if copied { "Copied" } else { "Copy path (⌥⌘C)" },
                !has_path,
                move |_, cx| t2.update(cx, |this, cx| this.copy_path(cx)),
            ))
            .child(icon_button("reset-widths", IconName::Columns3, "Reset column widths", move |_, cx| {
                t3.update(cx, |this, cx| this.reset_widths(cx))
            }))
    }
}

/// The status bar's available-space label and its tooltip. The columns show
/// the scan, so the label leads with the space recorded with it, then what
/// is available now. Scans that recorded no space show only now.
fn free_space_text(scan: Option<&ScanInfo>, now: Option<u64>) -> (Option<String>, SharedString) {
    match scan.filter(|s| s.disk_total > 0) {
        Some(s) => {
            let at_scan = format!("{} available at scan", format::bytes(s.disk_free));
            let when = format::date(s.finished_at);
            match now {
                Some(n) => (
                    Some(format!("{at_scan} · {} now", format::bytes(n))),
                    format!("Available on this disk when the scan ran ({when}), and right now.").into(),
                ),
                None => (Some(at_scan), format!("Available on this disk when the scan ran ({when}).").into()),
            }
        }
        None => (
            now.map(|n| format!("{} available now", format::bytes(n))),
            "Available on this disk right now. This scan didn't record available space.".into(),
        ),
    }
}

/// An invisible layer over the columns that takes scroll events in the
/// capture phase, before the column lists and the strip see them. The hitbox
/// keeps it from stealing scrolls meant for an overlay (e.g. Manage scans).
fn wheel_capture(browser: WeakEntity<Browser>) -> impl IntoElement {
    canvas(
        |bounds, window, _| window.insert_hitbox(bounds, HitboxBehavior::Normal),
        move |_, hitbox, window, _| {
            let browser = browser.clone();
            window.on_mouse_event(move |e: &ScrollWheelEvent, phase, window, cx| {
                if phase != DispatchPhase::Capture || !hitbox.should_handle_scroll(window) {
                    return;
                }
                cx.stop_propagation();
                let _ = browser.update(cx, |this, cx| this.on_wheel(e, cx));
            });
        },
    )
    .absolute()
    .inset_0()
}

/// Thumb length and track length for a viewport/content pair.
fn thumb_geom(view: f32, content: f32) -> (f32, f32) {
    let track = view.max(0.);
    if content <= 0. {
        return (track, track);
    }
    ((view / content * track).clamp(28f32.min(track), track), track)
}

fn unix(t: std::time::SystemTime) -> Option<i64> {
    t.duration_since(std::time::UNIX_EPOCH).ok().map(|d| d.as_secs() as i64)
}

#[allow(clippy::too_many_arguments)]
fn row(
    it: &Item,
    ix: usize,
    col: usize,
    width: f32,
    selected: bool,
    open: bool,
    focused: bool,
    comparing: bool,
    activity: Option<u64>,
    pending: bool,
    cloud_size: Option<CloudSize>,
    // Mid-animation: offset from its slot, opacity, and the size shown.
    motion: Option<(f32, f32, u64)>,
    // In a live scan: whether the folder is still being counted.
    counting: Option<bool>,
    browser: &Entity<Browser>,
    state: &Entity<AppState>,
    p: &Palette,
) -> Stateful<Div> {
    let removed = it.status == Status::Removed;
    // Moved away: shown faded like a deleted item, but not struck through.
    let moved_away = removed && it.moved.is_some();
    let on_sel = selected && focused;
    let fg = if on_sel { p.on_selected } else if removed || it.placeholder { p.faint } else { p.text };
    let sub = if on_sel { p.on_selected.opacity(0.85) } else { p.dim };
    let (icon_name, icon_color) = if it.placeholder {
        (IconName::Files, p.faint)
    } else if it.is_dir {
        (
            if it.package {
                IconName::Package
            } else if it.cloud {
                IconName::Cloud
            } else if it.unreadable {
                IconName::FolderLock
            } else {
                IconName::Folder
            },
            p.folder,
        )
    } else {
        (IconName::File, p.file)
    };
    let icon_color = if on_sel { p.on_selected } else if removed { p.faint } else { icon_color };
    let delta = it.change();
    let cloud = if counting.is_some() { None } else { cloud_badge(it, cloud_size, ix, on_sel, p) };
    let fit = row_fit(it, width, comparing, activity, pending, cloud.is_some());
    let (churn, loud) = (fit.churn, fit.loud);
    let widen = {
        let (browser, w) = (browser.clone(), fit.all_w);
        Rc::new(move |cx: &mut App| browser.update(cx, |b, cx| b.widen(col, w, cx)))
    };

    let delta_el: Option<AnyElement> = comparing.then(|| match it.status {
        Status::NoHistory => div()
            .id(("nohist", ix))
            .text_color(p.faint)
            .child("·")
            .tooltip(|w, cx| gpui_kit::component::tooltip::Tooltip::new("No earlier scan covers this item").build(w, cx))
            .into_any_element(),
        Status::Unreadable => div()
            .id(("unread", ix))
            .text_color(if on_sel { p.on_selected } else { p.faint })
            .child("?")
            .tooltip(|w, cx| {
                gpui_kit::component::tooltip::Tooltip::new("Couldn’t be read in one of the scans, so its change is unknown").build(w, cx)
            })
            .into_any_element(),
        _ => {
            let color = if on_sel { p.on_selected } else { p.delta(delta, it.status == Status::Added) };
            let real = if fit.full_delta {
                div().text_color(color).child(format::delta(delta)).into_any_element()
            } else {
                let tip = format!("Net change: {}", format::delta(delta));
                collapsed(("delta-pill", ix), tip, color, if on_sel { p.on_selected.opacity(0.2) } else { p.border }, widen.clone())
            };
            div()
                .flex()
                .gap(px(6.))
                .when_some(it.moved.as_ref().filter(|m| !m.label.is_empty()), |el, m| el.child(moved_link(m, ix, on_sel, sub, state, p)))
                // Bytes that came from or went to another folder: grey, so
                // the coloured number is only what was added or freed.
                .when(it.moved.is_none() && it.moved_bytes != 0, |el| {
                    let text = format::delta(it.moved_bytes);
                    el.child(if fit.full_moved {
                        div().text_color(sub).child(text).into_any_element()
                    } else {
                        let tip = format!("{text} moved to or from other folders");
                        collapsed(("moved-pill", ix), tip, sub, if on_sel { p.on_selected.opacity(0.2) } else { p.border }, widen.clone())
                    })
                })
                .child(real)
                .into_any_element()
        }
    });

    div()
        .id(SharedString::from(format!("row-{col}-{ix}")))
        .w_full()
        .h(px(ROW_H))
        .px(px(8.))
        .flex()
        .items_center()
        .gap(px(6.))
        .rounded(px(5.))
        .text_size(px(12.))
        .cursor_default()
        .when_some(motion, |el, (offset, alpha, _)| el.relative().top(px(offset)).opacity(alpha))
        .when(selected, |el| el.bg(if focused { p.row_selected } else { p.row_selected_inactive }))
        .when(open, |el| el.bg(p.row_open))
        .when(!selected && !open, |el| el.hover(|s| s.bg(p.row_hover)))
        .child(icon(icon_name, 15., icon_color))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .text_ellipsis()
                .whitespace_nowrap()
                .text_color(fg)
                .when(removed && !moved_away, |el| el.line_through())
                .when(it.placeholder, |el| el.italic())
                .child(display_name(&it.name)),
        )
        .when(pending, |el| el.child(spinner(sub, 10.)))
        .when_some(churn, |el, a| {
            let tip = if loud {
                format!("Total changed inside this folder: {}, far more than its net change", format::bytes(a))
            } else {
                format!("Total changed inside this folder: {}", format::bytes(a))
            };
            let (color, bg) = match (on_sel, loud) {
                (true, _) => (p.on_selected, p.on_selected.opacity(0.2)),
                (false, true) => (p.churn, p.churn_bg),
                (false, false) => (sub, p.border),
            };
            if !fit.full_churn {
                return el.child(div().flex_shrink_0().text_size(px(11.)).child(collapsed(("churn-pill", ix), tip, color, bg, widen.clone())));
            }
            el.child(
                div()
                    .id(("churn", ix))
                    .flex_shrink_0()
                    .text_size(px(11.))
                    .text_color(sub)
                    .when(loud, |el| el.px(px(5.)).rounded(px(4.)).text_color(color).bg(bg))
                    .child(format!("⇅ {}", format::bytes(a)))
                    .tooltip(move |w, cx| gpui_kit::component::tooltip::Tooltip::new(tip.clone()).build(w, cx)),
            )
        })
        .when_some(cloud, |el, b| el.child(b))
        .when_some(delta_el, |el, d| el.child(div().flex_shrink_0().text_size(px(11.)).child(d)))
        .child(
            div()
                .flex_shrink_0()
                .w(px(58.))
                .flex()
                .justify_end()
                // Equal-width digits, so counting sizes don't jitter.
                .font_features(FontFeatures(Arc::new(vec![("tnum".into(), 1)])))
                .text_color(if counting == Some(true) && !on_sel { p.faint } else { sub })
                .child(if removed { String::new() } else { format::bytes(motion.map_or(it.size, |m| m.2)) }),
        )
        .child(
            div()
                .flex_shrink_0()
                .w(px(10.))
                .flex()
                .justify_end()
                .when(it.is_dir, |el| el.child(icon(IconName::ChevronRight, 12., sub))),
        )
}

/// How a row's figures fit its column. The name keeps its width, up to
/// [`NAME_MIN`]; past that the grey moved bytes, then the churn, then the
/// net change shrink to "…" pills. Widths are estimated from the text (11 px digits are about 6.3 px wide, 12 px
/// name letters about 6.2).
struct RowFit {
    /// Folder churn worth showing, and whether it dwarfs the net change.
    churn: Option<u64>,
    loud: bool,
    full_churn: bool,
    full_moved: bool,
    full_delta: bool,
    /// Column width that shows the whole name and every figure.
    all_w: f32,
}

fn row_fit(it: &Item, width: f32, comparing: bool, activity: Option<u64>, pending: bool, cloud: bool) -> RowFit {
    let delta = it.change();
    // Folder churn: only when it says more than the net delta does. Moved
    // bytes already show in grey, so they don't count as churn.
    let churn = activity.map(|a| a.saturating_sub(it.moved_bytes.unsigned_abs())).filter(|&a| {
        let d = delta.unsigned_abs();
        comparing && it.is_dir && a >= MB && (d == 0 || (a as f64 >= 1.25 * d as f64 && a - d >= MB))
    });
    // Churn that dwarfs the net delta (e.g. ⇅ 27 GB behind −4 GB) means far
    // more happened than the coloured number says, so it stands out.
    let loud = churn.is_some_and(|a| a >= GB && a >= 3 * delta.unsigned_abs());

    let est = |s: &str| s.chars().count() as f32 * 6.3;
    let mut fixed = 2. * 8. + 15. + 58. + 10. + 3. * 6.;
    if pending {
        fixed += 10. + 6.;
    }
    if cloud {
        fixed += 60. + 6.;
    }
    let (mut moved_w, mut real_w) = (0., 0.);
    if comparing {
        if !matches!(it.status, Status::NoHistory | Status::Unreadable) {
            real_w = est(&format::delta(delta)) + 6.;
        }
        match it.moved.as_ref() {
            Some(m) if !m.label.is_empty() => fixed += est(&m.label) + est("from ") + 6.,
            Some(_) => {}
            None if it.moved_bytes != 0 => moved_w = est(&format::delta(it.moved_bytes)) + 6.,
            None => {}
        }
    }
    let churn_w = churn.map_or(0., |a| est(&format!("⇅ {}", format::bytes(a))) + if loud { 10. } else { 0. } + 6.);
    let pill = |w: f32| if w > 0. { PILL_W + 6. } else { 0. };
    let name_w = it.name.chars().count() as f32 * 6.2 + 2.;
    // The row sits inside the column's gutters.
    let room = width - 2. * GUTTER - 4. - fixed - name_w.min(NAME_MIN);
    RowFit {
        churn,
        loud,
        full_moved: room >= real_w + churn_w + moved_w,
        full_churn: room >= real_w + churn_w + pill(moved_w),
        full_delta: room >= real_w + pill(churn_w) + pill(moved_w),
        all_w: 2. * GUTTER + 4. + fixed + real_w + churn_w + moved_w + name_w,
    }
}

/// A figure with no room in its row, shrunk to a "…" pill. Hovering shows
/// the figure; double-clicking widens the column so it shows in full.
fn collapsed(id: impl Into<ElementId>, tip: String, color: Hsla, bg: Hsla, widen: Rc<dyn Fn(&mut App)>) -> AnyElement {
    let tip: SharedString = format!("{tip}. Double-click to widen the column.").into();
    div()
        .id(id)
        .w(px(PILL_W))
        .flex()
        .justify_center()
        .rounded(px(4.))
        .text_color(color)
        .bg(bg)
        .child("…")
        .on_click(move |e: &ClickEvent, _, cx| {
            if e.click_count() == 2 {
                cx.stop_propagation();
                widen(cx);
            }
        })
        .tooltip(move |w, cx| gpui_kit::component::tooltip::Tooltip::new(tip.clone()).build(w, cx))
        .into_any_element()
}

/// "from Downloads" / "to Projects": the other end of a moved item, as a
/// link that reveals it.
fn moved_link(m: &diskdiff_engine::Moved, ix: usize, on_sel: bool, sub: Hsla, state: &Entity<AppState>, p: &Palette) -> AnyElement {
    let (word, tip) = if m.arrived { ("from", "Moved here from") } else { ("to", "Moved to") };
    let tip: SharedString = format!("{tip} {}", format::home_path(&m.other)).into();
    let (state, target) = (state.clone(), m.other.clone());
    div()
        .flex()
        .gap(px(3.))
        .text_color(sub)
        .child(word)
        .child(
            div()
                .id(("moved", ix))
                .cursor_pointer()
                .text_color(if on_sel { p.on_selected } else { p.accent_text })
                .hover(|s| s.underline())
                .child(m.label.to_string())
                .on_click(move |_, _, cx| {
                    cx.stop_propagation();
                    state.update(cx, |s, cx| s.go_to(target.clone(), cx));
                })
                .tooltip(move |w, cx| gpui_kit::component::tooltip::Tooltip::new(tip.clone()).build(w, cx)),
        )
        .into_any_element()
}

/// "☁ 2.3 GB" when an item holds files whose contents are online only:
/// their real length, next to the on-disk size the row already shows.
fn cloud_badge(it: &Item, c: Option<CloudSize>, ix: usize, on_sel: bool, p: &Palette) -> Option<AnyElement> {
    let color = if on_sel { p.on_selected } else { p.accent };
    let (text, tip) = match c {
        Some(c) if c.online_only() >= MB || (c.partial && c.logical > 0) => (
            format!("{}{}", format::bytes(c.logical), if c.partial { "+" } else { "" }),
            format!(
                "{} in total, {} on this Mac{}",
                format::bytes(c.logical),
                format::bytes(c.on_disk),
                if c.partial { ". At least: some folders inside are online only and weren’t listed" } else { "" }
            ),
        ),
        Some(c) if c.partial || it.cloud => {
            ("online".into(), "Contents are online only; macOS hasn’t listed them on this Mac".into())
        }
        _ if it.cloud => ("online".into(), "Contents are online only; macOS hasn’t listed them on this Mac".into()),
        _ => return None,
    };
    Some(
        div()
            .id(("cloud", ix))
            .flex_shrink_0()
            .flex()
            .items_center()
            .gap(px(3.))
            .text_size(px(11.))
            .text_color(color)
            .child(icon(IconName::Cloud, 11., color))
            .child(text)
            .tooltip(move |w, cx| gpui_kit::component::tooltip::Tooltip::new(tip.clone()).build(w, cx))
            .into_any_element(),
    )
}

fn display_name(name: &str) -> String {
    if cfg!(windows) && name.eq_ignore_ascii_case("$recycle.bin") { "Recycle Bin".into() } else { name.to_string() }
}

/// Finder-like name order: case-insensitive, numbers by value ("file 2" before "file 10").
pub fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let (mut x, mut y) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (x.peek().copied(), y.peek().copied()) {
            (None, None) => return a.cmp(b),
            (None, _) => return Ordering::Less,
            (_, None) => return Ordering::Greater,
            (Some(c), Some(d)) if c.is_ascii_digit() && d.is_ascii_digit() => {
                let mut na = String::new();
                while let Some(c) = x.peek().copied().filter(char::is_ascii_digit) {
                    na.push(c);
                    x.next();
                }
                let mut nb = String::new();
                while let Some(d) = y.peek().copied().filter(char::is_ascii_digit) {
                    nb.push(d);
                    y.next();
                }
                let (ta, tb) = (na.trim_start_matches('0'), nb.trim_start_matches('0'));
                let ord = ta.len().cmp(&tb.len()).then_with(|| ta.cmp(tb));
                if ord != Ordering::Equal {
                    return ord;
                }
            }
            (Some(c), Some(d)) => {
                let ord = c.to_lowercase().cmp(d.to_lowercase());
                if ord != Ordering::Equal {
                    return ord;
                }
                x.next();
                y.next();
            }
        }
    }
}

/// Removed items sort by the size they had.
fn shown_size(it: &Item) -> u64 {
    if it.status == Status::Removed { it.prev.unwrap_or(0) } else { it.size }
}

/// The browser's ordering (the same for every column):
/// * size: by size;
/// * name: folders first in both directions, then natural name order;
/// * change: by |delta|, unchanged rows always last;
/// * change +/−: unchanged last, growers before shrinkers in either
///   direction, each group by |delta|.
pub fn sort_items(items: &mut [Item], key: SortKey, descending: bool, comparing: bool) {
    use std::cmp::Ordering;
    let key = if !comparing && matches!(key, SortKey::Delta | SortKey::DeltaSigned) { SortKey::Size } else { key };
    let dir = |o: Ordering| if descending { o.reverse() } else { o };
    let changed = |it: &Item| it.has_delta() && it.change() != 0;
    items.sort_by(|a, b| match key {
        SortKey::Size => dir(shown_size(a).cmp(&shown_size(b))).then_with(|| natural_cmp(&a.name, &b.name)),
        SortKey::Name => b.is_dir.cmp(&a.is_dir).then_with(|| dir(natural_cmp(&a.name, &b.name))),
        SortKey::Delta => changed(b)
            .cmp(&changed(a))
            .then_with(|| dir(a.change().unsigned_abs().cmp(&b.change().unsigned_abs())))
            .then_with(|| natural_cmp(&a.name, &b.name)),
        SortKey::DeltaSigned => changed(b)
            .cmp(&changed(a))
            .then_with(|| (b.change() > 0).cmp(&(a.change() > 0)))
            .then_with(|| dir(a.change().unsigned_abs().cmp(&b.change().unsigned_abs())))
            .then_with(|| natural_cmp(&a.name, &b.name)),
    });
}

impl Focusable for Browser {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for Browser {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = cx.palette();
        let focused = self.focus.contains_focused(window, cx) || self.focus.is_focused(window);
        if let Some(col) = self.pending_reveal_col {
            if self.viewport_width() > 0. {
                self.pending_reveal_col = None;
                if self.chain.last().is_some_and(|c| !c.is_dir) {
                    self.ensure_preview_visible();
                } else {
                    self.ensure_visible(col);
                }
            } else {
                cx.on_next_frame(window, |_, _, cx| cx.notify());
            }
        }
        // Animation: read a live scan a few times a second and ease every row
        // towards it on each frame in between.
        let now = Instant::now();
        let dt = (now - self.frame_at).as_secs_f32().min(0.1);
        self.frame_at = now;
        if self.live.is_some() && now - self.live_at >= LIVE_TICK {
            self.live_at = now;
            self.load_columns(cx);
        }
        let live = self.live.is_some();
        let mut moving = false;
        for col in &mut self.columns {
            moving |= col.step(dt, live);
        }
        if live || moving {
            window.request_animation_frame();
        }
        // Drop the trailing spacer once the user scrolls back into the content.
        if self.spacer > 0. && self.scroll_x() + self.viewport_width() <= self.total_width() + 1. {
            self.spacer = 0.;
        }
        let columns: Vec<AnyElement> =
            (0..self.columns.len()).map(|i| self.render_column(i, focused, &p, cx).into_any_element()).collect();
        let preview = self.preview(&p, cx);
        let spacer = self.spacer;

        div()
            .id("browser")
            .key_context("Browser")
            .track_focus(&self.focus)
            .size_full()
            .flex()
            .flex_col()
            .bg(p.column)
            .on_action(cx.listener(|this, _: &SelectPrev, _, cx| this.move_selection(-1, cx)))
            .on_action(cx.listener(|this, _: &SelectNext, _, cx| this.move_selection(1, cx)))
            .on_action(cx.listener(|this, _: &SelectFirst, _, cx| this.move_selection(isize::MIN / 2, cx)))
            .on_action(cx.listener(|this, _: &SelectLast, _, cx| this.move_selection(isize::MAX / 2, cx)))
            .on_action(cx.listener(|this, _: &OpenSelected, _, cx| {
                let n = this.chain.len();
                if this.chain.last().is_some_and(|c| c.is_dir) && this.columns.get(n).is_some_and(|c| !c.items.is_empty()) {
                    this.select(n, 0, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &GoBack, _, cx| {
                if this.chain.len() > 1 {
                    let col = this.chain.len() - 2;
                    if let Some(ix) = this.selected_index(col) {
                        this.select(col, ix, cx);
                    }
                }
            }))
            .on_action(cx.listener(|this, _: &GoParent, _, cx| {
                if this.chain.len() > 1 {
                    let col = this.chain.len() - 2;
                    if let Some(ix) = this.selected_index(col) {
                        this.select(col, ix, cx);
                    }
                }
            }))
            .on_action(cx.listener(|this, _: &Reveal, _, cx| this.reveal_in_finder(cx)))
            .on_action(cx.listener(|this, _: &CopyPath, _, cx| this.copy_path(cx)))
            .on_action(cx.listener(|this, _: &crate::menus::Copy, _, cx| this.copy_path(cx)))
            .on_action(cx.listener(|this, _: &OpenItem, _, cx| this.open_item(cx)))
            .on_action(cx.listener(|this, _: &AddToSidebar, _, cx| this.toggle_favorite(cx)))
            .on_key_down(cx.listener(|this, e: &KeyDownEvent, _, cx| {
                let m = &e.keystroke.modifiers;
                if m.platform || m.control || m.alt || m.function {
                    return;
                }
                if let Some(ch) = e.keystroke.key_char.as_deref() {
                    if ch.chars().count() == 1 && !ch.chars().all(char::is_control) && (ch != " " || !this.typed.is_empty()) {
                        this.type_select(ch, cx);
                        cx.stop_propagation();
                    }
                }
            }))
            .on_mouse_move(cx.listener(|this, e: &MouseMoveEvent, _, cx| {
                if this.drag.is_some() {
                    if e.pressed_button == Some(MouseButton::Left) {
                        this.on_drag_move(e, cx);
                    } else {
                        this.drag = None;
                        cx.notify();
                    }
                    return;
                }
                let b = this.hscroll.bounds();
                let near = b.size.height > px(0.) && f32::from(b.origin.y + b.size.height - e.position.y) <= NEAR && b.contains(&e.position);
                if near != this.near_h {
                    this.near_h = near;
                    cx.notify();
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    if this.drag.take().is_some() {
                        cx.notify();
                    }
                }),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    if this.drag.take().is_some() {
                        cx.notify();
                    }
                }),
            )
            .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                if !*hovered && (this.near_v.is_some() || this.near_h) {
                    this.near_v = None;
                    this.near_h = false;
                    cx.notify();
                }
            }))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .relative()
                    .child(
                        div()
                            .id("columns")
                            .size_full()
                            .flex()
                            .overflow_x_scroll()
                            .track_scroll(&self.hscroll)
                            .on_scroll_wheel(cx.listener(|_, _, _, cx| cx.notify()))
                            .children(columns)
                            .children(preview)
                            .when(spacer > 0., |el| el.child(div().flex_shrink_0().w(px(spacer)).h_full())),
                    )
                    .child(self.hscrollbar(&p, cx))
                    .child(wheel_capture(cx.entity().downgrade())),
            )
            .child(self.status_bar(&p, cx))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // `gpui_kit::*` brings GPUI's own `test` macro; use the standard one.
    use core::prelude::v1::test;

    fn item(name: &str, is_dir: bool, size: u64, prev: Option<u64>) -> Item {
        let status = match prev {
            None => Status::Added,
            Some(p) if p == size => Status::Same,
            Some(_) if size == 0 => Status::Removed,
            Some(_) => Status::Changed,
        };
        Item { name: name.into(), is_dir, package: false, unreadable: false, cloud: false, size, prev, files: 1, dirs: 0, status, placeholder: false, moved_bytes: 0, moved: None }
    }

    fn names(v: &[Item]) -> Vec<&str> {
        v.iter().map(|i| &*i.name).collect()
    }

    #[test]
    fn natural_order() {
        let mut v = vec!["file 10", "File 2", "file 1", "alpha"];
        v.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(v, ["alpha", "file 1", "File 2", "file 10"]);
    }

    #[test]
    fn name_sort_keeps_folders_first_both_ways() {
        let mut v = vec![item("b.txt", false, 1, Some(1)), item("Zed", true, 1, Some(1)), item("a.txt", false, 1, Some(1)), item("Apps", true, 1, Some(1))];
        sort_items(&mut v, SortKey::Name, false, true);
        assert_eq!(names(&v), ["Apps", "Zed", "a.txt", "b.txt"]);
        sort_items(&mut v, SortKey::Name, true, true);
        assert_eq!(names(&v), ["Zed", "Apps", "b.txt", "a.txt"]);
    }

    #[test]
    fn change_sorts_sink_unchanged_rows() {
        let mut v = vec![
            item("same", false, 5, Some(5)),
            item("grow-small", false, 6, Some(5)),
            item("shrink-big", false, 1, Some(50)),
            item("grow-big", false, 90, Some(10)),
        ];
        sort_items(&mut v, SortKey::Delta, true, true);
        assert_eq!(names(&v), ["grow-big", "shrink-big", "grow-small", "same"]);
        sort_items(&mut v, SortKey::Delta, false, true);
        assert_eq!(names(&v), ["grow-small", "shrink-big", "grow-big", "same"]);
        sort_items(&mut v, SortKey::DeltaSigned, true, true);
        assert_eq!(names(&v), ["grow-big", "grow-small", "shrink-big", "same"]);
        sort_items(&mut v, SortKey::DeltaSigned, false, true);
        assert_eq!(names(&v), ["grow-small", "grow-big", "shrink-big", "same"]);
    }

    #[test]
    fn thumb_never_exceeds_track() {
        assert_eq!(thumb_geom(100., 50.), (100., 100.));
        let (t, tr) = thumb_geom(100., 10_000.);
        assert!(t >= 28. && tr == 100.);
    }
}
