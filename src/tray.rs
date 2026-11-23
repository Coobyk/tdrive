//! StatusNotifierItem tray over D-Bus (ksni) — Show and Quit.
//! Wayland can't unmap a toplevel and gpui's `hide()` is a Linux
//! no-op, so this stays a presence/entry point rather than true
//! close-to-tray.
use std::sync::mpsc::{Receiver, Sender};

/// Menu activations reported back to the UI thread.
#[derive(Debug)]
pub enum TrayMsg {
    Show,
    Quit,
}

struct TdriveTray {
    tx: Sender<TrayMsg>,
}

impl ksni::Tray for TdriveTray {
    fn id(&self) -> String {
        "tdrive".into()
    }

    fn title(&self) -> String {
        "TDrive".into()
    }

    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        // 22×22 ARGB — filled accent-blue rounded square with a
        // cut-out inner square so it reads as a folder-ish tile in
        // both light and dark tray themes.
        const N: i32 = 22;
        let mut data = Vec::with_capacity((N * N * 4) as usize);
        for y in 0..N {
            for x in 0..N {
                let inside = (4..18).contains(&x) && (6..18).contains(&y);
                let tab = (5..12).contains(&x) && (4..6).contains(&y);
                let inner = (7..15).contains(&x) && (9..15).contains(&y);
                let (a, r, g, b) = if inside || tab {
                    if inner {
                        (0, 0, 0, 0)
                    } else {
                        (255, 0x38, 0x8e, 0xd7)
                    }
                } else {
                    (0, 0, 0, 0)
                };
                data.extend_from_slice(&[b, g, r, a]); // ARGB32 little-endian = BGRA bytes
            }
        }
        vec![ksni::Icon {
            width: N,
            height: N,
            data,
        }]
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        use ksni::menu::*;
        vec![
            StandardItem {
                label: "Show".into(),
                activate: Box::new(|t: &mut Self| {
                    let _ = t.tx.send(TrayMsg::Show);
                }),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: "Quit".into(),
                activate: Box::new(|t: &mut Self| {
                    let _ = t.tx.send(TrayMsg::Quit);
                }),
                ..Default::default()
            }
            .into(),
        ]
    }
}

/// Spawn the SNI service on a background thread; returns the channel
/// the UI polls for menu activations. Fails quietly when no tray
/// host exists (bare WMs) — the app works the same without it.
pub fn spawn() -> Receiver<TrayMsg> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        match ksni::blocking::TrayMethods::spawn(TdriveTray { tx }) {
            // The service lives as long as the handle — park with it
            // held until process exit.
            Ok(_handle) => loop {
                std::thread::park();
            },
            Err(e) => eprintln!("tray: StatusNotifierItem unavailable: {e}"),
        }
    });
    rx
}
