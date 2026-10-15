# Weavn — Design

A Slack-like team chat application whose **system of record is JIRA**. Every
conversation is stored, and can be viewed, as JIRA issues and comments. People in
chat see tickets as conversations; people in JIRA see conversations as tickets —
the same data, two faces.

Scope of this pass: text messaging, threads, reactions, mentions, attachments,
channels and DMs, edit/delete, search, read state. Out of scope: huddles, voice/
video, screen share, live audio.

---

## 1. The core decision: what maps to what

JIRA and Slack have different primitives. The whole design hinges on choosing the
granularity at which a chat message becomes a JIRA object. Three candidates were
considered:

| Option | Mapping | Verdict |
|---|---|---|
| A | Channel = issue, message = comment | ❌ A channel becomes one unbounded issue with tens of thousands of comments (JIRA performance degrades, no per-conversation status, threads can't nest). |
| **B** | **Channel = Board (a filtered view), thread = issue, reply = comment** | ✅ **Chosen.** Natural 2-level fit; each conversation gets a lifecycle. (Channel→Board model detailed below.) |
| C | Every message = its own issue, replies = linked issues | ❌ Issue explosion is extreme and a thread becomes an awkward issue-link graph to read. |

### Why Option B is clean

Slack threads are **exactly two levels deep**: a root message, and a flat list of
replies. JIRA is **exactly two levels deep**: an issue, and a flat list of
comments. The shapes are identical, so the mapping needs no nesting tricks.

```
Slack                         JIRA
─────                         ────
Workspace / Instance    ↔     JIRA Site
Channel (identity)      ↔     Stable `weavn_channel` ID, stamped on every issue (indexed custom field)
Channel (view)          ↔     A Kanban Board whose filter is `weavn_channel = <id>`
Channel (storage)       ↔     Company-managed Project(s); sharded for scale, not user-facing
Top-level message       ↔     Issue  (summary = first line, description = body)
Thread reply            ↔     Comment on that Issue
@mention                ↔     Native JIRA @mention (account-to-account)
Attachment              ↔     JIRA attachment on the Issue
Edit                    ↔     Edit of Issue description / Comment (native changelog = edit history)
Delete                  ↔     Soft delete: body redacted + `chat.deleted` property set
Reaction (emoji)        ↔     Issue/Comment **entity property** `chat.reactions`
"Resolved" a thread     ↔     Workflow transition on the Issue (Open → Resolved) = a board column
Channel membership      ↔     Project role / browse permission + filter visibility
Private channel         ↔     Restricted Project browse permission + restricted filter
DM / group DM           ↔     Encrypted "DM Vault": 1 issue per participant-set per time window, messages = comments (ciphertext; see §7)
Pin a message           ↔     Label `pinned` / issue property
Saved / bookmark        ↔     Per-user property (projection DB)
```

A flat channel (most Slack traffic is not threaded) is simply a stream of issues
with zero comments. The channel view is "issues on this channel's board, ordered by
created time"; opening a thread is "open the issue, read its comments." Both read
naturally.

### The payoff: bidirectional truth

Because JIRA is the real store (not an export target), the flow goes **both ways**:

- Someone comments on the ticket **in the JIRA UI** → it appears as a chat reply.
- Someone resolves the conversation **in chat** → the ticket transitions to Resolved in JIRA, visible to project managers, reportable in JQL and dashboards.

This is the headline feature and the reason to build on JIRA at all rather than
treating it as a dumb archive.

### The channel model: storage, identity, view — three layers

A JIRA **board** is not a container; it is a saved **filter + columns** — a lens
over issues that physically live in Projects. That is exactly what a channel is: a
filtered, column-organized stream. So a channel is three decoupled layers:

| Layer | JIRA object | Role | Key |
|---|---|---|---|
| **Storage** | company-managed Project(s) | where issues physically live; sharded for scale/retention; never user-facing | sharding policy |
| **Identity** | indexed custom field `weavn_channel` on every issue | the stable join key | channel **ID** |
| **View** | a Kanban **Board**, filter `weavn_channel = <id>` | the channel the user sees; columns = conversation status | — |

Why this (not channel = Epic): a board *is* a channel; its columns give the
Open/Resolved/Archived lifecycle for free; and because a board filter spans
Projects, **storage can be sharded across many Projects (§8.3) while the channel
still presents as one unified stream.** Decoupling storage from view is what makes
"shard aggressively" and "channel feels coherent" both true. Constraints:
company-managed (classic) Jira Software projects only (team-managed don't do
arbitrary cross-project boards); `weavn_channel` is an **indexed custom field, not a
label** (exact-match stays fast at millions of issues); the board is presentation,
**not** security — privacy still lives at the Project/issue-security layer (§7).

### Two channel types: native and mirror

Because a channel is just *a board over a filter*, the filter can select issues two
different ways — so Weavn supports two channel types in one sidebar:

| Type | Filter | What it is | Who creates it |
|---|---|---|---|
| **Native** | `weavn_channel = <id>` | A chat-first channel; conversations here *create* issues. The default. | A person, by posting to a path |
| **Mirror** | any JQL, e.g. `"Epic Link" = PROJ-42` or `project = OPS AND sprint in openSprints()` | A view bound to pre-existing Jira work; the Epic/sprint/query *is* the channel | Auto-provisioned from Jira, or defined by a saved JQL |

Mirror channels are how the chat-first model absorbs the "mirror Jira structure"
ideas (auto-provision a channel when an Epic is created, auto-archive when it
closes, group the sidebar by sprint) **without** abandoning user-named channels: a
workspace can hold both, and a mirror channel's "messages" are comments on the real
Jira issues its JQL selects. Native is the product's spine; mirror is the bridge to
teams already living in Jira.

**Kanban vs Scrum.** Native channels are backed by a **Kanban** board: continuous
flow matches a chat stream, whereas a Scrum board shows only the active sprint and
hides the rest in a backlog — most of the conversation would vanish from the board.
Native conversations are never sprint-gated. Scrum teams are served by **mirror**
channels: bind the filter to a sprint (`sprint in openSprints()`) or a backlog, and
sprint membership becomes a badge/filter, not a gate. Either way the chat read path
renders from the projection as a time-ordered stream, so the Jira board *type* only
affects how a channel looks when viewed natively in Jira — not the chat experience.
(Consequence: a new issue created in a Scrum project lands in the backlog, not on
the active-sprint board.)

### Channel naming: hierarchical paths, stable IDs

Channel names are human **paths**: `eng / backend / deploys`, `/` separating
segments, spaces allowed inside a segment. Slashes drive grouping and collapsing in
the sidebar. **The hierarchy is projection-only and never touches issues** — issues
carry just the stable `weavn_channel` leaf ID. This is the load-bearing rule: stamping
a full path onto issues (`weavn_channel_path`) would mean a channel move bulk-rewrites
millions of issues, a non-starter at §8.3 scale.

```
channel(id, parent_id, segment, display_name, is_private, board_id, …)   -- a tree
```

- **Typing a path** splits on `/`, trims/normalizes each segment, and resolves or
  creates tree nodes under each parent. Intermediate segments are collapsible
  **folders**; they become real channels only when posted to or created explicitly.
- **Identity on issues** stays `weavn_channel = <leaf id>`.
- **A channel's board** = `weavn_channel = <id>`.
- **A folder/subtree board** (optional) = `weavn_channel IN (<descendant ids>)`, built
  by expanding the tree to an ID set — never by prefix-matching a string. (JQL `IN`
  has a size limit, so very large subtrees are capped or paginated.)
- **Move / rename** = update one row's `parent_id` / `segment`; the board name
  follows; **zero issues touched** (O(1) on the JIRA side).
- **Collapsed state** is per-viewer UI (projection / localStorage).

Canonicalization: split on `/`; trim each segment and collapse internal whitespace;
reject empty segments (`//`, leading/trailing `/`); **fold case + whitespace for
identity within a parent** (`Eng/Backend` ≡ `eng / backend`) while **preserving the
display casing** set at first creation; `/` is always a separator (no literal slash
in a segment in v1); cap depth and segment length. Membership is per-channel in v1
(inheritance from a parent folder is a later option).

---

## 2. Architecture

A direct client→JIRA app is not viable: JIRA's REST API is rate-limited, not
real-time, and too slow for chat read volume. A gateway service sits between the
clients and JIRA, owns the websockets, translates the two models, and keeps a fast
read projection.

```
   ┌────────────┐        websocket / HTTPS        ┌──────────────────────────┐
   │  Clients   │ ───────────────────────────────▶│       Gateway (BFF)       │
   │ web/desktop│ ◀───────────────────────────────│                           │
   └────────────┘      live events (fan-out)       │  • auth / sessions        │
                                                    │  • websocket hub          │
                                                    │  • model translation      │
                                                    │    (mrkdwn ⇆ ADF)         │
                                                    │  • write-through to JIRA  │
                                                    │  • read from projection   │
                                                    └───────┬───────────┬───────┘
                                              REST (writes) │           │ read/write
                                                            ▼           ▼
                                                   ┌──────────────┐  ┌──────────────────┐
                                                   │     JIRA     │  │  Projection store │
                                                   │ (system of   │  │  Postgres + search│
                                                   │   record)    │  │  (fast chat reads)│
                                                   └──────┬───────┘  └──────────────────┘
                                                          │ webhooks (issue/comment
                                                          ▼  created, updated, deleted)
                                                   ┌──────────────┐
                                                   │ Gateway sync │ ──▶ update projection, fan out
                                                   │   consumer   │
                                                   └──────────────┘
```

**Components**

- **Client** — chat UI, holds a websocket. Optimistic rendering. Never talks to JIRA directly.
- **Gateway / BFF** — stateless horizontally-scaled service. Terminates websockets, authenticates, translates chat ops ↔ JIRA ops, does write-through to JIRA, serves reads from the projection, and fans out real-time events.
- **JIRA** — system of record (Cloud or Data Center). Every durable message lives here.
- **Projection store** — Postgres (relational mirror of channels/threads/messages/read-state) plus a search index (Postgres FTS or Elasticsearch/OpenSearch). This is what the chat read path actually hits; JIRA is never on the hot read path.
- **Sync consumer** — subscribes to JIRA webhooks, applies changes to the projection, and fans them out over websockets. This is what makes direct-in-JIRA edits show up in chat.

### Identity & SSO

Enterprise login is **Okta/SSO**, handled at the **gateway** (not Supabase Auth,
§10). Two distinct layers, deliberately separated:

1. **Human → Weavn.** A standard OIDC Authorization-Code-+-PKCE flow
   (`openidconnect`) or SAML; the gateway issues its own session. Well-trodden,
   bounded work.
2. **Weavn → JIRA.** Writes still need a JIRA identity — the §8 decision
   (per-user JIRA accounts for clean attribution, *or* a service account with the
   real author in a `chat.author` property). SSO sits on top of that choice; it
   doesn't remove it.

The **bridge** is the one piece to get right: after login, resolve the person to a
**JIRA `accountId`** (match on email / SSO subject — trivial when JIRA and
Weavn are provisioned from the same Okta org, the normal case). **SCIM**
auto-provision/deprovision is optional and can come later. SSO is table-stakes for
the compliance-driven enterprise buyer §8 targets.

*Prototype status:* wired in the gateway, two ways in, both producing one cookie
session. **Okta/OIDC** (`/auth/login` → Okta → `/auth/callback`). **JIRA-token
login** (`/auth/jira`): POST a JIRA email + token, validated against `/myself`,
binding the session to the JIRA account — *your JIRA identity is your Weavn
identity*, a pragmatic stand-in until Okta is configured. Both are env-gated; when
any is on, the chat routes require a session and attribute each message to the
authenticated user (verified live — a spoofed request author is overridden); when
none is configured the gateway runs open (dev mode).

---

## 3. Write path (send a message)

1. Client sends message over websocket with a client-generated `nonce`.
2. Gateway **optimistically** broadcasts to the channel (message shows instantly, marked "sending", keyed by `nonce`).
3. Gateway converts body mrkdwn → ADF, then:
   - **new top-level message** → `POST /issue` (summary = first line, description = body, reporter = author, Epic link = channel).
   - **thread reply** → `POST /issue/{key}/comment`.
4. JIRA returns the issue key / comment id.
5. Gateway writes the row to the projection and broadcasts the **confirmed** message (with JIRA id, replacing the optimistic one via `nonce`).
6. JIRA webhook fires later; the sync consumer **dedupes** it against the row already written in step 5 (idempotency key = JIRA id), so no double-render.

Failure handling: if step 3 fails (rate limit / outage), the optimistic message flips to "failed — retry", and the write goes to a retry queue with backoff. JIRA is authoritative, so nothing is "sent" until it's in JIRA.

## 4. Read path (open a channel / thread)

Served entirely from the **projection store** — paginated, indexed, millisecond
reads. JIRA is queried only for cold/rarely-touched history not in the projection
(lazy backfill) and for power-search fallbacks (raw JQL). Never paginate a channel
by calling JIRA.

## 5. Real-time & bidirectional sync

- JIRA webhooks (`jira:issue_created`, `jira:issue_updated`, `comment_created/updated/deleted`) → sync consumer → projection update → websocket fan-out.
- Idempotency: every event carries the JIRA object id; the consumer upserts by id, so webhook + write-through convergence never duplicates.
- Ordering: per-channel monotonic sequence assigned by the projection on insert; JIRA `created` timestamp is the tiebreaker/display time. Clients order by sequence.
- This path is also the recovery mechanism: if the gateway misses events, a periodic JQL reconciliation (`updated >= lastSyncedAt`) backfills the projection.

### First connect: discovery, then bounded backfill

When a workspace connects a JIRA site, Weavn starts empty and **auto-discovers**,
running as the connecting user's token (so it only ever sees what that user can):

1. **Structure** — list accessible **projects** (`/project/search`) and **boards**
   (`/rest/agile/1.0/board`, with their scrum/kanban type and sprints). These are
   offered as candidate **mirror channels** (§1).
2. **Field schema** (`/rest/api/3/field`) — auto-detect the per-instance field ids
   that would otherwise be manual config: **Story Points**, **Sprint**, and whether
   a `weavn_channel` field exists. Resolved ids are written to `config` (§10), so
   the admin never has to paste `customfield_10050`.
3. **Seed + pick** — discovered structure lands in `config`/`channel`, and the user
   **chooses** which projects/boards become channels. Discovery is automatic;
   import is **opt-in** — a site with millions of issues is never slurped wholesale.

**Backfill is bounded and incremental, never a blocking full import.** For each
selected channel, pull newest-first by JQL in pages (e.g. `updated >= now-30d`),
enough to render; older history loads lazily on scroll and via a background job.
Then webhooks (above) keep it live. This reuses the reconciliation machinery and
respects the rate limits of §8.

**Native channels vs provisioning.** Read-only discovery and mirror channels work
for any user. *Native* channels need infrastructure — a storage project, the indexed
`weavn_channel` field, boards — whose creation requires a JIRA **admin**; that is a
separate one-time setup step, distinct from a normal user connecting.

---

## 6. Handling the JIRA gaps

Things Slack has that JIRA doesn't natively — and where they live:

- **Reactions** — JIRA has no emoji reactions (native voting is a single 👍 only, too limited). Stored as an **entity property** on the issue/comment: `chat.reactions = { "👍": ["accountId1", ...], "🎉": [...] }`. Entity properties are arbitrary JSON attached to JIRA objects; they don't clutter the JIRA UI and survive as part of the record.
- **Custom emoji** — a per-workspace registry in the operational store (`emoji` table, §10): shortcode (e.g. `:~Y:`), image asset, creator, and an optional **usage allowlist** (which accounts may *apply* it; empty = anyone). A restricted emoji is still visible to everyone once applied — the list only gates who can apply it. The shortcode parser must allow custom characters (e.g. `~`), matching registered shortcodes exactly rather than a fixed `[a-z0-9_]` set.
- **Typing indicators & live presence** — ephemeral (connection-derived); never persisted; gateway/websocket layer only. The fuller presence/status/availability model is below.
- **Read / unread state** — per-user, high-churn, per-channel last-read marker. Lives in the projection store, not JIRA.
- **Rich text** — a bidirectional **mrkdwn ⇆ ADF** (Atlassian Document Format) converter in the gateway handles bold/italic/code blocks/links/lists/mentions/emoji. This is a real, testable module, not an afterthought.
- **Edit history** — free: JIRA's changelog records every edit of a description/comment.
- **Delete** — soft delete (redact body, set `chat.deleted` property) to preserve the audit trail, consistent with "everything is a durable record." Hard delete only for compliance/DSAR flows.

### Presence, status, and availability

Slack conflates "connected" with "around." Weavn separates them into three
layers that differ by intent and persistence — and **none of them is a JIRA
record** (they are operational, per-user and high-churn, the same bucket as typing
and read-state; storing them as issues would explode volume and, for availability,
turn into a surveillance ledger):

| Layer | What it is | Set by | Lives in |
|---|---|---|---|
| **Live presence** | connected / active / idle | auto, from the websocket + client activity | gateway only, ephemeral (not even persisted) |
| **Status** | an emoji + text ("🎧 heads down"), optional auto-expiry | the person | projection; optionally mirrored to a JIRA user property so it shows Jira-side |
| **Availability** | explicit **clock-in / clock-out** — "I'm around" vs "I'm out" | the person, deliberately | projection, as a session log |

**Availability is the "time clock."** Each clock-in opens an `availability_session`;
clock-out closes it; several in and out per day is normal. The team sees the current
state derived from the open session — *"Around since 9:14" · "Away" · "Out"* — with
live presence as a faint secondary hint (the green dot lies when a phone sleeps;
availability is the deliberate signal, so it leads). Quick presets ("out 1h",
"out until tomorrow") auto-close the session. Availability may **auto-clock-out** on
a long disconnect, but never auto-clock-*in* — being around is always a choice.

**Culture guardrail (by design).** This is self-reported availability for *peers to
coordinate*, employee-controlled and visible to teammates — explicitly **not** a
manager timesheet. No hours dashboards, no enforcement, not wired to payroll or
performance. An org that wants timesheet-style auditing could opt into exporting
sessions, but that is a separate, consented feature, off by default; a clock-in/out
log reads as monitoring the moment it is pointed upward, and the default must not.

Projection tables:
```
presence(user_id, conn_state, last_active_at)              -- live, in-memory/ephemeral
status(user_id, emoji, text, clears_at NULL)               -- self-set
availability_session(user_id, started_at, ended_at NULL)   -- clock in/out; open row = around now
```
Status and availability changes fan out over the same websocket stream as messages
(they are live), even though — unlike pure presence — they persist in the
projection. The gateway owns all three; JIRA is not involved.

---

## 7. DMs: the encrypted vault

DMs are the sharpest problem, and naive issue-security doesn't solve it. Jira's
native access control **cannot provide chat-grade DM privacy at scale**, for three
reasons:

1. **Admins bypass issue security.** A site admin can read any issue and rewrite
   the security scheme, so issue-security-level "private" DMs are still readable by
   Jira admins — unacceptable for a DM.
2. **ACLs don't scale to per-pair privacy.** A security level per DM, or a project
   per DM, blows past Jira's practical limits (security levels per scheme, projects
   per site) once there are thousands of pairs. Jira access control is built for
   teams and roles, not N×N private conversations.
