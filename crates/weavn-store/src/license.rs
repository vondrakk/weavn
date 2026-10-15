//! Licensing & seat metering (docs/LICENSING.md).
//!
//! Model: METER, NEVER DENY. A seat is a distinct user who has signed in to a
//! JIRA namespace (`workspace`). Licenses are time-bounded and seat-count based;
//! usage over entitlement is reported for right-sizing, never enforced. Seats are
//! reclaimable (quits / new hires), with a `seat_event` audit trail.
//!
//! Times are read via `::text` casts and counts as `bigint`, so no extra sqlx
//! features (chrono/time) are needed.

use crate::Pool;

/// Default free seats per JIRA namespace when there is no current license.
/// Overridable per workspace via the `config` key `license.free_seats`.
pub const FREE_SEATS: i64 = 10;

/// Result of metering a sign-in: usage vs entitlement after the claim.
#[derive(Debug, Clone)]
pub struct SeatClaim {
    pub newly_claimed: bool, // first sign-in, or a reactivation of a released seat
    pub active_seats: i64,   // active seats in the namespace after this claim
    pub entitled: i64,       // current entitlement (license seats or free)
    pub over: i64,           // max(0, active_seats - entitled); 0 when within entitlement
    pub licensed: bool,
}

/// Meter a sign-in: upsert the seat, bump `last_seen`, reactivate a released user,
/// and log the lifecycle event. Never denies. Returns current usage vs entitlement.
pub async fn seat_claim(
    pool: &Pool,
    ws: &str,
    user_id: &str,
    display: &str,
    email: Option<&str>,
) -> sqlx::Result<SeatClaim> {
    let prior: Option<bool> =
        sqlx::query_scalar::<_, bool>("select active from seat where workspace = $1 and user_id = $2")
            .bind(ws)
            .bind(user_id)
            .fetch_optional(pool)
            .await?;
    let newly = !matches!(prior, Some(true));

    sqlx::query(
        "insert into seat (workspace, user_id, display, email, active, first_seen, last_seen, released_at) \
         values ($1, $2, $3, $4, true, now(), now(), null) \
         on conflict (workspace, user_id) do update set \
           display = excluded.display, email = excluded.email, \
           active = true, last_seen = now(), released_at = null",
    )
    .bind(ws)
    .bind(user_id)
    .bind(display)
    .bind(email)
    .execute(pool)
    .await?;

    if newly {
        let ev = if prior.is_none() { "claimed" } else { "reactivated" };
        sqlx::query("insert into seat_event (workspace, user_id, event) values ($1, $2, $3)")
            .bind(ws)
            .bind(user_id)
            .bind(ev)
            .execute(pool)
            .await?;
    }

    let active_seats = seat_active_count(pool, ws).await?;
    let ent = entitlement(pool, ws).await?;
    Ok(SeatClaim {
        newly_claimed: newly,
        active_seats,
        entitled: ent.entitled,
        over: (active_seats - ent.entitled).max(0),
        licensed: ent.licensed,
    })
}

/// Distinct active users in the namespace.
pub async fn seat_active_count(pool: &Pool, ws: &str) -> sqlx::Result<i64> {
    sqlx::query_scalar::<_, i64>("select count(*) from seat where workspace = $1 and active")
        .bind(ws)
        .fetch_one(pool)
        .await
}

/// Offboard a user: free their seat (so it can be reused) and log it.
pub async fn seat_release(pool: &Pool, ws: &str, user_id: &str) -> sqlx::Result<bool> {
    let n = sqlx::query(
        "update seat set active = false, released_at = now() \
         where workspace = $1 and user_id = $2 and active",
    )
    .bind(ws)
    .bind(user_id)
    .execute(pool)
    .await?
    .rows_affected();
    if n > 0 {
        sqlx::query("insert into seat_event (workspace, user_id, event) values ($1, $2, 'released')")
            .bind(ws)
            .bind(user_id)
            .execute(pool)
            .await?;
    }
    Ok(n > 0)
}

#[derive(Debug, Clone)]
pub struct SeatRow {
    pub user_id: String,
    pub display: String,
    pub email: Option<String>,
    pub active: bool,
    pub first_seen: String,
    pub last_seen: String,
}

pub async fn seats(pool: &Pool, ws: &str) -> sqlx::Result<Vec<SeatRow>> {
    let rows = sqlx::query_as::<_, (String, String, Option<String>, bool, String, String)>(
        "select user_id, display, email, active, first_seen::text, last_seen::text \
         from seat where workspace = $1 order by active desc, last_seen desc",
    )
    .bind(ws)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(user_id, display, email, active, first_seen, last_seen)| SeatRow {
            user_id,
            display,
            email,
            active,
            first_seen,
            last_seen,
        })
        .collect())
}

#[derive(Debug, Clone)]
pub struct License {
    pub plan: String,
    pub seats: i64,
    pub starts_at: String,
    pub ends_at: Option<String>,
    pub status: String,
    pub note: Option<String>,
}

pub async fn license_get(pool: &Pool, ws: &str) -> sqlx::Result<Option<License>> {
    let row = sqlx::query_as::<_, (String, i32, String, Option<String>, String, Option<String>)>(
        "select plan, seats, starts_at::text, ends_at::text, status, note \
         from license where workspace = $1",
    )
    .bind(ws)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|(plan, seats, starts_at, ends_at, status, note)| License {
        plan,
        seats: seats as i64,
        starts_at,
        ends_at,
        status,
        note,
    }))
}

