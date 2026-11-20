//! Mailbox access over IMAP, for Gmail and Outlook.
//!
//! Reading always uses `EXAMINE` (read-only) and `BODY.PEEK`, so browsing never
//! marks mail as read. The only write operation is moving messages to the
//! provider's Trash (Gmail) or Deleted Items (Outlook), which needs `SELECT`
//! and happens only in the `trash_*` methods.
//!
//! Gmail searches run server-side with Gmail's own syntax in All Mail. Outlook
//! has no All Mail and no Gmail syntax, so it searches each folder with
//! standard IMAP criteria and re-checks every match locally before acting on it.

use chrono::{Local, TimeZone};
use imap_proto::NameAttribute;
use mail_parser::{MessageParser, PartKind};

use crate::auth::{AuthError, Provider};
use crate::filter::Search;
use crate::model::{Address, Message};

const IMAP_PORT: u16 = 993;
/// Messages larger than this (usually big attachments) are listed from their
/// headers only, so a refresh doesn't download megabytes of attachments.
const MAX_BODY_BYTES: u32 = 1024 * 1024;
const SNIPPET_LEN: usize = 200;
/// How many messages go in one IMAP MOVE command when trashing.
const MOVE_BATCH: usize = 100;
/// How many messages' headers are fetched at once when re-checking search results.
const HEADER_BATCH: usize = 50;

type ImapSession = imap::Session<imap::Connection>;

#[derive(Debug, Clone)]
pub enum ApiError {
    Auth(AuthError),
    Network(String),
    /// The server refused a command (other than login).
    Server(String),
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Auth(e) => write!(f, "{e}"),
            Self::Network(m) => write!(f, "Network error: {m}"),
            Self::Server(m) => write!(f, "Mail server error: {m}"),
        }
    }
}

impl ApiError {
    /// The full technical description, including the underlying cause, for
    /// the "Details" view. May include server text, so it's shown only on request.
    pub fn details(&self) -> String {
        format!("{self:?}")
    }

    /// Short, non-sensitive name used for logging.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Auth(e) => e.kind(),
            Self::Network(_) => "network",
            Self::Server(_) => "server",
        }
    }
}

impl From<AuthError> for ApiError {
    fn from(e: AuthError) -> Self {
        Self::Auth(e)
    }
}

fn map_imap_error(e: imap::Error) -> ApiError {
    use imap::Error as E;
    match e {
        E::Io(io) => ApiError::Network(io.to_string()),
        E::RustlsHandshake(_) => ApiError::Network("TLS handshake with the mail server failed".into()),
        E::ConnectionLost => ApiError::Network("connection to the mail server was lost".into()),
        E::No(no) => ApiError::Server(no.information),
        E::Bad(bad) => ApiError::Server(bad.information),
        E::Bye(bye) => ApiError::Server(bye.information),
        other => ApiError::Server(format!("unexpected response ({other:?})")),
    }
}

/// How to log in.
pub enum Login<'a> {
    /// Gmail App Password (IMAP `LOGIN`).
    Password(&'a str),
    /// OAuth access token (IMAP `AUTHENTICATE XOAUTH2`).
    Token(&'a str),
}

/// SASL XOAUTH2, as used by Gmail and Outlook.
struct XOAuth2<'a> {
    user: &'a str,
    token: &'a str,
}

impl imap::Authenticator for XOAuth2<'_> {
    type Response = String;
    fn process(&self, challenge: &[u8]) -> String {
        if challenge.is_empty() {
            format!("user={}\x01auth=Bearer {}\x01\x01", self.user, self.token)
        } else {
            // A non-empty challenge carries an error; an empty reply lets the
            // server finish with NO.
            String::new()
        }
    }
}

/// An authenticated IMAP connection to one account.
pub struct MailClient {
    session: ImapSession,
    provider: Provider,
}

