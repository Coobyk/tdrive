//! Minimal single-line text field for GPUI.
//!
//! Raw GPUI ships no ready-made input widget; this covers what the app
//! needs: click-to-focus, character insertion, Backspace/Delete, arrows,
//! Home/End, Ctrl+V paste and Enter-to-submit. No selection or IME.

use gpui::*;

pub struct TextField {
    id: SharedString,
    text: String,
    /// Cursor position as a byte index (always on a char boundary).
    cursor: usize,
    placeholder: SharedString,
    masked: bool,
    focus: FocusHandle,
}

impl TextField {
    pub fn new(
        cx: &mut Context<Self>,
        id: impl Into<SharedString>,
        placeholder: impl Into<SharedString>,
    ) -> Self {
        Self {
            id: id.into(),
            text: String::new(),
            cursor: 0,
            placeholder: placeholder.into(),
            masked: false,
            focus: cx.focus_handle(),
        }
    }

    pub fn masked(mut self) -> Self {
        self.masked = true;
        self
    }

    pub fn text(&self) -> String {
        self.text.clone()
    }

    pub fn is_focused(&self, window: &Window) -> bool {
        self.focus.is_focused(window)
    }

    /// Set both text and placeholder — modal reuse starts each dialog
    /// fresh instead of leaking the last dialog's value.
    pub fn reset(
        &mut self,
        text: &str,
        placeholder: impl Into<SharedString>,
        cx: &mut Context<Self>,
    ) {
        self.clear(cx);
        self.text = text.to_string();
        self.cursor = text.len();
        self.placeholder = placeholder.into();
        cx.notify();
    }

    pub fn clear(&mut self, cx: &mut Context<Self>) {
        self.text.clear();
        self.cursor = 0;
        cx.notify();
    }

    fn handle_key(&mut self, ev: &KeyDownEvent, cx: &mut Context<Self>) {
        let ks = &ev.keystroke;
        let key = ks.key.as_str();
        let mods = ks.modifiers;

        if (mods.control || mods.platform) && key == "v" {
            if let Some(item) = cx.read_from_clipboard() {
                if let Some(text) = item.text() {
                    let text: String = text.chars().filter(|c| *c != '\n' && *c != '\r').collect();
                    self.text.insert_str(self.cursor, &text);
                    self.cursor += text.len();
                }
            }
        } else if (mods.control || mods.platform) && key == "c" {
            if !self.masked {
                cx.write_to_clipboard(ClipboardItem::new_string(self.text.clone()));
            }
        } else {
            match key {
                "backspace" => {
                    if self.cursor > 0 {
                        let prev = prev_boundary(&self.text, self.cursor);
                        self.text.replace_range(prev..self.cursor, "");
                        self.cursor = prev;
                    }
                }
                "delete" => {
                    if self.cursor < self.text.len() {
                        let next = next_boundary(&self.text, self.cursor);
                        self.text.replace_range(self.cursor..next, "");
                    }
                }
                "left" => self.cursor = prev_boundary(&self.text, self.cursor),
                "right" => self.cursor = next_boundary(&self.text, self.cursor),
                "home" => self.cursor = 0,
                "end" => self.cursor = self.text.len(),
                "enter" | "tab" | "shift-tab" => return, // bubbles to the form
                _ => {
                    if mods.control || mods.platform || mods.alt {
                        return;
                    }
                    if let Some(ch) = &ks.key_char {
                        self.text.insert_str(self.cursor, ch);
                        self.cursor += ch.len();
                    }
                }
            }
        }
        cx.notify();
    }

    fn display_text(&self) -> String {
        if self.masked {
            "•".repeat(self.text.chars().count())
        } else {
            self.text.clone()
        }
    }
}

fn prev_boundary(s: &str, cursor: usize) -> usize {
    s[..cursor]
        .char_indices()
        .next_back()
        .map(|(i, _)| i)
        .unwrap_or(0)
}

fn next_boundary(s: &str, cursor: usize) -> usize {
    s[cursor..]
        .char_indices()
        .nth(1)
        .map(|(i, _)| cursor + i)
        .unwrap_or(s.len())
}

impl Render for TextField {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let focused = self.focus.is_focused(window);
        let entity = cx.entity();

        // Split the display text at the cursor to place the caret.
        let shown = self.display_text();
        let cursor_chars = self.text[..self.cursor].chars().count();
        let (before, after) = if self.masked {
            let chars: Vec<char> = shown.chars().collect();
            let split = cursor_chars.min(chars.len());
            (
                chars[..split].iter().collect::<String>(),
                chars[split..].iter().collect::<String>(),
            )
        } else {
            (
                shown[..self.cursor].to_string(),
                shown[self.cursor..].to_string(),
            )
        };

        div()
            .id(ElementId::Name(self.id.clone()))
            .track_focus(&self.focus)
            .on_key_down(move |ev, _window, cx| {
                entity.update(cx, |this, cx| this.handle_key(ev, cx));
            })
            .cursor(CursorStyle::IBeam)
            .on_mouse_down(MouseButton::Left, {
                let focus = self.focus.clone();
                move |_, window, _| {
                    focus.focus(window);
                    // put the caret at the end on click
                }
            })
            .flex()
            .items_center()
            .w_full()
            .h(px(34.))
            .px_3()
            .rounded_md()
            .bg(rgb(0x1e222a))
            .border_1()
            .border_color(if focused {
                rgb(0x61afef)
            } else {
                rgb(0x3e4451)
            })
            .child(div().flex().items_center().overflow_hidden().child(
                if self.text.is_empty() && !focused {
                    div()
                        .text_color(rgb(0x5c6370))
                        .child(self.placeholder.clone())
                        .into_any_element()
                } else {
                    div()
                        .flex()
                        .items_center()
                        .child(div().text_color(rgb(0xabb2bf)).child(before))
                        .child(div().w(px(1.)).h(px(16.)).bg(if focused {
                            rgb(0x61afef)
                        } else {
                            rgb(0x5c6370)
                        }))
                        .child(div().text_color(rgb(0xabb2bf)).child(after))
                        .into_any_element()
                },
            ))
    }
}
