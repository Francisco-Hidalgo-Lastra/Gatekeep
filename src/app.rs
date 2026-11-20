use std::collections::HashSet;
use std::time::{Duration, Instant};

use chrono::{DateTime, Datelike, Local};
use eframe::egui;

use crate::auth::{self, Account, CredentialStore, Provider};
use crate::config::Config;
use crate::filter::{self, DeleteFilter, Search};
use crate::mail::InboxSync;
use crate::model::Message;
use crate::rules::{self, Verdict};
use crate::theme;
use crate::worker::{self, AccountInfo, Command, Event, WorkerHandle};

/// Auto-refresh choices offered in the toolbar, in minutes (0 = off).
const AUTO_REFRESH_CHOICES: [u32; 5] = [0, 1, 5, 15, 30];

/// How long a toast stays on screen before it disappears.
const TOAST_LIFETIME: Duration = Duration::from_secs(4);

/// Height of one row in the message list.
const ROW_HEIGHT: f32 = 66.0;

/// Width reserved at the right edge of a row for its selection checkbox.
const CHECK_WIDTH: f32 = 28.0;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthState {
    /// Checking for stored accounts at startup.
    Checking,
    SignedOut,
    /// `accounts` are the signed-in addresses; `active` is the one whose INBOX is shown.
    SignedIn { accounts: Vec<AccountInfo>, active: String, store: CredentialStore },
}

pub struct GatekeepApp {
    config: Config,
    worker: WorkerHandle,
    auth: AuthState,
    /// True while the sign-in form is verifying an account.
    signing_in: bool,
    /// Which provider the sign-in form is for.
    login_provider: Provider,
    /// Microsoft sign-in in progress: the code to enter, and where.
    device_code: Option<(String, String)>,
    /// True when the sign-in form is open to add another account to a signed-in session.
    adding_account: bool,
    /// Notices in the bottom-right corner: info (e.g. "Moved 12 emails to Trash") and errors.
    toasts: Vec<Toast>,
    /// Commands sent to the worker that haven't finished yet.
    in_flight: usize,
    /// A finished command asked for a refresh. It runs once the queue is empty,
    /// so a batch of deletes triggers one refresh instead of one per job.
    refresh_when_idle: bool,
    /// What the worker is doing, for the toolbar.
    status: &'static str,
    /// The blocked-mail cleanup, while it is queued or running. The whole UI is
    /// locked behind a progress window until it finishes.
    sweep: Option<Sweep>,
    delete: DeleteDialog,
    blocked: BlockedDialog,
    /// Login form fields. The password is cleared as soon as it is submitted.
    login_email: String,
    login_password: String,
    inbox: Inbox,
}

/// The fetched INBOX and what the user is looking at.
#[derive(Default)]
struct Inbox {
    /// Newest first.
    messages: Vec<Message>,
    /// `verdicts[i]` is the rule verdict for `messages[i]`.
    verdicts: Vec<Verdict>,
    /// IMAP UID of the selected message.
    selected: Option<u32>,
    /// UIDs of the messages ticked for a bulk delete.
    checked: HashSet<u32>,
    /// Number of messages in the INBOX on the server.
    total: usize,
    /// When the last refresh finished (wall clock for display, monotonic for auto-refresh).
    updated: Option<(DateTime<Local>, Instant)>,
}

impl Inbox {
    /// Indices of the messages shown in the list (not hidden by a rule), newest first.
    fn visible(&self) -> Vec<usize> {
        (0..self.messages.len()).filter(|&i| !self.verdicts[i].hidden).collect()
    }

    fn find(&self, uid: u32) -> Option<usize> {
        self.messages.iter().position(|m| m.uid == uid)
    }
}

/// The "Delete emails" window.
#[derive(Default)]
struct DeleteDialog {
    open: bool,
    filter: DeleteFilter,
    /// Last search result; only valid while its query equals the current one.
    result: Option<SearchResult>,
}

struct SearchResult {
    search: Search,
    count: usize,
    sample: Vec<Message>,
}

/// Progress of the blocked-mail cleanup.
struct Sweep {
    /// `Done` events still to come before the cleanup's own (commands queued
    /// ahead of it, plus itself). The worker runs commands in order.
    done_left: usize,
    fraction: f32,
    text: String,
}

/// The "Blocked senders" window and the block confirmation.
#[derive(Default)]
struct BlockedDialog {
    open: bool,
    /// Filter text for the list of blocked senders.
    search: String,
    input: String,
    input_error: Option<&'static str>,
    /// Sender waiting for the user to confirm blocking.
    confirm: Option<String>,
}

/// Something the user asked for in the reading pane.
enum PaneAction {
    Trash(u32),
    /// Trash every email from this address right away.
    DeleteFrom(String),
    /// Open the "Delete emails" window pre-filled with this address.
    ReviewFrom(String),
    Block(String),
}

