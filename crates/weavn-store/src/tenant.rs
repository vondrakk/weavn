//! SaaS control plane: tenant provisioning + login activity (docs/MULTITENANCY.md).
//!
//! A tenant is one JIRA site, published at `<subdomain>.weavn.io`. The subdomain is
//! the JIRA site's own DNS label (`domaintools.atlassian.net` -> `domaintools`), so
//! the mapping is 1:1 and immutable — no vanity names, no collisions.
//!
//! This module and [`crate::license`] are the ONLY places the hosted service stores
//! account data: the subdomain, the JIRA base it maps to, the founding user's email
//! and display name, and a login-activity log. Conversation content stays in the
//! per-tenant projection (a cache of the customer's own JIRA), never here.

use crate::Pool;

/// Subdomains the platform keeps for itself — never handed to a tenant. These are
/// also the labels that mark a host as "apex / not a tenant" (e.g. `app.weavn.io`).
pub const RESERVED: &[&str] = &[
    "app", "www", "api", "admin", "auth", "static", "assets", "cdn", "mail", "smtp",
    "status", "docs", "doc", "help", "support", "dashboard", "console", "internal",
    "ns", "ns1", "ns2", "ns3", "mx", "ftp", "test", "dev", "staging", "demo",
];

/// Derive the tenant subdomain from a JIRA base URL: the first host label,
/// lowercased. `https://domaintools.atlassian.net` -> `Some("domaintools")`.
/// Returns `None` for a reserved label or anything that isn't a valid DNS label.
pub fn subdomain_for_base(jira_base: &str) -> Option<String> {
    let host = jira_base
        .split("://")
        .nth(1)
        .unwrap_or(jira_base)
        .split('/')
        .next()
        .unwrap_or("")
        .split(':')
        .next()
        .unwrap_or("");
    let label = host.split('.').next().unwrap_or("").to_ascii_lowercase();
    let valid = (1..=63).contains(&label.len())
        && !label.starts_with('-')
        && !label.ends_with('-')
        && label.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-');
    if valid && !RESERVED.contains(&label.as_str()) {
        Some(label)
    } else {
        None
    }
}

#[derive(Debug, Clone)]
pub struct Tenant {
    pub subdomain: String,
    pub jira_base: String,
    pub founding_email: Option<String>,
    pub founding_name: Option<String>,
    pub plan: String,
    pub created_at: String,
    pub last_active_at: String,
}

/// Provision a tenant on first login (idempotent). The first writer sets the
/// founding identity; later sign-ins leave it untouched (handled by the caller via
/// [`touch`]). Returns `true` when this call created the tenant.
pub async fn provision(
    pool: &Pool,
    subdomain: &str,
    jira_base: &str,
    email: Option<&str>,
    name: Option<&str>,
) -> sqlx::Result<bool> {
    let n = sqlx::query(
        "insert into tenant (subdomain, jira_base, founding_email, founding_name) \
         values ($1, $2, $3, $4) on conflict (subdomain) do nothing",
    )
    .bind(subdomain)
    .bind(jira_base)
    .bind(email)
    .bind(name)
    .execute(pool)
    .await?
    .rows_affected();
    Ok(n > 0)
}

/// Bump the tenant's `last_active_at` (called on each sign-in).
pub async fn touch(pool: &Pool, subdomain: &str) -> sqlx::Result<()> {
    sqlx::query("update tenant set last_active_at = now() where subdomain = $1")
        .bind(subdomain)
        .execute(pool)
        .await?;
    Ok(())
}

/// Reflect the current license state onto the tenant's `plan` label
/// (`commercial` when licensed, else `free`) — purely informational.
pub async fn set_plan(pool: &Pool, subdomain: &str, plan: &str) -> sqlx::Result<()> {
    sqlx::query("update tenant set plan = $2 where subdomain = $1")
        .bind(subdomain)
        .bind(plan)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn by_subdomain(pool: &Pool, subdomain: &str) -> sqlx::Result<Option<Tenant>> {
    let row = sqlx::query_as::<_, (String, String, Option<String>, Option<String>, String, String, String)>(
        "select subdomain, jira_base, founding_email, founding_name, plan, \
         created_at::text, last_active_at::text from tenant where subdomain = $1",
    )
    .bind(subdomain)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(
        |(subdomain, jira_base, founding_email, founding_name, plan, created_at, last_active_at)| Tenant {
            subdomain,
            jira_base,
            founding_email,
            founding_name,
            plan,
            created_at,
            last_active_at,
        },
    ))
}

/// Log a sign-in — the "login activity" we retain (identity + time, no content).
pub async fn log_login(
    pool: &Pool,
    subdomain: &str,
    user_id: &str,
    email: Option<&str>,
) -> sqlx::Result<()> {
    sqlx::query("insert into login_event (subdomain, user_id, email) values ($1, $2, $3)")
        .bind(subdomain)
        .bind(user_id)
        .bind(email)
        .execute(pool)
        .await?;
    Ok(())
}

/// Every provisioned tenant, newest first (ops / fleet view).
pub async fn list(pool: &Pool) -> sqlx::Result<Vec<Tenant>> {
    let rows = sqlx::query_as::<_, (String, String, Option<String>, Option<String>, String, String, String)>(
        "select subdomain, jira_base, founding_email, founding_name, plan, \
         created_at::text, last_active_at::text from tenant order by created_at desc",
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(
            |(subdomain, jira_base, founding_email, founding_name, plan, created_at, last_active_at)| Tenant {
                subdomain,
                jira_base,
                founding_email,
                founding_name,
                plan,
                created_at,
                last_active_at,
            },
        )
        .collect())
}

#[cfg(test)]
mod tests {
    use super::subdomain_for_base;

    #[test]
    fn derives_label_from_jira_base() {
        assert_eq!(subdomain_for_base("https://domaintools.atlassian.net").as_deref(), Some("domaintools"));
        assert_eq!(subdomain_for_base("https://ACME.atlassian.net/").as_deref(), Some("acme"));
        assert_eq!(subdomain_for_base("https://acme.atlassian.net:443").as_deref(), Some("acme"));
    }

    #[test]
    fn rejects_reserved_and_malformed() {
        assert_eq!(subdomain_for_base("https://app.atlassian.net"), None);
        assert_eq!(subdomain_for_base("https://-bad.atlassian.net"), None);
        assert_eq!(subdomain_for_base(""), None);
    }
}
