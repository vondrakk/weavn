-- Per-issue metadata captured at backfill: the JIRA issue type (Epic/Story/Bug/…)
-- and its parent/epic link, so the client can decorate each message by type and
-- show which epic it belongs to. Lives on the top-level (root) message rows.
alter table message add column if not exists issue_type     text not null default '';
alter table message add column if not exists parent_key     text;
alter table message add column if not exists parent_summary text;
