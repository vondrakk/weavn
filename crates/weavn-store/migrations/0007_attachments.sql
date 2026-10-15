-- Tier-3 file uploads: files are stored as native JIRA attachments on the
-- conversation-issue; this table projects them for listing and for the
-- gateway's authenticated download proxy.
create table if not exists attachment (
    id          bigserial primary key,
    workspace   text not null,
    issue_key   text not null,
    jira_id     text not null,
    filename    text not null,
    mime        text not null default '',
    size_bytes  bigint not null default 0,
    uploaded_by text not null,
    created_at  timestamptz not null default now()
);
create index if not exists attachment_issue on attachment (workspace, issue_key);
