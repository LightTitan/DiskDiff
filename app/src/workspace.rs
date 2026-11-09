//! The window's root view: title bar, graph, summary, toolbar, browser, and
//! the overlays (manage scans, toast).

use crate::browser::Browser;
use crate::format;
use crate::graph::GraphView;
use crate::insights::InsightsView;
use crate::manage::ManageView;
use crate::state::{AppState, Screen, SortKey, location_detail, location_name};
use crate::summary::SummaryView;
use crate::theme::{ActivePalette, Palette};
use crate::widgets::{icon, icon_button, segmented};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::menu::{DropdownMenu, PopupMenuItem};
use gpui_kit::component::{Disableable, Icon, Sizable, TitleBar};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::path::PathBuf;
use std::sync::atomic::Ordering::Relaxed;
use std::time::Duration;

pub struct Workspace {
    state: Entity<AppState>,
    graph: Entity<GraphView>,
    summary: Entity<SummaryView>,
    browser: Entity<Browser>,
    sidebar: Entity<crate::sidebar::Sidebar>,
    insights: Entity<InsightsView>,
    manage: Entity<ManageView>,
    toast_shown: Option<u64>,
}

impl Workspace {
    pub fn new(state: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        cx.observe(&state, |this, state, cx| {
            // Toasts dismiss themselves after 8 s, unless their path was
            // opened to read it in full.
            let id = state.read(cx).toast.as_ref().map(|t| t.id);
            if id.is_some() && id != this.toast_shown {
                this.toast_shown = id;
                cx.spawn(async move |_, cx| {
                    cx.background_executor().timer(Duration::from_secs(8)).await;
                    let _ = state.update(cx, |s, cx| {
                        if s.toast.as_ref().is_some_and(|t| Some(t.id) == id && !t.expanded) {
                            s.dismiss_toast(cx);
                        }
                    });
                })
                .detach();
            }
            cx.notify();
        })
        .detach();
        cx.observe_window_activation(window, |this: &mut Self, window, cx| {
            if window.is_window_active() {
                this.state.update(cx, |s, cx| s.check_access(cx));
            }
        })
        .detach();
        Workspace {
            graph: cx.new(|cx| GraphView::new(state.clone(), cx)),
            summary: cx.new(|cx| SummaryView::new(state.clone(), cx)),
            browser: cx.new(|cx| Browser::new(state.clone(), window, cx)),
            sidebar: cx.new(|cx| crate::sidebar::Sidebar::new(state.clone(), cx)),
            insights: cx.new(|cx| InsightsView::new(state.clone(), window, cx)),
            manage: cx.new(|cx| ManageView::new(state.clone(), window, cx)),
            state,
            toast_shown: None,
        }
    }

    fn pick_folder(&self, temporary: bool, cx: &mut Context<Self>) {
        pick_folder(self.state.clone(), temporary, cx);
    }

    fn title_bar(&self, p: &Palette, cx: &mut Context<Self>) -> impl IntoElement {
        // Windows and Linux have no system menu bar: draw the menus here.
        let menu_bar = (!cfg!(target_os = "macos")).then(|| crate::menus::MenuBar::get(cx));
        TitleBar::new().bg(p.elevated).border_color(p.border).children(menu_bar).child(
            div()
                .flex_1()
                .flex()
                .justify_center()
                .text_size(px(13.))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(p.text)
                .child("DiskDiff"),
        )
    }

