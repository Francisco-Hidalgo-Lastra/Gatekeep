//! A copy of Gatekeep's signed-in UI (`src/app.rs` in the main crate) driven by
//! fictional data. Actions only change local state, so every window and state
//! can be staged for a screenshot.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use chrono::{DateTime, Datelike, Local};
use eframe::egui;

use crate::data::{self, Address, Message};
use crate::theme;

const AUTO_REFRESH_CHOICES: [u32; 5] = [0, 1, 5, 15, 30];
const TOAST_LIFETIME: Duration = Duration::from_secs(4);
const ROW_HEIGHT: f32 = 66.0;
const CHECK_WIDTH: f32 = 28.0;
const TOAST_WIDTH: f32 = 340.0;
/// The fictional INBOX size on the "server".
const TOTAL: usize = 1_284;

pub struct DemoApp {
    active: usize,
    messages: Vec<Message>,
    selected: Option<u32>,
    checked: HashSet<u32>,
    total: usize,
    updated: DateTime<Local>,
    auto_refresh: u32,
    blocked_senders: Vec<String>,
    toasts: Vec<Toast>,
    /// Fake "busy" spinner in the toolbar until this instant.
    busy_until: Option<Instant>,
    sweep: Option<Sweep>,
    wipe: Option<String>,
    delete: DeleteDialog,
    blocked: BlockedDialog,
}

#[derive(Default)]
struct DeleteDialog {
    open: bool,
    from: String,
    subject: String,
    older_than_days: u32,
    raw: String,
    inbox_only: bool,
    /// The `from` the last fake search ran for.
    searched: Option<String>,
}

#[derive(Default)]
struct BlockedDialog {
    open: bool,
    search: String,
    input: String,
    confirm: Option<String>,
}

struct Sweep {
    title: &'static str,
    note: String,
    started: Instant,
}

struct Toast {
    message: String,
    shown: Instant,
}

enum PaneAction {
    Trash(u32),
    DeleteFrom(String),
    ReviewFrom(String),
    Block(String),
}

impl DemoApp {
    pub fn new(cc: &eframe::CreationContext<'_>, theme_pref: Option<egui::ThemePreference>) -> Self {
        theme::install(&cc.egui_ctx);
        cc.egui_ctx.set_theme(theme_pref.unwrap_or(egui::ThemePreference::System));
        let messages = data::messages();
        let selected = messages.first().map(|m| m.uid);
        let checked = messages.iter().skip(3).take(2).map(|m| m.uid).collect();
        Self {
            active: 0,
            messages,
            selected,
            checked,
            total: TOTAL,
            updated: Local::now(),
            auto_refresh: 5,
            blocked_senders: data::BLOCKED.iter().map(|s| s.to_string()).collect(),
            toasts: vec![Toast::new("Moved 23 emails from blocked senders to Trash.")],
            busy_until: None,
            sweep: None,
            wipe: None,
            delete: DeleteDialog::default(),
            blocked: BlockedDialog::default(),
        }
    }

    fn busy(&self) -> bool {
        self.busy_until.is_some_and(|t| Instant::now() < t)
    }

    fn fake_work(&mut self, ctx: &egui::Context) {
        self.busy_until = Some(Instant::now() + Duration::from_millis(900));
        ctx.request_repaint_after(Duration::from_millis(950));
    }

    fn toast(&mut self, message: impl Into<String>) {
        self.toasts.push(Toast::new(message));
    }

    fn find(&self, uid: u32) -> Option<usize> {
        self.messages.iter().position(|m| m.uid == uid)
    }

    fn trash_message(&mut self, uid: u32) {
        if self.selected == Some(uid) {
            let pos = self.find(uid);
            self.selected = pos
                .and_then(|p| self.messages.get(p + 1).or_else(|| p.checked_sub(1).and_then(|q| self.messages.get(q))))
                .map(|m| m.uid);
        }
        if let Some(i) = self.find(uid) {
            self.messages.remove(i);
            self.total -= 1;
        }
    }

    fn trash_from(&mut self, sender: &str) -> usize {
        let sender = sender.to_lowercase();
        let uids: Vec<u32> = self
            .messages
            .iter()
            .filter(|m| m.from.email.to_lowercase().ends_with(&sender))
            .map(|m| m.uid)
            .collect();
        for &uid in &uids {
            self.trash_message(uid);
        }
        uids.len()
    }
}

