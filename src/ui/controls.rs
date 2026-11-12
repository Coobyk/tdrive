//! Reusable controls — icons, modal buttons, toolbar chips, size formatting.

use super::*;

/// Embedded SVG icon tinted by the theme — GPUI rasterizes svgs to an
/// alpha mask and paints them with the element's text color.
pub(super) fn icon(name: &'static str) -> Svg {
    svg().path(SharedString::from(format!("icons/{name}.svg")))
}

/// Toolbar/list icon at a theme color.
pub(super) fn icon_sized(name: &'static str, size: f32, color: Hsla) -> impl IntoElement {
    icon(name).size(px(size)).text_color(color)
}

pub(super) fn button(
    label: &str,
    color: Hsla,
    on_click: impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    div()
        .id(SharedString::from(format!("btn-{label}")))
        .flex()
        .justify_center()
        .items_center()
        .h(px(38.))
        .rounded_md()
        .bg(color)
        .hover(|s| s.opacity(0.9))
        .cursor_pointer()
        .on_mouse_down(MouseButton::Left, on_click)
        .child(
            div()
                .text_sm()
                .text_color(theme().on_accent)
                .font_weight(FontWeight::SEMIBOLD)
                .child(label.to_string()),
        )
}

pub(super) fn button_secondary(
    label: &str,
    on_click: impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    div()
        .id(SharedString::from(format!("btn-{label}")))
        .flex()
        .justify_center()
        .items_center()
        .h(px(38.))
        .rounded_md()
        .border_1()
        .border_color(theme().border)
        .hover(|s| s.bg(theme().panel2))
        .cursor_pointer()
        .on_mouse_down(MouseButton::Left, on_click)
        .child(
            div()
                .text_sm()
                .text_color(theme().ghost)
                .font_weight(FontWeight::MEDIUM)
                .child(label.to_string()),
        )
}

/// Toolbar chip tone — ghost is the default chrome, primary is the
/// accent action, danger is destructive.
#[derive(Clone, Copy)]
pub(super) enum Tone {
    Ghost,
    Primary,
    Danger,
}

pub(super) fn tool_button(
    icon_name: &'static str,
    label: &str,
    tone: Tone,
    on_click: impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    tool_button_id(format!("tb-{label}"), icon_name, label, tone, on_click)
}

pub(super) fn tool_button_id(
    id: String,
    icon_name: &'static str,
    label: &str,
    tone: Tone,
    on_click: impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let t = theme();
    let (bg, hover_bg, text) = match tone {
        Tone::Ghost => (None, t.panel2, t.ghost),
        Tone::Primary => (Some(t.accent), t.accent_hover, t.on_accent),
        Tone::Danger => (None, t.danger_soft, t.danger),
    };
    let mut d = div()
        .id(SharedString::from(id))
        .flex()
        .items_center()
        .gap_1()
        .px_3()
        .py_1()
        .rounded_md()
        .hover(move |s| s.bg(hover_bg))
        .cursor_pointer()
        .on_mouse_down(MouseButton::Left, on_click);
    if let Some(c) = bg {
        d = d.bg(c);
    }
    d.child(icon_sized(icon_name, 13., text)).child(
        div()
            .text_sm()
            .text_color(text)
            .font_weight(match tone {
                Tone::Primary => FontWeight::SEMIBOLD,
                _ => FontWeight::NORMAL,
            })
            .child(label.to_string()),
    )
}

/// Icon-only control — transfer row actions, viewer close, toggles.
pub(super) fn icon_button(
    id: String,
    icon_name: &'static str,
    color: Hsla,
    hover_bg: Hsla,
    on_click: impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    div()
        .id(SharedString::from(id))
        .flex()
        .items_center()
        .justify_center()
        .w(px(24.))
        .h(px(24.))
        .rounded_md()
        .hover(move |s| s.bg(hover_bg))
        .cursor_pointer()
        .on_mouse_down(MouseButton::Left, on_click)
        .child(icon_sized(icon_name, 13., color))
}

/// Per-entry row/cell icon — folder vs file, with media-type variants.
pub(super) fn entry_icon(is_folder: bool, name: &str, size: f32) -> impl IntoElement {
    let t = theme();
    if is_folder {
        icon_sized("folder", size, t.accent).into_any_element()
    } else if crate::tg::is_image_name(name) {
        icon_sized("image", size, t.dim).into_any_element()
    } else if crate::tg::is_video_name(name) {
        icon_sized("video", size, t.dim).into_any_element()
    } else {
        icon_sized("file", size, t.file_icon).into_any_element()
    }
}

pub fn human_size(n: i64) -> String {
    const UNIT: i64 = 1024;
    if n < UNIT {
        return format!("{n} B");
    }
    let mut div = UNIT as f64;
    let mut exp = 0;
    while (n as f64) / div >= UNIT as f64 && exp < 6 {
        div *= UNIT as f64;
        exp += 1;
    }
    format!(
        "{:.1} {}B",
        n as f64 / div,
        "KMGTPE".chars().nth(exp.min(5)).unwrap_or('E')
    )
}
