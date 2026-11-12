//! Light/dark palettes resolved from the OS appearance. One global —
//! the app is single-window, so the current window's appearance is
//! the theme.

use gpui::{Hsla, WindowAppearance, hsla, rgb, rgba};

#[derive(Clone, Copy)]
pub(crate) struct Theme {
    pub bg: Hsla,
    pub panel: Hsla,
    pub panel2: Hsla,
    pub border: Hsla,
    pub text: Hsla,
    pub dim: Hsla,
    /// Secondary button text — slightly brighter than `dim`.
    pub ghost: Hsla,
    pub accent: Hsla,
    /// Primary button hover.
    pub accent_hover: Hsla,
    /// Text drawn on top of the accent color.
    pub on_accent: Hsla,
    /// Selected row/cell background.
    pub selected: Hsla,
    /// Faint accent fill (grid-cell folder tile, selection tint).
    pub accent_soft: Hsla,
    pub danger: Hsla,
    /// Danger hover wash.
    pub danger_soft: Hsla,
    pub ok: Hsla,
    /// Inert icon color for plain files.
    pub file_icon: Hsla,
    /// Text input background — darker than panels in dark mode.
    pub field: Hsla,
}

fn c(v: u32) -> Hsla {
    rgb(v).into()
}

/// One Dark — the palette the app shipped with.
fn dark() -> Theme {
    Theme {
        bg: c(0x282c34),
        panel: c(0x21252b),
        panel2: c(0x2c313a),
        border: c(0x3e4451),
        text: c(0xabb2bf),
        dim: c(0x5c6370),
        ghost: c(0x9ea8ba),
        accent: c(0x61afef),
        accent_hover: c(0x7db9f2),
        on_accent: c(0x1e222a),
        selected: c(0x3e4452),
        accent_soft: hsla(207. / 360., 0.82, 0.66, 0.13),
        danger: c(0xe06c75),
        danger_soft: rgba(0xe06c751f).into(),
        ok: c(0x98c379),
        file_icon: c(0x4d5666),
        field: c(0x1e222a),
    }
}

/// One Light — the matching light variant.
fn light() -> Theme {
    Theme {
        bg: c(0xfafafa),
        panel: c(0xf0f0f2),
        panel2: c(0xe5e6ea),
        border: c(0xd4d6dc),
        text: c(0x383a42),
        dim: c(0x8a8f9e),
        ghost: c(0x5c6172),
        accent: c(0x2f6fdf),
        accent_hover: c(0x4a86e8),
        on_accent: c(0xffffff),
        selected: c(0xdde2ea),
        accent_soft: hsla(222. / 360., 0.80, 0.55, 0.12),
        danger: c(0xcf4436),
        danger_soft: rgba(0xcf44361f).into(),
        ok: c(0x4c9e4f),
        file_icon: c(0xb0b3bc),
        field: c(0xeaeaee),
    }
}

static THEME: std::sync::LazyLock<std::sync::RwLock<Theme>> =
    std::sync::LazyLock::new(|| std::sync::RwLock::new(dark()));

/// Current palette — read at render time only.
pub(crate) fn theme() -> Theme {
    *THEME.read().unwrap()
}

/// Resolve the OS appearance into a palette. Call once at startup and
/// again on every appearance change. `TDRIVE_APPEARANCE=dark|light`
/// overrides the OS value — portals on some Linux sessions report the
/// wrong scheme, and it gives us a way to render-check both palettes.
pub(crate) fn apply_appearance(appearance: WindowAppearance) {
    let appearance = match std::env::var("TDRIVE_APPEARANCE").as_deref() {
        Ok("dark") => WindowAppearance::Dark,
        Ok("light") => WindowAppearance::Light,
        _ => appearance,
    };
    *THEME.write().unwrap() = match appearance {
        WindowAppearance::Light | WindowAppearance::VibrantLight => light(),
        WindowAppearance::Dark | WindowAppearance::VibrantDark => dark(),
    };
}
