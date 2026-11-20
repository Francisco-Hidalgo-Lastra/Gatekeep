//! Account credentials and their storage.
//!
//! Gmail accounts sign in with an App Password. Outlook / Hotmail / Microsoft
//! accounts sign in with Microsoft's OAuth (see [`crate::oauth`]) and keep a
//! refresh token, because Microsoft no longer accepts passwords over IMAP.
//!
//! Every signed-in account is kept as one JSON list in the OS keychain. If no
//! keychain is available, the same list goes to a file in the config directory.

use std::io::Write;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::config;

const KEYRING_SERVICE: &str = "gatekeep";
/// Holds the JSON list of accounts (of every provider; the name predates
/// Outlook support). Older versions stored a single account object under this
/// name; [`parse_accounts`] still reads that.
const KEYRING_USER: &str = "gmail-account";
/// Where users create an App Password for Gmail IMAP sign-in.
pub const APP_PASSWORD_URL: &str = "https://myaccount.google.com/apppasswords";

/// Which mail service an account belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Provider {
    #[default]
    Gmail,
    /// Outlook.com, Hotmail, Live, MSN, and Microsoft 365 work or school accounts.
    Outlook,
}

impl Provider {
    pub const ALL: [Provider; 2] = [Provider::Gmail, Provider::Outlook];

    pub fn name(self) -> &'static str {
        match self {
            Self::Gmail => "Gmail",
            Self::Outlook => "Outlook",
        }
    }

    /// Label for the provider picker on the sign-in form.
    pub fn label(self) -> &'static str {
        match self {
            Self::Gmail => "Gmail",
            Self::Outlook => "Outlook / Hotmail",
        }
    }

    /// What the provider calls the folder deleted mail goes to.
    pub fn trash_name(self) -> &'static str {
        match self {
            Self::Gmail => "Trash",
            Self::Outlook => "Deleted Items",
        }
    }

    pub fn imap_host(self) -> &'static str {
        match self {
            Self::Gmail => "imap.gmail.com",
            Self::Outlook => "outlook.office365.com",
        }
    }

    /// Guess the provider from an address. `None` for custom domains, which
    /// could be Google Workspace or Microsoft 365.
    pub fn detect(email: &str) -> Option<Self> {
        let domain = email.rsplit_once('@')?.1.trim().to_ascii_lowercase();
        if matches!(domain.as_str(), "gmail.com" | "googlemail.com") {
            return Some(Self::Gmail);
        }
        // outlook.com, hotmail.co.uk, live.fr, msn.com, …
        let first = domain.split('.').next().unwrap_or_default();
        let microsoft = ["outlook", "hotmail", "live", "msn", "windowslive", "passport"];
        (microsoft.contains(&first) && domain.contains('.')).then_some(Self::Outlook)
    }
}

#[derive(Debug, Clone)]
pub enum AuthError {
    /// The address or App Password is malformed (caught before contacting the server).
    Invalid(String),
    /// The provider rejected the stored credentials (bad App Password, or a
    /// Microsoft sign-in that expired or was revoked). The account is forgotten.
    Rejected(Provider),
    /// Microsoft sign-in worked, but the IMAP server refused the account.
    /// Usually IMAP is turned off for it; the account is kept.
    ImapRefused,
    /// Microsoft sign-in failed or isn't set up.
    OAuth(String),
    /// The user cancelled Microsoft sign-in.
    Cancelled,
    /// Keychain / credentials file problem.
    Storage(String),
}

impl std::fmt::Display for AuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(m) => f.write_str(m),
            Self::Rejected(Provider::Gmail) => f.write_str(
                "Gmail rejected the sign-in. Check the address, and use an App Password \
                 (not your normal Google password). App Passwords need 2-Step Verification.",
            ),
            Self::Rejected(Provider::Outlook) => f.write_str(
                "Your Microsoft sign-in expired or was revoked. Sign in to the account again.",
            ),
            Self::ImapRefused => f.write_str(
                "Outlook refused IMAP access for this account. In Outlook on the web, turn on \
                 Settings → Mail → Forwarding and IMAP → \"Let devices and apps use IMAP\". \
                 For a work or school account, your administrator must allow IMAP.",
            ),
            Self::OAuth(m) => write!(f, "Microsoft sign-in failed: {m}"),
            Self::Cancelled => f.write_str("Sign-in cancelled."),
            Self::Storage(m) => write!(f, "Credential storage error: {m}"),
        }
    }
}

