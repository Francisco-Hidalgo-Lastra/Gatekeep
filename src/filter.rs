//! Searches for bulk deletion and the block list.
//!
//! A [`Search`] is provider-neutral. Gmail runs it server-side as a Gmail
//! search (`X-GM-RAW`). Other servers (Outlook) run it as standard IMAP
//! `SEARCH` criteria, and every match is then checked again locally with
//! [`Search::matches`], because Outlook's server-side search is fuzzy.

use chrono::{DateTime, Local, NaiveDate};

use crate::auth::Provider;
use crate::model::Message;

/// How many senders go in one IMAP `SEARCH`; longer block lists are split.
const IMAP_FROM_CHUNK: usize = 20;

/// Parameters of a bulk delete, as entered in the "Delete emails" window.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DeleteFilter {
    /// Sender address or domain.
    pub from: String,
    pub subject: String,
    /// Only messages older than this many days; 0 means any age.
    pub older_than_days: u32,
    pub inbox_only: bool,
    /// Extra Gmail search terms, appended as-is. Ignored for other providers.
    pub raw: String,
}

impl DeleteFilter {
    /// The search for `provider`, or `None` when no criterion narrows it (an
    /// empty filter would match the whole mailbox, so it's never allowed).
    pub fn search(&self, provider: Provider) -> Option<Search> {
        let raw = match provider {
            Provider::Gmail => self.raw.trim().to_owned(),
            Provider::Outlook => String::new(),
        };
        Search {
            from: [self.from.trim()].into_iter().filter(|s| !s.is_empty()).map(str::to_owned).collect(),
            subject: self.subject.trim().to_owned(),
            older_than_days: self.older_than_days,
            inbox_only: self.inbox_only,
            raw,
        }
        .non_empty()
    }
}

/// Which messages to find. `None`-returning constructors guarantee at least one
/// of `from`, `subject`, `older_than_days` or `raw` is set.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Search {
    /// Matches mail from any of these addresses or domains.
    pub from: Vec<String>,
    /// Subject contains this.
    pub subject: String,
    /// 0 means any age.
    pub older_than_days: u32,
    pub inbox_only: bool,
    /// Gmail search syntax; only Gmail can run it.
    pub raw: String,
}

impl Search {
    fn non_empty(self) -> Option<Self> {
        let narrows = !self.from.is_empty()
            || !self.subject.is_empty()
            || self.older_than_days > 0
            || !self.raw.is_empty();
        narrows.then_some(self)
    }

    /// Gmail search syntax, for `X-GM-RAW`.
    pub fn gmail_query(&self) -> Option<String> {
        let mut terms = Vec::new();
        let from: Vec<String> = self.from.iter().map(|s| format!("from:{}", gmail_quote(s))).collect();
        match from.len() {
            0 => {}
            1 => terms.extend(from),
            // `{a b}` is Gmail's OR group.
            _ => terms.push(format!("{{{}}}", from.join(" "))),
        }
        if !self.subject.is_empty() {
            terms.push(format!("subject:{}", gmail_quote(&self.subject)));
        }
        if self.older_than_days > 0 {
            terms.push(format!("older_than:{}d", self.older_than_days));
        }
        if !self.raw.is_empty() {
            terms.push(self.raw.clone());
        }
        if terms.is_empty() {
            return None;
        }
        if self.inbox_only {
            terms.push("in:inbox".into());
        }
        Some(terms.join(" "))
    }

    /// Standard IMAP `SEARCH` criteria. Long sender lists give several searches
    /// whose results are combined. `None` if empty or it needs Gmail syntax.
    /// `inbox_only` is applied by choosing the folder, not here.
    pub fn imap_criteria(&self, today: NaiveDate) -> Option<Vec<String>> {
        if !self.raw.is_empty() {
            return None;
        }
        let mut common = Vec::new();
        if !self.subject.is_empty() {
            common.push(format!("SUBJECT {}", imap_quote(&self.subject)));
        }
        if self.older_than_days > 0 {
            let cutoff = today - chrono::Days::new(u64::from(self.older_than_days));
            common.push(format!("BEFORE {}", cutoff.format("%-d-%b-%Y")));
        }
        let mut searches = Vec::new();
        if self.from.is_empty() {
            if common.is_empty() {
                return None;
            }
            searches.push(common.join(" "));
        } else {
            for chunk in self.from.chunks(IMAP_FROM_CHUNK) {
                // `OR a OR b c`: IMAP's OR takes exactly two keys.
                let mut keys = chunk.iter().rev().map(|s| format!("FROM {}", imap_quote(s)));
                let last = keys.next().unwrap_or_default();
                let from = keys.fold(last, |acc, k| format!("OR {k} {acc}"));
                let parts: Vec<&str> = std::iter::once(from.as_str()).chain(common.iter().map(String::as_str)).collect();
                searches.push(parts.join(" "));
            }
        }
        // Non-ASCII text needs an explicit charset.
        if !self.subject.is_ascii() || self.from.iter().any(|s| !s.is_ascii()) {
            for s in &mut searches {
                s.insert_str(0, "CHARSET UTF-8 ");
            }
        }
        Some(searches)
    }