3. **Volume.** DMs are usually the majority of messages and get zero ticket value
   (no board, workflow or epic) — the worst case for the §8.3 ceiling with none of
   the upside.

So we keep DMs **as records** (holding the §13 line) but move privacy out of Jira's
hands:

- **Storage shape.** A DM conversation (pair or group) = **one issue per
  participant-set per time window** (e.g. monthly); messages = comments. This
  bounds the issue count (pairs × months, not messages) and the comments-per-issue
  (the reason channel=issue was rejected in §1). All live in one shared **"DM
  Vault"** project — no project/security-level explosion.
- **Encryption.** Message bodies are **app-layer encrypted** before reaching Jira;
  Jira stores ciphertext. Access control lives in the gateway's key service — only
  participants hold content keys — so admins see gibberish.
- **Key model: org-escrow** (chosen). Participants read DMs normally and admins
  can't read them day-to-day, but the org holds an **escrowed recovery key** for
  compliance, eDiscovery, and offboarding. Privacy from casual admin snooping, not
  from a lawful org request. (Not true E2E — the org can recover content when it
  must; that tradeoff is deliberate for an enterprise, Jira-centric product.)
- **What we give up (intentionally).** Jira-side search, the "view the DM as a
  ticket" bidirectional view, and native @mention rendering for DMs. DMs were never
  the conversation-as-auditable-ticket value; channels are. DMs still get the
  record, retention, and recoverable export.

