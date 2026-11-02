//! Insights: cleanup suggestions, the largest items, and what changed.
//!
//! It replaces the file list rather than floating above it: acting on
//! anything here means going to that place in the file list. Nothing here
//! deletes or rescans anything. Results are computed on a background thread
//! and cached per scan, compare scan and data version.

use crate::format;
use crate::state::{AppState, InsightsTab, Screen, file_manager, location_name};
use crate::theme::{ActivePalette, Palette};
use crate::widgets::{icon, link, segmented, spinner};
use diskdiff_engine::insights::{self, ChangeSummary, Finding, FindingKind, LargeFolder};
use diskdiff_engine::{ScanInfo, Status};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::scroll::ScrollableElement;
use gpui_kit::component::{Icon, Sizable};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Changes kept for the Changes tab, largest first; Largest keeps as many.
const CHANGES_KEEP: usize = 1000;

/// A background computation's result, keyed by what it was computed from.
struct Slot<K, T> {
    key: Option<K>,
    value: Option<Result<Arc<T>, SharedString>>,
    _task: Option<Task<()>>,
}

impl<K: PartialEq, T> Slot<K, T> {
    fn new() -> Self {
        Slot { key: None, value: None, _task: None }
    }
    fn get(&self, key: &K) -> Option<&Result<Arc<T>, SharedString>> {
        if self.key.as_ref() == Some(key) { self.value.as_ref() } else { None }
    }
}

/// (scan, data version)
type ScanKey = (i64, u64);

struct Largest {
    files: Vec<(PathBuf, u64)>,
    folders: Vec<LargeFolder>,
}

pub struct InsightsView {
    state: Entity<AppState>,
    show_folders: bool,
    largest_scroll: UniformListScrollHandle,
    changes_scroll: UniformListScrollHandle,
    show_hidden: bool,
    suggestions: Slot<ScanKey, Vec<Finding>>,
    largest: Slot<ScanKey, Largest>,
    changes: Slot<(i64, i64, u64), ChangeSummary>,
}

impl InsightsView {
    pub fn new(state: Entity<AppState>, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        cx.observe(&state, |_, _, cx| cx.notify()).detach();
        InsightsView {
            state,
            show_folders: false,
            largest_scroll: UniformListScrollHandle::new(),
            changes_scroll: UniformListScrollHandle::new(),
            show_hidden: false,
            suggestions: Slot::new(),
            largest: Slot::new(),
            changes: Slot::new(),
        }
    }

    /// Starts computing `work` unless the slot already holds (or is
    /// computing) the result for `key`.
    fn ensure<K, T>(
        &mut self,
        cx: &mut Context<Self>,
        slot: fn(&mut Self) -> &mut Slot<K, T>,
        key: K,
        work: impl FnOnce() -> anyhow::Result<T> + Send + 'static,
    ) where
        K: PartialEq + Clone + 'static,
        T: Send + Sync + 'static,
    {
        let s = slot(self);
        if s.key.as_ref() == Some(&key) {
            return;
        }
        s.key = Some(key.clone());
        s.value = None;
        let bg = cx
            .background_executor()
            .spawn(async move { work().map(Arc::new).map_err(|e| SharedString::from(format!("{e:#}"))) });
        let task = cx.spawn(async move |this, cx| {
            let result = bg.await;
            let _ = this.update(cx, |this, cx| {
                let s = slot(this);
                if s.key.as_ref() == Some(&key) {
                    s.value = Some(result);
                    cx.notify();
                }
            });
        });
        slot(self)._task = Some(task);
    }
}

impl AppState {
    fn is_hidden(&self, root: &str, kind: FindingKind, path: &Path) -> bool {
        let path = path.to_string_lossy();
        self.settings.hidden.iter().any(|(r, k, p)| r == root && k == kind.key() && *p == path)
    }

    fn set_suggestion_hidden(&mut self, root: &str, kind: FindingKind, path: &Path, hidden: bool, cx: &mut Context<Self>) {
        let key = (root.to_owned(), kind.key().to_owned(), path.to_string_lossy().into_owned());
        self.settings.hidden.retain(|k| *k != key);
        if hidden {
            self.settings.hidden.push(key);
        }
        self.settings.save();
        cx.notify();
    }
}

