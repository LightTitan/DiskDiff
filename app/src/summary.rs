//! The comparison summary under the graph, in three variants:
//!
//! * **Comparison** (a saved current scan and a compare scan): the real disk
//!   change as the headline, and the disk bar (see [`crate::diskbar`]) that
//!   splits it into scanned files, unscanned space and free space.
//! * **Snapshot** (a saved scan, nothing to compare): its totals.
//! * **Temporary**: the folder and its size are the headline; volume-wide
//!   history numbers are left out.
//!
//! Labels are sentence case, never all-caps (see CLAUDE.md).

use crate::diskbar::{Figures, Part, disk_bar, part_color, part_name};
use crate::format;
use crate::state::{AppState, location_detail, location_name, used_bytes};
use crate::theme::{ActivePalette, Palette};
use crate::widgets::icon;
use diskdiff_engine::ScanInfo;
use gpui_kit::assets::IconName;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::cell::Cell;
use std::path::Path;
use std::rc::Rc;


// ---- figures ------------------------------------------------------------

/// The reconciliation of two scans' totals.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Reconcile {
    /// Both scans recorded volume info, so real disk usage is known.
    both_volume: bool,
    used_cur: u64,
    used_cmp: u64,
    /// Real disk usage change.
    real: i64,
    /// Change in what the scanner could see.
    scanned: i64,
    /// `real − scanned`: change the scan can't attribute to a file.
    unaccounted: i64,
}

fn reconcile(cur: &ScanInfo, cmp: &ScanInfo) -> Reconcile {
    let used_cur = used_bytes(cur);
    let used_cmp = used_bytes(cmp);
    let real = used_cur as i64 - used_cmp as i64;
    let scanned = cur.size as i64 - cmp.size as i64;
    let unaccounted = real - scanned;
    Reconcile {
        both_volume: cur.disk_total > 0 && cmp.disk_total > 0,
        used_cur,
        used_cmp,
        real,
        scanned,
        unaccounted,
    }
}

const NO_VOLUME_NOTE: &str = "Real disk usage wasn’t recorded for one of these scans, so the unaccounted figure \
                              can’t be computed. Re-scan to start reconciling against actual disk usage.";

/// "N days apart", by rounding the gap to whole days; `None` within a day.
fn days_apart(a: i64, b: i64) -> Option<String> {
    let days = ((a - b).unsigned_abs() as f64 / 86_400.).round() as u64;
    (days > 0).then(|| format!("{} day{} apart", format::count(days), if days == 1 { "" } else { "s" }))
}

/// Why totals may read low: scans without full disk access or with
/// unreadable folders. `scans` pairs a subject ("This scan", "The earlier
/// scan") with the scan.
fn access_note(scans: &[(&str, &ScanInfo)]) -> Option<String> {
    let no_access = if cfg!(windows) { "ran without administrator rights" } else { "ran without Full Disk Access" };
    let parts: Vec<String> = scans
        .iter()
        .filter_map(|(who, s)| {
            let unreadable = (s.unreadable > 0).then(|| {
                format!("couldn’t read {} folder{}", format::count(s.unreadable), if s.unreadable == 1 { "" } else { "s" })
            });
            match (s.full_access, unreadable) {
                (true, None) => None,
                (false, None) => Some(format!("{who} {no_access}")),
                (true, Some(u)) => Some(format!("{who} {u}")),
                (false, Some(u)) => Some(format!("{who} {no_access} and {u}")),
            }
        })
        .collect();
    if parts.is_empty() {
        return None;
    }
    // Both scans with the same single issue read better merged.
    let both_no_access = scans.len() == 2 && scans.iter().all(|(_, s)| !s.full_access && s.unreadable == 0);
    let lead = if both_no_access { format!("Both scans {no_access}") } else { parts.join(", and ") };
    let mut chars = lead.chars();
    let first = chars.next().map(|c| c.to_uppercase().collect::<String>()).unwrap_or_default();
    Some(format!("{first}{}, so totals may read low.", chars.as_str()))
}

// ---- view ---------------------------------------------------------------

pub struct SummaryView {
    state: Entity<AppState>,
    /// Width of the disk bar at the last paint.
    bar_width: Rc<Cell<f32>>,
}

impl SummaryView {
    pub fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        cx.observe(&state, |_, _, cx| cx.notify()).detach();
        SummaryView { state, bar_width: Rc::new(Cell::new(0.)) }
    }
}

