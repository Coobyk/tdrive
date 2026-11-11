//! Drive lifecycle — listing, creating, opening channels and
//! shared-drive invite join/poll.
use super::*;

impl Worker {
    pub(super) async fn list_channels(&mut self) -> Result<()> {
        let client = self.client()?.clone();
        let mut dialogs = client.iter_dialogs();
        let mut out = Vec::new();
        while let Some(dialog) = dialogs.next().await? {
            // Min/incomplete entities yield None from to_ref — never call
            // Dialog::peer_ref (it unwraps and kills the worker thread).
            let (kind, title) = match dialog.peer() {
                Peer::Channel(ch) => {
                    if ch.raw.left {
                        continue;
                    }
                    match ch.kind() {
                        Some(ChannelKind::Broadcast) => ("broadcast", ch.title().to_string()),
                        _ => continue,
                    }
                }
                Peer::Group(g) if g.is_megagroup() => {
                    ("megagroup", g.title().unwrap_or_default().to_string())
                }
                _ => continue,
            };
            let Ok(Some(peer_ref)) = dialog.peer().to_ref().await else {
                continue;
            };
            out.push(ChannelInfo {
                id: peer_ref.id.bare_id_unchecked(),
                access_hash: peer_ref.auth.hash(),
                title,
                kind,
            });
        }
        out.sort_by(|a, b| a.title.to_lowercase().cmp(&b.title.to_lowercase()));
        self.emit(Event::Channels(out));
        Ok(())
    }

    pub(super) async fn create_channel(&mut self, title: String) -> Result<()> {
        self.create_channel_inner(title, false).await
    }

    /// `shared`: megagroup + exported invite link (TDrive shared drive).
    pub(super) async fn create_channel_inner(&mut self, title: String, shared: bool) -> Result<()> {
        let client = self.client()?.clone();
        let updates = client
            .invoke(&grammers_client::tl::functions::channels::CreateChannel {
                broadcast: !shared,
                megagroup: shared,
                for_import: false,
                forum: false,
                title: title.clone(),
                about: "TDrive storage".to_string(),
                geo_point: None,
                address: None,
                ttl_period: None,
            })
            .await?;
        use grammers_client::tl::enums::Updates;
        let chats = match updates {
            Updates::Combined(u) => u.chats,
            Updates::Updates(u) => u.chats,
            _ => return Err(anyhow!("unexpected updates response")),
        };
        for chat in chats {
            if let grammers_client::tl::enums::Chat::Channel(c) = chat {
                self.open_channel(
                    c.id,
                    c.access_hash.unwrap_or(0),
                    c.title,
                    if shared { "megagroup" } else { "broadcast" }.to_string(),
                )
                .await?;
                if shared {
                    self.export_invite().await?;
                }
                return Ok(());
            }
        }
        Err(anyhow!("channel not found in create response"))
    }

    /// Export the invite link for a shared drive and surface it to the UI.
    pub(super) async fn export_invite(&mut self) -> Result<()> {
        use grammers_client::tl::enums::ExportedChatInvite;
        let invite = self
            .client()?
            .invoke(
                &grammers_client::tl::functions::messages::ExportChatInvite {
                    peer: self.channel()?.into(),
                    legacy_revoke_permanent: false,
                    request_needed: false,
                    expire_date: None,
                    usage_limit: None,
                    title: None,
                    subscription_pricing: None,
                },
            )
            .await?;
        if let ExportedChatInvite::ChatInviteExported(i) = invite {
            self.emit(Event::InviteLink { link: i.link });
        }
        Ok(())
    }