    fn access_prompt(&self, p: &Palette, cx: &mut Context<Self>) -> Option<impl IntoElement + use<>> {
        let s = self.state.read(cx);
        if !cfg!(target_os = "macos") || s.full_access != Some(false) || s.access_prompt_dismissed {
            return None;
        }
        let state = self.state.clone();
        let state2 = self.state.clone();
        let state3 = self.state.clone();
        Some(
            div()
                .mx(px(14.))
                .mt(px(10.))
                .px(px(16.))
                .py(px(12.))
                .rounded(px(8.))
                .border_1()
                .border_color(p.new.opacity(0.35))
                .bg(p.new.opacity(0.07))
                .flex()
                .items_center()
                .gap(px(14.))
                .child(icon(IconName::ShieldAlert, 22., p.new))
                .child(
                    div()
                        .flex_1()
                        .flex()
                        .flex_col()
                        .gap(px(3.))
                        .child(div().font_weight(FontWeight::SEMIBOLD).child("Grant Full Disk Access"))
                        .child(div().text_size(px(12.)).text_color(p.dim).child(
                            "macOS is hiding protected folders from DiskDiff, so totals read low. Turn on DiskDiff in Privacy & Security → Full Disk Access, then relaunch.",
                        )),
                )
                .child(
                    Button::new("fda-open").primary().small().label("Open System Settings").on_click(move |_, _, cx| {
                        cx.open_url("x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles");
                        let _ = &state;
                    }),
                )
                .child(Button::new("fda-recheck").small().label("Re-check").on_click(move |_, _, cx| {
                    state2.update(cx, |s, cx| s.check_access(cx));
                }))
                .child(icon_button("fda-close", IconName::X, "Dismiss", move |_, cx| {
                    state3.update(cx, |s, cx| {
                        s.access_prompt_dismissed = true;
                        cx.notify();
                    });
                })),
        )
    }

    fn toolbar(&self, p: &Palette, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let (sort, descending, comparing) = {
            let s = self.state.read(cx);
            (s.sort, s.descending, s.comparing())
        };
        let location_el = self.location_menu(p, cx);
        let sort_el = self.sort_control(p, sort, descending, comparing, cx);
        let more_el = self.more_menu(cx.entity(), cx);
        let s = self.state.read(cx);
        let scanning = s.job.is_some();
        let cur_temp = s.current_scan().is_some_and(|c| c.temporary);
        let location = s.location.clone();
        let primary_label = if scanning {
            "Scanning…"
        } else if cur_temp {
            "Scan and save"
        } else if location.is_some() {
            "Scan now"
        } else {
            "Add location…"
        };
        let state = self.state.clone();
        let loc_for_scan = location.clone();
        let primary = Button::new("scan")
            .primary()
            .icon(Icon::new(IconName::RefreshCw))
            .label(primary_label)
            .disabled(scanning)
            .on_click(cx.listener(move |this, _, _, cx| match &loc_for_scan {
                Some(loc) => {
                    let loc = loc.clone();
                    this.state.update(cx, |s, cx| s.start_scan(loc, false, cx));
                }
                None => this.pick_folder(false, cx),
            }));

        let free_ok = {
            let h = s.history();
            !h.is_empty() && h.iter().all(|x| x.disk_total > 0)
        };
        let graph_free = s.settings.graph_free && free_ok;

        div()
            .h(px(46.))
            .flex_shrink_0()
            .px(px(14.))
            .flex()
            .items_center()
            .gap(px(8.))
            .bg(p.elevated)
            .border_b_1()
            .border_color(p.border)
            .child(primary)
            .child(location_el)
            .when_some(s.job.as_ref(), |el, job| el.child(scan_pill(job, p, state.clone())))
            .child(div().flex_1().min_w(px(8.)))
            .child(
                div().flex().items_center().gap(px(6.)).child(div().text_size(px(11.)).text_color(p.dim).child("Graph")).child(
                    segmented(
                        "metric",
                        &["Used", "Available"],
                        usize::from(graph_free),
                        &[true, free_ok],
                        {
                            let state = self.state.clone();
                            move |ix, cx| state.update(cx, |s, cx| s.set_graph_free(ix == 1, cx))
                        },
                    ),
                ),
            )
            .child(sort_el)
            .child(
                Button::new("insights")
                    .icon(Icon::new(IconName::Sparkles))
                    .label("Insights")
                    .small()
                    .disabled(s.current_scan().is_none())
                    .tooltip("Suggestions, the largest items, and what changed")
                    .on_click({
                        let state = self.state.clone();
                        move |_, _, cx| state.update(cx, |s, cx| s.set_screen(Screen::Insights(crate::state::InsightsTab::Suggestions), cx))
                    }),
            )
            .child(more_el)
    }