impl Toast {
    fn new(message: impl Into<String>) -> Self {
        Self { message: message.into(), shown: Instant::now() }
    }
}

impl DemoApp {
    fn top_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            theme::logo(ui, 26.0);
            ui.heading("Gatekeep");
            ui.add_space(6.0);
            let mut chosen = self.active;
            egui::ComboBox::from_id_salt("account_switch")
                .selected_text(data::ACCOUNTS[self.active].0)
                .show_ui(ui, |ui| {
                    for (i, (email, provider)) in data::ACCOUNTS.iter().enumerate() {
                        ui.selectable_value(&mut chosen, i, format!("{email} ({provider})"));
                    }
                });
            self.active = chosen;
            let _ = ui.button("+ Add account");
            let _ = ui.button("Sign out");
        });
    }

    fn inbox_toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            let refresh = ui.button("⟳ Refresh").on_hover_text("Fetch new mail and clean up blocked senders in every account (F5)");
            if refresh.clicked() || ui.input(|i| i.key_pressed(egui::Key::F5)) {
                self.fake_work(ui.ctx());
                self.updated = Local::now();
            }
            let label = |m: u32| if m == 0 { "Auto-refresh: off".to_owned() } else { format!("Auto-refresh: {m} min") };
            egui::ComboBox::from_id_salt("auto_refresh")
                .selected_text(label(self.auto_refresh))
                .show_ui(ui, |ui| {
                    for m in AUTO_REFRESH_CHOICES {
                        ui.selectable_value(&mut self.auto_refresh, m, label(m));
                    }
                });
            ui.separator();
            if ui.button("🗑 Delete emails…").on_hover_text("Move every email matching a filter to Trash").clicked() {
                self.delete.open = true;
            }
            let n = self.blocked_senders.len();
            if ui.button(format!("⛔ Blocked ({n})…")).clicked() {
                self.blocked.open = true;
            }
            let clean = ui
                .add_enabled(n > 0, egui::Button::new("🧹 Delete blocked mail"))
                .on_hover_text("Search every account and move all mail from blocked senders and domains to Trash");
            if clean.clicked() {
                self.sweep = Some(Sweep {
                    title: "Deleting blocked mail",
                    note: "Mail from blocked senders and domains is moved to Trash in every account.".into(),
                    started: Instant::now(),
                });
            }
            let wipe = ui
                .button(egui::RichText::new("🗑 Delete all email…").color(ui.visuals().error_fg_color))
                .on_hover_text("Move every email in this account to Trash");
            if wipe.clicked() {
                self.wipe = Some(String::new());
            }
            let checked = self.checked.len();
            if checked > 0 {
                ui.separator();
                if ui.button(format!("🗑 Delete {checked} selected")).clicked() {
                    let uids: Vec<u32> = self.checked.drain().collect();
                    for uid in &uids {
                        self.trash_message(*uid);
                    }
                    self.toast(format!("Moved {} emails to Trash.", uids.len()));
                }
                if ui.small_button("Clear selection").clicked() {
                    self.checked.clear();
                }
            }
            ui.separator();
            if self.busy() {
                ui.spinner();
                ui.weak("Refreshing…");
            } else {
                ui.weak(format!("Updated {}", self.updated.format("%H:%M")));
            }
        });
    }

    fn message_list(&mut self, ui: &mut egui::Ui) {
        egui::Panel::bottom("list_footer")
            .frame(egui::Frame::new().inner_margin(egui::Margin::symmetric(10, 8)))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.weak(format!("{} of {} loaded", self.messages.len(), self.total));
                    let _ = ui.button("Load 50 more");
                    if ui.small_button("Select all shown").clicked() {
                        self.checked.extend(self.messages.iter().map(|m| m.uid));
                    }
                });
            });
        if self.messages.is_empty() {
            ui.add_space(48.0);
            ui.vertical_centered(|ui| {
                ui.label(egui::RichText::new("📭").size(36.0));
                ui.add_space(6.0);
                ui.weak("Nothing in this view.");
            });
            return;
        }
        let typing = ui.ctx().egui_wants_keyboard_input();
        let step = ui.input(|i| i.key_pressed(egui::Key::ArrowDown) as i32 - i.key_pressed(egui::Key::ArrowUp) as i32);
        if step != 0 && !typing {
            let pos = self.selected.and_then(|s| self.find(s));
            let next = match pos {
                Some(p) => (p as i32 + step).clamp(0, self.messages.len() as i32 - 1) as usize,
                None => 0,
            };
            self.selected = Some(self.messages[next].uid);
        }
        if !typing && ui.input(|i| i.key_pressed(egui::Key::Delete)) && let Some(uid) = self.selected {
            self.trash_message(uid);
            return;
        }
        egui::ScrollArea::vertical()
            .auto_shrink(false)
            .id_salt("message_list_scroll")
            .show_rows(ui, ROW_HEIGHT, self.messages.len(), |ui, rows| {
                for row in rows {
                    let msg = &self.messages[row];
                    let uid = msg.uid;
                    let selected = self.selected == Some(uid);
                    let mut checked = self.checked.contains(&uid);
                    if message_row(ui, msg, selected, &mut checked).clicked() {
                        self.selected = Some(uid);
                    }
                    if checked {
                        self.checked.insert(uid);
                    } else {
                        self.checked.remove(&uid);
                    }
                }
            });
    }

    fn reading_pane(&self, ui: &mut egui::Ui) -> Option<PaneAction> {
        let Some(i) = self.selected.and_then(|uid| self.find(uid)) else {
            ui.vertical_centered(|ui| {
                ui.add_space(ui.available_height() / 2.0 - 40.0);
                ui.label(egui::RichText::new("📬").size(44.0));
                ui.add_space(6.0);
                ui.weak("Select a message to read it.");
            });
            return None;
        };
        let msg = &self.messages[i];
        let mut action = None;
        ui.add_space(4.0);
        ui.horizontal_wrapped(|ui| {
            if ui.button("🗑 Delete").on_hover_text("Move this email to Trash (Delete key)").clicked() {
                action = Some(PaneAction::Trash(msg.uid));
            }
            let from = &msg.from;
            if ui.button("Delete all from sender").clicked() {
                action = Some(PaneAction::DeleteFrom(from.email.clone()));
            }
            if ui.button("Review sender deletion…").clicked() {
                action = Some(PaneAction::ReviewFrom(from.email.clone()));
            }
            if ui.button("⛔ Block sender").clicked() {
                action = Some(PaneAction::Block(from.email.clone()));
            }
            if let Some((_, domain)) = from.email.split_once('@')
                && ui.button("⛔ Block domain").clicked()
            {
                action = Some(PaneAction::Block(domain.to_owned()));
            }
        });
        ui.add_space(8.0);

        theme::card(ui).show(ui, |ui| {
            ui.horizontal(|ui| {
                let name = msg.from.short();
                let (avatar, _) = ui.allocate_exact_size(egui::vec2(44.0, 44.0), egui::Sense::hover());
                theme::paint_avatar(ui.painter(), avatar, name);
                ui.add_space(4.0);
                ui.vertical(|ui| {
                    ui.add(egui::Label::new(egui::RichText::new(name).strong()).truncate());
                    ui.add(egui::Label::new(egui::RichText::new(&msg.from.email).weak()).truncate());
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                    ui.weak(msg.date.format("%a, %e %b %Y · %H:%M").to_string());
                });
            });
            ui.add_space(10.0);
            ui.add(egui::Label::new(egui::RichText::new(&msg.subject).heading().strong()).wrap());

            let recipients = [("To", join(&msg.to)), ("Cc", join(&msg.cc))];
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

            ui.add_space(8.0);
            ui.horizontal_wrapped(|ui| {
                if self.active == 0 {
                    let _ = ui.link("Open in Gmail ↗");
                }
                for rule in &msg.rules {
                    let chip = egui::RichText::new(format!("rule: {rule}")).small();
                    egui::Frame::new()
                        .fill(ui.visuals().hyperlink_color.gamma_multiply(0.2))
                        .corner_radius(10.0)
                        .inner_margin(egui::Margin::symmetric(8, 2))
                        .show(ui, |ui| ui.label(chip));
                }
            });
        });

        ui.add_space(10.0);
        egui::ScrollArea::vertical().id_salt(("body", msg.uid)).auto_shrink(false).show(ui, |ui| {
            ui.set_max_width(760.0);
            ui.add(egui::Label::new(&msg.body).wrap().selectable(true));
        });
        action
    }

    fn delete_window(&mut self, ctx: &egui::Context) {
        let gmail = self.active == 0;
        let (provider, trash) = if gmail { ("Gmail", "Trash") } else { ("Outlook", "Deleted Items") };
        let mut open = self.delete.open;
        let mut trash_from = None;
        egui::Window::new("Delete emails")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(460.0)
            .show(ctx, |ui| {
                ui.label(format!(
                    "Find emails anywhere in the current account (not only the loaded ones) and move them to {provider}'s {trash}."
                ));
                ui.add_space(6.0);
                let f = &mut self.delete;
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
                    if gmail {
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
                let from = f.from.trim().to_owned();
                let ready = !from.is_empty() || !f.subject.trim().is_empty() || f.older_than_days > 0 || !f.raw.trim().is_empty();
                if ready {
                    ui.weak(format!("Search: from:{}", if from.is_empty() { "*" } else { &from }));
                } else {
                    ui.weak("Fill in at least one of From, Subject, Older than, or Gmail search.");
                }
                if ui.add_enabled(ready, egui::Button::new("🔍 Find matching emails")).clicked() {
                    f.searched = Some(from.clone());
                }
                if f.searched.as_deref() == Some(from.as_str()) {
                    ui.separator();
                    let sample: Vec<&Message> = self
                        .messages
                        .iter()
                        .filter(|m| from.is_empty() || m.from.email.contains(&from))
                        .take(4)
                        .collect();
                    // A fictional server-side count, larger than what's loaded.
                    let count = sample.len() * 9 + 5;
                    ui.strong(format!("{count} emails match."));
                    ui.weak("Most recent:");
                    for m in sample {
                        ui.add(egui::Label::new(format!("{}  {} — {}", short_date(m.date), m.from.short(), m.subject)).truncate());
                    }
                    ui.add_space(6.0);
                    let danger = egui::Button::new(
                        egui::RichText::new(format!("Move {count} emails to {trash}")).color(egui::Color32::WHITE),
                    )
                    .fill(ui.visuals().error_fg_color);
                    if ui.add(danger).clicked() {
                        trash_from = Some((from.clone(), count));
                    }
                    ui.weak(if gmail {
                        "Gmail keeps Trash for 30 days, then deletes it permanently."
                    } else {
                        "You can restore them from Deleted Items in Outlook until it's emptied."
                    });
                }
            });
        if let Some((from, count)) = trash_from {
            self.trash_from(&from);
            self.toast(format!("Moved {count} emails to Trash."));
            self.delete.searched = None;
            open = false;
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
                ui.weak("Gatekeep has to be running for this to work. To block on the server even when Gatekeep is closed, add a filter in Gmail's settings or a blocked sender in Outlook's settings.");
                ui.add_space(6.0);
                ui.add(
                    egui::TextEdit::singleline(&mut self.blocked.search)
                        .hint_text("🔍 Search blocked senders")
                        .desired_width(f32::INFINITY),
                );
                ui.add_space(4.0);
                let needle = self.blocked.search.trim().to_lowercase();
                let mut entries: Vec<(usize, &String)> = self
                    .blocked_senders
                    .iter()
                    .enumerate()
                    .filter(|(_, s)| needle.is_empty() || s.to_lowercase().contains(&needle))
                    .collect();
                entries.sort_by_key(|(_, s)| s.to_lowercase());
                let (emails, domains): (Vec<_>, Vec<_>) = entries.into_iter().partition(|(_, s)| s.contains('@'));
                egui::ScrollArea::vertical().max_height(260.0).auto_shrink([false, false]).show(ui, |ui| {
                    for (title, list) in [("Email addresses", &emails), ("Domains", &domains)] {
                        if list.is_empty() {
                            continue;
                        }
                        ui.strong(format!("{title} ({})", list.len()));
                        for &(i, sender) in list {
                            ui.horizontal(|ui| {
                                if ui.small_button("✕").on_hover_text("Unblock").clicked() {
                                    remove = Some(i);
                                }
                                ui.label(sender);
                            });
                        }
                        ui.add_space(6.0);
                    }
                });
                ui.separator();
                ui.horizontal(|ui| {
                    let edit = ui.add(
                        egui::TextEdit::singleline(&mut self.blocked.input)
                            .hint_text("news@shop.com or shop.com")
                            .desired_width(240.0),
                    );
                    let enter = edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    if (ui.button("Block").clicked() || enter) && !self.blocked.input.trim().is_empty() {
                        add = Some(self.blocked.input.trim().to_lowercase());
                    }
                });
            });
        self.blocked.open = open;
        if let Some(i) = remove {
            self.blocked_senders.remove(i);
        }
        if let Some(s) = add {
            self.blocked.input.clear();
            self.blocked.confirm = Some(s);
        }
    }

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
                let n = self.trash_from(&sender);
                self.blocked_senders.push(sender);
                self.toast(format!("Moved {} emails from blocked senders to Trash.", n.max(1)));
            }
            Some(false) => self.blocked.confirm = None,
            None => {}
        }
    }

    /// The progress modal; it fills over a few seconds, then closes.
    fn sweep_window(&mut self, ctx: &egui::Context) {
        let Some(sweep) = &self.sweep else { return };
        let fraction = (sweep.started.elapsed().as_secs_f32() / 6.0).min(1.0);
        if fraction >= 1.0 {
            self.sweep = None;
            self.toast("Moved 41 emails from blocked senders to Trash.");
            return;
        }
        let account = (fraction * 2.0) as usize;
        egui::Modal::new(egui::Id::unique("sweep_progress")).show(ctx, |ui| {
            ui.set_width(380.0);
            ui.heading(sweep.title);
            ui.add_space(6.0);
            ui.add(egui::ProgressBar::new(fraction).show_percentage().animate(true));
            ui.add_space(4.0);
            ui.label(format!(
                "{}: moving {} of 41 to Trash…",
                data::ACCOUNTS[account.min(1)].0,
                (fraction * 41.0) as usize
            ));
            ui.weak(&sweep.note);
        });
        ctx.request_repaint();
    }

    fn wipe_confirm_window(&mut self, ctx: &egui::Context) {
        let account = data::ACCOUNTS[self.active].0;
        let Some(typed) = &mut self.wipe else { return };
        let mut decision = None;
        egui::Modal::new(egui::Id::unique("wipe_confirm")).show(ctx, |ui| {
            ui.set_max_width(420.0);
            ui.heading("Delete all email?");
            ui.add_space(4.0);
            ui.label(format!(
                "Every email in {account} will be moved to Trash. This covers the whole account, not only the INBOX."
            ));
            ui.add_space(2.0);
            ui.weak("Nothing is permanently deleted, and mail in Trash can be restored from there.");
            ui.add_space(8.0);
            ui.label("Type the address to confirm:");
            ui.add(egui::TextEdit::singleline(typed).hint_text(account).desired_width(f32::INFINITY));
            let matches = typed.trim().eq_ignore_ascii_case(account);
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let go = egui::Button::new(egui::RichText::new("Delete all email").color(egui::Color32::WHITE))
                    .fill(ui.visuals().error_fg_color);
                if ui.add_enabled(matches, go).clicked() {
                    decision = Some(true);
                }
                if ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    decision = Some(false);
                }
            });
        });
        match decision {
            Some(true) => {
                self.wipe = None;
                self.sweep = Some(Sweep {
                    title: "Deleting all email",
                    note: format!("Every email in {account} is being moved to Trash."),
                    started: Instant::now(),
                });
            }
            Some(false) => self.wipe = None,
            None => {}
        }
    }

    fn toasts_ui(&mut self, ctx: &egui::Context) {
        self.toasts.retain(|t| t.shown.elapsed() < TOAST_LIFETIME);
        if self.toasts.is_empty() {
            return;
        }
        ctx.request_repaint_after(Duration::from_millis(200));
        let mut dismiss = None;
        egui::Area::new(egui::Id::unique("toasts"))
            .anchor(egui::Align2::RIGHT_BOTTOM, egui::vec2(-16.0, -16.0))
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                for (i, t) in self.toasts.iter().enumerate() {
                    egui::Frame::popup(ui.style()).show(ui, |ui| {
                        ui.set_max_width(TOAST_WIDTH);
                        ui.add(egui::Label::new(&t.message).wrap());
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.small_button("✕").on_hover_text("Dismiss").clicked() {
                                dismiss = Some(i);
                            }
                        });
                    });
                    ui.add_space(6.0);
                }
            });
        if let Some(i) = dismiss {
            self.toasts.remove(i);
        }
    }
}

