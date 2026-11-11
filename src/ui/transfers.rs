//! Transfer panel — rows for the pinned/expanded lists.
use super::*;

impl Root {
    /// One transfers-pane row: name, progress bar, size/state, controls.
    pub(super) fn render_transfer(
        &mut self,
        t: &TransferUpdate,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let frac = if t.total > 0 {
            (t.sent as f32 / t.total as f32).clamp(0., 1.)
        } else {
            0.
        };
        let (state_label, color) = match t.state {
            TransferState::Active => ("", ACCENT),
            TransferState::Paused => ("paused", DIM),
            TransferState::Done => ("done", OK),
            TransferState::Failed => ("failed", DANGER),
        };
        let mut row = div()
            .flex()
            .items_center()
            .gap_3()
            .child(
                div()
                    .w(px(200.))
                    .text_sm()
                    .text_color(rgb(TEXT))
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .child(t.label.clone()),
            )
            .child(
                div()
                    .flex_1()
                    .h(px(4.))
                    .rounded_full()
                    .bg(rgb(PANEL2))
                    .child(
                        div()
                            .h(px(4.))
                            .w(relative(frac))
                            .rounded_full()
                            .bg(rgb(color)),
                    ),
            )
            .child(
                div()
                    .w(px(150.))
                    .text_xs()
                    .font_family("monospace")
                    .whitespace_nowrap()
                    .overflow_hidden()
                    .text_color(rgb(DIM))
                    .flex()
                    .justify_end()
                    .child(if state_label.is_empty() {
                        format!("{} / {}", human_size(t.sent), human_size(t.total))
                    } else {
                        state_label.to_string()
                    }),
            );
        if t.state == TransferState::Active || t.state == TransferState::Paused {
            let id = t.id.clone();
            let action = if t.state == TransferState::Active {
                ("pause", format!("pa-{id}"))
            } else {
                ("resume", format!("rs-{id}"))
            };
            row = row.child(
                div()
                    .id(SharedString::from(action.1))
                    .px_2()
                    .rounded_md()
                    .text_xs()
                    .text_color(rgb(DIM))
                    .hover(|s| s.bg(rgb(PANEL2)).text_color(rgb(TEXT)))
                    .cursor_pointer()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                            if action.0 == "pause" {
                                this.send(Command::Pause { id: id.clone() });
                            } else {
                                this.send(Command::Resume { id: id.clone() });
                            }
                            cx.notify();
                        }),
                    )
                    .child(action.0),
            );
            let id = t.id.clone();
            row = row.child(
                div()
                    .id(SharedString::from(format!("cx-{id}")))
                    .px_2()
                    .rounded_md()
                    .text_xs()
                    .text_color(rgb(DANGER))
                    .hover(|s| s.bg(rgba(0xe06c751f)))
                    .cursor_pointer()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| {
                            this.send(Command::Cancel { id: id.clone() });
                            cx.notify();
                        }),
                    )
                    .child("cancel"),
            );
        }
        row
    }
}