Key management: per-conversation content key, wrapped once per participant (their
device/public key) and once with the org escrow key; stored alongside the vault
issue (wrapped keys only). Rotating a participant out rotates the content key at the
next time-window rollover.

---

## 8. The hard constraints (call these out before building)

1. **JIRA licensing — the dominant cost.** JIRA Cloud bills **per licensed user,
   whether or not they log in**. 2026 list: Free ≤10 users, **Standard
   $7.91/user/mo**, **Premium $14.54/user/mo**, Enterprise custom; Standard falls
   to ~$5–6/user/mo at 1,000+ users on annual billing. A company-wide chat needs a
   seat per sender, so a 500-person org is **~$3K/mo (~$36K/yr) for the storage
   substrate alone** — roughly Slack's own per-seat price, before building or
   hosting any of the chat app. **Decide this first.**
   - *Variant:* a pool of service accounts posts on behalf of users, real author in
     a `chat.author` property. Cuts seats but sacrifices native @mention, per-user
     permissions, and clean attribution. A real tradeoff, not a free win.
2. **Rate limits — dynamic, not a fixed number.** JIRA Cloud runs three limiters at
   once: a per-hour **points quota** (cost-weighted per call), **per-second burst
   limits per endpoint**, and a new **tiered points system for apps enforced from
   2026-03-02**; token/OAuth traffic (our gateway) is governed by the burst limits.
   It is cost-based and dynamic — you design to stay well under a *moving* ceiling,
   not to a published "X/min." A workspace peaking at ~100 msgs/sec (each message =
   1+ writes, more with reactions/edits) would be 429'd. Hence the two
   non-negotiables: **every read from the projection, never JIRA**, and **writes
   queued/coalesced with backoff** behind optimistic UI. This is the main argument
   for the gateway+projection design.