impl Render for SummaryView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = cx.palette();
        let state = self.state.clone();
        let s = state.read(cx);
        let Some(cur) = s.current_scan().cloned() else { return div().into_any_element() };
        let panel = div()
            .flex_shrink_0()
            .px(px(16.))
            .pt(px(10.))
            .pb(px(11.))
            .flex()
            .flex_col()
            .bg(p.elevated)
            .border_b_1()
            .border_color(p.border)
            .text_size(px(12.));

        if cur.temporary {
            return panel.children(temporary(&cur, &p)).into_any_element();
        }
        let cmp = if s.comparing() { s.compare_scan().cloned() } else { None };
        match cmp {
            Some(cmp) => panel.children(comparison(&cur, &cmp, self.bar_width.clone(), &p)).into_any_element(),
            None => {
                let others = s.history().iter().any(|x| !x.temporary && x.id != cur.id);
                panel.children(snapshot(&cur, others, &p)).into_any_element()
            }
        }
    }
}

// ---- pieces -------------------------------------------------------------

fn header(p: &Palette, eyebrow: &'static str) -> Div {
    div()
        .flex()
        .items_center()
        .gap(px(7.))
        .min_w_0()
        .whitespace_nowrap()
        .child(div().text_size(px(11.)).text_color(p.dim).child(eyebrow))
}

fn strong(text: impl Into<SharedString>) -> Div {
    div().font_weight(FontWeight::SEMIBOLD).child(text.into())
}

fn dim(p: &Palette, text: impl Into<SharedString>) -> Div {
    div().text_color(p.dim).child(text.into())
}

fn sign_color(p: &Palette, d: i64) -> Hsla {
    p.delta(d, false)
}

/// A small inline stat: label, value, optional colored delta.
fn chip(p: &Palette, label: &'static str, value: impl IntoElement, delta: Option<(String, Hsla)>) -> Div {
    div()
        .flex()
        .items_baseline()
        .gap(px(6.))
        .max_w(px(280.))
        .min_w_0()
        .px(px(9.))
        .py(px(4.))
        .rounded(px(7.))
        .bg(p.column)
        .border_1()
        .border_color(p.border)
        .whitespace_nowrap()
        .child(div().flex_shrink_0().text_size(px(11.)).text_color(p.dim).child(label))
        .child(div().min_w_0().overflow_hidden().text_ellipsis().text_color(p.text).font_weight(FontWeight::SEMIBOLD).child(value))
        .when_some(delta.filter(|d| !d.0.is_empty()), |el, (d, c)| {
            el.child(div().flex_shrink_0().text_size(px(11.)).text_color(c).child(d))
        })
}

fn note(p: &Palette, text: impl Into<SharedString>) -> Div {
    div().max_w(px(560.)).text_size(px(11.)).line_height(px(16.)).text_color(p.dim).child(text.into())
}

fn access_line(p: &Palette, text: String) -> Div {
    div()
        .flex()
        .items_start()
        .gap(px(5.))
        .max_w(px(560.))
        .text_size(px(11.))
        .line_height(px(16.))
        .text_color(p.dim)
        .child(div().pt(px(2.)).child(icon(IconName::Info, 12., p.faint)))
        .child(text)
}

fn comparison(cur: &ScanInfo, cmp: &ScanInfo, bar_width: Rc<Cell<f32>>, p: &Palette) -> Vec<AnyElement> {
    let r = reconcile(cur, cmp);
    let headline_delta = if r.both_volume { r.real } else { r.scanned };

    let dates = div()
        .flex()
        .items_center()
        .gap(px(6.))
        .min_w_0()
        .whitespace_nowrap()
        .text_size(px(11.))
        .text_color(p.dim)
        .child(format::date(cmp.finished_at))
        .child(icon(IconName::ArrowRight, 12., p.faint))
        .child(div().text_color(p.text).font_weight(FontWeight::MEDIUM).child(format::date(cur.finished_at)))
        .when_some(days_apart(cur.finished_at, cmp.finished_at), |el, d| el.child(format!("· {d}")));
    let access = access_note(&[("The current scan", cur), ("the earlier scan", cmp)]);
    let head = div()
        .flex()
        .items_center()
        .justify_between()
        .gap(px(12.))
        .child(dates)
        .when_some(access, |el, a| el.child(hint(p, "summary-access", "Totals may read low", a)));

    let headline = div()
        .flex()
        .flex_col()
        .flex_shrink_0()
        .child(
            div()
                .text_size(px(24.))
                .line_height(px(28.))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(sign_color(p, headline_delta))
                .child(format::delta(headline_delta)),
        )
        .child(
            dim(
                p,
                if r.both_volume {
                    format!("{} used · was {}", format::bytes(r.used_cur), format::bytes(r.used_cmp))
                } else {
                    format!("{} scanned · was {}", format::bytes(cur.size), format::bytes(cmp.size))
                },
            )
            .text_size(px(11.)),
        );

    let detail = if r.both_volume {
        let outside = |s: &ScanInfo| used_bytes(s) as i64 - s.size as i64;
        let figures = Figures {
            scanned: (cmp.size as i64, cur.size as i64),
            outside: (outside(cmp), outside(cur)),
            free: (cmp.disk_free as i64, cur.disk_free as i64),
            total: cur.disk_total as i64,
        };
        let legend = div()
            .flex()
            .flex_wrap()
            .gap_x(px(16.))
            .gap_y(px(4.))
            .child(legend_item(p, Part::Scanned, cur.size, None))
            .child(legend_item(p, Part::Outside, outside(cur).max(0) as u64, Some(OUTSIDE_TIP)))
            .child(legend_item(p, Part::Free, cur.disk_free, None));
        div().flex().flex_col().gap(px(6.)).child(disk_bar(figures, bar_width, p)).child(legend)
    } else {
        div().child(note(p, NO_VOLUME_NOTE))
    };

    vec![
        head.into_any_element(),
        div()
            .mt(px(8.))
            .flex()
            .flex_wrap()
            .items_center()
            .gap_x(px(28.))
            .gap_y(px(10.))
            .child(headline)
            .child(detail.flex_1().min_w(px(260.)).max_w(px(720.)))
            .into_any_element(),
    ]
}

