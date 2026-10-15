# weaveN

[![release](https://img.shields.io/github/v/release/vondrakk/weavn?sort=semver)](https://github.com/vondrakk/weavn/releases/latest)

**Slack-style team chat, backed entirely by JIRA.** Messages are issues, replies
are comments, and channel names drive the boards — so every conversation is a
first-class, searchable, auditable JIRA record, with none of the manual ticket
bookkeeping.

## Download

Grab the `weavn` CLI from the [latest release](https://github.com/vondrakk/weavn/releases/latest):

| Platform | Asset |
|---|---|
| Linux x86_64 | `weavn-v0.1.0-linux-x86_64.tar.gz` |
| macOS (Apple Silicon) | `weavn-v0.1.0-macos-arm64.tar.gz` |

```sh
# verify + unpack (Linux example)
sha256sum -c weavn-v0.1.0-linux-x86_64.sha256
tar xzf weavn-v0.1.0-linux-x86_64.tar.gz
./weavn --help
```

## What it is

A Rust workspace:

- **`weavn-gateway`** — an axum server: JIRA-token auth, a real-time WebSocket
  hub, and a single-page web client, over a Postgres projection.
- **`weavn-store`** — the Postgres projection (sqlx migrations).
- **`weavn-cli`** (`weavn`) — the command-line client (shipped in releases).
- **`weavn`** (lib) — the engine: channel paths, mrkdwn⇆ADF, the JIRA backend,
  and the `age`-encrypted DM vault.

## How it maps to JIRA

| Chat | JIRA |
|---|---|
| message | issue (summary + description) |
| reply | comment |
| channel | board / filter (the channel name drives it) |
| status chip | the issue's workflow status (one-click transitions) |
| DMs | an `age`-encrypted vault in Postgres — **never** a JIRA issue |

## Highlights

Channels (hierarchical), threads, rich text, reactions, full-text search,
@mentions, file attachments, pins/saves/stars, edit/soft-delete, live delivery /
presence / typing, availability, notifications + keyword alerts, scheduled
messages & reminders, link unfurling, private channels, incoming & bot webhooks,
interactive action buttons, a no-code Workflow Builder, and an encrypted DM
vault with org escrow.

## Build & run from source

```sh
cargo build --release                 # all crates
./scripts/pg.sh up                    # local Postgres (Podman)
./scripts/gateway.sh                  # run the gateway (see scripts/*.env.example)
```

The gateway needs `DATABASE_URL` and a JIRA site + API token (see
`scripts/jira.env.example`). Then open the printed URL and sign in with your JIRA
site, email, and token.

## Docs

- [`docs/DESIGN.md`](docs/DESIGN.md) — architecture and the full design.
- [`docs/ROADMAP.md`](docs/ROADMAP.md) — Slack-parity features, ranked, with build status.
- [`docs/INTEGRATIONS_BACKLOG.md`](docs/INTEGRATIONS_BACKLOG.md) — the spec'd-but-unbuilt items.
- [`docs/STARTER_CHANNELS.md`](docs/STARTER_CHANNELS.md) — suggested starter channels.