impl GatekeepApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let (config, config_err) = Config::load();
        theme::install(&cc.egui_ctx);
        cc.egui_ctx.set_theme(match config.theme {
            crate::config::ThemeChoice::System => egui::ThemePreference::System,
            crate::config::ThemeChoice::Light => egui::ThemePreference::Light,
            crate::config::ThemeChoice::Dark => egui::ThemePreference::Dark,
        });
        let worker = worker::spawn(cc.egui_ctx.clone());
        worker.send(Command::Restore { prefer: config.last_account.clone() });
        Self {
            config,
            worker,
            auth: AuthState::Checking,
            signing_in: false,
            login_provider: Provider::Gmail,
            device_code: None,
            adding_account: false,
            toasts: config_err.into_iter().map(|m| Toast::error(m, None)).collect(),
            in_flight: 1,
            refresh_when_idle: false,
            status: "Checking stored session…",
            sweep: None,
            delete: DeleteDialog::default(),
            blocked: BlockedDialog::default(),
            login_email: String::new(),
            login_password: String::new(),
            inbox: Inbox::default(),
        }
    }

    fn busy(&self) -> bool {
        self.in_flight > 0
    }

    /// Address of the account whose INBOX is shown, if signed in.
    fn active_email(&self) -> Option<&str> {
        match &self.auth {
            AuthState::SignedIn { active, .. } => Some(active.as_str()),
            _ => None,
        }
    }

    /// Provider of the account whose INBOX is shown.
    fn active_provider(&self) -> Provider {
        match &self.auth {
            AuthState::SignedIn { accounts, active, .. } => accounts
                .iter()
                .find(|a| &a.email == active)
                .map_or(Provider::Gmail, |a| a.provider),
            _ => Provider::Gmail,
        }
    }

    /// The toolbar text while busy: what the worker is doing, plus how many
    /// commands are waiting in the queue when there is more than one.
    fn status_label(&self) -> String {
        match self.in_flight {
            0 | 1 => self.status.to_owned(),
            n => format!("{} · {n} in queue", self.status),
        }
    }

    fn send(&mut self, cmd: Command, status: &'static str) {
        self.in_flight += 1;
        self.status = status;
        self.worker.send(cmd);
    }

    /// Trash mail from blocked senders in every account, then fetch new INBOX mail.
    fn refresh(&mut self) {
        let known = self.inbox.messages.iter().map(|m| m.uid).collect();
        let cmd = Command::Sync {
            known,
            page: self.config.page_size,
            blocked: self.config.blocked_senders.clone(),
        };
        self.send(cmd, "Refreshing…");
    }

    fn load_older(&mut self) {
        if let Some(oldest) = self.inbox.messages.iter().map(|m| m.uid).min() {
            let page = self.config.page_size;
            self.send(Command::LoadOlder { oldest, page }, "Loading older mail…");
        }
    }

    fn apply_sync(&mut self, sync: InboxSync) {
        let inbox = &mut self.inbox;
        if sync.replace {
            inbox.messages = sync.new;
        } else {
            inbox.messages.retain(|m| !sync.removed.contains(&m.uid));
            for (uid, unread) in sync.unread {
                if let Some(i) = inbox.find(uid) {
                    inbox.messages[i].unread = unread;
                }
            }
            inbox.messages.extend(sync.new);
        }
        inbox.total = sync.total;
        inbox.updated = Some((Local::now(), Instant::now()));
        self.messages_changed();
    }

    /// Re-sort, re-run the rules, and drop a selection that no longer exists.
    fn messages_changed(&mut self) {
        let inbox = &mut self.inbox;
        inbox.messages.sort_by_key(|m| std::cmp::Reverse(m.date));
        inbox.messages.dedup_by_key(|m| m.uid);
        inbox.verdicts = inbox.messages.iter().map(|m| rules::evaluate(&self.config.rules, m)).collect();
        if let Some(sel) = inbox.selected
            && inbox.find(sel).is_none()
        {
            inbox.selected = None;
        }
        let present: HashSet<u32> = inbox.messages.iter().map(|m| m.uid).collect();
        inbox.checked.retain(|uid| present.contains(uid));
    }

    /// Move one INBOX message to Trash and select the next one in the view.
    fn trash_message(&mut self, uid: u32) {
        let visible = self.inbox.visible();
        if self.inbox.selected == Some(uid) {
            let pos = visible.iter().position(|&i| self.inbox.messages[i].uid == uid);
            self.inbox.selected = pos
                .and_then(|p| visible.get(p + 1).or_else(|| p.checked_sub(1).and_then(|q| visible.get(q))))
                .map(|&i| self.inbox.messages[i].uid);
        }
        // Remove it right away; the worker reports an error if the move fails.
        if let Some(i) = self.inbox.find(uid) {
            self.inbox.messages.remove(i);
            self.inbox.total = self.inbox.total.saturating_sub(1);
            self.messages_changed();
        }
        self.send(Command::TrashMessages { uids: vec![uid] }, "Moving to Trash…");
    }

    /// Move every ticked message to Trash.
    fn trash_checked(&mut self) {
        let mut uids: Vec<u32> = self.inbox.checked.drain().collect();
        if uids.is_empty() {
            return;
        }
        uids.sort_unstable();
        let before = self.inbox.messages.len();
        // Remove them right away, as with a single delete.
        self.inbox.messages.retain(|m| uids.binary_search(&m.uid).is_err());
        self.inbox.total = self.inbox.total.saturating_sub(before - self.inbox.messages.len());
        self.messages_changed();
        self.send(Command::TrashMessages { uids }, "Moving to Trash…");
    }

    fn save_config(&mut self) {
        if let Err(e) = self.config.save() {
            self.push_error(e);
        }
    }

    /// Remember which account to open at the next start.
    fn remember_account(&mut self, email: &str) {
        if self.config.last_account.as_deref() != Some(email) {
            self.config.last_account = Some(email.to_owned());
            self.save_config();
        }
    }

    /// Show another signed-in account's INBOX.
    fn switch_account(&mut self, email: String) {
        let AuthState::SignedIn { active, .. } = &mut self.auth else { return };
        if *active == email {
            return;
        }
        *active = email.clone();
        self.inbox = Inbox::default();
        self.remember_account(&email);
        self.worker.send(Command::Activate(email));
        self.refresh();
    }

    /// Kick off a refresh when the auto-refresh interval has passed.
    fn auto_refresh(&mut self, ctx: &egui::Context) {
        let minutes = self.config.auto_refresh_minutes;
        if minutes == 0 || !matches!(self.auth, AuthState::SignedIn { .. }) || self.busy() {
            return;
        }
        let interval = Duration::from_secs(u64::from(minutes) * 60);
        match self.inbox.updated {
            Some((_, at)) if at.elapsed() < interval => ctx.request_repaint_after(interval - at.elapsed()),
            Some(_) => self.refresh(),
            None => {}
        }
    }

    fn drain_events(&mut self) {
        while let Ok(event) = self.worker.rx.try_recv() {
            match event {
                Event::SignedIn { accounts, active, store } => {
                    // A transient restore error (e.g. offline) arrives right after
                    // SignedIn, so clearing here doesn't hide it.
                    self.clear_errors();
                    self.signing_in = false;
                    self.device_code = None;
                    self.adding_account = false;
                    self.login_email.clear();
                    let changed = self.active_email() != Some(active.as_str());
                    self.auth = AuthState::SignedIn { accounts, active: active.clone(), store };
                    if changed {
                        self.inbox = Inbox::default();
                        self.remember_account(&active);
                        self.refresh();
                    }
                }
                Event::SignedOut => {
                    self.auth = AuthState::SignedOut;
                    self.signing_in = false;
                    self.adding_account = false;
                    self.inbox = Inbox::default();
                }
                Event::DeviceCode { user_code, verification_uri } => {
                    self.device_code = Some((user_code, verification_uri));
                }
                Event::SignInFailed { message, details } => {
                    self.signing_in = false;
                    self.device_code = None;
                    self.push_error_details(message, Some(details));
                }
                Event::SignInCancelled => {
                    self.signing_in = false;
                    self.device_code = None;
                }
                Event::Synced { account, sync } => {
                    let shown = self.active_email() == Some(account.as_str());
                    if shown {
                        self.apply_sync(sync);
                    }
                }
                Event::Older { account, messages, total } => {
                    let shown = self.active_email() == Some(account.as_str());
                    if shown {
                        self.inbox.messages.extend(messages);
                        self.inbox.total = total;
                        self.messages_changed();
                    }
                }
                Event::SearchResult { search, count, sample } => {
                    self.delete.result = Some(SearchResult { search, count, sample });
                }
                Event::Trashed { count, uids, what, resync } => {
                    // A single delete already left the list, so it needs no notice.
                    if count == 0 {
                        self.toast(format!("No emails{} found.", if what.is_empty() { String::new() } else { format!(" {what}") }));
                    } else if uids.len() != 1 {
                        let s = if count == 1 { "" } else { "s" };
                        let what = if what.is_empty() { String::new() } else { format!(" {what}") };
                        self.toast(format!("Moved {count} email{s}{what} to Trash."));
                    }
                    if resync {
                        self.refresh_when_idle = true;
                    }
                }
                Event::Progress { fraction, text } => {
                    if let Some(sweep) = &mut self.sweep {
                        sweep.fraction = fraction;
                        sweep.text = text;
                    }
                }
                Event::Done => {
                    self.in_flight = self.in_flight.saturating_sub(1);
                    if let Some(sweep) = &mut self.sweep {
                        sweep.done_left = sweep.done_left.saturating_sub(1);
                        if sweep.done_left == 0 {
                            self.sweep = None;
                        }
                    }
                    if self.in_flight == 0 && std::mem::take(&mut self.refresh_when_idle) {
                        self.refresh();
                    }
                }
                Event::Error { message, details, signed_out } => {
                    if signed_out {
                        self.auth = AuthState::SignedOut;
                        self.inbox = Inbox::default();
                    }
                    self.push_error_details(message, details);
                }
            }
        }
    }

    fn toast(&mut self, message: String) {
        self.toasts.push(Toast::info(message));
    }

    fn push_error(&mut self, message: String) {
        self.push_error_details(message, None);
    }

    fn push_error_details(&mut self, message: String, details: Option<String>) {
        // Avoid stacking identical messages (e.g. repeated offline refreshes).
        if !self.toasts.iter().any(|t| t.error && t.message == message) {
            self.toasts.push(Toast::error(message, details));
        }
    }

    fn top_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            theme::logo(ui, 26.0);
            ui.heading("Gatekeep");
            ui.add_space(6.0);
            match self.auth.clone() {
                AuthState::Checking => {
                    ui.spinner();
                    ui.weak("Checking stored session…");
                }
                AuthState::SignedOut => {
                    ui.weak("Signed out");
                }
                AuthState::SignedIn { accounts, active, store } => {
                    if accounts.len() > 1 {
                        let mut chosen = active.clone();
                        egui::ComboBox::from_id_salt("account_switch")
                            .selected_text(active.as_str())
                            .show_ui(ui, |ui| {
                                for a in &accounts {
                                    let label = format!("{} ({})", a.email, a.provider.name());
                                    ui.selectable_value(&mut chosen, a.email.clone(), label);
                                }
                            });
                        if chosen != active {
                            self.switch_account(chosen);
                        }
                    } else {
                        ui.weak(format!("· {active}"));
                    }
                    if store == CredentialStore::File {
                        ui.label("⚠")
                            .on_hover_text("No OS keychain was available; your sign-in credentials are stored in a file in your config directory.");
                    }
                    if ui.button("+ Add account").clicked() {
                        self.adding_account = true;
                    }
                    if ui
                        .button("Sign out")
                        .on_hover_text(format!("Remove {active} from Gatekeep"))
                        .clicked()
                    {
                        self.worker.send(Command::SignOut(active.clone()));
                    }
                }
            }
            if self.signing_in && self.device_code.is_none() {
                ui.spinner();
                ui.weak("Signing in…");
            }
        });
    }

    /// Errors are toasts too, so a new sign-in or refresh clears the old ones.
    fn clear_errors(&mut self) {
        self.toasts.retain(|t| !t.error);
    }
}

