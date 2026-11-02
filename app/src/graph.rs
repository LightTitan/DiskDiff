//! The scan graph: the disk of each scan of the selected location, oldest to
//! newest, drawn as stacked areas in the disk bar's colors: scanned files
//! (solid blue), unscanned space (light blue stripes), and free space up to a
//! dashed disk-size line. The bottom of the axis is cut (a diagonal break)
//! when it doesn't start at zero, so the areas never pretend to.
//!
//! It is the only scan selector: clicking a dot makes that scan current;
//! pressing anywhere else and dragging scrubs the compare scan, and a hollow
//! triangle in the empty space next to a nearby dot (above it, or below it
//! for free space) sets compare in one click. The two selected
//! scans span a shaded band with the change in used space on top.
//!
//! With "Graph Available Space" on, the graph plots free space instead: the same
//! stack upside down. The line is free space; above it hang unscanned space
//! (stripes) and scanned files (solid) toward the disk size, which is cut
//! (the diagonal break at the top) when it doesn't fit. The pill then shows
//! the change in free space.
//!
//! Geometry is plain `f32` math in canvas-local coordinates ([`Plot`],
//! [`outside_stripes`]), so it is unit-tested without a window. Painting happens
//! in one `canvas`; the hover card is an anchored overlay.
//!
//! Cursor rule (CLAUDE.md): arrow on dots, triangle and plot; ↔ over the
//! scrubber; closed hand while dragging. Never the pointing hand.

use crate::format;
use crate::state::{AppState, used_bytes};
use crate::theme::{ActivePalette, Palette};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

/// Total height of the graph band.
pub const HEIGHT: f32 = 168.;
const M_TOP: f32 = 40.;
const M_RIGHT: f32 = 18.;
const M_BOTTOM: f32 = 26.;
const M_LEFT: f32 = 56.;
/// Dots start this far inside the plot, so the current dot's halo clears
/// the y labels and the right edge.
const X_INSET: f32 = 6.;
/// The current and compare dots are spread at least this far apart.
const MIN_PAIR_GAP: f32 = 104.;
const DOT_R: f32 = 4.;
const CUR_R: f32 = 6.;
const CMP_R: f32 = 5.5;
/// Invisible click radius around every dot.
const HIT_R: f32 = 14.;
/// The compare-target triangle appears within this distance of a dot.
const TARGET_R: f32 = 24.;
const SCRUB_HALF: f32 = 8.;
/// Month labels closer than this to the previous one are skipped.
const MONTH_GAP: f32 = 80.;

const TICK_FONT: f32 = 10.;
const PILL_FONT: f32 = 10.5;
const LH: f32 = 12.;
/// Horizontal padding and height of the change pill.
const PILL_PX: f32 = 7.;
const PILL_H: f32 = 16.;
/// The change pill's row: the band's 6 px overhang, a 4 px gap and the pill.
/// It sits above the plot, or below it for free space, where the empty side
/// of the line is.
const PILL_ROW: f32 = 6. + 4. + PILL_H;
/// Stripe spacing in the unscanned area.
const STRIPE_GAP: f32 = 5.;

// ---- data ---------------------------------------------------------------

/// One plotted scan, copied out of the state so painting needs no borrow.
#[derive(Clone, Debug)]
struct SeriesPt {
    id: i64,
    used: u64,
    /// Free space when the scan recorded volume info.
    free: Option<u64>,
    /// Bytes of scanned files, at most `used`.
    scanned: u64,
    temporary: bool,
    at: i64,
    full_access: bool,
    unreadable: u64,
}

#[derive(Clone, Debug)]
struct Series {
    pts: Vec<SeriesPt>,
    cur: Option<usize>,
    cmp: Option<usize>,
    /// Plot free space instead of the used-space stack.
    free: bool,
}

impl Series {
    fn from_state(s: &AppState) -> Series {
        let hist = s.history();
        let pts: Vec<SeriesPt> = hist
            .iter()
            .map(|x| {
                let used = used_bytes(x);
                SeriesPt {
                    id: x.id,
                    used,
                    free: (x.disk_total > 0).then_some(x.disk_free),
                    scanned: x.size.min(used),
                    temporary: x.temporary,
                    at: x.finished_at,
                    full_access: x.full_access,
                    unreadable: x.unreadable,
                }
            })
            .collect();
        let cur = s.current.and_then(|id| pts.iter().position(|p| p.id == id));
        let cmp = s.compare.and_then(|id| pts.iter().position(|p| p.id == id)).filter(|&c| Some(c) != cur);
        Series { pts, cur, cmp, free: graph_free(s) }
    }

    /// Scans that can become the compare scan: saved ones other than the
    /// current scan. None while a temporary scan is current.
    fn candidates(&self) -> Vec<usize> {
        if self.cur.is_some_and(|c| self.pts[c].temporary) {
            return Vec::new();
        }
        (0..self.pts.len()).filter(|&i| !self.pts[i].temporary && Some(i) != self.cur).collect()
    }
}

// ---- geometry -----------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq)]
struct Rect {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
}

impl Rect {
    fn new(x: f32, y: f32, w: f32, h: f32) -> Rect {
        Rect { x, y, w, h }
    }
    fn right(&self) -> f32 {
        self.x + self.w
    }
    fn bottom(&self) -> f32 {
        self.y + self.h
    }
    fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && x <= self.right() && y >= self.y && y <= self.bottom()
    }
    fn around(x: f32, y: f32, r: f32) -> Rect {
        Rect::new(x - r, y - r, 2. * r, 2. * r)
    }
}

/// X positions, evenly spaced by index over `[0, w]`. When current and
/// compare are closer than [`MIN_PAIR_GAP`], the interval between them is
/// widened to that gap and the points on either side are compressed, so both
/// value labels stay readable. Near an edge the pair is held back far enough
/// that the points beyond it keep some room instead of piling onto the edge.
/// Order never changes.
fn x_positions(n: usize, w: f32, a: Option<usize>, b: Option<usize>) -> Vec<f32> {
    let natural = |i: usize| if n <= 1 { w / 2. } else { i as f32 / (n - 1) as f32 * w };
    let mut xs: Vec<f32> = (0..n).map(natural).collect();
    let (Some(a), Some(b)) = (a, b) else { return xs };
    if a == b || a >= n || b >= n {
        return xs;
    }
    let (l, r) = (a.min(b), a.max(b));
    let gap = MIN_PAIR_GAP.min(w);
    let (nl, nr) = (natural(l), natural(r));
    if nr - nl >= gap {
        return xs;
    }
    // Each interval outside the pair keeps at least 3/4 of an even share of
    // the width the pair leaves.
    let outside = (n - 1 - (r - l)) as f32;
    let room = if outside > 0. { (w - gap).max(0.) / outside * 0.75 } else { 0. };
    let mid = (nl + nr) / 2.;
    let tl = (mid - gap / 2.).clamp(l as f32 * room, (w - gap - (n - 1 - r) as f32 * room).max(0.));
    let tr = tl + gap;
    for (i, x) in xs.iter_mut().enumerate() {
        let nx = natural(i);
        *x = if i < l {
            if nl > 0. { nx / nl * tl } else { 0. }
        } else if i <= r {
            tl + (nx - nl) / (nr - nl) * gap
        } else if w > nr {
            tr + (nx - nr) / (w - nr) * (w - tr)
        } else {
            w
        };
    }
    xs
}

