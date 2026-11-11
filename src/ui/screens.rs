//! Login-flow screens — setup, auth, drive picker.
use super::controls::{button, button_secondary};
use super::*;

impl Root {
    pub(super) fn centered_card(&self, content: impl IntoElement) -> impl IntoElement {
        div()
            .flex()
            .items_center()
            .justify_center()
            .size_full()
            .child(
                div()
                    .w(px(400.))
                    .flex()
                    .flex_col()
                    .gap_4()
                    .p_6()
                    .rounded_lg()
                    .bg(rgb(PANEL))
                    .border_1()
                    .border_color(rgb(BORDER))
                    .shadow_lg()
                    .child(content),
            )
    }

    pub(super) fn render_setup(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let save = cx.listener(|this, _: &MouseDownEvent, _, cx| this.save_setup(cx));
        self.centered_card(
            div()
                .on_key_down(cx.listener(|this, ev: &KeyDownEvent, _, cx| {
                    if ev.keystroke.key == "enter" {
                        this.save_setup(cx);
                    }
                }))
                .flex()
                .flex_col()
                .gap_3()
                .child(
                    div()
                        .text_lg()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child("Connect to Telegram"),
                )
                .child(
                    div()
                        .text_sm()
                        .text_color(rgb(DIM))
                        .child("Create an app at my.telegram.org to get an api_id and api_hash."),
                )
                .child(self.f_api_id.clone())
                .child(self.f_api_hash.clone())
                .child(button("Continue", ACCENT, save)),
        )
    }

    pub(super) fn render_auth(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let inner = match self.screen {
            Screen::AuthPhone => {
                let submit = cx.listener(|this, _: &MouseDownEvent, _, cx| this.request_code(cx));
                div()
                    .on_key_down(cx.listener(|this, ev: &KeyDownEvent, _, cx| {
                        if ev.keystroke.key == "enter" {
                            this.request_code(cx);
                        }
                    }))
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(
                        div()
                            .text_lg()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("Sign in"),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(rgb(DIM))
                            .child("Enter your phone number with country code."),
                    )
                    .child(self.f_phone.clone())
                    .child(button("Send code", ACCENT, submit))
                    .into_any_element()
            }
            Screen::AuthCode => {
                let submit = cx.listener(|this, _: &MouseDownEvent, _, cx| this.submit_code(cx));
                div()
                    .on_key_down(cx.listener(|this, ev: &KeyDownEvent, _, cx| {
                        if ev.keystroke.key == "enter" {
                            this.submit_code(cx);
                        }
                    }))
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(
                        div()
                            .text_lg()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("Login code"),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(rgb(DIM))
                            .child("Enter the code sent to your Telegram app."),
                    )
                    .child(self.f_code.clone())
                    .child(button("Sign in", ACCENT, submit))
                    .into_any_element()
            }
            _ => {
                let submit =
                    cx.listener(|this, _: &MouseDownEvent, _, cx| this.submit_password(cx));
                div()
                    .on_key_down(cx.listener(|this, ev: &KeyDownEvent, _, cx| {
                        if ev.keystroke.key == "enter" {
                            this.submit_password(cx);
                        }
                    }))
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(
                        div()
                            .text_lg()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("Password"),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(rgb(DIM))
                            .child("Two-factor authentication is enabled."),
                    )
                    .child(self.f_password.clone())
                    .child(button("Unlock", ACCENT, submit))
                    .into_any_element()
            }
        };
        self.centered_card(inner)
    }

    pub(super) fn render_picker(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let create = cx.listener(|this, _: &MouseDownEvent, _, cx| this.create_channel(cx));
        let mut list = div().flex().flex_col().gap_1().w_full();
        for ch in &self.channels {
            let info = ch.clone();
            let id = format!("ch-{}", ch.id);
            list = list.child(
                div()
                    .id(SharedString::from(id))
                    .flex()
                    .items_center()
                    .justify_between()
                    .w_full()
                    .px_3()
                    .py_2()
                    .rounded_md()
                    .hover(|s| s.bg(rgb(PANEL2)))
                    .cursor_pointer()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| {
                            this.open_channel(info.clone());
                            cx.notify();
                        }),
                    )
                    .child(div().text_color(rgb(TEXT)).child(ch.title.clone()))
                    .child(div().text_sm().text_color(rgb(DIM)).child(ch.kind)),
            );
        }
        if self.channels.is_empty() {
            list = list.child(
                div()
                    .text_sm()
                    .text_color(rgb(DIM))
                    .child("No channels found — create a drive below."),
            );
        }

        self.centered_card(
            div()
                .flex()
                .flex_col()
                .gap_3()
                .child(
                    div()
                        .text_lg()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child("Choose a drive"),
                )
                .child(
                    div()
                        .text_sm()
                        .text_color(rgb(DIM))
                        .child("TDrive stores files in a private Telegram channel."),
                )
                .child(
                    div()
                        .id("channel-list")
                        .flex()
                        .flex_col()
                        .gap_2()
                        .max_h(px(280.))
                        .overflow_y_scroll()
                        .child(list),
                )
                .child(self.f_channel.clone())
                .child(button("Create new drive", ACCENT, create))
                .child(button_secondary(
                    "New shared drive",
                    cx.listener(|this, _: &MouseDownEvent, _, cx| {
                        let title = this.f_channel.read(cx).text().trim().to_string();
                        if !title.is_empty() {
                            this.send(Command::CreateSharedChannel { title });
                            this.status = "creating shared drive…".into();
                        }
                        cx.notify();
                    }),
                ))
                .child(self.f_invite.clone())
                .child(button(
                    "Join shared drive",
                    ACCENT,
                    cx.listener(|this, _: &MouseDownEvent, _, cx| {
                        let link = this.f_invite.read(cx).text().trim().to_string();
                        if !link.is_empty() {
                            this.send(Command::JoinInvite { link });
                            this.status = "joining…".into();
                        }
                        cx.notify();
                    }),
                )),
        )
    }
}
