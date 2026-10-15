# Weavn — Licensing & seat metering

**Principle: meter, never deny.** Weavn does not block a customer when they grow
past their licensed seats. It records usage, surfaces the overage, and lets the
customer **right-size the license later**. Service is never interrupted.

## Unit of licensing: the JIRA namespace

A license is scoped to a **JIRA namespace URL** — the JIRA site base (e.g.
`https://acme.atlassian.net`). This is the same `workspace` key used everywhere
else in the store, so licensing keys off data we already have.

## Seats

A **seat** is a distinct user who has **signed in** to a JIRA namespace. Identity
is the stable `Session.subject` (JIRA accountId, or OIDC subject) — not a display
name, so renames don't create phantom seats.

- **Free tier:** `10` seats per namespace with no license. Overridable per
  namespace via the `config` key `license.free_seats`.
- **Lifecycle (reassignable over time):** seats are reclaimable. Sign-in claims or
  reactivates a seat; offboarding (`seat release`) marks it inactive and frees it.
  A re-hire reactivates. Every transition is written to `seat_event` so seat
  assignments are auditable over time (quits, new hires, reassignment).
- **Active seats** = distinct users currently `active` in the namespace. This is
  the number compared against the entitlement.

## Licenses

Time-bounded and seat-count based, one per namespace:

| field | meaning |
|---|---|
| `seats` | entitled seats (right-sized over time) |
| `starts_at` / `ends_at` | validity window (dates); `ends_at` null = open-ended |
| `status` | `active` \| `suspended` |
| `plan`, `note` | informational |

A license is **current** when `status='active'` and today ∈ `[starts_at, ends_at]`.
**Entitlement** = the current license's `seats` if current, else `free_seats`.

## Enforcement = reporting (no denial)

The gateway never returns "license required". It only **meters**: on sign-in it
calls `seat_claim(...)`, which upserts the seat and returns current usage vs
entitlement (`active_seats`, `entitled`, `over`). The overage is what Sales/ops use
to right-size — surfaced via the admin CLI today, and later an in-app banner / an
account dashboard / a billing export.

### Exception: the hosted free-tier hard cap

"Never deny" holds for **licensed (commercial)** namespaces — paying customers are
metered and warned, never interrupted. The **hosted free tier is the one exception**:
it is a hard cap of 10 seats. On an *unlicensed* namespace, `seat_claim_enforced(...)`
refuses the 11th *distinct* user at sign-in (HTTP 403 `workspace_full`) and creates no
seat; the existing 10 keep working, and a user who already holds an active seat always
re-admits. A commercial license lifts the cap and reverts the namespace to warn-only.
Admission + claim run in one transaction serialized by a per-workspace advisory lock.
See `docs/MULTITENANCY.md`.

## Data model (`0005_licensing.sql`)

- `seat(workspace, user_id, display, email, active, first_seen, last_seen, released_at)` — PK (workspace,user_id)
- `seat_event(id, workspace, user_id, event, at, note)` — `claimed|reactivated|released`
- `license(workspace, plan, seats, starts_at, ends_at, status, note, updated_at)` — PK workspace

Times are `timestamptz`/`date`; Rust reads them via `::text` casts and counts as
`bigint`, so the store needs no extra sqlx features (`chrono`/`time`).

## Store API (`weavn-store`, module `license`)

- `seat_claim(pool, ws, user_id, display, email) -> SeatClaim` — meter a sign-in (idempotent, never denies).
- `seat_claim_enforced(pool, ws, user_id, display, email) -> Admission` — meter with the hosted free-tier hard cap (`Admitted | Full`).
- `seat_release(pool, ws, user_id)` / `seat_active_count` / `seats(...)`
- `license_get` / `license_set` / `license_clear` / `license_is_current`
- `entitlement(pool, ws)` and `usage_report(pool, ws)` — the right-sizing view.

## Admin CLI (`weavndb`)

```
weavndb license grant  <ws> <seats> [starts YYYY-MM-DD|-] [ends|-] [plan] [note]
weavndb license show   <ws>        # entitlement vs active seats + overage
weavndb license revoke <ws>
weavndb seat   list    <ws>
weavndb seat   release <ws> <user_id>
```

## Gateway integration (metering hook)

One call, on successful login (JIRA-token and OIDC paths), after the session is
created — fire-and-meter, failures logged not fatal:

```rust
// in auth_jira / auth_callback handlers (have State<Shared> -> s.pool + session)
if let Some(sess) = s.auth.session(&sid) {
    let _ = weavn_store::seat_claim(
        &s.pool, &sess.site.clone().unwrap_or_default(),
        &sess.subject, &sess.author(), sess.email.as_deref()).await;
}
```

No change to `send`/`reply` — writes are never gated. (If in-app overage banners
are wanted later, `usage_report` feeds `/auth/me`.)

## Later

- Self-serve purchase + Stripe reconciliation against metered overage.
- Signed, offline-verifiable license keys (Ed25519) for self-hosted / air-gapped.
- Automatic seat reclamation for users inactive beyond N days.