impl Render for InsightsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = cx.palette();
        let s = self.state.read(cx);
        let tab = match s.screen {
            Screen::Insights(t) => t,
            Screen::Files => InsightsTab::Suggestions,
        };
        let cur = s.current_scan().cloned();
        let cmp = s.compare_scan().cloned().filter(|_| s.comparing());
        let scans = s.scans.clone();
        let version = s.data_version;
        let store = s.store.clone();

        let body: AnyElement = match &cur {
            None => message(&p, "There is no scan to analyse yet. Run a scan first.").into_any_element(),
            Some(cur) => match tab {
                InsightsTab::Suggestions => {
                    let (store, cur2) = (store.clone(), cur.clone());
                    self.ensure(cx, |t| &mut t.suggestions, (cur.id, version), move || {
                        insights::suggestions(&store, &scans, &cur2)
                    });
                    self.suggestions_tab(&p, cur, cx).into_any_element()
                }
                InsightsTab::Largest => {
                    let (store, cur2) = (store.clone(), cur.clone());
                    self.ensure(cx, |t| &mut t.largest, (cur.id, version), move || {
                        let root = PathBuf::from(&cur2.root);
                        let mut files: Vec<(PathBuf, u64)> =
                            store.largest_files(cur2.id)?.into_iter().map(|f| (root.join(&f.path), f.size)).collect();
                        files.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
                        let folders = insights::largest_folders(&store, &cur2, 1000)?;
                        Ok(Largest { files, folders })
                    });
                    self.largest_tab(&p, cur, cx).into_any_element()
                }
                InsightsTab::Changes => match (&cmp, cur.temporary) {
                    (_, true) => message(
                        &p,
                        "Temporary scans are not part of a history, so there is nothing to compare this one against. Use “Scan and save” to start a history for this folder.",
                    )
                    .into_any_element(),
                    (None, _) => message(
                        &p,
                        "There is no earlier scan of this location to compare against. Pick one on the graph, or run another scan and come back.",
                    )
                    .into_any_element(),
                    (Some(cmp), _) => {
                        let (store, cur2, cmp2) = (store.clone(), cur.clone(), cmp.clone());
                        let min = change_threshold(cur);
                        self.ensure(cx, |t| &mut t.changes, (cur.id, cmp.id, version), move || {
                            insights::changes(&store, &cur2, &cmp2, min, CHANGES_KEEP)
                        });
                        self.changes_tab(&p, cur, cmp, cx).into_any_element()
                    }
                },
            },
        };

        // Largest and Changes are virtual lists that scroll themselves under
        // a fixed toolbar; Suggestions scrolls as one page.
        let fills = cur.is_some() && tab != InsightsTab::Suggestions;
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(self.header(&p, tab, cur.as_ref(), cx))
            .child(if fills {
                div().flex_1().min_h_0().flex().flex_col().px(px(26.)).pt(px(18.)).max_w(px(1180.)).child(body).into_any_element()
            } else {
                div()
                    .id(SharedString::from(format!("insights-scroll-{tab:?}")))
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scrollbar()
                    .child(div().px(px(26.)).pt(px(18.)).pb(px(40.)).max_w(px(1180.)).child(body))
                    .into_any_element()
            })
    }
}

// ---- header ---------------------------------------------------------------

