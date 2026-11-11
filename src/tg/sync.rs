//! Namespace sync — full channel scan, watermark, live-sync debounce.
use super::*;

impl Worker {
    /// History scan of the open drive. A full scan runs when the namespace
    /// is empty; later refreshes apply only messages past the watermark.
    pub(super) async fn sync(&mut self) -> Result<()> {
        self.emit(Event::SyncState { active: true });
        let r = self.sync_inner().await;
        self.emit(Event::SyncState { active: false });
        r
    }

    pub(super) async fn sync_inner(&mut self) -> Result<()> {
        let client = self.client()?.clone();
        let peer = self.channel()?;
        let watermark = self.namespace.last_msg_id;
        let mut ns = (*self.namespace).clone();
        if watermark > 0 {
            // Incremental: forward-iterate from the last seen message.
            let mut iter = client
                .iter_messages(peer)
                .offset_id(watermark as i32)
                .reverse(true);
            while let Some(msg) = iter.next().await? {
                if !ns.apply_message(msg.id() as i64, msg.text()) {
                    if let Some(Media::Document(d)) = msg.media() {
                        ns.record_unadopted(
                            msg.id() as i64,
                            d.name().unwrap_or("unnamed").to_string(),
                            d.size().unwrap_or(0) as i64,
                            msg.sender_id().map(|p| p.bare_id_unchecked()).unwrap_or(0),
                            msg.date().as_second(),
                        );
                    }
                }
            }
        } else {
            let mut iter = client.iter_messages(peer).reverse(true);
            while let Some(msg) = iter.next().await? {
                if !ns.apply_message(msg.id() as i64, msg.text()) {
                    if let Some(Media::Document(d)) = msg.media() {
                        ns.record_unadopted(
                            msg.id() as i64,
                            d.name().unwrap_or("unnamed").to_string(),
                            d.size().unwrap_or(0) as i64,
                            msg.sender_id().map(|p| p.bare_id_unchecked()).unwrap_or(0),
                            msg.date().as_second(),
                        );
                    }
                }
            }
        }
        self.namespace = Arc::new(ns);
        self.save_namespace();
        self.push_namespace();

        // Retention sweep: physically purge entries past purge_after, like
        // TDrive's expired-trash sweep. Failures are surfaced, not fatal.
        let now = ops::unix_now();
        let expired: Vec<String> = self
            .namespace
            .trash
            .values()
            .filter(|e| e.purge_after > 0 && e.purge_after <= now)
            .map(|e| e.object_id.clone())
            .collect();
        for obj in expired {
            if let Err(e) = self.purge(obj).await {
                self.emit(Event::Error {
                    context: "trash purge",
                    message: format!("{e:#}"),
                });
            }
        }
        Ok(())
    }

    /// Debounced incremental refresh triggered by incoming updates.
    pub(super) async fn live_sync(&mut self) -> Result<()> {
        self.live_sync_queued
            .store(false, std::sync::atomic::Ordering::Relaxed);
        // A zero watermark just means empty-or-unsynced — full scan is the
        // correct response, not a permanent wedge.
        if self.peer_cell.lock().unwrap().is_none() {
            return Ok(());
        }
        let now = std::time::Instant::now();
        if now.duration_since(self.last_live_sync) < std::time::Duration::from_millis(1500) {
            return Ok(());
        }
        self.last_live_sync = now;
        self.sync().await
    }
}
