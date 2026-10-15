//! Weavn gateway — axum server (docs/DESIGN.md §2) with Okta + JIRA-token auth.
//! Chat requires a JIRA session; sends/replies go to REAL JIRA with the user's own
//! token (reporter = them), and the projection lives in Postgres (survives
//! restarts). JIRA writes run on a blocking thread; the DB is async (sqlx).

mod auth;
mod hub;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Multipart, Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{AppendHeaders, Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::prelude::{Engine as _, BASE64_URL_SAFE_NO_PAD};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use weavn::jira::real::RealBackend;
use weavn::jira::Backend;
use weavn_store::Pool;

const COALESCE_SECS: i64 = 90;

struct AppState {
    pool: Pool,
    auth: auth::Auth,
    hub: hub::Hub,   // real-time fan-out (live delivery, typing, presence)
    /// SaaS base domain (e.g. `weavn.io`). When set, the gateway is multi-tenant:
    /// the Host's subdomain identifies the tenant and pins login to its JIRA site.
    /// Unset = single-tenant dev (login takes the JIRA site from the form).
    base_domain: Option<String>,
    /// Cache of project-access checks for channel→project resolution, keyed by
    /// "workspace\u{1f}KEY" → (checked_at, canonical key if accessible). TTL'd.
    proj_cache: std::sync::Mutex<HashMap<String, (std::time::Instant, Option<String>)>>,
}

/// A live "the channel changed, reload it" event for the fan-out hub.
fn event_message(channel: &str) -> String {
    serde_json::json!({ "type": "message", "channel": channel }).to_string()
}

/// Generate in-app notifications for a new message: @mentions and keyword
/// subscriptions (never the actor themselves), then nudge clients to refetch.
async fn notify_for_message(s: &AppState, ws: &str, actor: &str, actor_acc: &str, channel: &str, issue: &str, body: &str, mentions: &[Mention]) {
    let snippet: String = body.chars().take(140).collect();
    for m in mentions {
        if m.id != actor_acc {
            let _ = weavn_store::notify_insert(&s.pool, ws, &m.id, "mention", channel, issue, actor, &snippet).await;
        }
    }
    let lower = body.to_lowercase();
    if let Ok(subs) = weavn_store::keyword_subs_all(&s.pool, ws).await {
        for (account, kw) in subs {
            if account != actor_acc && !mentions.iter().any(|m| m.id == account) && lower.contains(&kw.to_lowercase()) {
                let _ = weavn_store::notify_insert(&s.pool, ws, &account, "keyword", channel, issue, actor, &snippet).await;
            }
        }
    }
    s.hub.publish(ws, serde_json::json!({ "type": "notify" }).to_string());
}

/// Workflow Builder: run enabled rules against a new top-level message. Actions
/// are an allowlist executed server-side (JIRA writes via the service account).
/// Only fires on new conversations (not replies/coalesce), so it can't loop.
async fn run_workflows(s: &AppState, ws: &str, channel: &str, issue: &str, body: &str) {
    let rules = match weavn_store::workflows_enabled(&s.pool, ws).await {
        Ok(r) => r,
        Err(_) => return,
    };
    let bl = body.to_lowercase();
    let mut ran = false;
    for (_id, rchan, contains, kind, arg) in rules {
        if rchan != "*" && rchan != channel {
            continue;
        }
        if !contains.is_empty() && !bl.contains(&contains.to_lowercase()) {
            continue;
        }
        match kind.as_str() {
            "react" => {
                let _ = weavn_store::react_set(&s.pool, ws, issue, &arg, "__workflow__", true).await;
                ran = true;
            }
            "pin" => {
                let _ = weavn_store::pin_set(&s.pool, ws, channel, issue, "__workflow__", true).await;
                ran = true;
            }
            "reply" | "transition" | "addlabel" => {
                let (issue2, arg2, kind2) = (issue.to_string(), arg.clone(), kind.clone());
                let res = tokio::task::spawn_blocking(move || -> Result<Option<String>, String> {
                    let mut be = RealBackend::from_env()?;
                    match kind2.as_str() {
                        "reply" => {
                            let adf = weavn::format::mrkdwn_to_adf(&arg2);
                            be.add_comment(&issue2, "Workflow", &arg2, &adf)?;
                            Ok(None)
                        }
                        "transition" => {
                            be.transition_to(&issue2, &arg2)?;
                            Ok(Some(be.issue_status(&issue2).unwrap_or_default()))
                        }
                        "addlabel" => {
                            be.set_label(&issue2, &arg2, true)?;
                            Ok(None)
                        }
                        _ => Ok(None),
                    }
                })
                .await;
                if let Ok(Ok(opt)) = res {
                    match kind.as_str() {
                        "reply" => {
                            let _ = weavn_store::message_insert(&s.pool, ws, channel, issue, Some(issue), "Workflow", &arg, false, false).await;
                        }
                        "transition" => {
                            if let Some(st) = opt {
                                let _ = weavn_store::status_set(&s.pool, ws, issue, &st).await;
                            }
                        }
                        _ => {}
                    }
                    ran = true;
                }
            }
            _ => {}
        }
    }
    if ran {
        s.hub.publish(ws, event_message(channel));
    }
}

const SCHED_MAX_TRIES: i32 = 40; // ~10 min of retries while the author is offline

/// Background scheduler: every 15s, fire due reminders and send-later messages.
async fn scheduler_loop(s: Shared) {
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(15)).await;
        let due = match weavn_store::scheduled_due(&s.pool, 20).await {
            Ok(d) => d,
            Err(_) => continue,
        };
        for row in due {
            process_scheduled(&s, row).await;
        }
    }
}

async fn process_scheduled(s: &AppState, row: weavn_store::ScheduledRow) {
    let ws = row.workspace;
    if row.kind == "reminder" {
        let snippet: String = row.body.chars().take(140).collect();
        let _ = weavn_store::notify_insert(&s.pool, &ws, &row.author_account, "reminder", &row.channel, "", "Reminder", &snippet).await;
        s.hub.publish(&ws, serde_json::json!({ "type": "notify" }).to_string());
        let _ = weavn_store::scheduled_mark_done(&s.pool, row.id).await;
        return;
    }
    // message: needs the author's live JIRA creds (tokens are session-only)
    let Some(creds) = s.auth.creds_for_account(&row.author_account) else {
        if row.tries >= SCHED_MAX_TRIES {
            let _ = weavn_store::notify_insert(&s.pool, &ws, &row.author_account, "reminder", &row.channel, "", "Weavn", "A scheduled message could not be sent — you were offline when it was due.").await;
            s.hub.publish(&ws, serde_json::json!({ "type": "notify" }).to_string());
            let _ = weavn_store::scheduled_mark_done(&s.pool, row.id).await;
        } else {
            let _ = weavn_store::scheduled_bump(&s.pool, row.id).await;
        }
        return;
    };
    let channel = row.channel.clone();
    let (project, slug) = match resolve_target(s, Some(&creds), &ws, &channel).await {
        Ok(v) => v,
        Err((_, msg)) => {
            let _ = weavn_store::notify_insert(&s.pool, &ws, &row.author_account, "reminder", &channel, "", "Weavn", &format!("A scheduled message could not be sent: {msg}")).await;
            s.hub.publish(&ws, serde_json::json!({ "type": "notify" }).to_string());
            let _ = weavn_store::scheduled_mark_done(&s.pool, row.id).await;
            return;
        }
    };
    let body = row.body.clone();
    let summary = weavn::proto::summarize(&body, 80);
    let (c, b2, sum2) = (creds.clone(), body.clone(), summary.clone());
    let created = tokio::task::spawn_blocking(move || {
        let mut b = RealBackend::from_parts(&c.base, c.email.as_deref(), &c.token, c.bearer);
        let adf = weavn::format::mrkdwn_to_adf(&b2);
        let key = b.create_issue(&project, &slug, &sum2, &b2, &adf).map(|ci| ci.key)?;
        let status = b.issue_status(&key).unwrap_or_default();
        Ok::<_, String>((key, status))
    })
    .await;
    match created {
        Ok(Ok((key, status))) => {
            let _ = weavn_store::message_insert(&s.pool, &ws, &channel, &key, None, &row.author_display, &body, false, false).await;
            let _ = weavn_store::status_set(&s.pool, &ws, &key, &status).await;
            notify_for_message(s, &ws, &row.author_display, &row.author_account, &channel, &key, &body, &[]).await;
            s.hub.publish(&ws, event_message(&channel));
            let _ = weavn_store::scheduled_mark_done(&s.pool, row.id).await;
        }
        _ => {
            // JIRA rejected or task failed — retry up to the cap, then give up.
            if row.tries >= SCHED_MAX_TRIES {
                let _ = weavn_store::scheduled_mark_done(&s.pool, row.id).await;
            } else {
                let _ = weavn_store::scheduled_bump(&s.pool, row.id).await;
            }
        }
    }
}

type Shared = Arc<AppState>;

#[tokio::main]
async fn main() {
    let db_url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
        eprintln!("DATABASE_URL is required (run ./scripts/pg.sh up and set scripts/db.env)");
        std::process::exit(1);
    });
    let pool = weavn_store::connect(&db_url).await.expect("connect Postgres");
    weavn_store::migrate(&pool).await.expect("migrate");

    let base_domain = std::env::var("WEAVN_BASE_DOMAIN")
        .ok()
        .map(|d| d.trim().trim_start_matches('.').to_ascii_lowercase())
        .filter(|d| !d.is_empty());
    if let Some(d) = &base_domain {
        println!("Tenancy: multi-tenant on *.{d} (subdomain = JIRA site label)");
    } else {
        println!("Tenancy: single-tenant dev (set WEAVN_BASE_DOMAIN to enable subdomains)");
    }

    let state: Shared = Arc::new(AppState {
        pool,
        auth: auth::Auth::from_env().await,
        hub: hub::Hub::default(),
        base_domain,
        proj_cache: std::sync::Mutex::new(HashMap::new()),
    });

    // In-process scheduler for "send later" messages and reminders.
    tokio::spawn(scheduler_loop(state.clone()));

    let app = Router::new()
        .route("/", get(client))
        .route("/health", get(health))
        .route("/send", post(send))
        .route("/reply", post(reply))
        .route("/edit", post(edit))
        .route("/delete", post(delete_conversation))
        .route("/pin", post(pin))
        .route("/transitions", get(transitions))
        .route("/transition", post(transition))
        .route("/refine", post(refine))
        .route("/read", get(read))
        .route("/threads", get(threads_feed))
        .route("/stream", get(stream_feed))
        .route("/epics", get(epics))
        .route("/children", get(children))
        .route("/issue", get(issue_detail))
        .route("/issue/comment", post(issue_comment))
        .route("/status-map", get(status_map_get).post(status_map_set))
        .route("/sidebar", get(sidebar_get).post(sidebar_set))
        .route("/export", get(export))
        .route("/discover", get(discover))
        .route("/users", get(users))
        .route("/upload", post(upload))
        .route("/attachment/{jid}", get(attachment))
        .route("/seed", post(seed))
        .route("/backfill", post(backfill))
        .route("/channels", get(channels))
        .route("/channel/acl", get(channel_acl_get))
        .route("/channel/private", post(channel_set_private))
        .route("/channel/member", post(channel_member))
        .route("/star", post(star))
        .route("/archive", post(archive))
        .route("/save", post(save))
        .route("/saved", get(saved))
        .route("/state", get(get_state).post(set_state))
        .route("/availability", get(availability_get).post(availability_set))
        .route("/notifications", get(notifications_get))
        .route("/notifications/read", post(notifications_read))
        .route("/keywords", get(keywords_get).post(keywords_set))
        .route("/schedule", post(schedule_create))
        .route("/scheduled", get(scheduled_get))
        .route("/schedule/cancel", post(schedule_cancel))
        .route("/webhook", post(webhook_create))
        .route("/webhooks", get(webhooks_list))
        .route("/webhook/delete", post(webhook_delete))
        .route("/hook/{token}", post(hook))
        .route("/unfurl", get(unfurl))
        .route("/sprints", get(sprints))
        .route("/sprint/assign", post(sprint_assign))
        .route("/dm/send", post(dm_send))
        .route("/dm/list", get(dm_list))
        .route("/dm/read", get(dm_read))
        .route("/bots", get(bots_list).post(bots_create))
        .route("/bots/delete", post(bots_delete))
        .route("/bot/post", post(bot_post))
        .route("/action", post(action))
        .route("/workflows", get(workflows_get).post(workflows_create))
        .route("/workflows/delete", post(workflows_delete))
        .route("/workflows/toggle", post(workflows_toggle))
        .route("/react", post(react))
        .route("/markread", post(mark_read))
        .route("/search", get(search))
        .route("/ws", get(ws_handler))
        .route("/auth/login", get(auth_login))
        .route("/auth/callback", get(auth_callback))
        .route("/auth/jira", post(auth_jira))
        .route("/auth/config", get(auth_config))
        .route("/auth/me", get(auth_me))
        .route("/auth/logout", get(auth_logout))
        // allow file uploads up to ~26 MB (the /upload handler caps content at 25)
        .layer(axum::extract::DefaultBodyLimit::max(26 * 1024 * 1024))
        .with_state(state);

    // Bind address: default to localhost for dev; the container sets
    // WEAVN_BIND=0.0.0.0:8080 so the pod is reachable by the Service/probes.
    let bind = std::env::var("WEAVN_BIND").unwrap_or_else(|_| "127.0.0.1:8080".to_string());
    let listener = tokio::net::TcpListener::bind(&bind).await.expect("bind");
    println!("weavn-gateway listening on http://{bind}");
    axum::serve(listener, app).await.expect("serve");
}

async fn health() -> &'static str {
    "ok"
}

/// The single-page web client (login + chat), wired to the endpoints below.
async fn client() -> Html<&'static str> {
    Html(include_str!("app.html"))
}

// --- real-time: WebSocket fan-out (docs/DESIGN.md §5) -----------------------

/// Upgrade to a WebSocket for live events. Authenticated by the session cookie;
/// a logged-in session is enough (no JIRA token needed to receive updates).
async fn ws_handler(ws: WebSocketUpgrade, State(s): State<Shared>, headers: HeaderMap) -> Response {
    let Some(sess) = sid_from(&headers).and_then(|sid| s.auth.session(&sid)) else {
        return (StatusCode::UNAUTHORIZED, "login required").into_response();
    };
    let wsid = sess.site.clone().unwrap_or_else(|| "default".to_string());
    let id = sess.subject.clone();
    let display = sess.author();
    ws.on_upgrade(move |socket| handle_socket(socket, s, wsid, id, display))
}

