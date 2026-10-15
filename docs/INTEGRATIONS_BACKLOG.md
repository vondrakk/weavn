# Integrations backlog — the infra-gated Tier-3 items

Everything else in `ROADMAP.md` Tiers 1–3 is built and verified. These four
remain **not built** because each needs external infrastructure/credentials or a
product/security decision that can't be made or verified from the prototype
environment. This file is the precise spec for finishing each when that's ready.

---

## 1. SSO — Okta / OIDC login

**State:** flow is scaffolded in the gateway (`openidconnect` crate; routes
`/auth/login` and `/auth/callback`, gated on env). Not verifiable without an IdP.

**To wire:**
- Register a web app (Authorization Code + PKCE) in Okta (or any OIDC IdP).
- Redirect URI: `https://<host>/auth/callback`.
- Scopes: `openid profile email`.
- Env for the gateway: `WEAVN_OIDC_ISSUER` (e.g. `https://<org>.okta.com/oauth2/default`),
  `WEAVN_OIDC_CLIENT_ID`, `WEAVN_OIDC_CLIENT_SECRET`.

**Open issue — writes:** OIDC authenticates the *person*, but posting to JIRA as
them needs a JIRA token, which an OIDC login doesn't provide. Two paths:
(a) after SSO, still collect a JIRA API token per user (SSO for identity, token
for writes); or (b) use **Atlassian OAuth 2.0 (3LO)** so each user grants Weavn a
per-user JIRA token — the cleaner long-term answer, and its own work item.

---

## 2. SCIM provisioning / deprovisioning

**State:** not built. Needs an IdP to drive it and a decision on semantics.

**To wire:** implement SCIM 2.0 under `/scim/v2/` — `Users` and `Groups`
(GET/POST/PATCH/DELETE), authenticated by a bearer token the IdP presents.

**Decision needed:** JIRA already *is* the user directory, so in Weavn "provision"
shouldn't create JIRA users. It should grant/revoke **workspace access** and
**default channel membership** (reusing the `channel_member` ACL). Deprovision =
revoke access + clear memberships. Define that mapping before building, or SCIM
will duplicate JIRA's directory.

---

## 3. Guest accounts

**State:** not built. This is a **new auth surface**, so build it deliberately.

**Model (recommended):** a channel member mints a **guest invite token** scoped to
named channels. The guest "logs in" with the token (no JIRA account) and gets a
session restricted to those channels. Because a guest has no JIRA identity:
- **Read-only** is the safe default (view the invited channels only).
- To let guests *post*, route their messages through the **service account**
  (like incoming webhooks), attributed `Guest: <name>`.

**Must-haves:** tokens are revocable and expiring; the session is hard-scoped to
the invited channels (enforced like `require_channel_access`); rate-limited.
Reuse the webhook token generator (`/dev/urandom`, base64url).

---

## 4. Outgoing webhooks

**State:** not built (incoming webhooks shipped). Needs an egress policy decision.

**Shape:** a per-channel `outgoing_webhook(url, secret)`; on each new message,
POST JSON `{channel, author, text, issue, ts}` to the URL, signed with an HMAC of
the body using `secret` in an `X-Weavn-Signature` header so the receiver can
verify authenticity. Use this to bridge to Slack, trigger CI, etc.

**Security (required):** apply the same SSRF guard as `/unfurl`
(`is_blocked_ip` — reject loopback/private/link-local/CGNAT/ULA) so a configured
URL can't reach internal services; add a timeout, bounded retries with backoff,
and a small delivery log. The deployment must permit outbound egress.

---

## Why these are parked, in one line each

| Item | Blocker |
|---|---|
| Okta/OIDC | needs an IdP tenant + client credentials to verify |
| SCIM | needs an IdP + a decision on what "provision" means over JIRA |
| Guest accounts | a new non-JIRA auth surface; needs a read-only-vs-post decision |
| Outgoing webhooks | an outbound-egress/SSRF policy decision + a receiver to test |