impl InsightsView {
    fn header(&self, p: &Palette, tab: InsightsTab, cur: Option<&ScanInfo>, _cx: &mut Context<Self>) -> impl IntoElement {
        let tabs = [
            (InsightsTab::Suggestions, "Suggestions"),
            (InsightsTab::Largest, "Largest"),
            (InsightsTab::Changes, "Changes"),
        ];
        let context = cur.map(|c| {
            let kind = if c.temporary { "Temporary scan" } else { "Scan" };
            format!("{kind} of {} · {}", location_name(Path::new(&c.root)), format::date(c.started_at))
        });
        div()
            .flex_shrink_0()
            .px(px(26.))
            .py(px(10.))
            .flex()
            .items_center()
            .gap(px(2.))
            .bg(p.elevated)
            .border_b_1()
            .border_color(p.border)
            .children(tabs.into_iter().map(|(t, label)| {
                let on = t == tab;
                let state = self.state.clone();
                let p = *p;
                div()
                    .id(label)
                    .px(px(14.))
                    .py(px(6.))
                    .rounded(px(7.))
                    .text_size(px(12.5))
                    .cursor_default()
                    .when(on, |el| el.bg(p.accent_tint).text_color(p.accent_text).font_weight(FontWeight::SEMIBOLD))
                    .when(!on, |el| {
                        el.text_color(p.dim)
                            .hover(|s| s.bg(p.control_hover).text_color(p.text))
                            .on_click(move |_, _, cx| state.update(cx, |s, cx| s.set_screen(Screen::Insights(t), cx)))
                    })
                    .child(label)
            }))
            .child(div().flex_1())
            .when_some(context, |el, c| el.child(div().mr(px(14.)).text_size(px(12.)).text_color(p.dim).child(c)))
            .child(Button::new("back-to-files").small().icon(Icon::new(IconName::ArrowLeft)).label("Back to files").on_click({
                let state = self.state.clone();
                move |_, _, cx| state.update(cx, |s, cx| s.set_screen(Screen::Files, cx))
            }))
    }
}

// ---- suggestions ----------------------------------------------------------