async fn handle_socket(socket: WebSocket, s: Shared, wsid: String, id: String, display: String) {
    let rx = s.hub.join(&wsid, &id, &display);
    let (mut sender, mut receiver) = socket.split();

    // hub -> client
    let send_task = tokio::spawn(async move {
        let mut rx = rx;
        loop {
            match rx.recv().await {
                Ok(msg) => {
                    if sender.send(Message::Text(msg.into())).await.is_err() {
                        break;
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(_) => break,
            }
        }
    });

    // client -> hub (typing notices); ends when the socket closes
    while let Some(Ok(msg)) = receiver.next().await {
        match msg {
            Message::Text(txt) => {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&txt) {
                    if v["type"] == "typing" {
                        if let Some(ch) = v["channel"].as_str() {
                            let ev = serde_json::json!({ "type": "typing", "channel": ch, "who": display });
                            s.hub.publish(&wsid, ev.to_string());
                        }
                    }
                }
            }
            Message::Close(_) => break,
            _ => {}
        }
    }

    send_task.abort();
    s.hub.leave(&wsid, &id);
}

// --- auth / SSO -------------------------------------------------------------

async fn auth_login(State(s): State<Shared>) -> Response {
    match s.auth.start_login() {
        Some(url) => Redirect::to(&url).into_response(),
        None => (StatusCode::SERVICE_UNAVAILABLE, "SSO not configured").into_response(),
    }
}

#[derive(Deserialize)]
struct CallbackQuery {
    code: String,
    state: String,
}

async fn auth_callback(State(s): State<Shared>, Query(q): Query<CallbackQuery>) -> Response {
    match s.auth.complete_login(q.code, q.state).await {
        Ok(sid) => {
            meter_seat(&s, &sid).await;
            (set_session_cookie(&sid), Redirect::to("/auth/me")).into_response()
        }
        Err(e) => (StatusCode::BAD_REQUEST, e).into_response(),
    }
}

/// The tenant subdomain for this request's Host, when multi-tenant and the Host is
/// `<label>.<base_domain>` with a non-reserved single label. `None` for the apex
/// (`weavn.io`), a reserved host (`app.`/`www.`…), or single-tenant dev.
fn host_subdomain(s: &AppState, headers: &HeaderMap) -> Option<String> {
    let base = s.base_domain.as_deref()?;
    let host = headers.get(header::HOST)?.to_str().ok()?;
    let host = host.split(':').next().unwrap_or(host).to_ascii_lowercase();
    let label = host.strip_suffix(&format!(".{base}"))?;
    if label.is_empty() || label.contains('.') {
        return None; // apex, or a deeper name we don't treat as a tenant
    }
    if weavn_store::tenant::RESERVED.contains(&label) {
        return None; // app./www./api.… are platform hosts, not tenants
    }
    Some(label.to_string())
}

/// What the client needs to render the right login UI for this host.
#[derive(Serialize)]
struct AuthConfig {
    multitenant: bool,
    base_domain: Option<String>,
    subdomain: Option<String>, // Some = a tenant host; None = apex / dev
}

async fn auth_config(State(s): State<Shared>, headers: HeaderMap) -> Response {
    Json(AuthConfig {
        multitenant: s.base_domain.is_some(),
        base_domain: s.base_domain.clone(),
        subdomain: host_subdomain(&s, &headers),
    })
    .into_response()
}

#[derive(Deserialize)]
struct JiraLoginReq {
    #[serde(default)]
    site: Option<String>,
    email: String,
    token: String,
}

/// Usage snapshot returned on login so the client can show a warning banner.
#[derive(Serialize)]
struct UsageInfo {
    active_seats: i64,
    entitled: i64,
    over: i64,
    licensed: bool,
}

#[derive(Serialize)]
struct LoginResp {
    session: Option<auth::Session>,
    subdomain: Option<String>, // canonical tenant subdomain (for apex -> redirect)
    redirect: Option<String>,  // set when the browser is on the wrong host
    usage: Option<UsageInfo>,
}

async fn auth_jira(State(s): State<Shared>, headers: HeaderMap, Json(r): Json<JiraLoginReq>) -> Response {
    // Multi-tenant: the Host's subdomain pins which JIRA site this login may use, so
    // a browser on domaintools.weavn.io can only sign into domaintools.atlassian.net.
    let host_sub = host_subdomain(&s, &headers);
    let (site, apex): (Option<String>, bool) = match (&s.base_domain, &host_sub) {
        (Some(_), Some(sub)) => (Some(sub.clone()), false), // pinned by host
        (Some(_), None) => {
            // Apex host: don't authenticate here — send them to their workspace host.
            return (
                StatusCode::BAD_REQUEST,
                "sign in at your workspace URL, e.g. https://<site>.weavn.io".to_string(),
            )
                .into_response();
        }
        (None, _) => (r.site.clone(), true), // single-tenant dev: take site from the form
    };

    let sid = match s.auth.jira_login(site.as_deref(), &r.email, &r.token).await {
        Ok(sid) => sid,
        Err(e) => return (StatusCode::UNAUTHORIZED, e).into_response(),
    };
    let Some(sess) = s.auth.session(&sid) else {
        return (StatusCode::INTERNAL_SERVER_ERROR, "session lost".to_string()).into_response();
    };
    let jira_base = sess.site.clone().unwrap_or_default();
    let canonical = weavn_store::tenant::subdomain_for_base(&jira_base);

    // Admission control: meter the seat, enforcing the free-tier hard cap.
    let usage = match weavn_store::seat_claim_enforced(
        &s.pool,
        &jira_base,
        &sess.subject,
        &sess.author(),
        sess.email.as_deref(),
    )
    .await
    {
        Ok(weavn_store::Admission::Admitted(c)) => Some(UsageInfo {
            active_seats: c.active_seats,
            entitled: c.entitled,
            over: c.over,
            licensed: c.licensed,
        }),
        Ok(weavn_store::Admission::Full { active_seats, entitled }) => {
            // Over the free cap and unlicensed: refuse, drop the session.
            s.auth.logout(&sid);
            let body = serde_json::json!({
                "error": "workspace_full",
                "active_seats": active_seats,
                "entitled": entitled,
                "message": format!(
                    "This workspace is full ({active_seats}/{entitled} seats on the free plan). \
                     Contact us to lift the cap."
                ),
            });
            return (StatusCode::FORBIDDEN, Json(body)).into_response();
        }
        Err(e) => return db_err(e).into_response(),
    };

    // Provision/refresh the tenant record (control-plane: subdomain + email only).
    if let Some(sub) = &canonical {
        let _ = weavn_store::tenant::provision(
            &s.pool,
            sub,
            &jira_base,
            sess.email.as_deref(),
            sess.display_name.as_deref(),
        )
        .await;
        let _ = weavn_store::tenant::touch(&s.pool, sub).await;
        let plan = if usage.as_ref().map(|u| u.licensed).unwrap_or(false) { "commercial" } else { "free" };
        let _ = weavn_store::tenant::set_plan(&s.pool, sub, plan).await;
        let _ = weavn_store::tenant::log_login(&s.pool, sub, &sess.subject, sess.email.as_deref()).await;
    }

    // On the apex (dev or a stray entry point), tell the client where its home is.
    let redirect = if apex {
        None
    } else {
        match (&s.base_domain, &canonical, &host_sub) {
            (Some(base), Some(c), Some(h)) if c != h => Some(format!("https://{c}.{base}/")),
            _ => None,
        }
    };

    (
        set_session_cookie(&sid),
        Json(LoginResp { session: Some(sess), subdomain: canonical, redirect, usage }),
    )
        .into_response()
}

/// Licensing meter (docs/LICENSING.md): claim a seat for an OIDC sign-in under the
/// user's JIRA namespace. Fire-and-forget — never blocks a login (OIDC carries no
/// JIRA creds, so it's a no-op unless a JIRA session exists).
async fn meter_seat(s: &AppState, sid: &str) {
    if let Some(sess) = s.auth.session(sid) {
        if let Some(creds) = sess.jira.as_ref() {
            let _ = weavn_store::seat_claim(
                &s.pool,
                &creds.base,
                &sess.subject,
                &sess.author(),
                sess.email.as_deref(),
            )
            .await;
        }
    }
}

fn set_session_cookie(sid: &str) -> impl axum::response::IntoResponseParts {
    AppendHeaders([(
        header::SET_COOKIE,
        format!("weavn_session={sid}; HttpOnly; SameSite=Lax; Path=/"),
    )])
}

fn sid_from(headers: &HeaderMap) -> Option<String> {
    auth::session_cookie(headers.get(header::COOKIE).and_then(|v| v.to_str().ok()))
}

async fn auth_me(State(s): State<Shared>, headers: HeaderMap) -> Response {
    match sid_from(&headers).and_then(|sid| s.auth.session(&sid)) {
        Some(session) => Json(session).into_response(),
        None => (StatusCode::UNAUTHORIZED, "not logged in").into_response(),
    }
}

async fn auth_logout(State(s): State<Shared>, headers: HeaderMap) -> Response {
    if let Some(sid) = sid_from(&headers) {
        s.auth.logout(&sid);
    }
    let clear = "weavn_session=; HttpOnly; SameSite=Lax; Path=/; Max-Age=0";
    (AppendHeaders([(header::SET_COOKIE, clear)]), "logged out").into_response()
}

// --- shared helpers ---------------------------------------------------------

type ApiError = (StatusCode, String);

fn bad_request(e: String) -> ApiError {
    (StatusCode::BAD_REQUEST, e)
}

fn db_err<E: std::fmt::Display>(e: E) -> ApiError {
    (StatusCode::INTERNAL_SERVER_ERROR, format!("db: {e}"))
}

fn unauthorized() -> ApiError {
    (StatusCode::UNAUTHORIZED, "login required".to_string())
}

/// The current session, or 401 when auth is configured and absent.
fn session(s: &AppState, headers: &HeaderMap) -> Result<auth::Session, ApiError> {
    sid_from(headers)
        .and_then(|sid| s.auth.session(&sid))
        .ok_or_else(unauthorized)
}

/// Require a JIRA-login session (has a token); returns (creds, author, workspace).
fn jira_session(s: &AppState, headers: &HeaderMap) -> Result<(auth::JiraCreds, String, String), ApiError> {
    let sess = session(s, headers)?;
    let author = sess.author();
    let creds = sess
        .jira
        .ok_or((StatusCode::BAD_REQUEST, "JIRA login required".to_string()))?;
    let ws = creds.base.clone();
    Ok((creds, author, ws))
}

/// The session's JIRA accountId (stable per-user key), or 401.
fn subject(s: &AppState, headers: &HeaderMap) -> Result<String, ApiError> {
    Ok(session(s, headers)?.subject)
}

/// 403 if `channel` is private and `user` (accountId) is not a member.
async fn require_channel_access(s: &AppState, ws: &str, user: &str, channel: &str) -> Result<(), ApiError> {
    if let Some((private, _by)) = weavn_store::channel_acl(&s.pool, ws, channel).await.map_err(db_err)? {
        if private && !weavn_store::channel_is_member(&s.pool, ws, channel, user).await.map_err(db_err)? {
            return Err((StatusCode::FORBIDDEN, "private channel — you are not a member".to_string()));
        }
    }
    Ok(())
}

/// A space-free channel id for the JIRA label (labels can't contain spaces).
fn channel_slug(ch: &str) -> String {
    ch.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '_' })
        .collect()
}

/// Whether `s` is shaped like a JIRA project key (a letter then alphanumerics,
/// 2–10 chars). Used to decide if a channel's first segment names a project.
fn looks_like_project_key(s: &str) -> bool {
    let b = s.as_bytes();
    (2..=10).contains(&b.len())
        && b[0].is_ascii_alphabetic()
        && b.iter().all(|c| c.is_ascii_alphanumeric())
}

/// The canonical key if `creds` can access a project named `key` (any accessible
/// project, not just the first 50), else `None`. Cached per workspace+key for 5 min.
async fn project_access(
    s: &AppState,
    creds: &auth::JiraCreds,
    ws: &str,
    key: &str,
) -> Result<Option<String>, ApiError> {
    const TTL: std::time::Duration = std::time::Duration::from_secs(300);
    let ck = format!("{ws}\u{1f}{}", key.to_ascii_uppercase());
    if let Some((at, v)) = s.proj_cache.lock().unwrap().get(&ck) {
        if at.elapsed() < TTL {
            return Ok(v.clone());
        }
    }
    let (c, k) = (creds.clone(), key.to_ascii_uppercase());
    let canonical = blocking(move || {
        RealBackend::from_parts(&c.base, c.email.as_deref(), &c.token, c.bearer)
            .project_key_if_accessible(&k)
    })
    .await?;
    s.proj_cache
        .lock()
        .unwrap()
        .insert(ck, (std::time::Instant::now(), canonical.clone()));
    Ok(canonical)
}

/// Resolve which JIRA project a NEW conversation in `channel` is created in, and the
/// label to tag it with (docs/MULTITENANCY.md). A channel IS a JIRA board/project:
/// the channel's first segment must name a project the poster can access (`ENG`, or
/// `ENG/backend` → project `ENG`, labelled `eng_backend`). There is NO default — you
/// can't post without a real channel, so an unresolvable channel is refused (409).
/// `creds` is the identity whose access is checked; `None` (a bot/webhook on the
/// service account) trusts a project-key-shaped segment (the create fails if it's
/// unreachable).
async fn resolve_target(
    s: &AppState,
    creds: Option<&auth::JiraCreds>,
    ws: &str,
    channel: &str,
) -> Result<(String, String), ApiError> {
    let label = channel_slug(channel);
    let top = channel.split('/').next().unwrap_or(channel).trim();
    if looks_like_project_key(top) {
        match creds {
            Some(c) => {
                if let Some(key) = project_access(s, c, ws, top).await? {
                    return Ok((key, label));
                }
            }
            None => return Ok((top.to_ascii_uppercase(), label)),
        }
    }
    Err((
        StatusCode::CONFLICT,
        format!(
            "\"{channel}\" isn't a board you can post to. A channel is a JIRA project \
             you can access — pick one (e.g. a project key like ENG), or use \
             PROJECT/topic for a sub-topic."
        ),
    ))
}

// --- chat -------------------------------------------------------------------

/// A resolved @mention the client inserted: the display text and JIRA accountId.
#[derive(Deserialize, Default)]
struct Mention {
    text: String,
    id: String,
}

fn mention_pairs(ms: &[Mention]) -> Vec<(String, String)> {
    ms.iter().map(|m| (m.text.clone(), m.id.clone())).collect()
}

#[derive(Deserialize)]
struct SendReq {
    channel: String,
    #[serde(default)]
    body: String,
    #[serde(default)]
    mentions: Vec<Mention>,
}

#[derive(Deserialize)]
struct ReplyReq {
    issue: String,
    #[serde(default)]
    body: String,
    #[serde(default)]
    broadcast: bool,
    #[serde(default)]
    mentions: Vec<Mention>,
}

#[derive(Deserialize)]
struct EditReq {
    issue: String,
    #[serde(default)]
    body: String,
}

#[derive(Deserialize)]
struct DeleteReq {
    issue: String,
}

#[derive(Deserialize)]
struct PinReq {
    channel: String,
    issue: String,
    on: bool,
}

#[derive(Deserialize)]
struct RefineReq {
    issue: String,
    commands: Vec<String>,
}

#[derive(Deserialize)]
struct ReadQuery {
    channel: String,
    #[serde(default)]
    subtree: bool,
}

#[derive(Serialize)]
struct OutcomeResp {
    kind: &'static str,
    issue: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    comment: Option<String>,
}