impl MailClient {
    /// Connect over TLS and log in. A wrong App Password (Gmail) gives
    /// `Rejected`; a refused OAuth token (Outlook) gives `ImapRefused`.
    pub fn connect(email: &str, provider: Provider, login: Login<'_>) -> Result<Self, ApiError> {
        let client = imap::ClientBuilder::new(provider.imap_host(), IMAP_PORT)
            .mode(imap::ConnectionMode::Tls)
            .connect()
            .map_err(map_imap_error)?;
        let session = match login {
            Login::Password(password) => client.login(email, password).map_err(|(e, _client)| match e {
                imap::Error::No(_) => ApiError::Auth(AuthError::Rejected(provider)),
                // The password contained characters IMAP can't send; it can't be an App Password.
                imap::Error::Validate(_) => ApiError::Auth(AuthError::Rejected(provider)),
                other => map_imap_error(other),
            })?,
            Login::Token(token) => client
                .authenticate("XOAUTH2", &XOAuth2 { user: email, token })
                .map_err(|(e, _client)| match e {
                    imap::Error::No(_) => ApiError::Auth(AuthError::ImapRefused),
                    other => map_imap_error(other),
                })?,
        };
        Ok(Self { session, provider })
    }

    /// Open INBOX read-only and return how many messages it holds.
    /// Used to confirm the account works after signing in.
    pub fn inbox_count(&mut self) -> Result<u32, ApiError> {
        let mailbox = self.session.examine("INBOX").map_err(map_imap_error)?;
        Ok(mailbox.exists)
    }

    /// Bring the INBOX up to date, given the UIDs already loaded (`known`).
    ///
    /// Fetches messages newer than the newest known one (up to `page`), reports
    /// which known messages are gone, and refreshes unread state. If more than
    /// `page` new messages arrived, the result replaces the list instead.
    pub fn sync_inbox(&mut self, known: &[u32], page: u32) -> Result<InboxSync, ApiError> {
        self.session.examine("INBOX").map_err(map_imap_error)?;
        let mut all: Vec<u32> = self.session.uid_search("ALL").map_err(map_imap_error)?.into_iter().collect();
        all.sort_unstable();
        let total = all.len();
        let page = page as usize;
        let newest_known = known.iter().copied().max().unwrap_or(0);
        let newer: Vec<u32> = all.iter().copied().filter(|&u| u > newest_known).collect();

        if known.is_empty() || newer.len() > page {
            let uids = &all[all.len().saturating_sub(page)..];
            return Ok(InboxSync {
                replace: true,
                new: self.fetch_uids(uids)?,
                removed: Vec::new(),
                unread: Vec::new(),
                total,
            });
        }

        let removed: Vec<u32> = known.iter().copied().filter(|u| all.binary_search(u).is_err()).collect();
        let still: Vec<u32> = known.iter().copied().filter(|u| all.binary_search(u).is_ok()).collect();
        let mut unread = Vec::new();
        if !still.is_empty() {
            let flags = self.session.uid_fetch(uid_set(&still), "(UID FLAGS)").map_err(map_imap_error)?;
            for f in flags.iter() {
                if let Some(uid) = f.uid {
                    unread.push((uid, is_unread(f.flags())));
                }
            }
        }
        Ok(InboxSync {
            replace: false,
            new: self.fetch_uids(&newer)?,
            removed,
            unread,
            total,
        })
    }

    /// Fetch up to `page` INBOX messages older than UID `oldest`.
    /// Returns the messages and the current INBOX size.
    pub fn fetch_older(&mut self, oldest: u32, page: u32) -> Result<(Vec<Message>, usize), ApiError> {
        let mailbox = self.session.examine("INBOX").map_err(map_imap_error)?;
        if oldest <= 1 {
            return Ok((Vec::new(), mailbox.exists as usize));
        }
        let mut older: Vec<u32> = self
            .session
            .uid_search(format!("UID 1:{}", oldest - 1))
            .map_err(map_imap_error)?
            .into_iter()
            // Servers may return the highest UID for a range past the end.
            .filter(|&u| u < oldest)
            .collect();
        older.sort_unstable();
        let uids = &older[older.len().saturating_sub(page as usize)..];
        Ok((self.fetch_uids(uids)?, mailbox.exists as usize))
    }

