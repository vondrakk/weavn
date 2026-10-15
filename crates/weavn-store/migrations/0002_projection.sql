-- Projection: messages mirrored from JIRA (docs/DESIGN.md §10). A top-level
-- message has parent_issue NULL; a reply points at its issue. Grouped by
-- issue_key on read into threads.
create table if not exists message (
    id           bigserial primary key,
    workspace    text not null,
    channel      text not null,
    issue_key    text not null,
    parent_issue text,
    author       text not null,
    body         text not null,
    from_jira    boolean not null default false,
    created_at   timestamptz not null default now()
);
create index if not exists message_chan_idx on message (workspace, channel, id);
create index if not exists message_issue_idx on message (workspace, issue_key);