3. **Issue volume — a one-year wall.** Issue-per-message. Cloud has no advertised
   hard cap but degrades early (filters/gadgets optimized ~2,500 issues; enforced
   config ceilings phasing in 2026-09). Data Center has an explicit max-issues
   setting, **recommended ≤18M for performance**. The math: 500 users × ~100
   msgs/day ≈ 50K issues/day ≈ **~18M issues/year** — i.e. Data Center's
   recommended ceiling hit in *one year* at mid-size scale. Forces **partitioning**
   (channels sharded across many projects, time-based project rotation) and
   **archival of cold conversations** to cheap storage with JIRA holding a pointer.
   Makes Data Center / Enterprise the realistic deployment, not a single Standard
   Cloud site.
4. **Latency floor.** JIRA write latency (100s of ms) sets the floor for message
   confirmation. Optimistic UI hides it, but "confirmed" is never instant.

> **Bottom line.** Stacked together these make JIRA-as-store elegant (the
> bidirectional ticket⇄conversation duality is genuinely valuable) but
> economically and operationally heavy. Most defensible for a **smaller,
> high-value, compliance-driven org** that *wants* every conversation to be a
> first-class auditable JIRA record — not as a mass-market Slack replacement.

*Figures: Atlassian list pricing and rate-limit/data-limit docs, current as of
Oct 2026; verify against Atlassian's live pages before committing.*