/// A notice in the bottom-right corner. Info toasts disappear by themselves. Error
/// toasts stay until dismissed, so the user has time to read and copy their Details.
struct Toast {
    message: String,
    /// Full technical text for an error, hidden until expanded.
    details: Option<String>,
    expanded: bool,
    error: bool,
    shown: Instant,
}

impl Toast {
    fn info(message: String) -> Self {
        Self { message, details: None, expanded: false, error: false, shown: Instant::now() }
    }

    fn error(message: String, details: Option<String>) -> Self {
        Self { message, details, expanded: false, error: true, shown: Instant::now() }
    }
}

/// Width of a toast, so long error messages wrap instead of stretching the window.
const TOAST_WIDTH: f32 = 340.0;

impl GatekeepApp {
    /// Draw the toasts stacked in the bottom-right corner, dropping expired info toasts.
    fn toasts_ui(&mut self, ctx: &egui::Context) {
        self.toasts.retain(|t| t.error || t.shown.elapsed() < TOAST_LIFETIME);
        if self.toasts.iter().any(|t| !t.error) {
            // Repaint so info toasts expire on time without any user input.
            ctx.request_repaint_after(Duration::from_millis(200));
        }
        if self.toasts.is_empty() {
            return;
        }
        let mut dismiss = None;
        egui::Area::new(egui::Id::unique("toasts"))
            .anchor(egui::Align2::RIGHT_BOTTOM, egui::vec2(-16.0, -16.0))
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                let error_color = ui.visuals().error_fg_color;
                for (i, t) in self.toasts.iter_mut().enumerate() {
                    egui::Frame::popup(ui.style()).show(ui, |ui| {
                        ui.set_max_width(TOAST_WIDTH);
                        ui.horizontal(|ui| {
                            if t.error {
                                ui.colored_label(error_color, "⚠");
                            }
                            ui.add(egui::Label::new(&t.message).wrap().selectable(t.error));
                        });
                        ui.horizontal(|ui| {
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if ui.small_button("✕").on_hover_text("Dismiss").clicked() {
                                    dismiss = Some(i);
                                }
                                if let Some(details) = &t.details {
                                    let label = if t.expanded { "Hide details" } else { "Details" };
                                    if ui.small_button(label).clicked() {
                                        t.expanded = !t.expanded;
                                    }
                                    if t.expanded && ui.small_button("Copy").on_hover_text("Copy the full error").clicked() {
                                        ui.ctx().copy_text(format!("{}\n\n{details}", t.message));
                                    }
                                }
                            });
                        });
                        if let (true, Some(details)) = (t.expanded, &t.details) {
                            ui.add(egui::Label::new(egui::RichText::new(details).monospace()).wrap().selectable(true));
                        }
                    });
                    ui.add_space(6.0);
                }
            });
        if let Some(i) = dismiss {
            self.toasts.remove(i);
        }
    }
}