impl AuthError {
    /// Short, non-sensitive name used for logging.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Invalid(_) => "invalid",
            Self::Rejected(_) => "rejected",
            Self::ImapRefused => "imap refused",
            Self::OAuth(_) => "oauth",
            Self::Cancelled => "cancelled",
            Self::Storage(_) => "storage",
        }
    }
}

/// One signed-in account. Gmail accounts use `app_password`; Outlook accounts
/// use `refresh_token`, which only works with the `client_id` that obtained it.
#[derive(Clone, Serialize, Deserialize)]
pub struct Account {
    pub email: String,
    /// Accounts stored before Outlook support have no provider: they're Gmail.
    #[serde(default)]
    pub provider: Provider,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub app_password: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub refresh_token: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub client_id: String,
}

// Never print secrets, even in debug logs.
impl std::fmt::Debug for Account {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Account")
            .field("email", &self.email)
            .field("provider", &self.provider)
            .field("app_password", &"<redacted>")
            .field("refresh_token", &"<redacted>")
            .finish()
    }
}

impl Account {
    /// A Gmail account from user input. Google displays App Passwords in groups
    /// of four (`abcd efgh ijkl mnop`), so whitespace is removed.
    pub fn gmail(email: &str, app_password: &str) -> Result<Self, AuthError> {
        let email = email.trim().to_owned();
        let app_password: String = app_password.chars().filter(|c| !c.is_whitespace()).collect();
        if !email.contains('@') {
            return Err(AuthError::Invalid("Enter your full Gmail address.".into()));
        }
        if app_password.is_empty() {
            return Err(AuthError::Invalid("Enter an App Password.".into()));
        }
        Ok(Self {
            email,
            provider: Provider::Gmail,
            app_password,
            refresh_token: String::new(),
            client_id: String::new(),
        })
    }

    /// An Outlook account after Microsoft sign-in.
    pub fn outlook(email: &str, client_id: &str, refresh_token: &str) -> Self {
        Self {
            email: email.trim().to_owned(),
            provider: Provider::Outlook,
            app_password: String::new(),
            refresh_token: refresh_token.to_owned(),
            client_id: client_id.to_owned(),
        }
    }
}

// ---------------------------------------------------------------------------
// Storage
// ---------------------------------------------------------------------------

/// Where the credentials were stored, shown in the UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialStore {
    Keychain,
    File,
}

/// TODO: the file fallback stores credentials unencrypted (mode 0600 on Unix).
/// It is only used when the OS keychain is unavailable (e.g. no Secret Service
/// running on Linux). Consider an encrypted file store if this becomes common.
fn credentials_file_path() -> Option<PathBuf> {
    config::config_dir().map(|d| d.join("account.json"))
}

fn keyring_entry() -> Result<keyring::Entry, keyring::Error> {
    keyring::Entry::new(KEYRING_SERVICE, KEYRING_USER)
}

/// Read a stored list of accounts. Also accepts the older single-account format.
fn parse_accounts(text: &str) -> Option<Vec<Account>> {
    if let Ok(list) = serde_json::from_str::<Vec<Account>>(text) {
        return Some(list);
    }
    serde_json::from_str::<Account>(text).ok().map(|a| vec![a])
}

/// Store the full list of signed-in accounts, replacing what was there.
pub fn save_accounts(accounts: &[Account]) -> Result<CredentialStore, AuthError> {
    let secret = serde_json::to_string(accounts).map_err(|e| AuthError::Storage(e.to_string()))?;
    match keyring_entry().and_then(|e| e.set_password(&secret)) {
        Ok(()) => {
            // Remove any stale fallback file.
            if let Some(p) = credentials_file_path() {
                let _ = std::fs::remove_file(p);
            }
            Ok(CredentialStore::Keychain)
        }
        Err(e) => {
            log::warn!("keychain unavailable, using file fallback ({})", keyring_error_kind(&e));
            write_credentials_file(&secret)?;
            Ok(CredentialStore::File)
        }
    }
}

fn write_credentials_file(secret: &str) -> Result<(), AuthError> {
    let path = credentials_file_path().ok_or_else(|| AuthError::Storage("no config directory".into()))?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| AuthError::Storage(e.to_string()))?;
    }
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(&path).map_err(|e| AuthError::Storage(e.to_string()))?;
    f.write_all(secret.as_bytes())
        .map_err(|e| AuthError::Storage(e.to_string()))
}

