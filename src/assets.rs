//! Embedded icon assets — the SVGs in assets/icons are compiled into
//! the binary so GPUI's svg renderer can tint them with text color.

use gpui::AssetSource;
use std::borrow::Cow;

pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> anyhow::Result<Option<Cow<'static, [u8]>>> {
        let bytes: Option<&'static [u8]> = match path {
            "icons/arrow_down.svg" => Some(include_bytes!("../assets/icons/arrow_down.svg")),
            "icons/arrow_up.svg" => Some(include_bytes!("../assets/icons/arrow_up.svg")),
            "icons/camera.svg" => Some(include_bytes!("../assets/icons/camera.svg")),
            "icons/check.svg" => Some(include_bytes!("../assets/icons/check.svg")),
            "icons/chevron_down.svg" => Some(include_bytes!("../assets/icons/chevron_down.svg")),
            "icons/chevron_right.svg" => Some(include_bytes!("../assets/icons/chevron_right.svg")),
            "icons/chevron_up.svg" => Some(include_bytes!("../assets/icons/chevron_up.svg")),
            "icons/download.svg" => Some(include_bytes!("../assets/icons/download.svg")),
            "icons/drives.svg" => Some(include_bytes!("../assets/icons/drives.svg")),
            "icons/external.svg" => Some(include_bytes!("../assets/icons/external.svg")),
            "icons/eye.svg" => Some(include_bytes!("../assets/icons/eye.svg")),
            "icons/file.svg" => Some(include_bytes!("../assets/icons/file.svg")),
            "icons/folder.svg" => Some(include_bytes!("../assets/icons/folder.svg")),
            "icons/folder_plus.svg" => Some(include_bytes!("../assets/icons/folder_plus.svg")),
            "icons/grid.svg" => Some(include_bytes!("../assets/icons/grid.svg")),
            "icons/hard_drive.svg" => Some(include_bytes!("../assets/icons/hard_drive.svg")),
            "icons/image.svg" => Some(include_bytes!("../assets/icons/image.svg")),
            "icons/inbox.svg" => Some(include_bytes!("../assets/icons/inbox.svg")),
            "icons/list.svg" => Some(include_bytes!("../assets/icons/list.svg")),
            "icons/lock.svg" => Some(include_bytes!("../assets/icons/lock.svg")),
            "icons/log_out.svg" => Some(include_bytes!("../assets/icons/log_out.svg")),
            "icons/move.svg" => Some(include_bytes!("../assets/icons/move.svg")),
            "icons/pause.svg" => Some(include_bytes!("../assets/icons/pause.svg")),
            "icons/pencil.svg" => Some(include_bytes!("../assets/icons/pencil.svg")),
            "icons/play.svg" => Some(include_bytes!("../assets/icons/play.svg")),
            "icons/refresh.svg" => Some(include_bytes!("../assets/icons/refresh.svg")),
            "icons/restore.svg" => Some(include_bytes!("../assets/icons/restore.svg")),
            "icons/search.svg" => Some(include_bytes!("../assets/icons/search.svg")),
            "icons/shield.svg" => Some(include_bytes!("../assets/icons/shield.svg")),
            "icons/sliders.svg" => Some(include_bytes!("../assets/icons/sliders.svg")),
            "icons/sparkles.svg" => Some(include_bytes!("../assets/icons/sparkles.svg")),
            "icons/trash.svg" => Some(include_bytes!("../assets/icons/trash.svg")),
            "icons/unlock.svg" => Some(include_bytes!("../assets/icons/unlock.svg")),
            "icons/upload.svg" => Some(include_bytes!("../assets/icons/upload.svg")),
            "icons/video.svg" => Some(include_bytes!("../assets/icons/video.svg")),
            "icons/x.svg" => Some(include_bytes!("../assets/icons/x.svg")),
            _ => None,
        };
        Ok(bytes.map(Cow::Borrowed))
    }

    fn list(&self, _path: &str) -> anyhow::Result<Vec<gpui::SharedString>> {
        Ok(Vec::new())
    }
}
