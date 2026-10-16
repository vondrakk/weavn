-- Real JIRA avatars. We learn each person's display name + avatar URL wherever we
-- already see them (webhook events, backfill, issue fetches) and cache it per
-- workspace. The stream renders by display name, so the SPA resolves name → avatar
-- from this table; account_id is the stable key for refreshing a changed name/photo.
create table if not exists jira_user (
    workspace    text not null,
    account_id   text not null,
    display_name text not null,
    avatar_url   text not null,
    updated_at   timestamptz not null default now(),
    primary key (workspace, account_id)
);
create index if not exists jira_user_name on jira_user (workspace, display_name);
