-- Weavn licensing & seat metering (docs/LICENSING.md).
-- Model: METER, NEVER DENY. A "seat" is a distinct user who has signed in to a
-- JIRA namespace (workspace = JIRA site base URL). Licenses are time-bounded and
-- seat-count based; usage over entitlement is reported so the customer can
-- right-size later. Seats are reclaimable (quits / new hires), with an audit trail.

create table if not exists seat (
    workspace    text not null,
    user_id      text not null,                   -- JIRA accountId / OIDC subject
    display      text not null default '',
    email        text,
    active       boolean not null default true,
    first_seen   timestamptz not null default now(),
    last_seen    timestamptz not null default now(),
    released_at  timestamptz,
    primary key (workspace, user_id)
);
create index if not exists seat_ws_active_idx on seat (workspace, active);

create table if not exists seat_event (
    id         bigserial primary key,
    workspace  text not null,
    user_id    text not null,
    event      text not null,                      -- claimed | reactivated | released
    at         timestamptz not null default now(),
    note       text
);
create index if not exists seat_event_ws_idx on seat_event (workspace, at);

create table if not exists license (
    workspace   text primary key,
    plan        text not null default 'team',
    seats       integer not null,                  -- entitled seats (right-sized over time)
    starts_at   date not null default current_date,
    ends_at     date,                              -- null = open-ended
    status      text not null default 'active',    -- active | suspended
    note        text,
    updated_at  timestamptz not null default now()
);
