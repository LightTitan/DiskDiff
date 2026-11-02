//! The disk bar in the comparison summary: the current disk split into
//! scanned files, unscanned space and free space, with each part's
//! change drawn at its end.
//!
//! * A part that shrank ends in a light, dashed green ghost: the space it gave
//!   up, empty now.
//! * A part that grew ends in red with a solid outline: the space it took, in
//!   use now.
//! * Stripes only ever mean "not scanned": its changes are striped,
//!   scanned-file changes are solid.
//! * Each change has its value above it; labels are pushed apart so they
//!   never overlap.
//! * Parts that would take over 40% of the bar are cut short (a diagonal
//!   break) so changes of a few GB on a large disk stay visible. Exact values
//!   are in the tooltips and the legend.
//!
//! Geometry is plain `f32` math ([`layout`], [`place_labels`]) so it can be
//! tested; the canvas only paints it.

use crate::format;
use crate::theme::Palette;
use gpui_kit::*;
use std::cell::Cell;
use std::rc::Rc;

const MIB: i64 = 1_048_576;
/// Drawn width of a part that is cut short.
const STUB: f32 = 56.;
/// Parts larger than this share of the bar are cut short.
const BREAK_SHARE: f64 = 0.4;
/// Narrowest a change is drawn, so a small one is still seen.
const MIN_CHANGE_W: f32 = 3.;
const LABEL_H: f32 = 15.;
const LABEL_GAP: f32 = 10.;
const BAR_TOP: f32 = LABEL_H + 3.;
const BAR_H: f32 = 16.;
pub const HEIGHT: f32 = BAR_TOP + BAR_H;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Part {
    Scanned,
    Outside,
    Free,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fill {
    /// Present in both scans.
    Solid,
    /// Given up since the earlier scan.
    Freed,
    /// Taken since the earlier scan.
    Grown,
}

/// Before and now, in bytes. `outside` may be negative when the scan counts
/// more than the disk reports in use (hard links, clones); it is drawn as 0.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Figures {
    pub scanned: (i64, i64),
    pub outside: (i64, i64),
    pub free: (i64, i64),
    /// Disk size now.
    pub total: i64,
}

impl Figures {
    fn change(&self, part: Part) -> i64 {
        let (b, n) = match part {
            Part::Scanned => self.scanned,
            Part::Outside => self.outside,
            Part::Free => self.free,
        };
        n - b
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Piece {
    pub part: Part,
    pub fill: Fill,
    pub bytes: i64,
    pub x: f32,
    pub w: f32,
    /// Cut short: drawn [`STUB`] wide with a break mark.
    pub broken: bool,
}

/// Left to right: scanned, its change, outside, its change, free.
pub fn layout(f: &Figures, width: f32) -> Vec<Piece> {
    let mut raw: Vec<(Part, Fill, i64)> = Vec::new();
    let mut taken = 0;
    for (part, (b, n)) in [(Part::Scanned, f.scanned), (Part::Outside, f.outside)] {
        let (b, n) = (b.max(0), n.max(0));
        raw.push((part, Fill::Solid, b.min(n)));
        let d = n - b;
        if d.abs() >= MIB {
            raw.push((part, if d < 0 { Fill::Freed } else { Fill::Grown }, d.abs()));
        }
        taken += b.max(n);
    }
    raw.push((Part::Free, Fill::Solid, (f.total - taken).max(0)));
    raw.retain(|r| r.2 > 0);

    let sum: i64 = raw.iter().map(|r| r.2).sum();
    if sum == 0 || width <= 0. {
        return Vec::new();
    }
    let stub = STUB.min(width / 5.);
    let broken: Vec<bool> = raw.iter().map(|r| r.1 == Fill::Solid && r.2 as f64 > sum as f64 * BREAK_SHARE).collect();
    let n_broken = broken.iter().filter(|&&b| b).count() as f32;
    let rest: i64 = raw.iter().zip(&broken).filter(|(_, b)| !**b).map(|(r, _)| r.2).sum();
    let k = if rest > 0 { (width - stub * n_broken).max(0.) / rest as f32 } else { 0. };

    let mut ws: Vec<f32> = raw
        .iter()
        .zip(&broken)
        .map(|(r, &b)| {
            if b {
                stub
            } else if r.1 == Fill::Solid {
                r.2 as f32 * k
            } else {
                (r.2 as f32 * k).max(MIN_CHANGE_W)
            }
        })
        .collect();
    // Take what the minimum widths added from the widest unbroken solid part.
    let over: f32 = ws.iter().sum::<f32>() - width;
    if over > 0.01 {
        if let Some(i) = (0..ws.len()).filter(|&i| !broken[i] && raw[i].1 == Fill::Solid).max_by(|&a, &b| ws[a].total_cmp(&ws[b])) {
            ws[i] = (ws[i] - over).max(0.);
        }
    }

    let mut x = 0.;
    raw.iter()
        .zip(&broken)
        .zip(ws)
        .map(|((&(part, fill, bytes), &broken), w)| {
            let p = Piece { part, fill, bytes, x, w, broken };
            x += w;
            p
        })
        .collect()
}

/// Left edges for labels of the given (center, width), kept inside
/// `0..width` and at least [`LABEL_GAP`] apart where there is room.
pub fn place_labels(labels: &[(f32, f32)], width: f32) -> Vec<f32> {
    let mut lefts: Vec<f32> = labels.iter().map(|&(c, w)| c - w / 2.).collect();
    for i in 0..lefts.len() {
        let min = if i == 0 { 0. } else { lefts[i - 1] + labels[i - 1].1 + LABEL_GAP };
        lefts[i] = lefts[i].max(min);
    }
    for i in (0..lefts.len()).rev() {
        let max = if i + 1 == lefts.len() { width - labels[i].1 } else { lefts[i + 1] - LABEL_GAP - labels[i].1 };
        lefts[i] = lefts[i].min(max).max(0.);
    }
    lefts
}

pub fn part_name(part: Part) -> &'static str {
    match part {
        Part::Scanned => "Scanned files",
        Part::Outside => "Not scanned",
        Part::Free => "Free",
    }
}

/// The part as it reads in a change tooltip: "Freed …" and "… grew". These
/// keep the longer "outside the scan" wording, which reads better in a
/// sentence than the legend's "Not scanned".
fn change_name(part: Part, freed: bool) -> &'static str {
    match (part, freed) {
        (Part::Scanned, true) => "from scanned files",
        (Part::Scanned, false) => "Scanned files",
        (_, true) => "outside the scan",
        (_, false) => "Outside the scan",
    }
}

