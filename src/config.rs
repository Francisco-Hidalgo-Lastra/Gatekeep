//! Settings and filter rules, persisted as TOML in the user's config directory.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::rules::Rule;

/// Default number of inbox messages fetched per page ("Load more" adds another page).
pub const DEFAULT_PAGE_SIZE: u32 = 50;
/// Upper bound on the page size.
pub const MAX_PAGE_SIZE: u32 = 500;

/// Microsoft app registration (client ID) used for Outlook sign-in when
/// `microsoft_client_id` isn't set in `config.toml`. Set it at build time with
/// `GATEKEEP_MS_CLIENT_ID=<id> cargo build --release`.
const BUILTIN_MICROSOFT_CLIENT_ID: Option<&str> = option_env!("GATEKEEP_MS_CLIENT_ID");

/// Directory that holds `config.toml` (and `account.json` if no keychain is available).
pub fn config_dir() -> Option<PathBuf> {
    directories::ProjectDirs::from("", "", "gatekeep").map(|d| d.config_dir().to_path_buf())
}

fn config_path() -> Option<PathBuf> {
    config_dir().map(|d| d.join("config.toml"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThemeChoice {
    #[default]
    System,
    Light,
    Dark,
}

impl ThemeChoice {
    pub fn next(self) -> Self {
        match self {
            Self::System => Self::Light,
            Self::Light => Self::Dark,
            Self::Dark => Self::System,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::System => "Theme: system",
            Self::Light => "Theme: light",
            Self::Dark => "Theme: dark",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Number of INBOX messages fetched per page.
    pub page_size: u32,
    /// Auto-refresh interval in minutes; 0 disables auto-refresh.
    pub auto_refresh_minutes: u32,
    pub theme: ThemeChoice,
    pub rules: Vec<Rule>,
    /// Senders (addresses or domains) whose mail is moved to Trash on every refresh
    /// in every signed-in account.
    pub blocked_senders: Vec<String>,
    /// Address of the account that was shown last; opened first at startup.
    pub last_account: Option<String>,
    /// Client ID of your Microsoft Entra app registration, needed to sign in to
    /// Outlook / Hotmail accounts. Overrides the one built into the app.
    pub microsoft_client_id: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            page_size: DEFAULT_PAGE_SIZE,
            auto_refresh_minutes: 0,
            theme: ThemeChoice::System,
            rules: Vec::new(),
            blocked_senders: Vec::new(),
            last_account: None,
            microsoft_client_id: None,
        }
    }
}

impl Config {
    /// The Microsoft client ID to sign in with, if one is configured.
    pub fn microsoft_client_id(&self) -> Option<String> {
        self.microsoft_client_id
            .as_deref()
            .or(BUILTIN_MICROSOFT_CLIENT_ID)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
    }

    /// Load the config, falling back to defaults if missing.
    /// A parse error is reported so the UI can show it, and defaults are used.
    pub fn load() -> (Self, Option<String>) {
        let Some(path) = config_path() else {
            return (Self::default(), None);
        };
        match std::fs::read_to_string(&path) {
            Ok(text) => match toml::from_str::<Config>(&text) {
                Ok(mut cfg) => {
                    cfg.page_size = cfg.page_size.clamp(1, MAX_PAGE_SIZE);
                    (cfg, None)
                }
                Err(e) => (
                    Self::default(),
                    Some(format!(
                        "Could not parse {} ({}). Using defaults; the file will be overwritten on the next save.",
                        path.display(),
                        e.message()
                    )),
                ),
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (Self::default(), None),
            Err(e) => (
                Self::default(),
                Some(format!("Could not read {}: {e}", path.display())),
            ),
        }
    }

    pub fn save(&self) -> Result<(), String> {
        let path = config_path().ok_or("No config directory available")?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("Could not create config dir: {e}"))?;
        }
        let text = toml::to_string_pretty(self).map_err(|e| format!("Could not serialize config: {e}"))?;
        // Write to a temp file then rename, so a crash never leaves a half-written config.
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, text).map_err(|e| format!("Could not write config: {e}"))?;
        std::fs::rename(&tmp, &path).map_err(|e| format!("Could not save config: {e}"))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::{Action, Field, MatchKind};

    #[test]
    fn config_roundtrips_through_toml() {
        let mut cfg = Config::default();
        cfg.blocked_senders.push("spam@example.com".into());
        cfg.rules.push(Rule {
            name: "Newsletters".into(),
            enabled: true,
            field: Field::Sender,
            kind: MatchKind::Contains,
            pattern: "newsletter".into(),
            action: Action::LowPriority,
        });
        let text = toml::to_string_pretty(&cfg).unwrap();
        let back: Config = toml::from_str(&text).unwrap();
        assert_eq!(cfg, back);
    }

    #[test]
    fn missing_fields_use_defaults() {
        let cfg: Config = toml::from_str("auto_refresh_minutes = 5").unwrap();
        assert_eq!(cfg.page_size, DEFAULT_PAGE_SIZE);
        assert!(cfg.blocked_senders.is_empty());
        assert_eq!(cfg.auto_refresh_minutes, 5);
        assert!(cfg.rules.is_empty());
    }
}
