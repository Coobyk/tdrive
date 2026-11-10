mod config;
mod crypto;
mod input;
mod journal;
mod project;
mod stream;
mod tg;
mod ui;
mod wire;

use gpui::*;

fn main() {
    let handle = tg::spawn();
    Application::new().run(move |cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(960.), px(640.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                window_min_size: Some(size(px(640.), px(420.))),
                app_id: Some("tdrive-rs".into()),
                ..Default::default()
            },
            |_, cx| cx.new(|cx| ui::Root::new(cx, handle)),
        )
        .unwrap();
        cx.activate(true);
    });
}
