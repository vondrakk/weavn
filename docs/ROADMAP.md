# Weavn Roadmap — Slack feature parity, ranked by difficulty

A complete inventory of Slack's features, each stack-ranked by how hard it is to
reproduce in Weavn. The ranking is **specific to Weavn's architecture**: JIRA is the
system of record (message → issue, reply → comment, channel → board), with a Postgres
projection and a per-user JIRA token at the gateway (see `DESIGN.md`).

The key lens: **difficulty is a function of how well a feature maps to "everything is
a JIRA record."** A few consequences that make this ranking different from a generic
chat clone:

- **Some features are *easier* in Weavn than in Slack**, because JIRA already does
  them: audit trail, retention/eDiscovery, status & workflow lifecycle, dashboards,
  permissions, and "project tracking" (Slack's newest feature, Lists, is literally
  what JIRA is).
- **A few are *harder*** than in Slack because the record model fights them: DMs
  (privacy at odds with a shared JIRA), and anything real-time or interactive that
  JIRA has no primitive for (Block Kit, the app platform).
- **Real-time audio/video is out of scope** (the founding brief), so it's listed but
  parked at the bottom.

## Difficulty scale

| Score | Meaning |
|---|---|
| **1 — Trivial** | Client-only, or already native in JIRA; little/no backend work |
| **2 — Easy** | Small gateway + projection work on top of what exists |
| **3 — Medium** | Real integration work: a JIRA API surface, projection logic, or real-time plumbing |
| **4 — Hard** | A new subsystem (crypto, a mini platform, media handling, cross-org) |
| **5 — Very Hard / out of scope** | Real-time A/V, or a full third-party app platform |

**Status:** ✅ built · ◑ designed · ○ to do

---

## The stack rank

### Tier 1 — Trivial (ship now; mostly done or client-only)

