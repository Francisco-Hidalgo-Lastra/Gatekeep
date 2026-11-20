//! Filter rules and local matching.
//!
//! Matching is case-insensitive and ignores leading/trailing whitespace in the
//! pattern. A rule with an empty pattern never matches, so a half-written rule
//! can't hide your whole inbox.

use serde::{Deserialize, Serialize};

use crate::model::{Address, Message};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Field {
    #[default]
    Sender,
    /// Any address in To or Cc.
    Recipient,
    Subject,
    /// Plain-text body (falls back to the snippet if the body is empty).
    Body,
}

impl Field {
    pub const ALL: [Field; 4] = [Field::Sender, Field::Recipient, Field::Subject, Field::Body];
    pub fn label(self) -> &'static str {
        match self {
            Self::Sender => "Sender",
            Self::Recipient => "Recipient",
            Self::Subject => "Subject",
            Self::Body => "Body",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MatchKind {
    #[default]
    Contains,
    Equals,
    StartsWith,
}

impl MatchKind {
    pub const ALL: [MatchKind; 3] = [MatchKind::Contains, MatchKind::Equals, MatchKind::StartsWith];
    pub fn label(self) -> &'static str {
        match self {
            Self::Contains => "contains",
            Self::Equals => "equals",
            Self::StartsWith => "starts with",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Hide,
    #[default]
    LowPriority,
    Highlight,
}

impl Action {
    pub const ALL: [Action; 3] = [Action::Hide, Action::LowPriority, Action::Highlight];
    pub fn label(self) -> &'static str {
        match self {
            Self::Hide => "hide",
            Self::LowPriority => "mark low priority",
            Self::Highlight => "highlight",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rule {
    #[serde(default)]
    pub name: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    pub field: Field,
    #[serde(default)]
    pub kind: MatchKind,
    pub pattern: String,
    pub action: Action,
}

fn default_true() -> bool {
    true
}

impl Default for Rule {
    fn default() -> Self {
        Self {
            name: String::new(),
            enabled: true,
            field: Field::Sender,
            kind: MatchKind::Contains,
            pattern: String::new(),
            action: Action::LowPriority,
        }
    }
}

impl Rule {
    /// Does this rule match the message? Disabled rules never match.
    pub fn matches(&self, msg: &Message) -> bool {
        if !self.enabled {
            return false;
        }
        let pattern = self.pattern.trim().to_lowercase();
        if pattern.is_empty() {
            return false;
        }
        let test = |candidate: &str| text_matches(self.kind, &candidate.trim().to_lowercase(), &pattern);
        match self.field {
            Field::Sender => msg.from.as_ref().is_some_and(|a| address_matches(a, &test)),
            Field::Recipient => msg.to.iter().chain(&msg.cc).any(|a| address_matches(a, &test)),
            Field::Subject => test(&msg.subject),
            Field::Body => {
                if msg.body.trim().is_empty() {
                    test(&msg.snippet)
                } else {
                    test(&msg.body)
                }
            }
        }
    }

    /// Name for display: the rule name, or a generated description.
    pub fn display_name(&self) -> String {
        if self.name.trim().is_empty() {
            format!(
                "{} {} \"{}\" → {}",
                self.field.label(),
                self.kind.label(),
                self.pattern.trim(),
                self.action.label()
            )
        } else {
            self.name.clone()
        }
    }
}

/// An address matches if its email, display name, or the full "Name <email>" form matches.
fn address_matches(addr: &Address, test: &impl Fn(&str) -> bool) -> bool {
    test(&addr.email) || (!addr.name.is_empty() && test(&addr.name)) || test(&addr.to_string())
}

fn text_matches(kind: MatchKind, haystack: &str, pattern: &str) -> bool {
    match kind {
        MatchKind::Contains => haystack.contains(pattern),
        MatchKind::Equals => haystack == pattern,
        MatchKind::StartsWith => haystack.starts_with(pattern),
    }
}

/// Combined result of evaluating every rule against one message.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Verdict {
    pub hidden: bool,
    pub low_priority: bool,
    pub highlight: bool,
    /// Indices into the rule list of every rule that matched.
    pub matched: Vec<usize>,
}

impl Verdict {
    /// Caught by a low-priority rule and not hidden. Such mail is dimmed in the list.
    pub fn is_low_priority(&self) -> bool {
        self.low_priority && !self.hidden
    }
}

/// Evaluate all rules against a message. All matching rules apply, so a
/// message can be both highlighted and low priority.
pub fn evaluate(rules: &[Rule], msg: &Message) -> Verdict {
    let mut v = Verdict::default();
    for (i, rule) in rules.iter().enumerate() {
        if rule.matches(msg) {
            v.matched.push(i);
            match rule.action {
                Action::Hide => v.hidden = true,
                Action::LowPriority => v.low_priority = true,
                Action::Highlight => v.highlight = true,
            }
        }
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(name: &str, email: &str) -> Address {
        Address {
            name: name.into(),
            email: email.into(),
        }
    }

    fn msg() -> Message {
        Message {
            account: String::new(),
            uid: 1,
            message_id: "1@example.com".into(),
            from: Some(addr("Acme Weekly", "news@acme.example")),
            to: vec![addr("Me", "me@example.com")],
            cc: vec![addr("", "team@corp.example")],
            subject: "Your Weekly Digest".into(),
            snippet: "snippet text".into(),
            body: "Hello,\nHere is your UNSUBSCRIBE link.".into(),
            date: None,
            unread: true,
        }
    }

    fn rule(field: Field, kind: MatchKind, pattern: &str, action: Action) -> Rule {
        Rule {
            name: String::new(),
            enabled: true,
            field,
            kind,
            pattern: pattern.into(),
            action,
        }
    }

    #[test]
    fn sender_contains_is_case_insensitive() {
        let r = rule(Field::Sender, MatchKind::Contains, "ACME", Action::Hide);
        assert!(r.matches(&msg()));
    }

    #[test]
    fn sender_equals_matches_email_or_name() {
        let m = msg();
        assert!(rule(Field::Sender, MatchKind::Equals, "news@acme.example", Action::Hide).matches(&m));
        assert!(rule(Field::Sender, MatchKind::Equals, "acme weekly", Action::Hide).matches(&m));
        assert!(rule(Field::Sender, MatchKind::Equals, "Acme Weekly <news@acme.example>", Action::Hide).matches(&m));
        assert!(!rule(Field::Sender, MatchKind::Equals, "acme", Action::Hide).matches(&m));
    }

    #[test]
    fn sender_starts_with() {
        let m = msg();
        assert!(rule(Field::Sender, MatchKind::StartsWith, "news@", Action::Hide).matches(&m));
        assert!(!rule(Field::Sender, MatchKind::StartsWith, "acme.example", Action::Hide).matches(&m));
    }

    #[test]
    fn recipient_checks_to_and_cc() {
        let m = msg();
        assert!(rule(Field::Recipient, MatchKind::Equals, "me@example.com", Action::Highlight).matches(&m));
        assert!(rule(Field::Recipient, MatchKind::StartsWith, "team@", Action::Highlight).matches(&m));
        assert!(!rule(Field::Recipient, MatchKind::Contains, "news@", Action::Highlight).matches(&m));
    }

    #[test]
    fn subject_kinds() {
        let m = msg();
        assert!(rule(Field::Subject, MatchKind::Contains, "digest", Action::LowPriority).matches(&m));
        assert!(rule(Field::Subject, MatchKind::Equals, "your weekly digest", Action::LowPriority).matches(&m));
        assert!(rule(Field::Subject, MatchKind::StartsWith, "your", Action::LowPriority).matches(&m));
        assert!(!rule(Field::Subject, MatchKind::StartsWith, "weekly", Action::LowPriority).matches(&m));
    }

    #[test]
    fn body_keyword_and_snippet_fallback() {
        let mut m = msg();
        assert!(rule(Field::Body, MatchKind::Contains, "unsubscribe", Action::LowPriority).matches(&m));
        m.body.clear();
        assert!(!rule(Field::Body, MatchKind::Contains, "unsubscribe", Action::LowPriority).matches(&m));
        assert!(rule(Field::Body, MatchKind::Contains, "snippet", Action::LowPriority).matches(&m));
    }

    #[test]
    fn pattern_whitespace_is_trimmed() {
        assert!(rule(Field::Subject, MatchKind::Equals, "  your weekly digest ", Action::Hide).matches(&msg()));
    }

    #[test]
    fn empty_pattern_never_matches() {
        let m = msg();
        for kind in MatchKind::ALL {
            assert!(!rule(Field::Subject, kind, "   ", Action::Hide).matches(&m));
        }
    }

    #[test]
    fn disabled_rule_never_matches() {
        let mut r = rule(Field::Sender, MatchKind::Contains, "acme", Action::Hide);
        r.enabled = false;
        assert!(!r.matches(&msg()));
    }

    #[test]
    fn missing_sender_does_not_match() {
        let mut m = msg();
        m.from = None;
        assert!(!rule(Field::Sender, MatchKind::Contains, "a", Action::Hide).matches(&m));
    }

    #[test]
    fn evaluate_combines_actions() {
        let rules = vec![
            rule(Field::Sender, MatchKind::Contains, "acme", Action::LowPriority),
            rule(Field::Subject, MatchKind::Contains, "digest", Action::Highlight),
            rule(Field::Subject, MatchKind::Contains, "invoice", Action::Hide),
        ];
        let v = evaluate(&rules, &msg());
        assert!(v.low_priority && v.highlight && !v.hidden);
        assert_eq!(v.matched, vec![0, 1]);
        assert!(v.is_low_priority());
    }

    #[test]
    fn hidden_is_excluded_from_low_priority() {
        let rules = vec![
            rule(Field::Sender, MatchKind::Contains, "acme", Action::LowPriority),
            rule(Field::Sender, MatchKind::Contains, "acme", Action::Hide),
        ];
        let v = evaluate(&rules, &msg());
        assert!(v.hidden);
        assert!(!v.is_low_priority());
    }

    #[test]
    fn no_rules_means_visible_and_not_dimmed() {
        let v = evaluate(&[], &msg());
        assert!(!v.hidden && !v.is_low_priority());
        assert!(!v.highlight);
        assert!(v.matched.is_empty());
    }

    #[test]
    fn highlight_only_stays_visible() {
        let rules = vec![rule(Field::Subject, MatchKind::Contains, "digest", Action::Highlight)];
        let v = evaluate(&rules, &msg());
        assert!(!v.hidden && v.highlight);
    }
}