/// d3's `tickIncrement`.
fn tick_increment(start: f64, stop: f64, count: f64) -> f64 {
    let step = (stop - start) / count.max(1.);
    let power = step.log10().floor();
    let error = step / 10f64.powf(power);
    let factor = if error >= 50f64.sqrt() {
        10.
    } else if error >= 10f64.sqrt() {
        5.
    } else if error >= 2f64.sqrt() {
        2.
    } else {
        1.
    };
    if power >= 0. { factor * 10f64.powf(power) } else { -(10f64.powf(-power)) / factor }
}

/// d3's `scale.nice()` (count 10).
fn nice(mut lo: f64, mut hi: f64) -> (f64, f64) {
    let mut prev = None;
    for _ in 0..10 {
        let step = tick_increment(lo, hi, 10.);
        if !step.is_finite() || step == 0. || Some(step) == prev {
            break;
        }
        if step > 0. {
            lo = (lo / step).floor() * step;
            hi = (hi / step).ceil() * step;
        } else {
            lo = (lo * step).ceil() / step;
            hi = (hi * step).floor() / step;
        }
        prev = Some(step);
    }
    (lo, hi)
}

/// d3's `ticks(lo, hi, count)`.
fn ticks(lo: f64, hi: f64, count: f64) -> Vec<f64> {
    let step = tick_increment(lo, hi, count);
    if !step.is_finite() || step == 0. {
        return Vec::new();
    }
    if step > 0. {
        let (i0, i1) = ((lo / step).ceil() as i64, (hi / step).floor() as i64);
        (i0..=i1).map(|i| i as f64 * step).collect()
    } else {
        let s = -step;
        let (i0, i1) = ((lo * s).ceil() as i64, (hi * s).floor() as i64);
        (i0..=i1).map(|i| i as f64 / s).collect()
    }
}

/// Free space is plotted only when chosen and every plotted scan recorded
/// volume info, since a partial series would misrepresent the trend.
fn graph_free(s: &AppState) -> bool {
    let h = s.history();
    s.settings.graph_free && !h.is_empty() && h.iter().all(|x| x.disk_total > 0)
}

/// The y domain for free space, [`y_domain`] mirrored, and whether the
/// stack is cut below the disk size. The top sits above the highest bottom
/// of the scanned area, so it always has some height. The bottom is zero
/// (a full disk) when the free space below the lowest line is no taller
/// than the data itself; otherwise it hugs the data.
fn free_domain(pts: &[SeriesPt]) -> (f64, f64, bool) {
    let free = |p: &SeriesPt| p.free.unwrap_or(0) as f64;
    let min_free = pts.iter().map(free).fold(f64::MAX, f64::min);
    let max_top = pts.iter().map(|p| free(p) + (p.used - p.scanned) as f64).fold(0., f64::max);
    let disk = pts.iter().map(|p| free(p) + p.used as f64).fold(0., f64::max);
    let span = (max_top - min_free).max(max_top * 0.25);
    let lo = if min_free <= span { 0. } else { min_free - (span * 0.14).max(1_073_741_824.) };
    let hi = (max_top + (max_top - lo) * 0.14).min(disk);
    // Rounding never lifts the top past the disk, where the stack ends.
    let (lo, hi) = nice(lo, hi);
    (lo, hi.min(disk), hi < disk)
}

/// The y domain and whether the disk-size line fits in it. The bottom sits
/// below the smallest scanned size, so the scanned area always has some
/// height. The disk size is the top when the free space above the highest
/// used value is no taller than the data itself; otherwise (a mostly empty
/// disk) the top hugs the data and the disk line is left out.
fn y_domain(pts: &[SeriesPt]) -> (f64, f64, bool) {
    let max_used = pts.iter().map(|p| p.used).max().unwrap_or(0) as f64;
    let min_scanned = pts.iter().map(|p| p.scanned).min().unwrap_or(0) as f64;
    let cap = pts.iter().filter_map(|p| p.free.map(|f| p.used + f)).max().map(|c| c as f64);
    let span = (max_used - min_scanned).max(max_used * 0.25);
    let show_cap = cap.is_some_and(|c| c > max_used && c - max_used <= span);
    let hi = match cap {
        Some(c) if show_cap => c,
        _ => max_used + (span * 0.14).max(1_073_741_824.),
    };
    let lo = (min_scanned - (hi - min_scanned) * 0.14).max(0.);
    let (lo, hi) = nice(lo, hi);
    (lo, hi, show_cap)
}

/// Which scans get a month label: the first, the last, and the first of
/// each new month, skipping any closer than [`MONTH_GAP`] to the previous.
/// The last scan is also skipped when it would repeat the previous label's
/// month, since a month label carries no day.
fn month_labels(xs: &[f32], keys: &[(i32, u32)]) -> Vec<usize> {
    let n = xs.len();
    let mut out: Vec<usize> = Vec::new();
    for i in 0..n {
        let new_month = i == 0 || keys[i] != keys[i - 1];
        let candidate = new_month || (i == n - 1 && out.last().is_none_or(|&l| keys[l] != keys[i]));
        if !candidate {
            continue;
        }
        if let Some(&last) = out.last() {
            if xs[i] - xs[last] < MONTH_GAP {
                continue;
            }
        }
        out.push(i);
    }
    out
}

/// Laid-out plot in canvas-local coordinates.
#[derive(Clone, Debug)]
struct Plot {
    w: f32,
    top: f32,
    left: f32,
    iw: f32,
    ih: f32,
    xs: Vec<f32>,
    /// The line: used space, or free space.
    ys: Vec<f32>,
    /// The edge of the scanned area: its top, or its bottom for free space
    /// (where the stack hangs from the top).
    ys_scanned: Vec<f32>,
    /// Disk size per scan, when it fits in the domain and is known.
    ys_cap: Vec<Option<f32>>,
    /// The stack is cut: the axis doesn't start at zero, or for free space
    /// doesn't reach the disk size.
    broken: bool,
    /// Free space is plotted, with the stack above the line.
    free: bool,
    ticks: Vec<(f32, u64)>,
    cur: Option<usize>,
    cmp: Option<usize>,
    candidates: Vec<usize>,
}