    fn sort_control(&self, p: &Palette, sort: SortKey, descending: bool, comparing: bool, _cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let state = self.state.clone();
        let state2 = self.state.clone();
        div()
            .flex()
            .items_center()
            .gap(px(6.))
            .child(div().text_size(px(11.)).text_color(p.dim).child("Sort"))
            .child(
                Button::new("sort-key").small().label(sort.label()).dropdown_menu(move |menu, _, _| {
                    let mut menu = menu;
                    for key in SortKey::ALL {
                        let state = state.clone();
                        let needs_compare = matches!(key, SortKey::Delta | SortKey::DeltaSigned);
                        menu = menu.item(
                            PopupMenuItem::new(key.label())
                                .checked(key == sort)
                                .disabled(needs_compare && !comparing)
                                .on_click(move |_, _, cx| state.update(cx, |s, cx| s.set_sort(key, cx))),
                        );
                    }
                    menu
                }),
            )
            .child(icon_button(
                "sort-order",
                if descending { IconName::ArrowDownWideNarrow } else { IconName::ArrowUpNarrowWide },
                if descending { "Descending — click for ascending" } else { "Ascending — click for descending" },
                move |_, cx| state2.update(cx, |s, cx| s.toggle_order(cx)),
            ))
    }

    fn location_menu(&self, p: &Palette, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let s = self.state.read(cx);
        let label = s.location.as_deref().map(location_name).unwrap_or_else(|| "Add location…".into());
        let tooltip: SharedString = s.location.as_ref().map(|l| l.display().to_string()).unwrap_or("Choose a scan location".into()).into();
        let locations = s.settings.locations.clone();
        let current_loc = s.location.clone();
        let temp = s.scans.iter().rev().find(|x| x.temporary).map(|x| (x.id, PathBuf::from(&x.root)));
        let cur_temp = s.current_scan().is_some_and(|c| c.temporary);
        let state = self.state.clone();
        let entity = cx.entity();
        let _ = p;
        Button::new("location")
            .small()
            .icon(Icon::new(IconName::HardDrive))
            .label(label)
            .tooltip(tooltip)
            .disabled(s.job.is_some())
            .dropdown_menu(move |menu, _, _| {
                let mut menu = menu.min_w(px(240.)).max_w(px(360.));
                if let Some((id, root)) = &temp {
                    let state = state.clone();
                    let (id, root) = (*id, root.clone());
                    menu = menu
                        .item(
                            PopupMenuItem::new(format!("Temporary: {}", location_name(&root)))
                                .checked(cur_temp)
                                .on_click(move |_, _, cx| {
                                    state.update(cx, |s, cx| {
                                        s.location = Some(root.clone());
                                        s.set_current(id, cx);
                                        cx.notify();
                                    })
                                }),
                        )
                        .separator();
                }
                for loc in &locations {
                    let state = state.clone();
                    let l = loc.clone();
                    let detail = location_detail(loc);
                    let name = location_name(loc);
                    let text = if detail.is_empty() { name } else { format!("{name}  —  {detail}") };
                    menu = menu.item(
                        PopupMenuItem::new(text)
                            .checked(current_loc.as_ref() == Some(loc) && !cur_temp)
                            .on_click(move |_, _, cx| state.update(cx, |s, cx| s.set_location(l.clone(), cx))),
                    );
                }
                let entity = entity.clone();
                let state2 = state.clone();
                let forget = current_loc.clone().filter(|_| !cur_temp && locations.len() > 1);
                menu = menu.separator().item(
                    PopupMenuItem::new("Add location…").on_click(move |_, _, cx| entity.update(cx, |this, cx| this.pick_folder(false, cx))),
                );
                if let Some(loc) = forget {
                    menu = menu.item(
                        PopupMenuItem::new(format!("Forget “{}”", location_name(&loc)))
                            .on_click(move |_, _, cx| state2.update(cx, |s, cx| s.forget_location(&loc, cx))),
                    );
                }
                menu
            })
    }

    fn more_menu(&self, entity: Entity<Self>, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let s = self.state.read(cx);
        let scanning = s.job.is_some();
        let deep = s.settings.deep_activity;
        let state = self.state.clone();
        Button::new("more")
            .ghost()
            .small()
            .icon(Icon::new(IconName::Ellipsis))
            .tooltip("More")
            .dropdown_menu_with_anchor(Anchor::TopRight, move |menu, _, _| {
                let e1 = entity.clone();
                let s1 = state.clone();
                let s2 = state.clone();
                menu.item(
                    PopupMenuItem::new("Temporary scan…")
                        .icon(Icon::new(IconName::FolderSearch))
                        .disabled(scanning)
                        .on_click(move |_, _, cx| e1.update(cx, |this, cx| this.pick_folder(true, cx))),
                )
                .item(
                    PopupMenuItem::new("Manage scans…").icon(Icon::new(IconName::Database)).on_click(move |_, _, cx| {
                        s1.update(cx, |s, cx| {
                            s.show_manage = true;
                            cx.notify();
                        })
                    }),
                )
                .separator()
                .item(
                    PopupMenuItem::new("Deep folder activity")
                        .checked(deep)
                        .on_click(move |_, _, cx| s2.update(cx, |s, cx| s.set_deep_activity(!deep, cx))),
                )
            })
    }