impl GatekeepApp {
    fn login_form(&mut self, ui: &mut egui::Ui) {
        ui.vertical_centered(|ui| {
            ui.add_space(48.0);
            theme::card(ui).inner_margin(28.0).show(ui, |ui| {
                ui.set_width(360.0);
                ui.vertical_centered(|ui| {
                    theme::logo(ui, 52.0);
                    ui.add_space(14.0);
                    let title = if self.adding_account { "Add an account" } else { "Sign in" };
                    ui.heading(title);
                    ui.add_space(6.0);
                    ui.weak("Gatekeep never sends mail or marks it as read. It only deletes mail when you ask it to (or block a sender), and deleted mail goes to your Trash (Deleted Items in Outlook).");
                });
                ui.add_space(14.0);

                ui.add_enabled_ui(!self.signing_in, |ui| {
                    ui.horizontal(|ui| {
                        for p in Provider::ALL {
                            ui.selectable_value(&mut self.login_provider, p, p.label());
                        }
                    });
                });
                ui.add_space(10.0);

                match self.login_provider {
                    Provider::Gmail => self.gmail_login_fields(ui),
                    Provider::Outlook => self.microsoft_login_fields(ui),
                }
                if self.adding_account
                    && self.device_code.is_none()
                    && ui.add_enabled(!self.signing_in, egui::Button::new("Cancel")).clicked()
                {
                    self.adding_account = false;
                    self.login_email.clear();
                    self.login_password.clear();
                }
            });
        });
    }

    fn primary_button(ui: &egui::Ui, label: &str) -> egui::Button<'static> {
        egui::Button::new(egui::RichText::new(label).strong().color(egui::Color32::WHITE))
            .fill(ui.visuals().hyperlink_color)
            .min_size(egui::vec2(ui.available_width(), 34.0))
    }

    fn gmail_login_fields(&mut self, ui: &mut egui::Ui) {
        ui.label("Email");
        ui.add(
            egui::TextEdit::singleline(&mut self.login_email)
                .hint_text("you@gmail.com")
                .desired_width(f32::INFINITY),
        );
        if Provider::detect(&self.login_email) == Some(Provider::Outlook) {
            ui.horizontal_wrapped(|ui| {
                ui.weak("That's a Microsoft address.");
                if ui.link("Sign in with Outlook instead").clicked() {
                    self.login_provider = Provider::Outlook;
                }
            });
        }
        ui.add_space(8.0);
        ui.label("App Password");
        let pw = ui.add(
            egui::TextEdit::singleline(&mut self.login_password)
                .password(true)
                .hint_text("16 letters")
                .desired_width(f32::INFINITY),
        );
        let enter = pw.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));

        ui.add_space(14.0);
        let ready = !self.signing_in
            && !self.login_email.trim().is_empty()
            && !self.login_password.trim().is_empty();
        let label = if self.signing_in { "Signing in…" } else { "Sign in" };
        let clicked = ui.add_enabled(ready, Self::primary_button(ui, label)).clicked();
        if (clicked || enter) && ready {
            self.submit_login();
        }

        ui.add_space(16.0);
        ui.separator();
        ui.add_space(8.0);
        ui.weak("Use an App Password, not your normal Google password. It needs 2-Step Verification turned on.");
        ui.hyperlink_to("Create an App Password", auth::APP_PASSWORD_URL);
        ui.add_space(8.0);
    }

    /// Outlook sign-in: Microsoft's device code flow. The user signs in on
    /// Microsoft's page in their browser and types the code shown here.
    fn microsoft_login_fields(&mut self, ui: &mut egui::Ui) {
        if let Some((code, uri)) = self.device_code.clone() {
            ui.label("1. Open this page in your browser:");
            ui.hyperlink_to(format!("{uri} ↗"), &uri);
            ui.add_space(8.0);
            ui.label("2. Enter this code and sign in to your Microsoft account:");
            ui.horizontal(|ui| {
                ui.add(egui::Label::new(egui::RichText::new(&code).monospace().size(26.0).strong()).selectable(true));
                if ui.button("Copy").clicked() {
                    ui.ctx().copy_text(code.clone());
                }
            });
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                ui.spinner();
                ui.weak("Waiting for you to finish signing in…");
            });
            ui.add_space(8.0);
            if ui.button("Cancel sign-in").clicked() {
                self.worker.cancel_sign_in();
            }
            ui.add_space(8.0);
            return;
        }

        ui.label("For Outlook.com, Hotmail, Live, MSN and Microsoft 365 accounts.");
        ui.weak("Microsoft doesn't allow passwords for this, so you sign in on Microsoft's own page in your browser. Gatekeep never sees your password.");
        ui.add_space(12.0);
        let client_id = self.config.microsoft_client_id();
        let label = if self.signing_in { "Contacting Microsoft…" } else { "Sign in with Microsoft" };
        let ready = !self.signing_in && client_id.is_some();
        if ui.add_enabled(ready, Self::primary_button(ui, label)).clicked()
            && let Some(id) = client_id.clone()
        {
            self.clear_errors();
            self.signing_in = true;
            self.worker.sign_in_microsoft(id);
        }
        if client_id.is_none() {
            ui.add_space(8.0);
            ui.colored_label(
                ui.visuals().warn_fg_color,
                "Outlook sign-in isn't set up in this build. Add your Microsoft app's client ID as \
                 microsoft_client_id in config.toml (see the README), then restart Gatekeep.",
            );
            if let Some(dir) = crate::config::config_dir() {
                ui.weak(format!("Config folder: {}", dir.display()));
            }
        }
        ui.add_space(8.0);
    }

    fn submit_login(&mut self) {
        let password = std::mem::take(&mut self.login_password);
        match Account::gmail(&self.login_email, &password) {
            Ok(account) => {
                self.clear_errors();
                self.signing_in = true;
                self.worker.send(Command::SignIn(account));
            }
            Err(e) => self.push_error(e.to_string()),
        }
    }
}

