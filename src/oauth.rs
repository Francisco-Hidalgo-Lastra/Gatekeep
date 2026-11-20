//! Microsoft sign-in for Outlook / Hotmail / Microsoft 365 IMAP.
//!
//! Microsoft no longer accepts passwords (or app passwords) over IMAP, so
//! Outlook accounts use OAuth 2.0. Gatekeep uses the *device code* flow: the
//! user opens microsoft.com/devicelogin in any browser and types a short code.
//! That needs no local web server or redirect URI.
//!
//! This needs an app registration in Microsoft Entra ID (its "client ID"). See
//! the README for how to create one.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use base64::Engine;
use serde::Deserialize;

use crate::auth::{AuthError, Provider};
use crate::mail::ApiError;

/// `common` accepts both personal Microsoft accounts and work/school accounts.
const AUTHORITY: &str = "https://login.microsoftonline.com/common/oauth2/v2.0";
/// IMAP access, a refresh token (`offline_access`), and the signed-in address
/// (`openid email profile`, read from the ID token).
const SCOPES: &str = "https://outlook.office.com/IMAP.AccessAsUser.All offline_access openid email profile";
/// Refresh access tokens this long before Microsoft says they expire.
const EXPIRY_MARGIN: Duration = Duration::from_secs(300);
const HTTP_TIMEOUT: Duration = Duration::from_secs(30);

/// A pending device-code sign-in.
pub struct DeviceCode {
    /// The code the user types at `verification_uri`.
    pub user_code: String,
    pub verification_uri: String,
    device_code: String,
    interval: Duration,
    expires_at: Instant,
}

/// Tokens returned by a successful sign-in or refresh.
pub struct Tokens {
    pub access_token: String,
    /// Microsoft usually sends a new refresh token on every refresh; store it.
    pub refresh_token: Option<String>,
    /// When to stop using `access_token` (already includes a safety margin).
    pub expires_at: Instant,
    /// The signed-in address, from the ID token (sign-in only).
    pub email: Option<String>,
}

/// Every field Microsoft's endpoints may return; each response uses a subset.
#[derive(Deserialize, Default)]
struct Reply {
    access_token: Option<String>,
    refresh_token: Option<String>,
    expires_in: Option<u64>,
    id_token: Option<String>,
    device_code: Option<String>,
    user_code: Option<String>,
    verification_uri: Option<String>,
    interval: Option<u64>,
    error: Option<String>,
    error_description: Option<String>,
}

impl Reply {
    fn error_text(&self) -> String {
        // Microsoft's descriptions start with an AADSTS code and end with trace IDs.
        let desc = self.error_description.as_deref().unwrap_or_default();
        let first_line = desc.lines().next().unwrap_or_default();
        let first_line = first_line.split(" Trace ID:").next().unwrap_or_default().trim();
        match (&self.error, first_line.is_empty()) {
            (Some(e), true) => e.clone(),
            (Some(e), false) => format!("{e}: {first_line}"),
            (None, _) => "unexpected response from Microsoft".into(),
        }
    }
}

fn oauth_error(msg: impl Into<String>) -> ApiError {
    ApiError::Auth(AuthError::OAuth(msg.into()))
}

fn post(endpoint: &str, form: &[(&str, &str)]) -> Result<Reply, ApiError> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(Some(HTTP_TIMEOUT))
        .build()
        .into();
    let url = format!("{AUTHORITY}/{endpoint}");
    let mut response = agent
        .post(&url)
        .send_form(form.iter().copied())
        .map_err(|e| ApiError::Network(format!("could not reach Microsoft sign-in ({e})")))?;
    let text = response
        .body_mut()
        .read_to_string()
        .map_err(|e| ApiError::Network(format!("could not read Microsoft's reply ({e})")))?;
    serde_json::from_str(&text).map_err(|_| oauth_error(format!("unexpected reply from Microsoft (HTTP {})", response.status())))
}

/// Ask Microsoft for a device code to show the user.
pub fn start_device_flow(client_id: &str) -> Result<DeviceCode, ApiError> {
    let r = post("devicecode", &[("client_id", client_id), ("scope", SCOPES)])?;
    match (r.device_code.clone(), r.user_code.clone(), r.verification_uri.clone()) {
        (Some(device_code), Some(user_code), Some(verification_uri)) => Ok(DeviceCode {
            user_code,
            verification_uri,
            device_code,
            interval: Duration::from_secs(r.interval.unwrap_or(5).max(1)),
            expires_at: Instant::now() + Duration::from_secs(r.expires_in.unwrap_or(900)),
        }),
        _ => Err(oauth_error(r.error_text())),
    }
}

