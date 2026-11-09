//! The Finder-style sidebar next to the browser: the user's favorite folders
//! and the cloud drives found in the scan, each with its size in the current
//! scan. Clicking one reveals it in the columns.

use crate::format;
use crate::state::AppState;
use crate::theme::{ActivePalette, Palette};
use crate::widgets::icon;
use diskdiff_engine::cloud::{self, CloudSize};
use diskdiff_engine::query;
use std::collections::HashMap;
use gpui_kit::assets::IconName;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::path::{Path, PathBuf};

pub const DEFAULT_WIDTH: f32 = 200.;
pub const MIN_WIDTH: f32 = 140.;
pub const MAX_WIDTH: f32 = 420.;

/// Dragged while resizing the sidebar by its right edge.
pub struct ResizeSidebar;

pub struct Sidebar {
    state: Entity<AppState>,
    /// Live totals of the cloud drives (memory only), and the data version
    /// they were measured for.
    cloud_totals: HashMap<PathBuf, CloudSize>,
    measured_for: Option<u64>,
    /// The favorite whose × was clicked once and now asks "Remove?".
    confirm_remove: Option<PathBuf>,
}

/// A favorite being dragged to a new position; also its drag preview.
#[derive(Clone)]
struct DraggedFavorite {
    from: usize,
    name: String,
    icon: IconName,
}

impl Render for DraggedFavorite {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = cx.palette();
        div()
            .h(px(26.))
            .px(px(8.))
            .flex()
            .items_center()
            .gap(px(7.))
            .rounded(px(6.))
            .bg(p.elevated)
            .border_1()
            .border_color(p.border)
            .shadow_md()
            .opacity(0.9)
            .text_size(px(12.))
            .text_color(p.text)
            .child(icon(self.icon.clone(), 14., p.accent))
            .child(self.name.clone())
    }
}

struct Entry {
    path: PathBuf,
    name: String,
    icon: IconName,
    size: Option<u64>,
    delta: Option<i64>,
    removable: bool,
    cloud: Option<CloudSize>,
}