impl GatekeepApp {
    fn inbox_toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            let refresh = ui
                .button("⟳ Refresh")
                .on_hover_text("Fetch new mail and clean up blocked senders in every account (F5)");
            let f5 = ui.input(|i| i.key_pressed(egui::Key::F5));
            if refresh.clicked() || f5 {
                self.refresh();
            }
            let minutes = self.config.auto_refresh_minutes;
            let label = |m: u32| if m == 0 { "Auto-refresh: off".to_owned() } else { format!("Auto-refresh: {m} min") };
            let mut chosen = minutes;
            egui::ComboBox::from_id_salt("auto_refresh")
                .selected_text(label(minutes))
                .show_ui(ui, |ui| {
                    for m in AUTO_REFRESH_CHOICES {
                        ui.selectable_value(&mut chosen, m, label(m));
                    }
                });
            if chosen != minutes {
                self.config.auto_refresh_minutes = chosen;
                self.save_config();
            }
            ui.separator();
            if ui.button("🗑 Delete emails…").on_hover_text("Move every email matching a filter to Trash").clicked() {
                self.delete.open = true;
            }
            let n = self.config.blocked_senders.len();
            if ui.button(format!("⛔ Blocked ({n})…")).clicked() {
                self.blocked.open = true;
            }
            let clean = ui
                .add_enabled(n > 0, egui::Button::new("🧹 Delete blocked mail"))
                .on_hover_text("Search every account and move all mail from blocked senders and domains to Trash")
                .on_disabled_hover_text("No blocked senders yet");
            if clean.clicked() {
                let blocked = self.config.blocked_senders.clone();
                self.send(Command::TrashBlocked { blocked }, "Deleting blocked mail…");
                let ahead = self.in_flight - 1;
                self.sweep = Some(Sweep {
                    done_left: self.in_flight,
                    fraction: 0.0,
                    text: match ahead {
                        0 => "Starting…".into(),
                        n => format!("Waiting for {n} earlier task{} to finish…", if n == 1 { "" } else { "s" }),
                    },
                });
            }
            let checked = self.inbox.checked.len();
            if checked > 0 {
                ui.separator();
                let delete = egui::Button::new(format!("🗑 Delete {checked} selected"));
                if ui.add(delete).on_hover_text("Move the checked emails to Trash").clicked() {
                    self.trash_checked();
                }
                if ui.small_button("Clear selection").clicked() {
                    self.inbox.checked.clear();
                }
            }
            ui.separator();
            if self.busy() {
                ui.spinner();
                ui.weak(self.status_label());
            } else if let Some((at, _)) = self.inbox.updated {
                ui.weak(format!("Updated {}", at.format("%H:%M")));
            }
        });
    }

    fn message_list(&mut self, ui: &mut egui::Ui, visible: &[usize]) {
        egui::Panel::bottom("list_footer")
            .frame(egui::Frame::new().inner_margin(egui::Margin::symmetric(10, 8)))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    let loaded = self.inbox.messages.len();
                    ui.weak(format!("{loaded} of {} loaded", self.inbox.total));
                    if loaded < self.inbox.total {
                        let label = format!("Load {} more", self.config.page_size);
                        if ui.button(label).clicked() {
                            self.load_older();
                        }
                    }
                    if !visible.is_empty() && ui.small_button("Select all shown").clicked() {
                        for &i in visible {
                            let uid = self.inbox.messages[i].uid;
                            self.inbox.checked.insert(uid);
                        }
                    }
                });
            });
        if visible.is_empty() {
            ui.add_space(48.0);
            ui.vertical_centered(|ui| {
                if self.busy() && self.inbox.messages.is_empty() {
                    ui.spinner();
                    ui.label("Loading inbox…");
                } else {
                    ui.label(egui::RichText::new("📭").size(36.0));
                    ui.add_space(6.0);
                    ui.weak("Nothing in this view.");
                }
            });
            return;
        }
        // Up/Down move the selection within the current view.
        let step = ui.input(|i| {
            i.key_pressed(egui::Key::ArrowDown) as i32 - i.key_pressed(egui::Key::ArrowUp) as i32
        });
        let typing = ui.ctx().egui_wants_keyboard_input();
        if !typing
            && ui.input(|i| i.key_pressed(egui::Key::Delete))
            && let Some(uid) = self.inbox.selected
        {
            self.trash_message(uid);
            return;
        }
        let mut scroll_to = None;
        if step != 0 && !typing {
            let pos = self
                .inbox
                .selected
                .and_then(|sel| visible.iter().position(|&i| self.inbox.messages[i].uid == sel));
            let next = match pos {
                Some(p) => (p as i32 + step).clamp(0, visible.len() as i32 - 1) as usize,
                None => 0,
            };
            self.inbox.selected = Some(self.inbox.messages[visible[next]].uid);
            scroll_to = Some(next);
        }

        let mut area = egui::ScrollArea::vertical().auto_shrink(false);
        if let Some(row) = scroll_to {
            // Keep the keyboard selection on screen.
            let row_top = row as f32 * ROW_HEIGHT;
            let view_h = ui.available_height();
            let id = ui.make_persistent_id("message_list_scroll");
            let offset = egui::scroll_area::State::load(ui.ctx(), id).map_or(0.0, |s| s.clamped_offset().y);
            if row_top < offset {
                area = area.vertical_scroll_offset(row_top);
            } else if row_top + ROW_HEIGHT > offset + view_h {
                area = area.vertical_scroll_offset(row_top + ROW_HEIGHT - view_h);
            }
        }
        area.id_salt("message_list_scroll")
            .show_rows(ui, ROW_HEIGHT, visible.len(), |ui, rows| {
                for row in rows {
                    let i = visible[row];
                    let msg = &self.inbox.messages[i];
                    let uid = msg.uid;
                    let selected = self.inbox.selected == Some(uid);
                    let mut checked = self.inbox.checked.contains(&uid);
                    if message_row(ui, msg, &self.inbox.verdicts[i], selected, &mut checked).clicked() {
                        self.inbox.selected = Some(uid);
                    }
                    if checked {
                        self.inbox.checked.insert(uid);
                    } else {
                        self.inbox.checked.remove(&uid);
                    }
                }
            });
    }

    fn reading_pane(&self, ui: &mut egui::Ui, account: Option<&str>, provider: Provider) -> Option<PaneAction> {
        let Some(i) = self.inbox.selected.and_then(|uid| self.inbox.find(uid)) else {
            ui.vertical_centered(|ui| {
                ui.add_space(ui.available_height() / 2.0 - 40.0);
                ui.label(egui::RichText::new("📬").size(44.0));
                ui.add_space(6.0);
                ui.weak("Select a message to read it.");
            });
            return None;
        };
        let msg = &self.inbox.messages[i];
        let mut action = None;
        ui.add_space(4.0);
        ui.horizontal_wrapped(|ui| {
            if ui
                .button("🗑 Delete")
                .on_hover_text("Move this email to Trash (Delete key)")
                .clicked()
            {
                action = Some(PaneAction::Trash(msg.uid));
            }
            if let Some(from) = &msg.from {
                if ui
                    .button("Delete all from sender")
                    .on_hover_text(format!("Move every email from {} to Trash", from.email))
                    .clicked()
                {
                    action = Some(PaneAction::DeleteFrom(from.email.clone()));
                }
                if ui
                    .button("Review sender deletion…")
                    .on_hover_text(format!("Open the Delete emails window filled in for {}", from.email))
                    .clicked()
                {
                    action = Some(PaneAction::ReviewFrom(from.email.clone()));
                }
                let blocked_by = filter::blocked_by(&self.config.blocked_senders, &from.email);
                if ui
                    .add_enabled(blocked_by.is_none(), egui::Button::new("⛔ Block sender"))
                    .on_hover_text("Move all existing and future mail from this sender to Trash, in every account")
                    .on_disabled_hover_text(format!("Already blocked by \"{}\"", blocked_by.unwrap_or_default()))
                    .clicked()
                {
                    action = Some(PaneAction::Block(from.email.clone()));
                }
                if let Some(domain) = filter::domain_of(&from.email) {
                    // A blocked parent domain already covers this one.
                    let domain_blocked = filter::blocked_by(&self.config.blocked_senders, &format!("x@{domain}"))
                        .filter(|b| !b.contains('@'));
                    if ui
                        .add_enabled(domain_blocked.is_none(), egui::Button::new("⛔ Block domain"))
                        .on_hover_text(format!(
                            "Move all existing and future mail from any address at {domain} to Trash, in every account"
                        ))
                        .on_disabled_hover_text(format!("Already blocked by \"{}\"", domain_blocked.unwrap_or_default()))
                        .clicked()
                    {
                        action = Some(PaneAction::Block(domain));
                    }
                }
            }
        });
        ui.add_space(8.0);

        // Header card: who it's from, when, and the subject.
        theme::card(ui).show(ui, |ui| {
            ui.horizontal(|ui| {
                let name = msg.from.as_ref().map_or("(unknown sender)", |a| a.short());
                let (avatar, _) = ui.allocate_exact_size(egui::vec2(44.0, 44.0), egui::Sense::hover());
                theme::paint_avatar(ui.painter(), avatar, name);
                ui.add_space(4.0);
                ui.vertical(|ui| {
                    ui.add(egui::Label::new(egui::RichText::new(name).strong()).truncate());
                    if let Some(from) = msg.from.as_ref().filter(|a| !a.name.is_empty()) {
                        ui.add(egui::Label::new(egui::RichText::new(&from.email).weak()).truncate());
                    }
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                    if let Some(date) = msg.date {
                        ui.weak(date.format("%a, %e %b %Y · %H:%M").to_string());
                    }
                });
            });
            ui.add_space(10.0);
            let subject = if msg.subject.is_empty() { "(no subject)" } else { &msg.subject };
            ui.add(egui::Label::new(egui::RichText::new(subject).heading().strong()).wrap());

            let recipients = [("To", join(&msg.to)), ("Cc", join(&msg.cc))];
            if recipients.iter().any(|(_, v)| !v.is_empty()) {
                ui.add_space(8.0);
                egui::Grid::new("headers").num_columns(2).spacing([10.0, 3.0]).show(ui, |ui| {
                    for (name, value) in recipients {
                        if !value.is_empty() {
                            ui.weak(name);
                            ui.add(egui::Label::new(value).wrap());
                            ui.end_row();
                        }
                    }
                });
            }

            ui.add_space(8.0);
            ui.horizontal_wrapped(|ui| {
                // Outlook on the web has no link to a message by its Message-ID.
                if provider == Provider::Gmail
                    && let Some(url) = msg.web_url(account)
                {
                    ui.hyperlink_to("Open in Gmail ↗", url);
                }
                let verdict = &self.inbox.verdicts[i];
                for &r in &verdict.matched {
                    if let Some(rule) = self.config.rules.get(r) {
                        let chip = egui::RichText::new(format!("rule: {}", rule.display_name())).small();
                        egui::Frame::new()
                            .fill(ui.visuals().hyperlink_color.gamma_multiply(0.2))
                            .corner_radius(10.0)
                            .inner_margin(egui::Margin::symmetric(8, 2))
                            .show(ui, |ui| ui.label(chip));
                    }
                }
            });
        });

        ui.add_space(10.0);
        egui::ScrollArea::vertical()
            .id_salt(("body", msg.uid))
            .auto_shrink(false)
            .show(ui, |ui| {
                ui.set_max_width(760.0);
                if msg.body.trim().is_empty() {
                    ui.weak(format!(
                        "No text body was downloaded. Messages over 1 MB (usually large attachments) are listed from their headers only; open it in {} to read it.",
                        provider.name()
                    ));
                } else {
                    ui.add(egui::Label::new(&msg.body).wrap().selectable(true));
                }
            });
        action
    }

    fn delete_window(&mut self, ctx: &egui::Context) {
        let provider = self.active_provider();
        let mut open = self.delete.open;
        let mut send = None;
        egui::Window::new("Delete emails")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(460.0)
            .show(ctx, |ui| {
                ui.label(format!(
                    "Find emails anywhere in the current account (not only the loaded ones) and move them to {}'s {}.",
                    provider.name(),
                    provider.trash_name()
                ));
                ui.add_space(6.0);
                let f = &mut self.delete.filter;
                egui::Grid::new("delete_filter").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
                    ui.label("From");
                    ui.add(egui::TextEdit::singleline(&mut f.from).hint_text("address or domain").desired_width(280.0));
                    ui.end_row();
                    ui.label("Subject contains");
                    ui.add(egui::TextEdit::singleline(&mut f.subject).desired_width(280.0));
                    ui.end_row();
                    ui.label("Older than");
                    ui.horizontal(|ui| {
                        ui.add(egui::DragValue::new(&mut f.older_than_days).range(0..=36500).suffix(" days"));
                        ui.weak("(0 = any age)");
                    });
                    ui.end_row();
                    // Only Gmail understands its search syntax.
                    if provider == Provider::Gmail {
                        ui.label("Gmail search");
                        ui.add(
                            egui::TextEdit::singleline(&mut f.raw)
                                .hint_text("optional, e.g. has:attachment larger:5M")
                                .desired_width(280.0),
                        );
                        ui.end_row();
                    }
                    ui.label("");
                    ui.checkbox(&mut f.inbox_only, "Only emails in the Inbox");
                    ui.end_row();
                });
                ui.add_space(6.0);
                let search = self.delete.filter.search(provider);
                match &search {
                    Some(s) => ui.weak(format!("Search: {}", s.describe(provider))),
                    None if provider == Provider::Gmail => {
                        ui.weak("Fill in at least one of From, Subject, Older than, or Gmail search.")
                    }
                    None => ui.weak("Fill in at least one of From, Subject, or Older than."),
                };
                let find = ui.add_enabled(search.is_some(), egui::Button::new("🔍 Find matching emails"));
                if find.clicked() {
                    self.delete.result = None;
                    send = search.clone().map(|search| (Command::Search { search }, "Searching…"));
                }
                if let Some(r) = &self.delete.result {
                    ui.separator();
                    if Some(&r.search) != search.as_ref() {
                        ui.weak("The filter changed. Search again before deleting.");
                    } else if r.count == 0 {
                        ui.label("No emails match.");
                    } else {
                        let s = if r.count == 1 { "" } else { "s" };
                        ui.strong(format!("{} email{s} match.", r.count));
                        if !r.sample.is_empty() {
                            ui.weak("Most recent:");
                            for m in &r.sample {
                                let date = m.date.map(short_date).unwrap_or_default();
                                let from = m.from.as_ref().map_or("?", |a| a.short());
                                let subject = if m.subject.is_empty() { "(no subject)" } else { &m.subject };
                                ui.add(egui::Label::new(format!("{date}  {from} — {subject}")).truncate());
                            }
                        }
                        ui.add_space(6.0);
                        let danger = egui::Button::new(
                            egui::RichText::new(format!("Move {} email{s} to {}", r.count, provider.trash_name()))
                                .color(egui::Color32::WHITE),
                        )
                        .fill(ui.visuals().error_fg_color);
                        if ui.add(danger).clicked() {
                            send = Some((Command::TrashMatching { search: r.search.clone() }, "Moving to Trash…"));
                        }
                        ui.weak(match provider {
                            Provider::Gmail => "Gmail keeps Trash for 30 days, then deletes it permanently.",
                            Provider::Outlook => "You can restore them from Deleted Items in Outlook until it's emptied.",
                        });
                    }
                }
            });
        if let Some((cmd, status)) = send {
            if matches!(cmd, Command::TrashMatching { .. }) {
                self.delete.result = None;
                open = false;
            }
            self.send(cmd, status);
        }
        self.delete.open = open;
    }

    fn blocked_window(&mut self, ctx: &egui::Context) {
        let mut open = self.blocked.open;
        let mut remove = None;
        let mut add = None;
        egui::Window::new("Blocked senders")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(420.0)
            .show(ctx, |ui| {
                ui.label("Mail from these addresses and domains is moved to Trash on every refresh, in every signed-in account, including mail that's already there.");
                if self.config.auto_refresh_minutes == 0 {
                    ui.weak("Auto-refresh is off, so this only happens when you click Refresh.");
                }
                ui.weak("Gatekeep has to be running for this to work. To block on the server even when Gatekeep is closed, add a filter in Gmail's settings or a blocked sender in Outlook's settings.");
                ui.add_space(6.0);
                if self.config.blocked_senders.is_empty() {
                    ui.weak("No blocked senders. Add an address, or a domain to block everyone at it.");
                } else {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.blocked.search)
                            .hint_text("🔍 Search blocked senders")
                            .desired_width(f32::INFINITY),
                    );
                    ui.add_space(4.0);
                    let needle = self.blocked.search.trim().to_lowercase();
                    let mut shown = 0;
                    // Keep the real index so "✕" removes the right entry while filtering.
                    for (i, sender) in self.config.blocked_senders.iter().enumerate() {
                        if !needle.is_empty() && !sender.to_lowercase().contains(&needle) {
                            continue;
                        }
                        shown += 1;
                        ui.horizontal(|ui| {
                            if ui.small_button("✕").on_hover_text("Unblock").clicked() {
                                remove = Some(i);
                            }
                            ui.label(sender);
                        });
                    }
                    if shown == 0 {
                        ui.weak("No blocked senders match your search.");
                    }
                }
                ui.separator();
                ui.horizontal(|ui| {
                    let edit = ui.add(
                        egui::TextEdit::singleline(&mut self.blocked.input)
                            .hint_text("news@shop.com or shop.com")
                            .desired_width(240.0),
                    );
                    let enter = edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    if ui.button("Block").clicked() || enter {
                        match filter::validate_sender(&self.blocked.input) {
                            Ok(s) => add = Some(s),
                            Err(e) => self.blocked.input_error = Some(e),
                        }
                    }
                });
                if let Some(e) = self.blocked.input_error {
                    ui.colored_label(ui.visuals().error_fg_color, e);
                }
            });
        self.blocked.open = open;
        if let Some(i) = remove {
            self.config.blocked_senders.remove(i);
            self.save_config();
        }
        if let Some(s) = add {
            self.blocked.input.clear();
            self.blocked.input_error = None;
            self.blocked.confirm = Some(s);
        }
    }

    /// Locks the UI while blocked mail is being cleaned up, with a progress bar.
    fn sweep_window(&self, ctx: &egui::Context) {
        let Some(sweep) = &self.sweep else { return };
        egui::Modal::new(egui::Id::unique("sweep_progress")).show(ctx, |ui| {
            ui.set_width(380.0);
            ui.heading("Deleting blocked mail");
            ui.add_space(6.0);
            ui.add(egui::ProgressBar::new(sweep.fraction).show_percentage().animate(true));
            ui.add_space(4.0);
            ui.label(&sweep.text);
            ui.weak("Mail from blocked senders and domains is moved to Trash in every account.");
        });
    }

    /// Ask before blocking, since it immediately trashes existing mail.
    fn block_confirm_window(&mut self, ctx: &egui::Context) {
        let Some(sender) = self.blocked.confirm.clone() else { return };
        let mut decision = None;
        egui::Modal::new(egui::Id::unique("block_confirm")).show(ctx, |ui| {
            ui.set_max_width(380.0);
            ui.heading(format!("Block {sender}?"));
            ui.add_space(4.0);
            if sender.contains('@') {
                ui.label("All existing mail from this sender will be moved to Trash now in every signed-in account, and new mail on every refresh.");
            } else {
                ui.label(format!(
                    "All existing mail from any address at {sender} (or its subdomains) will be moved to Trash now in every signed-in account, and new mail on every refresh."
                ));
                if filter::is_public_mail_domain(&sender) {
                    ui.add_space(4.0);
                    ui.colored_label(
                        ui.visuals().error_fg_color,
                        format!("⚠ {sender} is a public mail provider. Blocking it trashes mail from everyone who uses it, not only this sender."),
                    );
                }
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let block = egui::Button::new(egui::RichText::new("Block and delete").color(egui::Color32::WHITE))
                    .fill(ui.visuals().error_fg_color);
                if ui.add(block).clicked() {
                    decision = Some(true);
                }
                if ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    decision = Some(false);
                }
            });
        });
        match decision {
            Some(true) => {
                self.blocked.confirm = None;
                if !self.config.blocked_senders.iter().any(|b| b.eq_ignore_ascii_case(&sender)) {
                    self.config.blocked_senders.push(sender);
                    self.save_config();
                }
                // The worker runs commands in order, so this runs after anything pending.
                self.refresh();
            }
            Some(false) => self.blocked.confirm = None,
            None => {}
        }
    }

    fn inbox_ui(&mut self, ui: &mut egui::Ui) {
        let account = self.active_email().map(str::to_owned);
        let provider = self.active_provider();
        egui::Panel::top("inbox_toolbar")
            .frame(egui::Frame::new().inner_margin(egui::Margin::symmetric(12, 10)))
            .show(ui, |ui| self.inbox_toolbar(ui));
        let visible = self.inbox.visible();
        let list_bg = ui.visuals().extreme_bg_color;
        egui::Panel::left("message_list")
            .resizable(true)
            .default_size(420.0)
            .size_range(260.0..=800.0)
            .frame(egui::Frame::new().fill(list_bg).inner_margin(egui::Margin::symmetric(6, 6)))
            .show(ui, |ui| self.message_list(ui, &visible));
        let action = egui::CentralPanel::default()
            .frame(egui::Frame::new().inner_margin(egui::Margin::symmetric(18, 12)))
            .show(ui, |ui| self.reading_pane(ui, account.as_deref(), provider))
            .inner;
        match action {
            Some(PaneAction::Trash(uid)) => self.trash_message(uid),
            Some(PaneAction::DeleteFrom(email)) => {
                let filter = DeleteFilter { from: email, ..Default::default() };
                if let Some(search) = filter.search(provider) {
                    self.send(Command::TrashMatching { search }, "Moving to Trash…");
                }
            }
            Some(PaneAction::ReviewFrom(email)) => {
                self.delete = DeleteDialog {
                    open: true,
                    filter: DeleteFilter { from: email, ..Default::default() },
                    result: None,
                };
            }
            Some(PaneAction::Block(email)) => match filter::validate_sender(&email) {
                Ok(s) => self.blocked.confirm = Some(s),
                Err(e) => self.push_error(e.to_string()),
            },
            None => {}
        }
        let ctx = ui.ctx().clone();
        self.delete_window(&ctx);
        self.blocked_window(&ctx);
        self.block_confirm_window(&ctx);
    }
}

