-- Tier-4: bot users. A named workspace identity with a token that can post to
-- channels via the API (posts go through the server's service JIRA account).
create table if not exists bot (
    token      text primary key,
    workspace  text not null,
    name       text not null default 'bot',
    created_by text not null default '',
    created_at timestamptz not null default now()
);
create index if not exists bot_ws on bot (workspace);