    pub(super) async fn open_channel(
        &mut self,
        id: i64,
        access_hash: i64,
        title: String,
        kind: String,
    ) -> Result<()> {
        let peer_id = PeerId::channel(id).context("bad channel id")?;
        *self.peer_cell.lock().unwrap() = Some(PeerRef {
            id: peer_id,
            auth: PeerAuth::from_hash(access_hash),
        });
        // Read-write WebDAV surface for this drive.
        dav::ensure_server(self.remote()?, self.cmd_tx.clone()).await;
        if let Some(url) = dav::url() {
            self.emit(Event::DavUrl { url });
        }
        if self.channel_id != id {
            // Different drive → different vault.
            *self.master_key.lock().unwrap() = None;
        }
        self.channel_id = id;
        self.channel_title = title.clone();
        self.channel_kind = kind;
        // Load the persisted projection so the first sync is incremental.
        self.namespace = Arc::new({
            let mut ns = config::namespace_path(id)
                .ok()
                .and_then(|p| std::fs::read_to_string(p).ok())
                .and_then(|s| serde_json::from_str::<crate::project::NamespaceSnapshot>(&s).ok())
                .map(crate::project::Namespace::restore_snapshot)
                .unwrap_or_default();
            ns.drop_zero_ids();
            ns
        });
        let pending = self
            .journal
            .uploads
            .iter()
            .filter(|j| j.channel_id == id && j.manifest_msg_id.is_none())
            .count();
        self.emit(Event::ChannelOpened {
            info: ChannelInfo {
                id,
                access_hash,
                title,
                kind: match self.channel_kind.as_str() {
                    "megagroup" => "megagroup",
                    "gigagroup" => "gigagroup",
                    _ => "broadcast",
                },
            },
            pending_uploads: pending,
        });
        self.sync().await?;
        self.emit_vault_state();

        // Resume interrupted downloads for this drive.
        let pending_downloads: Vec<DownloadJob> = self
            .journal
            .downloads
            .iter()
            .filter(|j| j.channel_id == id)
            .cloned()
            .collect();
        for job in pending_downloads {
            let id = format!("dl:{}", job.file_id);
            let token = CancellationToken::new();
            self.cancels.insert(id.clone(), token.clone());
            match self.transfer_ctx(token) {
                Ok(ctx) => {
                    let name = job.name.clone();
                    self.spawn_transfer(&id, &name, async move {
                        Self::run_download_task(&ctx, job).await
                    });
                }
                Err(e) => self.emit(Event::Error {
                    context: "download",
                    message: format!("{e:#}"),
                }),
            }
        }
        Ok(())
    }

    pub(super) async fn join_invite(&mut self, link: &str) -> Result<()> {
        // t.me/+hash or t.me/joinchat/hash
        let hash = link
            .trim()
            .rsplit('/')
            .next()
            .unwrap_or("")
            .trim_start_matches('+')
            .to_string();
        if hash.is_empty() {
            return Err(anyhow!("not an invite link"));
        }
        let client = self.client()?.clone();
        let result = client
            .invoke(
                &grammers_client::tl::functions::messages::ImportChatInvite { hash: hash.clone() },
            )
            .await;
        use grammers_client::tl::enums::{Updates, messages::ChatInviteJoinResult};
        let ok = match result {
            Ok(ChatInviteJoinResult::Ok(ok)) => ok,
            Err(e) if format!("{e}").contains("INVITE_REQUEST_SENT") => {
                // Join-request links: poll CheckChatInvite until approved,
                // mirroring TDrive's CheckPendingJoin loop.
                self.poll_pending_join(&hash).await?;
                return Ok(());
            }
            _ => return Err(anyhow!("invite requires an interactive join")),
        };
        let chats = match ok.updates {
            Updates::Combined(u) => u.chats,
            Updates::Updates(u) => u.chats,
            _ => return Err(anyhow!("unexpected updates response")),
        };
        for chat in chats {
            if let grammers_client::tl::enums::Chat::Channel(c) = chat {
                self.open_channel(
                    c.id,
                    c.access_hash.unwrap_or(0),
                    c.title,
                    if c.megagroup {
                        "megagroup"
                    } else {
                        "broadcast"
                    }
                    .to_string(),
                )
                .await?;
                self.list_channels().await?;
                return Ok(());
            }
        }
        Err(anyhow!("invite did not resolve to a channel"))
    }

    /// Poll `CheckChatInvite` until a join-request is approved (or give up).
    pub(super) async fn poll_pending_join(&mut self, hash: &str) -> Result<()> {
        use grammers_client::tl::enums::{Chat, ChatInvite};
        self.emit(Event::Error {
            context: "join",
            message: "waiting for admin approval…".to_string(),
        });
        for _ in 0..24 {
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            let inv = self
                .client()?
                .invoke(&grammers_client::tl::functions::messages::CheckChatInvite {
                    hash: hash.to_string(),
                })
                .await;
            if let Ok(ChatInvite::Already(already)) = inv {
                if let Chat::Channel(c) = already.chat {
                    self.open_channel(
                        c.id,
                        c.access_hash.unwrap_or(0),
                        c.title,
                        if c.megagroup {
                            "megagroup"
                        } else {
                            "broadcast"
                        }
                        .to_string(),
                    )
                    .await?;
                    self.list_channels().await?;
                    return Ok(());
                }
            }
        }
        Err(anyhow!("join request still pending"))
    }
}
