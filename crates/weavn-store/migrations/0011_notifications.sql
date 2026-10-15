-- Tier-3: in-app notifications + keyword subscriptions.
-- (Email/push delivery is a separate subsystem; this covers in-app + live.)
create table if not exists notification (
    id         bigserial primary key,
    workspace  text not null,
    recipient  text not null,          -- JIRA accountId
    kind       text not null,          -- 'mention' | 'keyword' | 'reply'
    channel    text not null default '',
    issue_key  text not null default '',
    actor      text not null default '',
    snippet    text not null default '',
    read       boolean not null default false,
    created_at timestamptz not null default now()
);
create index if not exists notification_recip on notification (workspace, recipient, read, id);

create table if not exists keyword_sub (
    workspace text not null,
    account   text not null,
    keyword   text not null,
    primary key (workspace, account, keyword)
);
