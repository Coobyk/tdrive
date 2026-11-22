//! Login and session lifecycle — connect, code/2FA submit, sign-out.
use super::*;

impl Worker {
    pub(super) async fn connect(&mut self, api_id: i32, api_hash: String) -> Result<()> {
        // A reconnect must kill the old runner — two live connections
        // on one auth key trip AUTH_KEY_DUPLICATED server-side.
        if let Some(h) = self.pool_runner.take() {
            h.abort();
        }
        self.client = None;
        let session_path = config::session_path()?;
        let session = SqliteSession::open(session_path).await?;
        let pool = SenderPool::with_configuration(
            Arc::new(session),
            api_id,
            grammers_client::sender::ConnectionParams {
                retry_policy: Box::new(FloodSleep {
                    tx: self.tx.clone(),
                    countdown: std::sync::Mutex::new(None),
                }),
                ..Default::default()
            },
        );
        let client = Client::new(pool.handle.clone());
        let mut updates = pool.updates;
        self.pool_runner = Some(tokio::spawn(pool.runner.run()));
        // Any incoming update hints at new history — trigger a (cheap)
        // incremental sync back through the command queue.
        let cmd_tx = self.cmd_tx.clone();
        let queued = self.live_sync_queued.clone();
        tokio::spawn(async move {
            while updates.recv().await.is_some() {
                // Coalesce — a chatty account must not queue a resync
                // backlog that starves user commands.
                if !queued.swap(true, std::sync::atomic::Ordering::Relaxed)
                    && cmd_tx.send(Command::LiveSync).is_err()
                {
                    break;
                }
            }
        });
        self.api_id = api_id;
        self.api_hash = api_hash;
        self.client = Some(client);
        let authorized = self.client()?.is_authorized().await.unwrap_or(false);
        self.emit(Event::Connected { authorized });
        Ok(())
    }

    /// The stored auth key is dead — Telegram flagged it as used by
    /// another client (session copied, stale key, second instance).
    /// Nothing recovers a 406'd key: wipe the session db and connect
    /// fresh, which mints a new auth key server-side.
    async fn reconnect_fresh_session(&mut self) -> Result<()> {
        if let Some(h) = self.pool_runner.take() {
            h.abort();
        }
        self.client = None;
        let base = config::session_path()?;
        for suffix in ["", "-shm", "-wal", "-journal"] {
            let _ = std::fs::remove_file(format!("{}{}", base.display(), suffix));
        }
        self.connect(self.api_id, self.api_hash.clone()).await
    }

    pub(super) async fn request_code(&mut self, phone: String) -> Result<()> {
        match self
            .client()?
            .request_login_code(&phone, &self.api_hash)
            .await
        {
            Ok(token) => {
                self.login_token = Some(token);
                self.emit(Event::CodeSent);
                Ok(())
            }
            Err(e) if e.to_string().contains("AUTH_KEY_DUPLICATED") => {
                self.reconnect_fresh_session().await?;
                let token = self
                    .client()?
                    .request_login_code(&phone, &self.api_hash)
                    .await
                    .map_err(|e| anyhow!("request code: {e}"))?;
                self.login_token = Some(token);
                self.emit(Event::CodeSent);
                Ok(())
            }
            Err(e) => Err(anyhow!("request code: {e}")),
        }
    }

    pub(super) async fn submit_code(&mut self, code: String) -> Result<()> {
        let token = self.login_token.as_ref().context("no login in progress")?;
        match self.client()?.sign_in(token, &code).await {
            Ok(user) => {
                self.emit(Event::SignedIn {
                    name: user.full_name(),
                });
                self.after_login().await?;
            }
            Err(SignInError::PasswordRequired(token)) => {
                let hint = token.hint().unwrap_or("").to_string();
                self.password_token = Some(token);
                self.emit(Event::NeedPassword { hint });
            }
            Err(SignInError::InvalidCode) => {
                return Err(anyhow!("invalid login code"));
            }
            Err(SignInError::InvalidPassword(token)) => {
                self.password_token = Some(token);
                return Err(anyhow!("invalid 2FA password"));
            }
            Err(SignInError::SignUpRequired) => {
                return Err(anyhow!("this phone number has no Telegram account"));
            }
            Err(SignInError::Other(e)) => return Err(anyhow!("sign in: {e}")),
        }
        Ok(())
    }

