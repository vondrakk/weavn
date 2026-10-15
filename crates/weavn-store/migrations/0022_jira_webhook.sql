-- JIRA → weavn inbound webhook: a per-workspace secret in the callback URL both
-- authenticates the call and routes the event to the right workspace. One secret
-- per workspace; the receiver looks the workspace up by secret.
create table if not exists jira_webhook (
    workspace  text primary key,
    secret     text not null unique,
    created_at timestamptz not null default now()
);
