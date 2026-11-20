//! Background thread that does all credential and network work.
//!
//! The UI sends [`Command`]s and receives [`Event`]s over channels, so the UI
//! thread never blocks on I/O.
//!
//! Several accounts (Gmail and Outlook, mixed) can be signed in at once. One of
//! them is *active*: its INBOX is synced and shown, and search/delete act on it.
//! Blocked senders are trashed in every signed-in account on each sync.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Instant;

use crate::auth::{self, Account, AuthError, CredentialStore, Provider};
use crate::filter::{self, Search};
use crate::mail::{ApiError, InboxSync, Login, MailClient};
use crate::model::Message;
use crate::oauth;

pub enum Command {
    /// Load the stored accounts. `prefer` is made active if it is among them.
    Restore { prefer: Option<String> },
    /// Verify and add a Gmail account. It becomes the active one.
    SignIn(Account),
    /// Sign in to a Microsoft account in the browser (device code flow) with
    /// this app registration, then add it. Send via [`WorkerHandle::sign_in_microsoft`].
    SignInMicrosoft { client_id: String },
    /// Remove one account (by address) from this device.
    SignOut(String),
    /// Choose the account whose INBOX is shown and acted on.
    Activate(String),
    /// Trash blocked-sender mail in every account, then bring the active INBOX up to date.
    /// `known` are the UIDs already loaded in the active account.
    Sync { known: Vec<u32>, page: u32, blocked: Vec<String> },
    /// Load up to `page` messages older than UID `oldest` in the active account.
    LoadOlder { oldest: u32, page: u32 },
    /// Count messages matching a search (for the delete preview).
    Search { search: Search },
    /// Move every message matching a search to Trash.
    TrashMatching { search: Search },
    /// Move specific INBOX messages of the active account to Trash.
    TrashMessages { uids: Vec<u32> },
    /// Trash blocked-sender mail in every account now, and report the count even if zero.
    TrashBlocked { blocked: Vec<String> },
}

/// A signed-in account, as the UI sees it (no secrets).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountInfo {
    pub email: String,
    pub provider: Provider,
}

pub enum Event {
    /// The signed-in accounts, which one is active, and where they're stored.
    SignedIn { accounts: Vec<AccountInfo>, active: String, store: CredentialStore },
    /// No accounts are signed in.
    SignedOut,
    /// Microsoft sign-in is waiting for the user to enter `user_code` at `verification_uri`.
    DeviceCode { user_code: String, verification_uri: String },
    /// Adding an account failed. The sign-in form stays open.
    SignInFailed { message: String, details: String },
    /// The user cancelled Microsoft sign-in.
    SignInCancelled,
    /// `account` says which INBOX the result belongs to, so a late result for a
    /// previously shown account can be ignored.
    Synced { account: String, sync: InboxSync },
    Older { account: String, messages: Vec<Message>, total: usize },
    SearchResult { search: Search, count: usize, sample: Vec<Message> },
    /// Messages were moved to Trash. `uids` are INBOX UIDs, when known.
    /// `resync` asks the UI to refresh the INBOX afterwards.
    Trashed { count: usize, uids: Vec<u32>, what: String, resync: bool },
    /// How far the blocked-mail cleanup has got: `fraction` from 0 to 1, and a line of status text.
    Progress { fraction: f32, text: String },
    /// The worker finished a command (sent after every command, success or not).
    Done,
    /// An error to show to the user. `signed_out` is set when no account is left.
    /// `details` is the full technical text, shown only when the user asks.
    Error { message: String, details: Option<String>, signed_out: bool },
}

pub struct WorkerHandle {
    tx: Sender<Command>,
    pub rx: Receiver<Event>,
    /// Set to stop a Microsoft sign-in that is waiting for the user.
    cancel: Arc<AtomicBool>,
}

impl WorkerHandle {
    pub fn send(&self, cmd: Command) {
        // The worker only exits when this handle is dropped, so send can't fail
        // while we're alive; ignore the error just in case.
        let _ = self.tx.send(cmd);
    }

    pub fn sign_in_microsoft(&self, client_id: String) {
        self.cancel.store(false, Ordering::Relaxed);
        self.send(Command::SignInMicrosoft { client_id });
    }