impl Plot {
    fn new(series: &Series, w: f32, h: f32) -> Plot {
        let iw = (w - M_LEFT - M_RIGHT).max(1.);
        // For free space the pill's row moves from the top margin to the
        // bottom one; the plot keeps its height.
        let (m_top, m_bottom) = if series.free { (M_TOP - PILL_ROW, M_BOTTOM + PILL_ROW) } else { (M_TOP, M_BOTTOM) };
        let ih = (h - m_top - m_bottom).max(1.);
        let (lo, hi, show_cap, broken) = if series.free {
            let (lo, hi, cut) = free_domain(&series.pts);
            (lo, hi, false, cut)
        } else {
            let (lo, hi, show_cap) = y_domain(&series.pts);
            (lo, hi, show_cap, lo > 0.)
        };
        let y = |v: f64| m_top + ih - ((v.max(lo) - lo) / (hi - lo)) as f32 * ih;
        let span = (iw - 2. * X_INSET).max(1.);
        let xs = x_positions(series.pts.len(), span, series.cur, series.cmp).into_iter().map(|x| x + M_LEFT + X_INSET).collect();
        let (ys, ys_scanned) = if series.free {
            // Free space plus unscanned space: everything but scanned files.
            let free = |p: &SeriesPt| p.free.unwrap_or(0);
            (
                series.pts.iter().map(|p| y(free(p) as f64)).collect(),
                series.pts.iter().map(|p| y((free(p) + p.used - p.scanned) as f64)).collect(),
            )
        } else {
            (series.pts.iter().map(|p| y(p.used as f64)).collect(), series.pts.iter().map(|p| y(p.scanned as f64)).collect())
        };
        let ys_cap =
            series.pts.iter().map(|p| p.free.filter(|_| show_cap).map(|f| y((p.used + f) as f64))).collect();
        let ticks = ticks(lo, hi, 3.).into_iter().map(|t| (y(t), t.max(0.) as u64)).collect();
        Plot {
            w,
            top: m_top,
            left: M_LEFT,
            iw,
            ih,
            xs,
            ys,
            ys_scanned,
            ys_cap,
            broken,
            free: series.free,
            ticks,
            cur: series.cur,
            cmp: series.cmp,
            candidates: series.candidates(),
        }
    }

    fn bottom(&self) -> f32 {
        self.top + self.ih
    }

    /// Top and height of the compare band: the plot plus a 6 px overhang
    /// toward the change pill.
    fn band(&self) -> (f32, f32) {
        (if self.free { self.top } else { self.top - 6. }, self.ih + 6.)
    }

    /// Top of the change pill.
    fn pill_y(&self) -> f32 {
        if self.free { self.bottom() + PILL_ROW - PILL_H } else { self.top - PILL_ROW }
    }

    /// Where the month row starts: right under the plot, or under the
    /// pill's row for free space.
    fn months_y(&self) -> f32 {
        if self.free { self.bottom() + PILL_ROW } else { self.bottom() }
    }

    fn dot_r(&self, i: usize) -> f32 {
        if Some(i) == self.cur {
            CUR_R
        } else if Some(i) == self.cmp {
            CMP_R
        } else {
            DOT_R
        }
    }

    /// The dot nearest `(x, y)` within `radius`.
    fn dot_at(&self, x: f32, y: f32, radius: impl Fn(usize) -> f32) -> Option<usize> {
        (0..self.xs.len())
            .map(|i| (i, (self.xs[i] - x).hypot(self.ys[i] - y)))
            .filter(|&(i, d)| d <= radius(i))
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(i, _)| i)
    }

    /// The compare candidate nearest `x`, ignoring y (scrubbing).
    fn snap(&self, x: f32) -> Option<usize> {
        self.candidates.iter().copied().min_by(|&a, &b| (self.xs[a] - x).abs().total_cmp(&(self.xs[b] - x).abs()))
    }

    /// The compare candidate within [`TARGET_R`] of the pointer that gets the
    /// hollow triangle; never the current compare scan.
    fn target_near(&self, x: f32, y: f32) -> Option<usize> {
        self.candidates
            .iter()
            .copied()
            .filter(|&i| Some(i) != self.cmp)
            .map(|i| (i, (self.xs[i] - x).hypot(self.ys[i] - y)))
            .filter(|&(_, d)| d <= TARGET_R)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(i, _)| i)
    }

    /// Triangle points and its click area, on the empty side of the line so
    /// it never sits on the stack: above the dot pointing down, or below it
    /// pointing up for free space. The click area stops just short of the
    /// dot so the dot itself stays clickable.
    fn triangle(&self, i: usize) -> ([(f32, f32); 3], Rect) {
        let (x, y, r) = (self.xs[i], self.ys[i], self.dot_r(i));
        if self.free {
            let ty = y + r + 8.;
            let pts = [(x - 6., ty + 8.), (x + 6., ty + 8.), (x, ty - 2.)];
            let hit_top = y + r + 2.;
            (pts, Rect::new(x - 16., hit_top, 32., (ty + 14.) - hit_top))
        } else {
            let ty = y - r - 8.;
            let pts = [(x - 6., ty - 8.), (x + 6., ty - 8.), (x, ty + 2.)];
            let hit_top = ty - 14.;
            (pts, Rect::new(x - 16., hit_top, 32., (y - r - 2.) - hit_top))
        }
    }

    fn scrub_rect(&self) -> Option<Rect> {
        let c = self.cmp?;
        Some(Rect::new(self.xs[c] - SCRUB_HALF, self.top, 2. * SCRUB_HALF, self.ih))
    }
}

// ---- view ---------------------------------------------------------------

#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Hover {
    /// Dot under the pointer (hover card).
    dot: Option<i64>,
    /// Scan whose compare-target triangle is shown.
    target: Option<i64>,
    /// Pointer is over that triangle.
    target_hot: bool,
}

pub struct GraphView {
    state: Entity<AppState>,
    bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    hover: Hover,
    dragging: bool,
}

