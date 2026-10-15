//! Weavn operational store (docs/DESIGN.md §10): plain PostgreSQL via sqlx,
//! reached only from here. Host-agnostic by connection string (Supabase for dev).
//!
//! First slice: per-workspace `config` (the prototype's env vars) and the custom
//! `emoji` registry with an optional usage allowlist. Projection/cache tables come
//! as later migrations when the gateway is moved off its in-memory store.

use sqlx::postgres::{PgPool, PgPoolOptions};

pub use sqlx::PgPool as Pool;

/// Licensing & seat metering (docs/LICENSING.md).
mod license;
pub use license::*;

/// Embedded migrations (read at compile time; no DB needed to build).
pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

pub async fn connect(url: &str) -> Result<PgPool, sqlx::Error> {
    PgPoolOptions::new().max_connections(5).connect(url).await
}

pub async fn migrate(pool: &PgPool) -> Result<(), sqlx::migrate::MigrateError> {
    MIGRATOR.run(pool).await
}

// --- config: per-workspace settings -----------------------------------------

pub async fn config_set(pool: &PgPool, workspace: &str, key: &str, value: &str) -> sqlx::Result<()> {
    sqlx::query(
        "insert into config (workspace_id, key, value) values ($1, $2, $3) \
         on conflict (workspace_id, key) do update set value = excluded.value",
    )
    .bind(workspace)
    .bind(key)
    .bind(value)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn config_get(pool: &PgPool, workspace: &str, key: &str) -> sqlx::Result<Option<String>> {
    sqlx::query_scalar::<_, String>("select value from config where workspace_id = $1 and key = $2")
        .bind(workspace)
        .bind(key)
        .fetch_optional(pool)
        .await
}

pub async fn config_list(pool: &PgPool, workspace: &str) -> sqlx::Result<Vec<(String, String)>> {
    sqlx::query_as::<_, (String, String)>(
        "select key, value from config where workspace_id = $1 order by key",
    )
    .bind(workspace)
    .fetch_all(pool)
    .await
}

// --- custom emoji registry ---------------------------------------------------

/// Register (or update) a custom emoji. `allowed` empty = anyone may use it;
/// otherwise only those account ids may apply it.
pub async fn emoji_register(
    pool: &PgPool,
    workspace: &str,
    shortcode: &str,
    image_ref: &str,
    created_by: &str,
    allowed: &[String],
) -> sqlx::Result<()> {
    let allowed_csv: Option<String> = if allowed.is_empty() {
        None
    } else {
        Some(allowed.join(","))
    };
    sqlx::query(
        "insert into emoji (workspace_id, shortcode, image_ref, created_by, allowed_accounts) \
         values ($1, $2, $3, $4, $5) \
         on conflict (workspace_id, shortcode) \
         do update set image_ref = excluded.image_ref, allowed_accounts = excluded.allowed_accounts",
    )
    .bind(workspace)
    .bind(shortcode)
    .bind(image_ref)
    .bind(created_by)
    .bind(allowed_csv)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn emoji_list(pool: &PgPool, workspace: &str) -> sqlx::Result<Vec<(String, Option<String>)>> {
    sqlx::query_as::<_, (String, Option<String>)>(
        "select shortcode, allowed_accounts from emoji where workspace_id = $1 order by shortcode",
    )
    .bind(workspace)
    .fetch_all(pool)
    .await
}

// --- per-user preferences (Tier 2) ------------------------------------------

/// Distinct channels in a workspace with their top-level conversation count.
pub async fn channels_list(pool: &PgPool, workspace: &str) -> sqlx::Result<Vec<(String, i64)>> {
    sqlx::query_as::<_, (String, i64)>(
        "select channel, count(*) filter (where parent_issue is null) as n \
         from message where workspace = $1 group by channel order by channel",
    )
    .bind(workspace)
    .fetch_all(pool)
    .await
}

async fn flag_set(pool: &PgPool, table: &str, ws: &str, user: &str, value: &str, on: bool) -> sqlx::Result<()> {
    if on {
        let sql = format!("insert into {table} (workspace, user_id, {col}) values ($1,$2,$3) on conflict do nothing",
            col = if table == "saved" { "issue_key" } else { "channel" });
        sqlx::query(&sql).bind(ws).bind(user).bind(value).execute(pool).await?;
    } else {
        let sql = format!("delete from {table} where workspace=$1 and user_id=$2 and {col}=$3",
            col = if table == "saved" { "issue_key" } else { "channel" });
        sqlx::query(&sql).bind(ws).bind(user).bind(value).execute(pool).await?;
    }
    Ok(())
}

async fn flag_list(pool: &PgPool, table: &str, ws: &str, user: &str) -> sqlx::Result<Vec<String>> {
    let col = if table == "saved" { "issue_key" } else { "channel" };
    let sql = format!("select {col} from {table} where workspace=$1 and user_id=$2 order by {col}");
    sqlx::query_scalar::<_, String>(&sql).bind(ws).bind(user).fetch_all(pool).await
}

pub async fn star_set(pool: &PgPool, ws: &str, user: &str, channel: &str, on: bool) -> sqlx::Result<()> {
    flag_set(pool, "star", ws, user, channel, on).await
}
pub async fn stars(pool: &PgPool, ws: &str, user: &str) -> sqlx::Result<Vec<String>> {
    flag_list(pool, "star", ws, user).await
}
pub async fn archive_set(pool: &PgPool, ws: &str, user: &str, channel: &str, on: bool) -> sqlx::Result<()> {
    flag_set(pool, "archived", ws, user, channel, on).await
}
pub async fn archived(pool: &PgPool, ws: &str, user: &str) -> sqlx::Result<Vec<String>> {
    flag_list(pool, "archived", ws, user).await
}
pub async fn saved_set(pool: &PgPool, ws: &str, user: &str, issue: &str, on: bool) -> sqlx::Result<()> {
    flag_set(pool, "saved", ws, user, issue, on).await
}
pub async fn saved_list(pool: &PgPool, ws: &str, user: &str) -> sqlx::Result<Vec<String>> {
    flag_list(pool, "saved", ws, user).await
}

pub async fn state_set(pool: &PgPool, ws: &str, user: &str, emoji: Option<&str>, text: Option<&str>, dnd: bool) -> sqlx::Result<()> {
    sqlx::query(
        "insert into user_state (workspace, user_id, status_emoji, status_text, dnd) values ($1,$2,$3,$4,$5) \
         on conflict (workspace, user_id) do update set status_emoji = excluded.status_emoji, status_text = excluded.status_text, dnd = excluded.dnd",
    )
    .bind(ws).bind(user).bind(emoji).bind(text).bind(dnd)
    .execute(pool).await?;
    Ok(())
}

pub async fn state_get(pool: &PgPool, ws: &str, user: &str) -> sqlx::Result<Option<(Option<String>, Option<String>, bool)>> {
    sqlx::query_as::<_, (Option<String>, Option<String>, bool)>(
        "select status_emoji, status_text, dnd from user_state where workspace=$1 and user_id=$2",
    )
    .bind(ws).bind(user).fetch_optional(pool).await
}

// --- projection: messages ----------------------------------------------------

/// One projected message row (a top-level message has `parent_issue = None`).
#[derive(sqlx::FromRow)]
pub struct MsgRow {
    pub issue_key: String,
    pub parent_issue: Option<String>,
    pub author: String,
    pub body: String,
    pub from_jira: bool,
    pub deleted: bool,
    pub broadcast: bool,
    pub status: String,
    pub created_at: i64, // unix seconds
}

#[allow(clippy::too_many_arguments)]
pub async fn message_insert(
    pool: &PgPool,
    workspace: &str,
    channel: &str,
    issue_key: &str,
    parent_issue: Option<&str>,
    author: &str,
    body: &str,
    from_jira: bool,
    broadcast: bool,
) -> sqlx::Result<()> {
    sqlx::query(
        "insert into message (workspace, channel, issue_key, parent_issue, author, body, from_jira, broadcast) \
         values ($1, $2, $3, $4, $5, $6, $7, $8)",
    )
    .bind(workspace)
    .bind(channel)
    .bind(issue_key)
    .bind(parent_issue)
    .bind(author)
    .bind(body)
    .bind(from_jira)
    .bind(broadcast)
    .execute(pool)
    .await?;
    Ok(())
}

/// Replace a conversation's top-level body with `body`, collapsing any coalesced
/// fragments into the earliest row, and set its `deleted` flag. Used by edit
/// (deleted=false) and soft delete (deleted=true). JIRA keeps the full changelog.
pub async fn toplevel_set(pool: &PgPool, workspace: &str, issue_key: &str, body: &str, deleted: bool) -> sqlx::Result<()> {
    // Remove coalesced fragments, keeping the earliest top-level row.
    sqlx::query(
        "delete from message where workspace = $1 and issue_key = $2 and parent_issue is null \
         and id <> (select min(id) from message where workspace = $1 and issue_key = $2 and parent_issue is null)",
    )
    .bind(workspace)
    .bind(issue_key)
    .execute(pool)
    .await?;
    sqlx::query(
        "update message set body = $3, deleted = $4 \
         where workspace = $1 and issue_key = $2 and parent_issue is null",
    )
    .bind(workspace)
    .bind(issue_key)
    .bind(body)
    .bind(deleted)
    .execute(pool)
    .await?;
    Ok(())
}

// --- shared pins -------------------------------------------------------------

pub async fn pin_set(pool: &PgPool, workspace: &str, channel: &str, issue_key: &str, pinned_by: &str, on: bool) -> sqlx::Result<()> {
    if on {
        sqlx::query(
            "insert into pinned (workspace, channel, issue_key, pinned_by) values ($1,$2,$3,$4) \
             on conflict (workspace, issue_key) do nothing",
        )
        .bind(workspace).bind(channel).bind(issue_key).bind(pinned_by)
        .execute(pool).await?;
    } else {
        sqlx::query("delete from pinned where workspace = $1 and issue_key = $2")
            .bind(workspace).bind(issue_key).execute(pool).await?;
    }
    Ok(())
}

/// Pinned issue keys in a channel (or its subtree, mirroring `read_messages`).
pub async fn pins(pool: &PgPool, workspace: &str, channel: &str, subtree: bool) -> sqlx::Result<Vec<String>> {
    let sql = if subtree {
        "select issue_key from pinned where workspace = $1 and (channel = $2 or channel like $2 || ' / %')"
    } else {
        "select issue_key from pinned where workspace = $1 and channel = $2"
    };
    sqlx::query_scalar::<_, String>(sql).bind(workspace).bind(channel).fetch_all(pool).await
}

pub async fn issue_exists(pool: &PgPool, workspace: &str, issue_key: &str) -> sqlx::Result<bool> {
    let n: i64 = sqlx::query_scalar("select count(*) from message where workspace = $1 and issue_key = $2")
        .bind(workspace)
        .bind(issue_key)
        .fetch_one(pool)
        .await?;
    Ok(n > 0)
}

pub async fn channel_of_issue(pool: &PgPool, workspace: &str, issue_key: &str) -> sqlx::Result<Option<String>> {
    sqlx::query_scalar("select channel from message where workspace = $1 and issue_key = $2 order by id limit 1")
        .bind(workspace)
        .bind(issue_key)
        .fetch_optional(pool)
        .await
}

/// Messages in a channel (or, with `subtree`, that channel and its descendants),
/// ordered for grouping into threads.
pub async fn read_messages(pool: &PgPool, workspace: &str, channel: &str, subtree: bool) -> sqlx::Result<Vec<MsgRow>> {
    let cols = "issue_key, parent_issue, author, body, from_jira, deleted, broadcast, status, extract(epoch from created_at)::bigint as created_at";
    let sql = if subtree {
        format!("select {cols} from message where workspace = $1 and (channel = $2 or channel like $2 || ' / %') order by id")
    } else {
        format!("select {cols} from message where workspace = $1 and channel = $2 order by id")
    };
    sqlx::query_as::<_, MsgRow>(&sql).bind(workspace).bind(channel).fetch_all(pool).await
}

/// Merged top-level body of an issue (root + coalesced fragments).
pub async fn issue_toplevel_body(pool: &PgPool, workspace: &str, issue_key: &str) -> sqlx::Result<String> {
    let rows: Vec<String> = sqlx::query_scalar(
        "select body from message where workspace = $1 and issue_key = $2 and parent_issue is null order by id",
    )
    .bind(workspace)
    .bind(issue_key)
    .fetch_all(pool)
    .await?;
    Ok(rows.join("\n"))
}

/// The most recent top-level message in a channel within the coalescing window,
/// as (issue_key, author). Used to decide whether a new send coalesces.
pub async fn last_toplevel(pool: &PgPool, workspace: &str, channel: &str, window_secs: i64) -> sqlx::Result<Option<(String, String)>> {
    let sql = format!(
        "select issue_key, author from message \
         where workspace = $1 and channel = $2 and parent_issue is null and from_jira = false \
         and created_at > now() - interval '{window_secs} seconds' order by id desc limit 1"
    );
    sqlx::query_as::<_, (String, String)>(&sql)
        .bind(workspace)
        .bind(channel)
        .fetch_optional(pool)
        .await
}

/// May `account` apply the emoji `shortcode`? `false` if the emoji is unknown.
pub async fn emoji_can_use(
    pool: &PgPool,
    workspace: &str,
    shortcode: &str,
    account: &str,
) -> sqlx::Result<bool> {
    let row: Option<Option<String>> = sqlx::query_scalar::<_, Option<String>>(
        "select allowed_accounts from emoji where workspace_id = $1 and shortcode = $2",
    )
    .bind(workspace)
    .bind(shortcode)
    .fetch_optional(pool)
    .await?;
    Ok(match row {
        None => false,                       // no such emoji
        Some(None) => true,                  // public
        Some(Some(csv)) if csv.is_empty() => true,
        Some(Some(csv)) => csv.split(',').any(|a| a == account),
    })
}

// --- Tier 3: reactions, read-state, search ----------------------------------

/// The channel-match SQL predicate shared by read/search/reactions.
fn channel_pred(subtree: bool) -> &'static str {
    if subtree { "(channel = $2 or channel like $2 || ' / %')" } else { "channel = $2" }
}

pub async fn react_set(pool: &PgPool, ws: &str, issue: &str, shortcode: &str, account: &str, on: bool) -> sqlx::Result<()> {
    if on {
        sqlx::query(
            "insert into reaction (workspace, issue_key, shortcode, account) values ($1,$2,$3,$4) \
             on conflict do nothing",
        )
        .bind(ws).bind(issue).bind(shortcode).bind(account).execute(pool).await?;
    } else {
        sqlx::query("delete from reaction where workspace=$1 and issue_key=$2 and shortcode=$3 and account=$4")
            .bind(ws).bind(issue).bind(shortcode).bind(account).execute(pool).await?;
    }
    Ok(())
}

/// Aggregated reactions for every conversation in a channel: (issue, emoji, count, mine).
pub async fn reactions_in_channel(pool: &PgPool, ws: &str, channel: &str, subtree: bool, me: &str) -> sqlx::Result<Vec<(String, String, i64, bool)>> {
    let sql = format!(
        "select r.issue_key, r.shortcode, count(*)::bigint, bool_or(r.account = $3) \
         from reaction r where r.workspace = $1 and r.issue_key in ( \
            select distinct issue_key from message where workspace = $1 and {pred} ) \
         group by r.issue_key, r.shortcode order by r.issue_key, r.shortcode",
        pred = channel_pred(subtree)
    );
    sqlx::query_as::<_, (String, String, i64, bool)>(&sql)
        .bind(ws).bind(channel).bind(me).fetch_all(pool).await
}

pub async fn last_read_set(pool: &PgPool, ws: &str, user: &str, channel: &str, ts: i64) -> sqlx::Result<()> {
    sqlx::query(
        "insert into last_read (workspace, user_id, channel, last_ts) values ($1,$2,$3,$4) \
         on conflict (workspace, user_id, channel) do update set last_ts = greatest(last_read.last_ts, excluded.last_ts)",
    )
    .bind(ws).bind(user).bind(channel).bind(ts).execute(pool).await?;
    Ok(())
}

/// Unread message counts per channel for a user (messages newer than last_read).
pub async fn unread_counts(pool: &PgPool, ws: &str, user: &str) -> sqlx::Result<Vec<(String, i64)>> {
    sqlx::query_as::<_, (String, i64)>(
        "select m.channel, count(*)::bigint from message m \
         left join last_read lr on lr.workspace = m.workspace and lr.user_id = $2 and lr.channel = m.channel \
         where m.workspace = $1 and extract(epoch from m.created_at)::bigint > coalesce(lr.last_ts, 0) \
         group by m.channel",
    )
    .bind(ws).bind(user).fetch_all(pool).await
}

/// Full-text search over message bodies in a workspace: (channel, issue, author, body, ts).
pub async fn search(pool: &PgPool, ws: &str, query: &str, limit: i64) -> sqlx::Result<Vec<(String, String, String, String, i64)>> {
    sqlx::query_as::<_, (String, String, String, String, i64)>(
        "select channel, issue_key, author, body, extract(epoch from created_at)::bigint \
         from message \
         where workspace = $1 and body_tsv @@ plainto_tsquery('english', $2) \
         order by ts_rank(body_tsv, plainto_tsquery('english', $2)) desc, created_at desc \
         limit $3",
    )
    .bind(ws).bind(query).bind(limit).fetch_all(pool).await
}

// --- Tier 3: attachments -----------------------------------------------------

#[allow(clippy::too_many_arguments)]
pub async fn attachment_insert(pool: &PgPool, ws: &str, issue: &str, jira_id: &str, filename: &str, mime: &str, size: i64, by: &str) -> sqlx::Result<()> {
    sqlx::query(
        "insert into attachment (workspace, issue_key, jira_id, filename, mime, size_bytes, uploaded_by) \
         values ($1,$2,$3,$4,$5,$6,$7)",
    )
    .bind(ws).bind(issue).bind(jira_id).bind(filename).bind(mime).bind(size).bind(by)
    .execute(pool).await?;
    Ok(())
}

/// Attachments for every conversation in a channel: (issue, jira_id, filename, mime, size).
pub async fn attachments_in_channel(pool: &PgPool, ws: &str, channel: &str, subtree: bool) -> sqlx::Result<Vec<(String, String, String, String, i64)>> {
    let sql = format!(
        "select a.issue_key, a.jira_id, a.filename, a.mime, a.size_bytes from attachment a \
         where a.workspace = $1 and a.issue_key in ( \
            select distinct issue_key from message where workspace = $1 and {pred} ) \
         order by a.id",
        pred = channel_pred(subtree)
    );
    sqlx::query_as::<_, (String, String, String, String, i64)>(&sql)
        .bind(ws).bind(channel).fetch_all(pool).await
}

/// Look up a projected attachment by its JIRA id within a workspace (for the
/// download proxy): (filename, mime). `None` if this workspace never stored it.
pub async fn attachment_meta(pool: &PgPool, ws: &str, jira_id: &str) -> sqlx::Result<Option<(String, String)>> {
    sqlx::query_as::<_, (String, String)>(
        "select filename, mime from attachment where workspace = $1 and jira_id = $2 limit 1",
    )
    .bind(ws).bind(jira_id).fetch_optional(pool).await
}

/// Record a conversation-issue's JIRA workflow status on its top-level row(s).
pub async fn status_set(pool: &PgPool, ws: &str, issue: &str, status: &str) -> sqlx::Result<()> {
    sqlx::query("update message set status = $3 where workspace = $1 and issue_key = $2 and parent_issue is null")
        .bind(ws).bind(issue).bind(status).execute(pool).await?;
    Ok(())
}

// --- Tier 3: private channels + membership ----------------------------------

/// (private, created_by) for a channel, or None if it has no ACL row (public).
pub async fn channel_acl(pool: &PgPool, ws: &str, channel: &str) -> sqlx::Result<Option<(bool, String)>> {
    sqlx::query_as::<_, (bool, String)>("select private, created_by from channel_acl where workspace=$1 and channel=$2")
        .bind(ws).bind(channel).fetch_optional(pool).await
}

/// Set a channel's private flag (upserting the ACL row).
pub async fn channel_set_private(pool: &PgPool, ws: &str, channel: &str, private: bool, by: &str) -> sqlx::Result<()> {
    sqlx::query(
        "insert into channel_acl (workspace, channel, private, created_by) values ($1,$2,$3,$4) \
         on conflict (workspace, channel) do update set private = excluded.private",
    )
    .bind(ws).bind(channel).bind(private).bind(by).execute(pool).await?;
    Ok(())
}

pub async fn channel_member_add(pool: &PgPool, ws: &str, channel: &str, account: &str, display: &str) -> sqlx::Result<()> {
    sqlx::query(
        "insert into channel_member (workspace, channel, account, display) values ($1,$2,$3,$4) \
         on conflict (workspace, channel, account) do update set display = excluded.display",
    )
    .bind(ws).bind(channel).bind(account).bind(display).execute(pool).await?;
    Ok(())
}

pub async fn channel_member_remove(pool: &PgPool, ws: &str, channel: &str, account: &str) -> sqlx::Result<()> {
    sqlx::query("delete from channel_member where workspace=$1 and channel=$2 and account=$3")
        .bind(ws).bind(channel).bind(account).execute(pool).await?;
    Ok(())
}

pub async fn channel_members(pool: &PgPool, ws: &str, channel: &str) -> sqlx::Result<Vec<(String, String)>> {
    sqlx::query_as::<_, (String, String)>("select account, display from channel_member where workspace=$1 and channel=$2 order by display")
        .bind(ws).bind(channel).fetch_all(pool).await
}

pub async fn channel_is_member(pool: &PgPool, ws: &str, channel: &str, account: &str) -> sqlx::Result<bool> {
    let n: i64 = sqlx::query_scalar("select count(*) from channel_member where workspace=$1 and channel=$2 and account=$3")
        .bind(ws).bind(channel).bind(account).fetch_one(pool).await?;
    Ok(n > 0)
}

/// Names of all private channels in a workspace (for listing/search filtering).
pub async fn private_channels(pool: &PgPool, ws: &str) -> sqlx::Result<Vec<String>> {
    sqlx::query_scalar::<_, String>("select channel from channel_acl where workspace=$1 and private = true")
        .bind(ws).fetch_all(pool).await
}

/// Private channels a given account is a member of (to compute visibility).
pub async fn my_private_channels(pool: &PgPool, ws: &str, account: &str) -> sqlx::Result<Vec<String>> {
    sqlx::query_scalar::<_, String>(
        "select cm.channel from channel_member cm join channel_acl ca \
         on ca.workspace=cm.workspace and ca.channel=cm.channel \
         where cm.workspace=$1 and cm.account=$2 and ca.private = true",
    )
    .bind(ws).bind(account).fetch_all(pool).await
}

// --- Tier 3: availability (time clock) --------------------------------------

pub async fn availability_set(pool: &PgPool, ws: &str, account: &str, display: &str, on: bool) -> sqlx::Result<()> {
    if on {
        sqlx::query(
            "insert into availability (workspace, account, display) values ($1,$2,$3) \
             on conflict (workspace, account) do nothing",
        )
        .bind(ws).bind(account).bind(display).execute(pool).await?;
    } else {
        sqlx::query("delete from availability where workspace=$1 and account=$2")
            .bind(ws).bind(account).execute(pool).await?;
    }
    Ok(())
}

/// Everyone on the clock in a workspace: (account, display, since unix seconds).
pub async fn availability_list(pool: &PgPool, ws: &str) -> sqlx::Result<Vec<(String, String, i64)>> {
    sqlx::query_as::<_, (String, String, i64)>(
        "select account, display, extract(epoch from since)::bigint from availability where workspace=$1 order by since",
    )
    .bind(ws).fetch_all(pool).await
}

pub async fn availability_is_on(pool: &PgPool, ws: &str, account: &str) -> sqlx::Result<bool> {
    let n: i64 = sqlx::query_scalar("select count(*) from availability where workspace=$1 and account=$2")
        .bind(ws).bind(account).fetch_one(pool).await?;
    Ok(n > 0)
}

// --- Tier 3: notifications + keyword subscriptions --------------------------

#[allow(clippy::too_many_arguments)]
pub async fn notify_insert(pool: &PgPool, ws: &str, recipient: &str, kind: &str, channel: &str, issue_key: &str, actor: &str, snippet: &str) -> sqlx::Result<()> {
    sqlx::query(
        "insert into notification (workspace, recipient, kind, channel, issue_key, actor, snippet) \
         values ($1,$2,$3,$4,$5,$6,$7)",
    )
    .bind(ws).bind(recipient).bind(kind).bind(channel).bind(issue_key).bind(actor).bind(snippet)
    .execute(pool).await?;
    Ok(())
}

/// A user's recent notifications: (id, kind, channel, issue_key, actor, snippet, read, ts).
pub async fn notifications_list(pool: &PgPool, ws: &str, recipient: &str, limit: i64) -> sqlx::Result<Vec<(i64, String, String, String, String, String, bool, i64)>> {
    sqlx::query_as::<_, (i64, String, String, String, String, String, bool, i64)>(
        "select id, kind, channel, issue_key, actor, snippet, read, extract(epoch from created_at)::bigint \
         from notification where workspace=$1 and recipient=$2 order by id desc limit $3",
    )
    .bind(ws).bind(recipient).bind(limit).fetch_all(pool).await
}

pub async fn notifications_unread(pool: &PgPool, ws: &str, recipient: &str) -> sqlx::Result<i64> {
    sqlx::query_scalar("select count(*) from notification where workspace=$1 and recipient=$2 and read=false")
        .bind(ws).bind(recipient).fetch_one(pool).await
}

pub async fn notifications_mark_read(pool: &PgPool, ws: &str, recipient: &str, id: Option<i64>) -> sqlx::Result<()> {
    match id {
        Some(i) => {
            sqlx::query("update notification set read=true where workspace=$1 and recipient=$2 and id=$3")
                .bind(ws).bind(recipient).bind(i).execute(pool).await?;
        }
        None => {
            sqlx::query("update notification set read=true where workspace=$1 and recipient=$2 and read=false")
                .bind(ws).bind(recipient).execute(pool).await?;
        }
    }
    Ok(())
}

pub async fn keyword_set(pool: &PgPool, ws: &str, account: &str, keyword: &str, on: bool) -> sqlx::Result<()> {
    if on {
        sqlx::query("insert into keyword_sub (workspace, account, keyword) values ($1,$2,$3) on conflict do nothing")
            .bind(ws).bind(account).bind(keyword).execute(pool).await?;
    } else {
        sqlx::query("delete from keyword_sub where workspace=$1 and account=$2 and keyword=$3")
            .bind(ws).bind(account).bind(keyword).execute(pool).await?;
    }
    Ok(())
}

pub async fn keywords_list(pool: &PgPool, ws: &str, account: &str) -> sqlx::Result<Vec<String>> {
    sqlx::query_scalar::<_, String>("select keyword from keyword_sub where workspace=$1 and account=$2 order by keyword")
        .bind(ws).bind(account).fetch_all(pool).await
}

/// All (account, keyword) subscriptions in a workspace (for matching on send).
pub async fn keyword_subs_all(pool: &PgPool, ws: &str) -> sqlx::Result<Vec<(String, String)>> {
    sqlx::query_as::<_, (String, String)>("select account, keyword from keyword_sub where workspace=$1")
        .bind(ws).fetch_all(pool).await
}

// --- Tier 3: scheduled messages + reminders ---------------------------------

/// One due/undue scheduled item.
#[derive(sqlx::FromRow)]
pub struct ScheduledRow {
    pub id: i64,
    pub workspace: String,
    pub author_account: String,
    pub author_display: String,
    pub channel: String,
    pub body: String,
    pub kind: String,
    pub due_at: i64, // unix seconds
    pub tries: i32,
}

#[allow(clippy::too_many_arguments)]
pub async fn scheduled_insert(pool: &PgPool, ws: &str, account: &str, display: &str, channel: &str, body: &str, kind: &str, due_at: i64) -> sqlx::Result<i64> {
    sqlx::query_scalar::<_, i64>(
        "insert into scheduled (workspace, author_account, author_display, channel, body, kind, due_at) \
         values ($1,$2,$3,$4,$5,$6, to_timestamp($7)) returning id",
    )
    .bind(ws).bind(account).bind(display).bind(channel).bind(body).bind(kind).bind(due_at)
    .fetch_one(pool).await
}

/// Due, not-yet-done items (across all workspaces) for the scheduler to process.
pub async fn scheduled_due(pool: &PgPool, limit: i64) -> sqlx::Result<Vec<ScheduledRow>> {
    sqlx::query_as::<_, ScheduledRow>(
        "select id, workspace, author_account, author_display, channel, body, kind, \
         extract(epoch from due_at)::bigint as due_at, tries \
         from scheduled where done=false and due_at <= now() order by due_at limit $1",
    )
    .bind(limit).fetch_all(pool).await
}

pub async fn scheduled_mark_done(pool: &PgPool, id: i64) -> sqlx::Result<()> {
    sqlx::query("update scheduled set done=true where id=$1").bind(id).execute(pool).await?;
    Ok(())
}

pub async fn scheduled_bump(pool: &PgPool, id: i64) -> sqlx::Result<()> {
    sqlx::query("update scheduled set tries=tries+1 where id=$1").bind(id).execute(pool).await?;
    Ok(())
}

/// A user's upcoming (not-done) scheduled items: (id, channel, body, kind, due).
pub async fn scheduled_list(pool: &PgPool, ws: &str, account: &str) -> sqlx::Result<Vec<(i64, String, String, String, i64)>> {
    sqlx::query_as::<_, (i64, String, String, String, i64)>(
        "select id, channel, body, kind, extract(epoch from due_at)::bigint \
         from scheduled where workspace=$1 and author_account=$2 and done=false order by due_at",
    )
    .bind(ws).bind(account).fetch_all(pool).await
}

/// Cancel (mark done) a user's own scheduled item.
pub async fn scheduled_cancel(pool: &PgPool, ws: &str, account: &str, id: i64) -> sqlx::Result<()> {
    sqlx::query("update scheduled set done=true where id=$3 and workspace=$1 and author_account=$2")
        .bind(ws).bind(account).bind(id).execute(pool).await?;
    Ok(())
}

// --- Tier 3: incoming webhooks ----------------------------------------------

pub async fn webhook_create(pool: &PgPool, token: &str, ws: &str, channel: &str, label: &str, by: &str) -> sqlx::Result<()> {
    sqlx::query("insert into webhook (token, workspace, channel, label, created_by) values ($1,$2,$3,$4,$5)")
        .bind(token).bind(ws).bind(channel).bind(label).bind(by).execute(pool).await?;
    Ok(())
}

/// Resolve a webhook token to (workspace, channel, label).
pub async fn webhook_lookup(pool: &PgPool, token: &str) -> sqlx::Result<Option<(String, String, String)>> {
    sqlx::query_as::<_, (String, String, String)>("select workspace, channel, label from webhook where token=$1")
        .bind(token).fetch_optional(pool).await
}

pub async fn webhook_list(pool: &PgPool, ws: &str, channel: &str) -> sqlx::Result<Vec<(String, String, String)>> {
    sqlx::query_as::<_, (String, String, String)>("select token, label, created_by from webhook where workspace=$1 and channel=$2 order by created_at")
        .bind(ws).bind(channel).fetch_all(pool).await
}

pub async fn webhook_delete(pool: &PgPool, ws: &str, token: &str) -> sqlx::Result<()> {
    sqlx::query("delete from webhook where workspace=$1 and token=$2").bind(ws).bind(token).execute(pool).await?;
    Ok(())
}

// --- Tier 4: encrypted DM vault ---------------------------------------------

/// (public_key, secret_key) for an owner, if a keypair exists.
pub async fn vault_key_get(pool: &PgPool, owner: &str) -> sqlx::Result<Option<(String, String)>> {
    sqlx::query_as::<_, (String, String)>("select public_key, secret_key from vault_key where owner=$1")
        .bind(owner).fetch_optional(pool).await
}

pub async fn vault_key_put(pool: &PgPool, owner: &str, public_key: &str, secret_key: &str) -> sqlx::Result<()> {
    sqlx::query("insert into vault_key (owner, public_key, secret_key) values ($1,$2,$3) on conflict (owner) do nothing")
        .bind(owner).bind(public_key).bind(secret_key).execute(pool).await?;
    Ok(())
}

/// Get-or-create a DM conversation id for a sorted member set.
pub async fn dm_convo_upsert(pool: &PgPool, ws: &str, members: &str, title: &str) -> sqlx::Result<i64> {
    sqlx::query_scalar::<_, i64>(
        "insert into dm_convo (workspace, members, title) values ($1,$2,$3) \
         on conflict (workspace, members) do update set title = excluded.title returning id",
    )
    .bind(ws).bind(members).bind(title).fetch_one(pool).await
}

pub async fn dm_member_put(pool: &PgPool, convo_id: i64, account: &str, display: &str) -> sqlx::Result<()> {
    sqlx::query(
        "insert into dm_member (convo_id, account, display) values ($1,$2,$3) \
         on conflict (convo_id, account) do update set display = excluded.display",
    )
    .bind(convo_id).bind(account).bind(display).execute(pool).await?;
    Ok(())
}

pub async fn dm_members(pool: &PgPool, convo_id: i64) -> sqlx::Result<Vec<(String, String)>> {
    sqlx::query_as::<_, (String, String)>("select account, display from dm_member where convo_id=$1 order by display")
        .bind(convo_id).fetch_all(pool).await
}

pub async fn dm_is_member(pool: &PgPool, convo_id: i64, account: &str) -> sqlx::Result<bool> {
    let n: i64 = sqlx::query_scalar("select count(*) from dm_member where convo_id=$1 and account=$2")
        .bind(convo_id).bind(account).fetch_one(pool).await?;
    Ok(n > 0)
}

/// Conversations an account is in: (id, members, title, last activity unix secs).
pub async fn dm_convos_for(pool: &PgPool, ws: &str, account: &str) -> sqlx::Result<Vec<(i64, String, String, i64)>> {
    sqlx::query_as::<_, (i64, String, String, i64)>(
        "select c.id, c.members, c.title, \
         coalesce((select extract(epoch from max(created_at))::bigint from dm_message m where m.convo_id=c.id), \
                  extract(epoch from c.created_at)::bigint) as last_ts \
         from dm_convo c join dm_member mem on mem.convo_id = c.id \
         where c.workspace=$1 and mem.account=$2 order by last_ts desc",
    )
    .bind(ws).bind(account).fetch_all(pool).await
}

pub async fn dm_message_insert(pool: &PgPool, convo_id: i64, sender: &str, sender_display: &str, ciphertext: &str) -> sqlx::Result<()> {
    sqlx::query("insert into dm_message (convo_id, sender, sender_display, ciphertext) values ($1,$2,$3,$4)")
        .bind(convo_id).bind(sender).bind(sender_display).bind(ciphertext).execute(pool).await?;
    Ok(())
}

/// Messages in a conversation: (sender, sender_display, ciphertext, ts).
pub async fn dm_messages(pool: &PgPool, convo_id: i64) -> sqlx::Result<Vec<(String, String, String, i64)>> {
    sqlx::query_as::<_, (String, String, String, i64)>(
        "select sender, sender_display, ciphertext, extract(epoch from created_at)::bigint \
         from dm_message where convo_id=$1 order by id",
    )
    .bind(convo_id).fetch_all(pool).await
}

// --- Tier 4: bot users ------------------------------------------------------

pub async fn bot_create(pool: &PgPool, token: &str, ws: &str, name: &str, by: &str) -> sqlx::Result<()> {
    sqlx::query("insert into bot (token, workspace, name, created_by) values ($1,$2,$3,$4)")
        .bind(token).bind(ws).bind(name).bind(by).execute(pool).await?;
    Ok(())
}

/// Resolve a bot token to (workspace, name).
pub async fn bot_lookup(pool: &PgPool, token: &str) -> sqlx::Result<Option<(String, String)>> {
    sqlx::query_as::<_, (String, String)>("select workspace, name from bot where token=$1")
        .bind(token).fetch_optional(pool).await
}

pub async fn bot_list(pool: &PgPool, ws: &str) -> sqlx::Result<Vec<(String, String, String)>> {
    sqlx::query_as::<_, (String, String, String)>("select token, name, created_by from bot where workspace=$1 order by created_at")
        .bind(ws).fetch_all(pool).await
}

pub async fn bot_delete(pool: &PgPool, ws: &str, token: &str) -> sqlx::Result<()> {
    sqlx::query("delete from bot where workspace=$1 and token=$2").bind(ws).bind(token).execute(pool).await?;
    Ok(())
}

// --- Tier 4: interactive message actions ------------------------------------

pub async fn actions_clear(pool: &PgPool, ws: &str, issue: &str) -> sqlx::Result<()> {
    sqlx::query("delete from message_action where workspace=$1 and issue_key=$2").bind(ws).bind(issue).execute(pool).await?;
    Ok(())
}

pub async fn action_add(pool: &PgPool, ws: &str, issue: &str, idx: i32, label: &str, kind: &str, arg: &str) -> sqlx::Result<()> {
    sqlx::query("insert into message_action (workspace, issue_key, idx, label, kind, arg) values ($1,$2,$3,$4,$5,$6)")
        .bind(ws).bind(issue).bind(idx).bind(label).bind(kind).bind(arg).execute(pool).await?;
    Ok(())
}

/// Action buttons for every conversation in a channel: (issue, idx, label).
pub async fn actions_in_channel(pool: &PgPool, ws: &str, channel: &str, subtree: bool) -> sqlx::Result<Vec<(String, i32, String)>> {
    let sql = format!(
        "select a.issue_key, a.idx, a.label from message_action a \
         where a.workspace = $1 and a.issue_key in ( \
            select distinct issue_key from message where workspace = $1 and {pred} ) \
         order by a.issue_key, a.idx",
        pred = channel_pred(subtree)
    );
    sqlx::query_as::<_, (String, i32, String)>(&sql).bind(ws).bind(channel).fetch_all(pool).await
}

/// Resolve one action button to (kind, arg).
pub async fn action_get(pool: &PgPool, ws: &str, issue: &str, idx: i32) -> sqlx::Result<Option<(String, String)>> {
    sqlx::query_as::<_, (String, String)>("select kind, arg from message_action where workspace=$1 and issue_key=$2 and idx=$3")
        .bind(ws).bind(issue).bind(idx).fetch_optional(pool).await
}

// --- Tier 4: Workflow Builder -----------------------------------------------

#[allow(clippy::too_many_arguments)]
pub async fn workflow_create(pool: &PgPool, ws: &str, channel: &str, contains: &str, kind: &str, arg: &str, by: &str) -> sqlx::Result<i64> {
    sqlx::query_scalar::<_, i64>(
        "insert into workflow (workspace, channel, contains, action_kind, action_arg, created_by) \
         values ($1,$2,$3,$4,$5,$6) returning id",
    )
    .bind(ws).bind(channel).bind(contains).bind(kind).bind(arg).bind(by).fetch_one(pool).await
}

/// All rules in a workspace: (id, channel, contains, kind, arg, enabled, created_by).
pub async fn workflows_list(pool: &PgPool, ws: &str) -> sqlx::Result<Vec<(i64, String, String, String, String, bool, String)>> {
    sqlx::query_as::<_, (i64, String, String, String, String, bool, String)>(
        "select id, channel, contains, action_kind, action_arg, enabled, created_by from workflow where workspace=$1 order by id",
    )
    .bind(ws).fetch_all(pool).await
}

/// Enabled rules for matching on a new message: (id, channel, contains, kind, arg).
pub async fn workflows_enabled(pool: &PgPool, ws: &str) -> sqlx::Result<Vec<(i64, String, String, String, String)>> {
    sqlx::query_as::<_, (i64, String, String, String, String)>(
        "select id, channel, contains, action_kind, action_arg from workflow where workspace=$1 and enabled=true",
    )
    .bind(ws).fetch_all(pool).await
}

pub async fn workflow_delete(pool: &PgPool, ws: &str, id: i64) -> sqlx::Result<()> {
    sqlx::query("delete from workflow where workspace=$1 and id=$2").bind(ws).bind(id).execute(pool).await?;
    Ok(())
}

pub async fn workflow_toggle(pool: &PgPool, ws: &str, id: i64, enabled: bool) -> sqlx::Result<()> {
    sqlx::query("update workflow set enabled=$3 where workspace=$1 and id=$2").bind(ws).bind(id).bind(enabled).execute(pool).await?;
    Ok(())
}