---

## 9. Issue-volume controls (keeping the issue count down)

Constraint §8.3 is governed by one number in the capacity model: the **thread-root
rate** — the share of messages that start a new topic (an Issue) rather than reply
(a Comment). Comments are cheap and don't count against the 18M issue ceiling, so
steering messages into threads is the main lever. Target: push the root rate from
~80% toward ~40%, which roughly halves issues created and doubles time-to-ceiling.
Four mechanisms, ordered by leverage per effort:

1. **Message coalescing** *(gateway, invisible — build first).* Debounce
   consecutive messages from the same author within a ~60s window (no intervening
   reply) into a **single Issue**; body is newline-joined and each fragment keeps
   its own timestamp in a `chat.fragments` property. Pure server-side, no UX change,
   typically cuts raw issue count 30–50% because much "new topic" volume is really
   one person typing in bursts. A later edit or a reply closes the window.
2. **Reply-by-default composer** *(client).* Enter appends to the active topic;
   starting a genuinely new topic is the explicit action (distinct gesture + a
   one-line summary that becomes the Issue `summary`). Reply is the path of least
   resistance, inverting Slack's default.
3. **Thread auto-suggest** *(gateway heuristic, no ML).* When someone composes a
   top-level message within N minutes of an active thread, nudge inline
   ("Continue 'deploy rollback'? ⏎ to add, Esc for new"). Recency + keyword/@mention
   overlap catches most cases; escalate to embedding similarity only if it
   underperforms.