fn tooltip_text(f: &Figures, piece: &Piece) -> String {
    let (b, n) = match piece.part {
        Part::Scanned => f.scanned,
        Part::Outside => f.outside,
        Part::Free => f.free,
    };
    let size = |v: i64| format::bytes(v.max(0) as u64);
    match piece.fill {
        Fill::Solid if b == n => format!("{} · {}", part_name(piece.part), size(n)),
        Fill::Solid => format!("{} · {} → {}", part_name(piece.part), size(b), size(n)),
        Fill::Freed => format!("Freed {} · {}", change_name(piece.part, true), format::bytes(piece.bytes as u64)),
        Fill::Grown => format!("{} grew · {}", change_name(piece.part, false), format::bytes(piece.bytes as u64)),
    }
}

/// The color a part is drawn in.
pub fn part_color(p: &Palette, part: Part) -> Hsla {
    match part {
        Part::Scanned => p.scanned,
        Part::Outside => p.outside,
        Part::Free => p.column,
    }
}

/// The labels row and the bar. `width` keeps the canvas width from the last
/// paint, which places the tooltip areas.
pub fn disk_bar(f: Figures, width: Rc<Cell<f32>>, p: &Palette) -> AnyElement {
    let p = *p;
    let overlays: Vec<AnyElement> = layout(&f, width.get())
        .into_iter()
        .enumerate()
        .map(|(i, piece)| {
            let tip: SharedString = tooltip_text(&f, &piece).into();
            div()
                .id(("disk-bar", i))
                .absolute()
                .left(px(piece.x))
                .top(px(BAR_TOP))
                .w(px(piece.w))
                .h(px(BAR_H))
                .tooltip(move |window, cx| gpui_kit::component::tooltip::Tooltip::new(tip.clone()).build(window, cx))
                .into_any_element()
        })
        .collect();

    let prepaint = move |bounds: Bounds<Pixels>, window: &mut Window, _: &mut App| {
        let w = f32::from(bounds.size.width);
        if (width.get() - w).abs() > 0.5 {
            width.set(w);
            // Re-render so the tooltip areas match the new width.
            window.request_animation_frame();
        }
    };
    let paint = move |bounds: Bounds<Pixels>, _: (), window: &mut Window, cx: &mut App| {
        paint_bar(&f, bounds, &p, window, cx);
    };
    div()
        .relative()
        .w_full()
        .h(px(HEIGHT))
        .child(canvas(prepaint, paint).size_full())
        .children(overlays)
        .into_any_element()
}

// ---- painting -----------------------------------------------------------

fn rect(o: Point<Pixels>, x: f32, y: f32, w: f32, h: f32) -> Bounds<Pixels> {
    Bounds::new(point(o.x + px(x), o.y + px(y)), size(px(w), px(h)))
}