| Feature | Weavn mapping | Diff | Status |
|---|---|---|---|
| Public channels | Board over a filter (native channel) | 1 | ✅ |
| Threads / replies | Comments on the conversation-issue | 1 | ✅ |
| Post a message | A new issue (summary + description) | 1 | ✅ |
| Code blocks / inline code | ADF `codeBlock` / `code` mark | 1 | ✅ |
| **Audit log** | **Native in JIRA** — every change is already recorded | 1 | ✅ (free) |
| Message permalinks | The issue/comment URL | 1 | ✅ |
| Dark mode / themes | Client CSS tokens | 1 | ✅ (client) |
| User profiles (name, title, avatar) | The JIRA user | 1 | ✅ |
| Time zones | JIRA user / client | 1 | ✅ |
| Quick switcher (jump to) | Client over the channel list | 1 | ✅ |
| Channel topic / description | Board / project description | 1 | ✅ |
| Drafts | Client (localStorage) / projection | 1 | ✅ |
| Emoji picker | Client | 1 | ✅ |
| **Project tracking / "Lists"** | **This *is* JIRA** — issues with fields | 1 | ✅ (that's the point) |

### Tier 2 — Easy (small gateway/projection work)

| Feature | Weavn mapping | Diff | Status |
|---|---|---|---|
| Hierarchical channel sections | Channel path tree (sidebar grouping) | 2 | ✅ |
| Discover channels / channel browser | `/discover` over projects & boards | 2 | ✅ |
| Backfill existing issues as conversations | `/backfill` → projection | 2 | ✅ |
| Message editing | Edit the issue description (JIRA keeps the changelog) | 2 | ✅ |
| Message deletion | Soft delete — redact the body; JIRA keeps the prior version | 2 | ✅ |
| Pinned messages | Shared `pinned` projection row + `weavn_pinned` label | 2 | ✅ |
| Saved items / bookmarks | Per-user projection row | 2 | ✅ |
| Starred / favorite channels | Per-user projection row | 2 | ✅ |
| Archive a channel | Close the epic/board; dim in the sidebar | 2 | ✅ |
| Custom status (emoji + text) | Status, optional JIRA user property | 2 | ✅ |
| Slash field commands (`/assign`, `/points`, `/status`…) | Direct issue field updates | 2 | ✅ |
| Workspace analytics / dashboards | **JIRA dashboards & JQL reports** | 2 | ✅ (JIRA-native) |
| Broadcast a thread reply to channel | A comment flagged to surface | 2 | ✅ |
| Do not disturb / snooze | Per-user projection setting | 2 | ✅ |

### Tier 3 — Medium (real integration or real-time work)

| Feature | Weavn mapping | Diff | Status |
|---|---|---|---|
| Rich text (full mrkdwn ⇆ ADF fidelity) | The `weavn-format` converter, full mark set | 3 | ✅ |
| Reactions (emoji) | Per-user `reaction` projection rows, aggregated | 3 | ✅ |
| Custom emoji (+ restricted-use) | `emoji` registry with an allowlist | 3 | ✅ (registry) |
| Mentions (`@user`, `@here`, `@channel`) | JIRA `@mention` by accountId (identity mapping) | 3 | ✅ (`@user`) |
| Unread / read state | Per-user last-read marker in the projection | 3 | ✅ |
| Typing indicators | Ephemeral, over websockets | 3 | ✅ |
| Live presence (active/idle) | Ephemeral, from the websocket connection | 3 | ✅ |
| **Availability (clock-in/out "time clock")** | Projection `availability`; peer-facing, live | 3 | ✅ |
| Live message delivery (fan-out) | Gateway websocket hub + broadcast | 3 | ✅ |
| File uploads / inline media | Native JIRA attachments + auth download proxy | 3 | ✅ |
| Message search (full-text) | Projection FTS (tsvector + GIN); JQL fallback | 3 | ✅ |
| Private channels | Gateway-enforced membership allowlist (ACL) | 3 | ✅ |
| SSO (Okta/OIDC) + JIRA-token login | Gateway OIDC; JIRA identity = Weavn identity | 3 | ✅ JIRA login · ◑ Okta (needs IdP — see INTEGRATIONS_BACKLOG) |
| SCIM provisioning / deprovisioning | Standard SCIM → JIRA account mapping | 3 | ○ (spec'd — INTEGRATIONS_BACKLOG) |
| Permissions & roles | Channel membership ACL (Weavn) + JIRA project roles | 3 | ✅ (channel ACL) |
| Notifications (in-app, email, keywords) | In-app + keyword alerts, live via hub (email = infra) | 3 | ✅ (in-app) |
| Scheduled messages / send later | In-process scheduler (sends via live session) | 3 | ✅ |
| Reminders (`/remind`, Slackbot) | Same scheduler → a self-notification | 3 | ✅ |
| Link unfurling / previews | Server OG fetcher with SSRF guards + client cache | 3 | ✅ |
| Incoming / outgoing webhooks | Gateway webhook endpoints | 3 | ✅ incoming · ○ outgoing (spec'd — INTEGRATIONS_BACKLOG) |
| Retention & eDiscovery / export | Channel transcript export (MD/JSON) + **JIRA retention** | 3 | ✅ (export) |
| Shortcuts (global / message actions) | Client + gateway actions on issues | 3 | ✅ (message actions) |
| Sprints as a first-class concept | List board sprints + assign a conversation (Agile API) | 3 | ✅ |
| Guest accounts | Scoped JIRA accounts / restricted sessions | 3 | ○ (spec'd — INTEGRATIONS_BACKLOG) |

### Tier 4 — Hard (a whole new subsystem)

| Feature | Weavn mapping | Diff | Status |
|---|---|---|---|
| **Direct messages / group DMs** | **Encrypted vault** (age, per-participant + org escrow) — Postgres-only, never JIRA | 4 | ✅ |
| Block Kit / interactive messages (buttons, modals) | No JIRA primitive; a bespoke interaction layer | 4 | ○ |
| Workflow Builder (no-code automations) | A workflow engine over JIRA transitions/fields | 4 | ○ |
| Bots / bot users | Service identities + an events/API surface | 4 | ○ |
| Custom app platform (OAuth, scopes, app directory) | A third-party developer platform | 4 | ○ |
| Canvas (collaborative docs) | A real-time collaborative doc editor | 4 | ○ |
| Slack Connect (cross-org shared channels) | Federation across two JIRA sites + trust | 4 | ○ |
| Enterprise Grid (multi-workspace org) | Multi-tenant org model over many JIRA sites | 4 | ○ |
| Rich media (GIF/Giphy, large video) | Media pipeline + CDN | 4 | ○ |
| Push notifications (mobile/desktop) | APNs/FCM + a mobile client | 4 | ○ |
| Enterprise Key Management (EKM) | Customer-managed keys over the vault | 4 | ○ |

### Tier 5 — Very Hard / out of scope (real-time A/V)

| Feature | Why | Diff | Status |
|---|---|---|---|
| Voice & video calls | WebRTC media stack — explicitly out of scope | 5 | ⛔ |
| Huddles | Persistent audio rooms + WebRTC | 5 | ⛔ |
| Screen sharing | Real-time media capture/transport | 5 | ⛔ |
| Clips (recorded audio/video messages) | Media capture, storage, transcription | 5 | ⛔ |

---

## Weavn's structural advantages (cheaper here than in Slack)

Because the record *is* a JIRA issue, several "hard in a chat app" features are
nearly free or JIRA-native:

- **Audit trail** — every edit/transition is already in JIRA's changelog.
- **Retention, legal hold, eDiscovery, export** — JIRA's compliance tooling.
- **Status & lifecycle** — conversations have Open/In-Progress/Done for free,
  now surfaced as a status chip in the chat client with one-click workflow
  transitions (the JIRA workflow drives the available moves). ✅
- **Dashboards & reporting** — JQL and JIRA dashboards over the conversation data.
- **Permissions** — reuse JIRA's project/issue-security model.
- **Project tracking** — Slack's flagship new feature (Lists) is table stakes here.

## The structural tax (harder here than in Slack)

- **DMs** — a shared system of record is the wrong place for private 1:1s, so
  they live in an **age-encrypted vault in Postgres and never become JIRA
  issues** (Tier 4, shipped): each message is encrypted to every participant
  plus an org-escrow key, so participants read it, outsiders can't, and the org
  can recover for compliance. (Prototype stores keypairs at rest; production
  wraps the secret keys under a KMS.)
- **Private channels** — shipped as a gateway-enforced membership ACL. Because
  native-channel issues still live in the shared storage project, this is access
  control at the Weavn layer, not JIRA issue-level security; a JIRA admin can
  still see the underlying issues. For hard isolation, back a private channel with
  a restricted JIRA project (project-browse permission), which Weavn inherits.
- **Interactivity (Block Kit) and the app platform** — JIRA has no equivalent of
  Slack's interactive message/app model; these are green-field subsystems.
- **Real-time A/V** — no overlap with the JIRA model at all; out of scope.

## Build order implication

The cheapest, highest-value parity sits in Tiers 1–3 and is largely what Weavn is
already executing (channels, threads, discovery, backfill, persistence, slash
refinement, and now live updates). DMs and the app/interaction platform (Tier 4) are
the big deliberate investments. Tier 5 stays out of scope. See `DESIGN.md` §13–§14
for the capabilities already committed.