    fn toast(&self, p: &Palette, cx: &mut Context<Self>) -> Option<impl IntoElement + use<>> {
        let t = self.state.read(cx).toast.clone()?;
        let state = self.state.clone();
        Some(
            div().absolute().bottom(px(18.)).left_0().right_0().flex().justify_center().child(
                div()
                    .w(px(460.))
                    .bg(p.elevated)
                    .border_1()
                    .border_color(p.border)
                    .rounded(px(10.))
                    .shadow_lg()
                    .pl(px(15.))
                    .pr(px(10.))
                    .py(px(12.))
                    .flex()
                    .gap(px(10.))
                    .child(
                        // min_w_0 + overflow_hidden: a long word (a path inside
                        // an error message) is clipped at the edge instead of
                        // pushing the close button out of the toast.
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .flex()
                            .flex_col()
                            .gap(px(5.))
                            .child(div().font_weight(FontWeight::SEMIBOLD).child(t.title))
                            .when_some(t.detail, |el, d| el.child(div().text_size(px(12.)).text_color(p.dim).child(d)))
                            .when_some(t.path, |el, path| {
                                let state = self.state.clone();
                                let line = div()
                                    .id("toast-path")
                                    .min_w_0()
                                    .text_size(px(12.))
                                    .text_color(p.dim)
                                    .cursor_default()
                                    .hover(|s| s.text_color(p.text))
                                    // Only while shortened: a tooltip left over after the click
                                    // would cover the path it just opened.
                                    .when(!t.expanded, |el| {
                                        el.tooltip(|window, cx| {
                                            gpui_kit::component::tooltip::Tooltip::new("Show the full path").build(window, cx)
                                        })
                                    })
                                    .on_click(move |_, _, cx| state.update(cx, |s, cx| s.toggle_toast_path(cx)));
                                // Shortened from the start, so the folder itself stays visible.
                                let line = if t.expanded {
                                    line
                                } else {
                                    line.whitespace_nowrap().overflow_hidden().text_ellipsis_start()
                                };
                                el.child(line.child(path))
                            }),
                    )
                    .child(icon_button("toast-close", IconName::X, "Dismiss", move |_, cx| {
                        state.update(cx, |s, cx| s.dismiss_toast(cx))
                    })),
            ),
        )
    }
}

/// Asks for a folder, then adds it as a location or scans it temporarily.
pub fn pick_folder(state: Entity<AppState>, temporary: bool, cx: &mut App) {
    let rx = cx.prompt_for_paths(PathPromptOptions {
        files: false,
        directories: true,
        multiple: false,
        prompt: Some(if temporary { "Scan".into() } else { "Add".into() }),
    });
    cx.spawn(async move |cx| {
        let Ok(Ok(Some(paths))) = rx.await else { return };
        let Some(path) = paths.into_iter().next() else { return };
        let _ = state.update(cx, |s, cx| {
            if temporary {
                s.start_scan(path, true, cx);
            } else {
                s.set_location(path, cx);
            }
        });
    })
    .detach();
}