fn message_row(ui: &mut egui::Ui, msg: &Message, selected: bool, checked: &mut bool) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(ui.available_width(), ROW_HEIGHT), egui::Sense::click());
    let accent = ui.visuals().hyperlink_color;
    let card = rect.shrink2(egui::vec2(2.0, 3.0));
    let fill = if selected {
        Some(accent.gamma_multiply(0.22))
    } else if msg.highlight {
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
        let bar = egui::Rect::from_min_size(card.left_top(), egui::vec2(3.0, card.height()));
        ui.painter().rect_filled(bar, 2.0, accent);
    }
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(card.shrink2(egui::vec2(10.0, 6.0))));
    if msg.low_priority {
        child.multiply_opacity(0.6);
    }
    let strong = |t: &str| {
        let t = egui::RichText::new(t);
        if msg.unread { t.strong() } else { t }
    };
    let sender = msg.from.short();
    child.horizontal(|ui| {
        let (avatar, _) = ui.allocate_exact_size(egui::vec2(36.0, 36.0), egui::Sense::hover());
        theme::paint_avatar(ui.painter(), avatar, sender);
        if msg.unread {
            ui.painter().circle_filled(avatar.right_top() + egui::vec2(-3.0, 3.0), 4.5, accent);
        }
        ui.add_space(8.0);
        ui.vertical(|ui| {
            ui.set_max_width(ui.available_width() - CHECK_WIDTH);
            ui.horizontal(|ui| {
                let date_w = 56.0;
                ui.add_sized(
                    [ui.available_width() - date_w, 18.0],
                    egui::Label::new(strong(sender)).truncate().selectable(false),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add(egui::Label::new(egui::RichText::new(short_date(msg.date)).small().weak()).selectable(false));
                });
            });
            let mut line = egui::text::LayoutJob::default();
            let font = egui::TextStyle::Body.resolve(ui.style());
            let text_color = if msg.unread { ui.visuals().strong_text_color() } else { ui.visuals().text_color() };
            line.append(&msg.subject, 0.0, egui::TextFormat::simple(font.clone(), text_color));
            let weak = ui.visuals().weak_text_color();
            line.append(" — ", 0.0, egui::TextFormat::simple(font.clone(), weak));
            line.append(&msg.snippet, 0.0, egui::TextFormat::simple(font, weak));
            ui.add(egui::Label::new(line).truncate().selectable(false));
        });
        ui.add(egui::Checkbox::without_text(checked)).on_hover_text("Select to delete in bulk");
    });
    response
}

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

