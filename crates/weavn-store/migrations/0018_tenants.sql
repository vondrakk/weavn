-- Weavn SaaS control plane: one tenant per JIRA site, provisioned on first login
-- (docs/MULTITENANCY.md). The subdomain IS the JIRA site's own DNS label
-- (domaintools.atlassian.net -> domaintools.weavn.io), so the mapping is 1:1 and
-- immutable.
--
-- PRIVACY: this table, plus the licensing tables (0005), are the ONLY place the
-- hosted service captures account data — the subdomain, the JIRA site it maps to,
-- and the founding user's email + display name. Conversation content is NOT here:
-- it lives in the per-tenant projection (a cache of the customer's own JIRA),
-- keyed by the same jira_base/workspace.

create table if not exists tenant (
    subdomain      text primary key,                 -- public label: <subdomain>.weavn.io
    jira_base      text not null unique,             -- https://<subdomain>.atlassian.net (= workspace key)
    founding_email text,                              -- first user to provision (informational)
    founding_name  text,                              -- their display name (retained per product decision)
    plan           text not null default 'free',     -- free | commercial (mirrors a current license)
    created_at     timestamptz not null default now(),
    last_active_at timestamptz not null default now()
);

-- Login-activity log: the "usage activity" we retain. Identity + time only, never
-- any message content. One row per successful sign-in.
create table if not exists login_event (
    id        bigserial primary key,
    subdomain text not null,
    user_id   text not null,                          -- JIRA accountId / OIDC subject
    email     text,
    at        timestamptz not null default now()
);
create index if not exists login_event_sub_idx on login_event (subdomain, at);
