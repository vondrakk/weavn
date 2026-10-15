-- Tier-4: interactive messages (Block-Kit-lite). Action buttons attached to a
-- conversation-issue, posted by a bot/webhook; clicking runs an allowlisted
-- gateway action (transition / canned reply / acknowledge) as the clicking user.
create table if not exists message_action (
    id        bigserial primary key,
    workspace text not null,
    issue_key text not null,
    idx       int not null,
    label     text not null,
    kind      text not null,          -- 'transition' | 'reply' | 'ack'
    arg       text not null default ''
);
create index if not exists message_action_issue on message_action (workspace, issue_key, idx);