fn paint_bar(f: &Figures, bounds: Bounds<Pixels>, p: &Palette, window: &mut Window, cx: &mut App) {
    let o = bounds.origin;
    let width = f32::from(bounds.size.width);
    let pieces = layout(f, width);
    let last = pieces.len().saturating_sub(1);
    let (y, h) = (BAR_TOP, BAR_H);
    for (i, pc) in pieces.iter().enumerate() {
        // A hairline gap between pieces.
        let w = if i == last { pc.w } else { pc.w - 1. };
        if w <= 0. {
            continue;
        }
        let r = rect(o, pc.x, y, w, h);
        let corners = Corners {
            top_left: px(if i == 0 { 4. } else { 0. }),
            bottom_left: px(if i == 0 { 4. } else { 0. }),
            top_right: px(if i == last { 4. } else { 0. }),
            bottom_right: px(if i == last { 4. } else { 0. }),
        };
        let color = part_color(p, pc.part);
        match (pc.part, pc.fill) {
            (Part::Free, _) => {
                window.paint_quad(quad(r, corners, p.column, px(1.), p.border, BorderStyle::Solid));
            }
            (Part::Outside, Fill::Solid) => {
                window.paint_quad(quad(r, corners, p.outside_bg, px(0.), transparent_black(), BorderStyle::Solid));
                stripes(window, r, color, 2.);
            }
            (_, Fill::Solid) => {
                window.paint_quad(quad(r, corners, color, px(0.), transparent_black(), BorderStyle::Solid));
            }
            // Stripes mean "not scanned", so scanned-file changes stay solid.
            (Part::Scanned, Fill::Grown) => {
                window.paint_quad(quad(r, corners, p.up, px(0.), transparent_black(), BorderStyle::Solid));
            }
            (Part::Scanned, Fill::Freed) => {
                window.paint_quad(quad(r, corners, p.down.opacity(0.35), px(1.), p.down, BorderStyle::Dashed));
            }
            (_, Fill::Grown) => {
                window.paint_quad(quad(r, corners, p.up.opacity(0.4), px(0.), transparent_black(), BorderStyle::Solid));
                stripes(window, r, p.up.opacity(0.85), 2.);
                window.paint_quad(quad(r, corners, transparent_black(), px(1.), p.up, BorderStyle::Solid));
            }
            (_, Fill::Freed) => {
                window.paint_quad(quad(r, corners, p.column, px(0.), transparent_black(), BorderStyle::Solid));
                stripes(window, r, p.down.opacity(0.55), 1.5);
                window.paint_quad(quad(r, corners, transparent_black(), px(1.), p.down, BorderStyle::Dashed));
            }
        }
        if pc.broken {
            // A diagonal cut in the window color.
            let x = pc.x + (pc.w * 0.4).min(20.);
            let pts = [(x + 3., y - 2.), (x + 7., y - 2.), (x - 1., y + h + 2.), (x - 5., y + h + 2.)];
            let mut b = PathBuilder::fill();
            let pts: Vec<Point<Pixels>> = pts.iter().map(|&(px_, py)| point(o.x + px(px_), o.y + px(py))).collect();
            b.add_polygon(&pts, true);
            if let Ok(path) = b.build() {
                window.paint_path(path, p.elevated);
            }
        }
    }

    // Change labels above their pieces.
    let changes: Vec<&Piece> = pieces.iter().filter(|pc| pc.fill != Fill::Solid).collect();
    let texts: Vec<(ShapedLine, f32)> = changes
        .iter()
        .map(|pc| {
            let d = f.change(pc.part);
            let text = format::delta(d);
            let mut font = window.text_style().font();
            font.weight = FontWeight::SEMIBOLD;
            font.features = FontFeatures(std::sync::Arc::new(vec![("tnum".into(), 1)]));
            let run = TextRun { len: text.len(), font, color: p.delta(d, false), background_color: None, underline: None, strikethrough: None };
            let line = window.text_system().shape_line(text.into(), px(11.), &[run], None);
            let w = f32::from(line.width());
            (line, w)
        })
        .collect();
    let spans: Vec<(f32, f32)> = changes.iter().zip(&texts).map(|(pc, t)| (pc.x + pc.w / 2., t.1)).collect();
    for ((line, _), x) in texts.iter().zip(place_labels(&spans, width)) {
        let _ = line.paint(point(o.x + px(x), o.y), px(LABEL_H), TextAlign::Left, None, window, cx);
    }
}

