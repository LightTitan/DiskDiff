//! Manage scans: every saved scan grouped by location, what deleting each
//! would free, multi-select deletion, and database upkeep.
//!
//! A full-window overlay; the workspace adds it only while
//! `state.show_manage` is set. Closes with ×, Escape, or a click outside.

use crate::format;
use crate::state::{AppState, file_manager, location_detail, location_name};
use crate::theme::{ActivePalette, Palette};
use crate::widgets::{icon, icon_button, spinner};
use diskdiff_engine::ScanInfo;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::scroll::ScrollableElement;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{Disableable, Icon, Sizable};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Confirm {
    Row(i64),
    Bulk,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Busy {
    Deleting,
    Compacting,
}

struct Group {
    root: PathBuf,
    tracked: bool,
    /// Newest first.
    scans: Vec<ScanInfo>,
}

pub struct ManageView {
    state: Entity<AppState>,
    focus: FocusHandle,
    /// What had focus before the window opened, to hand it back on close.
    prev_focus: Option<FocusHandle>,
    open: bool,
    collapsed: HashSet<PathBuf>,
    selected: BTreeSet<i64>,
    confirm: Option<Confirm>,
    busy: Option<Busy>,
    /// Bytes deleting each scan would free, filled in lazily.
    /// `None` when it could not be worked out.
    frees: HashMap<i64, Option<u64>>,
    frees_version: Option<u64>,
    db_bytes: Option<u64>,
    stats_version: Option<u64>,
    note: Option<SharedString>,
    _tasks: Vec<Task<()>>,
}

impl ManageView {
    pub fn new(state: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        cx.observe(&state, |this, state, cx| {
            if !state.read(cx).show_manage && this.open {
                this.open = false;
                this.confirm = None;
            }
            cx.notify();
        })
        .detach();
        let _ = window;
        ManageView {
            state,
            focus: cx.focus_handle(),
            prev_focus: None,
            open: false,
            collapsed: HashSet::new(),
            selected: BTreeSet::new(),
            confirm: None,
            busy: None,
            frees: HashMap::new(),
            frees_version: None,
            db_bytes: None,
            stats_version: None,
            note: None,
            _tasks: Vec::new(),
        }
    }

    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.confirm = None;
        if let Some(f) = self.prev_focus.take() {
            f.focus(window, cx);
        }
        self.state.update(cx, |s, cx| {
            s.show_manage = false;
            cx.notify();
        });
    }

    fn groups(&self, cx: &App) -> Vec<Group> {
        let s = self.state.read(cx);
        let mut by_root: HashMap<PathBuf, Vec<ScanInfo>> = HashMap::new();
        for scan in s.scans.iter().filter(|x| !x.temporary) {
            by_root.entry(PathBuf::from(&scan.root)).or_default().push(scan.clone());
        }
        let mut groups: Vec<Group> = by_root
            .into_iter()
            .map(|(root, mut scans)| {
                scans.sort_by_key(|x| std::cmp::Reverse((x.started_at, x.id)));
                Group { tracked: s.settings.locations.contains(&root), root, scans }
            })
            .collect();
        groups.sort_by(|a, b| {
            b.tracked
                .cmp(&a.tracked)
                .then_with(|| location_name(&a.root).to_lowercase().cmp(&location_name(&b.root).to_lowercase()))
                .then_with(|| a.root.cmp(&b.root))
        });
        groups
    }

    /// Refreshes the database size and the per-scan "frees" figures when the
    /// stored data changed since they were computed.
    fn refresh(&mut self, cx: &mut Context<Self>) {
        let (version, store, ids) = {
            let s = self.state.read(cx);
            (s.data_version, s.store.clone(), s.scans.iter().filter(|x| !x.temporary).map(|x| x.id).collect::<Vec<_>>())
        };
        if self.stats_version != Some(version) {
            self.stats_version = Some(version);
            self.load_stats(cx);
        }
        if self.frees_version != Some(version) {
            self.frees_version = Some(version);
            self.frees.clear();
            // One scan at a time, each on the background executor, so the
            // figures appear as they are ready.
            let task = cx.spawn(async move |this, cx| {
                for id in ids {
                    let store = store.clone();
                    let bytes = cx.background_executor().spawn(async move { store.exclusive_bytes(id) }).await;
                    let alive = this.update(cx, |this, cx| {
                        if this.frees_version != Some(version) {
                            return false;
                        }
                        this.frees.insert(id, bytes.ok());
                        cx.notify();
                        true
                    });
                    if !matches!(alive, Ok(true)) {
                        return;
                    }
                }
            });
            self._tasks.push(task);
        }
        self._tasks.retain(|t| !t.is_ready());
    }

    fn load_stats(&mut self, cx: &mut Context<Self>) {
        let store = self.state.read(cx).store.clone();
        let task = cx.spawn(async move |this, cx| {
            let stats = cx.background_executor().spawn(async move { store.stats() }).await;
            let _ = this.update(cx, |this, cx| {
                if let Ok(st) = stats {
                    this.db_bytes = Some(st.file_bytes);
                    cx.notify();
                }
            });
        });
        self._tasks.push(task);
    }

    fn delete(&mut self, ids: Vec<i64>, cx: &mut Context<Self>) {
        if self.busy.is_some() || ids.is_empty() {
            return;
        }
        self.busy = Some(Busy::Deleting);
        self.confirm = None;
        self.note = None;
        for id in &ids {
            self.selected.remove(id);
        }
        let n = ids.len();
        let task = self.state.update(cx, |s, cx| s.delete_scans(ids, cx));
        let done = cx.spawn(async move |this, cx| {
            task.await;
            let _ = this.update(cx, |this, cx| {
                this.busy = None;
                this.note = Some(format!("Deleted {n} scan{}", if n == 1 { "" } else { "s" }).into());
                cx.notify();
            });
        });
        self._tasks.push(done);
        cx.notify();
    }

    fn compact(&mut self, cx: &mut Context<Self>) {
        if self.busy.is_some() {
            return;
        }
        self.busy = Some(Busy::Compacting);
        self.confirm = None;
        self.note = None;
        let store = self.state.read(cx).store.clone();
        let before = self.db_bytes;
        let task = cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    store.compact()?;
                    store.stats()
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.busy = None;
                match result {
                    Ok(st) => {
                        let saved = before.unwrap_or(st.file_bytes).saturating_sub(st.file_bytes);
                        this.db_bytes = Some(st.file_bytes);
                        this.note = Some(if saved > 0 {
                            format!("Reclaimed {}", format::bytes(saved)).into()
                        } else {
                            "Nothing to reclaim".into()
                        });
                    }
                    Err(e) => {
                        this.state.update(cx, |s, cx| {
                            s.notify_toast("Database cleanup failed", Some(format!("{e:#} You can retry from Manage scans.").into()), cx)
                        });
                    }
                }
                cx.notify();
            });
        });
        self._tasks.push(task);
        cx.notify();
    }

    fn toggle(&mut self, id: i64, cx: &mut Context<Self>) {
        if !self.selected.remove(&id) {
            self.selected.insert(id);
        }
        if self.confirm == Some(Confirm::Bulk) {
            self.confirm = None;
        }
        cx.notify();
    }
}