#[derive(Serialize)]
struct ThreadResp {
    issue_key: String,
    author: String,
    ts: i64, // unix seconds of the root message
    pinned: bool,
    deleted: bool,
    status: String,
    issue_type: String,
    parent: Option<String>,
    parent_summary: Option<String>,
    lines: Vec<String>,
    replies: Vec<ReplyResp>,
    reactions: Vec<ReactionResp>,
    attachments: Vec<AttachmentResp>,
    actions: Vec<ActionResp>,
}

#[derive(Serialize)]
struct ReplyResp {
    author: String,
    body: String,
    from_jira: bool,
    broadcast: bool,
    ts: i64,
}

#[derive(Serialize)]
struct ReactionResp {
    shortcode: String,
    count: i64,
    mine: bool,
}

#[derive(Serialize)]
struct AttachmentResp {
    jira_id: String,
    filename: String,
    mime: String,
    size: i64,
    is_image: bool,
    url: String,
}

/// Run a blocking JIRA closure off the async runtime, flattening join + app errors.
async fn blocking<T, F>(f: F) -> Result<T, ApiError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, String> + Send + 'static,
{
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("task join: {e}")))?
        .map_err(|e| (StatusCode::BAD_GATEWAY, e))
}

async fn send(State(s): State<Shared>, headers: HeaderMap, Json(r): Json<SendReq>) -> Result<Json<OutcomeResp>, ApiError> {
    let (creds, author, ws) = jira_session(&s, &headers)?;
    let me_acc = subject(&s, &headers)?;
    require_channel_access(&s, &ws, &me_acc, &r.channel).await?;
    let body = r.body.clone();
    let ments = mention_pairs(&r.mentions);

    // Coalesce: a recent top-level by the same author in this channel.
    let last = weavn_store::last_toplevel(&s.pool, &ws, &r.channel, COALESCE_SECS).await.map_err(db_err)?;
    if let Some(issue) = last.and_then(|(k, a)| (a == author).then_some(k)) {
        let existing = weavn_store::issue_toplevel_body(&s.pool, &ws, &issue).await.map_err(db_err)?;
        let merged = format!("{existing}\n{body}");
        let (c, issue2, merged2, m2) = (creds.clone(), issue.clone(), merged.clone(), ments.clone());
        blocking(move || {
            let mut b = RealBackend::from_parts(&c.base, c.email.as_deref(), &c.token, c.bearer);
            let adf = weavn::format::mrkdwn_to_adf_mentions(&merged2, &m2);
            b.set_description(&issue2, &merged2, &adf)
        })
        .await?;
        weavn_store::message_insert(&s.pool, &ws, &r.channel, &issue, None, &author, &body, false, false).await.map_err(db_err)?;
        notify_for_message(&s, &ws, &author, &me_acc, &r.channel, &issue, &body, &r.mentions).await;
        s.hub.publish(&ws, event_message(&r.channel));
        return Ok(Json(OutcomeResp { kind: "coalesced", issue, comment: None }));
    }

    // New conversation → a new JIRA issue in the channel's project.
    let (project, slug) = resolve_target(&s, Some(&creds), &ws, &r.channel).await?;
    let summary = weavn::proto::summarize(&body, 80);
    let (c, b2, sum2, m2) = (creds.clone(), body.clone(), summary.clone(), ments.clone());
    let (key, status) = blocking(move || {
        let mut b = RealBackend::from_parts(&c.base, c.email.as_deref(), &c.token, c.bearer);
        let adf = weavn::format::mrkdwn_to_adf_mentions(&b2, &m2);
        let key = b.create_issue(&project, &slug, &sum2, &b2, &adf).map(|ci| ci.key)?;
        let status = b.issue_status(&key).unwrap_or_default();
        Ok::<_, String>((key, status))
    })
    .await?;
    weavn_store::message_insert(&s.pool, &ws, &r.channel, &key, None, &author, &body, false, false).await.map_err(db_err)?;
    weavn_store::status_set(&s.pool, &ws, &key, &status).await.map_err(db_err)?;
    notify_for_message(&s, &ws, &author, &me_acc, &r.channel, &key, &body, &r.mentions).await;
    run_workflows(&s, &ws, &r.channel, &key, &body).await;
    s.hub.publish(&ws, event_message(&r.channel));
    Ok(Json(OutcomeResp { kind: "new_issue", issue: key, comment: None }))
}

async fn reply(State(s): State<Shared>, headers: HeaderMap, Json(r): Json<ReplyReq>) -> Result<Json<OutcomeResp>, ApiError> {
    let (creds, author, ws) = jira_session(&s, &headers)?;
    let channel = weavn_store::channel_of_issue(&s.pool, &ws, &r.issue)
        .await
        .map_err(db_err)?
        .ok_or_else(|| bad_request(format!("unknown issue {}", r.issue)))?;
    let me_acc = subject(&s, &headers)?;
    require_channel_access(&s, &ws, &me_acc, &channel).await?;
    let (c, issue2, author2, body2, m2) = (creds.clone(), r.issue.clone(), author.clone(), r.body.clone(), mention_pairs(&r.mentions));
    let comment = blocking(move || {
        let mut b = RealBackend::from_parts(&c.base, c.email.as_deref(), &c.token, c.bearer);
        let adf = weavn::format::mrkdwn_to_adf_mentions(&body2, &m2);
        b.add_comment(&issue2, &author2, &body2, &adf).map(|c| c.id)
    })
    .await?;
    weavn_store::message_insert(&s.pool, &ws, &channel, &r.issue, Some(&r.issue), &author, &r.body, false, r.broadcast).await.map_err(db_err)?;
    notify_for_message(&s, &ws, &author, &me_acc, &channel, &r.issue, &r.body, &r.mentions).await;
    s.hub.publish(&ws, event_message(&channel));
    Ok(Json(OutcomeResp { kind: "comment", issue: r.issue, comment: Some(comment) }))
}

const REDACTED_BODY: &str = "_message deleted_";

/// Edit a conversation: rewrite the issue description. JIRA keeps the full edit
/// history in its changelog, so this is non-destructive by design.
async fn edit(State(s): State<Shared>, headers: HeaderMap, Json(r): Json<EditReq>) -> Result<Json<OutcomeResp>, ApiError> {
    let (creds, _author, ws) = jira_session(&s, &headers)?;
    if !weavn_store::issue_exists(&s.pool, &ws, &r.issue).await.map_err(db_err)? {
        return Err(bad_request(format!("unknown issue {}", r.issue)));
    }
    let body = r.body.trim().to_string();
    if body.is_empty() {
        return Err(bad_request("edited message is empty".to_string()));
    }
    let (c, issue2, b2) = (creds.clone(), r.issue.clone(), body.clone());
    blocking(move || {
        let mut b = RealBackend::from_parts(&c.base, c.email.as_deref(), &c.token, c.bearer);
        let adf = weavn::format::mrkdwn_to_adf(&b2);
        b.set_description(&issue2, &b2, &adf)
    })
    .await?;
    weavn_store::toplevel_set(&s.pool, &ws, &r.issue, &body, false).await.map_err(db_err)?;
    if let Some(ch) = weavn_store::channel_of_issue(&s.pool, &ws, &r.issue).await.map_err(db_err)? {
        s.hub.publish(&ws, event_message(&ch));
    }
    Ok(Json(OutcomeResp { kind: "edited", issue: r.issue, comment: None }))
}

/// Soft-delete a conversation: redact the description (never a hard delete — JIRA
/// stays the system of record and keeps the prior text in its changelog).
async fn delete_conversation(State(s): State<Shared>, headers: HeaderMap, Json(r): Json<DeleteReq>) -> Result<Json<OutcomeResp>, ApiError> {
    let (creds, _author, ws) = jira_session(&s, &headers)?;
    if !weavn_store::issue_exists(&s.pool, &ws, &r.issue).await.map_err(db_err)? {
        return Err(bad_request(format!("unknown issue {}", r.issue)));
    }
    let (c, issue2) = (creds.clone(), r.issue.clone());
    blocking(move || {
        let mut b = RealBackend::from_parts(&c.base, c.email.as_deref(), &c.token, c.bearer);
        let adf = weavn::format::mrkdwn_to_adf(REDACTED_BODY);
        b.set_description(&issue2, REDACTED_BODY, &adf)
    })
    .await?;
    weavn_store::toplevel_set(&s.pool, &ws, &r.issue, REDACTED_BODY, true).await.map_err(db_err)?;
    if let Some(ch) = weavn_store::channel_of_issue(&s.pool, &ws, &r.issue).await.map_err(db_err)? {
        s.hub.publish(&ws, event_message(&ch));
    }
    Ok(Json(OutcomeResp { kind: "deleted", issue: r.issue, comment: None }))
}