    /// Fetch and parse the given UIDs in the currently open mailbox, newest first.
    ///
    /// Headers come first; full bodies are then fetched only for messages up to
    /// `MAX_BODY_BYTES`. `BODY.PEEK` never marks mail as read.
    fn fetch_uids(&mut self, uids: &[u32]) -> Result<Vec<Message>, ApiError> {
        if uids.is_empty() {
            return Ok(Vec::new());
        }
        let parser = MessageParser::new();
        let mut messages = Vec::new();
        let mut small = Vec::new();
        let headers = self
            .session
            .uid_fetch(uid_set(uids), "(UID FLAGS RFC822.SIZE BODY.PEEK[HEADER])")
            .map_err(map_imap_error)?;
        for f in headers.iter() {
            let Some(uid) = f.uid else { continue };
            let raw = f.header().unwrap_or_default();
            messages.push(parse_message(&parser, uid, raw, is_unread(f.flags())));
            if f.size.is_some_and(|s| s <= MAX_BODY_BYTES) {
                small.push(uid);
            }
        }
        if !small.is_empty() {
            let bodies = self
                .session
                .uid_fetch(uid_set(&small), "(UID BODY.PEEK[])")
                .map_err(map_imap_error)?;
            for f in bodies.iter() {
                let (Some(uid), Some(raw)) = (f.uid, f.body()) else { continue };
                if let Some(m) = messages.iter_mut().find(|m| m.uid == uid) {
                    let unread = m.unread;
                    *m = parse_message(&parser, uid, raw, unread);
                }
            }
        }
        messages.sort_by_key(|m| std::cmp::Reverse(m.date));
        Ok(messages)
    }

    /// Fetch only the headers of the given UIDs in the open mailbox.
    fn fetch_headers(&mut self, uids: &[u32]) -> Result<Vec<Message>, ApiError> {
        let parser = MessageParser::new();
        let mut messages = Vec::with_capacity(uids.len());
        for chunk in uids.chunks(HEADER_BATCH) {
            let fetched = self
                .session
                .uid_fetch(uid_set(chunk), "(UID FLAGS BODY.PEEK[HEADER])")
                .map_err(map_imap_error)?;
            for f in fetched.iter() {
                let Some(uid) = f.uid else { continue };
                messages.push(parse_message(&parser, uid, f.header().unwrap_or_default(), is_unread(f.flags())));
            }
        }
        Ok(messages)
    }

    /// All folders, with their attributes.
    fn folders(&mut self) -> Result<Vec<(String, Vec<NameAttribute<'static>>)>, ApiError> {
        let names = self.session.list(None, Some("*")).map_err(map_imap_error)?;
        Ok(names
            .iter()
            .map(|n| (n.name().to_owned(), n.attributes().iter().map(|a| a.clone().into_owned()).collect()))
            .collect())
    }

    /// Name of the folder with a special-use attribute (e.g. `\Trash`), or, if
    /// the server doesn't mark one, the first folder named one of `fallback`.
    /// Gmail localizes these names, so they're looked up rather than hard-coded.
    fn special_mailbox(&mut self, want: NameAttribute<'static>, fallback: &[&str]) -> Result<String, ApiError> {
        let folders = self.folders()?;
        folders
            .iter()
            .find(|(_, attrs)| attrs.contains(&want))
            .or_else(|| {
                fallback
                    .iter()
                    .find_map(|f| folders.iter().find(|(name, _)| name.eq_ignore_ascii_case(f)))
            })
            .map(|(name, _)| name.clone())
            .ok_or_else(|| ApiError::Server(format!("the server did not report a {want:?} folder")))
    }

    fn trash_mailbox(&mut self) -> Result<String, ApiError> {
        self.special_mailbox(NameAttribute::Trash, &["Deleted Items", "Deleted", "Trash"])
    }

    /// Folders an Outlook account-wide search covers: everything selectable
    /// except Deleted Items, Junk and Outlook's internal folders. This mirrors
    /// Gmail's All Mail, which excludes Trash and Spam.
    fn searchable_folders(&mut self, inbox_only: bool) -> Result<Vec<String>, ApiError> {
        if inbox_only {
            return Ok(vec!["INBOX".into()]);
        }
        let skip_names = ["deleted items", "deleted", "trash", "junk email", "junk", "outbox"];
        Ok(self
            .folders()?
            .into_iter()
            .filter(|(name, attrs)| {
                let lower = name.to_lowercase();
                !attrs.iter().any(|a| matches!(a, NameAttribute::NoSelect | NameAttribute::Trash | NameAttribute::Junk))
                    && !skip_names.contains(&lower.as_str())
                    && !lower.starts_with("sync issues")
            })
            .map(|(name, _)| name)
            .collect())
    }