    pub(super) async fn submit_password(&mut self, password: String) -> Result<()> {
        let token = self
            .password_token
            .take()
            .context("no password requested")?;
        match self.client()?.check_password(token, password).await {
            Ok(user) => {
                self.emit(Event::SignedIn {
                    name: user.full_name(),
                });
                self.after_login().await?;
            }
            Err(SignInError::InvalidPassword(token)) => {
                self.password_token = Some(token);
                return Err(anyhow!("invalid 2FA password"));
            }
            Err(e) => return Err(anyhow!("password check: {e}")),
        }
        Ok(())
    }

    pub(super) async fn after_login(&mut self) -> Result<()> {
        self.login_token = None;
        self.password_token = None;
        if let Ok(me) = self.client()?.get_me().await {
            self.actor_id = me.id().bare_id_unchecked();
            let name = me.full_name();
            if !name.is_empty() {
                let _ = config::label_active(&name);
                self.emit_accounts();
            }
        }
        self.list_channels().await
    }

    /// Kill the connection — only for account teardown. Sign-out keeps
    /// the client + pool: grammers accepts a fresh login on the same
    /// connection, so dropping it would strand the worker with no path
    /// back to the phone screen.
    fn teardown_client(&mut self) {
        if let Some(h) = self.pool_runner.take() {
            h.abort();
        }
        self.client = None;
    }

    /// Drop every trace of the live session's drive state — the local
    /// half of sign_out and of an account switch alike.
    fn reset_session_state(&mut self) {
        for (_, token) in self.cancels.drain() {
            token.cancel();
        }
        self.cancelled.clear();
        self.paused_downloads.clear();
        self.pending_optimize.clear();
        self.reserved_names.clear();
        #[cfg(target_os = "linux")]
        {
            self.fuse_session = None;
        }
        *self.peer_cell.lock().unwrap() = None;
        *self.ns_shared.lock().unwrap() = Arc::new(Namespace::new());
        self.channel_id = 0;
        self.channel_kind = DriveKind::Personal;
        self.login_token = None;
        self.password_token = None;
        self.actor_id = 0;
        *self.master_key.lock().unwrap() = None;
        self.namespace = Arc::new(Namespace::new());
    }

    /// Swap the journal for the slot the active dir now points at —
    /// call only after `reset_session_state` + activate.
    fn reload_account_state(&mut self) {
        if let Ok(p) = config::journal_path() {
            self.journal_path = p;
            self.journal = Journal::load(&self.journal_path);
        }
    }

    pub(super) fn emit_accounts(&self) {
        match config::Accounts::load() {
            Ok(acc) => self.emit(Event::Accounts {
                accounts: acc.accounts,
                active: acc.active,
            }),
            Err(e) => self.emit(Event::Error {
                context: "accounts",
                message: e.to_string(),
            }),
        }
    }

    pub(super) async fn switch_account(&mut self, id: &str) -> Result<()> {
        self.teardown_client();
        self.reset_session_state();
        let mut acc = config::Accounts::load()?;
        acc.activate(id)?;
        self.reload_account_state();
        self.emit_accounts();
        self.connect(self.api_id, self.api_hash.clone()).await
    }

    pub(super) async fn add_account(&mut self) -> Result<()> {
        self.teardown_client();
        self.reset_session_state();
        config::create_account()?;
        self.reload_account_state();
        self.emit_accounts();
        self.connect(self.api_id, self.api_hash.clone()).await
    }

    pub(super) async fn remove_account(&mut self, id: &str) -> Result<()> {
        let was_active = config::Accounts::load()?.active.as_deref() == Some(id);
        config::remove_account(id)?;
        if was_active {
            self.teardown_client();
            self.reset_session_state();
            // Re-init activates the next slot (or mints a default).
            config::init_accounts()?;
            self.reload_account_state();
            self.emit_accounts();
            self.connect(self.api_id, self.api_hash.clone()).await
        } else {
            self.emit_accounts();
            Ok(())
        }
    }

    pub(super) async fn sign_out(&mut self) -> Result<()> {
        if let Some(client) = &self.client {
            let _ = client.sign_out().await;
        }
        self.reset_session_state();
        self.emit(Event::SignedOut);
        Ok(())
    }
}