impl InsightsView {
    fn suggestions_tab(&self, p: &Palette, cur: &ScanInfo, cx: &mut Context<Self>) -> AnyElement {
        let key = (cur.id, self.state.read(cx).data_version);
        let findings = match self.suggestions.get(&key) {
            None => return loading(p, "Comparing folders against earlier scans…").into_any_element(),
            Some(Err(e)) => return error(p, format!("Could not analyse this scan: {e}")).into_any_element(),
            Some(Ok(f)) => f.clone(),
        };
        let s = self.state.read(cx);
        let root = cur.root.clone();
        let (hidden, shown): (Vec<&Finding>, Vec<&Finding>) =
            findings.iter().partition(|f| s.is_hidden(&root, f.kind, &f.path));
        let earlier = s.history().iter().filter(|x| !x.temporary && x.started_at < cur.started_at).count();

        let mut col = div().flex().flex_col().gap(px(14.));
        if shown.is_empty() && hidden.is_empty() {
            return div()
                .flex()
                .flex_col()
                .gap(px(10.))
                .max_w(px(560.))
                .child(
                    div()
                        .text_color(p.dim)
                        .line_height(relative(1.55))
                        .child(format!(
                            "No large caches, duplicated folders, or long-untouched folders turned up in this scan of {}.",
                            cur.root
                        )),
                )
                .when(earlier == 0, |el| el.child(div().text_size(px(12.)).text_color(p.faint).line_height(relative(1.55)).child(
                    "Untouched folders need earlier scans of the same location to compare against, so this screen gets more useful the longer you keep scanning.",
                )))
                .into_any_element();
        }

        let total: u64 = shown.iter().map(|f| f.reclaimable).sum();
        col = col.child(
            div()
                .flex()
                .items_baseline()
                .gap(px(8.))
                .child(div().text_size(px(15.)).font_weight(FontWeight::SEMIBOLD).child(format!(
                    "{} suggestion{}",
                    shown.len(),
                    if shown.len() == 1 { "" } else { "s" }
                )))
                .when(total > 0, |el| {
                    el.child(div().text_color(p.dim).child(format!("· up to {} recoverable", format::bytes(total))))
                }),
        );

        for kind in FindingKind::ALL {
            let group: Vec<&Finding> = shown.iter().copied().filter(|f| f.kind == kind).collect();
            if group.is_empty() {
                continue;
            }
            let (title, blurb) = match kind {
                FindingKind::Regenerable => (
                    "Regenerable",
                    "Caches and dependency folders that whatever created them will rebuild on demand. The safest space to reclaim.",
                ),
                FindingKind::Duplicate => (
                    "Duplicated",
                    "Folders holding the same files, at the same sizes, in more than one place. Compare them in the file list before removing a copy.",
                ),
                FindingKind::Untouched => (
                    "Untouched",
                    "Large folders that have not changed across any of your recent scans — the things it is easy to add once and forget about.",
                ),
            };
            let sum: u64 = group.iter().map(|f| f.reclaimable).sum();
            col = col.child(
                div()
                    .mt(px(10.))
                    .flex()
                    .flex_col()
                    .gap(px(4.))
                    .child(
                        div()
                            .flex()
                            .items_baseline()
                            .gap(px(8.))
                            .child(div().text_size(px(14.)).font_weight(FontWeight::SEMIBOLD).child(title))
                            .child(div().text_size(px(12.)).text_color(p.dim).child(format::bytes(sum))),
                    )
                    .child(div().max_w(px(640.)).text_size(px(12.)).text_color(p.dim).child(blurb)),
            );
            for f in group {
                col = col.child(self.card(p, cur, f, false));
            }
        }

        if !hidden.is_empty() {
            let open = self.show_hidden;
            col = col.child(
                div()
                    .id("toggle-hidden")
                    .mt(px(12.))
                    .flex()
                    .items_center()
                    .gap(px(4.))
                    .px(px(6.))
                    .py(px(3.))
                    .rounded(px(5.))
                    .w(px(120.))
                    .cursor_default()
                    .text_size(px(12.))
                    .text_color(p.dim)
                    .hover(|s| s.bg(p.control_hover).text_color(p.text))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.show_hidden = !this.show_hidden;
                        cx.notify();
                    }))
                    .child(icon(if open { IconName::ChevronDown } else { IconName::ChevronRight }, 13., p.dim))
                    .child(format!("{} hidden", hidden.len())),
            );
            if open {
                for f in hidden {
                    col = col.child(self.card(p, cur, f, true));
                }
            }
        }
        col.into_any_element()
    }

    fn card(&self, p: &Palette, cur: &ScanInfo, f: &Finding, hidden: bool) -> AnyElement {
        let id = format!("{}:{}", f.kind.key(), f.path.display());
        let go = {
            let state = self.state.clone();
            let path = f.path.clone();
            move |_: &mut Window, cx: &mut App| state.update(cx, |s, cx| s.go_to(path.clone(), cx))
        };
        let hide_toggle = {
            let state = self.state.clone();
            let (root, kind, path) = (cur.root.clone(), f.kind, f.path.clone());
            move |_: &gpui_kit::ClickEvent, _: &mut Window, cx: &mut App| {
                state.update(cx, |s, cx| s.set_suggestion_hidden(&root, kind, &path, !hidden, cx))
            }
        };
        let name = f.name();
        let path_text = crate::format::home_path(&f.path);

        if hidden {
            return div()
                .opacity(0.6)
                .px(px(15.))
                .py(px(8.))
                .flex()
                .items_center()
                .gap(px(12.))
                .bg(p.elevated)
                .border_1()
                .border_color(p.border)
                .rounded(px(9.))
                .child(link(SharedString::from(format!("{id}-name")), name, p.text, go).font_weight(FontWeight::SEMIBOLD).flex_shrink_0())
                .child(path_line(p, path_text).flex_1())
                .child(div().text_size(px(12.)).text_color(p.dim).child(format::bytes(f.reclaimable)))
                .child(Button::new(SharedString::from(format!("{id}-unhide"))).small().label("Show again").on_click(hide_toggle))
                .into_any_element();
        }

        let pill = match f.kind {
            FindingKind::Regenerable => f.category.to_string(),
            FindingKind::Duplicate => format!("{} copies", f.copies.len()),
            FindingKind::Untouched => match f.unchanged_since {
                Some(t) => format!("Unchanged for {}", format::span(cur.finished_at - t)),
                None => "Untouched".into(),
            },
        };
        let detail = if f.reclaimable == f.size {
            format!("{} file{}", format::count(f.files), if f.files == 1 { "" } else { "s" })
        } else {
            format!("of {}", format::bytes(f.size))
        };
        let reveal_path = f.path.clone();

        div()
            .px(px(15.))
            .py(px(13.))
            .flex()
            .gap(px(18.))
            .bg(p.elevated)
            .border_1()
            .border_color(p.border)
            .rounded(px(9.))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(5.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .min_w_0()
                            .child(icon(IconName::Folder, 14., p.folder))
                            .child(
                                link(SharedString::from(format!("{id}-name")), name, p.text, go.clone())
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .min_w_0()
                                    .tooltip({
                                        let t: SharedString = format!("Go to {path_text}").into();
                                        move |window, cx| Tooltip::new(t.clone()).build(window, cx)
                                    }),
                            )
                            .child(pill_el(p, pill)),
                    )
                    .when(f.kind != FindingKind::Duplicate, |el| el.child(path_line(p, path_text.clone())))
                    .child(
                        div()
                            .max_w(px(620.))
                            .text_size(px(12.))
                            .line_height(relative(1.45))
                            .text_color(p.text)
                            .child(explanation(f, cur)),
                    )
                    .when(f.kind == FindingKind::Duplicate, |el| {
                        el.child(div().flex().flex_col().gap(px(2.)).children(f.copies.iter().enumerate().map(|(i, c)| {
                            let state = self.state.clone();
                            let c2 = c.clone();
                            div()
                                .flex()
                                .items_center()
                                .gap(px(6.))
                                .min_w_0()
                                .child(icon(IconName::Copy, 12., p.faint))
                                .child(
                                    link(
                                        SharedString::from(format!("{id}-copy-{i}")),
                                        crate::format::home_path(c),
                                        p.accent_text,
                                        move |_, cx| state.update(cx, |s, cx| s.go_to(c2.clone(), cx)),
                                    )
                                    .text_size(px(11.5))
                                    .text_ellipsis_start()
                                    .min_w_0(),
                                )
                        })))
                    }),
            )
            .child(
                div()
                    .flex_shrink_0()
                    .flex()
                    .flex_col()
                    .items_end()
                    .gap(px(4.))
                    .child(div().text_size(px(15.)).font_weight(FontWeight::SEMIBOLD).child(format::bytes(f.reclaimable)))
                    .child(div().text_size(px(11.5)).text_color(p.dim).child(detail))
                    .child(
                        div()
                            .mt(px(6.))
                            .flex()
                            .gap(px(6.))
                            .child(Button::new(SharedString::from(format!("{id}-go"))).small().label("Go to folder").on_click(move |_, w, cx| go(w, cx)))
                            .child(
                                Button::new(SharedString::from(format!("{id}-reveal")))
                                    .small()
                                    .icon(Icon::new(IconName::ExternalLink))
                                    .tooltip(format!("Show in {}", file_manager()))
                                    .on_click(move |_, _, _| diskdiff_engine::platform::reveal(&reveal_path)),
                            )
                            .child(
                                Button::new(SharedString::from(format!("{id}-hide")))
                                    .small()
                                    .ghost()
                                    .label("Hide")
                                    .tooltip("Stop suggesting this folder")
                                    .on_click(hide_toggle),
                            ),
                    ),
            )
            .into_any_element()
    }
}