/// Load every stored account, and where they came from.
pub fn load_accounts() -> Option<(Vec<Account>, CredentialStore)> {
    match keyring_entry().and_then(|e| e.get_password()) {
        Ok(s) => match parse_accounts(&s) {
            Some(accounts) => return Some((accounts, CredentialStore::Keychain)),
            None => log::warn!("stored keychain credentials are unreadable; ignoring"),
        },
        Err(keyring::Error::NoEntry) => {}
        Err(e) => log::warn!("keychain read failed ({})", keyring_error_kind(&e)),
    }
    let text = std::fs::read_to_string(credentials_file_path()?).ok()?;
    let accounts = parse_accounts(&text)?;
    Some((accounts, CredentialStore::File))
}

/// Delete all stored credentials from both the keychain and the fallback file.
pub fn delete_all_accounts() -> Result<(), AuthError> {
    let mut problems = Vec::new();
    match keyring_entry().and_then(|e| e.delete_credential()) {
        Ok(()) | Err(keyring::Error::NoEntry) => {}
        // No keychain at all: nothing could have been stored there.
        Err(keyring::Error::NoDefaultStore) | Err(keyring::Error::NoStorageAccess(_)) => {}
        Err(e) => problems.push(format!("keychain: {}", keyring_error_kind(&e))),
    }
    if let Some(p) = credentials_file_path() {
        match std::fs::remove_file(p) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => problems.push(format!("credentials file: {e}")),
        }
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(AuthError::Storage(problems.join("; ")))
    }
}

fn keyring_error_kind(e: &keyring::Error) -> &'static str {
    match e {
        keyring::Error::NoEntry => "no entry",
        keyring::Error::NoDefaultStore => "no default store",
        keyring::Error::NoStorageAccess(_) => "no storage access",
        keyring::Error::PlatformFailure(_) => "platform failure",
        _ => "other",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_spaces_from_app_password() {
        let a = Account::gmail("  me@gmail.com ", "abcd efgh ijkl mnop").unwrap();
        assert_eq!(a.email, "me@gmail.com");
        assert_eq!(a.app_password, "abcdefghijklmnop");
        assert_eq!(a.provider, Provider::Gmail);
    }

    #[test]
    fn rejects_bad_input() {
        assert!(Account::gmail("me", "pw").is_err());
        assert!(Account::gmail("me@gmail.com", "   ").is_err());
    }

    #[test]
    fn debug_hides_secrets() {
        let a = Account::gmail("me@gmail.com", "secretpw").unwrap();
        assert!(!format!("{a:?}").contains("secretpw"));
        let o = Account::outlook("me@outlook.com", "cid", "secret-refresh");
        assert!(!format!("{o:?}").contains("secret-refresh"));
    }

    #[test]
    fn reads_account_lists_and_the_old_single_account_format() {
        let list = r#"[{"email":"a@gmail.com","app_password":"x"},{"email":"b@gmail.com","app_password":"y"}]"#;
        let accounts = parse_accounts(list).unwrap();
        assert_eq!(accounts.len(), 2);
        assert_eq!(accounts[1].email, "b@gmail.com");
        // Lists saved before Outlook support are Gmail accounts.
        assert_eq!(accounts[1].provider, Provider::Gmail);

        let single = r#"{"email":"a@gmail.com","app_password":"x"}"#;
        let accounts = parse_accounts(single).unwrap();
        assert_eq!(accounts.len(), 1);
        assert_eq!(accounts[0].app_password, "x");

        assert!(parse_accounts("not json").is_none());
    }

    #[test]
    fn outlook_accounts_roundtrip() {
        let list = vec![
            Account::gmail("a@gmail.com", "pw").unwrap(),
            Account::outlook("b@hotmail.com", "cid", "rt"),
        ];
        let back = parse_accounts(&serde_json::to_string(&list).unwrap()).unwrap();
        assert_eq!(back[1].provider, Provider::Outlook);
        assert_eq!((back[1].refresh_token.as_str(), back[1].client_id.as_str()), ("rt", "cid"));
        assert!(back[1].app_password.is_empty());
    }

    #[test]
    fn detects_provider_from_address() {
        assert_eq!(Provider::detect("me@gmail.com"), Some(Provider::Gmail));
        assert_eq!(Provider::detect("me@Hotmail.co.uk"), Some(Provider::Outlook));
        assert_eq!(Provider::detect("me@outlook.com"), Some(Provider::Outlook));
        assert_eq!(Provider::detect("me@live.fr"), Some(Provider::Outlook));
        assert_eq!(Provider::detect("me@msn.com"), Some(Provider::Outlook));
        assert_eq!(Provider::detect("me@company.com"), None);
        assert_eq!(Provider::detect("me"), None);
    }
}
