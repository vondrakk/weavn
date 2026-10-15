-- Tier-3 chat parity: reactions, read-state, and full-text search.

-- Emoji reactions on a conversation (issue-level), one row per (user, emoji).
create table if not exists reaction (
    workspace  text not null,
    issue_key  text not null,
    shortcode  text not null,
    account    text not null,
    created_at timestamptz not null default now(),
    primary key (workspace, issue_key, shortcode, account)
);
create index if not exists reaction_issue on reaction (workspace, issue_key);

-- Per-user last-read marker per channel (unix seconds of the newest seen message).
create table if not exists last_read (
    workspace text not null,
    user_id   text not null,
    channel   text not null,
    last_ts   bigint not null default 0,
    primary key (workspace, user_id, channel)
);

-- Full-text search over message bodies (generated tsvector + GIN index).
alter table message add column if not exists body_tsv tsvector
    generated always as (to_tsvector('english', coalesce(body, ''))) stored;
create index if not exists message_tsv on message using gin (body_tsv);
