-- Tier-3: project the JIRA workflow status of a conversation-issue so the chat
-- client can show it and move it between states. Lives on the top-level row(s).
alter table message add column if not exists status text not null default '';
