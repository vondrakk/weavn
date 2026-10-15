-- Generic per-user preference store (workspace + user + key → JSON/text value).
-- First use: each user's personal sidebar layout (custom sections + ordering).
create table if not exists user_pref (
    workspace  text not null,
    user_id    text not null,
    key        text not null,
    value      text not null,
    updated_at timestamptz not null default now(),
    primary key (workspace, user_id, key)
);