    /// Count the messages matching a search anywhere in the account, and
    /// return up to `sample` of the newest ones.
    pub fn search_all_mail(&mut self, search: &Search, sample: usize) -> Result<(usize, Vec<Message>), ApiError> {
        match self.provider {
            Provider::Gmail => {
                let all_mail = self.special_mailbox(NameAttribute::All, &[])?;
                self.session.examine(&all_mail).map_err(map_imap_error)?;
                let mut uids = self.gmail_search(search)?;
                uids.sort_unstable();
                let newest = &uids[uids.len().saturating_sub(sample)..];
                Ok((uids.len(), self.fetch_uids(newest)?))
            }
            Provider::Outlook => {
                let mut count = 0;
                let mut found = Vec::new();
                for folder in self.searchable_folders(search.inbox_only)? {
                    self.session.examine(&folder).map_err(map_imap_error)?;
                    let matched = self.verified_search(search)?;
                    count += matched.len();
                    found.extend(matched);
                }
                found.sort_by_key(|m| std::cmp::Reverse(m.date));
                found.truncate(sample);
                Ok((count, found))
            }
        }
    }

    /// Move every message matching a search (anywhere in the account) to the
    /// Trash / Deleted Items folder. Returns how many were moved.
    pub fn trash_matching(&mut self, search: &Search) -> Result<usize, ApiError> {
        self.trash_matching_with_progress(search, &mut |_, _| {})
    }

    /// [`Self::trash_matching`], calling `progress(fraction, moved)` as it goes.
    /// `fraction` runs from 0 to 1 over the whole account.
    pub fn trash_matching_with_progress(
        &mut self,
        search: &Search,
        progress: &mut dyn FnMut(f32, usize),
    ) -> Result<usize, ApiError> {
        let trash = self.trash_mailbox()?;
        match self.provider {
            Provider::Gmail => {
                let all_mail = self.special_mailbox(NameAttribute::All, &[])?;
                self.session.select(&all_mail).map_err(map_imap_error)?;
                let mut uids = self.gmail_search(search)?;
                uids.sort_unstable();
                let total = uids.len().max(1) as f32;
                self.move_uids(&uids, &trash, &mut |moved| progress(moved as f32 / total, moved))?;
                progress(1.0, uids.len());
                Ok(uids.len())
            }
            Provider::Outlook => {
                let folders = self.searchable_folders(search.inbox_only)?;
                let n = folders.len().max(1) as f32;
                let mut moved = 0;
                for (i, folder) in folders.iter().enumerate() {
                    progress(i as f32 / n, moved);
                    self.session.select(folder).map_err(map_imap_error)?;
                    let uids: Vec<u32> = self.verified_search(search)?.iter().map(|m| m.uid).collect();
                    let len = uids.len().max(1) as f32;
                    self.move_uids(&uids, &trash, &mut |done| {
                        progress((i as f32 + done as f32 / len) / n, moved + done)
                    })?;
                    moved += uids.len();
                }
                progress(1.0, moved);
                Ok(moved)
            }
        }
    }

    /// Move every message in the account to Trash (Gmail: All Mail; Outlook: the
    /// same folders as an account-wide search). Returns how many were moved.
    /// `progress(fraction, moved)` is called as it goes.
    pub fn trash_everything_with_progress(&mut self, progress: &mut dyn FnMut(f32, usize)) -> Result<usize, ApiError> {
        let trash = self.trash_mailbox()?;
        let folders = match self.provider {
            Provider::Gmail => vec![self.special_mailbox(NameAttribute::All, &[])?],
            Provider::Outlook => self.searchable_folders(false)?,
        };
        let n = folders.len().max(1) as f32;
        let mut moved = 0;
        for (i, folder) in folders.iter().enumerate() {
            progress(i as f32 / n, moved);
            self.session.select(folder).map_err(map_imap_error)?;
            let mut uids: Vec<u32> = self.session.uid_search("ALL").map_err(map_imap_error)?.into_iter().collect();
            uids.sort_unstable();
            let len = uids.len().max(1) as f32;
            self.move_uids(&uids, &trash, &mut |done| progress((i as f32 + done as f32 / len) / n, moved + done))?;
            moved += uids.len();
        }
        progress(1.0, moved);
        Ok(moved)
    }

