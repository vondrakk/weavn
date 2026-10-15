-- Tier-3: private channels + membership, enforced at the Weavn gateway.
-- NOTE: native-channel issues still live in the shared storage project, so this
-- is access control at the Weavn layer, not JIRA issue-level security. For hard
-- isolation, point a private channel at a restricted JIRA project (see DESIGN).
create table if not exists channel_acl (
    workspace  text not null,
    channel    text not null,
    private    boolean not null default false,
    created_by text not null default '',
    created_at timestamptz not null default now(),
    primary key (workspace, channel)
);

create table if not exists channel_member (
    workspace text not null,
    channel   text not null,
    account   text not null,     -- JIRA accountId
    display   text not null default '',
    added_at  timestamptz not null default now(),
    primary key (workspace, channel, account)
);
create index if not exists channel_member_lookup on channel_member (workspace, account);
