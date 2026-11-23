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
                    .bg(theme().panel)
                    .border_1()
                    .border_color(theme().border)
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
                        .text_color(theme().dim)
                        .child("Create an app at my.telegram.org to get an api_id and api_hash."),
                )
                .child(self.f_api_id.clone())
                .child(self.f_api_hash.clone())
                .child(button("Continue", theme().accent, save)),
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
                            .text_color(theme().dim)
                            .child("Enter your phone number with country code."),
                    )
                    .child(self.f_phone.clone())
                    .child(button("Send code", theme().accent, submit))
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
                            .text_color(theme().dim)
                            .child("Enter the code sent to your Telegram app."),
                    )
                    .child(self.f_code.clone())
                    .child(button("Sign in", theme().accent, submit))
                    .into_any_element()
            }
            Screen::AuthPassword => {
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
                            .text_color(theme().dim)
                            .child("Two-factor authentication is enabled."),
                    )
                    .child(self.f_password.clone())
                    .child(button("Unlock", theme().accent, submit))
                    .into_any_element()
            }
            // render_auth only runs for auth screens — Setup/Connecting/
            // Picker/Drive dispatch before this match.
            _ => unreachable!(),
        };
        self.centered_card(inner)
    }

    /// Account switcher — rows switch the active login; sign-in happens
    /// on whichever account is active afterwards (unauthorized slots
    /// land on the phone screen via the Connected event).
    pub(super) fn render_accounts(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let mut list = div().flex().flex_col().gap_1().w_full();
        for acc in &self.accounts {
            let id = acc.id.clone();
            let active = self.active_account.as_deref() == Some(&*acc.id);
            let drives = if acc.drives.is_empty() {
                "no drives yet".to_string()
            } else {
                acc.drives.join("  ·  ")
            };
            let row = div()
                .id(SharedString::from(format!("acct-{}", acc.id)))
                .flex()
                .items_center()
                .justify_between()
                .w_full()
                .px_3()
                .py_2()
                .rounded_md();
            // Active row reads selected and isn't clickable; inactive
            // rows hover and switch on click.
            let row = if active {
                row.bg(theme().panel2)
            } else {
                row.hover(|s| s.bg(theme().panel2)).cursor_pointer()
            };
            list = list.child(
                row
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| {
                            if this.active_account.as_deref() != Some(id.as_str()) {
                                this.status = "switching account…".into();
                                this.send(Command::SwitchAccount { id: id.clone() });
                            }
                            cx.notify();
                        }),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(super::controls::icon_sized(
                                "person",
                                15.,
                                theme().accent,
                            ))
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .gap_2()
                                            .child(
                                                div()
                                                    .text_color(theme().text)
                                                    .child(acc.label.clone()),
                                            )
                                            .child(if active {
                                                div()
                                                    .text_xs()
                                                    .text_color(theme().accent)
                                                    .child("active")
                                                    .into_any_element()
                                            } else {
                                                div().into_any_element()
                                            }),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(theme().dim)
                                            .child(drives),
                                    ),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1()
                            .child(if active {
                                div()
                                    .id(SharedString::from(format!("acct-out-{}", acc.id)))
                                    .cursor_pointer()
                                    .px_1()
                                    .py(px(1.))
                                    .rounded_sm()
                                    .hover(|s| s.bg(theme().danger.opacity(0.2)))
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(move |this, _, _, cx| {
                                            cx.stop_propagation();
                                            this.send(Command::SignOut);
                                            cx.notify();
                                        }),
                                    )
                                    .child(super::controls::icon_sized(
                                        "log_out",
                                        13.,
                                        theme().dim,
                                    ))
                                    .into_any_element()
                            } else {
                                div().into_any_element()
                            })
                            .child(
                                div()
                                    .id(SharedString::from(format!("acct-rm-{}", acc.id)))
                                    .cursor_pointer()
                                    .px_1()
                                    .py(px(1.))
                                    .rounded_sm()
                                    .hover(|s| s.bg(theme().danger.opacity(0.2)))
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener({
                                            let id = acc.id.clone();
                                            move |this, _, _, cx| {
                                                cx.stop_propagation();
                                                this.send(Command::RemoveAccount {
                                                    id: id.clone(),
                                                });
                                                cx.notify();
                                            }
                                        }),
                                    )
                                    .child(super::controls::icon_sized(
                                        "trash",
                                        13.,
                                        theme().danger,
                                    )),
                            ),
                    ),
            );
        }
        if self.accounts.is_empty() {
            list = list.child(
                div()
                    .text_sm()
                    .text_color(theme().dim)
                    .child("No accounts yet — add one below."),
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
                        .child("Accounts"),
                )
                .child(
                    div()
                        .text_sm()
                        .text_color(theme().dim)
                        .child("Each account keeps its own session, drives and transfers."),
                )
                .child(
                    div()
                        .id("account-list")
                        .flex()
                        .flex_col()
                        .gap_2()
                        .max_h(px(280.))
                        .overflow_y_scroll()
                        .child(list),
                )
                .child(button(
                    "Add account",
                    theme().accent,
                    cx.listener(|this, _: &MouseDownEvent, _, cx| {
                        this.status = "adding account — sign in".into();
                        this.send(Command::AddAccount);
                        cx.notify();
                    }),
                ))
                .child(button_secondary(
                    if self.accounts_from == Screen::Drive {
                        "Back to files"
                    } else {
                        "Back to drives"
                    },
                    cx.listener(|this, _: &MouseDownEvent, _, cx| {
                        this.screen = this.accounts_from.clone();
                        if this.screen == Screen::Picker {
                            this.send(Command::ListChannels);
                        }
                        cx.notify();
                    }),
                )),
        )
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
                    .hover(|s| s.bg(theme().panel2))
                    .cursor_pointer()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| {
                            this.open_channel(info.clone());
                            cx.notify();
                        }),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(super::controls::icon_sized(
                                if ch.kind.is_personal() { "hard_drive" } else { "drives" },
                                15.,
                                theme().accent,
                            ))
                            .child(div().text_color(theme().text).child(ch.title.clone())),
                    )
                    .child(div().text_sm().text_color(theme().dim).child(ch.kind.as_str())),
            );
        }
        if self.channels.is_empty() {
            list = list.child(
                div()
                    .text_sm()
                    .text_color(theme().dim)
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
                        .text_color(theme().dim)
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
                .child(button("Create new drive", theme().accent, create))
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
                    theme().accent,
                    cx.listener(|this, _: &MouseDownEvent, _, cx| {
                        let link = this.f_invite.read(cx).text().trim().to_string();
                        if !link.is_empty() {
                            this.send(Command::JoinInvite { link });
                            this.status = "joining…".into();
                        }
                        cx.notify();
                    }),
                ))
                .child(button_secondary(
                    &self.account_label(),
                    cx.listener(|this, _: &MouseDownEvent, _, cx| {
                        this.open_accounts(cx);
                    }),
                )),
        )
    }
}