impl Sidebar {
    pub fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        cx.observe(&state, |_, _, cx| cx.notify()).detach();
        Sidebar { state, cloud_totals: HashMap::new(), measured_for: None, confirm_remove: None }
    }

    /// First click on ×: ask, and stop asking after a few seconds.
    fn ask_remove(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.confirm_remove = Some(path.clone());
        cx.notify();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(std::time::Duration::from_secs(4)).await;
            let _ = this.update(cx, |this, cx| {
                if this.confirm_remove.as_ref() == Some(&path) {
                    this.confirm_remove = None;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Measures the cloud drives' real totals in the background, once per
    /// data version (a new scan re-measures).
    fn measure_cloud(&mut self, drives: Vec<PathBuf>, cx: &mut Context<Self>) {
        let version = self.state.read(cx).data_version;
        if self.measured_for == Some(version) || drives.is_empty() {
            return;
        }
        self.measured_for = Some(version);
        let task = cx.background_executor().spawn(async move {
            let never = std::sync::atomic::AtomicBool::new(false);
            drives.into_iter().map(|d| {
                let size = cloud::measure(&d, &never);
                (d, size)
            }).collect::<HashMap<_, _>>()
        });
        cx.spawn(async move |this, cx| {
            let totals = task.await;
            let _ = this.update(cx, |this, cx| {
                this.cloud_totals = totals;
                cx.notify();
            });
        })
        .detach();
    }

    fn entries(&self, cx: &App) -> (Vec<Entry>, Vec<Entry>) {
        let s = self.state.read(cx);
        // dirs: HOME on macOS/Linux, the user profile on Windows (no HOME there).
        let home = dirs::home_dir().unwrap_or_default();
        let cur = s.current_scan();
        let cmp = if s.comparing() { s.compare_scan() } else { None };
        let size_of = |p: &Path| {
            let size = cur.and_then(|c| query::size_at(&s.store, c, p).ok().flatten()).filter(|_| cur.is_some_and(|c| p.starts_with(&c.root)));
            let delta = match (size, cmp.and_then(|c| query::size_at(&s.store, c, p).ok().flatten())) {
                (Some(a), Some(b)) => Some(a as i64 - b as i64),
                _ => None,
            };
            (size, delta)
        };
        let favorites = s
            .favorites()
            .iter()
            .map(|p| {
                let (size, delta) = size_of(p);
                let (name, icon) = favorite_look(p, &home);
                Entry { path: p.clone(), name, icon, size, delta, removable: true, cloud: None }
            })
            .collect();

        // Cloud drives: whatever the scan saw under ~/Library/CloudStorage,
        // plus iCloud Drive.
        let mut cloud = Vec::new();
        if let Some(c) = cur {
            let storage = home.join("Library/CloudStorage");
            if let Ok(listing) = query::list(&s.store, c, None, &storage) {
                let mut drives: Vec<_> = listing.items.iter().filter(|i| i.is_dir).collect();
                drives.sort_by(|a, b| a.name.cmp(&b.name));
                for d in drives {
                    let p = storage.join(&*d.name);
                    let (size, delta) = size_of(&p);
                    let total = self.cloud_totals.get(&p).copied();
                    cloud.push(Entry { path: p, name: cloud_name(&d.name), icon: IconName::Cloud, size, delta, removable: false, cloud: total });
                }
            }
            let icloud = home.join("Library/Mobile Documents/com~apple~CloudDocs");
            let (size, delta) = size_of(&icloud);
            if size.is_some_and(|s| s > 0) {
                let total = self.cloud_totals.get(&icloud).copied();
                cloud.insert(0, Entry { path: icloud, name: "iCloud Drive".into(), icon: IconName::Cloud, size, delta, removable: false, cloud: total });
            }
        }
        (favorites, cloud)
    }
}

/// Finder's names and icons for the well-known folders.
fn favorite_look(p: &Path, home: &Path) -> (String, IconName) {
    let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| p.display().to_string());
    if p == home {
        return (name, IconName::House);
    }
    if p == Path::new("/Applications") {
        return ("Applications".into(), IconName::LayoutGrid);
    }
    if p.parent() == Some(home) {
        match name.as_str() {
            "Desktop" => return (name, IconName::Monitor),
            "Documents" => return (name, IconName::FileText),
            "Downloads" => return (name, IconName::CircleArrowDown),
            "Movies" => return (name, IconName::Film),
            "Music" => return (name, IconName::Music),
            "Pictures" => return (name, IconName::Image),
            ".Trash" => return ("Trash".into(), IconName::Trash),
            _ => {}
        }
    }
    (name, IconName::Folder)
}

/// "OneDrive-Personal" → "OneDrive – Personal", "GoogleDrive-me@x.com" →
/// "Google Drive", like Finder's sidebar.
fn cloud_name(dir: &str) -> String {
    let (provider, account) = dir.split_once('-').unwrap_or((dir, ""));
    let provider = match provider {
        "GoogleDrive" => return "Google Drive".into(),
        "OneDrive" => "OneDrive",
        "Dropbox" => "Dropbox",
        "Box" => "Box",
        other => other,
    };
    if account.is_empty() { provider.into() } else { format!("{provider} – {account}") }
}

fn section(title: &'static str, p: &Palette) -> Div {
    div().px(px(10.)).pt(px(14.)).pb(px(4.)).text_size(px(11.)).font_weight(FontWeight::SEMIBOLD).text_color(p.dim).child(title)
}

impl Render for Sidebar {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = cx.palette();
        let (favorites, cloud) = self.entries(cx);
        self.measure_cloud(cloud.iter().map(|e| e.path.clone()).collect(), cx);
        let row = |e: Entry, ix: usize, cx: &mut Context<Self>| {
            let in_scan = e.size.is_some();
            let path = e.path.clone();
            let remove = e.path.clone();
            let group: SharedString = format!("fav-{ix}-{}", e.removable).into();
            let confirming = e.removable && self.confirm_remove.as_ref() == Some(&e.path);
            let dragged = DraggedFavorite { from: ix, name: e.name.clone(), icon: e.icon.clone() };
            let online = e.cloud.filter(|c| c.online_only() >= 1_000_000);
            let tooltip = match (e.size, e.delta) {
                _ if online.is_some() => {
                    let c = online.unwrap();
                    format!("{} · {} in total, {} on this Mac", e.path.display(), format::bytes(c.logical), format::bytes(c.on_disk))
                }
                (Some(_), Some(d)) if d != 0 => format!("{} · {} since the compared scan", e.path.display(), format::delta(d)),
                (Some(_), _) => e.path.display().to_string(),
                (None, _) => format!("{} · not part of the current scan", e.path.display()),
            };
            div()
                .id(SharedString::from(format!("sidebar-{ix}-{}", e.path.display())))
                .group(group.clone())
                .mx(px(6.))
                .h(px(26.))
                .px(px(6.))
                .flex()
                .items_center()
                .gap(px(7.))
                .rounded(px(5.))
                .cursor_default()
                .text_size(px(12.))
                .hover(|s| s.bg(p.row_hover))
                .when(!in_scan, |el| el.opacity(0.5))
                // Favorites reorder by dragging one onto another: it lands in
                // that row's place, marked by an accent line while dragging.
                .when(e.removable, |el| {
                    let accent = p.accent;
                    el.on_drag(dragged, |d, _, _, cx| cx.new(|_| d.clone()))
                        .drag_over::<DraggedFavorite>(move |style, d, _, _| {
                            if d.from == ix {
                                style
                            } else if d.from > ix {
                                style.border_t_2().border_color(accent)
                            } else {
                                style.border_b_2().border_color(accent)
                            }
                        })
                        .on_drop(cx.listener(move |this, d: &DraggedFavorite, _, cx| {
                            this.state.update(cx, |s, cx| s.move_favorite(d.from, ix, cx));
                        }))
                })
                .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                    if !*hovered && confirming {
                        this.confirm_remove = None;
                        cx.notify();
                    }
                }))
                .child(icon(e.icon, 14., p.accent))
                .child(div().flex_1().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().child(e.name))
                .when_some(online, |el, c| {
                    el.child(
                        div()
                            .flex()
                            .flex_shrink_0()
                            .items_center()
                            .gap(px(3.))
                            .text_size(px(11.))
                            .text_color(p.accent)
                            .child(icon(IconName::Cloud, 10., p.accent))
                            .child(format!("{}{}", format::bytes(c.logical), if c.partial { "+" } else { "" })),
                    )
                })
                .when(online.is_none(), |el| {
                    el.when_some(e.size, |el, size| {
                        el.child(div().flex_shrink_0().text_size(px(11.)).text_color(p.dim).child(format::bytes(size)))
                    })
                })
                // Removing takes two clicks: × first turns into "Remove".
                .when(e.removable && confirming, |el| {
                    let state = self.state.clone();
                    let remove = remove.clone();
                    el.child(
                        div()
                            .id(SharedString::from(format!("unfav-confirm-{ix}")))
                            .flex_shrink_0()
                            .px(px(6.))
                            .h(px(18.))
                            .flex()
                            .items_center()
                            .rounded(px(4.))
                            .bg(p.up.opacity(0.16))
                            .text_size(px(11.))
                            .text_color(p.up)
                            .hover(|s| s.bg(p.up.opacity(0.26)))
                            .child("Remove")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.confirm_remove = None;
                                let remove = remove.clone();
                                state.update(cx, |s, cx| s.toggle_favorite(remove, cx));
                            })),
                    )
                })
                .when(e.removable && !confirming, |el| {
                    let path = remove.clone();
                    el.child(
                        div()
                            .id(SharedString::from(format!("unfav-{ix}")))
                            .size(px(16.))
                            .flex()
                            .flex_shrink_0()
                            .items_center()
                            .justify_center()
                            .rounded(px(4.))
                            .opacity(0.)
                            .group_hover(group, |s| s.opacity(1.))
                            .hover(|s| s.bg(p.control_hover))
                            .child(icon(IconName::X, 11., p.dim))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.ask_remove(path.clone(), cx);
                            }))
                            .tooltip(|w, cx| gpui_kit::component::tooltip::Tooltip::new("Remove from Sidebar").build(w, cx)),
                    )
                })
                .tooltip(move |w, cx| gpui_kit::component::tooltip::Tooltip::new(tooltip.clone()).build(w, cx))
                .on_click(cx.listener(move |this, _, _, cx| {
                    let path = path.clone();
                    this.state.update(cx, |s, cx| {
                        if in_scan {
                            s.go_to(path, cx);
                        } else {
                            s.notify_toast("That folder is not part of the current scan.", None, cx);
                        }
                    });
                }))
        };
        let width = self.state.read(cx).settings.sidebar_width.clamp(MIN_WIDTH, MAX_WIDTH);
        let favorites_empty = favorites.is_empty();
        let has_cloud = !cloud.is_empty();
        let fav_rows: Vec<_> = favorites.into_iter().enumerate().map(|(i, e)| row(e, i, cx)).collect();
        let cloud_rows: Vec<_> = cloud.into_iter().enumerate().map(|(i, e)| row(e, 1000 + i, cx)).collect();

        let accent = p.accent;
        div()
            .relative()
            .flex_shrink_0()
            .w(px(width))
            .h_full()
            .bg(p.elevated)
            .border_r_1()
            .border_color(p.border)
            .child(
                div()
                    .id("sidebar")
                    .size_full()
                    .overflow_y_scroll()
                    .pb(px(10.))
                    .child(section("Favorites", &p))
                    .children(fav_rows)
                    .when(favorites_empty, |el| {
                        el.child(
                            div().px(px(14.)).py(px(4.)).text_size(px(11.)).text_color(p.dim).child("Select a folder and press ⌃⌘T to add it here."),
                        )
                    })
                    .when(has_cloud, |el| el.child(section("Cloud", &p)).children(cloud_rows)),
            )
            // Resize grip on the right edge; double-click restores the default.
            .child(
                div()
                    .id("sidebar-grip")
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .right(px(-3.))
                    .w(px(6.))
                    .cursor(CursorStyle::ResizeLeftRight)
                    .group("sidebar-grip-g")
                    .child(div().absolute().top_0().bottom_0().left(px(2.)).w(px(2.)).bg(accent).opacity(0.).group_hover("sidebar-grip-g", |s| s.opacity(1.)))
                    .on_drag(ResizeSidebar, |_, _, _, cx| cx.new(|_| EmptyView))
                    .on_click(cx.listener(|this, e: &ClickEvent, _, cx| {
                        if e.click_count() == 2 {
                            this.state.update(cx, |s, cx| s.set_sidebar_width(DEFAULT_WIDTH, true, cx));
                        }
                    }))
                    .tooltip(|w, cx| gpui_kit::component::tooltip::Tooltip::new("Drag to resize · double-click to reset").build(w, cx)),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;

    #[test]
    fn cloud_names() {
        assert_eq!(cloud_name("OneDrive-Personal"), "OneDrive – Personal");
        assert_eq!(cloud_name("GoogleDrive-me@example.com"), "Google Drive");
        assert_eq!(cloud_name("Dropbox"), "Dropbox");
    }
}