    pub fn cancel_sign_in(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

pub fn spawn(ctx: eframe::egui::Context) -> WorkerHandle {
    let (cmd_tx, cmd_rx) = channel::<Command>();
    let (evt_tx, evt_rx) = channel::<Event>();
    let cancel = Arc::new(AtomicBool::new(false));
    let worker_cancel = cancel.clone();
    std::thread::Builder::new()
        .name("gatekeep-worker".into())
        .spawn(move || {
            let mut worker = Worker {
                accounts: Vec::new(),
                active: None,
                store: CredentialStore::Keychain,
                tokens: HashMap::new(),
                cancel: worker_cancel,
                tx: evt_tx,
                ctx,
            };
            while let Ok(cmd) = cmd_rx.recv() {
                worker.handle(cmd);
                worker.emit(Event::Done);
            }
        })
        .expect("spawn worker thread");
    WorkerHandle {
        tx: cmd_tx,
        rx: evt_rx,
        cancel,
    }
}

struct Worker {
    /// Every signed-in account; IMAP connections are opened from these on demand.
    accounts: Vec<Account>,
    /// Address of the account the inbox shows.
    active: Option<String>,
    /// Where `accounts` is stored.
    store: CredentialStore,
    /// Outlook access tokens (in memory only), keyed by lowercase address.
    tokens: HashMap<String, (String, Instant)>,
    cancel: Arc<AtomicBool>,
    tx: Sender<Event>,
    ctx: eframe::egui::Context,
}

impl Worker {
    fn emit(&self, e: Event) {
        let _ = self.tx.send(e);
        self.ctx.request_repaint();
    }

    fn error(&self, err: ApiError, signed_out: bool) {
        log::warn!("worker error: {}", err.kind());
        self.emit(Event::Error {
            message: err.to_string(),
            details: Some(err.details()),
            signed_out,
        });
    }

    fn emails(&self) -> Vec<String> {
        self.accounts.iter().map(|a| a.email.clone()).collect()
    }

    /// Tell the UI which accounts are signed in, or that none are.
    fn announce(&mut self) {
        if self.accounts.is_empty() {
            self.active = None;
            self.emit(Event::SignedOut);
            return;
        }
        let active_is_valid = self
            .active
            .as_ref()
            .is_some_and(|a| self.accounts.iter().any(|acc| &acc.email == a));
        if !active_is_valid {
            self.active = Some(self.accounts[0].email.clone());
        }
        let active = self.active.clone().unwrap_or_default();
        let accounts = self
            .accounts
            .iter()
            .map(|a| AccountInfo { email: a.email.clone(), provider: a.provider })
            .collect();
        self.emit(Event::SignedIn { accounts, active, store: self.store });
    }

    /// Write the account list to the keychain (or the fallback file).
    fn save(&mut self) -> Result<(), AuthError> {
        if self.accounts.is_empty() {
            return auth::delete_all_accounts();
        }
        self.store = auth::save_accounts(&self.accounts)?;
        Ok(())
    }

    /// Remove one account from this device. Used for sign-out and revoked credentials.
    fn forget(&mut self, email: &str) {
        self.accounts.retain(|a| !a.email.eq_ignore_ascii_case(email));
        self.tokens.remove(&email.to_lowercase());
        if self.active.as_deref().is_some_and(|a| a.eq_ignore_ascii_case(email)) {
            self.active = None;
        }
        let saved = self.save();
        // Announce first: the UI clears errors when it hears about a sign-in.
        self.announce();
        if let Err(e) = saved {
            self.error(e.into(), false);
        }
    }

    /// Add (or replace) a verified account and make it the active one.
    fn add_account(&mut self, account: Account) {
        let email = account.email.clone();
        self.accounts.retain(|a| !a.email.eq_ignore_ascii_case(&email));
        self.accounts.push(account);
        self.active = Some(email);
        let saved = self.save();
        self.announce();
        if let Err(e) = saved {
            self.error(e.into(), false);
        }
    }

    fn sign_in_failed(&self, e: ApiError) {
        if matches!(e, ApiError::Auth(AuthError::Cancelled)) {
            self.emit(Event::SignInCancelled);
            return;
        }
        log::warn!("sign-in failed: {}", e.kind());
        self.emit(Event::SignInFailed {
            message: e.to_string(),
            details: e.details(),
        });
    }

    fn handle(&mut self, cmd: Command) {
        match cmd {
            Command::Restore { prefer } => match auth::load_accounts() {
                Some((accounts, store)) => self.restore(accounts, store, prefer),
                None => self.announce(),
            },
            Command::SignIn(account) => {
                let login = Login::Password(&account.app_password);
                match verify(&account.email, account.provider, login) {
                    Ok(()) => self.add_account(account),
                    Err(e) => self.sign_in_failed(e),
                }
            }
            Command::SignInMicrosoft { client_id } => match self.microsoft_sign_in(&client_id) {
                Ok(account) => self.add_account(account),
                Err(e) => self.sign_in_failed(e),
            },
            Command::SignOut(email) => self.forget(&email),
            Command::Activate(email) => {
                if self.accounts.iter().any(|a| a.email == email) {
                    self.active = Some(email);
                }
            }
            Command::Sync { known, page, blocked } => {
                let Some(active) = self.active.clone() else {
                    self.announce();
                    return;
                };
                if let Some(trashed) = self.trash_blocked(&blocked, false)
                    && trashed > 0
                {
                    self.emit(Event::Trashed {
                        count: trashed,
                        uids: Vec::new(),
                        what: "from blocked senders".into(),
                        resync: false,
                    });
                }
                if let Some(sync) = self.with_account(&active, |c| c.sync_inbox(&known, page)) {
                    self.emit(Event::Synced { account: active, sync });
                }
            }
            Command::LoadOlder { oldest, page } => {
                if let Some(account) = self.active.clone()
                    && let Some((messages, total)) =
                        self.with_account(&account, |c| c.fetch_older(oldest, page))
                {
                    self.emit(Event::Older { account, messages, total });
                }
            }
            Command::Search { search } => {
                if let Some((count, sample)) = self.with_client(|c| c.search_all_mail(&search, 5)) {
                    self.emit(Event::SearchResult { search, count, sample });
                }
            }
            Command::TrashMatching { search } => {
                if let Some(count) = self.with_client(|c| c.trash_matching(&search)) {
                    log::info!("trashed {count} messages matching a search");
                    self.emit(Event::Trashed {
                        count,
                        uids: Vec::new(),
                        what: "matching the filter".into(),
                        resync: true,
                    });
                }
            }
            Command::TrashBlocked { blocked } => {
                if let Some(count) = self.trash_blocked(&blocked, true) {
                    self.emit(Event::Trashed {
                        count,
                        uids: Vec::new(),
                        what: "from blocked senders".into(),
                        resync: true,
                    });
                }
            }
            Command::TrashMessages { uids } => {
                if let Some(count) = self.with_client(|c| c.trash_inbox_uids(&uids)) {
                    self.emit(Event::Trashed {
                        count,
                        uids,
                        what: String::new(),
                        resync: false,
                    });
                }
            }
        }
    }

    /// Trash mail from blocked senders in every signed-in account, not only the
    /// one shown. `None` if the block list is empty (an empty filter must never run).
    /// With `report`, sends [`Event::Progress`] as it goes.
    fn trash_blocked(&mut self, blocked: &[String], report: bool) -> Option<usize> {
        let search = filter::blocked_search(blocked)?;
        let emails = self.emails();
        let n = emails.len().max(1) as f32;
        let mut trashed = 0;
        for (i, email) in emails.iter().enumerate() {
            // The closure can't borrow `self` (with_account holds it), so it sends events itself.
            let (tx, ctx) = (self.tx.clone(), self.ctx.clone());
            let label = format!("Checking {email} ({} of {})", i + 1, emails.len());
            let before = trashed;
            let mut progress = |f: f32, moved: usize| {
                if report {
                    let text = format!("{label} · {} moved to Trash", before + moved);
                    let _ = tx.send(Event::Progress { fraction: (i as f32 + f) / n, text });
                    ctx.request_repaint();
                }
            };
            progress(0.0, 0);
            trashed += self
                .with_account(email, |c| c.trash_matching_with_progress(&search, &mut progress))
                .unwrap_or(0);
        }
        if trashed > 0 {
            log::info!("trashed {trashed} messages from blocked senders");
        }
        Some(trashed)
    }

    /// Run the device code flow, then confirm the account can use IMAP.
    fn microsoft_sign_in(&mut self, client_id: &str) -> Result<Account, ApiError> {
        let code = oauth::start_device_flow(client_id)?;
        self.emit(Event::DeviceCode {
            user_code: code.user_code.clone(),
            verification_uri: code.verification_uri.clone(),
        });
        let tokens = oauth::wait_for_tokens(client_id, &code, &self.cancel)?;
        let email = tokens.email.clone().ok_or_else(|| {
            AuthError::OAuth("Microsoft didn't say which address you signed in with.".into())
        })?;
        let refresh_token = tokens
            .refresh_token
            .clone()
            .ok_or_else(|| AuthError::OAuth("Microsoft didn't return a refresh token.".into()))?;
        verify(&email, Provider::Outlook, Login::Token(&tokens.access_token))?;
        self.tokens.insert(email.to_lowercase(), (tokens.access_token, tokens.expires_at));
        Ok(Account::outlook(&email, client_id, &refresh_token))
    }

    /// A valid Outlook access token for `account`: cached, or refreshed (in
    /// which case a rotated refresh token is saved).
    fn access_token(&mut self, account: &Account, allow_cached: bool) -> Result<String, ApiError> {
        let key = account.email.to_lowercase();
        if allow_cached
            && let Some((token, expires)) = self.tokens.get(&key)
            && Instant::now() < *expires
        {
            return Ok(token.clone());
        }
        let tokens = oauth::refresh(&account.client_id, &account.refresh_token)?;
        if let Some(new) = tokens.refresh_token.filter(|r| *r != account.refresh_token)
            && let Some(stored) = self.accounts.iter_mut().find(|a| a.email == account.email)
        {
            stored.refresh_token = new;
            if let Err(e) = self.save() {
                log::warn!("could not save refreshed Microsoft token ({})", e.kind());
            }
        }
        self.tokens.insert(key, (tokens.access_token.clone(), tokens.expires_at));
        Ok(tokens.access_token)
    }

    /// Log in to one account's IMAP server.
    fn connect(&mut self, account: &Account) -> Result<MailClient, ApiError> {
        match account.provider {
            Provider::Gmail => MailClient::connect(&account.email, account.provider, Login::Password(&account.app_password)),
            Provider::Outlook => {
                let had_cached = self.tokens.contains_key(&account.email.to_lowercase());
                let token = self.access_token(account, true)?;
                match MailClient::connect(&account.email, account.provider, Login::Token(&token)) {
                    // A cached token may have been invalidated early; retry once with a fresh one.
                    Err(ApiError::Auth(AuthError::ImapRefused)) if had_cached => {
                        let token = self.access_token(account, false)?;
                        MailClient::connect(&account.email, account.provider, Login::Token(&token))
                    }
                    other => other,
                }
            }
        }
    }

    /// Run `f` on the active account. See [`Worker::with_account`].
    fn with_client<T>(&mut self, f: impl FnOnce(&mut MailClient) -> Result<T, ApiError>) -> Option<T> {
        match self.active.clone() {
            Some(email) => self.with_account(&email, f),
            None => {
                self.announce();
                None
            }
        }
    }

    /// Run `f` on a fresh connection to one account. Errors are reported to the UI
    /// and give `None`; rejected credentials forget that account.
    fn with_account<T>(&mut self, email: &str, f: impl FnOnce(&mut MailClient) -> Result<T, ApiError>) -> Option<T> {
        let account = self.accounts.iter().find(|a| a.email == email)?.clone();
        let result = self.connect(&account).and_then(|mut client| {
            let out = f(&mut client);
            client.logout();
            out
        });
        match result {
            Ok(v) => Some(v),
            // The App Password or Microsoft sign-in was revoked while we were running.
            Err(e @ ApiError::Auth(AuthError::Rejected(_))) => {
                self.forget(&account.email);
                let signed_out = self.accounts.is_empty();
                self.error(e, signed_out);
                None
            }
            Err(e) => {
                self.error(e, false);
                None
            }
        }
    }

    /// Check the stored accounts still work. Revoked ones are forgotten; accounts
    /// that failed for another reason (e.g. offline) are kept.
    fn restore(&mut self, stored: Vec<Account>, store: CredentialStore, prefer: Option<String>) {
        self.accounts = stored;
        self.store = store;
        let mut errors: Vec<ApiError> = Vec::new();
        let mut rejected = Vec::new();
        for account in self.accounts.clone() {
            let result = self.connect(&account).and_then(|mut client| {
                let count = client.inbox_count();
                client.logout();
                count
            });
            match result {
                Ok(n) => log::info!("restored account; INBOX has {n} messages"),
                Err(e @ ApiError::Auth(AuthError::Rejected(_))) => {
                    rejected.push(account.email);
                    errors.push(e);
                }
                Err(e) => errors.push(e),
            }
        }
        self.accounts.retain(|a| !rejected.contains(&a.email));
        self.active = prefer.filter(|p| self.accounts.iter().any(|a| &a.email == p));
        let saved = if rejected.is_empty() { Ok(()) } else { self.save() };
        // Announce before the errors: the UI clears errors when it hears about a sign-in.
        self.announce();
        if let Err(e) = saved {
            errors.push(e.into());
        }
        for e in errors {
            self.error(e, false);
        }
    }
}

/// Log in and open INBOX read-only to confirm the account works.
fn verify(email: &str, provider: Provider, login: Login<'_>) -> Result<(), ApiError> {
    let mut client = MailClient::connect(email, provider, login)?;
    let count = client.inbox_count();
    client.logout();
    count.map(|n| log::info!("signed in; INBOX has {n} messages"))
}
