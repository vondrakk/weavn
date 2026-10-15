-- Tier-4: encrypted DM vault (docs/DESIGN.md §7). DMs never become JIRA issues;
-- bodies are age-encrypted (to each participant + an org escrow key) and live
-- only here. NOTE: keypairs are stored at rest for the prototype — a production
-- deployment wraps secret keys under a KMS / key service, not in the DB.
create table if not exists vault_key (
    owner      text primary key,     -- JIRA accountId, or '__escrow__'
    public_key text not null,         -- age1...
    secret_key text not null,         -- AGE-SECRET-KEY-1...
    created_at timestamptz not null default now()
);

create table if not exists dm_convo (
    id         bigserial primary key,
    workspace  text not null,
    members    text not null,         -- sorted comma-joined accountIds (the identity)
    title      text not null default '',
    created_at timestamptz not null default now(),
    unique (workspace, members)
);

create table if not exists dm_member (
    convo_id bigint not null,
    account  text not null,
    display  text not null default '',
    primary key (convo_id, account)
);
create index if not exists dm_member_acct on dm_member (account);

create table if not exists dm_message (
    id             bigserial primary key,
    convo_id       bigint not null,
    sender         text not null,
    sender_display text not null default '',
    ciphertext     text not null,     -- base64 age ciphertext
    created_at     timestamptz not null default now()
);
create index if not exists dm_message_convo on dm_message (convo_id, id);
