//! Internal message types, independent of the Gmail API's JSON shapes.

use chrono::{DateTime, Local};

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Address {
    pub name: String,
    pub email: String,
}

impl std::fmt::Display for Address {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.name.is_empty() {
            f.write_str(&self.email)
        } else {
            write!(f, "{} <{}>", self.name, self.email)
        }
    }
}

impl Address {
    /// Short form for the message list: the display name, or the email.
    pub fn short(&self) -> &str {
        if self.name.is_empty() { &self.email } else { &self.name }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    /// Address of the account it belongs to (filled in by the worker). UIDs are
    /// only unique within one account, so the UI keys messages by both.
    pub account: String,
    /// IMAP UID within INBOX.
    pub uid: u32,
    /// RFC 5322 `Message-ID` without angle brackets (may be empty).
    pub message_id: String,
    pub from: Option<Address>,
    pub to: Vec<Address>,
    pub cc: Vec<Address>,
    pub subject: String,
    pub snippet: String,
    /// Plain-text body (text/plain part, or stripped text/html).
    pub body: String,
    pub date: Option<DateTime<Local>>,
    pub unread: bool,
}

impl Message {
    /// Link that opens this message in Gmail's web UI (via a Message-ID search).
    /// `account` selects the signed-in Google account when several are logged in.
    /// `None` if the message has no Message-ID.
    pub fn web_url(&self, account: Option<&str>) -> Option<String> {
        if self.message_id.is_empty() {
            return None;
        }
        let query = percent_encode(&format!("rfc822msgid:{}", self.message_id));
        Some(match account {
            Some(email) => format!(
                "https://mail.google.com/mail/?authuser={}#search/{query}",
                percent_encode(email)
            ),
            None => format!("https://mail.google.com/mail/u/0/#search/{query}"),
        })
    }
}

/// Percent-encode everything except RFC 3986 unreserved characters.
fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Parse a header like `"Jane Doe" <jane@x.com>, bob@y.com` into addresses.
pub fn parse_address_list(header: &str) -> Vec<Address> {
    split_addresses(header)
        .into_iter()
        .filter_map(|part| parse_address(&part))
        .collect()
}

/// Split on commas that are not inside quotes or angle brackets.
fn split_addresses(header: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut cur = String::new();
    let mut in_quotes = false;
    let mut in_angle = false;
    let mut escaped = false;
    for c in header.chars() {
        if escaped {
            cur.push(c);
            escaped = false;
            continue;
        }
        match c {
            '\\' if in_quotes => {
                escaped = true;
                cur.push(c);
            }
            '"' => {
                in_quotes = !in_quotes;
                cur.push(c);
            }
            '<' if !in_quotes => {
                in_angle = true;
                cur.push(c);
            }
            '>' if !in_quotes => {
                in_angle = false;
                cur.push(c);
            }
            ',' if !in_quotes && !in_angle => parts.push(std::mem::take(&mut cur)),
            _ => cur.push(c),
        }
    }
    parts.push(cur);
    parts
}

pub fn parse_address(raw: &str) -> Option<Address> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    if let (Some(lt), Some(gt)) = (raw.rfind('<'), raw.rfind('>'))
        && lt < gt
    {
        let email = raw[lt + 1..gt].trim().to_owned();
        let name = unquote(raw[..lt].trim());
        return Some(Address { name, email });
    }
    Some(Address {
        name: String::new(),
        email: raw.to_owned(),
    })
}

fn unquote(s: &str) -> String {
    let s = s.trim();
    let inner = s
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .unwrap_or(s);
    inner.replace("\\\"", "\"").replace("\\\\", "\\")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_name_and_email() {
        let a = parse_address("\"Doe, Jane\" <jane@x.com>").unwrap();
        assert_eq!(a.name, "Doe, Jane");
        assert_eq!(a.email, "jane@x.com");
    }

    #[test]
    fn parses_bare_email() {
        let a = parse_address("bob@y.com").unwrap();
        assert_eq!(a.name, "");
        assert_eq!(a.email, "bob@y.com");
    }

    #[test]
    fn splits_list_respecting_quotes() {
        let list = parse_address_list("\"Doe, Jane\" <jane@x.com>, bob@y.com,  Carl <c@z.com>");
        assert_eq!(list.len(), 3);
        assert_eq!(list[0].email, "jane@x.com");
        assert_eq!(list[1].email, "bob@y.com");
        assert_eq!(list[2].name, "Carl");
    }

    #[test]
    fn empty_header_gives_no_addresses() {
        assert!(parse_address_list("  ").is_empty());
    }

    #[test]
    fn web_url_searches_by_message_id() {
        let mut m = Message {
            account: String::new(),
            uid: 7,
            message_id: "abc+1@x.com".into(),
            from: None,
            to: vec![],
            cc: vec![],
            subject: String::new(),
            snippet: String::new(),
            body: String::new(),
            date: None,
            unread: false,
        };
        assert_eq!(
            m.web_url(None).unwrap(),
            "https://mail.google.com/mail/u/0/#search/rfc822msgid%3Aabc%2B1%40x.com"
        );
        assert_eq!(
            m.web_url(Some("me@example.com")).unwrap(),
            "https://mail.google.com/mail/?authuser=me%40example.com#search/rfc822msgid%3Aabc%2B1%40x.com"
        );
        m.message_id.clear();
        assert_eq!(m.web_url(None), None);
    }
}
