-- Tier-4: Workflow Builder. "When a new message in <channel> contains <text>,
-- run <action>." Actions are an allowlist executed server-side.
create table if not exists workflow (
    id          bigserial primary key,
    workspace   text not null,
    channel     text not null default '*',   -- '*' = any channel
    contains    text not null default '',     -- case-insensitive substring; '' = any
    action_kind text not null,                -- react | reply | transition | addlabel | pin
    action_arg  text not null default '',
    enabled     boolean not null default true,
    created_by  text not null default '',
    created_at  timestamptz not null default now()
);
create index if not exists workflow_ws on workflow (workspace, enabled);
