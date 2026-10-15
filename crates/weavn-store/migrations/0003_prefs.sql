-- Per-user preferences for Tier-2 features (stars, saved items, status, DND, archive).
create table if not exists star (
    workspace text not null,
    user_id   text not null,
    channel   text not null,
    primary key (workspace, user_id, channel)
);

create table if not exists saved (
    workspace  text not null,
    user_id    text not null,
    issue_key  text not null,
    created_at timestamptz not null default now(),
    primary key (workspace, user_id, issue_key)
);

create table if not exists archived (
    workspace text not null,
    user_id   text not null,
    channel   text not null,
    primary key (workspace, user_id, channel)
);

create table if not exists user_state (
    workspace    text not null,
    user_id      text not null,
    status_emoji text,
    status_text  text,
    dnd          boolean not null default false,
    primary key (workspace, user_id)
);
