//! Auth at the gateway (docs/DESIGN.md §2). Two ways in, both producing the same
//! cookie session:
//!   * Okta/OIDC — Authorization-Code-+-PKCE via `openidconnect` (`/auth/login`).
//!   * JIRA-token login — POST email+token, validated against JIRA `/myself`
//!     (`/auth/jira`); your JIRA identity becomes your Weavn identity. A pragmatic
//!     stand-in for Okta while SSO isn't set up.
//! Either is env-gated and absent = off (dev mode). When any is on, the chat routes
//! require a session and attribute messages to the authenticated user.
//!
//! Env: WEAVN_OIDC_ISSUER/_CLIENT_ID/_CLIENT_SECRET/_REDIRECT_URL (Okta);
//!      WEAVN_JIRA_BASE (+ WEAVN_JIRA_AUTH=basic|bearer) enables JIRA-token login.

use std::collections::HashMap;
use std::sync::Mutex;

use base64::prelude::{Engine as _, BASE64_STANDARD};
use openidconnect::core::{CoreAuthenticationFlow, CoreClient, CoreProviderMetadata};
use openidconnect::reqwest::async_http_client;
use openidconnect::{
    AuthorizationCode, ClientId, ClientSecret, CsrfToken, IssuerUrl, Nonce, PkceCodeChallenge,
    PkceCodeVerifier, RedirectUrl, Scope, TokenResponse,
};

/// A user's JIRA credentials, kept server-side for per-user writes. Never serialized.
#[derive(Clone)]
pub struct JiraCreds {
    pub base: String,
    pub email: Option<String>,
    pub token: String,
    pub bearer: bool,
}

/// A logged-in person.
#[derive(Clone, serde::Serialize)]
pub struct Session {
    pub subject: String, // JIRA accountId or OIDC subject
    pub email: Option<String>,
    pub display_name: Option<String>,
    pub site: Option<String>, // public JIRA base URL, for permalinks (not secret)
    #[serde(skip)]
    pub jira: Option<JiraCreds>, // present for JIRA-token logins; enables per-user writes
}

impl Session {
    /// Who to attribute a message to: display name, else email, else subject.
    pub fn author(&self) -> String {
        self.display_name
            .clone()
            .or_else(|| self.email.clone())
            .unwrap_or_else(|| self.subject.clone())
    }
}

struct Pending {
    pkce_verifier: String,
    nonce: String,
}

pub struct Auth {
    client: Option<CoreClient>,
    jira_base: Option<String>,   // default site (single-tenant dev)
    jira_bearer: bool,
    #[allow(dead_code)] // reserved for gating; the Postgres gateway always requires a session
    jira_multitenant: bool,      // accept any <subdomain>.atlassian.net at login
    http: reqwest::Client,
    pending: Mutex<HashMap<String, Pending>>,
    sessions: Mutex<HashMap<String, Session>>,
}

/// A JIRA Cloud site subdomain is a DNS label: letters/digits/hyphens, 1–63 chars,
/// not starting or ending with a hyphen. Validating it keeps the constructed URL
/// pinned to `https://<label>.atlassian.net` (no injected host / SSRF).
fn valid_subdomain(s: &str) -> bool {
    let s = s.as_bytes();
    (1..=63).contains(&s.len())
        && s[0] != b'-'
        && s[s.len() - 1] != b'-'
        && s.iter().all(|b| b.is_ascii_alphanumeric() || *b == b'-')
}