const OUTSIDE_TIP: &str = "Used space DiskDiff can’t attribute to a scanned file: system files, protected areas and \
                           operating-system caches.";

/// Swatch, part name and its size now.
fn legend_item(p: &Palette, part: Part, bytes: u64, tip: Option<&'static str>) -> Stateful<Div> {
    let swatch = div().size(px(8.)).rounded(px(2.)).map(|el| match part {
        Part::Free => el.bg(p.column).border_1().border_color(p.border),
        _ => el.bg(part_color(p, part)),
    });
    div()
        .id(SharedString::from(format!("legend-{part:?}")))
        .flex()
        .items_center()
        .gap(px(6.))
        .text_size(px(11.))
        .whitespace_nowrap()
        .child(swatch)
        .child(dim(p, part_name(part)))
        .child(div().text_color(p.text).font_weight(FontWeight::MEDIUM).child(format::bytes(bytes)))
        .when_some(tip, |el, tip| {
            el.child(icon(IconName::Info, 11., p.faint))
                .tooltip(move |window, cx| gpui_kit::component::tooltip::Tooltip::new(tip).build(window, cx))
        })
}

/// A quiet info line with the full explanation in a tooltip.
fn hint(p: &Palette, id: &'static str, text: &'static str, tip: String) -> Stateful<Div> {
    let tip: SharedString = tip.into();
    div()
        .id(id)
        .flex()
        .items_center()
        .gap(px(5.))
        .flex_shrink_0()
        .whitespace_nowrap()
        .text_size(px(11.))
        .text_color(p.dim)
        .child(icon(IconName::Info, 12., p.faint))
        .child(text)
        .tooltip(move |window, cx| gpui_kit::component::tooltip::Tooltip::new(tip.clone()).build(window, cx))
}

fn snapshot(cur: &ScanInfo, others: bool, p: &Palette) -> Vec<AnyElement> {
    let hint = if others {
        "· pick an earlier scan on the graph to compare"
    } else {
        "· scan again later to see what changed"
    };
    let head = header(p, "Scan from").child(strong(format::date(cur.finished_at))).child(dim(p, hint));
    let chips = div()
        .mt(px(9.))
        .flex()
        .flex_wrap()
        .items_center()
        .gap(px(6.))
        .when(cur.disk_total > 0, |el| el.child(chip(p, "Disk used", format::bytes(cur.disk_used()), None)))
        .when(cur.disk_total > 0 && cur.disk_free > 0, |el| el.child(chip(p, "Free", format::bytes(cur.disk_free), None)))
        .child(chip(p, "Scanned", format::bytes(cur.size), None))
        .child(chip(p, "Files", format::count(cur.files), None))
        .child(chip(p, "Folders", format::count(cur.dirs), None));
    let mut out = vec![head.into_any_element(), chips.into_any_element()];
    if let Some(a) = access_note(&[("This scan", cur)]) {
        out.push(access_line(p, a).mt(px(8.)).into_any_element());
    }
    out
}

/// "12.3" percent with one decimal, or "< 0.1".
fn share(part: u64, whole: u64) -> String {
    let pct = part as f64 / whole as f64 * 100.;
    if pct < 0.1 { "< 0.1".into() } else { format!("{pct:.1}") }
}