    /// Move specific INBOX messages to Trash.
    pub fn trash_inbox_uids(&mut self, uids: &[u32]) -> Result<usize, ApiError> {
        let trash = self.trash_mailbox()?;
        self.session.select("INBOX").map_err(map_imap_error)?;
        self.move_uids(uids, &trash, &mut |_| {})?;
        Ok(uids.len())
    }

    /// `on_batch` gets the running total moved after each batch.
    fn move_uids(&mut self, uids: &[u32], mailbox: &str, on_batch: &mut dyn FnMut(usize)) -> Result<(), ApiError> {
        // Small batches keep each UID MOVE well under server line-length limits
        // and make a failure affect only one batch.
        let mut moved = 0;
        for chunk in uids.chunks(MOVE_BATCH) {
            self.session.uid_mv(uid_set(chunk), mailbox).map_err(map_imap_error)?;
            moved += chunk.len();
            on_batch(moved);
        }
        Ok(())
    }

    /// UID SEARCH using Gmail's own search syntax (`X-GM-RAW`).
    fn gmail_search(&mut self, search: &Search) -> Result<Vec<u32>, ApiError> {
        // Never turn an empty filter into "match everything".
        let query = search
            .gmail_query()
            .ok_or_else(|| ApiError::Server("refusing to run an empty search".into()))?;
        if query.contains(['\r', '\n']) {
            return Err(ApiError::Server("search can't contain line breaks".into()));
        }
        let quoted = format!("\"{}\"", query.replace('\\', "\\\\").replace('"', "\\\""));
        let uids = self
            .session
            .uid_search(format!("X-GM-RAW {quoted}"))
            .map_err(map_imap_error)?;
        Ok(uids.into_iter().collect())
    }

    /// Standard IMAP search in the open folder, with every hit re-checked
    /// against its headers. Returns the matches, parsed from their headers.
    fn verified_search(&mut self, search: &Search) -> Result<Vec<Message>, ApiError> {
        let criteria = search
            .imap_criteria(Local::now().date_naive())
            .ok_or_else(|| ApiError::Server("refusing to run an empty search".into()))?;
        let mut uids = Vec::new();
        for c in criteria {
            uids.extend(self.session.uid_search(c).map_err(map_imap_error)?);
        }
        uids.sort_unstable();
        uids.dedup();
        let now = Local::now();
        Ok(self
            .fetch_headers(&uids)?
            .into_iter()
            .filter(|m| search.matches(m, now))
            .collect())
    }

    pub fn logout(mut self) {
        let _ = self.session.logout();
    }
}

/// Result of [`MailClient::sync_inbox`].
#[derive(Debug, Default)]
pub struct InboxSync {
    /// Discard the current list and use `new` instead.
    pub replace: bool,
    /// Newly fetched messages, newest first.
    pub new: Vec<Message>,
    /// Known UIDs no longer in INBOX (deleted or archived elsewhere).
    pub removed: Vec<u32>,
    /// Current unread state of known UIDs.
    pub unread: Vec<(u32, bool)>,
    /// Number of messages in INBOX.
    pub total: usize,
}