impl GraphView {
    pub fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        cx.observe(&state, |_, _, cx| cx.notify()).detach();
        #[allow(unused_mut)]
        let mut view = GraphView { state, bounds: Rc::new(Cell::new(None)), hover: Hover::default(), dragging: false };
        #[cfg(debug_assertions)]
        view.debug_hooks(cx);
        view
    }

    /// `DISKDIFF_GRAPH="cur=3 cmp=2 hover=5 target=6 free"` (indexes into
    /// the history, oldest first) sets up a state for screenshots. Debug
    /// builds only; nothing is saved.
    #[cfg(debug_assertions)]
    fn debug_hooks(&mut self, cx: &mut Context<Self>) {
        let Ok(spec) = std::env::var("DISKDIFF_GRAPH") else { return };
        let ids: Vec<i64> = self.state.read(cx).history().iter().map(|s| s.id).collect();
        let mut hover = Hover::default();
        for part in spec.split_whitespace() {
            let (k, v) = part.split_once('=').unwrap_or((part, ""));
            let id = v.parse::<usize>().ok().and_then(|i| ids.get(i).copied());
            match (k, id) {
                ("cur", Some(id)) => self.state.update(cx, |s, cx| s.set_current(id, cx)),
                ("cmp", id) => self.state.update(cx, |s, cx| s.set_compare(id, cx)),
                ("hover", id) => hover.dot = id,
                ("target", id) => hover.target = id,
                ("hot", _) => hover.target_hot = true,
                ("free", _) => self.state.update(cx, |s, _| s.settings.graph_free = true),
                _ => {}
            }
        }
        self.hover = hover;
        // The hover card needs the canvas bounds from a first paint.
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(std::time::Duration::from_millis(600)).await;
            let _ = this.update(cx, |_, cx| cx.notify());
        })
        .detach();
    }

    fn set_hover(&mut self, hover: Hover, cx: &mut Context<Self>) {
        if self.hover != hover {
            self.hover = hover;
            cx.notify();
        }
    }

    fn set_dragging(&mut self, dragging: bool, cx: &mut Context<Self>) {
        if self.dragging != dragging {
            self.dragging = dragging;
            if dragging {
                self.hover = Hover::default();
            }
            cx.notify();
        }
    }

    fn hover_card(&self, series: &Series, p: &Palette) -> Option<AnyElement> {
        if self.dragging {
            return None;
        }
        let id = self.hover.dot?;
        let bounds = self.bounds.get()?;
        let i = series.pts.iter().position(|x| x.id == id)?;
        let plot = Plot::new(series, f32::from(bounds.size.width), HEIGHT);
        let pt = &series.pts[i];
        let r = plot.dot_r(i);
        let right_half = plot.xs[i] > plot.w / 2.;
        let (dx, anchor) = if right_half { (-(r + 12.), Anchor::RightCenter) } else { (r + 12., Anchor::LeftCenter) };
        let pos = point(bounds.origin.x + px(plot.xs[i] + dx), bounds.origin.y + px(plot.ys[i]));

        let row = |label: &'static str, value: String| {
            div()
                .flex()
                .justify_between()
                .gap(px(14.))
                .child(div().text_color(p.dim).child(label))
                .child(div().text_color(p.text).child(value))
        };
        let mut notes: Vec<String> = Vec::new();
        if !pt.full_access {
            notes.push(if cfg!(windows) { "Ran without administrator rights".into() } else { "Ran without Full Disk Access".into() });
        }
        if pt.unreadable > 0 {
            notes.push(format!(
                "{} folder{} couldn’t be read",
                format::count(pt.unreadable),
                if pt.unreadable == 1 { "" } else { "s" }
            ));
        }
        let card = div()
            .min_w(px(150.))
            .px(px(10.))
            .py(px(8.))
            .flex()
            .flex_col()
            .gap(px(3.))
            .rounded(px(7.))
            .bg(p.control)
            .border_1()
            .border_color(p.border)
            .shadow(vec![BoxShadow {
                color: p.shadow,
                offset: point(px(0.), px(4.)),
                blur_radius: px(14.),
                spread_radius: px(0.),
                inset: false,
            }])
            .text_size(px(11.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .mb(px(2.))
                    .child(div().text_color(p.text).font_weight(FontWeight::SEMIBOLD).child(format::long_date(pt.at)))
                    .when(pt.temporary, |el| el.child(div().text_color(p.new).child("Temporary"))),
            )
            .when_some(pt.free, |el, f| el.child(row("Available", format::bytes(f))))
            .child(row("Used", format::bytes(pt.used)))
            .child(row("Not scanned", format::bytes(pt.used - pt.scanned)))
            .child(row("Scanned files", format::bytes(pt.scanned)))
            .children(notes.into_iter().map(|n| div().mt(px(2.)).text_color(p.faint).child(n)));
        Some(
            deferred(anchored().position(pos).anchor(anchor).snap_to_window_with_margin(px(8.)).child(card))
                .with_priority(1)
                .into_any_element(),
        )
    }
}

/// Hitboxes inserted in prepaint, in z-order.
struct Prepared {
    plot: Rc<Plot>,
    area: Hitbox,
    scrub: Option<Hitbox>,
    dots: Vec<Hitbox>,
    target: Option<Hitbox>,
}

fn to_bounds(origin: Point<Pixels>, r: Rect) -> Bounds<Pixels> {
    Bounds::new(point(origin.x + px(r.x), origin.y + px(r.y)), size(px(r.w), px(r.h)))
}