    /// Whether a message (parsed at least from its headers) really matches.
    /// Used to double-check results of fuzzy server-side searches, so it errs
    /// on the side of *not* matching.
    pub fn matches(&self, msg: &Message, now: DateTime<Local>) -> bool {
        if !self.raw.is_empty() {
            return false;
        }
        if !self.from.is_empty() {
            let Some(from) = &msg.from else { return false };
            if !self.from.iter().any(|want| sender_matches(want, &from.email, &from.name)) {
                return false;
            }
        }
        if !self.subject.is_empty() && !msg.subject.to_lowercase().contains(&self.subject.to_lowercase()) {
            return false;
        }
        if self.older_than_days > 0 {
            let cutoff = now - chrono::Duration::days(i64::from(self.older_than_days));
            if !msg.date.is_some_and(|d| d < cutoff) {
                return false;
            }
        }
        true
    }

    /// Readable summary shown in the "Delete emails" window.
    pub fn describe(&self, provider: Provider) -> String {
        if provider == Provider::Gmail {
            return self.gmail_query().unwrap_or_default();
        }
        let mut parts = Vec::new();
        if !self.from.is_empty() {
            let list: Vec<String> = self.from.iter().map(|s| format!("\"{s}\"")).collect();
            parts.push(format!("from {}", list.join(" or ")));
        }
        if !self.subject.is_empty() {
            parts.push(format!("subject contains \"{}\"", self.subject));
        }
        if self.older_than_days > 0 {
            parts.push(format!("older than {} days", self.older_than_days));
        }
        parts.push(if self.inbox_only { "in Inbox".into() } else { "in all folders except Deleted Items and Junk".into() });
        parts.join(", ")
    }
}

/// A search matching mail from any blocked sender, or `None` if the list is empty.
pub fn blocked_search(blocked: &[String]) -> Option<Search> {
    Search {
        from: blocked.iter().map(|s| s.trim()).filter(|s| !s.is_empty()).map(str::to_owned).collect(),
        ..Default::default()
    }
    .non_empty()
}

/// The block-list entry that covers this address, if any (an exact address,
/// or a domain covering it or one of its parent domains).
pub fn blocked_by<'a>(blocked: &'a [String], email: &str) -> Option<&'a str> {
    blocked
        .iter()
        .find(|b| validate_sender(b).is_ok() && sender_matches(b, email, ""))
        .map(String::as_str)
}

/// The domain part of an address, lowercased.
pub fn domain_of(email: &str) -> Option<String> {
    let (_, domain) = email.trim().rsplit_once('@')?;
    validate_sender(domain).ok()
}

/// Big shared mail providers: blocking one of these would trash mail from
/// millions of unrelated people, so the UI warns first.
pub fn is_public_mail_domain(domain: &str) -> bool {
    const PUBLIC: &[&str] = &[
        "gmail.com", "googlemail.com", "outlook.com", "hotmail.com", "live.com", "msn.com",
        "yahoo.com", "ymail.com", "icloud.com", "me.com", "mac.com", "aol.com", "proton.me",
        "protonmail.com", "gmx.com", "gmx.net", "mail.com", "zoho.com", "yandex.com",
    ];
    let d = domain.trim().trim_start_matches('@').to_lowercase();
    // Country variants such as hotmail.es or yahoo.co.uk count too.
    PUBLIC.contains(&d.as_str())
        || ["gmail.", "hotmail.", "outlook.", "live.", "yahoo.", "gmx."].iter().any(|p| d.starts_with(p))
}