/// Pin / unpin a conversation for the whole channel. The projection row is
/// authoritative; a `weavn_pinned` JIRA label mirrors it (best-effort).
async fn pin(State(s): State<Shared>, headers: HeaderMap, Json(r): Json<PinReq>) -> Result<StatusCode, ApiError> {
    let (creds, author, ws) = jira_session(&s, &headers)?;
    require_channel_access(&s, &ws, &subject(&s, &headers)?, &r.channel).await?;
    weavn_store::pin_set(&s.pool, &ws, &r.channel, &r.issue, &author, r.on).await.map_err(db_err)?;
    let (c, issue2, on) = (creds.clone(), r.issue.clone(), r.on);
    let _ = blocking(move || {
        RealBackend::from_parts(&c.base, c.email.as_deref(), &c.token, c.bearer).set_label(&issue2, "weavn_pinned", on)
    })
    .await; // label mirror is cosmetic; the pin stands even if JIRA rejects the label
    s.hub.publish(&ws, event_message(&r.channel));
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct TransitionsQuery {
    issue: String,
}

/// Target workflow states reachable from a conversation-issue's current status.
async fn transitions(State(s): State<Shared>, headers: HeaderMap, Query(q): Query<TransitionsQuery>) -> Result<Json<Vec<String>>, ApiError> {
    let (creds, _author, _ws) = jira_session(&s, &headers)?;
    let issue = q.issue.clone();
    let targets = blocking(move || {
        RealBackend::from_parts(&creds.base, creds.email.as_deref(), &creds.token, creds.bearer).list_transitions(&issue)
    })
    .await?;
    Ok(Json(targets))
}

#[derive(Deserialize)]
struct TransitionReq {
    issue: String,
    to: String,
}

#[derive(Serialize)]
struct StatusResp {
    issue: String,
    status: String,
}

/// Move a conversation-issue to a new workflow state, then project the new status.
async fn transition(State(s): State<Shared>, headers: HeaderMap, Json(r): Json<TransitionReq>) -> Result<Json<StatusResp>, ApiError> {
    let (creds, _author, ws) = jira_session(&s, &headers)?;
    if let Some(ch) = weavn_store::channel_of_issue(&s.pool, &ws, &r.issue).await.map_err(db_err)? {
        require_channel_access(&s, &ws, &subject(&s, &headers)?, &ch).await?;
    }
    let (c, issue2, to2) = (creds.clone(), r.issue.clone(), r.to.clone());
    let status = blocking(move || {
        let b = RealBackend::from_parts(&c.base, c.email.as_deref(), &c.token, c.bearer);
        b.transition_to(&issue2, &to2)?;
        Ok::<_, String>(b.issue_status(&issue2).unwrap_or_else(|_| to2.clone()))
    })
    .await?;
    weavn_store::status_set(&s.pool, &ws, &r.issue, &status).await.map_err(db_err)?;
    if let Some(ch) = weavn_store::channel_of_issue(&s.pool, &ws, &r.issue).await.map_err(db_err)? {
        s.hub.publish(&ws, event_message(&ch));
    }
    Ok(Json(StatusResp { issue: r.issue, status }))
}

async fn refine(State(s): State<Shared>, headers: HeaderMap, Json(r): Json<RefineReq>) -> Result<Json<Vec<String>>, ApiError> {
    let (creds, _author, _ws) = jira_session(&s, &headers)?;
    let update = weavn::slash::parse(&r.commands).map_err(bad_request)?;
    if update.is_empty() {
        return Err(bad_request("no fields to update".to_string()));
    }
    let (c, issue2) = (creds.clone(), r.issue.clone());
    let applied = blocking(move || {
        let mut b = RealBackend::from_parts(&c.base, c.email.as_deref(), &c.token, c.bearer);
        b.update_fields(&issue2, &update)
    })
    .await?;
    Ok(Json(applied))
}

async fn read(State(s): State<Shared>, headers: HeaderMap, Query(q): Query<ReadQuery>) -> Result<Json<Vec<ThreadResp>>, ApiError> {
    let sess = session(&s, &headers)?;
    let me = sess.subject.clone();
    let ws = sess.jira.as_ref().map(|c| c.base.clone()).unwrap_or_else(|| "default".to_string());
    require_channel_access(&s, &ws, &me, &q.channel).await?;
    let rows = weavn_store::read_messages(&s.pool, &ws, &q.channel, q.subtree).await.map_err(db_err)?;
    let pinned: HashSet<String> = weavn_store::pins(&s.pool, &ws, &q.channel, q.subtree).await.map_err(db_err)?.into_iter().collect();

    let mut order: Vec<String> = Vec::new();
    let mut threads: HashMap<String, ThreadResp> = HashMap::new();
    for row in rows {
        let t = threads.entry(row.issue_key.clone()).or_insert_with(|| {
            order.push(row.issue_key.clone());
            ThreadResp {
                pinned: pinned.contains(&row.issue_key),
                issue_key: row.issue_key.clone(),
                author: row.author.clone(),
                ts: row.created_at,
                deleted: false,
                status: String::new(),
                issue_type: String::new(),
                parent: None,
                parent_summary: None,
                lines: Vec::new(),
                replies: Vec::new(),
                reactions: Vec::new(),
                attachments: Vec::new(),
                actions: Vec::new(),
            }
        });
        match row.parent_issue {
            None => {
                t.deleted = row.deleted;
                if !row.status.is_empty() {
                    t.status = row.status; // don't let a coalesced fragment (status '') clobber it
                }
                if !row.issue_type.is_empty() {
                    t.issue_type = row.issue_type;
                }
                if row.parent_key.as_deref().is_some_and(|p| !p.is_empty()) {
                    t.parent = row.parent_key;
                    t.parent_summary = row.parent_summary;
                }
                t.lines.push(row.body);
            }
            Some(_) => t.replies.push(ReplyResp { author: row.author, body: row.body, from_jira: row.from_jira, broadcast: row.broadcast, ts: row.created_at }),
        }
    }
    // Attach aggregated reactions to each conversation.
    for (issue, shortcode, count, mine) in weavn_store::reactions_in_channel(&s.pool, &ws, &q.channel, q.subtree, &me).await.map_err(db_err)? {
        if let Some(t) = threads.get_mut(&issue) {
            t.reactions.push(ReactionResp { shortcode, count, mine });
        }
    }
    // Attach files (native JIRA attachments) to each conversation.
    for (issue, jira_id, filename, mime, size) in weavn_store::attachments_in_channel(&s.pool, &ws, &q.channel, q.subtree).await.map_err(db_err)? {
        if let Some(t) = threads.get_mut(&issue) {
            let is_image = mime.starts_with("image/");
            t.attachments.push(AttachmentResp { url: format!("/attachment/{jira_id}"), jira_id, filename, mime, size, is_image });
        }
    }
    // Interactive action buttons per conversation.
    for (issue, idx, label) in weavn_store::actions_in_channel(&s.pool, &ws, &q.channel, q.subtree).await.map_err(db_err)? {
        if let Some(t) = threads.get_mut(&issue) {
            t.actions.push(ActionResp { idx, label });
        }
    }
    // Pinned conversations surface first; otherwise keep channel (creation) order.
    order.sort_by_key(|k| !threads.get(k).map(|t| t.pinned).unwrap_or(false));
    Ok(Json(order.into_iter().map(|k| threads.remove(&k).unwrap()).collect()))
}

#[derive(Serialize)]
struct ThreadFeedItem {
    channel: String,
    issue_key: String,
    root_author: String,
    snippet: String,
    replies: i64,
    last_author: String,
    last_snippet: String,
    ts: i64,
}

/// First line of `body`, collapsed to one line and capped at `max` chars.
fn one_line(body: &str, max: usize) -> String {
    let flat: String = body.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() > max {
        format!("{}…", flat.chars().take(max).collect::<String>())
    } else {
        flat
    }
}

/// One item in the unified temporal stream.
#[derive(Serialize)]
struct StreamItem {
    channel: String,
    issue_key: String,
    kind: String, // "post" | "issue" | "reply" | "comment"
    author: String,
    snippet: String,
    status: String,
    ts: i64,
}

/// The unified "everything happening" stream: posts + comments across all boards
/// the user can see, newest first. The client filters it down ("for you", by
/// project, by text) — a channel is a saved filter over this stream.
async fn stream_feed(State(s): State<Shared>, headers: HeaderMap) -> Result<Json<Vec<StreamItem>>, ApiError> {
    let (ws, user) = user_ctx(&s, &headers)?;
    let priv_all: HashSet<String> = weavn_store::private_channels(&s.pool, &ws).await.map_err(db_err)?.into_iter().collect();
    let mine: HashSet<String> = weavn_store::my_private_channels(&s.pool, &ws, &user).await.map_err(db_err)?.into_iter().collect();
    let rows = weavn_store::workspace_stream(&s.pool, &ws, 500).await.map_err(db_err)?;
    let items = rows
        .into_iter()
        .filter(|r| !priv_all.contains(&r.channel) || mine.contains(&r.channel))
        .map(|r| {
            let kind = match (r.parent_issue.is_some(), r.from_jira) {
                (true, true) => "comment",
                (true, false) => "reply",
                (false, true) => "issue",
                (false, false) => "post",
            };
            StreamItem {
                channel: r.channel,
                issue_key: r.issue_key,
                kind: kind.to_string(),
                author: r.author,
                snippet: one_line(&r.body, 160),
                status: r.status,
                ts: r.created_at,
            }
        })
        .collect();
    Ok(Json(items))
}

// --- Browse: Projects → Epics → Stories/Tasks (live JIRA drill-down) ----------

#[derive(Deserialize)]
struct ProjQuery {
    project: String,
}
#[derive(Serialize)]
struct EpicItem {
    key: String,
    summary: String,
    status: String,
}
async fn epics(State(s): State<Shared>, headers: HeaderMap, Query(q): Query<ProjQuery>) -> Result<Json<Vec<EpicItem>>, ApiError> {
    let (creds, _author, _ws) = jira_session(&s, &headers)?;
    let proj = q.project.clone();
    let rows = blocking(move || {
        RealBackend::from_parts(&creds.base, creds.email.as_deref(), &creds.token, creds.bearer).list_epics(&proj)
    })
    .await?;
    Ok(Json(rows.into_iter().map(|e| EpicItem { key: e.key, summary: e.summary, status: e.status }).collect()))
}

#[derive(Deserialize)]
struct ParentQuery {
    parent: String,
}
#[derive(Serialize)]
struct ChildItem {
    key: String,
    summary: String,
    issue_type: String,
    status: String,
}
async fn children(State(s): State<Shared>, headers: HeaderMap, Query(q): Query<ParentQuery>) -> Result<Json<Vec<ChildItem>>, ApiError> {
    let (creds, _author, _ws) = jira_session(&s, &headers)?;
    let parent = q.parent.clone();
    let rows = blocking(move || {
        RealBackend::from_parts(&creds.base, creds.email.as_deref(), &creds.token, creds.bearer).list_children(&parent)
    })
    .await?;
    Ok(Json(rows.into_iter().map(|c| ChildItem { key: c.key, summary: c.summary, issue_type: c.issue_type, status: c.status }).collect()))
}

#[derive(Deserialize)]
struct IssueQuery {
    key: String,
}
/// One issue as a conversation (its description + comments), shaped like a thread.
#[derive(Serialize)]
struct IssueConvo {
    issue_key: String,
    author: String,
    ts: i64,
    status: String,
    issue_type: String,
    parent: Option<String>,
    parent_summary: Option<String>,
    lines: Vec<String>,
    replies: Vec<ReplyResp>,
}
async fn issue_detail(State(s): State<Shared>, headers: HeaderMap, Query(q): Query<IssueQuery>) -> Result<Json<IssueConvo>, ApiError> {
    let (creds, _author, _ws) = jira_session(&s, &headers)?;
    let key = q.key.clone();
    let d = blocking(move || {
        RealBackend::from_parts(&creds.base, creds.email.as_deref(), &creds.token, creds.bearer).fetch_issue(&key)
    })
    .await?;
    let ts = parse_jira_ts(&d.created).unwrap_or_else(unix_now);
    let lines = if d.description.is_empty() { vec![d.summary.clone()] } else { vec![format!("{}\n{}", d.summary, d.description)] };
    let replies = d
        .comments
        .into_iter()
        .map(|(author, body, cts)| ReplyResp { author, body, from_jira: true, broadcast: false, ts: parse_jira_ts(&cts).unwrap_or(ts) })
        .collect();
    Ok(Json(IssueConvo {
        issue_key: d.key,
        author: d.author,
        ts,
        status: d.status,
        issue_type: d.issue_type,
        parent: (!d.parent_key.is_empty()).then_some(d.parent_key),
        parent_summary: (!d.parent_summary.is_empty()).then_some(d.parent_summary),
        lines,
        replies,
    }))
}

#[derive(Deserialize)]
struct IssueCommentReq {
    key: String,
    body: String,
}
/// Post a comment directly to a JIRA issue (used by Browse, which reads live from
/// JIRA and so doesn't require the issue to be in a weavn channel/projection).
async fn issue_comment(State(s): State<Shared>, headers: HeaderMap, Json(r): Json<IssueCommentReq>) -> Result<Json<serde_json::Value>, ApiError> {
    let (creds, author, _ws) = jira_session(&s, &headers)?;
    if r.body.trim().is_empty() {
        return Err(bad_request("empty comment".to_string()));
    }
    let (c, key, auth, body) = (creds.clone(), r.key.clone(), author.clone(), r.body.clone());
    let id = blocking(move || {
        let mut b = RealBackend::from_parts(&c.base, c.email.as_deref(), &c.token, c.bearer);
        let adf = weavn::format::mrkdwn_to_adf(&body);
        b.add_comment(&key, &auth, &body, &adf).map(|cm| cm.id)
    })
    .await?;
    Ok(Json(serde_json::json!({ "id": id })))
}

/// A user's personal sidebar layout (custom sections + channel ordering). Opaque
/// JSON owned by the client; `null` until the user customizes it.
async fn sidebar_get(State(s): State<Shared>, headers: HeaderMap) -> Result<Json<serde_json::Value>, ApiError> {
    let (ws, user) = user_ctx(&s, &headers)?;
    let raw = weavn_store::user_pref_get(&s.pool, &ws, &user, "sidebar").await.map_err(db_err)?;
    let v = raw.and_then(|r| serde_json::from_str::<serde_json::Value>(&r).ok()).unwrap_or(serde_json::Value::Null);
    Ok(Json(v))
}

async fn sidebar_set(State(s): State<Shared>, headers: HeaderMap, Json(body): Json<serde_json::Value>) -> Result<StatusCode, ApiError> {
    let (ws, user) = user_ctx(&s, &headers)?;
    let txt = body.to_string();
    if txt.len() > 100_000 {
        return Err((StatusCode::BAD_REQUEST, "sidebar layout too large".to_string()));
    }
    weavn_store::user_pref_set(&s.pool, &ws, &user, "sidebar", &txt).await.map_err(db_err)?;
    Ok(StatusCode::NO_CONTENT)
}

/// Per-workspace overrides mapping a JIRA status NAME to a category
/// ("done" | "active" | "todo"). Stored as one JSON object in `config`; the client
/// falls back to a keyword heuristic for any status with no override. Shared across
/// the workspace so everyone sees the same "In progress" grouping.
async fn status_map_get(State(s): State<Shared>, headers: HeaderMap) -> Result<Json<serde_json::Value>, ApiError> {
    let (ws, _user) = user_ctx(&s, &headers)?;
    let raw = weavn_store::config_get(&s.pool, &ws, "status_categories").await.map_err(db_err)?;
    let v = raw.and_then(|r| serde_json::from_str::<serde_json::Value>(&r).ok()).unwrap_or_else(|| serde_json::json!({}));
    Ok(Json(v))
}

#[derive(Deserialize)]
struct StatusMapReq {
    status: String,
    /// "done" | "active" | "todo" to set, anything else (e.g. "auto") clears it.
    category: String,
}

async fn status_map_set(State(s): State<Shared>, headers: HeaderMap, Json(r): Json<StatusMapReq>) -> Result<Json<serde_json::Value>, ApiError> {
    let (ws, _user) = user_ctx(&s, &headers)?;
    let status = r.status.trim().to_string();
    if status.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "status required".to_string()));
    }
    let cat = r.category.trim().to_lowercase();
    let raw = weavn_store::config_get(&s.pool, &ws, "status_categories").await.map_err(db_err)?;
    let mut map: serde_json::Map<String, serde_json::Value> =
        raw.and_then(|r| serde_json::from_str(&r).ok()).unwrap_or_default();
    // Drop any existing entry that matches case-insensitively, then re-add.
    let key_lc = status.to_lowercase();
    map.retain(|k, _| k.to_lowercase() != key_lc);
    if matches!(cat.as_str(), "done" | "active" | "todo") {
        map.insert(status, serde_json::Value::String(cat));
    } // else: "auto"/unknown → leave removed (revert to the heuristic)
    let out = serde_json::Value::Object(map);
    weavn_store::config_set(&s.pool, &ws, "status_categories", &out.to_string()).await.map_err(db_err)?;
    Ok(Json(out))
}

/// Workspace-wide "Threads" feed: every conversation that has replies, across all
/// boards the user can see, newest activity first (the cross-board inbox).
async fn threads_feed(State(s): State<Shared>, headers: HeaderMap) -> Result<Json<Vec<ThreadFeedItem>>, ApiError> {
    let (ws, user) = user_ctx(&s, &headers)?;
    let priv_all: HashSet<String> = weavn_store::private_channels(&s.pool, &ws).await.map_err(db_err)?.into_iter().collect();
    let mine: HashSet<String> = weavn_store::my_private_channels(&s.pool, &ws, &user).await.map_err(db_err)?.into_iter().collect();
    let rows = weavn_store::workspace_threads(&s.pool, &ws, 100).await.map_err(db_err)?;
    let items = rows
        .into_iter()
        .filter(|t| !priv_all.contains(&t.channel) || mine.contains(&t.channel))
        .map(|t| ThreadFeedItem {
            channel: t.channel,
            issue_key: t.issue_key,
            root_author: t.root_author,
            snippet: one_line(&t.root_body, 140),
            replies: t.replies,
            last_author: t.last_author,
            last_snippet: one_line(&t.last_body, 100),
            ts: t.last_at,
        })
        .collect();
    Ok(Json(items))
}

#[derive(Deserialize)]
struct ExportQuery {
    channel: String,
    #[serde(default)]
    subtree: bool,
    #[serde(default)]
    format: Option<String>,
}

/// Export a channel transcript as Markdown (default) or JSON. Served as a
/// download; the record mirrors what /read returns (status, replies, reactions,
/// attachments), ordered pinned-first then by creation.
async fn export(State(s): State<Shared>, headers: HeaderMap, Query(q): Query<ExportQuery>) -> Result<Response, ApiError> {
    let site = session(&s, &headers)?.site.unwrap_or_default().trim_end_matches('/').to_string();
    let threads = read(State(s.clone()), headers, Query(ReadQuery { channel: q.channel.clone(), subtree: q.subtree })).await?.0;
    let json = q.format.as_deref() == Some("json");
    let fname = format!("{}.{}", slugify_file(&q.channel), if json { "json" } else { "md" });

    let (ctype, body) = if json {
        ("application/json; charset=utf-8", serde_json::to_string_pretty(&threads).unwrap_or_default())
    } else {
        ("text/markdown; charset=utf-8", export_markdown(&q.channel, &threads, &site))
    };
    let mut resp = Response::new(axum::body::Body::from(body));
    resp.headers_mut().insert(header::CONTENT_TYPE, ctype.parse().unwrap());
    if let Ok(v) = format!("attachment; filename=\"{fname}\"").parse() {
        resp.headers_mut().insert(header::CONTENT_DISPOSITION, v);
    }
    Ok(resp)
}

fn slugify_file(ch: &str) -> String {
    let mut out: String = ch.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
    while out.contains("--") {
        out = out.replace("--", "-");
    }
    out.trim_matches('-').to_lowercase()
}

fn export_markdown(channel: &str, threads: &[ThreadResp], site: &str) -> String {
    let mut out = String::new();
    out.push_str(&format!("# {channel}\n\n_Exported from weaveN — {} conversation(s)_\n", threads.len()));
    for t in threads {
        let perma = if site.is_empty() { t.issue_key.clone() } else { format!("{site}/browse/{}", t.issue_key) };
        let status = if t.status.is_empty() { String::new() } else { format!(" · {}", t.status) };
        let pin = if t.pinned { " 📌" } else { "" };
        out.push_str(&format!("\n## {}{} — {}{}\n", t.author, pin, t.issue_key, status));
        out.push_str(&format!("_{}_\n\n", perma));
        if t.deleted {
            out.push_str("_(message deleted)_\n");
        } else {
            for line in &t.lines {
                out.push_str(line);
                out.push('\n');
            }
        }
        if !t.reactions.is_empty() {
            let r: Vec<String> = t.reactions.iter().map(|x| format!("{} {}", x.shortcode, x.count)).collect();
            out.push_str(&format!("\nReactions: {}\n", r.join("  ")));
        }
        for a in &t.attachments {
            out.push_str(&format!("📎 {} ({} bytes)\n", a.filename, a.size));
        }
        for rep in &t.replies {
            let tag = if rep.broadcast { " [↗ channel]" } else { "" };
            out.push_str(&format!("- **{}**{}: {}\n", rep.author, tag, rep.body));
        }
    }
    out
}

