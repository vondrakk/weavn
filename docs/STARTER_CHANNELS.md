# Starter channels & boards — customer onboarding guidance

In Weavn a **channel name drives the JIRA board** it lives on, `/` creates a
collapsible hierarchy (e.g. `eng / backend / deploys`), every top-level message
becomes a JIRA issue, and replies are comments. So the starter set isn't just
cosmetic — it decides which JIRA boards exist and how issue volume is shaped.

This is the suggested set we surface on first connect. **Start lean** (pick ~6–8),
and let hierarchy grow organically rather than pre-building everything.

## 1. Company-wide (everyone)

| Channel | Purpose | Notes |
|---|---|---|
| `general` | Day-to-day cross-team chatter and open questions | The default landing channel |
| `announcements` | Leadership / company broadcasts | **Read-mostly** — restrict who can start topics |
| `random` *(or `watercooler`)* | Non-business social — the "general / non-business board" | See §5 on keeping this off work reporting |
| `help` | "How do I… / who owns…" catch-all, routed to owners | Threading keeps it tidy |
| `kudos` | Recognition and shout-outs | Light, high-engagement |

## 2. Function / team boards

One board per team; use `/` for sub-topics so each team is a collapsible section.

| Area | Suggested channels |
|---|---|
| Engineering | `eng / backend`, `eng / frontend`, `eng / platform`, `eng / deploys`, `eng / code-review`, `eng / incidents` |
| Product | `product / roadmap`, `product / discovery` |
| Design | `design / reviews`, `design / assets` |
| Data / Analytics | `data`, `data / dashboards` |
| Marketing | `marketing / content`, `marketing / campaigns`, `marketing / events` |
| Sales | `sales / pipeline`, `sales / deals` |
| Support / CS | `support / tickets`, `support / escalations` |
| People / HR | `people` *(private)* |
| Finance / Ops | `finance`, `ops` |
| Legal | `legal` *(private)* |
| IT / Security | `it-help`, `security` *(private)* |

## 3. Cross-cutting / process

| Channel | Purpose |
|---|---|
| `oncall` | Rotation coordination and page follow-ups |
| `incidents` | Live incident coordination (postmortems as threads) |
| `releases` *(or `changelog`)* | What shipped, when |
| `all-hands` | Meeting notes and pre-submitted questions |
| `vendors` | Procurement / vendor threads |

## 4. Projects / initiatives (time-boxed)

One channel per active initiative; **archive when the initiative ends** (it dims
in the browser and stops counting toward noise).

- `proj / <name>` — a working channel per project
- `launch / <name>` — a go-to-market / launch room

## 5. Social / non-business

Everyday social space. Because these still become JIRA issues, see the tip below.

- `social / watercooler`, `social / food`, `social / events`
- `social / pets`, `social / music`, `social / books`
- `social / wins` (personal), `introductions`

## Guidance we give customers

- **Keep issue volume sane.** Favor **threading** (replies → comments) over new
  topics, and prefer a few broad channels over many narrow ones. Weavn coalesces
  a burst from one author into a single issue automatically.
- **Put social on its own JIRA project.** Point the `social / *` and `random`
  channels at a dedicated JIRA project/board so casual chatter never pollutes
  work-board reporting, JQL, or velocity metrics.
- **Read-mostly channels** (`announcements`, `releases`) should restrict who can
  start conversations — map to a JIRA project where only some roles create issues.
- **Private/sensitive** areas (`people`, `legal`, `security`, `sales / deals`)
  belong in a **restricted JIRA project** (project-level browse permission), which
  Weavn inherits as channel visibility.
- **Hierarchy over sprawl.** Use `team / topic / subtopic`; the channel browser
  groups by the first segment into sections, starred channels first.
- **Archive finished projects** instead of deleting — JIRA keeps the record.

## Recommended default (what to auto-create on first connect)

A minimal, opinionated starter so a new workspace isn't empty:

```
general
announcements        (read-mostly)
random               (social project)
help
eng / deploys
eng / incidents
product / roadmap
proj / onboarding    (example initiative — archive when done)
```