fn untouched_sentence(f: &Finding, cur: &ScanInfo) -> Option<String> {
    let since = f.unchanged_since?;
    let span = format::span(cur.finished_at - since);
    Some(if f.unchanged_scans == 1 && f.since_first_scan {
        format!("Unchanged since your earlier scan {span} ago.")
    } else if f.since_first_scan {
        format!("Unchanged in all {} of your earlier scans, going back {span}.", f.unchanged_scans)
    } else {
        format!(
            "Unchanged for {span}, across {} scan{}.",
            f.unchanged_scans,
            if f.unchanged_scans == 1 { "" } else { "s" }
        )
    })
}

fn explanation(f: &Finding, cur: &ScanInfo) -> String {
    match f.kind {
        FindingKind::Regenerable => match untouched_sentence(f, cur) {
            Some(u) => format!("{} {u}", f.hint),
            None => f.hint.to_string(),
        },
        FindingKind::Duplicate => format!(
            "{} folders hold the same files at the same sizes. Keeping one and removing the rest frees {}.",
            f.copies.len(),
            format::bytes(f.reclaimable)
        ),
        FindingKind::Untouched if f.untouched_children > 0 => {
            let mut s = format!(
                "{} of its subfolders have not changed at all across your recent scans",
                f.untouched_children
            );
            if !f.examples.is_empty() {
                s.push_str(" — ");
                s.push_str(&f.examples.join(", "));
                if f.untouched_children > f.examples.len() {
                    s.push_str(", and others");
                }
            }
            s.push('.');
            s
        }
        FindingKind::Untouched => match untouched_sentence(f, cur) {
            Some(u) => format!("{u} Nothing inside it has been added, removed, or resized."),
            None => "Nothing inside this folder has changed since your earlier scans.".into(),
        },
    }
}