fn join(addrs: &[Address]) -> String {
    addrs.iter().map(ToString::to_string).collect::<Vec<_>>().join(", ")
}

impl eframe::App for DemoApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::Panel::top("top_bar")
            .frame(egui::Frame::new().inner_margin(egui::Margin::symmetric(14, 8)))
            .show(ui, |ui| self.top_bar(ui));
        egui::Panel::top("inbox_toolbar")
            .frame(egui::Frame::new().inner_margin(egui::Margin::symmetric(12, 10)))
            .show(ui, |ui| self.inbox_toolbar(ui));
        let list_bg = ui.visuals().extreme_bg_color;
        egui::Panel::left("message_list")
            .resizable(true)
            .default_size(440.0)
            .size_range(260.0..=800.0)
            .frame(egui::Frame::new().fill(list_bg).inner_margin(egui::Margin::symmetric(6, 6)))
            .show(ui, |ui| self.message_list(ui));
        let action = egui::CentralPanel::default()
            .frame(egui::Frame::new().inner_margin(egui::Margin::symmetric(18, 12)))
            .show(ui, |ui| self.reading_pane(ui))
            .inner;
        match action {
            Some(PaneAction::Trash(uid)) => self.trash_message(uid),
            Some(PaneAction::DeleteFrom(email)) => {
                let n = self.trash_from(&email);
                self.toast(format!("Moved {n} email{} from {email} to Trash.", if n == 1 { "" } else { "s" }));
            }
            Some(PaneAction::ReviewFrom(email)) => {
                self.delete = DeleteDialog { open: true, from: email, ..Default::default() };
            }
            Some(PaneAction::Block(s)) => self.blocked.confirm = Some(s),
            None => {}
        }
        let ctx = ui.ctx().clone();
        self.delete_window(&ctx);
        self.blocked_window(&ctx);
        self.block_confirm_window(&ctx);
        self.toasts_ui(&ctx);
        self.sweep_window(&ctx);
        self.wipe_confirm_window(&ctx);
    }
}