/// One clickable card-like row: an avatar, then sender and date, then subject and
/// snippet, with a selection checkbox at the right edge.
fn message_row(
    ui: &mut egui::Ui,
    msg: &Message,
    verdict: &Verdict,
    selected: bool,
    checked: &mut bool,
) -> egui::Response {
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), ROW_HEIGHT), egui::Sense::click());
    let accent = ui.visuals().hyperlink_color;
    let card = rect.shrink2(egui::vec2(2.0, 3.0));
    let fill = if selected {
        Some(accent.gamma_multiply(0.22))
    } else if verdict.highlight {
        Some(ui.visuals().warn_fg_color.gamma_multiply(0.14))
    } else if response.hovered() {
        Some(ui.visuals().widgets.hovered.weak_bg_fill)
    } else {
        None
    };
    if let Some(fill) = fill {
        ui.painter().rect_filled(card, theme::RADIUS, fill);
    }
    if selected {
        // Accent bar on the left edge marks the open message.
        let bar = egui::Rect::from_min_size(card.left_top(), egui::vec2(3.0, card.height()));
        ui.painter().rect_filled(bar, 2.0, accent);
    }
    // Dim low-priority mail when it appears alongside important mail in "All".
    let dim = verdict.is_low_priority();
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(card.shrink2(egui::vec2(10.0, 6.0))));
    if dim {
        child.multiply_opacity(0.6);
    }
    let strong = |t: &str| {
        let t = egui::RichText::new(t);
        if msg.unread { t.strong() } else { t }
    };
    let sender = msg.from.as_ref().map_or("(unknown sender)", |a| a.short());
    child.horizontal(|ui| {
        let (avatar, _) = ui.allocate_exact_size(egui::vec2(36.0, 36.0), egui::Sense::hover());
        theme::paint_avatar(ui.painter(), avatar, sender);
        if msg.unread {
            // Unread dot on the avatar's top-right edge.
            ui.painter().circle_filled(avatar.right_top() + egui::vec2(-3.0, 3.0), 4.5, accent);
        }
        ui.add_space(8.0);
        ui.vertical(|ui| {
            // Leave the right edge free for the checkbox.
            ui.set_max_width(ui.available_width() - CHECK_WIDTH);
            ui.horizontal(|ui| {
                let date = msg.date.map(short_date).unwrap_or_default();
                let date_w = 56.0;
                ui.add_sized(
                    [ui.available_width() - date_w, 18.0],
                    egui::Label::new(strong(sender)).truncate().selectable(false),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add(egui::Label::new(egui::RichText::new(date).small().weak()).selectable(false));
                });
            });
            let subject = if msg.subject.is_empty() { "(no subject)" } else { &msg.subject };
            let mut line = egui::text::LayoutJob::default();
            let font = egui::TextStyle::Body.resolve(ui.style());
            let text_color = if msg.unread { ui.visuals().strong_text_color() } else { ui.visuals().text_color() };
            line.append(subject, 0.0, egui::TextFormat::simple(font.clone(), text_color));
            if !msg.snippet.is_empty() {
                let weak = ui.visuals().weak_text_color();
                line.append(" — ", 0.0, egui::TextFormat::simple(font.clone(), weak));
                line.append(&msg.snippet, 0.0, egui::TextFormat::simple(font, weak));
            }
            ui.add(egui::Label::new(line).truncate().selectable(false));
        });
        ui.add(egui::Checkbox::without_text(checked)).on_hover_text("Select to delete in bulk");
    });
    response
}