// ---- largest --------------------------------------------------------------

impl InsightsView {
    fn largest_tab(&self, p: &Palette, cur: &ScanInfo, cx: &mut Context<Self>) -> AnyElement {
        let key = (cur.id, self.state.read(cx).data_version);
        let folders = self.show_folders;
        let entity = cx.entity();
        let toolbar = div()
            .flex()
            .items_center()
            .gap(px(12.))
            .mb(px(12.))
            .child(segmented("largest-kind", &["Files", "Folders"], usize::from(folders), &[true, true], move |ix, cx| {
                entity.update(cx, |this, cx| {
                    this.show_folders = ix == 1;
                    this.largest_scroll.scroll_to_item(0, ScrollStrategy::Top);
                    cx.notify();
                })
            }))
            .child(div().text_size(px(11.5)).text_color(p.dim).child(if folders {
                "Every folder in the scan. A folder that just wraps one big subfolder is skipped in favour of the subfolder."
            } else {
                "Every file in the scan, largest first."
            }));

        let data = match self.largest.get(&key) {
            None => return div().child(toolbar).child(loading(p, "Reading the scan…")).into_any_element(),
            Some(Err(e)) => return div().child(toolbar).child(error(p, format!("Could not list this scan: {e}"))).into_any_element(),
            Some(Ok(d)) => d.clone(),
        };
        let total = if folders { data.folders.len() } else { data.files.len() };
        if total == 0 {
            return div()
                .child(toolbar)
                .child(message(p, if folders { "This scan recorded no folders." } else { "This scan recorded no files." }))
                .into_any_element();
        }
        let row = move |d: &Largest, i: usize| if folders { (d.folders[i].path.clone(), d.folders[i].size, true) } else { (d.files[i].0.clone(), d.files[i].1, false) };
        let max = row(&data, 0).1.max(1);
        let (state, p2) = (self.state.clone(), *p);
        let list = uniform_list(if folders { "largest-folders" } else { "largest-files" }, total, move |range, _, _| {
            let p = &p2;
            range
                .map(|i| {
                    let (path, size, is_dir) = row(&data, i);
                    let frac = (size as f32 / max as f32).max(0.01);
                    let bar = div()
                        .w(px(96.))
                        .h(px(5.))
                        .flex_shrink_0()
                        .rounded(px(3.))
                        .bg(p.border)
                        .child(div().h_full().rounded(px(3.)).w(relative(frac)).bg(p.accent.opacity(0.75)));
                    row_base(p, i)
                        .child(div().w(px(32.)).flex_shrink_0().flex().justify_end().text_size(px(11.)).text_color(p.dim).child(format!("{}", i + 1)))
                        .child(icon(if is_dir { IconName::Folder } else { IconName::File }, 14., if is_dir { p.folder } else { p.file }))
                        .child(path_cell(&state, p, format!("largest-{i}"), &path, path.clone()))
                        .child(bar)
                        .child(div().min_w(px(82.)).flex_shrink_0().flex().justify_end().font_weight(FontWeight::SEMIBOLD).child(format::bytes(size)))
                })
                .collect()
        });
        div().flex_1().min_h_0().flex().flex_col().child(toolbar).child(list_area(list, &self.largest_scroll)).into_any_element()
    }
}

/// "from Downloads": where a moved item came from, as a link to that place.
fn moved_from(state: &Entity<AppState>, p: &Palette, i: usize, from: &Path, to: &Path) -> impl IntoElement {
    let label = diskdiff_engine::moves::label(from, to);
    let tip: SharedString = format!("Moved here from {}", crate::format::home_path(from)).into();
    let (state, target) = (state.clone(), from.to_owned());
    div().flex().gap(px(3.)).child("from").child(
        link(SharedString::from(format!("moved-{i}")), label, p.accent_text, move |_, cx| state.update(cx, |s, cx| s.go_to(target.clone(), cx)))
            .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx)),
    )
}

