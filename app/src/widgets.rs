//! Small building blocks shared by the views.
//!
//! Cursor rule (see CLAUDE.md): controls keep the arrow; only real links get
//! the pointing hand, plus underline on hover. Everything here uses the arrow
//! except [`link`].

use crate::theme::ActivePalette;
use gpui_kit::assets::IconName;
use gpui_kit::*;
use gpui_kit::prelude::FluentBuilder as _;
use std::time::Duration;

pub fn icon(name: IconName, size: f32, color: Hsla) -> Svg {
    svg().path(gpui_kit::assets::IconNamed::path(name)).size(px(size)).flex_shrink_0().text_color(color)
}

/// A square, borderless icon button with a tooltip.
pub fn icon_button(
    id: impl Into<ElementId>,
    name: IconName,
    tooltip: impl Into<SharedString>,
    on_click: impl Fn(&mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let tooltip = tooltip.into();
    IconButton { id: id.into(), name, tooltip, disabled: false, on_click: Box::new(on_click) }
}

pub fn icon_button_disabled(
    id: impl Into<ElementId>,
    name: IconName,
    tooltip: impl Into<SharedString>,
    disabled: bool,
    on_click: impl Fn(&mut Window, &mut App) + 'static,
) -> impl IntoElement {
    IconButton { id: id.into(), name, tooltip: tooltip.into(), disabled, on_click: Box::new(on_click) }
}

#[derive(IntoElement)]
struct IconButton {
    id: ElementId,
    name: IconName,
    tooltip: SharedString,
    disabled: bool,
    on_click: Box<dyn Fn(&mut Window, &mut App)>,
}

impl RenderOnce for IconButton {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let p = cx.palette();
        let tooltip = self.tooltip.clone();
        let on_click = self.on_click;
        div()
            .id(self.id)
            .size(px(26.))
            .flex_shrink_0()
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(6.))
            .cursor_default()
            .when(self.disabled, |el| el.opacity(0.4))
            .when(!self.disabled, |el| {
                el.hover(|s| s.bg(p.control_hover)).on_click(move |_, window, cx| {
                    cx.stop_propagation();
                    on_click(window, cx)
                })
            })
            .tooltip(move |window, cx| gpui_kit::component::tooltip::Tooltip::new(tooltip.clone()).build(window, cx))
            .child(icon(self.name, 15., p.dim))
    }
}

/// A compact segmented control. `enabled[i] == false` greys out segment i.
pub fn segmented(
    id: &'static str,
    labels: &[&'static str],
    selected: usize,
    enabled: &[bool],
    on_select: impl Fn(usize, &mut App) + 'static,
) -> impl IntoElement {
    let on_select = std::rc::Rc::new(on_select);
    let labels: Vec<&'static str> = labels.to_vec();
    let enabled = enabled.to_vec();
    Segmented { id, labels, selected, enabled, on_select }
}

#[derive(IntoElement)]
struct Segmented {
    id: &'static str,
    labels: Vec<&'static str>,
    selected: usize,
    enabled: Vec<bool>,
    on_select: std::rc::Rc<dyn Fn(usize, &mut App)>,
}

impl RenderOnce for Segmented {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let p = cx.palette();
        div()
            .h(px(24.))
            .flex()
            .items_center()
            .p(px(2.))
            .gap(px(2.))
            .rounded(px(7.))
            .bg(p.control)
            .border_1()
            .border_color(p.border)
            .children(self.labels.iter().enumerate().map(|(i, label)| {
                let on = i == self.selected;
                let enabled = self.enabled.get(i).copied().unwrap_or(true);
                let f = self.on_select.clone();
                div()
                    .id(SharedString::from(format!("{}-{i}", self.id)))
                    .h_full()
                    .px(px(9.))
                    .flex()
                    .items_center()
                    .rounded(px(5.))
                    .text_size(px(11.))
                    .cursor_default()
                    .when(on, |el| el.bg(p.accent_tint).text_color(p.accent_text).font_weight(FontWeight::SEMIBOLD))
                    .when(!on, |el| el.text_color(p.dim))
                    .when(!enabled, |el| el.opacity(0.4))
                    .when(enabled && !on, |el| el.hover(|s| s.bg(p.control_hover).text_color(p.text)))
                    .when(enabled, |el| el.on_click(move |_, _, cx| f(i, cx)))
                    .child(*label)
            }))
    }
}

/// A text link: pointing hand and underline on hover.
pub fn link(id: impl Into<ElementId>, text: impl Into<SharedString>, color: Hsla, on_click: impl Fn(&mut Window, &mut App) + 'static) -> Stateful<Div> {
    div()
        .id(id)
        .cursor_pointer()
        .text_color(color)
        .overflow_hidden()
        .text_ellipsis()
        .whitespace_nowrap()
        .hover(|s| s.underline())
        .on_click(move |_, window, cx| on_click(window, cx))
        .child(text.into())
}

/// A small spinning ring.
pub fn spinner(color: Hsla, size: f32) -> impl IntoElement {
    svg()
        .path(gpui_kit::assets::IconNamed::path(IconName::LoaderCircle))
        .size(px(size))
        .flex_shrink_0()
        .text_color(color)
        .with_animation(
            "spin",
            Animation::new(Duration::from_millis(800)).repeat(),
            |svg, t| svg.with_transformation(Transformation::rotate(percentage(t))),
        )
}

/// Keeps the end of a long string ("…/Library/Caches/foo"), so the deepest
/// part of a path stays visible.
pub fn truncate_start(s: String, max_chars: usize) -> String {
    let n = s.chars().count();
    if n <= max_chars {
        return s;
    }
    let tail: String = s.chars().skip(n - max_chars + 1).collect();
    format!("…{tail}")
}