impl Render for ManageView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.open {
            self.open = true;
            self.note = None;
            self.prev_focus = window.focused(cx);
            self.focus.focus(window, cx);
        }
        self.refresh(cx);
        let p = cx.palette();
        let groups = self.groups(cx);
        // Drop selections of scans that no longer exist.
        let existing: HashSet<i64> = groups.iter().flat_map(|g| g.scans.iter().map(|s| s.id)).collect();
        self.selected.retain(|id| existing.contains(id));

        // Fit the content, up to 82 % of the window; the list scrolls beyond.
        let rows: usize = groups.iter().map(|g| if self.collapsed.contains(&g.root) { 0 } else { g.scans.len() }).sum();
        let open_groups = groups.iter().filter(|g| !self.collapsed.contains(&g.root)).count();
        let content = 46. + 52. + groups.len() as f32 * 38. + open_groups as f32 * 26. + rows as f32 * 47. + 12.;
        let height = content.max(200.).min(f32::from(window.viewport_size().height) * 0.82);
        let modal = div()
            .id("manage-modal")
            .w(px(920.))
            .max_w(relative(0.95))
            .h(px(height))
            .flex()
            .flex_col()
            .bg(p.elevated)
            .border_1()
            .border_color(p.border)
            .rounded(px(10.))
            .shadow_lg()
            .overflow_hidden()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                div()
                    .flex_shrink_0()
                    .px(px(18.))
                    .py(px(12.))
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .border_b_1()
                    .border_color(p.border)
                    .child(div().text_size(px(14.)).font_weight(FontWeight::SEMIBOLD).child("Manage scans"))
                    .child(div().text_size(px(12.)).text_color(p.dim).child(format!(
                        "{} saved scan{}",
                        existing.len(),
                        if existing.len() == 1 { "" } else { "s" }
                    )))
                    .child(div().flex_1())
                    .child(icon_button("manage-close", IconName::X, "Close", {
                        let this = cx.entity();
                        move |window, cx| this.update(cx, |this, cx| this.close(window, cx))
                    })),
            )
            .child(
                div()
                    .id("manage-list")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scrollbar()
                    .child(self.list(&p, &groups, cx)),
            )
            .child(self.footer(&p, cx));

        div()
            .id("manage-overlay")
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .bg(p.shadow)
            .occlude()
            .cursor_default()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, ev: &KeyDownEvent, window, cx| {
                if ev.keystroke.key == "escape" {
                    cx.stop_propagation();
                    this.close(window, cx);
                }
            }))
            .on_mouse_down(MouseButton::Left, cx.listener(|this, _, window, cx| this.close(window, cx)))
            .child(modal)
    }
}