/// Name (a link to `target`) with the parent folder beside it.
fn path_cell(state: &Entity<AppState>, p: &Palette, id: String, path: &Path, target: PathBuf) -> impl IntoElement {
    let name = path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned());
    let parent = path.parent().map(crate::format::home_path).unwrap_or_default();
    let state = state.clone();
    let tip: SharedString = format!("Go to {}", crate::format::home_path(path)).into();
    div()
        .flex_1()
        .min_w_0()
        .flex()
        .items_baseline()
        .gap(px(8.))
        .child(
            link(SharedString::from(id), name, p.text, move |_, cx| state.update(cx, |s, cx| s.go_to(target.clone(), cx)))
                .flex_shrink_0()
                .max_w(relative(0.6))
                .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx)),
        )
        .child(path_line(p, parent).flex_1())
}

/// A virtual list filling the rest of the tab, with its scrollbar.
fn list_area(list: UniformList, scroll: &UniformListScrollHandle) -> impl IntoElement {
    div().relative().flex_1().min_h_0().child(list.track_scroll(scroll).size_full().pr(px(14.)).pb(px(40.))).vertical_scrollbar(scroll)
}
// ---- changes --------------------------------------------------------------

/// 50 MB, or less for small scans so a single folder's history still shows
/// something: about 0.5 % of the scan, at least 1 MB, rounded down to a
/// 1-2-5 step so the label reads well.
fn change_threshold(cur: &ScanInfo) -> u64 {
    let raw = (cur.size / 200).clamp(1_000_000, 50_000_000);
    let mut step = 1_000_000u64;
    let mut best = step;
    while step <= raw {
        for m in [1, 2, 5] {
            if step * m <= raw {
                best = step * m;
            }
        }
        step *= 10;
    }
    best
}