impl Render for GraphView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = cx.palette();
        let series = Series::from_state(self.state.read(cx));
        if series.pts.is_empty() {
            return div()
                .h(px(HEIGHT))
                .w_full()
                .flex()
                .items_center()
                .justify_center()
                .text_size(px(12.))
                .text_color(p.dim)
                .child("No scans yet — run one to start a history.")
                .into_any_element();
        }

        let card = self.hover_card(&series, &p);
        let series = Rc::new(series);
        let view = cx.entity();
        let state = self.state.clone();
        let bounds_cell = self.bounds.clone();
        let hover = self.hover;
        let dragging = self.dragging;
        let target_ix = hover.target.and_then(|id| series.pts.iter().position(|x| x.id == id));

        let prepaint = {
            let series = series.clone();
            move |bounds: Bounds<Pixels>, window: &mut Window, _: &mut App| {
                bounds_cell.set(Some(bounds));
                let plot = Rc::new(Plot::new(&series, f32::from(bounds.size.width), f32::from(bounds.size.height)));
                let o = bounds.origin;
                let area = window.insert_hitbox(bounds, HitboxBehavior::Normal);
                let scrub = plot.scrub_rect().map(|r| window.insert_hitbox(to_bounds(o, r), HitboxBehavior::Normal));
                let dots = (0..plot.xs.len())
                    .map(|i| {
                        let r = Rect::around(plot.xs[i], plot.ys[i], plot.dot_r(i).max(10.));
                        window.insert_hitbox(to_bounds(o, r), HitboxBehavior::Normal)
                    })
                    .collect();
                let target = target_ix.map(|i| window.insert_hitbox(to_bounds(o, plot.triangle(i).1), HitboxBehavior::Normal));
                Prepared { plot, area, scrub, dots, target }
            }
        };

        let paint = move |bounds: Bounds<Pixels>, prep: Prepared, window: &mut Window, cx: &mut App| {
            let o = bounds.origin;
            let plot = prep.plot.clone();
            paint_graph(&series, &plot, hover, target_ix, o, &p, window, cx);

            // Cursors: later requests win where hitboxes overlap.
            if dragging {
                window.set_window_cursor_style(CursorStyle::ClosedHand);
            } else {
                window.set_cursor_style(CursorStyle::Arrow, &prep.area);
                if let Some(hb) = &prep.scrub {
                    window.set_cursor_style(CursorStyle::ResizeLeftRight, hb);
                }
                for hb in &prep.dots {
                    window.set_cursor_style(CursorStyle::Arrow, hb);
                }
                if let Some(hb) = &prep.target {
                    window.set_cursor_style(CursorStyle::Arrow, hb);
                }
            }

            let local = move |pos: Point<Pixels>| (f32::from(pos.x - o.x), f32::from(pos.y - o.y));
            let ids: Rc<Vec<i64>> = Rc::new(series.pts.iter().map(|x| x.id).collect());

            // Press: dot → current; triangle → compare; elsewhere → scrub.
            {
                let (plot, ids, view, state, area) = (plot.clone(), ids.clone(), view.clone(), state.clone(), prep.area.clone());
                window.on_mouse_event(move |ev: &MouseDownEvent, phase, window, cx| {
                    if phase != DispatchPhase::Bubble || ev.button != MouseButton::Left || !area.is_hovered(window) {
                        return;
                    }
                    cx.stop_propagation();
                    let (x, y) = local(ev.position);
                    let target = view.read(cx).hover.target.and_then(|id| ids.iter().position(|&x| x == id));
                    let on_dot = plot.dot_at(x, y, |i| plot.dot_r(i) + 2.);
                    let on_target = target.filter(|&t| plot.triangle(t).1.contains(x, y));
                    let near_dot = plot.dot_at(x, y, |_| HIT_R);
                    if let Some(i) = on_dot.or(on_target.is_none().then_some(near_dot).flatten()) {
                        let id = ids[i];
                        state.update(cx, |s, cx| s.set_current(id, cx));
                    } else if let Some(t) = on_target {
                        let id = ids[t];
                        state.update(cx, |s, cx| s.set_compare(Some(id), cx));
                        view.update(cx, |v, cx| v.set_hover(Hover::default(), cx));
                    } else if let Some(i) = plot.snap(x) {
                        window.capture_pointer(area.id);
                        let id = ids[i];
                        view.update(cx, |v, cx| v.set_dragging(true, cx));
                        state.update(cx, |s, cx| s.set_compare(Some(id), cx));
                    }
                });
            }

            // Move: re-snap while dragging, otherwise track hover.
            {
                let (plot, ids, view, state, area) = (plot.clone(), ids.clone(), view.clone(), state.clone(), prep.area.clone());
                window.on_mouse_event(move |ev: &MouseMoveEvent, phase, window, cx| {
                    if phase != DispatchPhase::Bubble {
                        return;
                    }
                    let (x, y) = local(ev.position);
                    if view.read(cx).dragging {
                        if ev.pressed_button != Some(MouseButton::Left) {
                            view.update(cx, |v, cx| v.set_dragging(false, cx));
                        } else if let Some(i) = plot.snap(x) {
                            let id = ids[i];
                            state.update(cx, |s, cx| s.set_compare(Some(id), cx));
                        }
                        return;
                    }
                    let hover = if area.is_hovered(window) {
                        let old = view.read(cx).hover.target.and_then(|id| ids.iter().position(|&x| x == id));
                        // Keep the triangle while the pointer travels to it.
                        let sticky = old.filter(|&t| Some(t) != plot.cmp && plot.triangle(t).1.contains(x, y));
                        let target = sticky.or_else(|| plot.target_near(x, y));
                        let hot = target.is_some_and(|t| plot.triangle(t).1.contains(x, y));
                        let dot = if hot { None } else { plot.dot_at(x, y, |_| HIT_R) };
                        Hover { dot: dot.map(|i| ids[i]), target: target.map(|i| ids[i]), target_hot: hot }
                    } else {
                        Hover::default()
                    };
                    view.update(cx, |v, cx| v.set_hover(hover, cx));
                });
            }

            {
                let view = view.clone();
                window.on_mouse_event(move |ev: &MouseUpEvent, phase, _, cx| {
                    if phase == DispatchPhase::Bubble && ev.button == MouseButton::Left && view.read(cx).dragging {
                        view.update(cx, |v, cx| v.set_dragging(false, cx));
                    }
                });
            }
            {
                let view = view.clone();
                window.on_mouse_event(move |_: &MouseExitEvent, phase, _, cx| {
                    if phase == DispatchPhase::Bubble {
                        view.update(cx, |v, cx| v.set_hover(Hover::default(), cx));
                    }
                });
            }
        };

        div()
            .h(px(HEIGHT))
            .w_full()
            .relative()
            .child(canvas(prepaint, paint).size_full())
            .children(card)
            .into_any_element()
    }
}

// ---- painting -----------------------------------------------------------

struct Text {
    line: ShapedLine,
    w: f32,
}

fn shape(window: &mut Window, text: &str, size: f32, weight: FontWeight, color: Hsla) -> Text {
    let mut font = window.text_style().font();
    font.weight = weight;
    font.features = FontFeatures(Arc::new(vec![("tnum".into(), 1)]));
    let run = TextRun { len: text.len(), font, color, background_color: None, underline: None, strikethrough: None };
    let line = window.text_system().shape_line(SharedString::from(text.to_string()), px(size), &[run], None);
    let w = f32::from(line.width());
    Text { line, w }
}

fn draw_text(t: &Text, o: Point<Pixels>, x: f32, y: f32, lh: f32, window: &mut Window, cx: &mut App) {
    let _ = t.line.paint(point(o.x + px(x), o.y + px(y)), px(lh), TextAlign::Left, None, window, cx);
}

fn pt(o: Point<Pixels>, x: f32, y: f32) -> Point<Pixels> {
    point(o.x + px(x), o.y + px(y))
}

fn circle(window: &mut Window, o: Point<Pixels>, x: f32, y: f32, r: f32, fill: Hsla, stroke: Hsla, sw: f32, dashed: bool) {
    // Quads draw borders inside their bounds; grow by half the stroke so the
    // stroke is centered on the radius, as in SVG.
    let rr = r + sw / 2.;
    let style = if dashed { BorderStyle::Dashed } else { BorderStyle::Solid };
    window.paint_quad(quad(to_bounds(o, Rect::around(x, y, rr)), px(rr), fill, px(sw), stroke, style));
}

fn polygon(window: &mut Window, o: Point<Pixels>, pts: &[(f32, f32)], fill: Hsla) {
    let mut b = PathBuilder::fill();
    let points: Vec<Point<Pixels>> = pts.iter().map(|&(x, y)| pt(o, x, y)).collect();
    b.add_polygon(&points, true);
    if let Ok(path) = b.build() {
        window.paint_path(path, fill);
    }
}