// ---- list -----------------------------------------------------------------

const COL_SIZE: f32 = 86.;
const COL_COUNTS: f32 = 150.;
const COL_TIME: f32 = 64.;
const COL_FREES: f32 = 86.;
const COL_ACTION: f32 = 128.;

impl ManageView {
    fn list(&self, p: &Palette, groups: &[Group], cx: &mut Context<Self>) -> impl IntoElement {
        if groups.is_empty() {
            return div()
                .px(px(18.))
                .py(px(30.))
                .text_color(p.dim)
                .child("No saved scans yet. Scans you run are listed here, grouped by location.")
                .into_any_element();
        }
        let s = self.state.read(cx);
        let (current, compare) = (s.current, s.compare.filter(|_| s.comparing()));
        div()
            .flex()
            .flex_col()
            .pb(px(8.))
            .children(groups.iter().enumerate().map(|(gi, g)| self.group(p, gi, g, current, compare, cx)))
            .into_any_element()
    }

    fn group(&self, p: &Palette, gi: usize, g: &Group, current: Option<i64>, compare: Option<i64>, cx: &mut Context<Self>) -> AnyElement {
        let open = !self.collapsed.contains(&g.root);
        let root = g.root.clone();
        let name = location_name(&g.root);
        let detail = location_detail(&g.root);
        let n = g.scans.len();
        let header = div()
            .id(SharedString::from(format!("group-{gi}")))
            .px(px(18.))
            .pt(px(12.))
            .pb(px(6.))
            .flex()
            .items_center()
            .gap(px(8.))
            .cursor_default()
            .when(gi > 0, |el| el.border_t_1().border_color(p.border))
            .on_click(cx.listener(move |this, _, _, cx| {
                if !this.collapsed.remove(&root) {
                    this.collapsed.insert(root.clone());
                }
                cx.notify();
            }))
            .child(icon(if open { IconName::ChevronDown } else { IconName::ChevronRight }, 14., p.dim))
            .child(icon(IconName::HardDrive, 14., p.dim))
            .child(div().flex_shrink_0().font_weight(FontWeight::SEMIBOLD).child(name))
            .child(
                div()
                    .min_w_0()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis_start()
                    .text_size(px(11.))
                    .text_color(p.dim)
                    .child(detail),
            )
            .when(!g.tracked, |el| {
                el.child(
                    div()
                        .id(SharedString::from(format!("untracked-{gi}")))
                        .flex_shrink_0()
                        .px(px(7.))
                        .py(px(1.))
                        .rounded(px(8.))
                        .border_1()
                        .border_color(p.new.opacity(0.6))
                        .text_color(p.new)
                        .text_size(px(10.5))
                        .child("No longer tracked")
                        .tooltip(|window, cx| {
                            Tooltip::new(
                                "This location was removed from your list. Its scans still take up space — delete them here to reclaim it.",
                            )
                            .build(window, cx)
                        }),
                )
            })
            .child(div().flex_1())
            .child(div().flex_shrink_0().text_size(px(11.5)).text_color(p.dim).child(format!(
                "{n} scan{}",
                if n == 1 { "" } else { "s" }
            )));

        let mut el = div().flex().flex_col().child(header);
        if open {
            el = el.child(self.table_header(p, gi, g, cx));
            for scan in &g.scans {
                el = el.child(self.row(p, scan, current == Some(scan.id), compare == Some(scan.id), cx));
            }
        }
        el.into_any_element()
    }

