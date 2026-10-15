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

## Why these are parked, in one line each (Tier 3)

| Item | Blocker |
|---|---|
| Okta/OIDC | needs an IdP tenant + client credentials to verify |
| SCIM | needs an IdP + a decision on what "provision" means over JIRA |
| Guest accounts | a new non-JIRA auth surface; needs a read-only-vs-post decision |
| Outgoing webhooks | an outbound-egress/SSRF policy decision + a receiver to test |

---

# Tier 4 — remaining (need external infra or scale)

Four Tier-4 items shipped: DMs (encrypted vault), bots, interactive messages,
and Workflow Builder. The rest need infrastructure, a real-time editing engine,
a second JIRA site, or a mobile app — none available from the prototype. Specs:

## 5. Custom app platform (OAuth, scopes, app directory)

A third-party developer platform: app registration, OAuth 2.0 authorization-code
with per-scope consent, signed request verification, and an app directory. This
is its own product. Prerequisite: **Atlassian OAuth 2.0 (3LO)** for per-user JIRA
tokens (see item 1) so installed apps can act as the user. Build only with a
dedicated scopes/permission model; until then, **bots + incoming webhooks** cover
the common "let an external system post/act" cases.

## 6. Canvas (collaborative docs)

A real-time collaborative document per channel. Needs a **CRDT/OT engine**
(e.g. Yjs/Automerge) with a sync protocol over the existing WebSocket hub, plus
presence cursors and conflict-free persistence. A non-real-time "channel note"
(one doc stored as an issue, last-writer-wins) is a cheap interim; true Canvas is
a real-time-editing subsystem and should be scoped as one.

## 7. Slack Connect (cross-org shared channels)

A channel shared between two organizations. Needs **two JIRA sites + a federation
/ trust model**: an invite+accept handshake, per-side identity mapping, and a
relay that mirrors each message into both sites' projections (deciding whose JIRA
holds the record). Can't be built or tested with a single site.

## 8. Enterprise Grid (multi-workspace org)

One org spanning many workspaces/JIRA sites with shared identity, org-wide
channels, and central admin. The gateway already keys everything by workspace and
has a `WEAVN_JIRA_MULTITENANT` flag; Grid adds an **org model above workspaces**
(directory, org policies, cross-workspace search/admin) — a multi-tenant control
plane, built when there's a second site to model against.

## 9. Rich media (GIF / Giphy, large video)

File/image attachments already ship. This adds **GIF search** (a Giphy/Tenor API
key + a picker) and a **media pipeline/CDN** for large video (transcode, range
serving) rather than storing big blobs as JIRA attachments. Blocker: an API key
and a decision on a storage/CDN backend.

## 10. Push notifications (mobile / desktop)

In-app notifications already ship. Push adds **APNs/FCM** (and a desktop/service
worker channel) plus device-token registration and a mobile/desktop client to
receive them. Blocker: provider credentials and a client app.

## 11. Enterprise Key Management (EKM)

Customer-managed keys over the DM vault. The vault already encrypts to
participants + an org escrow key; EKM swaps the escrow (and key-wrapping) to a
**customer-controlled KMS/HSM** (e.g. AWS KMS, Vault) with revocation and audit,
so the customer — not the operator — holds the master key. Blocker: a KMS to
integrate and a key-rotation/revocation policy. Builds directly on `weavn::keys`.

## Why these are parked, in one line each (Tier 4)

| Item | Blocker |
|---|---|
| Custom app platform | a whole developer platform + Atlassian 3LO for per-user tokens |
| Canvas | a real-time CRDT/OT editing engine |
| Slack Connect | a second JIRA site + a federation/trust model |
| Enterprise Grid | a multi-site org control plane (needs >1 site to model) |
| Rich media / Giphy | a Giphy/Tenor API key + a media CDN decision |
| Push notifications | APNs/FCM credentials + a mobile/desktop client |
| EKM | a customer KMS/HSM to integrate + a key-rotation policy |

## Tier 5 — out of scope

Voice & video calls, huddles, screen sharing, and clips are real-time A/V
(WebRTC) and were excluded by the founding brief. Not planned.