4. **Topic-list channel view** *(client).* Render a channel as a list of topics
   (Issues) with summaries, not an undifferentiated stream, so creating a topic has
   visible weight — and yields better JIRA summaries than "hey".

All four are observable as the **thread-root %** input in the capacity model
(`docs/` sizing artifact). Coalescing and the auto-suggest heuristic live in
`weavn-gateway`; the composer and topic-list view live in the client. The
deferred-promotion / daily-transcript fallback (§8) stays an escape hatch, not a
default, because it breaks per-message addressability.

## 10. Data model (projection store)

```
workspace(id, jira_site, …)
channel(id, workspace_id, parent_id NULL, segment, display_name, is_private,
        board_id, …)                             -- tree; parent_id builds the path; board_id = the Kanban board
membership(channel_id, user_id, role, last_read_seq, collapsed, …)
user(id, jira_account_id, display_name, avatar, …)
message(id, channel_id, jira_issue_key, seq, author_id, body_md, body_adf,
        status, created_at, edited_at, deleted, parent_issue_key NULL)   -- parent set ⇒ it's a reply/comment
reaction(message_id, emoji, user_id)            -- mirror of chat.reactions property
attachment(id, message_id, jira_attachment_id, filename, size, url)
ticket(issue_key, status, assignee, estimate, sprint, priority, labels, …)  -- cached Jira fields for the anchor card + refinement (§13)
config(workspace_id, key, value)                -- per-workspace settings (below); absorbs today's env vars
```
Issues carry only `weavn_channel = channel.id` (plus the storage Project). The path is
derived by walking `parent_id`, so a move updates one `channel` row and no issues.