#[allow(clippy::too_many_arguments)]
fn paint_graph(
    series: &Series,
    plot: &Plot,
    hover: Hover,
    target_ix: Option<usize>,
    o: Point<Pixels>,
    p: &Palette,
    window: &mut Window,
    cx: &mut App,
) {
    let n = plot.xs.len();
    let bottom = plot.bottom();

    // Gridlines and y labels; the areas cover the gridlines below them.
    for &(y, v) in &plot.ticks {
        window.paint_quad(fill(to_bounds(o, Rect::new(plot.left, y.round() - 0.5, plot.iw, 1.)), p.grid));
        let t = shape(window, &format::bytes(v), TICK_FONT, FontWeight::NORMAL, p.dim);
        draw_text(&t, o, plot.left - 8. - t.w, y - LH / 2., LH, window, cx);
    }

    // The band between the compare and current scans.
    if let (Some(c), Some(k)) = (plot.cmp, plot.cur) {
        let (l, r) = (plot.xs[c].min(plot.xs[k]), plot.xs[c].max(plot.xs[k]));
        let tint = p.accent.opacity(if p.dark { 0.14 } else { 0.08 });
        let (y, h) = plot.band();
        window.paint_quad(fill(to_bounds(o, Rect::new(l, y, r - l, h)), tint));
    }

    if n >= 2 {
        // Scanned files, up from the bottom, or down from the top for free
        // space.
        let base = if plot.free { plot.top } else { bottom };
        let mut pts: Vec<(f32, f32)> = plot.xs.iter().copied().zip(plot.ys_scanned.iter().copied()).collect();
        pts.push((plot.xs[n - 1], base));
        pts.push((plot.xs[0], base));
        polygon(window, o, &pts, p.scanned);

        // Not scanned: a tint and stripes between the two tops.
        let mut pts: Vec<(f32, f32)> = plot.xs.iter().copied().zip(plot.ys.iter().copied()).collect();
        pts.extend(plot.xs.iter().copied().zip(plot.ys_scanned.iter().copied()).rev());
        polygon(window, o, &pts, p.outside_bg);
        let mut b = PathBuilder::stroke(px(2.));
        for (a, z) in outside_stripes(plot) {
            b.move_to(pt(o, a.0, a.1));
            b.line_to(pt(o, z.0, z.1));
        }
        if let Ok(path) = b.build() {
            window.paint_path(path, p.outside);
        }
    }
    if n >= 2 {
        // The top: used space, or free space.
        let mut b = PathBuilder::stroke(px(1.5));
        b.move_to(pt(o, plot.xs[0], plot.ys[0]));
        for i in 1..n {
            b.line_to(pt(o, plot.xs[i], plot.ys[i]));
        }
        if let Ok(path) = b.build() {
            window.paint_path(path, p.accent);
        }
    }

    // Disk size.
    let caps: Vec<(f32, f32)> = (0..n).filter_map(|i| plot.ys_cap[i].map(|y| (plot.xs[i], y))).collect();
    if let (Some(first), Some(last)) = (caps.first(), caps.last()) {
        let (x0, x1) = if caps.len() == 1 { (plot.left, plot.left + plot.iw) } else { (first.0, last.0) };
        let mut b = PathBuilder::stroke(px(1.)).dash_array(&[px(3.), px(3.)]);
        b.move_to(pt(o, x0, first.1));
        for &(x, y) in caps.iter().skip(1) {
            b.line_to(pt(o, x, y));
        }
        if caps.len() == 1 {
            b.line_to(pt(o, x1, first.1));
        }
        if let Ok(path) = b.build() {
            window.paint_path(path, p.dim);
        }
        let disk = series.pts.iter().rev().find_map(|x| x.free.map(|f| x.used + f)).unwrap_or(0);
        let t = shape(window, &format!("{} disk", format::bytes(disk)), TICK_FONT, FontWeight::NORMAL, p.dim);
        draw_text(&t, o, x1 - t.w, last.1 - 3. - LH, LH, window, cx);
    }

    // The cut in the stack: at the bottom, or the top for free space.
    if plot.broken && n >= 2 {
        let x = plot.left + X_INSET + 12.;
        let (y0, y1) = if plot.free { (plot.top - 1., plot.top + 14.) } else { (bottom - 14., bottom + 1.) };
        polygon(window, o, &[(x + 4., y0), (x + 8., y0), (x + 1., y1), (x - 3., y1)], p.elevated);
    }

    // Month labels.
    let keys: Vec<(i32, u32)> = series.pts.iter().map(|x| month_key(x.at)).collect();
    let months = month_labels(&plot.xs, &keys);
    let my = plot.months_y();
    for (k, &i) in months.iter().enumerate() {
        let x = plot.xs[i];
        window.paint_quad(fill(to_bounds(o, Rect::new(x.round() - 0.5, my + 3., 1., 4.)), p.grid.opacity(2.)));
        let t = shape(window, &month_label(series.pts[i].at), TICK_FONT, FontWeight::NORMAL, p.dim);
        let lx = if n > 1 && k == 0 && i == 0 {
            x
        } else if n > 1 && i == n - 1 {
            x - t.w
        } else {
            x - t.w / 2.
        };
        draw_text(&t, o, lx, my + 18. - 9., LH, window, cx);
    }

    // The compare edge of the band, which drags.
    if let Some(c) = plot.cmp {
        let x = plot.xs[c].round() - 0.5;
        let (y, h) = plot.band();
        window.paint_quad(fill(to_bounds(o, Rect::new(x, y, 1., h)), p.accent.opacity(0.45)));
    }

    // Dots on the used line.
    for i in 0..n {
        let (x, y) = (plot.xs[i], plot.ys[i]);
        let r = plot.dot_r(i);
        let temp = series.pts[i].temporary;
        let hovered = hover.dot == Some(series.pts[i].id);
        if Some(i) == plot.cur {
            circle(window, o, x, y, r, p.accent, p.elevated, 2., temp);
        } else if Some(i) == plot.cmp {
            circle(window, o, x, y, r, p.elevated, p.accent, 2., false);
        } else {
            let fill = if hovered { p.accent } else { p.elevated };
            circle(window, o, x, y, r, fill, p.accent, 1.5, temp);
        }
    }

    // The change in used (or free) space, at the band's open end: on top,
    // or under it for free space. More free space is good news, so its
    // colors are flipped.
    if let (Some(c), Some(k)) = (plot.cmp, plot.cur) {
        let value = |i: usize| if plot.free { series.pts[i].free.unwrap_or(0) } else { series.pts[i].used } as i64;
        let d = value(k) - value(c);
        let t = shape(window, &format::delta(d), PILL_FONT, FontWeight::SEMIBOLD, p.on_selected);
        let w = t.w + 2. * PILL_PX;
        let mid = (plot.xs[c] + plot.xs[k]) / 2.;
        let x = (mid - w / 2.).clamp(2., (plot.w - w - 2.).max(2.));
        let y = plot.pill_y();
        let color = if d == 0 {
            p.dim
        } else {
            p.delta(if plot.free { -d } else { d }, false)
        };
        window.paint_quad(quad(to_bounds(o, Rect::new(x, y, w, PILL_H)), px(PILL_H / 2.), color, px(0.), transparent_black(), BorderStyle::Solid));
        draw_text(&t, o, x + PILL_PX, y + (PILL_H - LH) / 2., LH, window, cx);
    }

    // Compare-target triangle.
    if let Some(t) = target_ix {
        let (tri, _) = plot.triangle(t);
        polygon(window, o, &tri, if hover.target_hot { p.accent } else { p.elevated });
        let mut b = PathBuilder::stroke(px(2.));
        let pts: Vec<Point<Pixels>> = tri.iter().map(|&(x, y)| pt(o, x, y)).collect();
        b.add_polygon(&pts, true);
        if let Ok(path) = b.build() {
            window.paint_path(path, p.accent);
        }
    }
}