impl Auth {
    /// Build from env; discovery failure or missing config → SSO off (dev mode).
    pub async fn from_env() -> Self {
        let client = match Self::build_client().await {
            Ok(c) => {
                println!("SSO: Okta/OIDC configured");
                Some(c)
            }
            Err(e) => {
                println!("SSO: off ({e})");
                None
            }
        };
        let jira_base = std::env::var("WEAVN_JIRA_BASE")
            .ok()
            .map(|b| b.trim_end_matches('/').to_string());
        let jira_bearer = std::env::var("WEAVN_JIRA_AUTH")
            .map(|v| v.eq_ignore_ascii_case("bearer"))
            .unwrap_or(false);
        let jira_multitenant = std::env::var("WEAVN_JIRA_MULTITENANT")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);
        match (&jira_base, jira_multitenant) {
            (_, true) => println!("Auth: JIRA-token login enabled (multi-tenant — any *.atlassian.net)"),
            (Some(b), _) => println!("Auth: JIRA-token login enabled (default site {b})"),
            (None, false) => println!("Auth: JIRA-token login off (set WEAVN_JIRA_BASE or WEAVN_JIRA_MULTITENANT)"),
        }
        Auth {
            client,
            jira_base,
            jira_bearer,
            jira_multitenant,
            http: reqwest::Client::new(),
            pending: Mutex::new(HashMap::new()),
            sessions: Mutex::new(HashMap::new()),
        }
    }

    async fn build_client() -> Result<CoreClient, String> {
        let get = |k: &str| std::env::var(k).map_err(|_| format!("{k} unset"));
        let issuer = get("WEAVN_OIDC_ISSUER")?;
        let client_id = get("WEAVN_OIDC_CLIENT_ID")?;
        let client_secret = get("WEAVN_OIDC_CLIENT_SECRET")?;
        let redirect = get("WEAVN_OIDC_REDIRECT_URL")?;
        let metadata = CoreProviderMetadata::discover_async(
            IssuerUrl::new(issuer).map_err(|e| e.to_string())?,
            async_http_client,
        )
        .await
        .map_err(|e| format!("discovery failed: {e}"))?;
        Ok(CoreClient::from_provider_metadata(
            metadata,
            ClientId::new(client_id),
            Some(ClientSecret::new(client_secret)),
        )
        .set_redirect_uri(RedirectUrl::new(redirect).map_err(|e| e.to_string())?))
    }

    /// Whether any auth method is configured (Okta or JIRA-token login).
    #[allow(dead_code)]
    pub fn is_on(&self) -> bool {
        self.client.is_some() || self.jira_base.is_some() || self.jira_multitenant
    }

    /// Log in with a JIRA site subdomain + email + token: build the Cloud base URL
    /// from the subdomain (`https://<site>.atlassian.net`), validate against
    /// `/myself`, and create a session bound to the JIRA account. `site` may be
    /// omitted only when a default site (`WEAVN_JIRA_BASE`) is configured.
    pub async fn jira_login(&self, site: Option<&str>, email: &str, token: &str) -> Result<String, String> {
        let base = match site.map(str::trim).filter(|s| !s.is_empty()) {
            Some(s) => {
                if !valid_subdomain(s) {
                    return Err("invalid JIRA site — use just the subdomain, e.g. acme".to_string());
                }
                format!("https://{}.atlassian.net", s.to_ascii_lowercase())
            }
            None => self
                .jira_base
                .clone()
                .ok_or("JIRA site (subdomain) is required")?,
        };
        let authz = if self.jira_bearer {
            format!("Bearer {token}")
        } else {
            format!("Basic {}", BASE64_STANDARD.encode(format!("{email}:{token}")))
        };
        let resp = self
            .http
            .get(format!("{base}/rest/api/3/myself"))
            .header("Authorization", authz)
            .header("Accept", "application/json")
            .send()
            .await
            .map_err(|e| e.to_string())?;
        if !resp.status().is_success() {
            return Err(format!("JIRA auth failed ({})", resp.status()));
        }
        let v: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
        let session = Session {
            subject: v["accountId"].as_str().unwrap_or_default().to_string(),
            email: v["emailAddress"].as_str().map(str::to_string).or_else(|| Some(email.to_string())),
            display_name: v["displayName"].as_str().map(str::to_string),
            site: Some(base.clone()),
            jira: Some(JiraCreds {
                base,
                email: Some(email.to_string()),
                token: token.to_string(),
                bearer: self.jira_bearer,
            }),
        };
        let sid = CsrfToken::new_random().secret().clone();
        self.sessions.lock().unwrap().insert(sid.clone(), session);
        Ok(sid)
    }

    /// Build the Okta authorize URL and remember the PKCE verifier + nonce.
    pub fn start_login(&self) -> Option<String> {
        let client = self.client.as_ref()?;
        let (challenge, verifier) = PkceCodeChallenge::new_random_sha256();
        let (url, csrf, nonce) = client
            .authorize_url(
                CoreAuthenticationFlow::AuthorizationCode,
                CsrfToken::new_random,
                Nonce::new_random,
            )
            .add_scope(Scope::new("email".to_string()))
            .add_scope(Scope::new("profile".to_string()))
            .set_pkce_challenge(challenge)
            .url();
        self.pending.lock().unwrap().insert(
            csrf.secret().clone(),
            Pending {
                pkce_verifier: verifier.secret().clone(),
                nonce: nonce.secret().clone(),
            },
        );
        Some(url.to_string())
    }

    /// Exchange the code, verify the id_token, and create a session. Returns the
    /// new session id (to set as a cookie).
    pub async fn complete_login(&self, code: String, state: String) -> Result<String, String> {
        let client = self.client.as_ref().ok_or("SSO not configured")?;
        let pending = self
            .pending
            .lock()
            .unwrap()
            .remove(&state)
            .ok_or("unknown or expired state")?;
        let token = client
            .exchange_code(AuthorizationCode::new(code))
            .set_pkce_verifier(PkceCodeVerifier::new(pending.pkce_verifier))
            .request_async(async_http_client)
            .await
            .map_err(|e| format!("token exchange failed: {e}"))?;
        let id_token = token.id_token().ok_or("no id_token in response")?;
        let claims = id_token
            .claims(&client.id_token_verifier(), &Nonce::new(pending.nonce))
            .map_err(|e| format!("id_token verification failed: {e}"))?;
        let session = Session {
            subject: claims.subject().as_str().to_string(),
            email: claims.email().map(|e| e.as_str().to_string()),
            display_name: None,
            site: None,
            jira: None, // Okta login carries no JIRA token; per-user writes need JIRA login
        };
        let sid = CsrfToken::new_random().secret().clone();
        self.sessions.lock().unwrap().insert(sid.clone(), session);
        Ok(sid)
    }

    pub fn session(&self, sid: &str) -> Option<Session> {
        self.sessions.lock().unwrap().get(sid).cloned()
    }

    pub fn logout(&self, sid: &str) {
        self.sessions.lock().unwrap().remove(sid);
    }

    /// JIRA creds from any live session for `account` (accountId). Used by the
    /// scheduler to send a deferred message as its author — only while they have
    /// an active session, since tokens are never persisted at rest.
    pub fn creds_for_account(&self, account: &str) -> Option<JiraCreds> {
        self.sessions
            .lock()
            .unwrap()
            .values()
            .find(|s| s.subject == account && s.jira.is_some())
            .and_then(|s| s.jira.clone())
    }
}

/// Pull the `weavn_session` value out of a Cookie header, if present.
pub fn session_cookie(cookie_header: Option<&str>) -> Option<String> {
    cookie_header?.split(';').find_map(|part| {
        let part = part.trim();
        part.strip_prefix("weavn_session=").map(|v| v.to_string())
    })
}
