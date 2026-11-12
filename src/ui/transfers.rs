//! Transfer panel — rows for the pinned/expanded lists.
use super::controls::icon_button;
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
            TransferState::Active => ("", theme().accent),
            TransferState::Paused => ("paused", theme().dim),
            TransferState::Done => ("done", theme().ok),
            TransferState::Failed => ("failed", theme().danger),
        };
        let mut row = div()
            .flex()
            .items_center()
            .gap_3()
            .child(
                div()
                    .w(px(200.))
                    .text_sm()
                    .text_color(theme().text)
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .child(t.label.clone()),
            )
            .child(
                div()
                    .flex_1()
                    .h(px(4.))
                    .rounded_full()
                    .bg(theme().panel2)
                    .child(
                        div()
                            .h(px(4.))
                            .w(relative(frac))
                            .rounded_full()
                            .bg(color),
                    ),
            )
            .child(
                div()
                    .w(px(150.))
                    .text_xs()
                    .font_family("monospace")
                    .whitespace_nowrap()
                    .overflow_hidden()
                    .text_color(theme().dim)
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
            let active = t.state == TransferState::Active;
            let (action_icon, action_id) = if active {
                ("pause", format!("pa-{id}"))
            } else {
                ("play", format!("rs-{id}"))
            };
            row = row.child(icon_button(
                action_id,
                action_icon,
                theme().dim,
                theme().panel2,
                {
                    let id = id.clone();
                    cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                        if active {
                            this.send(Command::Pause { id: id.clone() });
                        } else {
                            this.send(Command::Resume { id: id.clone() });
                        }
                        cx.notify();
                    })
                },
            ));
            let id = t.id.clone();
            row = row.child(icon_button(
                format!("cx-{id}"),
                "x",
                theme().danger,
                theme().danger_soft,
                cx.listener(move |this, _, _, cx| {
                    this.send(Command::Cancel { id: id.clone() });
                    cx.notify();
                }),
            ));
        }
        row
    }
}