/// Time for today's mail, month and day for this year, full date otherwise.
fn short_date(d: DateTime<Local>) -> String {
    let now = Local::now();
    if d.date_naive() == now.date_naive() {
        d.format("%H:%M").to_string()
    } else if d.year() == now.year() {
        d.format("%b %e").to_string()
    } else {
        d.format("%Y-%m-%d").to_string()
    }
}

fn join(addrs: &[crate::model::Address]) -> String {
    addrs.iter().map(ToString::to_string).collect::<Vec<_>>().join(", ")
}

impl eframe::App for GatekeepApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.drain_events();
        self.auto_refresh(ui.ctx());
        if self.sweep.is_some() {
            // The progress modal blocks the mouse; drop key presses too so
            // shortcuts (Delete, F5, arrows) can't act behind it.
            ui.ctx().input_mut(|i| i.events.retain(|e| !matches!(e, egui::Event::Key { .. } | egui::Event::Text(_))));
        }

        egui::Panel::top("top_bar")
            .frame(egui::Frame::new().inner_margin(egui::Margin::symmetric(14, 8)))
            .show(ui, |ui| {
                self.top_bar(ui);
            });
        if matches!(self.auth, AuthState::SignedIn { .. }) && !self.adding_account {
            self.inbox_ui(ui);
        } else {
            egui::CentralPanel::default().show(ui, |ui| {
                if self.adding_account || self.auth == AuthState::SignedOut {
                    self.login_form(ui);
                }
            });
        }
        let ctx = ui.ctx().clone();
        self.toasts_ui(&ctx);
        self.sweep_window(&ctx);
    }
}