/// A license is current when active and today is within [starts_at, ends_at].
pub async fn license_is_current(pool: &Pool, ws: &str) -> sqlx::Result<bool> {
    sqlx::query_scalar::<_, bool>(
        "select exists(select 1 from license where workspace = $1 and status = 'active' \
         and starts_at <= current_date and (ends_at is null or ends_at >= current_date))",
    )
    .bind(ws)
    .fetch_one(pool)
    .await
}

/// Create or update a license. `starts_at`/`ends_at` are ISO dates (YYYY-MM-DD);
/// a null `starts_at` defaults to today, a null `ends_at` is open-ended.
pub async fn license_set(
    pool: &Pool,
    ws: &str,
    seats: i64,
    starts_at: Option<&str>,
    ends_at: Option<&str>,
    plan: Option<&str>,
    note: Option<&str>,
) -> sqlx::Result<()> {
    sqlx::query(
        "insert into license (workspace, plan, seats, starts_at, ends_at, note, updated_at) \
         values ($1, coalesce($5, 'team'), $2, coalesce($3::date, current_date), $4::date, $6, now()) \
         on conflict (workspace) do update set \
           seats = excluded.seats, plan = excluded.plan, starts_at = excluded.starts_at, \
           ends_at = excluded.ends_at, note = excluded.note, status = 'active', updated_at = now()",
    )
    .bind(ws)
    .bind(seats as i32)
    .bind(starts_at)
    .bind(ends_at)
    .bind(plan)
    .bind(note)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn license_clear(pool: &Pool, ws: &str) -> sqlx::Result<bool> {
    Ok(sqlx::query("delete from license where workspace = $1")
        .bind(ws)
        .execute(pool)
        .await?
        .rows_affected()
        > 0)
}

#[derive(Debug, Clone)]
pub struct Entitlement {
    pub entitled: i64,
    pub licensed: bool,
    pub source: &'static str, // "license" | "free"
    pub ends_at: Option<String>,
}

/// Free-seat allowance for a namespace (config override, else FREE_SEATS).
pub async fn free_seats(pool: &Pool, ws: &str) -> sqlx::Result<i64> {
    let v = crate::config_get(pool, ws, "license.free_seats").await?;
    Ok(v.and_then(|s| s.parse::<i64>().ok()).unwrap_or(FREE_SEATS))
}

/// The seats a namespace is entitled to right now (current license, else free).
pub async fn entitlement(pool: &Pool, ws: &str) -> sqlx::Result<Entitlement> {
    if license_is_current(pool, ws).await? {
        if let Some(l) = license_get(pool, ws).await? {
            return Ok(Entitlement {
                entitled: l.seats,
                licensed: true,
                source: "license",
                ends_at: l.ends_at,
            });
        }
    }
    Ok(Entitlement {
        entitled: free_seats(pool, ws).await?,
        licensed: false,
        source: "free",
        ends_at: None,
    })
}

/// The right-sizing view: active seats vs entitlement, with the overage.
#[derive(Debug, Clone)]
pub struct UsageReport {
    pub active_seats: i64,
    pub total_seats: i64, // distinct users ever seen (active + released)
    pub entitled: i64,
    pub over: i64,
    pub licensed: bool,
    pub source: &'static str,
    pub plan: Option<String>,
    pub starts_at: Option<String>,
    pub ends_at: Option<String>,
    pub status: Option<String>,
}

/// Fleet view: every namespace that has a license or any seats, with its usage
/// report. Admin-only (N+1 queries, fine for ops). For right-sizing at a glance.
pub async fn fleet(pool: &Pool) -> sqlx::Result<Vec<(String, UsageReport)>> {
    let wss: Vec<String> = sqlx::query_scalar::<_, String>(
        "select workspace from seat union select workspace from license order by 1",
    )
    .fetch_all(pool)
    .await?;
    let mut out = Vec::with_capacity(wss.len());
    for ws in wss {
        let r = usage_report(pool, &ws).await?;
        out.push((ws, r));
    }
    Ok(out)
}

/// A seat lifecycle event (claimed / reactivated / released).
#[derive(Debug, Clone)]
pub struct SeatEvent {
    pub user_id: String,
    pub event: String,
    pub at: String,
    pub note: Option<String>,
}

/// Recent seat lifecycle history for a namespace (audit trail of assignments).
pub async fn seat_history(pool: &Pool, ws: &str) -> sqlx::Result<Vec<SeatEvent>> {
    let rows = sqlx::query_as::<_, (String, String, String, Option<String>)>(
        "select user_id, event, at::text, note from seat_event \
         where workspace = $1 order by at desc limit 200",
    )
    .bind(ws)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(user_id, event, at, note)| SeatEvent { user_id, event, at, note })
        .collect())
}

pub async fn usage_report(pool: &Pool, ws: &str) -> sqlx::Result<UsageReport> {
    let active_seats = seat_active_count(pool, ws).await?;
    let total_seats = sqlx::query_scalar::<_, i64>("select count(*) from seat where workspace = $1")
        .bind(ws)
        .fetch_one(pool)
        .await?;
    let ent = entitlement(pool, ws).await?;
    let lic = license_get(pool, ws).await?;
    Ok(UsageReport {
        active_seats,
        total_seats,
        entitled: ent.entitled,
        over: (active_seats - ent.entitled).max(0),
        licensed: ent.licensed,
        source: ent.source,
        plan: lic.as_ref().map(|l| l.plan.clone()),
        starts_at: lic.as_ref().map(|l| l.starts_at.clone()),
        ends_at: lic.as_ref().and_then(|l| l.ends_at.clone()),
        status: lic.as_ref().map(|l| l.status.clone()),
    })
}