fn temporary(cur: &ScanInfo, p: &Palette) -> Vec<AnyElement> {
    let root = Path::new(&cur.root);
    let head = header(p, "Temporary scan")
        .child(strong(format::date(cur.finished_at)))
        .child(dim(p, "· not part of any history, removed when you quit — “Scan and save” starts one"));
    let used = used_bytes(cur);
    let detail = location_detail(root);
    let hero = div()
        .id("summary-temp-root")
        .flex()
        .items_center()
        .gap(px(10.))
        .pr(px(20.))
        .border_r_1()
        .border_color(p.border)
        .tooltip({
            let tip: SharedString = cur.root.clone().into();
            move |window, cx| gpui_kit::component::tooltip::Tooltip::new(tip.clone()).build(window, cx)
        })
        .child(icon(IconName::Folder, 26., p.dim))
        .child(
            div()
                .flex()
                .flex_col()
                .min_w_0()
                .max_w(px(340.))
                .child(
                    div()
                        .text_size(px(20.))
                        .line_height(px(24.))
                        .font_weight(FontWeight::BOLD)
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .child(location_name(root)),
                )
                .when(!detail.is_empty(), |el| {
                    el.child(
                        div()
                            .text_size(px(11.))
                            .text_color(p.dim)
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis_start()
                            .child(detail),
                    )
                }),
        );
    let size = div()
        .flex()
        .flex_col()
        .child(dim(p, "Folder size").text_size(px(11.)))
        .child(div().text_size(px(24.)).line_height(px(28.)).font_weight(FontWeight::SEMIBOLD).child(format::bytes(cur.size)))
        .when(cur.disk_total > 0 && used > 0, |el| {
            el.child(
                dim(p, format!("{}% of the {} in use on this disk", share(cur.size, used), format::bytes(used))).text_size(px(11.)),
            )
        });
    let chips = div()
        .flex()
        .items_center()
        .gap(px(6.))
        .child(chip(p, "Files", format::count(cur.files), None))
        .child(chip(p, "Folders", format::count(cur.dirs), None));
    let mut out = vec![
        head.into_any_element(),
        div()
            .mt(px(9.))
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(20.))
            .child(hero)
            .child(size)
            .child(chips)
            .into_any_element(),
    ];
    if let Some(a) = access_note(&[("This scan", cur)]) {
        out.push(access_line(p, a).mt(px(8.)).into_any_element());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    // `gpui_kit::*` brings its own `test` macro; use the standard one.
    use core::prelude::v1::test;

    fn scan(size: u64, total: u64, free: u64) -> ScanInfo {
        ScanInfo {
            id: 1,
            root: "/x".into(),
            started_at: 0,
            finished_at: 0,
            root_hash: Default::default(),
            size,
            files: 0,
            dirs: 0,
            disk_total: total,
            disk_free: free,
            unreadable: 0,
            added_bytes: 0,
            full_access: true,
            temporary: false,
            note: String::new(),
        }
    }

    #[test]
    fn reconciliation() {
        let cmp = scan(100, 1000, 600); // used 400
        let cur = scan(130, 1000, 500); // used 500
        let r = reconcile(&cur, &cmp);
        assert!(r.both_volume);
        assert_eq!((r.real, r.scanned, r.unaccounted), (100, 30, 70));
        // Opposite directions.
        let cur = scan(70, 1000, 500);
        let r = reconcile(&cur, &cmp);
        assert_eq!((r.scanned, r.unaccounted), (-30, 130));
        // Without volume info, used falls back to the scanned size.
        let r = reconcile(&scan(130, 0, 0), &cmp);
        assert!(!r.both_volume);
        assert_eq!(r.used_cur, 130);
    }

    #[test]
    fn notes() {
        assert_eq!(days_apart(0, 86_400), Some("1 day apart".into()));
        assert_eq!(days_apart(0, 3 * 86_400 + 100), Some("3 days apart".into()));
        assert_eq!(days_apart(0, 3600), None);
        assert_eq!(share(1, 10_000), "< 0.1");
        assert_eq!(share(1234, 10_000), "12.3");
    }

    #[test]
    fn access_notes() {
        let ok = scan(1, 1, 1);
        assert_eq!(access_note(&[("This scan", &ok)]), None);
        let mut limited = ok.clone();
        limited.full_access = false;
        let mut partial = ok.clone();
        partial.unreadable = 3;
        let no_access = if cfg!(windows) { "administrator rights" } else { "Full Disk Access" };
        let n = access_note(&[("The current scan", &limited), ("the earlier scan", &ok)]).unwrap();
        assert!(n.starts_with("The current scan ran without") && n.contains(no_access) && n.ends_with("read low."));
        let n = access_note(&[("The current scan", &limited), ("the earlier scan", &limited)]).unwrap();
        assert!(n.starts_with("Both scans ran without"));
        let n = access_note(&[("The current scan", &ok), ("the earlier scan", &partial)]).unwrap();
        assert!(n.starts_with("The earlier scan couldn’t read 3 folders"), "{n}");
    }
}