The projection is a **cache that can be rebuilt from JIRA** by replaying issues and
comments — except `config` (and the DM vault's wrapped keys), which are the DB's own
durable state, not a cache of JIRA. JIRA is truth for conversations; losing the rest
is a reindex, not data loss.

**Host & access.** The operational store is plain **PostgreSQL**, reached *only*
through `weavn-store` (sqlx) from the gateway — never by clients. The host is
swappable via a connection string: **Supabase** for dev (instant Postgres, dashboard,
migrations), RDS / Cloud SQL / self-hosted in prod. We use Supabase *as Postgres* and
deliberately **do not** adopt its client-direct features (PostgREST, RLS, Realtime,
Auth): clients go through the gateway, and exposing the projection directly would
bypass the gateway's JIRA permission mapping and model translation (§2).

**`config` holds what are prototype env vars today** — per-workspace JIRA base URL +
auth mode, storage project key(s), the `weavn_channel` field id, the story-points field
id, the sprint-registry board id (§13), retention + DM-escrow policy, and feature
flags. Env vars mean one workspace; the table makes it multi-workspace and editable
without a redeploy.

---

## 11. Implementation stack — Rust everywhere

The entire backend (and ideally the desktop client) is written in **Rust**.

- **Gateway / BFF** — `tokio` async runtime; `axum` for HTTP + websocket upgrade (`axum::extract::ws`); `tower` middleware for auth/rate-limit/tracing. Stateless, scaled horizontally.
- **Auth / SSO** — Okta/OIDC via `openidconnect` at the gateway (Authorization Code + PKCE), gateway-issued sessions; resolves the person to a JIRA `accountId` (§2). Not Supabase Auth.
- **JIRA client** — `reqwest` (JSON, ret/backoff via `tower`/`backon`); a hand-rolled typed client over the REST v3 API. ADF modeled as `serde` structs/enums.
- **mrkdwn ⇆ ADF converter** — a standalone crate (`weavn-format`) with its own fixture-based test corpus. Pure, no I/O — the most unit-testable piece, built first.
- **Projection store** — PostgreSQL via `sqlx` (compile-time-checked queries), reached only from `weavn-store`. Host-agnostic by connection string: **Supabase** for dev, RDS / Cloud SQL / self-hosted in prod — used *as Postgres*, not via its client-direct APIs (§10). Holds the projection cache **and** durable config (§10). Search via Postgres FTS first; `tantivy` (in-process) or OpenSearch later if needed.
- **Real-time fan-out** — in-process `tokio::sync::broadcast`/channels per node, with Redis (`fred`) pub/sub (or NATS) to fan out across gateway nodes.
- **Webhook sync consumer** — a separate `axum` endpoint receiving JIRA webhooks onto a queue (Redis/NATS), workers apply to the projection idempotently.
- **DM vault crypto** (`weavn-keys`) — app-layer encryption for the DM vault (§7): a per-conversation content key, wrapped once per participant and once with the org **escrow** key; only wrapped keys are stored beside the vault issue. Pure crypto + key-wrapping logic, no I/O — unit-testable like `weavn-format`. The gateway calls it on the DM write/read path; nothing else touches plaintext.
- **Serialization / types** — `serde` throughout; share DTOs between gateway and client in a `weavn-proto` crate.
- **Workspace layout** — a Cargo workspace:
  ```
  crates/
    weavn-proto     # shared wire types (DTOs, events)
    weavn-format    # mrkdwn ⇆ ADF (build & test first)
    weavn-jira      # typed JIRA REST + ADF client
    weavn-keys      # DM vault crypto: content keys, per-participant + escrow wrapping
    weavn-store     # projection: schema, migrations, queries (sqlx)
    weavn-gateway   # axum: websockets, auth, write-through, read API
    weavn-sync      # webhook consumer + reconciliation job
  ```
- **Client** — Tauri (Rust core + web UI) for a native desktop app that reuses `weavn-proto`; or a standalone web front end talking the same wire protocol. Decide in the next pass.
- **Testing** — `cargo test` for units; `testcontainers` for Postgres integration; a JIRA mock (recorded fixtures) so the suite runs without a live JIRA.

## 12. Suggested build order

1. **Model + converter** — lock the mapping; build & unit-test the mrkdwn ⇆ ADF converter against a fixture corpus.
2. **Write-through + projection** — send a message → issue/comment in JIRA → row in projection. No real-time yet.
3. **Read path** — channel and thread views served from the projection.
4. **Webhook sync** — bidirectional updates; prove a JIRA-side comment shows up in chat.
5. **Real-time** — websockets, optimistic send, fan-out, presence/typing, status + availability (clock-in/out, §6).
6. **Reactions, edits, soft delete, attachments.**
7. **DMs (encrypted vault)** — `weavn-keys` + vault issues (§7); private channels via restricted project/filter (§7 is for DMs; channel privacy is project permissions).
8. **Search** (projection FTS, JQL power-search fallback).
9. **Reconciliation + backfill** job for resilience.
10. **Agile: sprints + in-chat refinement** (§13) — slash commands and editable anchor-card fields write through to Jira; a sprint-registry Scrum board owns sprint objects.

---

## 13. Agile: sprints and refinement

Board *type* is a Jira-side detail (§1), but two agile capabilities are first-class
in Weavn's own interface, independent of it.

### Sprints as a Weavn concept

- A conversation-issue can belong to a **sprint**; in chat it shows as a badge on
  the anchor card and as a filter/section ("Current sprint" · "Next" · "Backlog").
- Planning happens in chat: create / start / close a sprint and move conversations
  in or out, without opening Jira.
- **Mechanism.** Jira sprints are objects that live on a Scrum board and are reached
  through the Agile REST API; issues reference them via the global **Sprint** custom
  field. So Weavn runs one (or a few) **sprint-registry Scrum board(s)** per
  team purely to own sprint objects, while conversations — displayed on Kanban
  channel boards (§1) — carry a Sprint value through that global field. Sprint stays
  a badge/filter on the stream, never a display gate; the Scrum board is plumbing,
  not the channel.

### Refinement from the interface

Teams avoid Jira because editing fields is slow. Weavn turns refinement into
in-chat micro-actions on the conversation-issue:

- **Slash commands** in the composer: `/points 5`, `/assign @alex`,
  `/status in-review`, `/sprint next`, `/priority high`, `/label backend`.
- **Editable anchor card** at the thread root: status, assignee, estimate, sprint
  and priority as inline controls.
- **Promote a message** into the issue's description or acceptance criteria.

All are write-through to Jira field updates (and the Agile API for sprint moves) on
the same path as sending a message (§3), reflected back by webhooks (§5). The
projection caches these ticket fields so the anchor card renders without hitting
Jira. Requires company-managed projects and a global Sprint field (§1, §8).

---

## 14. Capabilities roadmap

Beyond the core (map, store, view, sync), these are the capabilities that make the
product sticky. Tiered by when and whether to build.

### Decided

- **No ephemeral chat tier.** The founding requirement — every conversation is a
  JIRA record — holds. Noise is managed by coalescing (§9 #1) and the
  daily-transcript escape hatch (§8), **not** by letting banter escape the record.
  A true ephemeral mode was considered and rejected; revisit only if the
  all-is-a-record premise itself is dropped.
- **Two channel types** (§1): native (`weavn_channel = id`) and mirror (JQL-bound).
- **DMs = encrypted vault, org-escrow keys** (§7). Kept as records, but privacy lives in the gateway key service, not Jira ACLs. Not true E2E (org can recover for compliance).
- **Managed multi-tenant hosting is the default offering.** Each workspace gets a subdomain + managed TLS, enabled by the per-workspace `config` table (§10). Enterprise may self-host / on-prem. The business runs on the managed fee — infra is light (gateway + Postgres) because the big cost, JIRA seats, sits on the customer's own Atlassian bill (§8) — **not** on infra affiliate credit.

### Adopt — core, some already designed

- **Anchor card** at each conversation root: live status / key / assignee, updating in place.
- **State-driven lifecycle**: Done → collapse / dim / move to a Resolved section; reopened → resurfaces. (Board columns.)
- **Bi-directional comment streaming** (§5) — already core.
- **Subscription → Watcher sync** and **Jira permission / issue-security mirroring** (§5, §7) — non-negotiable for enterprise.
- **Smart unread triage**: assigned-to-me / blocker / mention rank above ambient replies. (On the read-state projection.)
- **Sprints as a first-class concept** and **in-chat refinement** (slash commands + editable anchor-card fields) — now required, detailed in §13.
- **Presence, status & availability** — three layers incl. a clock-in/out "time clock"; projection-backed, peer-facing not manager-facing; see §6.

### Adopt, reframed for the chat-first model

- **Slash + inline field commands** and editable anchor-card fields — promoted to core; see §13.
- **Promote a message into structured fields** (acceptance criteria, description) — the chat-first version of "Pin to Jira"; it's about structure, not persistence (everything is already durable).
- **Mirror channels + dependency banners** from Jira issue links (two-channel-type model, §1).

### Adopt with caution

- **Reaction-driven workflows** (`:eyes:` → watcher, `:white_check_mark:` → done) — only a small, clearly-signposted curated set, kept distinct from ordinary emoji reactions (stored as entity properties, §6). Overloading casual reactions with state changes is a footgun.

### Defer (LLM / notification features for later)

- Auto-generated state-transition summaries; ticket-aware presence ("Alex is on PROJ-1042" — also privacy-sensitive); daily async standup digest; auto-grouping the sidebar by sprint cadence (the sprint *concept* is core — §13; only the automatic sidebar grouping is deferred).

---

## 15. Open questions for the next pass

- Cloud vs Data Center (decided largely by the licensing/volume answer in §8).
- Do conversations get a real workflow (Open/Resolved/Archived) everywhere, or is lifecycle opt-in per channel? (Forcing status on every one-liner is heavy.)
- Attachment storage: JIRA attachments vs external object store (S3) with JIRA holding a link — depends on file-size limits and retention policy.
- Retention / compliance: hard-delete and export (DSAR) flows on top of the soft-delete default.
