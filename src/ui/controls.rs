//! Reusable controls — modal buttons, toolbar chips, size formatting.

use super::*;

pub(super) fn button(
    label: &str,
    color: u32,
    on_click: impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let dark_text = color == ACCENT;
    div()
        .id(SharedString::from(format!("btn-{label}")))
        .flex()
        .justify_center()
        .items_center()
        .h(px(38.))
        .rounded_md()
        .bg(rgb(color))
        .hover(|s| s.opacity(0.9))
        .cursor_pointer()
        .on_mouse_down(MouseButton::Left, on_click)
        .child(
            div()
                .text_sm()
                .text_color(if dark_text {
                    rgb(ACCENT_DARK)
                } else {
                    rgb(0xffffff)
                })
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
        .border_color(rgb(BORDER))
        .hover(|s| s.bg(rgb(PANEL2)))
        .cursor_pointer()
        .on_mouse_down(MouseButton::Left, on_click)
        .child(
            div()
                .text_sm()
                .text_color(rgb(0x9ea8ba))
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
    label: &str,
    tone: Tone,
    on_click: impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    tool_button_id(format!("tb-{label}"), label, tone, on_click)
}

pub(super) fn tool_button_id(
    id: String,
    label: &str,
    tone: Tone,
    on_click: impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let (bg, hover_bg, text) = match tone {
        Tone::Ghost => (None, rgb(PANEL2), rgb(0x9ea8ba)),
        Tone::Primary => (Some(rgb(ACCENT)), rgb(0x7db9f2), rgb(ACCENT_DARK)),
        Tone::Danger => (None, rgba(0xe06c751f), rgb(DANGER)),
    };
    let mut d = div()
        .id(SharedString::from(id))
        .px_3()
        .py_1()
        .rounded_md()
        .hover(move |s| s.bg(hover_bg))
        .cursor_pointer()
        .on_mouse_down(MouseButton::Left, on_click);
    if let Some(c) = bg {
        d = d.bg(c);
    }
    d.child(
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
