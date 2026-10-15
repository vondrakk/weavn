-- Tier-3: incoming webhooks. A secret token maps to a channel; external systems
-- POST to /hook/{token} to post a message via the server's service JIRA account.
create table if not exists webhook (
    token      text primary key,
    workspace  text not null,
    channel    text not null,
    label      text not null default 'webhook',
    created_by text not null default '',
    created_at timestamptz not null default now()
);
create index if not exists webhook_channel on webhook (workspace, channel);