/// Diagonal stripes clipped to `r`.
fn stripes(window: &mut Window, r: Bounds<Pixels>, color: Hsla, thickness: f32) {
    let h = f32::from(r.size.height);
    let (x0, x1) = (f32::from(r.origin.x), f32::from(r.origin.x + r.size.width));
    let (top, bottom) = (r.origin.y, r.origin.y + r.size.height);
    window.with_content_mask(Some(ContentMask { bounds: r }), |window| {
        let mut b = PathBuilder::stroke(px(thickness));
        let mut x = x0 - h;
        while x < x1 {
            b.move_to(point(px(x), bottom));
            b.line_to(point(px(x + h), top));
            x += 5.;
        }
        if let Ok(path) = b.build() {
            window.paint_path(path, color);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;

    const GB: i64 = 1_000_000_000;

    fn figs(s: (i64, i64), o: (i64, i64), total: i64) -> Figures {
        let free = (total - s.0 - o.0, total - s.1 - o.1);
        Figures { scanned: (s.0 * GB / 100, s.1 * GB / 100), outside: (o.0 * GB / 100, o.1 * GB / 100), free: (free.0 * GB / 100, free.1 * GB / 100), total: total * GB / 100 }
    }

    fn kinds(ps: &[Piece]) -> Vec<(Part, Fill, bool)> {
        ps.iter().map(|p| (p.part, p.fill, p.broken)).collect()
    }

    #[test]
    fn shrink_both() {
        // 404.35 → 402.00 scanned, 64.35 → 58.90 outside, 500 GB disk.
        let ps = layout(&figs((40435, 40200), (6435, 5890), 50000), 500.);
        assert_eq!(
            kinds(&ps),
            vec![
                (Part::Scanned, Fill::Solid, true),
                (Part::Scanned, Fill::Freed, false),
                (Part::Outside, Fill::Solid, false),
                (Part::Outside, Fill::Freed, false),
                (Part::Free, Fill::Solid, false),
            ]
        );
        assert_eq!(ps[0].w, STUB);
        let end = ps.last().map(|p| p.x + p.w).unwrap();
        assert!((end - 500.).abs() < 0.01, "{end}");
        // The freed outside space is wider than the freed scanned space.
        assert!(ps[3].w > ps[1].w * 2.);
    }

    #[test]
    fn growth_and_mixed() {
        let ps = layout(&figs((37100, 37720), (5800, 5840), 50000), 400.);
        assert_eq!(ps[1].fill, Fill::Grown);
        assert_eq!(ps[3].fill, Fill::Grown);
        let ps = layout(&figs((39000, 39310), (6200, 5800), 50000), 400.);
        assert_eq!((ps[1].fill, ps[3].fill), (Fill::Grown, Fill::Freed));
        let end = ps.last().map(|p| p.x + p.w).unwrap();
        assert!((end - 400.).abs() < 0.01);
    }

    #[test]
    fn tiny_changes_are_visible_or_dropped() {
        // 0.3 GB is drawn at least MIN_CHANGE_W wide.
        let ps = layout(&figs((38000, 36760), (6020, 5990), 50000), 400.);
        assert!(ps[3].fill == Fill::Freed && ps[3].w >= MIN_CHANGE_W);
        // Under a MiB there is no change piece.
        let f = Figures { scanned: (100 * GB, 100 * GB + 1000), outside: (10 * GB, 10 * GB), free: (5 * GB, 5 * GB), total: 115 * GB };
        assert!(layout(&f, 300.).iter().all(|p| p.fill == Fill::Solid));
    }

    #[test]
    fn folder_scan_breaks_outside() {
        // A 40 GB folder on a mostly full 500 GB disk: the unscanned part is huge.
        let ps = layout(&figs((4000, 3500), (40000, 40100), 50000), 400.);
        assert!(!ps[0].broken);
        assert!(ps.iter().any(|p| p.part == Part::Outside && p.broken));
    }

    #[test]
    fn negative_outside_is_drawn_as_zero() {
        let f = Figures { scanned: (10 * GB, 11 * GB), outside: (-GB, -2 * GB), free: (5 * GB, 5 * GB), total: 14 * GB };
        assert!(layout(&f, 300.).iter().all(|p| p.part != Part::Outside));
    }

    #[test]
    fn labels_never_overlap() {
        // Two labels centered on the same spot are pushed apart.
        let l = place_labels(&[(100., 50.), (104., 50.)], 400.);
        assert!(l[1] >= l[0] + 50. + LABEL_GAP);
        // Near the right edge both are pulled back inside.
        let l = place_labels(&[(390., 50.), (398., 50.)], 400.);
        assert!(l[1] + 50. <= 400.01 && l[0] + 50. + LABEL_GAP <= l[1] + 0.01);
        // Near the left edge they stay inside.
        let l = place_labels(&[(2., 50.)], 400.);
        assert_eq!(l[0], 0.);
    }
}