// --- discovery + backfill ---------------------------------------------------

#[derive(Serialize)]
struct ProjectResp {
    key: String,
    name: String,
}

async fn discover(State(s): State<Shared>, headers: HeaderMap) -> Result<Json<Vec<ProjectResp>>, ApiError> {
    let (creds, _author, _ws) = jira_session(&s, &headers)?;
    let projects = blocking(move || {
        RealBackend::from_parts(&creds.base, creds.email.as_deref(), &creds.token, creds.bearer).list_projects()
    })
    .await?;
    Ok(Json(projects.into_iter().map(|p| ProjectResp { key: p.key, name: p.name }).collect()))
}

#[derive(Deserialize)]
struct UsersQuery {
    q: String,
}

#[derive(Serialize)]
struct UserHit {
    id: String,
    display: String,
}

/// Mention autocomplete: search JIRA users by name/email.
async fn users(State(s): State<Shared>, headers: HeaderMap, Query(q): Query<UsersQuery>) -> Result<Json<Vec<UserHit>>, ApiError> {
    let (creds, _author, _ws) = jira_session(&s, &headers)?;
    let query = q.q.trim().to_string();
    if query.is_empty() {
        return Ok(Json(Vec::new()));
    }
    let found = blocking(move || {
        RealBackend::from_parts(&creds.base, creds.email.as_deref(), &creds.token, creds.bearer).search_users(&query, 8)
    })
    .await?;
    Ok(Json(found.into_iter().map(|(id, display)| UserHit { id, display }).collect()))
}

const MAX_UPLOAD: usize = 25 * 1024 * 1024; // 25 MB

/// Upload a file: it becomes a new conversation-issue with the file attached as a
/// native JIRA attachment. Multipart fields: `channel`, optional `caption`, `file`.
async fn upload(State(s): State<Shared>, headers: HeaderMap, mut mp: Multipart) -> Result<Json<OutcomeResp>, ApiError> {
    let (creds, author, ws) = jira_session(&s, &headers)?;
    let (mut channel, mut caption, mut filename, mut mime) = (String::new(), String::new(), String::new(), String::new());
    let mut data: Option<Vec<u8>> = None;
    while let Some(field) = mp.next_field().await.map_err(|e| bad_request(e.to_string()))? {
        match field.name().unwrap_or("") {
            "channel" => channel = field.text().await.map_err(|e| bad_request(e.to_string()))?,
            "caption" => caption = field.text().await.map_err(|e| bad_request(e.to_string()))?,
            "file" => {
                filename = field.file_name().unwrap_or("file").to_string();
                mime = field.content_type().map(|m| m.to_string()).unwrap_or_default();
                data = Some(field.bytes().await.map_err(|e| bad_request(e.to_string()))?.to_vec());
            }
            _ => {}
        }
    }
    let channel = channel.trim().to_string();
    if channel.is_empty() {
        return Err(bad_request("channel is required".to_string()));
    }
    require_channel_access(&s, &ws, &subject(&s, &headers)?, &channel).await?;
    let bytes = data.ok_or_else(|| bad_request("no file field".to_string()))?;
    if bytes.is_empty() {
        return Err(bad_request("empty file".to_string()));
    }
    if bytes.len() > MAX_UPLOAD {
        return Err(bad_request("file too large (max 25 MB)".to_string()));
    }
    let filename = sanitize_filename(&filename);
    let body = if caption.trim().is_empty() { format!("📎 {filename}") } else { caption.trim().to_string() };
    let summary = weavn::proto::summarize(&body, 80);
    let (project, slug) = resolve_target(&s, Some(&creds), &ws, &channel).await?;
    let (c, b2, sum2, fname, m2) = (creds.clone(), body.clone(), summary.clone(), filename.clone(), mime.clone());
    let (key, jid, fname2, mime2, size, status) = blocking(move || {
        let mut b = RealBackend::from_parts(&c.base, c.email.as_deref(), &c.token, c.bearer);
        let adf = weavn::format::mrkdwn_to_adf(&b2);
        let key = b.create_issue(&project, &slug, &sum2, &b2, &adf).map(|ci| ci.key)?;
        let (jid, fn2, mm, sz) = b.add_attachment(&key, &fname, bytes, &m2)?;
        let status = b.issue_status(&key).unwrap_or_default();
        Ok::<_, String>((key, jid, fn2, mm, sz, status))
    })
    .await?;
    weavn_store::message_insert(&s.pool, &ws, &channel, &key, None, &author, &body, false, false).await.map_err(db_err)?;
    weavn_store::attachment_insert(&s.pool, &ws, &key, &jid, &fname2, &mime2, size, &author).await.map_err(db_err)?;
    weavn_store::status_set(&s.pool, &ws, &key, &status).await.map_err(db_err)?;
    s.hub.publish(&ws, event_message(&channel));
    Ok(Json(OutcomeResp { kind: "file", issue: key, comment: None }))
}

/// Authenticated download proxy: streams a JIRA attachment back to the browser
/// using the session's JIRA token (the raw JIRA URL is not browser-reachable).
async fn attachment(State(s): State<Shared>, headers: HeaderMap, Path(jid): Path<String>) -> Result<Response, ApiError> {
    let (creds, _author, ws) = jira_session(&s, &headers)?;
    let (filename, _m) = weavn_store::attachment_meta(&s.pool, &ws, &jid)
        .await
        .map_err(db_err)?
        .ok_or((StatusCode::NOT_FOUND, "no such attachment".to_string()))?;
    let jid2 = jid.clone();
    let (bytes, mime) = blocking(move || {
        RealBackend::from_parts(&creds.base, creds.email.as_deref(), &creds.token, creds.bearer).fetch_attachment(&jid2)
    })
    .await?;
    let mut resp = Response::new(axum::body::Body::from(bytes));
    if let Ok(v) = mime.parse() {
        resp.headers_mut().insert(header::CONTENT_TYPE, v);
    }
    let disp = if mime.starts_with("image/") { "inline" } else { "attachment" };
    if let Ok(v) = format!("{disp}; filename=\"{}\"", sanitize_filename(&filename)).parse() {
        resp.headers_mut().insert(header::CONTENT_DISPOSITION, v);
    }
    Ok(resp)
}

/// Strip quotes, path separators, and control chars so a filename is safe in a
/// Content-Disposition header and as a display label.
fn sanitize_filename(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let cleaned: String = base.chars().filter(|c| !c.is_control() && *c != '"').collect();
    let cleaned = cleaned.trim();
    if cleaned.is_empty() { "file".to_string() } else { cleaned.to_string() }
}

#[derive(Deserialize)]
struct SeedChannel {
    channel: String,
    #[serde(default)]
    body: String,
}

#[derive(Deserialize)]
struct SeedReq {
    channels: Vec<SeedChannel>,
}

#[derive(Serialize)]
struct SeedResp {
    created: Vec<String>,
    skipped: Vec<String>,
}

/// First-run onboarding: create a starter set of channels by posting a welcome
/// message (one conversation-issue) to each. Idempotent — channels that already
/// hold messages are skipped, so it never double-seeds.
async fn seed(State(s): State<Shared>, headers: HeaderMap, Json(r): Json<SeedReq>) -> Result<Json<SeedResp>, ApiError> {
    let (creds, author, ws) = jira_session(&s, &headers)?;
    let existing: HashSet<String> = weavn_store::channels_list(&s.pool, &ws)
        .await
        .map_err(db_err)?
        .into_iter()
        .map(|(c, _)| c)
        .collect();
    let mut created = Vec::new();
    let mut skipped = Vec::new();
    for sc in r.channels {
        let channel = sc.channel.trim().to_string();
        if channel.is_empty() {
            continue;
        }
        if existing.contains(&channel) {
            skipped.push(channel);
            continue;
        }
        let body = if sc.body.trim().is_empty() { format!("Welcome to {channel}.") } else { sc.body.trim().to_string() };
        // A starter channel we can't map to a project is skipped, not fatal.
        let Ok((project, slug)) = resolve_target(&s, Some(&creds), &ws, &channel).await else {
            skipped.push(channel);
            continue;
        };
        let summary = weavn::proto::summarize(&body, 80);
        let (c, b2, sum2) = (creds.clone(), body.clone(), summary.clone());
        let (key, status) = blocking(move || {
            let mut b = RealBackend::from_parts(&c.base, c.email.as_deref(), &c.token, c.bearer);
            let adf = weavn::format::mrkdwn_to_adf(&b2);
            let key = b.create_issue(&project, &slug, &sum2, &b2, &adf).map(|ci| ci.key)?;
            let status = b.issue_status(&key).unwrap_or_default();
            Ok::<_, String>((key, status))
        })
        .await?;
        weavn_store::message_insert(&s.pool, &ws, &channel, &key, None, &author, &body, false, false).await.map_err(db_err)?;
        weavn_store::status_set(&s.pool, &ws, &key, &status).await.map_err(db_err)?;
        created.push(channel);
    }
    Ok(Json(SeedResp { created, skipped }))
}

#[derive(Deserialize)]
struct BackfillReq {
    project: String,
}

/// Pull a project's recent issues (+ comments) from JIRA into the projection as a
/// channel named after the project key. Idempotent per issue.
/// Current Unix time in seconds.
fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Days since the Unix epoch for a civil (proleptic Gregorian) date — Howard
/// Hinnant's `days_from_civil`.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// Parse a JIRA ISO-8601 timestamp (e.g. "2025-03-14T10:22:33.123+0000", also
/// "…+00:00" or "…Z") to Unix seconds (UTC). `None` if it isn't one.
fn parse_jira_ts(s: &str) -> Option<i64> {
    let s = s.trim();
    let (date, rest) = s.split_once('T')?;
    let mut dp = date.split('-');
    let y: i64 = dp.next()?.parse().ok()?;
    let mo: i64 = dp.next()?.parse().ok()?;
    let d: i64 = dp.next()?.parse().ok()?;
    // Split the trailing timezone (Z, or ±HHMM / ±HH:MM) off the time.
    let (time, off_secs): (&str, i64) = if let Some(t) = rest.strip_suffix(['Z', 'z']) {
        (t, 0)
    } else {
        let bytes = rest.as_bytes();
        let mut idx = None;
        for i in (1..bytes.len()).rev() {
            if bytes[i] == b'+' || bytes[i] == b'-' {
                idx = Some(i);
                break;
            }
        }
        match idx {
            Some(i) => {
                let (t, off) = rest.split_at(i);
                let sign = if off.starts_with('-') { -1 } else { 1 };
                let digits: String = off[1..].chars().filter(|c| c.is_ascii_digit()).collect();
                if digits.len() < 4 {
                    (t, 0)
                } else {
                    let oh: i64 = digits[0..2].parse().ok()?;
                    let om: i64 = digits[2..4].parse().ok()?;
                    (t, sign * (oh * 3600 + om * 60))
                }
            }
            None => (rest, 0),
        }
    };
    let time = time.split('.').next().unwrap_or(time);
    let mut tp = time.split(':');
    let hh: i64 = tp.next()?.parse().ok()?;
    let mm: i64 = tp.next()?.parse().ok()?;
    let ss: i64 = tp.next().unwrap_or("0").parse().ok()?;
    let local = days_from_civil(y, mo, d) * 86400 + hh * 3600 + mm * 60 + ss;
    Some(local - off_secs)
}

async fn backfill(State(s): State<Shared>, headers: HeaderMap, Json(r): Json<BackfillReq>) -> Result<Json<serde_json::Value>, ApiError> {
    let (creds, _author, ws) = jira_session(&s, &headers)?;
    let project = r.project.clone();
    let issues = blocking(move || {
        RealBackend::from_parts(&creds.base, creds.email.as_deref(), &creds.token, creds.bearer).backfill_issues(&project, 200)
    })
    .await?;

    let channel = r.project.clone();
    let mut added = 0usize;
    let mut redated = 0usize;
    for iss in issues {
        let p_key = (!iss.parent_key.is_empty()).then_some(iss.parent_key.as_str());
        let p_sum = (!iss.parent_summary.is_empty()).then_some(iss.parent_summary.as_str());
        if weavn_store::issue_exists(&s.pool, &ws, &iss.key).await.map_err(db_err)? {
            // Already cached — heal its date (old now()-stamp bug) and refresh its
            // type/epic metadata. All idempotent; JIRA `created` is immutable.
            if let Some(ts) = parse_jira_ts(&iss.created) {
                weavn_store::issue_set_created(&s.pool, &ws, &iss.key, ts).await.map_err(db_err)?;
                redated += 1;
            }
            weavn_store::issue_set_meta(&s.pool, &ws, &iss.key, &iss.issue_type, p_key, p_sum).await.map_err(db_err)?;
            continue;
        }
        let body = if iss.description.is_empty() {
            iss.summary.clone()
        } else {
            format!("{}\n{}", iss.summary, iss.description)
        };
        // Keep the issue's real JIRA date, not the moment we cached it — otherwise
        // every backfilled ticket looks brand-new and sorts to the top of the river.
        let root_ts = parse_jira_ts(&iss.created).unwrap_or_else(unix_now);
        weavn_store::message_insert_at(&s.pool, &ws, &channel, &iss.key, None, &iss.reporter, &body, true, false, root_ts).await.map_err(db_err)?;
        weavn_store::status_set(&s.pool, &ws, &iss.key, &iss.status).await.map_err(db_err)?;
        weavn_store::issue_set_meta(&s.pool, &ws, &iss.key, &iss.issue_type, p_key, p_sum).await.map_err(db_err)?;
        for (author, cbody, cts) in iss.comments {
            let ts = parse_jira_ts(&cts).unwrap_or(root_ts);
            weavn_store::message_insert_at(&s.pool, &ws, &channel, &iss.key, Some(&iss.key), &author, &cbody, true, false, ts).await.map_err(db_err)?;
        }
        added += 1;
    }
    Ok(Json(serde_json::json!({ "channel": channel, "added": added, "redated": redated })))
}

// --- Tier 2: per-user prefs (stars, saved, status, DND, archive) ------------

/// (workspace, user_id) from the session; user_id is the stable JIRA accountId.
fn user_ctx(s: &AppState, headers: &HeaderMap) -> Result<(String, String), ApiError> {
    let sess = session(s, headers)?;
    let ws = sess.site.clone().unwrap_or_else(|| "default".to_string());
    Ok((ws, sess.subject))
}

#[derive(Serialize)]
struct ChannelInfo {
    channel: String,
    count: i64,
    unread: i64,
    starred: bool,
    archived: bool,
    private: bool,
}

