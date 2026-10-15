-- Tier-2 message operations: soft delete, broadcast replies, and shared pins.
alter table message add column if not exists deleted   boolean not null default false;
alter table message add column if not exists broadcast boolean not null default false;

-- Pinned conversations are shared across the channel (not per-user), mirroring
-- Slack. The authoritative flag lives here; a `weavn_pinned` JIRA label mirrors it
-- for visibility inside JIRA.
create table if not exists pinned (
    workspace  text not null,
    channel    text not null,
    issue_key  text not null,
    pinned_by  text not null,
    created_at timestamptz not null default now(),
    primary key (workspace, issue_key)
);