/// Diagonal stripe segments clipped to the unscanned area, in phase
/// across the whole plot. Each span between two scans is a quad with
/// vertical sides, so each stripe (`x + y = k`) is clipped in x against its
/// straight top and bottom edges.
fn outside_stripes(plot: &Plot) -> Vec<((f32, f32), (f32, f32))> {
    let mut out = Vec::new();
    for i in 1..plot.xs.len() {
        let (xa, xb) = (plot.xs[i - 1], plot.xs[i]);
        if xb - xa < 0.5 {
            continue;
        }
        let slope = |ya: f32, yb: f32| (yb - ya) / (xb - xa);
        // For free space the scanned area is above the line.
        let (top, bot) = if plot.free { (&plot.ys_scanned, &plot.ys) } else { (&plot.ys, &plot.ys_scanned) };
        let (ta, st) = (top[i - 1], slope(top[i - 1], top[i]));
        let (ba, sb) = (bot[i - 1], slope(bot[i - 1], bot[i]));
        let (tb, bb) = (top[i], bot[i]);
        let corners = [xa + ta, xa + ba, xb + tb, xb + bb];
        let k_min = corners.iter().copied().fold(f32::MAX, f32::min).div_euclid(STRIPE_GAP) * STRIPE_GAP;
        let k_max = corners.iter().copied().fold(f32::MIN, f32::max);
        let mut k = k_min;
        while k <= k_max {
            // y = k − x must satisfy top(x) ≤ y ≤ bottom(x).
            let (mut lo, mut hi) = (xa, xb);
            for (y0, s, below) in [(ta, st, true), (ba, sb, false)] {
                // k − x − (y0 + s (x − xa)) ≥ 0 for the top, ≤ 0 for the bottom.
                let c = k - y0 + s * xa;
                let m = 1. + s;
                if m.abs() < 1e-6 {
                    if (below && c < 0.) || (!below && c > 0.) {
                        hi = lo - 1.;
                    }
                } else {
                    let x = c / m;
                    if below == (m > 0.) {
                        hi = hi.min(x);
                    } else {
                        lo = lo.max(x);
                    }
                }
            }
            if hi - lo > 0.1 {
                out.push(((lo, k - lo), (hi, k - hi)));
            }
            k += STRIPE_GAP;
        }
    }
    out
}

/// ("Oct", 2026) style key for grouping scans by calendar month.
fn month_key(unix: i64) -> (i32, u32) {
    let s = format::long_date(unix);
    let mut it = s.split_whitespace();
    let mon = it.next().unwrap_or("");
    let year = it.nth(1).and_then(|y| y.parse().ok()).unwrap_or(0);
    const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    (year, MONTHS.iter().position(|m| *m == mon).map_or(0, |i| i as u32 + 1))
}

/// "Oct 2026".
fn month_label(unix: i64) -> String {
    let s = format::long_date(unix);
    let mut it = s.split_whitespace();
    let mon = it.next().unwrap_or("");
    let year = it.nth(1).unwrap_or("");
    format!("{mon} {year}")
}

#[cfg(test)]
mod tests {
    use super::*;
    // `gpui_kit::*` brings its own `test` macro; use the standard one.
    use core::prelude::v1::test;

    fn series(n: usize, cur: Option<usize>, cmp: Option<usize>) -> Series {
        let pts = (0..n)
            .map(|i| SeriesPt {
                id: i as i64 + 1,
                used: 300_000_000_000 + i as u64 * 5_000_000_000,
                free: Some(150_000_000_000 - i as u64 * 5_000_000_000),
                scanned: 250_000_000_000 + (i as u64 % 3) * 4_000_000_000,
                temporary: false,
                at: 1_790_000_000 + i as i64 * 86_400 * 4,
                full_access: true,
                unreadable: 0,
            })
            .collect();
        Series { pts, cur, cmp, free: false }
    }

    #[test]
    fn evenly_spaced_without_selection() {
        let xs = x_positions(5, 400., None, None);
        assert_eq!(xs, vec![0., 100., 200., 300., 400.]);
        assert_eq!(x_positions(1, 400., Some(0), None), vec![200.]);
    }

    #[test]
    fn adjacent_selection_is_spread_to_min_gap() {
        // 21 points over 600 px: natural gap 30 px.
        let xs = x_positions(21, 600., Some(10), Some(9));
        assert!((xs[10] - xs[9] - MIN_PAIR_GAP).abs() < 0.01);
        // Centered on the natural midpoint.
        assert!(((xs[9] + xs[10]) / 2. - 285.).abs() < 0.01);
        // Order preserved, ends fixed.
        assert!(xs.windows(2).all(|w| w[0] <= w[1]));
        assert_eq!(xs[0], 0.);
        assert!((xs[20] - 600.).abs() < 0.01);
    }

    #[test]
    fn spread_clamps_at_edges() {
        let xs = x_positions(21, 600., Some(0), Some(1));
        assert_eq!(xs[0], 0.);
        assert!((xs[1] - MIN_PAIR_GAP).abs() < 0.01);
        assert!(xs.windows(2).all(|w| w[0] <= w[1]));
        let xs = x_positions(21, 600., Some(20), Some(19));
        assert!((xs[20] - 600.).abs() < 0.01);
        assert!((xs[20] - xs[19] - MIN_PAIR_GAP).abs() < 0.01);
        // Already far apart: untouched.
        assert_eq!(x_positions(5, 400., Some(0), Some(4)), x_positions(5, 400., None, None));
        // Narrower than the gap: the pair spans the whole width.
        let xs = x_positions(3, 80., Some(0), Some(1));
        assert_eq!((xs[0], xs[1]), (0., 80.));
    }

    #[test]
    fn points_beyond_a_spread_pair_never_pile_on_the_edge() {
        // 50 scans over 1900 px, the pair second and third from the end (or
        // the start): the scan beyond it keeps its own spot.
        let xs = x_positions(50, 1900., Some(48), Some(47));
        assert!((xs[48] - xs[47] - MIN_PAIR_GAP).abs() < 0.01);
        assert!(xs[49] - xs[48] > 20., "{} vs {}", xs[48], xs[49]);
        assert!((xs[49] - 1900.).abs() < 0.01);
        let xs = x_positions(50, 1900., Some(1), Some(2));
        assert!(xs[1] > 20.);
        assert!((xs[2] - xs[1] - MIN_PAIR_GAP).abs() < 0.01);
        for xs in [x_positions(50, 1900., Some(48), Some(47)), x_positions(50, 1900., Some(1), Some(2))] {
            assert!(xs.windows(2).all(|w| w[1] > w[0]));
        }
    }

    #[test]
    fn scrub_snaps_by_x_and_never_onto_current() {
        let s = series(5, Some(4), Some(3));
        let plot = Plot::new(&s, 600., HEIGHT);
        let x4 = plot.xs[4];
        // Pointer right on the current dot snaps to its nearest other scan.
        assert_eq!(plot.snap(x4), Some(3));
        assert_eq!(plot.snap(plot.xs[1] + 3.), Some(1));
        assert_eq!(plot.snap(-50.), Some(0));
    }

    #[test]
    fn temporary_scans_are_not_candidates() {
        let mut s = series(4, Some(3), None);
        s.pts[1].temporary = true;
        assert_eq!(s.candidates(), vec![0, 2]);
        s.cur = Some(1);
        assert!(s.candidates().is_empty(), "a temporary current scan cannot compare");
    }