/// Wait until the user finishes signing in, declines, or `cancel` is set.
pub fn wait_for_tokens(client_id: &str, code: &DeviceCode, cancel: &AtomicBool) -> Result<Tokens, ApiError> {
    let mut interval = code.interval;
    loop {
        // Sleep in small steps so Cancel takes effect quickly.
        let wake = Instant::now() + interval;
        while Instant::now() < wake {
            if cancel.load(Ordering::Relaxed) {
                return Err(ApiError::Auth(AuthError::Cancelled));
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        if Instant::now() >= code.expires_at {
            return Err(oauth_error("the code expired before sign-in finished. Try again."));
        }
        let r = post(
            "token",
            &[
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                ("client_id", client_id),
                ("device_code", &code.device_code),
            ],
        )?;
        match r.error.as_deref() {
            None => return tokens(r),
            Some("authorization_pending") => {}
            Some("slow_down") => interval += Duration::from_secs(5),
            Some("authorization_declined") => return Err(oauth_error("access was declined.")),
            Some("expired_token") => return Err(oauth_error("the code expired before sign-in finished. Try again.")),
            Some(_) => return Err(oauth_error(r.error_text())),
        }
    }
}

/// Get a fresh access token from a stored refresh token.
pub fn refresh(client_id: &str, refresh_token: &str) -> Result<Tokens, ApiError> {
    let r = post(
        "token",
        &[
            ("grant_type", "refresh_token"),
            ("client_id", client_id),
            ("refresh_token", refresh_token),
            ("scope", SCOPES),
        ],
    )?;
    match r.error.as_deref() {
        None => tokens(r),
        // Revoked, expired, password changed, or consent withdrawn: sign in again.
        Some("invalid_grant") => Err(ApiError::Auth(AuthError::Rejected(Provider::Outlook))),
        Some(_) => Err(oauth_error(r.error_text())),
    }
}

fn tokens(r: Reply) -> Result<Tokens, ApiError> {
    let access_token = r.access_token.ok_or_else(|| oauth_error("Microsoft did not return an access token"))?;
    let lifetime = Duration::from_secs(r.expires_in.unwrap_or(3600));
    Ok(Tokens {
        access_token,
        refresh_token: r.refresh_token,
        expires_at: Instant::now() + lifetime.saturating_sub(EXPIRY_MARGIN),
        email: r.id_token.as_deref().and_then(email_from_id_token),
    })
}

/// The signed-in address from an ID token's claims. The token came straight
/// from Microsoft over TLS, so its signature isn't checked here.
fn email_from_id_token(id_token: &str) -> Option<String> {
    #[derive(Deserialize)]
    struct Claims {
        email: Option<String>,
        preferred_username: Option<String>,
    }
    let payload = id_token.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload.trim_end_matches('='))
        .ok()?;
    let claims: Claims = serde_json::from_slice(&bytes).ok()?;
    // The sign-in name (UPN) is what IMAP's XOAUTH2 expects; `email` may be an alias.
    [claims.preferred_username, claims.email]
        .into_iter()
        .flatten()
        .map(|e| e.trim().to_owned())
        .find(|e| e.contains('@'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn token_with(claims: &str) -> String {
        let enc = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        format!("{}.{}.sig", enc.encode(r#"{"alg":"none"}"#), enc.encode(claims))
    }

    #[test]
    fn reads_email_from_id_token() {
        let t = token_with(r#"{"email":"alias@x.com","preferred_username":"me@outlook.com"}"#);
        assert_eq!(email_from_id_token(&t).as_deref(), Some("me@outlook.com"));
        let t = token_with(r#"{"email":"me@live.com","preferred_username":"+15555550100"}"#);
        assert_eq!(email_from_id_token(&t).as_deref(), Some("me@live.com"));
        let t = token_with(r#"{"preferred_username":"me@hotmail.com"}"#);
        assert_eq!(email_from_id_token(&t).as_deref(), Some("me@hotmail.com"));
        // A username that isn't an address (e.g. a phone number) is ignored.
        let t = token_with(r#"{"preferred_username":"+15555550100"}"#);
        assert_eq!(email_from_id_token(&t), None);
        assert_eq!(email_from_id_token("garbage"), None);
    }

    #[test]
    fn error_text_keeps_first_line_only() {
        let r = Reply {
            error: Some("invalid_client".into()),
            error_description: Some("AADSTS700016: App not found.\r\nTrace ID: abc".into()),
            ..Default::default()
        };
        assert_eq!(r.error_text(), "invalid_client: AADSTS700016: App not found.");
        let r = Reply {
            error: Some("unauthorized_client".into()),
            error_description: Some("AADSTS700038: not valid. Trace ID: 3c99 Correlation ID: 2598".into()),
            ..Default::default()
        };
        assert_eq!(r.error_text(), "unauthorized_client: AADSTS700038: not valid.");
    }
}
