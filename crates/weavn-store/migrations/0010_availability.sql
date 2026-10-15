-- Tier-3: availability "time clock" — who is on the clock right now (peer-facing,
-- distinct from live socket presence). One row per clocked-in account.
create table if not exists availability (
    workspace text not null,
    account   text not null,
    display   text not null default '',
    since     timestamptz not null default now(),
    primary key (workspace, account)
);