    #[test]
    fn target_appears_near_dots_except_compare() {
        let s = series(5, Some(4), Some(2));
        let plot = Plot::new(&s, 600., HEIGHT);
        assert_eq!(plot.target_near(plot.xs[1] + 5., plot.ys[1] - 10.), Some(1));
        assert_eq!(plot.target_near(plot.xs[2], plot.ys[2]), None, "compare already selected");
        assert_eq!(plot.target_near(plot.xs[4], plot.ys[4]), None, "current excluded");
        assert_eq!(plot.target_near(plot.xs[1], plot.ys[1] - 40.), None);
        // The triangle click area sits above the dot, not on it.
        let (_, hit) = plot.triangle(1);
        assert!(hit.bottom() < plot.ys[1] - DOT_R);
        assert!(hit.contains(plot.xs[1], plot.ys[1] - DOT_R - 10.));
        // For free space the stack is above the line, so the triangle goes
        // below the dot, pointing up at it.
        let mut s = s;
        s.free = true;
        let plot = Plot::new(&s, 600., HEIGHT);
        let (tri, hit) = plot.triangle(1);
        assert!(tri.iter().all(|&(_, y)| y > plot.ys[1] + DOT_R));
        assert!(tri[2].1 < tri[0].1, "points up");
        assert!(hit.y > plot.ys[1] + DOT_R);
        assert!(hit.contains(plot.xs[1], plot.ys[1] + DOT_R + 10.));
        assert_eq!(plot.target_near(plot.xs[1] + 5., plot.ys[1] + 10.), Some(1));
    }

    #[test]
    fn nice_ticks() {
        let (lo, hi) = nice(274e9, 357e9);
        assert_eq!((lo, hi), (270e9, 360e9));
        let t = ticks(lo, hi, 3.);
        assert_eq!(t.first().copied(), Some(280e9));
        assert_eq!(t.last().copied(), Some(360e9));
        // A flat series without disk info gets at least 1 GiB of padding.
        let (lo, hi, _) = y_domain(&[pt(10 * GB, None, 10 * GB)]);
        assert!(lo < 10e9 && hi > 10e9);
    }

    #[test]
    fn month_labels_are_sparse() {
        let xs: Vec<f32> = (0..10).map(|i| i as f32 * 30.).collect();
        let keys = vec![(2026, 9), (2026, 9), (2026, 9), (2026, 10), (2026, 10), (2026, 10), (2026, 10), (2026, 11), (2026, 11), (2026, 11)];
        // First at 0; Oct at 90 (≥ 80); Nov at 210; last at 270 is < 80 after 210 → skipped.
        assert_eq!(month_labels(&xs, &keys), vec![0, 3, 7]);
        // The last scan never repeats the previous label's month.
        let keys = vec![(2026, 9), (2026, 9), (2026, 9), (2026, 10), (2026, 10), (2026, 10), (2026, 10), (2026, 10), (2026, 10), (2026, 10)];
        assert_eq!(month_labels(&xs, &keys), vec![0, 3]);
        let keys = vec![(2026, 9); 10];
        assert_eq!(month_labels(&xs, &keys), vec![0]);
        let keys = vec![(2026, 9), (2026, 9), (2026, 9), (2026, 9), (2026, 9), (2026, 9), (2026, 9), (2026, 9), (2026, 9), (2026, 10)];
        assert_eq!(month_labels(&xs, &keys), vec![0, 9]);
    }

    const GB: u64 = 1_000_000_000;

    fn pt(used: u64, free: Option<u64>, scanned: u64) -> SeriesPt {
        SeriesPt { id: 1, used, free, scanned, temporary: false, at: 0, full_access: true, unreadable: 0 }
    }

    #[test]
    fn domain_shows_the_disk_when_it_is_close() {
        // A 500 GB disk, 300–470 GB used, 250+ GB scanned: the disk line fits
        // and the axis is cut below the scanned area.
        let pts = [pt(300 * GB, Some(200 * GB), 250 * GB), pt(470 * GB, Some(30 * GB), 400 * GB)];
        let (lo, hi, cap) = y_domain(&pts);
        assert!(cap && hi >= 500e9 && lo > 0. && lo < 250e9, "{lo} {hi}");
        // A mostly empty 2 TB disk: the top hugs the data, no disk line.
        let pts = [pt(300 * GB, Some(1700 * GB), 250 * GB), pt(320 * GB, Some(1680 * GB), 260 * GB)];
        let (_, hi, cap) = y_domain(&pts);
        assert!(!cap && hi < 500e9);
        // A folder scan: tiny scanned size, so the axis starts at zero.
        let pts = [pt(300 * GB, Some(200 * GB), GB), pt(310 * GB, Some(190 * GB), 2 * GB)];
        assert_eq!(y_domain(&pts).0, 0.);
    }

    #[test]
    fn free_mode_plots_free_space_without_the_stack() {
        let mut s = series(10, Some(9), Some(4));
        s.free = true;
        let plot = Plot::new(&s, 600., HEIGHT);
        // Free space shrinks over the series, so the line falls (y grows).
        assert!(plot.ys.windows(2).all(|w| w[1] > w[0]));
        // The scanned area hangs above the line, cut below the disk size.
        assert!(plot.ys.iter().zip(&plot.ys_scanned).all(|(l, s)| s < l));
        assert!(plot.ys_scanned.iter().all(|&s| s > plot.top));
        assert!(plot.broken);
        assert!(plot.ys_cap.iter().all(Option::is_none));
        // The pill moves under the band, between the plot and the months,
        // and the plot keeps its height.
        let used = Plot::new(&series(10, Some(9), Some(4)), 600., HEIGHT);
        assert_eq!(plot.ih, used.ih);
        assert!(used.pill_y() + PILL_H < used.band().0);
        let (by, bh) = plot.band();
        assert!(plot.pill_y() > by + bh && plot.pill_y() + PILL_H <= plot.months_y());
        assert!(plot.months_y() + 18. <= HEIGHT);
    }

    #[test]
    fn stripes_stay_between_used_and_scanned() {
        for (w, free) in [(420., false), (900., false), (420., true), (900., true)] {
            let mut s = series(10, Some(9), Some(4));
            s.free = free;
            let plot = Plot::new(&s, w, HEIGHT);
            let (top, bot) = if free { (&plot.ys_scanned, &plot.ys) } else { (&plot.ys, &plot.ys_scanned) };
            let stripes = outside_stripes(&plot);
            assert!(!stripes.is_empty());
            let at = |ys: &[f32], x: f32| {
                let i = plot.xs.windows(2).position(|p| x >= p[0] - 0.01 && x <= p[1] + 0.01).unwrap();
                let t = (x - plot.xs[i]) / (plot.xs[i + 1] - plot.xs[i]);
                ys[i] + (ys[i + 1] - ys[i]) * t
            };
            for &(a, z) in &stripes {
                for (x, y) in [a, z] {
                    assert!(y >= at(top, x) - 0.05 && y <= at(bot, x) + 0.05, "({x}, {y}) leaves the area");
                }
                // Every stripe runs at 45°.
                assert!(((z.0 - a.0) + (z.1 - a.1)).abs() < 0.01);
            }
        }
    }
}