async fn channels(State(s): State<Shared>, headers: HeaderMap) -> Result<Json<Vec<ChannelInfo>>, ApiError> {
    let (ws, user) = user_ctx(&s, &headers)?;
    let chans = weavn_store::channels_list(&s.pool, &ws).await.map_err(db_err)?;
    let st: HashSet<String> = weavn_store::stars(&s.pool, &ws, &user).await.map_err(db_err)?.into_iter().collect();
    let ar: HashSet<String> = weavn_store::archived(&s.pool, &ws, &user).await.map_err(db_err)?.into_iter().collect();
    let unread: HashMap<String, i64> = weavn_store::unread_counts(&s.pool, &ws, &user).await.map_err(db_err)?.into_iter().collect();
    // Private channels the user may NOT see (private minus their memberships).
    let priv_all: HashSet<String> = weavn_store::private_channels(&s.pool, &ws).await.map_err(db_err)?.into_iter().collect();
    let mine: HashSet<String> = weavn_store::my_private_channels(&s.pool, &ws, &user).await.map_err(db_err)?.into_iter().collect();
    Ok(Json(
        chans
            .into_iter()
            .filter(|(c, _)| !priv_all.contains(c) || mine.contains(c))
            .map(|(c, n)| ChannelInfo {
                starred: st.contains(&c),
                archived: ar.contains(&c),
                unread: unread.get(&c).copied().unwrap_or(0),
                private: priv_all.contains(&c),
                count: n,
                channel: c,
            })
            .collect(),
    ))
}

// --- private channels + membership -----------------------------------------

#[derive(Serialize)]
struct MemberResp {
    account: String,
    display: String,
}

#[derive(Serialize)]
struct AclResp {
    channel: String,
    private: bool,
    is_member: bool,
    created_by: String,
    members: Vec<MemberResp>,
}

#[derive(Deserialize)]
struct AclQuery {
    channel: String,
}

/// Current privacy + membership for a channel. Members of a private channel can
/// see the roster; non-members get 403.
async fn channel_acl_get(State(s): State<Shared>, headers: HeaderMap, Query(q): Query<AclQuery>) -> Result<Json<AclResp>, ApiError> {
    let (ws, user) = user_ctx(&s, &headers)?;
    let acl = weavn_store::channel_acl(&s.pool, &ws, &q.channel).await.map_err(db_err)?;
    let private = acl.as_ref().map(|(p, _)| *p).unwrap_or(false);
    let created_by = acl.map(|(_, b)| b).unwrap_or_default();
    let is_member = !private || weavn_store::channel_is_member(&s.pool, &ws, &q.channel, &user).await.map_err(db_err)?;
    if private && !is_member {
        return Err((StatusCode::FORBIDDEN, "private channel — you are not a member".to_string()));
    }
    let members = if private {
        weavn_store::channel_members(&s.pool, &ws, &q.channel)
            .await
            .map_err(db_err)?
            .into_iter()
            .map(|(account, display)| MemberResp { account, display })
            .collect()
    } else {
        Vec::new()
    };
    Ok(Json(AclResp { channel: q.channel, private, is_member, created_by, members }))
}

#[derive(Deserialize)]
struct PrivateReq {
    channel: String,
    private: bool,
}

/// Make a channel private (adding the caller as the first member) or public
/// again. Only a current member may change an already-private channel.
async fn channel_set_private(State(s): State<Shared>, headers: HeaderMap, Json(r): Json<PrivateReq>) -> Result<Json<AclResp>, ApiError> {
    let sess = session(&s, &headers)?;
    let ws = sess.site.clone().unwrap_or_else(|| "default".to_string());
    let user = sess.subject.clone();
    let display = sess.author();
    if let Some((true, _)) = weavn_store::channel_acl(&s.pool, &ws, &r.channel).await.map_err(db_err)? {
        if !weavn_store::channel_is_member(&s.pool, &ws, &r.channel, &user).await.map_err(db_err)? {
            return Err((StatusCode::FORBIDDEN, "only a member can change this channel".to_string()));
        }
    }
    weavn_store::channel_set_private(&s.pool, &ws, &r.channel, r.private, &user).await.map_err(db_err)?;
    if r.private {
        weavn_store::channel_member_add(&s.pool, &ws, &r.channel, &user, &display).await.map_err(db_err)?;
    }
    channel_acl_get(State(s), headers, Query(AclQuery { channel: r.channel })).await
}

#[derive(Deserialize)]
struct MemberReq {
    channel: String,
    account: String,
    #[serde(default)]
    display: String,
    on: bool,
}

/// Add or remove a member of a private channel (members-only).
async fn channel_member(State(s): State<Shared>, headers: HeaderMap, Json(r): Json<MemberReq>) -> Result<Json<AclResp>, ApiError> {
    let (ws, user) = user_ctx(&s, &headers)?;
    match weavn_store::channel_acl(&s.pool, &ws, &r.channel).await.map_err(db_err)? {
        Some((true, _)) => {
            if !weavn_store::channel_is_member(&s.pool, &ws, &r.channel, &user).await.map_err(db_err)? {
                return Err((StatusCode::FORBIDDEN, "only a member can manage this channel".to_string()));
            }
        }
        _ => return Err(bad_request("channel is not private — make it private first".to_string())),
    }
    if r.on {
        weavn_store::channel_member_add(&s.pool, &ws, &r.channel, &r.account, &r.display).await.map_err(db_err)?;
    } else {
        let members = weavn_store::channel_members(&s.pool, &ws, &r.channel).await.map_err(db_err)?;
        if members.len() <= 1 {
            return Err(bad_request("cannot remove the last member — make the channel public instead".to_string()));
        }
        weavn_store::channel_member_remove(&s.pool, &ws, &r.channel, &r.account).await.map_err(db_err)?;
    }
    channel_acl_get(State(s), headers, Query(AclQuery { channel: r.channel })).await
}

#[derive(Deserialize)]
struct StarReq {
    channel: String,
    on: bool,
}

async fn star(State(s): State<Shared>, headers: HeaderMap, Json(r): Json<StarReq>) -> Result<StatusCode, ApiError> {
    let (ws, user) = user_ctx(&s, &headers)?;
    weavn_store::star_set(&s.pool, &ws, &user, &r.channel, r.on).await.map_err(db_err)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn archive(State(s): State<Shared>, headers: HeaderMap, Json(r): Json<StarReq>) -> Result<StatusCode, ApiError> {
    let (ws, user) = user_ctx(&s, &headers)?;
    weavn_store::archive_set(&s.pool, &ws, &user, &r.channel, r.on).await.map_err(db_err)?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct SaveReq {
    issue: String,
    on: bool,
}

async fn save(State(s): State<Shared>, headers: HeaderMap, Json(r): Json<SaveReq>) -> Result<StatusCode, ApiError> {
    let (ws, user) = user_ctx(&s, &headers)?;
    weavn_store::saved_set(&s.pool, &ws, &user, &r.issue, r.on).await.map_err(db_err)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn saved(State(s): State<Shared>, headers: HeaderMap) -> Result<Json<Vec<String>>, ApiError> {
    let (ws, user) = user_ctx(&s, &headers)?;
    Ok(Json(weavn_store::saved_list(&s.pool, &ws, &user).await.map_err(db_err)?))
}

#[derive(Serialize, Deserialize, Default)]
struct StateBody {
    #[serde(default)]
    emoji: Option<String>,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    dnd: bool,
}

async fn get_state(State(s): State<Shared>, headers: HeaderMap) -> Result<Json<StateBody>, ApiError> {
    let (ws, user) = user_ctx(&s, &headers)?;
    let b = weavn_store::state_get(&s.pool, &ws, &user)
        .await
        .map_err(db_err)?
        .map(|(e, t, d)| StateBody { emoji: e, text: t, dnd: d })
        .unwrap_or_default();
    Ok(Json(b))
}

async fn set_state(State(s): State<Shared>, headers: HeaderMap, Json(r): Json<StateBody>) -> Result<StatusCode, ApiError> {
    let (ws, user) = user_ctx(&s, &headers)?;
    weavn_store::state_set(&s.pool, &ws, &user, r.emoji.as_deref(), r.text.as_deref(), r.dnd).await.map_err(db_err)?;
    Ok(StatusCode::NO_CONTENT)
}

// --- Tier 3: availability (time clock) --------------------------------------

#[derive(Deserialize)]
struct AvailReq {
    on: bool,
}

#[derive(Serialize)]
struct AvailEntry {
    account: String,
    display: String,
    since: i64,
}

#[derive(Serialize)]
struct AvailResp {
    on: bool,
    people: Vec<AvailEntry>,
}

async fn availability_get(State(s): State<Shared>, headers: HeaderMap) -> Result<Json<AvailResp>, ApiError> {
    let (ws, user) = user_ctx(&s, &headers)?;
    let people: Vec<AvailEntry> = weavn_store::availability_list(&s.pool, &ws)
        .await
        .map_err(db_err)?
        .into_iter()
        .map(|(account, display, since)| AvailEntry { account, display, since })
        .collect();
    let on = people.iter().any(|p| p.account == user);
    Ok(Json(AvailResp { on, people }))
}

async fn availability_set(State(s): State<Shared>, headers: HeaderMap, Json(r): Json<AvailReq>) -> Result<Json<AvailResp>, ApiError> {
    let sess = session(&s, &headers)?;
    let ws = sess.site.clone().unwrap_or_else(|| "default".to_string());
    let user = sess.subject.clone();
    let display = sess.author();
    weavn_store::availability_set(&s.pool, &ws, &user, &display, r.on).await.map_err(db_err)?;
    s.hub.publish(&ws, serde_json::json!({ "type": "availability" }).to_string());
    availability_get(State(s), headers).await
}

// --- Tier 3: notifications + keywords ---------------------------------------

#[derive(Serialize)]
struct NotifItem {
    id: i64,
    kind: String,
    channel: String,
    issue_key: String,
    actor: String,
    snippet: String,
    read: bool,
    ts: i64,
}

#[derive(Serialize)]
struct NotifResp {
    unread: i64,
    items: Vec<NotifItem>,
}

async fn notifications_get(State(s): State<Shared>, headers: HeaderMap) -> Result<Json<NotifResp>, ApiError> {
    let (ws, user) = user_ctx(&s, &headers)?;
    let unread = weavn_store::notifications_unread(&s.pool, &ws, &user).await.map_err(db_err)?;
    let items = weavn_store::notifications_list(&s.pool, &ws, &user, 50)
        .await
        .map_err(db_err)?
        .into_iter()
        .map(|(id, kind, channel, issue_key, actor, snippet, read, ts)| NotifItem { id, kind, channel, issue_key, actor, snippet, read, ts })
        .collect();
    Ok(Json(NotifResp { unread, items }))
}

#[derive(Deserialize)]
struct NotifReadReq {
    #[serde(default)]
    id: Option<i64>,
}

async fn notifications_read(State(s): State<Shared>, headers: HeaderMap, Json(r): Json<NotifReadReq>) -> Result<StatusCode, ApiError> {
    let (ws, user) = user_ctx(&s, &headers)?;
    weavn_store::notifications_mark_read(&s.pool, &ws, &user, r.id).await.map_err(db_err)?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct KeywordReq {
    keyword: String,
    on: bool,
}

async fn keywords_get(State(s): State<Shared>, headers: HeaderMap) -> Result<Json<Vec<String>>, ApiError> {
    let (ws, user) = user_ctx(&s, &headers)?;
    Ok(Json(weavn_store::keywords_list(&s.pool, &ws, &user).await.map_err(db_err)?))
}

async fn keywords_set(State(s): State<Shared>, headers: HeaderMap, Json(r): Json<KeywordReq>) -> Result<Json<Vec<String>>, ApiError> {
    let (ws, user) = user_ctx(&s, &headers)?;
    let kw = r.keyword.trim().to_lowercase();
    if kw.is_empty() {
        return Err(bad_request("empty keyword".to_string()));
    }
    weavn_store::keyword_set(&s.pool, &ws, &user, &kw, r.on).await.map_err(db_err)?;
    keywords_get(State(s), headers).await
}

// --- Tier 3: scheduled messages + reminders ---------------------------------

fn now_secs() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

#[derive(Deserialize)]
struct ScheduleReq {
    #[serde(default)]
    channel: String,
    #[serde(default)]
    body: String,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    at: Option<i64>,
    #[serde(default)]
    in_seconds: Option<i64>,
}

async fn schedule_create(State(s): State<Shared>, headers: HeaderMap, Json(r): Json<ScheduleReq>) -> Result<Json<serde_json::Value>, ApiError> {
    let sess = session(&s, &headers)?;
    let ws = sess.site.clone().unwrap_or_else(|| "default".to_string());
    let user = sess.subject.clone();
    let display = sess.author();
    let kind = match r.kind.as_deref().unwrap_or("message") {
        "reminder" => "reminder",
        "message" => "message",
        _ => return Err(bad_request("kind must be 'message' or 'reminder'".to_string())),
    };
    let body = r.body.trim().to_string();
    if body.is_empty() {
        return Err(bad_request("nothing to schedule".to_string()));
    }
    let due = r.at.unwrap_or_else(|| now_secs() + r.in_seconds.unwrap_or(60).max(5));
    if kind == "message" {
        if r.channel.trim().is_empty() {
            return Err(bad_request("a channel is required for a scheduled message".to_string()));
        }
        require_channel_access(&s, &ws, &user, r.channel.trim()).await?;
    }
    let id = weavn_store::scheduled_insert(&s.pool, &ws, &user, &display, r.channel.trim(), &body, kind, due)
        .await
        .map_err(db_err)?;
    Ok(Json(serde_json::json!({ "id": id, "due": due, "kind": kind })))
}

#[derive(Serialize)]
struct SchedItem {
    id: i64,
    channel: String,
    body: String,
    kind: String,
    due: i64,
}

async fn scheduled_get(State(s): State<Shared>, headers: HeaderMap) -> Result<Json<Vec<SchedItem>>, ApiError> {
    let (ws, user) = user_ctx(&s, &headers)?;
    let items = weavn_store::scheduled_list(&s.pool, &ws, &user)
        .await
        .map_err(db_err)?
        .into_iter()
        .map(|(id, channel, body, kind, due)| SchedItem { id, channel, body, kind, due })
        .collect();
    Ok(Json(items))
}

#[derive(Deserialize)]
struct CancelReq {
    id: i64,
}

async fn schedule_cancel(State(s): State<Shared>, headers: HeaderMap, Json(r): Json<CancelReq>) -> Result<StatusCode, ApiError> {
    let (ws, user) = user_ctx(&s, &headers)?;
    weavn_store::scheduled_cancel(&s.pool, &ws, &user, r.id).await.map_err(db_err)?;
    Ok(StatusCode::NO_CONTENT)
}

// --- Tier 3: link unfurling -------------------------------------------------

#[derive(Serialize)]
struct Unfurl {
    url: String,
    title: String,
    description: String,
    host: String,
}

#[derive(Deserialize)]
struct UnfurlQuery {
    url: String,
}

/// (host, port) from an http(s) URL; None for any other scheme.
fn parse_http(url: &str) -> Option<(String, u16)> {
    let (scheme, rest) = url.split_once("://")?;
    let https = match scheme.to_ascii_lowercase().as_str() {
        "https" => true,
        "http" => false,
        _ => return None,
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let authority = authority.rsplit('@').next().unwrap_or(authority); // strip userinfo
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) if !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()) => (h.to_string(), p.parse().unwrap_or(if https { 443 } else { 80 })),
        _ => (authority.to_string(), if https { 443 } else { 80 }),
    };
    if host.is_empty() {
        None
    } else {
        Some((host, port))
    }
}

/// Reject SSRF targets: loopback, private, link-local, CGNAT, ULA, etc.
fn is_blocked_ip(ip: std::net::IpAddr) -> bool {
    use std::net::IpAddr;
    match ip {
        IpAddr::V4(v) => {
            let o = v.octets();
            v.is_private() || v.is_loopback() || v.is_link_local() || v.is_unspecified() || v.is_broadcast() || o[0] == 0 || (o[0] == 100 && (o[1] & 0xc0) == 64)
        }
        IpAddr::V6(v) => {
            let s = v.segments();
            v.is_loopback() || v.is_unspecified() || (s[0] & 0xfe00) == 0xfc00 || (s[0] & 0xffc0) == 0xfe80
        }
    }
}

fn unfurl_attr(tag: &str, name: &str) -> Option<String> {
    for q in ['"', '\''] {
        let needle = format!("{name}={q}");
        if let Some(p) = tag.find(&needle) {
            let rest = &tag[p + needle.len()..];
            if let Some(e) = rest.find(q) {
                return Some(rest[..e].to_string());
            }
        }
    }
    None
}

fn unfurl_meta(html: &str, key: &str) -> Option<String> {
    for attr in ["property", "name"] {
        for q in ['"', '\''] {
            let needle = format!("{attr}={q}{key}{q}");
            if let Some(pos) = html.find(&needle) {
                let start = html[..pos].rfind('<').unwrap_or(pos);
                let end = html[pos..].find('>').map(|e| pos + e).unwrap_or(html.len());
                if let Some(c) = unfurl_attr(&html[start..end], "content") {
                    return Some(unfurl_decode(&c));
                }
            }
        }
    }
    None
}

fn unfurl_title(html: &str) -> Option<String> {
    let s = html.find("<title").or_else(|| html.find("<TITLE"))?;
    let gt = html[s..].find('>')? + s + 1;
    let e = html[gt..].to_ascii_lowercase().find("</title>")? + gt;
    Some(unfurl_decode(html[gt..e].trim()))
}

fn unfurl_decode(s: &str) -> String {
    s.replace("&amp;", "&").replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&#39;", "'").replace("&#x27;", "'").trim().chars().take(300).collect()
}

async fn unfurl(State(s): State<Shared>, headers: HeaderMap, Query(q): Query<UnfurlQuery>) -> Result<Json<Unfurl>, ApiError> {
    session(&s, &headers)?;
    let url = q.url.trim().to_string();
    let (host, port) = parse_http(&url).ok_or_else(|| bad_request("only http(s) URLs are supported".to_string()))?;
    let hp = format!("{host}:{port}");
    let safe = tokio::task::spawn_blocking(move || {
        use std::net::ToSocketAddrs;
        match hp.to_socket_addrs() {
            Ok(addrs) => {
                let addrs: Vec<_> = addrs.collect();
                !addrs.is_empty() && addrs.iter().all(|a| !is_blocked_ip(a.ip()))
            }
            Err(_) => false,
        }
    })
    .await
    .unwrap_or(false);
    if !safe {
        return Err((StatusCode::FORBIDDEN, "host not allowed".to_string()));
    }
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .redirect(reqwest::redirect::Policy::limited(3))
        .build()
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let resp = client.get(&url).header("user-agent", "weaveN-unfurl/1.0").send().await.map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))?;
    let ct = resp.headers().get("content-type").and_then(|v| v.to_str().ok()).unwrap_or("").to_lowercase();
    let mut out = Unfurl { url: url.clone(), title: String::new(), description: String::new(), host };
    if ct.contains("html") || ct.contains("xml") || ct.is_empty() {
        let body = resp.text().await.unwrap_or_default();
        let html = &body[..body.len().min(600_000)];
        out.title = unfurl_meta(html, "og:title").or_else(|| unfurl_title(html)).unwrap_or_default();
        out.description = unfurl_meta(html, "og:description").unwrap_or_default();
    }
    Ok(Json(out))
}