    fn table_header(&self, p: &Palette, gi: usize, g: &Group, cx: &mut Context<Self>) -> impl IntoElement {
        let ids: Vec<i64> = g.scans.iter().map(|s| s.id).collect();
        let picked = ids.iter().filter(|id| self.selected.contains(id)).count();
        let state = if picked == 0 {
            Check::Off
        } else if picked == ids.len() {
            Check::On
        } else {
            Check::Mixed
        };
        let cell = |w: f32, text: &'static str| div().w(px(w)).flex_shrink_0().flex().justify_end().child(text);
        div()
            .px(px(18.))
            .py(px(4.))
            .flex()
            .items_center()
            .gap(px(12.))
            .text_size(px(11.))
            .text_color(p.dim)
            .child(checkbox(
                p,
                SharedString::from(format!("all-{gi}")),
                state,
                cx.listener(move |this, _, _, cx| {
                    if picked == ids.len() {
                        for id in &ids {
                            this.selected.remove(id);
                        }
                    } else {
                        this.selected.extend(ids.iter().copied());
                    }
                    if this.confirm == Some(Confirm::Bulk) {
                        this.confirm = None;
                    }
                    cx.notify();
                }),
            ))
            .child(div().flex_1().min_w_0().child("Scan"))
            .child(cell(COL_SIZE, "Size"))
            .child(cell(COL_COUNTS, "Folders / files"))
            .child(cell(COL_TIME, "Took"))
            .child(
                div()
                    .id(SharedString::from(format!("frees-head-{gi}")))
                    .w(px(COL_FREES))
                    .flex_shrink_0()
                    .flex()
                    .justify_end()
                    .child("Deleting frees")
                    .tooltip(|window, cx| {
                        Tooltip::new("Database space that deleting this scan alone would free. Your own files are never touched.")
                            .build(window, cx)
                    }),
            )
            .child(div().w(px(COL_ACTION)).flex_shrink_0())
    }

