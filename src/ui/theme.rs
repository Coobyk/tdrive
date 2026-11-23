//! Light/dark palettes resolved from the OS appearance. One global —
//! the app is single-window, so the current window's appearance is
//! the theme.

use gpui::{Hsla, WindowAppearance, rgb, rgba};

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

/// Neutral dark — zinc greys, near-white accent, muted semantics.
fn dark() -> Theme {
    Theme {
        bg: c(0x18191b),
        panel: c(0x131416),
        panel2: c(0x1f2124),
        border: c(0x2c2f33),
        text: c(0xd6d6d6),
        dim: c(0x74767a),
        ghost: c(0xa5a7ab),
        accent: c(0xe4e4e6),
        accent_hover: c(0xf2f2f3),
        on_accent: c(0x17181a),
        selected: c(0x2a2d31),
        accent_soft: rgba(0xffffff17).into(),
        danger: c(0xc96a6a),
        danger_soft: rgba(0xc96a6a1f).into(),
        ok: c(0x86a986),
        file_icon: c(0x53565a),
        field: c(0x0f1012),
    }
}

/// Neutral light — same greys inverted, near-black accent.
fn light() -> Theme {
    Theme {
        bg: c(0xf7f7f6),
        panel: c(0xeeeeec),
        panel2: c(0xe4e4e1),
        border: c(0xd8d8d5),
        text: c(0x2b2b2b),
        dim: c(0x8b8b8b),
        ghost: c(0x595959),
        accent: c(0x232323),
        accent_hover: c(0x383838),
        on_accent: c(0xfafafa),
        selected: c(0xdededa),
        accent_soft: rgba(0x00000017).into(),
        danger: c(0xa8443f),
        danger_soft: rgba(0xa8443f1a).into(),
        ok: c(0x5e8a5e),
        file_icon: c(0xa3a3a0),
        field: c(0xececea),
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
