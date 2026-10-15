-- Tier-3: scheduled messages ("send later") and reminders, fired by an
-- in-process scheduler in the gateway.
create table if not exists scheduled (
    id             bigserial primary key,
    workspace      text not null,
    author_account text not null,
    author_display text not null default '',
    channel        text not null default '',
    body           text not null default '',
    kind           text not null default 'message',  -- 'message' | 'reminder'
    due_at         timestamptz not null,
    done           boolean not null default false,
    tries          int not null default 0,
    created_at     timestamptz not null default now()
);
create index if not exists scheduled_due on scheduled (done, due_at);
