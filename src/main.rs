mod assets;
mod cli;
mod config;
mod crypto;
mod dav;
#[cfg(target_os = "linux")]
mod fusefs;
mod http;
mod input;
mod journal;
mod ops;
mod project;
mod remote;
mod stream;
mod tg;
#[cfg(target_os = "linux")]
mod tray;
mod ui;
mod wire;

use gpui::*;

fn main() {
    // Any argv → headless CLI; none → GPUI. The instance lock lives in
    // cli::run for the headless path, so both paths gate on it exactly once.
    let argv: Vec<String> = std::env::args().skip(1).collect();
    if !argv.is_empty() {
        std::process::exit(cli::run(argv));
    }
    // One instance per session.db — a second process sharing the
    // Telegram auth key gets AUTH_KEY_DUPLICATED from the server.
    let _instance = match config::instance_lock() {
        Ok(l) => l,
        Err(e) => {
            eprintln!("tdrive: {e}");
            std::process::exit(1);
        }
    };
    let handle = tg::spawn();
    Application::new().with_assets(assets::Assets).run(move |cx: &mut App| {

        let bounds = Bounds::centered(None, size(px(960.), px(640.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                window_min_size: Some(size(px(640.), px(420.))),
                app_id: Some("tdrive-rs".into()),
                ..Default::default()
            },
            |window, cx| {
                ui::apply_appearance(window.appearance());
                window
                    .observe_window_appearance(|window, _| {
                        ui::apply_appearance(window.appearance());
                        window.refresh();
                    })
                    .detach();
                cx.new(|cx| ui::Root::new(cx, handle))
            },
        )
        .unwrap();
        cx.activate(true);
    });
}