// --- Tier 3: sprints (Agile) ------------------------------------------------

#[derive(Deserialize)]
struct SprintsQuery {
    project: String,
}

#[derive(Serialize)]
struct SprintItem {
    id: i64,
    state: String,
    name: String,
}

async fn sprints(State(s): State<Shared>, headers: HeaderMap, Query(q): Query<SprintsQuery>) -> Result<Json<Vec<SprintItem>>, ApiError> {
    let (creds, _author, _ws) = jira_session(&s, &headers)?;
    let project = q.project.clone();
    let list = blocking(move || {
        RealBackend::from_parts(&creds.base, creds.email.as_deref(), &creds.token, creds.bearer).list_sprints(&project)
    })
    .await?;
    Ok(Json(list.into_iter().map(|(id, state, name)| SprintItem { id, state, name }).collect()))
}

#[derive(Deserialize)]
struct SprintAssignReq {
    issue: String,
    sprint_id: i64,
}

async fn sprint_assign(State(s): State<Shared>, headers: HeaderMap, Json(r): Json<SprintAssignReq>) -> Result<StatusCode, ApiError> {
    let (creds, _author, _ws) = jira_session(&s, &headers)?;
    let (issue, sid) = (r.issue.clone(), r.sprint_id);
    blocking(move || {
        RealBackend::from_parts(&creds.base, creds.email.as_deref(), &creds.token, creds.bearer).add_to_sprint(sid, &issue)
    })
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

// --- Tier 4: encrypted DM vault ---------------------------------------------

/// Get-or-create an age keypair for an owner (accountId or "__escrow__"); returns
/// the public key. Secret keys are stored at rest (prototype; prod uses a KMS).
async fn vault_pub(s: &AppState, owner: &str) -> Result<String, ApiError> {
    if let Some((pubk, _sec)) = weavn_store::vault_key_get(&s.pool, owner).await.map_err(db_err)? {
        return Ok(pubk);
    }
    let kp = weavn::keys::generate();
    weavn_store::vault_key_put(&s.pool, owner, &kp.public, &kp.secret).await.map_err(db_err)?;
    Ok(kp.public)
}

#[derive(Deserialize)]
struct DmTarget {
    id: String,
    #[serde(default)]
    display: String,
}

#[derive(Deserialize)]
struct DmSendReq {
    #[serde(default)]
    to: Vec<DmTarget>,
    #[serde(default)]
    convo: Option<i64>,
    #[serde(default)]
    text: String,
}

#[derive(Serialize)]
struct DmConvoResp {
    id: i64,
    title: String,
    members: Vec<String>,
}

#[derive(Serialize)]
struct DmLineResp {
    from: String,
    text: String,
    ts: i64,
    mine: bool,
}

/// Send a DM: encrypt the body to every participant + the org escrow key and
/// store it in the vault. Never touches JIRA.
async fn dm_send(State(s): State<Shared>, headers: HeaderMap, Json(r): Json<DmSendReq>) -> Result<Json<serde_json::Value>, ApiError> {
    let sess = session(&s, &headers)?;
    let ws = sess.site.clone().unwrap_or_else(|| "default".to_string());
    let me = sess.subject.clone();
    let me_disp = sess.author();
    if r.text.trim().is_empty() {
        return Err(bad_request("empty message".to_string()));
    }
    let mut mem: std::collections::BTreeMap<String, String> = std::collections::BTreeMap::new();
    if let Some(cid) = r.convo {
        // reply into an existing conversation — reuse its member set
        if !weavn_store::dm_is_member(&s.pool, cid, &me).await.map_err(db_err)? {
            return Err((StatusCode::FORBIDDEN, "not a participant".to_string()));
        }
        for (id, disp) in weavn_store::dm_members(&s.pool, cid).await.map_err(db_err)? {
            mem.insert(id, disp);
        }
    } else {
        mem.insert(me.clone(), me_disp.clone());
        for t in &r.to {
            let id = t.id.trim();
            if !id.is_empty() {
                mem.insert(id.to_string(), if t.display.trim().is_empty() { id.to_string() } else { t.display.trim().to_string() });
            }
        }
    }
    if mem.len() < 2 {
        return Err(bad_request("add at least one recipient".to_string()));
    }
    let mut pubs = Vec::new();
    for id in mem.keys() {
        pubs.push(vault_pub(&s, id).await?);
    }
    pubs.push(vault_pub(&s, "__escrow__").await?);
    let ct = weavn::keys::encrypt_to(&pubs, r.text.trim().as_bytes()).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    let members_csv = mem.keys().cloned().collect::<Vec<_>>().join(","); // BTreeMap → sorted
    let title = mem.values().cloned().collect::<Vec<_>>().join(", ");
    let convo = weavn_store::dm_convo_upsert(&s.pool, &ws, &members_csv, &title).await.map_err(db_err)?;
    for (id, disp) in &mem {
        weavn_store::dm_member_put(&s.pool, convo, id, disp).await.map_err(db_err)?;
    }
    weavn_store::dm_message_insert(&s.pool, convo, &me, &me_disp, &ct).await.map_err(db_err)?;
    s.hub.publish(&ws, serde_json::json!({ "type": "dm" }).to_string());
    Ok(Json(serde_json::json!({ "convo": convo })))
}

async fn dm_list(State(s): State<Shared>, headers: HeaderMap) -> Result<Json<Vec<DmConvoResp>>, ApiError> {
    let (ws, me) = user_ctx(&s, &headers)?;
    let convos = weavn_store::dm_convos_for(&s.pool, &ws, &me).await.map_err(db_err)?;
    let mut out = Vec::new();
    for (id, _members, _title, _ts) in convos {
        let members = weavn_store::dm_members(&s.pool, id).await.map_err(db_err)?;
        let others: Vec<String> = members.iter().filter(|(acc, _)| acc != &me).map(|(_, d)| d.clone()).collect();
        let title = if others.is_empty() { "(you)".to_string() } else { others.join(", ") };
        out.push(DmConvoResp { id, title, members: members.into_iter().map(|(_, d)| d).collect() });
    }
    Ok(Json(out))
}

#[derive(Deserialize)]
struct DmReadQuery {
    convo: i64,
}

async fn dm_read(State(s): State<Shared>, headers: HeaderMap, Query(q): Query<DmReadQuery>) -> Result<Json<Vec<DmLineResp>>, ApiError> {
    let (_ws, me) = user_ctx(&s, &headers)?;
    if !weavn_store::dm_is_member(&s.pool, q.convo, &me).await.map_err(db_err)? {
        return Err((StatusCode::FORBIDDEN, "not a participant".to_string()));
    }
    let (_pub, sec) = weavn_store::vault_key_get(&s.pool, &me)
        .await
        .map_err(db_err)?
        .ok_or((StatusCode::INTERNAL_SERVER_ERROR, "no vault key for you".to_string()))?;
    let msgs = weavn_store::dm_messages(&s.pool, q.convo).await.map_err(db_err)?;
    let lines = msgs
        .into_iter()
        .map(|(sender, disp, ct, ts)| {
            let text = weavn::keys::decrypt_with(&sec, &ct).ok().and_then(|b| String::from_utf8(b).ok()).unwrap_or_else(|| "[locked]".to_string());
            DmLineResp { from: disp, text, ts, mine: sender == me }
        })
        .collect();
    Ok(Json(lines))
}

// --- Tier 4: bot users ------------------------------------------------------

fn bearer(headers: &HeaderMap) -> Option<String> {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
        .map(|s| s.trim().to_string())
}

#[derive(Deserialize)]
struct BotCreateReq {
    #[serde(default)]
    name: Option<String>,
}

#[derive(Serialize)]
struct BotInfo {
    token: String,
    name: String,
    created_by: String,
}

async fn bots_create(State(s): State<Shared>, headers: HeaderMap, Json(r): Json<BotCreateReq>) -> Result<Json<BotInfo>, ApiError> {
    let (ws, user) = user_ctx(&s, &headers)?;
    let name = r.name.unwrap_or_default();
    let name = if name.trim().is_empty() { "bot".to_string() } else { name.trim().to_string() };
    let token = gen_token();
    weavn_store::bot_create(&s.pool, &token, &ws, &name, &user).await.map_err(db_err)?;
    Ok(Json(BotInfo { token, name, created_by: user }))
}

async fn bots_list(State(s): State<Shared>, headers: HeaderMap) -> Result<Json<Vec<BotInfo>>, ApiError> {
    let (ws, _user) = user_ctx(&s, &headers)?;
    let list = weavn_store::bot_list(&s.pool, &ws).await.map_err(db_err)?.into_iter().map(|(token, name, created_by)| BotInfo { token, name, created_by }).collect();
    Ok(Json(list))
}

#[derive(Deserialize)]
struct BotDelReq {
    token: String,
}

async fn bots_delete(State(s): State<Shared>, headers: HeaderMap, Json(r): Json<BotDelReq>) -> Result<StatusCode, ApiError> {
    let (ws, _user) = user_ctx(&s, &headers)?;
    weavn_store::bot_delete(&s.pool, &ws, &r.token).await.map_err(db_err)?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct BotPostReq {
    #[serde(default)]
    token: String,
    channel: String,
    #[serde(default)]
    text: String,
    #[serde(default)]
    actions: Vec<ActionDef>,
}

/// Bot-authenticated post: `Authorization: Bearer <token>` (or token in body).
/// Posts to a channel as the bot via the service account. Rejects private channels.
async fn bot_post(State(s): State<Shared>, headers: HeaderMap, Json(r): Json<BotPostReq>) -> Result<Json<OutcomeResp>, ApiError> {
    let token = bearer(&headers).unwrap_or_else(|| r.token.clone());
    if token.is_empty() {
        return Err((StatusCode::UNAUTHORIZED, "bot token required".to_string()));
    }
    let (ws, name) = weavn_store::bot_lookup(&s.pool, &token).await.map_err(db_err)?.ok_or((StatusCode::UNAUTHORIZED, "invalid bot token".to_string()))?;
    let channel = r.channel.trim().to_string();
    if channel.is_empty() {
        return Err(bad_request("channel required".to_string()));
    }
    if let Some((true, _)) = weavn_store::channel_acl(&s.pool, &ws, &channel).await.map_err(db_err)? {
        return Err((StatusCode::FORBIDDEN, "bots cannot post to a private channel".to_string()));
    }
    let text = r.text.trim().to_string();
    if text.is_empty() {
        return Err(bad_request("missing text".to_string()));
    }
    let (project, slug) = resolve_target(&s, None, &ws, &channel).await?;
    let summary = weavn::proto::summarize(&text, 80);
    let (b2, sum2) = (text.clone(), summary.clone());
    let (key, status) = blocking(move || {
        let mut be = RealBackend::from_env()?;
        let adf = weavn::format::mrkdwn_to_adf(&b2);
        let key = be.create_issue(&project, &slug, &sum2, &b2, &adf).map(|ci| ci.key)?;
        let st = be.issue_status(&key).unwrap_or_default();
        Ok::<_, String>((key, st))
    })
    .await?;
    weavn_store::message_insert(&s.pool, &ws, &channel, &key, None, &name, &text, false, false).await.map_err(db_err)?;
    weavn_store::status_set(&s.pool, &ws, &key, &status).await.map_err(db_err)?;
    save_actions(&s, &ws, &key, &r.actions).await?;
    notify_for_message(&s, &ws, &name, "", &channel, &key, &text, &[]).await;
    run_workflows(&s, &ws, &channel, &key, &text).await;
    s.hub.publish(&ws, event_message(&channel));
    Ok(Json(OutcomeResp { kind: "bot", issue: key, comment: None }))
}

// --- Tier 4: interactive messages (action buttons) -------------------------

#[derive(Deserialize, Clone)]
struct ActionDef {
    #[serde(default)]
    text: String,
    #[serde(default)]
    kind: String,
    #[serde(default)]
    arg: String,
}

#[derive(Serialize)]
struct ActionResp {
    idx: i32,
    label: String,
}

/// Persist action buttons posted with a message (allowlisted kinds only).
async fn save_actions(s: &AppState, ws: &str, issue: &str, defs: &[ActionDef]) -> Result<(), ApiError> {
    if defs.is_empty() {
        return Ok(());
    }
    weavn_store::actions_clear(&s.pool, ws, issue).await.map_err(db_err)?;
    let mut idx = 0i32;
    for d in defs {
        let kind = d.kind.trim();
        if !matches!(kind, "transition" | "reply" | "ack") {
            continue; // allowlist — ignore anything else
        }
        let label = if d.text.trim().is_empty() { kind } else { d.text.trim() };
        weavn_store::action_add(&s.pool, ws, issue, idx, label, kind, d.arg.trim()).await.map_err(db_err)?;
        idx += 1;
    }
    Ok(())
}

#[derive(Deserialize)]
struct ActionReq {
    issue: String,
    idx: i32,
}

/// Click an action button — runs the allowlisted action as the clicking user.
async fn action(State(s): State<Shared>, headers: HeaderMap, Json(r): Json<ActionReq>) -> Result<Json<serde_json::Value>, ApiError> {
    let (creds, author, ws) = jira_session(&s, &headers)?;
    let me_acc = subject(&s, &headers)?;
    let (kind, arg) = weavn_store::action_get(&s.pool, &ws, &r.issue, r.idx).await.map_err(db_err)?.ok_or((StatusCode::NOT_FOUND, "no such action".to_string()))?;
    let channel = weavn_store::channel_of_issue(&s.pool, &ws, &r.issue).await.map_err(db_err)?.ok_or_else(|| bad_request(format!("unknown issue {}", r.issue)))?;
    require_channel_access(&s, &ws, &me_acc, &channel).await?;
    match kind.as_str() {
        "transition" => {
            let (c, issue2, to2) = (creds.clone(), r.issue.clone(), arg.clone());
            let status = blocking(move || {
                let b = RealBackend::from_parts(&c.base, c.email.as_deref(), &c.token, c.bearer);
                b.transition_to(&issue2, &to2)?;
                Ok::<_, String>(b.issue_status(&issue2).unwrap_or_else(|_| to2.clone()))
            })
            .await?;
            weavn_store::status_set(&s.pool, &ws, &r.issue, &status).await.map_err(db_err)?;
        }
        "reply" | "ack" => {
            let body = if kind == "ack" { format!("✓ {author} acknowledged") } else { arg.clone() };
            let (c, issue2, author2, body2) = (creds.clone(), r.issue.clone(), author.clone(), body.clone());
            blocking(move || {
                let mut b = RealBackend::from_parts(&c.base, c.email.as_deref(), &c.token, c.bearer);
                let adf = weavn::format::mrkdwn_to_adf(&body2);
                b.add_comment(&issue2, &author2, &body2, &adf).map(|_| ())
            })
            .await?;
            weavn_store::message_insert(&s.pool, &ws, &channel, &r.issue, Some(&r.issue), &author, &body, false, false).await.map_err(db_err)?;
        }
        _ => return Err(bad_request("unknown action kind".to_string())),
    }
    s.hub.publish(&ws, event_message(&channel));
    Ok(Json(serde_json::json!({ "ok": true })))
}

// --- Tier 4: Workflow Builder -----------------------------------------------

#[derive(Deserialize)]
struct WorkflowReq {
    #[serde(default)]
    channel: Option<String>,
    #[serde(default)]
    contains: String,
    kind: String,
    #[serde(default)]
    arg: String,
}

#[derive(Serialize)]
struct WorkflowInfo {
    id: i64,
    channel: String,
    contains: String,
    kind: String,
    arg: String,
    enabled: bool,
}

async fn workflows_create(State(s): State<Shared>, headers: HeaderMap, Json(r): Json<WorkflowReq>) -> Result<Json<WorkflowInfo>, ApiError> {
    let (ws, user) = user_ctx(&s, &headers)?;
    let kind = r.kind.trim().to_string();
    if !matches!(kind.as_str(), "react" | "reply" | "transition" | "addlabel" | "pin") {
        return Err(bad_request("kind must be react | reply | transition | addlabel | pin".to_string()));
    }
    let channel = r.channel.unwrap_or_default();
    let channel = if channel.trim().is_empty() { "*".to_string() } else { channel.trim().to_string() };
    let contains = r.contains.trim().to_string();
    let arg = r.arg.trim().to_string();
    let id = weavn_store::workflow_create(&s.pool, &ws, &channel, &contains, &kind, &arg, &user).await.map_err(db_err)?;
    Ok(Json(WorkflowInfo { id, channel, contains, kind, arg, enabled: true }))
}

async fn workflows_get(State(s): State<Shared>, headers: HeaderMap) -> Result<Json<Vec<WorkflowInfo>>, ApiError> {
    let (ws, _user) = user_ctx(&s, &headers)?;
    let list = weavn_store::workflows_list(&s.pool, &ws)
        .await
        .map_err(db_err)?
        .into_iter()
        .map(|(id, channel, contains, kind, arg, enabled, _by)| WorkflowInfo { id, channel, contains, kind, arg, enabled })
        .collect();
    Ok(Json(list))
}

#[derive(Deserialize)]
struct WorkflowIdReq {
    id: i64,
    #[serde(default)]
    enabled: Option<bool>,
}

async fn workflows_delete(State(s): State<Shared>, headers: HeaderMap, Json(r): Json<WorkflowIdReq>) -> Result<StatusCode, ApiError> {
    let (ws, _user) = user_ctx(&s, &headers)?;
    weavn_store::workflow_delete(&s.pool, &ws, r.id).await.map_err(db_err)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn workflows_toggle(State(s): State<Shared>, headers: HeaderMap, Json(r): Json<WorkflowIdReq>) -> Result<StatusCode, ApiError> {
    let (ws, _user) = user_ctx(&s, &headers)?;
    weavn_store::workflow_toggle(&s.pool, &ws, r.id, r.enabled.unwrap_or(true)).await.map_err(db_err)?;
    Ok(StatusCode::NO_CONTENT)
}

// --- Tier 3: incoming webhooks ----------------------------------------------

fn gen_token() -> String {
    use std::io::Read;
    let mut buf = [0u8; 18];
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        let _ = f.read_exact(&mut buf);
    }
    BASE64_URL_SAFE_NO_PAD.encode(buf)
}

#[derive(Deserialize)]
struct WebhookReq {
    channel: String,
    #[serde(default)]
    label: Option<String>,
}

#[derive(Serialize)]
struct WebhookHook {
    token: String,
    label: String,
    created_by: String,
    url: String,
}

async fn webhook_create(State(s): State<Shared>, headers: HeaderMap, Json(r): Json<WebhookReq>) -> Result<Json<WebhookHook>, ApiError> {
    let (ws, user) = user_ctx(&s, &headers)?;
    let channel = r.channel.trim().to_string();
    if channel.is_empty() {
        return Err(bad_request("channel required".to_string()));
    }
    require_channel_access(&s, &ws, &user, &channel).await?;
    let label = r.label.unwrap_or_default();
    let label = if label.trim().is_empty() { "webhook".to_string() } else { label.trim().to_string() };
    let token = gen_token();
    weavn_store::webhook_create(&s.pool, &token, &ws, &channel, &label, &user).await.map_err(db_err)?;
    Ok(Json(WebhookHook { url: format!("/hook/{token}"), token, label, created_by: user }))
}

#[derive(Deserialize)]
struct WebhookListQuery {
    channel: String,
}

async fn webhooks_list(State(s): State<Shared>, headers: HeaderMap, Query(q): Query<WebhookListQuery>) -> Result<Json<Vec<WebhookHook>>, ApiError> {
    let (ws, user) = user_ctx(&s, &headers)?;
    require_channel_access(&s, &ws, &user, &q.channel).await?;
    let list = weavn_store::webhook_list(&s.pool, &ws, &q.channel)
        .await
        .map_err(db_err)?
        .into_iter()
        .map(|(token, label, created_by)| WebhookHook { url: format!("/hook/{token}"), token, label, created_by })
        .collect();
    Ok(Json(list))
}

#[derive(Deserialize)]
struct WebhookDelReq {
    token: String,
}

async fn webhook_delete(State(s): State<Shared>, headers: HeaderMap, Json(r): Json<WebhookDelReq>) -> Result<StatusCode, ApiError> {
    let (ws, _user) = user_ctx(&s, &headers)?;
    weavn_store::webhook_delete(&s.pool, &ws, &r.token).await.map_err(db_err)?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct HookBody {
    #[serde(default)]
    text: String,
    #[serde(default)]
    body: String,
    #[serde(default)]
    actions: Vec<ActionDef>,
}

/// Unauthenticated incoming webhook: the secret token in the URL is the auth.
/// Posts a message to the mapped channel as the server's service JIRA account.
async fn hook(State(s): State<Shared>, Path(token): Path<String>, Json(b): Json<HookBody>) -> Result<Json<OutcomeResp>, ApiError> {
    let (ws, channel, label) = weavn_store::webhook_lookup(&s.pool, &token)
        .await
        .map_err(db_err)?
        .ok_or((StatusCode::NOT_FOUND, "unknown webhook".to_string()))?;
    let text = if !b.text.trim().is_empty() { b.text.trim().to_string() } else { b.body.trim().to_string() };
    if text.is_empty() {
        return Err(bad_request("missing 'text'".to_string()));
    }
    let (project, slug) = resolve_target(&s, None, &ws, &channel).await?;
    let summary = weavn::proto::summarize(&text, 80);
    let (b2, sum2) = (text.clone(), summary.clone());
    let (key, status) = blocking(move || {
        let mut be = RealBackend::from_env()?; // server's service account (bot identity)
        let adf = weavn::format::mrkdwn_to_adf(&b2);
        let key = be.create_issue(&project, &slug, &sum2, &b2, &adf).map(|ci| ci.key)?;
        let status = be.issue_status(&key).unwrap_or_default();
        Ok::<_, String>((key, status))
    })
    .await?;
    weavn_store::message_insert(&s.pool, &ws, &channel, &key, None, &label, &text, false, false).await.map_err(db_err)?;
    weavn_store::status_set(&s.pool, &ws, &key, &status).await.map_err(db_err)?;
    save_actions(&s, &ws, &key, &b.actions).await?;
    notify_for_message(&s, &ws, &label, "", &channel, &key, &text, &[]).await;
    run_workflows(&s, &ws, &channel, &key, &text).await;
    s.hub.publish(&ws, event_message(&channel));
    Ok(Json(OutcomeResp { kind: "webhook", issue: key, comment: None }))
}

// --- Tier 3: reactions, read-state, search ----------------------------------

#[derive(Deserialize)]
struct ReactReq {
    issue: String,
    shortcode: String,
    on: bool,
}

/// Toggle an emoji reaction on a conversation, then fan out a live update.
async fn react(State(s): State<Shared>, headers: HeaderMap, Json(r): Json<ReactReq>) -> Result<StatusCode, ApiError> {
    let (ws, user) = user_ctx(&s, &headers)?;
    let ch = weavn_store::channel_of_issue(&s.pool, &ws, &r.issue).await.map_err(db_err)?;
    if let Some(c) = &ch {
        require_channel_access(&s, &ws, &user, c).await?;
    }
    weavn_store::react_set(&s.pool, &ws, &r.issue, &r.shortcode, &user, r.on).await.map_err(db_err)?;
    if let Some(c) = ch {
        s.hub.publish(&ws, event_message(&c));
    }
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct MarkReadReq {
    channel: String,
    ts: i64,
}

async fn mark_read(State(s): State<Shared>, headers: HeaderMap, Json(r): Json<MarkReadReq>) -> Result<StatusCode, ApiError> {
    let (ws, user) = user_ctx(&s, &headers)?;
    weavn_store::last_read_set(&s.pool, &ws, &user, &r.channel, r.ts).await.map_err(db_err)?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct SearchQuery {
    q: String,
}

#[derive(Serialize)]
struct SearchHit {
    channel: String,
    issue_key: String,
    author: String,
    snippet: String,
    ts: i64,
}

async fn search(State(s): State<Shared>, headers: HeaderMap, Query(q): Query<SearchQuery>) -> Result<Json<Vec<SearchHit>>, ApiError> {
    let (ws, user) = user_ctx(&s, &headers)?;
    let query = q.q.trim();
    if query.is_empty() {
        return Ok(Json(Vec::new()));
    }
    // Exclude hits from private channels the user isn't a member of.
    let priv_all: HashSet<String> = weavn_store::private_channels(&s.pool, &ws).await.map_err(db_err)?.into_iter().collect();
    let mine: HashSet<String> = weavn_store::my_private_channels(&s.pool, &ws, &user).await.map_err(db_err)?.into_iter().collect();
    let hits = weavn_store::search(&s.pool, &ws, query, 80).await.map_err(db_err)?;
    Ok(Json(
        hits.into_iter()
            .filter(|(channel, ..)| !priv_all.contains(channel) || mine.contains(channel))
            .take(50)
            .map(|(channel, issue_key, author, body, ts)| {
                let snippet: String = body.chars().take(160).collect();
                SearchHit { channel, issue_key, author, snippet, ts }
            })
            .collect(),
    ))
}

#[cfg(test)]
mod tests {
    use super::{days_from_civil, parse_jira_ts};

    #[test]
    fn civil_epoch_anchor() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(1969, 12, 31), -1);
        assert_eq!(days_from_civil(2000, 1, 1), 10957);
    }

    #[test]
    fn parses_jira_timestamps() {
        // 2025-03-14T10:22:33Z == 1741947753 (UTC)
        assert_eq!(parse_jira_ts("2025-03-14T10:22:33.000+0000"), Some(1_741_947_753));
        assert_eq!(parse_jira_ts("2025-03-14T10:22:33Z"), Some(1_741_947_753));
        // +02:00 offset: the same wall clock is two hours earlier in UTC.
        assert_eq!(parse_jira_ts("2025-03-14T12:22:33.000+02:00"), Some(1_741_947_753));
        // A negative offset moves UTC later.
        assert_eq!(parse_jira_ts("2025-03-14T05:22:33.000-0500"), Some(1_741_947_753));
        // The Unix epoch itself.
        assert_eq!(parse_jira_ts("1970-01-01T00:00:00.000+0000"), Some(0));
        assert_eq!(parse_jira_ts("not a date"), None);
    }
}