impl InsightsView {
    fn changes_tab(&self, p: &Palette, cur: &ScanInfo, cmp: &ScanInfo, cx: &mut Context<Self>) -> AnyElement {
        let key = (cur.id, cmp.id, self.state.read(cx).data_version);
        let data = match self.changes.get(&key) {
            None => return loading(p, "Comparing the two scans…").into_any_element(),
            Some(Err(e)) => return error(p, format!("Could not compare these scans: {e}")).into_any_element(),
            Some(Ok(d)) => d.clone(),
        };
        let total = data.total_delta;
        let summary = div()
            .flex()
            .items_baseline()
            .gap(px(20.))
            .pb(px(14.))
            .mb(px(10.))
            .border_b_1()
            .border_color(p.border)
            .child(
                div()
                    .flex()
                    .items_baseline()
                    .gap(px(8.))
                    .child(
                        div()
                            .text_size(px(22.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(if total >= 0 { p.up } else { p.down })
                            .child(format::delta(total)),
                    )
                    .child(div().text_color(p.dim).child(format!("since {}", format::date(cmp.started_at)))),
            )
            .when(data.added > 0, |el| {
                el.child(div().text_size(px(12.)).text_color(p.up).child(format!("+{} appeared", format::bytes(data.added))))
            })
            .when(data.removed > 0, |el| {
                el.child(div().text_size(px(12.)).text_color(p.down).child(format!("−{} disappeared", format::bytes(data.removed))))
            })
            .when(data.moved > 0, |el| el.child(div().text_size(px(12.)).text_color(p.dim).child(format!("{} moved", format::bytes(data.moved)))))
            .child(div().flex_1())
            .child(div().text_size(px(11.5)).text_color(p.faint).child(format!("Changes of {} or more", format::bytes(data.min_size))));

        if data.changes.is_empty() {
            return div()
                .child(summary)
                .child(message(p, format!("Nothing changed by more than {} between these two scans.", format::bytes(data.min_size))))
                .into_any_element();
        }
        let (state, p2) = (self.state.clone(), *p);
        let count = data.changes.len();
        let list = uniform_list("changes", count, move |range, _, _| {
            let p = &p2;
            range
                .map(|i| {
                let c = &data.changes[i];
                let delta = c.change();
                let (label, color, alpha) = match c.status {
                    _ if c.moved_from.is_some() => ("Moved", p.accent, 0.16),
                    Status::Added => ("New", p.new, 0.16),
                    Status::Removed => ("Deleted", p.down, 0.16),
                    _ if delta >= 0 => ("Grew", p.up, 0.14),
                    _ => ("Shrank", p.down, 0.14),
                };
                // A deleted item no longer exists: show where it was.
                let target = if c.status == Status::Removed {
                    c.path.parent().map(Path::to_path_buf).unwrap_or_else(|| c.path.clone())
                } else {
                    c.path.clone()
                };
                row_base(p, i)
                    .child(
                        div()
                            .w(px(58.))
                            .flex_shrink_0()
                            .flex()
                            .justify_center()
                            .py(px(1.))
                            .rounded(px(9.))
                            .text_size(px(10.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(if c.moved_from.is_some() { p.accent_text } else { color })
                            .bg(color.opacity(alpha))
                            .child(label),
                    )
                    .child(icon(if c.is_dir { IconName::Folder } else { IconName::File }, 14., if c.is_dir { p.folder } else { p.file }))
                    .child(path_cell(&state, p, format!("change-{i}"), &c.path, target))
                    .child(
                        div()
                            .w(px(168.))
                            .flex_shrink_0()
                            .flex()
                            .justify_end()
                            .text_size(px(11.))
                            .text_color(p.dim)
                            .when(c.status == Status::Changed, |el| {
                                el.child(format!("{} → {}", format::bytes(c.prev), format::bytes(c.size)))
                            })
                            .when_some(c.moved_from.clone(), |el, from| el.child(moved_from(&state, p, i, &from, &c.path))),
                    )
                    .child(
                        div()
                            .min_w(px(82.))
                            .flex_shrink_0()
                            .flex()
                            .justify_end()
                            .gap(px(6.))
                            // Moved bytes in grey; the bold number is only what was added or freed.
                            .when(c.moved_from.is_some(), |el| el.text_color(p.dim).child(format::bytes(c.size)))
                            .when(c.moved_from.is_none(), |el| {
                                el.when(c.moved_bytes != 0, |el| {
                                    el.child(div().text_size(px(11.)).text_color(p.dim).child(format::delta(c.moved_bytes)))
                                })
                                .child(
                                    div()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .text_color(if delta >= 0 { p.up } else { p.down })
                                        .child(format::delta(delta)),
                                )
                            }),
                    )
                })
                .collect()
        });
        div().flex_1().min_h_0().flex().flex_col().child(summary).child(list_area(list, &self.changes_scroll)).into_any_element()
    }
}

// ---- shared pieces --------------------------------------------------------

fn row_base(p: &Palette, i: usize) -> Div {
    let hover = p.row_hover;
    div()
        .w_full()
        .h(px(30.))
        .px(px(8.))
        .flex()
        .items_center()
        .gap(px(10.))
        .rounded(px(6.))
        .when(i % 2 == 1, |el| el.bg(p.text.opacity(0.025)))
        .hover(move |s| s.bg(hover))
}

fn path_line(p: &Palette, text: String) -> Div {
    div()
        .min_w_0()
        .overflow_hidden()
        .whitespace_nowrap()
        .text_ellipsis_start()
        .text_size(px(11.5))
        .text_color(p.dim)
        .child(text)
}

fn pill_el(p: &Palette, text: String) -> impl IntoElement {
    div()
        .flex_shrink_0()
        .px(px(7.))
        .py(px(1.))
        .rounded(px(8.))
        .text_size(px(10.5))
        .bg(p.accent_tint)
        .text_color(p.accent_text)
        .child(text)
}

fn loading(p: &Palette, text: &'static str) -> impl IntoElement {
    div().py(px(40.)).flex().items_center().gap(px(10.)).text_color(p.dim).child(spinner(p.accent, 16.)).child(text)
}

fn message(p: &Palette, text: impl Into<SharedString>) -> impl IntoElement {
    div().py(px(8.)).max_w(px(560.)).line_height(relative(1.55)).text_color(p.dim).child(text.into())
}

fn error(p: &Palette, text: String) -> impl IntoElement {
    div().px(px(12.)).py(px(8.)).rounded(px(6.)).bg(p.up.opacity(0.14)).text_color(p.up).text_size(px(12.)).child(text)
}