    fn row(&self, p: &Palette, s: &ScanInfo, is_current: bool, is_compare: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let id = s.id;
        let picked = self.selected.contains(&id);
        let confirming = self.confirm == Some(Confirm::Row(id));
        let busy = self.busy.is_some();
        let frees = self.frees.get(&id).copied();
        let tint = if is_current { Some(p.accent_tint.opacity(0.5)) } else { None };
        let hover = if is_current { p.accent_tint } else { p.row_hover };

        let mut flags = div().flex().items_center().gap(px(6.)).text_size(px(11.));
        if is_current {
            flags = flags.child(tag(p.accent_text, p.accent_tint, "Current"));
        }
        if is_compare {
            flags = flags.child(tag(p.new, p.new.opacity(0.14), "Compared"));
        }
        if !s.full_access {
            flags = flags.child(
                div()
                    .id(SharedString::from(format!("limited-{id}")))
                    .flex()
                    .items_center()
                    .gap(px(3.))
                    .text_color(p.new)
                    .child(icon(IconName::ShieldAlert, 12., p.new))
                    .child("Limited access")
                    .tooltip(|window, cx| {
                        Tooltip::new("This scan ran without Full Disk Access, so protected folders are missing from it.").build(window, cx)
                    }),
            );
        }
        if s.unreadable > 0 {
            flags = flags.child(div().text_color(p.dim).child(format!(
                "{} unreadable folder{}",
                format::count(s.unreadable),
                if s.unreadable == 1 { "" } else { "s" }
            )));
        }
        if !s.note.is_empty() {
            flags = flags.child(div().text_color(p.dim).overflow_hidden().text_ellipsis().whitespace_nowrap().child(s.note.clone()));
        }

        let cell = |w: f32| div().w(px(w)).flex_shrink_0().flex().justify_end();
        let delete = if confirming {
            Button::new(SharedString::from(format!("delete-{id}")))
                .small()
                .danger()
                .label("Confirm delete")
                .disabled(busy)
                .on_click(cx.listener(move |this, _, _, cx| this.delete(vec![id], cx)))
        } else {
            Button::new(SharedString::from(format!("delete-{id}")))
                .small()
                .icon(Icon::new(IconName::Trash))
                .label("Delete")
                .tooltip("Delete this scan; other scans keep everything they share with it")
                .disabled(busy)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.confirm = Some(Confirm::Row(id));
                    cx.notify();
                }))
        };

        div()
            .id(SharedString::from(format!("scan-{id}")))
            .mx(px(8.))
            .px(px(10.))
            .py(px(7.))
            .flex()
            .items_center()
            .gap(px(12.))
            .rounded(px(6.))
            .text_size(px(12.))
            .cursor_default()
            .when_some(tint, |el, t| el.bg(t))
            .hover(move |st| st.bg(hover))
            .on_click(cx.listener(move |this, _, _, cx| this.toggle(id, cx)))
            .child(checkbox(
                p,
                SharedString::from(format!("pick-{id}")),
                if picked { Check::On } else { Check::Off },
                cx.listener(move |this, _, _, cx| this.toggle(id, cx)),
            ))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .child(div().font_weight(FontWeight::MEDIUM).child(format::long_date(s.started_at)))
                    .child(flags),
            )
            .child(cell(COL_SIZE).child(format::bytes(s.size)))
            .child(cell(COL_COUNTS).text_color(p.dim).child(format!("{} / {}", format::count(s.dirs), format::count(s.files))))
            .child(cell(COL_TIME).text_color(p.dim).child(duration(s.finished_at - s.started_at)))
            .child(cell(COL_FREES).child(match frees {
                Some(Some(b)) => div().text_color(p.text).child(format::bytes(b)).into_any_element(),
                Some(None) => div().text_color(p.faint).child("—").into_any_element(),
                None => spinner(p.faint, 12.).into_any_element(),
            }))
            .child(div().w(px(COL_ACTION)).flex_shrink_0().flex().justify_end().child(delete))
    }

    fn footer(&self, p: &Palette, cx: &mut Context<Self>) -> impl IntoElement {
        let busy = self.busy;
        let n = self.selected.len();
        let bulk_confirm = self.confirm == Some(Confirm::Bulk);
        let store_path = self.state.read(cx).store.path().to_path_buf();
        div()
            .flex_shrink_0()
            .px(px(18.))
            .py(px(10.))
            .flex()
            .items_center()
            .gap(px(12.))
            .border_t_1()
            .border_color(p.border)
            .text_size(px(12.))
            .child(
                div()
                    .flex()
                    .items_baseline()
                    .gap(px(5.))
                    .text_color(p.dim)
                    .child("Database size")
                    .child(
                        div()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(p.text)
                            .child(self.db_bytes.map(format::bytes).unwrap_or_else(|| "…".into())),
                    ),
            )
            .when_some(self.note.clone(), |el, note| el.child(div().text_color(p.down).child(note)))
            .when(busy.is_some(), |el| {
                el.child(div().flex().items_center().gap(px(6.)).text_color(p.dim).child(spinner(p.accent, 12.)).child(
                    match busy {
                        Some(Busy::Deleting) => "Deleting…",
                        _ => "Compacting…",
                    },
                ))
            })
            .when(n > 0, |el| {
                el.child(
                    div()
                        .pl(px(12.))
                        .border_l_1()
                        .border_color(p.border)
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .child(div().child(format!("{n} scan{} selected", if n == 1 { "" } else { "s" })))
                        .child(Button::new("bulk-clear").small().ghost().label("Clear").on_click(cx.listener(|this, _, _, cx| {
                            this.selected.clear();
                            this.confirm = None;
                            cx.notify();
                        })))
                        .child(if bulk_confirm {
                            Button::new("bulk-delete")
                                .small()
                                .danger()
                                .label(if n == 1 { "Confirm delete".to_string() } else { format!("Confirm delete {n} scans") })
                                .disabled(busy.is_some())
                                .on_click(cx.listener(|this, _, _, cx| {
                                    let ids: Vec<i64> = this.selected.iter().copied().collect();
                                    this.delete(ids, cx);
                                }))
                        } else {
                            Button::new("bulk-delete")
                                .small()
                                .icon(Icon::new(IconName::Trash))
                                .label("Delete")
                                .disabled(busy.is_some())
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.confirm = Some(Confirm::Bulk);
                                    cx.notify();
                                }))
                        }),
                )
            })
            .child(div().flex_1())
            .child(
                Button::new("reveal-db")
                    .small()
                    .label(format!("Show database in {}", file_manager()))
                    .on_click(move |_, _, _| diskdiff_engine::platform::reveal(&store_path)),
            )
            .child(
                Button::new("compact-db")
                    .small()
                    .label(if busy == Some(Busy::Compacting) { "Compacting…" } else { "Compact database" })
                    .tooltip("Rewrite the database file to its smallest size")
                    .disabled(busy.is_some())
                    .on_click(cx.listener(|this, _, _, cx| this.compact(cx))),
            )
    }
}