fn is_unread(flags: &[imap::types::Flag<'_>]) -> bool {
    !flags.iter().any(|f| matches!(f, imap::types::Flag::Seen))
}

/// Compact IMAP sequence set: `[1,2,3,7,9,10]` → `"1:3,7,9:10"`.
fn uid_set(uids: &[u32]) -> String {
    let mut sorted = uids.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    let mut parts: Vec<String> = Vec::new();
    let mut i = 0;
    while i < sorted.len() {
        let start = sorted[i];
        let mut end = start;
        while i + 1 < sorted.len() && sorted[i + 1] == end + 1 {
            i += 1;
            end = sorted[i];
        }
        parts.push(if start == end { start.to_string() } else { format!("{start}:{end}") });
        i += 1;
    }
    parts.join(",")
}

/// Turn raw RFC 5322 bytes (full message or headers only) into a [`Message`].
fn parse_message(parser: &MessageParser, uid: u32, raw: &[u8], unread: bool) -> Message {
    let mut msg = Message {
        account: String::new(),
        uid,
        message_id: String::new(),
        from: None,
        to: vec![],
        cc: vec![],
        subject: String::new(),
        snippet: String::new(),
        body: String::new(),
        date: None,
        unread,
    };
    let Some(parsed) = parser.parse(raw) else {
        return msg;
    };
    let addresses = |list: Option<mail_parser::AddressList<'_>>| -> Vec<Address> {
        list.map(|l| {
            l.mailboxes()
                .filter_map(|mb| {
                    Some(Address {
                        name: mb.name().unwrap_or_default().trim().to_owned(),
                        email: mb.address()?.trim().to_owned(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
    };
    msg.from = addresses(parsed.from()).into_iter().next();
    msg.to = addresses(parsed.to());
    msg.cc = addresses(parsed.cc());
    msg.subject = parsed.subject().unwrap_or_default().trim().to_owned();
    msg.message_id = parsed.message_id().unwrap_or_default().to_owned();
    msg.date = parsed
        .date()
        .and_then(|d| Local.timestamp_opt(d.to_timestamp(), 0).single());
    msg.body = body_text(&parsed);
    msg.snippet = snippet(&msg.body);
    msg
}

/// The readable body: the text/plain part, or the HTML part rendered as text.
fn body_text(parsed: &mail_parser::Message<'_>) -> String {
    let Some(part) = parsed.text_body().next() else {
        return String::new();
    };
    let Some(text) = part.text() else {
        return String::new();
    };
    match part.kind() {
        PartKind::Html => html2text::from_read(text.as_bytes(), 100).unwrap_or_else(|_| text.into_owned()),
        _ => text.replace("\r\n", "\n"),
    }
}

/// First `SNIPPET_LEN` characters of the body with whitespace collapsed.
fn snippet(body: &str) -> String {
    let mut out = String::new();
    for word in body.split_whitespace() {
        if out.chars().count() >= SNIPPET_LEN {
            out.push('…');
            break;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(word);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Talks to the real Gmail server: run with `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn bad_credentials_are_rejected() {
        let login = Login::Password("aaaabbbbccccdddd");
        match MailClient::connect("gatekeep.nonexistent.test@gmail.com", Provider::Gmail, login) {
            Err(ApiError::Auth(AuthError::Rejected(Provider::Gmail))) => {}
            Err(e) => panic!("expected Rejected, got {e:?}"),
            Ok(_) => panic!("expected Rejected, got a session"),
        }
    }

    #[test]
    fn uid_set_collapses_ranges() {
        assert_eq!(uid_set(&[9, 1, 2, 3, 7, 10, 2]), "1:3,7,9:10");
        assert_eq!(uid_set(&[5]), "5");
    }

    #[test]
    fn parses_plain_message() {
        let raw = b"From: \"Doe, Jane\" <jane@x.com>\r\nTo: me@y.com, Bob <bob@z.com>\r\n\
Subject: Hello there\r\nMessage-ID: <abc@x.com>\r\nDate: Tue, 3 Nov 2026 10:00:00 +0000\r\n\r\n\
Line one\r\nLine   two\r\n";
        let m = parse_message(&MessageParser::new(), 42, raw, true);
        assert_eq!(m.uid, 42);
        assert_eq!(m.message_id, "abc@x.com");
        let from = m.from.unwrap();
        assert_eq!((from.name.as_str(), from.email.as_str()), ("Doe, Jane", "jane@x.com"));
        assert_eq!(m.to.len(), 2);
        assert_eq!(m.subject, "Hello there");
        assert_eq!(m.body, "Line one\nLine   two\n");
        assert_eq!(m.snippet, "Line one Line two");
        assert!(m.date.is_some() && m.unread);
    }

    #[test]
    fn html_only_message_is_rendered_as_text() {
        let raw = b"From: a@b.com\r\nContent-Type: text/html\r\n\r\n<p>Hi <b>there</b></p>";
        let m = parse_message(&MessageParser::new(), 1, raw, false);
        assert!(m.body.contains("Hi") && !m.body.contains("<p>"), "{:?}", m.body);
    }

    #[test]
    fn headers_only_message_has_empty_body() {
        let raw = b"From: a@b.com\r\nSubject: Big attachment\r\n\r\n";
        let m = parse_message(&MessageParser::new(), 1, raw, false);
        assert_eq!(m.subject, "Big attachment");
        assert!(m.body.is_empty() && m.snippet.is_empty());
    }
}
