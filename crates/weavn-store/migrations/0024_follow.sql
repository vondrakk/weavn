-- weavN-native "follow": a person following an issue so it surfaces in their
-- For-You stream regardless of authorship/mention. (Emoji reactions already exist
-- in the `reaction` table from migration 0006 — reused, not redefined here.)
create table if not exists issue_follow (
    workspace  text not null,
    user_id    text not null,
    issue_key  text not null,
    created_at timestamptz not null default now(),
    primary key (workspace, user_id, issue_key)
);
create index if not exists issue_follow_user on issue_follow (workspace, user_id);
