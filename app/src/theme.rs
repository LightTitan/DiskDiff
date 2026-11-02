//! The Soft theme plus DiskDiff's own semantic colors.
//!
//! gpui-component's theme covers controls; [`Palette`] adds what only DiskDiff
//! needs (growth red, shrink green, "new" amber, column and selection shades).
//! Both follow the system light/dark appearance. Colors stay soft: gray
//! surfaces, off-white text, no pure black or white fills.

use gpui_kit::component::{Theme, ThemeMode, ThemeRegistry};
use gpui_kit::*;

#[derive(Clone, Copy, Debug)]
pub struct Palette {
    pub dark: bool,
    /// Window background behind everything.
    pub bg: Hsla,
    /// Title bar, graph band, toolbar, status bar, menus, cards.
    pub elevated: Hsla,
    /// Miller column background.
    pub column: Hsla,
    pub border: Hsla,
    pub text: Hsla,
    pub dim: Hsla,
    /// Even dimmer: placeholders, the "no history" dot.
    pub faint: Hsla,
    pub accent: Hsla,
    pub accent_tint: Hsla,
    pub accent_text: Hsla,
    pub row_hover: Hsla,
    pub row_selected: Hsla,
    /// Selection when the browser does not have keyboard focus (Finder grays it).
    pub row_selected_inactive: Hsla,
    pub row_open: Hsla,
    pub on_selected: Hsla,
    pub up: Hsla,
    pub down: Hsla,
    pub new: Hsla,
    /// Folder activity that dwarfs the net change: the label and the pill
    /// behind it. Violet, so it reads as neither growth nor shrinkage.
    pub churn: Hsla,
    pub churn_bg: Hsla,
    /// Scanned files, in the disk bar and the graph.
    ///
    /// `scanned`, `outside` and `outside_bg` are opaque and chosen per
    /// appearance: scanned files are the accent blue (strong), unscanned
    /// space quieter tints of it, close to the background. In dark mode the
    /// tints keep real colour (in OKLCH, chroma as a share of the most the
    /// lightness allows); dark tints with light mode's raw chroma read as
    /// grey. Translucent fills would land at a different distance on each
    /// background and blur the two together.
    pub scanned: Hsla,
    /// Used space that was not scanned, in the disk bar and the graph: the
    /// stripes and the legend swatch.
    pub outside: Hsla,
    /// The ground under those stripes.
    pub outside_bg: Hsla,
    pub folder: Hsla,
    pub file: Hsla,
    pub control: Hsla,
    pub control_hover: Hsla,
    pub scrollbar: Hsla,
    pub scrollbar_hover: Hsla,
    pub grid: Hsla,
    pub shadow: Hsla,
}

fn c(hex: u32) -> Hsla {
    rgb(hex).into()
}

fn ca(hex: u32, alpha: f32) -> Hsla {
    let mut h: Hsla = rgb(hex).into();
    h.a = alpha;
    h
}

impl Palette {
    pub const DARK: fn() -> Palette = || Palette {
        dark: true,
        bg: c(0x212328),
        elevated: c(0x272a2f),
        column: c(0x1f2126),
        border: c(0x383c43),
        text: c(0xc9cdd3),
        dim: c(0x8a9099),
        faint: c(0x5c616a),
        accent: c(0x4f7dd1),
        accent_tint: ca(0x4f7dd1, 0.20),
        accent_text: c(0xc4d6f5),
        row_hover: c(0x2b2e34),
        row_selected: c(0x3d68b8),
        row_selected_inactive: c(0x3a3e46),
        row_open: ca(0xffffff, 0.07),
        on_selected: c(0xeef2f8),
        up: c(0xd9726f),
        down: c(0x5fb38a),
        new: c(0xd4aa5c),
        churn: c(0xc3a9f0),
        churn_bg: c(0x3d3054),
        scanned: c(0x4f7dd1),
        outside: c(0x355390),
        outside_bg: c(0x273a60),
        folder: c(0x5e93d8),
        file: c(0xa4a9b1),
        control: c(0x2d3036),
        control_hover: c(0x353840),
        scrollbar: ca(0x8a9099, 0.45),
        scrollbar_hover: ca(0x8a9099, 0.75),
        grid: ca(0xffffff, 0.06),
        shadow: ca(0x000000, 0.35),
    };

    pub const LIGHT: fn() -> Palette = || Palette {
        dark: false,
        bg: c(0xf7f8fa),
        elevated: c(0xeef0f3),
        column: c(0xfbfbfc),
        border: c(0xdde0e5),
        text: c(0x2b2f36),
        dim: c(0x6b717c),
        faint: c(0xa3a8b0),
        accent: c(0x3d6dcc),
        accent_tint: ca(0x3d6dcc, 0.14),
        accent_text: c(0x2c56a8),
        row_hover: c(0xeceef1),
        row_selected: c(0x3d6dcc),
        row_selected_inactive: c(0xdcdfe4),
        row_open: ca(0x000000, 0.06),
        on_selected: c(0xffffff),
        up: c(0xc4504c),
        down: c(0x2f9466),
        new: c(0xb7851f),
        churn: c(0x7750b1),
        churn_bg: c(0xece5fb),
        scanned: c(0x396ac6),
        outside: c(0x97b0e2),
        outside_bg: c(0xc8d4eb),
        folder: c(0x4a86d4),
        file: c(0x7c828c),
        control: c(0xffffff),
        control_hover: c(0xf1f3f5),
        scrollbar: ca(0x6b717c, 0.35),
        scrollbar_hover: ca(0x6b717c, 0.65),
        grid: ca(0x000000, 0.06),
        shadow: ca(0x000000, 0.12),
    };

    /// Color for a size change: dim when zero, amber when the item is new,
    /// red when it grew, green when it shrank.
    pub fn delta(&self, delta: i64, is_new: bool) -> Hsla {
        if delta == 0 {
            self.dim
        } else if is_new {
            self.new
        } else if delta > 0 {
            self.up
        } else {
            self.down
        }
    }
}

impl Global for Palette {}

pub trait ActivePalette {
    fn palette(&self) -> Palette;
}

impl ActivePalette for App {
    fn palette(&self) -> Palette {
        *self.global::<Palette>()
    }
}

/// Loads the Soft themes into gpui-component and picks light or dark from the
/// system appearance.
pub fn init(cx: &mut App) {
    ThemeRegistry::global_mut(cx)
        .load_themes_from_str(include_str!("../themes/soft.json"))
        .expect("themes/soft.json is valid");
    let themes = ThemeRegistry::global(cx).themes().clone();
    for name in ["Soft Light", "Soft Dark"] {
        let config = themes[name].clone();
        Theme::global_mut(cx).apply_config(&config);
    }
    cx.set_global(Palette::DARK());
}

pub fn apply_appearance(appearance: WindowAppearance, window: Option<&mut Window>, cx: &mut App) {
    let dark = match std::env::var("DISKDIFF_APPEARANCE").as_deref() {
        Ok("dark") => true,
        Ok("light") => false,
        _ => matches!(appearance, WindowAppearance::Dark | WindowAppearance::VibrantDark),
    };
    Theme::change(if dark { ThemeMode::Dark } else { ThemeMode::Light }, window, cx);
    cx.set_global(if dark { Palette::DARK() } else { Palette::LIGHT() });
}