/// Does a sender match a From filter? A full address must match exactly, a
/// domain matches itself and its subdomains, and anything else (e.g. a name)
/// matches as a case-insensitive substring of the address or display name.
fn sender_matches(want: &str, email: &str, name: &str) -> bool {
    let email = email.to_lowercase();
    match validate_sender(want) {
        Ok(want) if want.contains('@') => email == want,
        Ok(domain) => {
            let got = email.rsplit('@').next().unwrap_or_default();
            got == domain || got.ends_with(&format!(".{domain}"))
        }
        Err(_) => {
            let want = want.trim().to_lowercase();
            !want.is_empty() && (email.contains(&want) || name.to_lowercase().contains(&want))
        }
    }
}

/// Check a block-list entry: it must look like an address or a domain, so a
/// stray word like "a" can't wipe out most of the mailbox.
pub fn validate_sender(s: &str) -> Result<String, &'static str> {
    let s = s.trim().trim_start_matches('@').to_lowercase();
    if s.is_empty() {
        return Err("Enter an email address or domain.");
    }
    if s.contains(char::is_whitespace) || s.contains(['"', '{', '}', '(', ')']) {
        return Err("Enter a single email address or domain, without spaces or quotes.");
    }
    let domain = s.rsplit('@').next().unwrap_or_default();
    if !domain.contains('.') || domain.starts_with('.') || domain.ends_with('.') {
        return Err("That doesn't look like an email address or domain (e.g. news@shop.com or shop.com).");
    }
    Ok(s)
}

/// Quote a value for a Gmail search term. Gmail has no escape for `"`, so it's dropped.
fn gmail_quote(s: &str) -> String {
    format!("\"{}\"", s.replace('"', ""))
}