fn scan_pill(job: &crate::state::ScanJob, p: &Palette, state: Entity<AppState>) -> impl IntoElement {
    let pr = &job.progress;
    let files = pr.files.load(Relaxed);
    let dirs = pr.dirs.load(Relaxed);
    let bytes = pr.bytes.load(Relaxed);
    let current = pr.current.lock().clone();
    let saving = pr.saving.load(Relaxed);
    let cancelling = pr.cancelled();
    // Tabular figures keep the digits equal-width, so counts don't jitter as they tick up.
    let cell = |text: String, w: f32| {
        div()
            .w(px(w))
            .flex()
            .justify_end()
            .font_features(FontFeatures(std::sync::Arc::new(vec![("tnum".into(), 1)])))
            .child(text)
    };
    div()
        .flex_1()
        .min_w(px(260.))
        .max_w(px(640.))
        .h(px(30.))
        .px(px(10.))
        .flex()
        .items_center()
        .gap(px(10.))
        .rounded(px(8.))
        .border_1()
        .border_color(p.border)
        .bg(p.control)
        .text_size(px(12.))
        .text_color(p.dim)
        .child(crate::widgets::spinner(p.accent, 14.))
        .child(
            div()
                .max_w(px(140.))
                .overflow_hidden()
                .text_ellipsis()
                .whitespace_nowrap()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(p.text)
                .child(location_name(&job.root)),
        )
        .child(cell(format!("{} folders", format::count(dirs)), 104.))
        .child(cell(format!("{} files", format::count(files)), 104.))
        .child(cell(format::bytes(bytes), 70.))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .whitespace_nowrap()
                .text_size(px(11.))
                .child(crate::widgets::truncate_start(
                    if cancelling {
                        "Cancelling…".into()
                    } else if saving {
                        "Saving…".into()
                    } else {
                        current
                    },
                    60,
                )),
        )
        .child(icon_button("cancel-scan", IconName::CircleX, "Cancel scan", move |_, cx| {
            state.update(cx, |s, cx| s.cancel_scan(cx))
        }))
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = cx.palette();
        let s = self.state.read(cx);
        let screen = s.screen;
        let show_manage = s.show_manage;
        let banner = s.banner.clone();
        let has_scan = s.current_scan().is_some();
        // A first scan shows live in the browser before anything is saved.
        let show_files = has_scan || s.live_job().is_some();
        let show_graph = {
            let h = s.history();
            !h.is_empty() && !h.iter().all(|x| x.temporary)
        };
        let has_location = s.location.is_some();
        let show_sidebar = s.settings.show_sidebar;
        let _ = window;

        let main: AnyElement = match screen {
            Screen::Insights(_) => self.insights.clone().into_any_element(),
            Screen::Files => div()
                .flex_1()
                .min_h_0()
                .flex()
                .flex_col()
                .when(show_graph, |el| {
                    el.child(div().flex_shrink_0().bg(p.elevated).border_b_1().border_color(p.border).child(self.graph.clone()))
                })
                .when(has_scan, |el| el.child(self.summary.clone()))
                .child(self.toolbar(&p, cx))
                .when_some(banner, |el, b| {
                    el.child(div().px(px(14.)).py(px(6.)).text_size(px(12.)).bg(p.up.opacity(0.14)).text_color(p.up).child(b))
                })
                .child(if show_files {
                    div()
                        .id("files-row")
                        .flex_1()
                        .min_h_0()
                        .flex()
                        // The sidebar's right edge follows the pointer while
                        // its grip is dragged; the width is saved on release.
                        .on_drag_move::<crate::sidebar::ResizeSidebar>(cx.listener(|this, e: &DragMoveEvent<crate::sidebar::ResizeSidebar>, _, cx| {
                            let w = f32::from(e.event.position.x - e.bounds.origin.x);
                            this.state.update(cx, |s, cx| s.set_sidebar_width(w, false, cx));
                        }))
                        .on_drop::<crate::sidebar::ResizeSidebar>(cx.listener(|this, _, _, cx| {
                            let w = this.state.read(cx).settings.sidebar_width;
                            this.state.update(cx, |s, cx| s.set_sidebar_width(w, true, cx));
                        }))
                        .when(show_sidebar, |el| el.child(self.sidebar.clone()))
                        .child(div().flex_1().min_w_0().h_full().child(self.browser.clone()))
                        .into_any_element()
                } else {
                    div()
                        .flex_1()
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_color(p.dim)
                        .child(if has_location {
                            "Run a scan to explore this location."
                        } else {
                            "Add a scan location to start a history."
                        })
                        .into_any_element()
                })
                .into_any_element(),
        };

        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(p.bg)
            .text_color(p.text)
            .text_size(px(13.))
            .cursor_default()
            .relative()
            .child(self.title_bar(&p, cx))
            .children(self.access_prompt(&p, cx))
            .child(main)
            .when(show_manage, |el| el.child(self.manage.clone()))
            .children(self.toast(&p, cx))
    }
}
