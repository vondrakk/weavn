# weaveN — Atlassian Marketplace listing & the OAuth 2.0 (3LO) app

_As of 2026-10-15._

How weaveN ships as a **single, distributable OAuth 2.0 (3LO) app** — the production
auth that replaces the dev-only email+API-token login — and how that app is listed on
the Atlassian Marketplace as a **third-party integration**. Design only; agree it
before implementation (per `CLAUDE.md`). Companion to `docs/DESIGN.md` §2 (Identity &
SSO) and `docs/MULTITENANCY.md`.

## The model in one paragraph

weaveN is not a Connect or Forge app — nothing installs **inside** Jira. It's an
external SaaS that calls Jira's REST API on each user's behalf, so on the Marketplace
it is a **"third-party integration"** ("My app isn't directly installable"), the
lighter listing path. The one non-negotiable rule behind this whole document:
Atlassian's security requirements for cloud apps say an app **must not collect or
store Atlassian account credentials — user passwords or user API tokens.** Today's
`/auth/jira` email+token login (`auth.rs`, a deliberate "stand-in for Okta while SSO
isn't set up") does exactly that, and is a **dev stopgap only**. The OAuth 2.0 (3LO)
app is built **specifically to retire the PAT**: one app weaveN owns, each user
consents to it once, and weaveN holds short-lived OAuth tokens — never the user's
API token. Building 3LO and listing on the Marketplace are therefore the same
project, approached from two ends.

## Why 3LO, not the PAT (the hard rule)

Atlassian's [Security requirements](https://developer.atlassian.com/platform/marketplace/security-requirements/)
for cloud apps, applied to any OAuth/3LO integration:

- **No account credentials.** Apps "must not collect or store credentials belonging
  to Atlassian user accounts such as user passwords or user API tokens." Apps that
  collect API tokens, or that tell each customer to stand up their own 3LO app, are
  called out as **non-compliant**. The correct shape is **one** distributable 3LO app
  that all customers consent to.
- **Enforcement is coming.** Atlassian has announced it is moving Marketplace apps
  off users' Atlassian account API tokens; this has been widely reported as an
  **end-of-Q2-2026** cutoff for apps that still collect/send/store them. Treat the
  exact date as _to be confirmed_ against the live
  [Marketplace security enforcement policy](https://developer.atlassian.com/platform/marketplace/marketplace-security-enforcement-policy/),
  but plan as if the PAT path cannot ship to production.
- **Secrets & transport.** Client secret + access + refresh tokens stored encrypted,
  never in logs or source; TLS 1.2+ with HSTS ≥ 1 year; strict tenant isolation
  (one tenant can never reach another's data — already our posture, see
  `docs/MULTITENANCY.md`).
- **Least privilege.** Request only the scopes the product actually uses.
- **State validation.** Unique, user-bound `state` on every authorize request,
  validated on callback (we already do PKCE+CSRF for OIDC in `auth.rs`).

**Good news for attribution:** 3LO _preserves_ "each user's own writes go out as
them." Today a user's own API token writes to their JIRA (`JiraCreds` in `auth.rs`,
`RealBackend::from_parts(...)`); under 3LO each user's own OAuth consent does the
same. We swap **per-user API tokens** for **per-user OAuth refresh tokens** — same
guarantee, compliant mechanism.

## How 3LO folds into the existing design

`docs/DESIGN.md` §2 separates two identity layers; 3LO **collapses them into one
consent**:

| Layer | Today (dev) | Under 3LO (live) |
| --- | --- | --- |
| Person → weaveN (who are you?) | Okta/OIDC _or_ email+token | OAuth consent identifies the user (`/myself` via `api.atlassian.com`); Okta/OIDC stays optional for enterprise person-layer SSO |
| weaveN → JIRA (write as you) | user's API token (Basic auth) | user's OAuth **access token** (Bearer), refreshed from a stored refresh token |

Concrete code deltas (for the implementing session — **design only here**):

- **API host changes.** 3LO calls do **not** hit the customer's `*.atlassian.net`
  base with Basic auth. They go to `https://api.atlassian.com/ex/jira/{cloudid}/…`
  with `Authorization: Bearer <access_token>`. `RealBackend::from_parts(base, email,
  token, bearer)` grows a 3LO mode: base = `api.atlassian.com/ex/jira/{cloudid}`,
  no email, token = access token, bearer = true. The existing `bearer` path is close;
  the new work is the `{cloudid}` prefix and token lifecycle.
- **`cloudid` discovery.** After consent, call
  `GET https://api.atlassian.com/oauth/token/accessible-resources` to get the sites
  (and their `cloudid`s) this token can reach. Map that to our `workspace` key.
- **`JiraCreds` → an OAuth credential.** Replace `{base, email, token, bearer}` with
  `{cloudid, access_token, refresh_token, expires_at, scopes}`, stored encrypted
  per-user (the projection store / `vault` already exists for secrets).
- **Token refresh.** Request `offline_access` to get refresh tokens. On 401/expiry,
  exchange the refresh token for a new access token — and **replace the stored
  refresh token with the one that comes back** (rotating refresh tokens: expire after
  90 days idle, 10-minute reuse window).
- **Login route.** `/auth/jira` (email+token POST) is replaced by an authorize →
  callback pair, mirroring the OIDC flow already in `auth.rs` (`/auth/login` +
  `/auth/callback` with CSRF `state`).

### Multi-tenancy interaction

`docs/MULTITENANCY.md` pins each `<subdomain>.weavn.io` to exactly one JIRA site, 1:1
and immutable. 3LO is compatible but needs one guard: an **account-level** grant lets
a user consent to _several_ sites, and `accessible-resources` returns all of them.
weaveN must **filter to the pinned subdomain's `cloudid`** so a tenant host still
can't reach another tenant's site — the same invariant the current per-site-token pin
enforces. (Resource-level grant scopes consent to selected sites instead; see the
decision below.)

### Admin approval — not required by default, but gated by org policy

A distributable 3LO app is **not installed into a site** like a Connect/Forge app.
It is authorized **per user**: each person hits the consent screen and grants it
themselves, and the app can only ever do what _that user_ can do (consent ≤ the
user's own JIRA permissions — no privilege elevation). So **by default no Jira admin
is involved** — any user self-connects.

The exception is the customer org's **Connected apps / app-access policy**. An org or
site admin can enable **"Block user apps"** (Atlassian Admin → Connected apps), and
orgs with **Atlassian Guard** can add data-security policies on top. When that's on,
users **cannot** self-consent — they get:

> _"Your site admin must authorize this app for the site `<instance>.atlassian.net`
> before the app can access your account."_

and a **site/org admin must authorize weaveN first**. This is common in
security-conscious enterprises (e.g. a locked-down `domaintools.atlassian.net` would
likely need an admin there to allow the app before anyone can connect). A site **you
administer yourself** has no such blocker.

**What the gateway's consent UX must do:** detect the admin-authorization error on the
callback (the authorize request comes back denied / the token exchange fails with this
condition) and, instead of a generic failure, show onboarding guidance — _"your
Atlassian admin needs to approve weaveN for this site"_ — with a link to the app's
authorization URL an admin can approve, and ideally a way to notify/forward to an
admin. Treat admin-gated orgs as a **first-class onboarding path**, not an error.
Getting the **Marketplace listing approved** materially helps here: admins are far
more willing to allowlist an Atlassian-reviewed app than an unreviewed one.

## The OAuth 2.0 (3LO) app — developer-console setup

This is **not** a shell script: the app is created in Atlassian's browser developer
console against your Atlassian login, and a created 3LO app cannot be cleanly deleted
and reinstalled, so it's an outward-facing action you take yourself. Ordered checklist
(you drive the browser; I don't log into your account):

1. **Developer console** → <https://developer.atlassian.com/console/myapps/> →
   **Create** → **OAuth 2.0 integration**. Name it `weaveN` (branding rules below).
2. **Authorization** → next to **OAuth 2.0 (3LO)** click **Configure** → set the
   **Callback URL**. For the hosted service this is the gateway's OAuth callback —
   e.g. `https://app.weavn.io/auth/callback` (and, if per-subdomain callbacks are
   needed, register each or use the apex as the single redirect and route internally).
   Keep the redirect-URI allowlist tight.
3. **Permissions** → **Add** the **Jira API** → **Configure** → add the granular
   scopes weaveN actually uses (candidate set below), plus `offline_access` for
   refresh tokens. Nothing more — least privilege is a review criterion.
4. **Settings** → **Client ID** and **Client secret**. These are already issued and
   stored locally in **`~/projects/innovai8/.conf/weavn-gateway-secret`** (dotenv,
   mode 0600, outside the repo — never committed) as:
   - `WEAVN_CLIENT_ID` — the 3LO app's Client ID
   - `WEAVN_SECRET` — the 3LO app's Client secret

   In prod the same pair lives in the in-cluster k8s Secret **`weavn-gateway-secret`**
   (same name, different home). **Naming gotcha for the gateway wiring:** these are the
   **Jira 3LO** app creds and are distinct from the Okta person-layer OIDC vars
   (`WEAVN_OIDC_CLIENT_ID` / `WEAVN_OIDC_CLIENT_SECRET`) in `auth.rs` — don't conflate
   the two credential sets.
5. **Distribution** → toggle sharing **on**. 3LO apps are **private by default**
   (only you can install) until this is flipped.
6. Verify end-to-end before any listing: run the authorize → callback → token →
   `accessible-resources` → one Jira read against a test site. A public Marketplace
   listing is **not** required to use the app — you can hand testers the authorization
   URL directly; the only cost of no listing is an "app has not yet been reviewed by
   Atlassian" warning on consent.

> Follow-up tooling: once the app exists, a `scripts/oauth.sh` (no-arg: exchange the
> stored refresh token, hit `accessible-resources`, print the sites read back; a flag
> to run the full authorize round-trip) fits the repo's script convention and gives a
> read-only health check. The Client ID/secret now exist
> (`~/projects/innovai8/.conf/weavn-gateway-secret`), so the script is buildable — but
> running it still needs a **refresh token from a real user-consent round-trip** (3LO
> has no client-credentials grant), so it can't be exercised until the gateway's
> authorize/callback flow exists.

### Candidate scopes (confirm exact granular names in the console)

weaveN's gateway today reads/creates issues and comments, transitions issues, toggles
labels (pin), searches users, lists projects, reads `/myself`, and fetches
attachments. That maps to roughly:

- `read:jira-work` — issues, comments, projects (read)
- `write:jira-work` — create/update issues & comments, transitions, labels
- `read:jira-user` — `/myself`, user search
- `offline_access` — refresh tokens

Prefer the **granular** scopes (e.g. `read:issue:jira`, `write:comment:jira`,
`read:project:jira`, `read:user:jira`, `read:attachment:jira`, …) over the coarse
classic scopes above — granular is what least-privilege review expects. Enumerate the
exact strings in the console against the operations in `crates/weavn/src/jira.rs` and
the gateway handlers, and request only those.

## The Marketplace listing (once 3LO is live)

Prerequisite: an **Atlassian Marketplace Partner account** (vendor profile; partner
identity/business verification; a designated security contact on
ecosystem.atlassian.net; incident reporting via ECOHELP). Then:

1. **Publish a new app** in the partner dashboard →
   choose **"My app isn't directly installable"** (this is the third-party-integration
   path, not a Connect/Forge upload).
2. Required fields:
   - **App Key** — unique, **immutable** once set (pick the final reverse-DNS-style
     key deliberately, e.g. `io.weavn.app`).
   - **Version** + **Build number** (build number orders versions).
   - An **info URL** customers can use to learn about / access the integration.
   - **Name** — branding rules: must **not** start with an Atlassian product name or
     imply Atlassian affiliation.
   - **Compatible application** — Jira Cloud.
3. Listing assets & tabs: **Privacy & Security** tab (data handling, what's stored —
   for us: control-plane keeps only email/display name/subdomain/usage per
   `docs/MULTITENANCY.md`; conversation content stays in the customer's own JIRA),
   support + documentation links, an EULA, logo, screenshots, and a demo video.
4. **Review:** every publicly listed app needs Atlassian approval. The Marketplace
   team needs **access to a test instance** of weaveN (a seeded tenant + test JIRA
   site) to exercise setup and the OAuth consent flow — provide this up front to
   speed review. (Docs don't commit to a fixed duration; the general Marketplace
   listing guidance cites ~5–10 business days for review to begin.)

## Open decisions

1. **Grant type — account-level vs resource-level.** Account-level (one consent spans
   the user's sites; we filter to the pinned `cloudid`) vs resource-level (consent
   scoped to selected sites). _Recommendation:_ account-level — smoother multi-site
   future, and the subdomain pin already constrains which site a host may use.
2. **Callback topology.** Single apex callback (`app.weavn.io/auth/callback`) with
   internal routing to the originating subdomain, vs a registered callback per
   subdomain. Apex-single is simpler to register; needs `state` to carry the target
   subdomain.
3. **Keep OIDC/Okta?** 3LO can be the sole auth (it both identifies the person and
   authorizes JIRA writes). Keep the Okta/OIDC path only as optional enterprise
   person-layer SSO, or retire it? _Recommendation:_ keep it wired but off, as now.
4. **PAT sunset.** Confirm the exact Atlassian enforcement date and set an internal
   deadline to remove `/auth/jira` email+token from production builds (dev/demo may
   keep it behind a flag).

## Not yet wired

- The 3LO app itself (developer console) — **you** create it (checklist above).
- Gateway OAuth flow: authorize/callback routes, token store + rotation, `cloudid`
  routing through `api.atlassian.com`, and the **admin-approval onboarding path**
  (detect "site admin must authorize" and guide, don't just error) — the implementing
  session owns this.
- Marketplace Partner account + listing assets (logo, screenshots, demo video, EULA,
  privacy/security copy).
- `scripts/oauth.sh` health-check (deferred until a Client ID exists).

## Sources

- [Creating a Marketplace listing](https://developer.atlassian.com/platform/marketplace/creating-a-marketplace-listing/)
- [OAuth 2.0 (3LO) apps](https://developer.atlassian.com/cloud/jira/software/oauth-2-3lo-apps/)
- [Listing a third-party integration on the Atlassian Marketplace](https://developer.atlassian.com/platform/marketplace/knowledge-base/listing-a-third-party-integration-on-the-atlassian-marketplace)
- [Security requirements for cloud apps](https://developer.atlassian.com/platform/marketplace/security-requirements/)
- [Marketplace security enforcement policy](https://developer.atlassian.com/platform/marketplace/marketplace-security-enforcement-policy/)