/// Quote an IMAP string. Control characters (including line breaks, which
/// would end the command) become spaces.
fn imap_quote(s: &str) -> String {
    let clean: String = s.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    format!("\"{}\"", clean.replace('\\', "\\\\").replace('"', "\\\""))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Address;
    use chrono::TimeZone;

    fn gmail_query(f: &DeleteFilter) -> Option<String> {
        f.search(Provider::Gmail).and_then(|s| s.gmail_query())
    }

    #[test]
    fn empty_filter_has_no_search() {
        assert_eq!(DeleteFilter::default().search(Provider::Gmail), None);
        // "Inbox only" alone would mean "delete the whole inbox": not allowed.
        let f = DeleteFilter { inbox_only: true, ..Default::default() };
        assert_eq!(f.search(Provider::Gmail), None);
        let f = DeleteFilter { from: "   ".into(), ..Default::default() };
        assert_eq!(f.search(Provider::Gmail), None);
        // Gmail search terms alone don't count for Outlook, which ignores them.
        let f = DeleteFilter { raw: "has:attachment".into(), ..Default::default() };
        assert!(f.search(Provider::Gmail).is_some());
        assert_eq!(f.search(Provider::Outlook), None);
    }

    #[test]
    fn builds_combined_gmail_query() {
        let f = DeleteFilter {
            from: " news@shop.com ".into(),
            subject: "sale \"now\"".into(),
            older_than_days: 30,
            inbox_only: true,
            raw: "has:attachment".into(),
        };
        assert_eq!(
            gmail_query(&f).unwrap(),
            "from:\"news@shop.com\" subject:\"sale now\" older_than:30d has:attachment in:inbox"
        );
    }

    #[test]
    fn blocked_query_ors_senders() {
        assert_eq!(blocked_search(&[]), None);
        let q = |list: &[&str]| {
            let list: Vec<String> = list.iter().map(|s| s.to_string()).collect();
            blocked_search(&list).unwrap().gmail_query().unwrap()
        };
        assert_eq!(q(&["a@b.com"]), "from:\"a@b.com\"");
        assert_eq!(q(&["a@b.com", " ", "spam.net"]), "{from:\"a@b.com\" from:\"spam.net\"}");
    }

    #[test]
    fn builds_imap_criteria() {
        let today = NaiveDate::from_ymd_opt(2026, 11, 19).unwrap();
        let f = DeleteFilter {
            from: "news@shop.com".into(),
            subject: "say \"hi\"".into(),
            older_than_days: 30,
            ..Default::default()
        };
        let s = f.search(Provider::Outlook).unwrap();
        assert_eq!(
            s.imap_criteria(today).unwrap(),
            vec!["FROM \"news@shop.com\" SUBJECT \"say \\\"hi\\\"\" BEFORE 20-Oct-2026"]
        );

        let blocked: Vec<String> = ["a.com", "b.com", "c.com"].map(String::from).to_vec();
        assert_eq!(
            blocked_search(&blocked).unwrap().imap_criteria(today).unwrap(),
            vec!["OR FROM \"a.com\" OR FROM \"b.com\" FROM \"c.com\""]
        );

        let many: Vec<String> = (0..45).map(|i| format!("s{i}.com")).collect();
        assert_eq!(blocked_search(&many).unwrap().imap_criteria(today).unwrap().len(), 3);

        let s = Search { subject: "café\r\n".into(), ..Default::default() };
        assert_eq!(s.imap_criteria(today).unwrap(), vec!["CHARSET UTF-8 SUBJECT \"café  \""]);

        let raw = Search { raw: "has:attachment".into(), ..Default::default() };
        assert_eq!(raw.imap_criteria(today), None);
    }

    fn msg(from: &str, name: &str, subject: &str, days_old: i64, now: DateTime<Local>) -> Message {
        Message {
            account: String::new(),
            uid: 1,
            message_id: String::new(),
            from: Some(Address { name: name.into(), email: from.into() }),
            to: vec![],
            cc: vec![],
            subject: subject.into(),
            snippet: String::new(),
            body: String::new(),
            date: Some(now - chrono::Duration::days(days_old)),
            unread: false,
        }
    }

    #[test]
    fn local_match_is_strict() {
        let now = Local.with_ymd_and_hms(2026, 11, 19, 12, 0, 0).unwrap();
        let domain = blocked_search(&["shop.com".into()]).unwrap();
        assert!(domain.matches(&msg("News@Shop.com", "", "", 0, now), now));
        assert!(domain.matches(&msg("a@mail.shop.com", "", "", 0, now), now));
        assert!(!domain.matches(&msg("a@notshop.com", "", "", 0, now), now));
        assert!(!domain.matches(&msg("shop.com@evil.net", "", "", 0, now), now));

        let address = blocked_search(&["news@shop.com".into()]).unwrap();
        assert!(!address.matches(&msg("xnews@shop.com", "", "", 0, now), now));

        // A plain word in From matches the display name or address.
        let f = DeleteFilter { from: "amazon".into(), subject: "Order".into(), older_than_days: 7, ..Default::default() };
        let s = f.search(Provider::Outlook).unwrap();
        assert!(s.matches(&msg("x@y.com", "Amazon.com", "Your order shipped", 10, now), now));
        assert!(!s.matches(&msg("x@y.com", "Amazon.com", "Your order shipped", 3, now), now));
        assert!(!s.matches(&msg("x@y.com", "Amazon.com", "Hello", 10, now), now));
        let mut undated = msg("x@y.com", "Amazon", "order", 10, now);
        undated.date = None;
        assert!(!s.matches(&undated, now));
    }

    #[test]
    fn finds_covering_block_entry() {
        let list: Vec<String> = ["news@shop.com", "spam.net"].map(String::from).to_vec();
        assert_eq!(blocked_by(&list, "News@Shop.com"), Some("news@shop.com"));
        assert_eq!(blocked_by(&list, "x@mail.spam.net"), Some("spam.net"));
        assert_eq!(blocked_by(&list, "other@shop.com"), None);
        assert_eq!(domain_of("A@Mail.Shop.com").as_deref(), Some("mail.shop.com"));
        assert_eq!(domain_of("nobody"), None);
        assert!(is_public_mail_domain("gmail.com"));
        assert!(is_public_mail_domain("hotmail.es"));
        assert!(!is_public_mail_domain("shop.com"));
    }

    #[test]
    fn validates_senders() {
        assert_eq!(validate_sender(" News@Shop.com ").unwrap(), "news@shop.com");
        assert_eq!(validate_sender("@spam.net").unwrap(), "spam.net");
        assert!(validate_sender("a").is_err());
        assert!(validate_sender("gmail").is_err());
        assert!(validate_sender("a b.com").is_err());
        assert!(validate_sender("x@.com").is_err());
        assert!(validate_sender("").is_err());
    }
}
