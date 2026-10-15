# weaveN — Multi-tenancy & subdomain provisioning

How the hosted service turns "one JIRA site" into "one weaveN workspace at
`<subdomain>.weavn.io`", provisioned on first login, metered by seat, and kept to
a strict minimum of captured account data.

## The model in one paragraph

Each **tenant** is one JIRA site. Its public home is `<subdomain>.weavn.io`, where
the **subdomain is the JIRA site's own DNS label** — `domaintools.atlassian.net`
becomes `domaintools.weavn.io`. The mapping is 1:1 and immutable; there are no
vanity names and therefore no collisions or squatting. The first person to sign in
on a subdomain **provisions** it (free plan). Everything a tenant stores is already
keyed by `workspace` = the JIRA base URL, so the subdomain is just a public alias
for that key.

## Subdomain = JIRA site label

Derivation is `tenant::subdomain_for_base()`: take the first host label of the JIRA
base, lowercase it, reject reserved labels (`app`, `www`, `api`, …) and anything
that isn't a valid DNS label. The gateway never lets a user pick it, so the
subdomain can't drift from the JIRA site it represents.

A browser on `domaintools.weavn.io` is **pinned** to the `domaintools` JIRA site:
the login form there sends no site, and the gateway forces
`https://domaintools.atlassian.net`. A token for a different site simply fails JIRA
auth, so a tenant host can't be used to reach another tenant's JIRA. Session cookies
are host-scoped (no parent `Domain`), so a session on one subdomain is never sent to
another.

### Entry points

- **`<slug>.weavn.io`** — the real login (email + API token). Provisions on first
  success, sets the session cookie on that host.
- **`app.weavn.io` (apex)** — a "find your workspace" redirector: enter the JIRA
  site, get sent to `<slug>.weavn.io/`. It never authenticates (so no cross-subdomain
  cookie is needed), and `POST /auth/jira` at the apex is refused.

`GET /auth/config` tells the client which mode it's in
(`{multitenant, base_domain, subdomain}`), driven by the `WEAVN_BASE_DOMAIN` env.
Unset = single-tenant dev: the login form takes the JIRA site as before.

## Licensing: free caps at 10, commercial lifts it

Built on the existing seat metering (`docs/LICENSING.md`; `workspace` = JIRA base):

- **Free tier — a hard cap of 10 seats.** `seat_claim_enforced()` refuses the 11th
  *distinct* user at sign-in with HTTP 403 `workspace_full`; no seat is created and
  the existing 10 keep working. A user who already holds an active seat always
  re-admits (a re-login is never blocked). Admission + claim run in one transaction
  serialized by a per-workspace advisory lock, so two simultaneous "11th" sign-ins
  can't both slip past. The cap is `FREE_SEATS` (10), overridable per workspace via
  the `config` key `license.free_seats`.
- **Commercial — the cap is lifted, usage is warned.** A current `license` row makes
  the tenant "licensed": sign-ins are never blocked, and usage over the entitled seat
  count is reported (`usage.over`) and shown as a banner, never enforced. This keeps
  the "meter, never deny" principle for paying customers; the hard cap is the
  free-tier exception.

Grant/right-size a license with the `weavndb` CLI (see `LICENSING.md`). A tenant's
`plan` label (`free`/`commercial`) is refreshed on each login to mirror the license.

## Privacy: what the control plane stores

The hosted service's **account data** is deliberately minimal. Only two places hold
it, both keyed by workspace/subdomain:

- `tenant(subdomain, jira_base, founding_email, founding_name, plan, created_at, last_active_at)`
- `seat(...)` / `seat_event(...)` / `login_event(subdomain, user_id, email, at)` — seats and the login-activity log.

That is: **email, display name, the subdomain, and login/usage activity** — nothing
more. We do **not** put conversation content in the control plane.

Conversation content lives in the **per-tenant projection** (`message`, `reaction`,
`attachment`, `dm_*`, …), every row already keyed by `workspace`. That projection is
a **cache of the customer's own JIRA** — their data, isolated per tenant, not part of
the account record. DMs remain the age-encrypted vault (`docs/DESIGN.md`), Postgres
only, never JIRA. Isolation is enforced by the subdomain resolving to exactly one
`workspace` and every query scoping by it.

## Gateway env

| Env | Meaning |
|---|---|
| `WEAVN_BASE_DOMAIN` | e.g. `weavn.io`. Set = multi-tenant (subdomain routing on). Unset = single-tenant dev. |
| `WEAVN_JIRA_MULTITENANT` | `1` to accept any `*.atlassian.net` at login (required for self-serve provisioning). |
_(There is no project-default env — see below.)_

## A channel *is* a board (channel → project)

A channel is a JIRA project/board; you can't post without one (as in Slack you can't
type with no channel selected). So a new post's target is resolved **only** from the
channel, with **no default fallback**:

- **The channel's first segment must name a project the poster can access** — `ENG`,
  or `ENG/backend` → project `ENG`, labelled `eng_backend`. Access is verified per-key
  against JIRA (`project_key_if_accessible`), so *any* project the user can reach
  works, not just the first 50.
- **Otherwise the post is refused** (HTTP 409): "that isn't a board you can post to —
  pick a project." Never a silent landing in some default project.

Reading, `/discover`, and `/backfill` are unaffected — they already span every project
the user can access. This governs only *creating* new conversations. Bot/webhook posts
(service account) trust a project-key-shaped channel segment.

## Verify it

`./scripts/tenancy.sh` (read-only) boots a throwaway multi-tenant gateway against the
local dev Postgres + the JIRA in `scripts/jira.env`, and checks: apex vs tenant
`/auth/config`, apex login refusal, Host-pinned login + provisioning, the session
cookie, and the control-plane rows written. The free-tier hard cap is covered by the
`enforced_free_cap_blocks_then_license_lifts` integration test in `weavn-store`.

## Not yet wired (infra)

Serving real `*.weavn.io` traffic needs, in `deploy/`:
1. A wildcard **DNS** record `*.weavn.io` → the ingress LB (add to `dns.sh --ensure`).
2. A wildcard **TLS** cert `*.weavn.io` via a cert-manager **DNS-01** issuer (DigitalOcean
   solver) — HTTP-01 can't do wildcards — and an ingress for host `*.weavn.io`.
3. Gateway config `WEAVN_BASE_DOMAIN=weavn.io` + `WEAVN_JIRA_MULTITENANT=1`, then
   `app.sh --all` (needs `scripts/gitlab-deploy.env`).