// ---- pieces ---------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum Check {
    Off,
    On,
    Mixed,
}

fn checkbox(p: &Palette, id: SharedString, state: Check, on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> impl IntoElement {
    let on = state != Check::Off;
    let hover_border = p.dim;
    div()
        .id(id)
        .size(px(15.))
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(4.))
        .border_1()
        .cursor_default()
        .when(on, |el| el.bg(p.accent).border_color(p.accent))
        .when(!on, |el| el.bg(p.control).border_color(p.faint).hover(move |s| s.border_color(hover_border)))
        .on_click(move |ev, window, cx| {
            cx.stop_propagation();
            on_click(ev, window, cx)
        })
        .when(state == Check::On, |el| el.child(icon(IconName::Check, 11., p.on_selected)))
        .when(state == Check::Mixed, |el| el.child(icon(IconName::Minus, 11., p.on_selected)))
}

fn tag(fg: Hsla, bg: Hsla, text: &'static str) -> impl IntoElement {
    div().px(px(6.)).rounded(px(7.)).text_size(px(10.5)).font_weight(FontWeight::SEMIBOLD).text_color(fg).bg(bg).child(text)
}

/// "42 s", "3 min", "1 h 5 min".
fn duration(secs: i64) -> String {
    let s = secs.max(0);
    if s < 60 {
        format!("{s} s")
    } else if s < 3600 {
        format!("{} min", s / 60)
    } else {
        format!("{} h {} min", s / 3600, (s % 3600) / 60)
    }
}
