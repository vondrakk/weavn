-- Weavn operational store — initial schema (docs/DESIGN.md §10).
-- This first slice is the durable, non-cache state: per-workspace config and the
-- custom-emoji registry. Projection/cache tables (channel, message, …) land in
-- later migrations as the store is wired into the gateway.

create table if not exists workspace (
    id        text primary key,
    jira_site text
);

-- Per-workspace settings that are env vars in the prototype today.
create table if not exists config (
    workspace_id text not null,
    key          text not null,
    value        text not null,
    primary key (workspace_id, key)
);

-- Custom emoji, optionally restricted to a set of accounts who may *use* it.
-- allowed_accounts: comma-separated account ids; NULL/empty = anyone may use it.
-- (A restricted emoji is still visible to everyone once applied; the list only
-- gates who can apply it.)
create table if not exists emoji (
    workspace_id     text not null,
    shortcode        text not null,   -- e.g. ':~Y:'
    image_ref        text not null,   -- asset id / URL of the image
    created_by       text not null,   -- account id of the creator
    allowed_accounts text,            -- NULL = public; else comma-separated account ids
    primary key (workspace_id, shortcode)
);
