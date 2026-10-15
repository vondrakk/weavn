//! The JIRA backend: a trait with a mock (always) and a real client (feature).
//!
//! The engine only ever talks to `Backend`, so mock-now / real-later is a
//! swap of the impl — the decision we locked for the prototype.

use crate::proto::{ChannelId, IssueKey, TicketUpdate};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone)]
pub struct CreatedIssue {
    pub key: IssueKey,
}

#[derive(Debug, Clone)]
pub struct CreatedComment {
    pub id: String,
}

/// Everything the gateway needs from JIRA. `body_md` is the plain source (handy
/// for the mock's readable dump); `body_adf` is what a real site actually stores.
pub trait Backend {
    fn create_issue(
        &mut self,
        project: &str,
        channel_id: &ChannelId,
        summary: &str,
        body_md: &str,
        body_adf: &Value,
    ) -> Result<CreatedIssue, String>;

    /// Coalesce: set an existing issue's description to the merged body (the
    /// engine passes the full merged content, not just the new fragment).
    fn set_description(
        &mut self,
        key: &IssueKey,
        body_md: &str,
        body_adf: &Value,
    ) -> Result<(), String>;

    fn add_comment(
        &mut self,
        key: &IssueKey,
        author: &str,
        body_md: &str,
        body_adf: &Value,
    ) -> Result<CreatedComment, String>;

    /// Apply ticket-field changes (assignment, status, points, priority, labels)
    /// from in-chat refinement (§13). Returns a human-readable list of what changed.
    fn update_fields(&mut self, key: &IssueKey, up: &TicketUpdate) -> Result<Vec<String>, String>;

    /// Introspection so the demo can show the JIRA side of the mapping.
    fn dump(&self) -> Vec<MockIssue> {
        Vec::new()
    }
}

/// A comment as JIRA holds it: author + body.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommentRec {
    pub author: String,
    pub body: String,
}

/// Ticket fields set by refinement (§13).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TicketFields {
    pub assignee: Option<String>,
    pub status: Option<String>,
    pub story_points: Option<f64>,
    pub priority: Option<String>,
    pub labels: Vec<String>,
}

/// A readable snapshot of one issue as JIRA would hold it.
#[derive(Debug, Clone)]
pub struct MockIssue {
    pub key: String,
    pub project: String,
    pub channel_id: String,
    pub summary: String,
    pub description: String,
    pub comments: Vec<CommentRec>,
    pub fields: TicketFields,
}

// ---------------------------------------------------------------------------
// Mock backend
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MockBackend {
    counter: u64,
    issues: Vec<StoredIssue>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredIssue {
    key: String,
    project: String,
    channel_id: String,
    summary: String,
    description: String,
    comments: Vec<CommentRec>,
    #[serde(default)]
    fields: TicketFields,
}

impl MockBackend {
    pub fn new() -> Self {
        Self::default()
    }

    fn find_mut(&mut self, key: &str) -> Option<&mut StoredIssue> {
        self.issues.iter_mut().find(|i| i.key == key)
    }
}

impl Backend for MockBackend {
    fn create_issue(
        &mut self,
        project: &str,
        channel_id: &ChannelId,
        summary: &str,
        body_md: &str,
        _body_adf: &Value,
    ) -> Result<CreatedIssue, String> {
        self.counter += 1;
        let key = format!("{project}-{}", self.counter);
        self.issues.push(StoredIssue {
            key: key.clone(),
            project: project.to_string(),
            channel_id: channel_id.clone(),
            summary: summary.to_string(),
            description: body_md.to_string(),
            comments: Vec::new(),
            fields: TicketFields::default(),
        });
        Ok(CreatedIssue { key })
    }

    fn set_description(
        &mut self,
        key: &IssueKey,
        body_md: &str,
        _body_adf: &Value,
    ) -> Result<(), String> {
        let issue = self.find_mut(key).ok_or_else(|| format!("no such issue {key}"))?;
        issue.description = body_md.to_string();
        Ok(())
    }

    fn add_comment(
        &mut self,
        key: &IssueKey,
        author: &str,
        body_md: &str,
        _body_adf: &Value,
    ) -> Result<CreatedComment, String> {
        let issue = self.find_mut(key).ok_or_else(|| format!("no such issue {key}"))?;
        issue.comments.push(CommentRec {
            author: author.to_string(),
            body: body_md.to_string(),
        });
        Ok(CreatedComment {
            id: format!("{key}#c{}", issue.comments.len()),
        })
    }

    fn update_fields(&mut self, key: &IssueKey, up: &TicketUpdate) -> Result<Vec<String>, String> {
        let issue = self.find_mut(key).ok_or_else(|| format!("no such issue {key}"))?;
        let mut applied = Vec::new();
        if let Some(a) = &up.assignee {
            issue.fields.assignee = Some(a.clone());
            applied.push(format!("assignee → {a}"));
        }
        if let Some(s) = &up.status {
            issue.fields.status = Some(s.clone());
            applied.push(format!("status → {s}"));
        }
        if let Some(p) = up.story_points {
            issue.fields.story_points = Some(p);
            applied.push(format!("points → {p}"));
        }
        if let Some(pr) = &up.priority {
            issue.fields.priority = Some(pr.clone());
            applied.push(format!("priority → {pr}"));
        }
        for l in &up.add_labels {
            if !issue.fields.labels.contains(l) {
                issue.fields.labels.push(l.clone());
            }
            applied.push(format!("label + {l}"));
        }
        Ok(applied)
    }

    fn dump(&self) -> Vec<MockIssue> {
        self.issues
            .iter()
            .map(|i| MockIssue {
                key: i.key.clone(),
                project: i.project.clone(),
                channel_id: i.channel_id.clone(),
                summary: i.summary.clone(),
                description: i.description.clone(),
                comments: i.comments.clone(),
                fields: i.fields.clone(),
            })
            .collect()
    }
}

// ---------------------------------------------------------------------------
// Real JIRA Cloud backend (feature = "real-jira")
// ---------------------------------------------------------------------------

#[cfg(feature = "real-jira")]
pub mod real {
    use super::*;
    use base64::prelude::{Engine as _, BASE64_STANDARD};
    use serde_json::json;

    /// Talks to a real JIRA Cloud site over REST v3.
    ///
    /// Config comes from the environment so no secrets live in code:
    ///   WEAVN_JIRA_BASE   e.g. https://your-site.atlassian.net
    ///   WEAVN_JIRA_EMAIL  the account email (basic auth only)
    ///   WEAVN_JIRA_TOKEN  an API token (Cloud) or PAT
    ///   WEAVN_JIRA_AUTH   "basic" (default, Cloud: email+token) or "bearer" (PAT)
    ///   WEAVN_CHANNEL_FIELD  (optional) custom field id, e.g. customfield_10050;
    ///                              if unset, the channel id is written as a label.
    pub struct RealBackend {
        base: String,
        auth_header: String,
        channel_field: Option<String>,
        http: reqwest::blocking::Client,
    }

    impl RealBackend {
        pub fn from_env() -> Result<Self, String> {
            let get = |k: &str| std::env::var(k).map_err(|_| format!("missing env var {k}"));
            let base = get("WEAVN_JIRA_BASE")?.trim_end_matches('/').to_string();
            let token = get("WEAVN_JIRA_TOKEN")?;
            let bearer = std::env::var("WEAVN_JIRA_AUTH")
                .map(|v| v.eq_ignore_ascii_case("bearer"))
                .unwrap_or(false);
            let auth_header = if bearer {
                format!("Bearer {token}")
            } else {
                let email = get("WEAVN_JIRA_EMAIL")?;
                format!("Basic {}", BASE64_STANDARD.encode(format!("{email}:{token}")))
            };
            Ok(Self {
                base,
                auth_header,
                channel_field: std::env::var("WEAVN_CHANNEL_FIELD").ok(),
                http: reqwest::blocking::Client::new(),
            })
        }

        /// Build from explicit credentials (e.g. a logged-in user's session) rather
        /// than the environment. `base` is the full site URL.
        pub fn from_parts(base: &str, email: Option<&str>, token: &str, bearer: bool) -> Self {
            let auth_header = if bearer {
                format!("Bearer {token}")
            } else {
                let email = email.unwrap_or("");
                format!("Basic {}", BASE64_STANDARD.encode(format!("{email}:{token}")))
            };
            Self {
                base: base.trim_end_matches('/').to_string(),
                auth_header,
                channel_field: std::env::var("WEAVN_CHANNEL_FIELD").ok(),
                http: reqwest::blocking::Client::new(),
            }
        }

        fn channel_fields(&self, channel_id: &str) -> Value {
            match &self.channel_field {
                Some(field) => json!({ field: channel_id }),
                None => json!({ "labels": [format!("weavn_channel_{channel_id}")] }),
            }
        }

        /// Read-only: confirm the token authenticates. Returns the display name.
        pub fn verify(&self) -> Result<String, String> {
            let resp = self
                .http
                .get(format!("{}/rest/api/3/myself", self.base))
                .header("Authorization", &self.auth_header)
                .send()
                .map_err(|e| e.to_string())?;
            let status = resp.status();
            if !status.is_success() {
                return Err(format!("auth failed ({status}) — check base URL, email, and token"));
            }
            let v: Value = resp.json().map_err(|e| e.to_string())?;
            Ok(v["displayName"].as_str().unwrap_or("?").to_string())
        }

        /// Read-only: confirm a project exists and is visible. Returns its name.
        pub fn check_project(&self, key: &str) -> Result<String, String> {
            let resp = self
                .http
                .get(format!("{}/rest/api/3/project/{key}", self.base))
                .header("Authorization", &self.auth_header)
                .send()
                .map_err(|e| e.to_string())?;
            let status = resp.status();
            if !status.is_success() {
                return Err(format!("project {key} not found or not visible ({status})"));
            }
            let v: Value = resp.json().map_err(|e| e.to_string())?;
            Ok(v["name"].as_str().unwrap_or(key).to_string())
        }

        /// Resolve a name/email query to a Jira Cloud accountId (first match).
        fn resolve_account_id(&self, query: &str) -> Result<String, String> {
            let resp = self
                .http
                .get(format!("{}/rest/api/3/user/search", self.base))
                .header("Authorization", &self.auth_header)
                .query(&[("query", query)])
                .send()
                .map_err(|e| e.to_string())?;
            let status = resp.status();
            let v: Value = resp.json().map_err(|e| e.to_string())?;
            if !status.is_success() {
                return Err(format!("user search {status}: {v}"));
            }
            v.as_array()
                .and_then(|a| a.first())
                .and_then(|u| u["accountId"].as_str())
                .map(|s| s.to_string())
                .ok_or_else(|| format!("no Jira user matched {query:?}"))
        }

        /// Apply a workflow transition whose target status matches `target`.
        fn apply_transition(&self, key: &str, target: &str) -> Result<(), String> {
            let resp = self
                .http
                .get(format!("{}/rest/api/3/issue/{key}/transitions", self.base))
                .header("Authorization", &self.auth_header)
                .send()
                .map_err(|e| e.to_string())?;
            let v: Value = resp.json().map_err(|e| e.to_string())?;
            let transitions = v["transitions"].as_array().ok_or("no transitions returned")?;
            let want = target.to_ascii_lowercase();
            let matches = |name: Option<&str>| name.map(|n| n.to_ascii_lowercase() == want).unwrap_or(false);
            let id = transitions
                .iter()
                .find(|t| matches(t["to"]["name"].as_str()) || matches(t["name"].as_str()))
                .and_then(|t| t["id"].as_str())
                .ok_or_else(|| {
                    let names: Vec<&str> = transitions
                        .iter()
                        .filter_map(|t| t["to"]["name"].as_str())
                        .collect();
                    format!("no transition to {target:?}; available: {names:?}")
                })?
                .to_string();
            let resp = self
                .http
                .post(format!("{}/rest/api/3/issue/{key}/transitions", self.base))
                .header("Authorization", &self.auth_header)
                .json(&json!({ "transition": { "id": id } }))
                .send()
                .map_err(|e| e.to_string())?;
            if !resp.status().is_success() {
                return Err(format!("transition failed: {}", resp.status()));
            }
            Ok(())
        }

        /// Current status name of an issue (e.g. "To Do", "In Progress", "Done").
        pub fn issue_status(&self, key: &str) -> Result<String, String> {
            let resp = self
                .http
                .get(format!("{}/rest/api/3/issue/{key}?fields=status", self.base))
                .header("Authorization", &self.auth_header)
                .send()
                .map_err(|e| e.to_string())?;
            if !resp.status().is_success() {
                return Err(format!("status fetch failed: {}", resp.status()));
            }
            let v: Value = resp.json().map_err(|e| e.to_string())?;
            Ok(v["fields"]["status"]["name"].as_str().unwrap_or("").to_string())
        }

        /// Target status names reachable from the issue's current status.
        pub fn list_transitions(&self, key: &str) -> Result<Vec<String>, String> {
            let resp = self
                .http
                .get(format!("{}/rest/api/3/issue/{key}/transitions", self.base))
                .header("Authorization", &self.auth_header)
                .send()
                .map_err(|e| e.to_string())?;
            if !resp.status().is_success() {
                return Err(format!("transitions fetch failed: {}", resp.status()));
            }
            let v: Value = resp.json().map_err(|e| e.to_string())?;
            Ok(v["transitions"]
                .as_array()
                .map(|a| a.iter().filter_map(|t| t["to"]["name"].as_str().map(String::from)).collect())
                .unwrap_or_default())
        }

        /// Public wrapper: move an issue to the workflow state named `target`.
        pub fn transition_to(&self, key: &str, target: &str) -> Result<(), String> {
            self.apply_transition(key, target)
        }

        /// Assign an issue to a JIRA accountId (empty string → unassign).
        pub fn assign(&self, key: &str, account_id: &str) -> Result<(), String> {
            let body = if account_id.is_empty() {
                json!({ "accountId": Value::Null })
            } else {
                json!({ "accountId": account_id })
            };
            let resp = self
                .http
                .put(format!("{}/rest/api/3/issue/{key}/assignee", self.base))
                .header("Authorization", &self.auth_header)
                .json(&body)
                .send()
                .map_err(|e| e.to_string())?;
            if !resp.status().is_success() {
                let st = resp.status();
                let t = resp.text().unwrap_or_default();
                return Err(format!("assign failed {st}: {t}"));
            }
            Ok(())
        }

        /// Active + future sprints on a project's (first) scrum board: (id, state, name).
        pub fn list_sprints(&self, project: &str) -> Result<Vec<(i64, String, String)>, String> {
            let b = self
                .http
                .get(format!("{}/rest/agile/1.0/board?projectKeyOrId={project}&maxResults=50", self.base))
                .header("Authorization", &self.auth_header)
                .send()
                .map_err(|e| e.to_string())?;
            let bv: Value = b.json().map_err(|e| e.to_string())?;
            let board = bv["values"]
                .as_array()
                .and_then(|a| a.iter().find(|b| b["type"] == "scrum").or_else(|| a.first()))
                .and_then(|b| b["id"].as_i64());
            let Some(board) = board else { return Ok(vec![]) };
            let s = self
                .http
                .get(format!("{}/rest/agile/1.0/board/{board}/sprint?state=active,future&maxResults=50", self.base))
                .header("Authorization", &self.auth_header)
                .send()
                .map_err(|e| e.to_string())?;
            if !s.status().is_success() {
                return Ok(vec![]); // board without a sprint backlog, etc.
            }
            let sv: Value = s.json().map_err(|e| e.to_string())?;
            Ok(sv["values"]
                .as_array()
                .map(|a| a.iter().filter_map(|sp| Some((sp["id"].as_i64()?, sp["state"].as_str()?.to_string(), sp["name"].as_str()?.to_string()))).collect())
                .unwrap_or_default())
        }

        /// Move an issue into a sprint.
        pub fn add_to_sprint(&self, sprint_id: i64, issue: &str) -> Result<(), String> {
            let r = self
                .http
                .post(format!("{}/rest/agile/1.0/sprint/{sprint_id}/issue", self.base))
                .header("Authorization", &self.auth_header)
                .json(&json!({ "issues": [issue] }))
                .send()
                .map_err(|e| e.to_string())?;
            if !r.status().is_success() {
                return Err(format!("add to sprint failed: {}", r.status()));
            }
            Ok(())
        }
    }

    /// A JIRA project (for discovery).
    #[derive(Clone)]
    pub struct ProjectRef {
        pub key: String,
        pub name: String,
    }

    /// One issue pulled from JIRA for backfill.
    pub struct BackfillIssue {
        pub key: String,
        pub summary: String,
        pub reporter: String,
        pub description: String,
        pub status: String,
        /// The issue's JIRA `created` timestamp (ISO 8601, e.g. 2025-03-14T10:22:33.000+0000).
        pub created: String,
        /// Issue type name (Epic, Story, Task, Bug, Sub-task, …).
        pub issue_type: String,
        /// Parent/epic key, if the issue is a child (epic child or sub-task).
        pub parent_key: String,
        /// Parent/epic summary, for display.
        pub parent_summary: String,
        /// (author, body, created-ISO) per comment.
        pub comments: Vec<(String, String, String)>,
    }

    /// An epic in the Projects→Epics→Stories browser.
    pub struct EpicRef {
        pub key: String,
        pub summary: String,
        pub status: String,
    }

    /// A child issue (story/task/sub-task) under an epic.
    pub struct ChildRef {
        pub key: String,
        pub summary: String,
        pub issue_type: String,
        pub status: String,
    }

    /// One issue rendered as a conversation (its description + comments).
    pub struct IssueDetail {
        pub key: String,
        pub summary: String,
        pub description: String,
        pub status: String,
        pub issue_type: String,
        pub parent_key: String,
        pub parent_summary: String,
        pub author: String,
        pub created: String,
        pub assignee: String,
        pub assignee_id: String,
        pub comments: Vec<(String, String, String)>,
    }

    /// Flatten an ADF document to plain text (one line per block). Superseded by
    /// `crate::format::adf_to_mrkdwn` for backfill, but kept as a plain-text helper.
    #[allow(dead_code)]
    fn adf_text(v: &Value) -> String {
        v["content"]
            .as_array()
            .map(|blocks| {
                blocks
                    .iter()
                    .map(|b| {
                        b["content"]
                            .as_array()
                            .map(|ns| ns.iter().filter_map(|n| n["text"].as_str()).collect::<String>())
                            .unwrap_or_default()
                    })
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default()
    }

    impl RealBackend {
        /// Read-only: list ALL accessible projects, paging through the search API
        /// (used to import every board the user can access, not just the first 50).
        pub fn list_projects(&self) -> Result<Vec<ProjectRef>, String> {
            let mut out: Vec<ProjectRef> = Vec::new();
            let mut start = 0usize;
            loop {
                let start_s = start.to_string();
                let resp = self
                    .http
                    .get(format!("{}/rest/api/3/project/search", self.base))
                    .header("Authorization", &self.auth_header)
                    .query(&[("maxResults", "50"), ("startAt", start_s.as_str())])
                    .send()
                    .map_err(|e| e.to_string())?;
                let status = resp.status();
                let v: Value = resp.json().map_err(|e| e.to_string())?;
                if !status.is_success() {
                    return Err(format!("project search {status}: {v}"));
                }
                let vals = v["values"].as_array().cloned().unwrap_or_default();
                let n = vals.len();
                for p in vals {
                    if let Some(key) = p["key"].as_str() {
                        out.push(ProjectRef {
                            key: key.to_string(),
                            name: p["name"].as_str().unwrap_or("").to_string(),
                        });
                    }
                }
                if n == 0 || v["isLast"].as_bool().unwrap_or(true) || out.len() >= 2000 {
                    break;
                }
                start += n;
            }
            Ok(out)
        }

        /// Read-only: resolve a single project by key, returning its canonical key if
        /// the user can access it, else `None` (not found / no permission). Used to
        /// target a channel's project without the first-50 cap of `list_projects`.
        pub fn project_key_if_accessible(&self, key: &str) -> Result<Option<String>, String> {
            let resp = self
                .http
                .get(format!("{}/rest/api/3/project/{}", self.base, key))
                .header("Authorization", &self.auth_header)
                .send()
                .map_err(|e| e.to_string())?;
            let status = resp.status();
            if matches!(status.as_u16(), 403 | 404) {
                return Ok(None);
            }
            let v: Value = resp.json().map_err(|e| e.to_string())?;
            if !status.is_success() {
                return Err(format!("project lookup {status}: {v}"));
            }
            Ok(v["key"].as_str().map(str::to_string))
        }

        /// Read-only: pull up to `max` issues of a project (oldest first) with comments.
        pub fn backfill_issues(&self, project: &str, max: u32) -> Result<Vec<BackfillIssue>, String> {
            // Most-recently-updated first, so a big board surfaces current activity
            // instead of its oldest `max` issues (the client re-sorts for display).
            let jql = format!("project = \"{project}\" ORDER BY updated DESC");
            let mr = max.to_string();
            let resp = self
                .http
                .get(format!("{}/rest/api/3/search/jql", self.base))
                .header("Authorization", &self.auth_header)
                .query(&[
                    ("jql", jql.as_str()),
                    ("maxResults", mr.as_str()),
                    ("fields", "summary,description,comment,reporter,status,created,issuetype,parent"),
                ])
                .send()
                .map_err(|e| e.to_string())?;
            let status = resp.status();
            let v: Value = resp.json().map_err(|e| e.to_string())?;
            if !status.is_success() {
                return Err(format!("issue search {status}: {v}"));
            }
            Ok(v["issues"]
                .as_array()
                .map(|issues| {
                    issues
                        .iter()
                        .map(|i| {
                            let f = &i["fields"];
                            let comments = f["comment"]["comments"]
                                .as_array()
                                .map(|cs| {
                                    cs.iter()
                                        .map(|c| {
                                            (
                                                c["author"]["displayName"].as_str().unwrap_or("?").to_string(),
                                                crate::format::adf_to_mrkdwn(&c["body"]),
                                                c["created"].as_str().unwrap_or("").to_string(),
                                            )
                                        })
                                        .collect()
                                })
                                .unwrap_or_default();
                            BackfillIssue {
                                key: i["key"].as_str().unwrap_or("").to_string(),
                                summary: f["summary"].as_str().unwrap_or("").to_string(),
                                created: f["created"].as_str().unwrap_or("").to_string(),
                                issue_type: f["issuetype"]["name"].as_str().unwrap_or("").to_string(),
                                parent_key: f["parent"]["key"].as_str().unwrap_or("").to_string(),
                                parent_summary: f["parent"]["fields"]["summary"].as_str().unwrap_or("").to_string(),
                                reporter: f["reporter"]["displayName"].as_str().unwrap_or("JIRA").to_string(),
                                status: f["status"]["name"].as_str().unwrap_or("").to_string(),
                                description: if f["description"].is_null() {
                                    String::new()
                                } else {
                                    crate::format::adf_to_mrkdwn(&f["description"])
                                },
                                comments,
                            }
                        })
                        .collect()
                })
                .unwrap_or_default())
        }

        /// Read-only helper: run a JQL search and return the raw `issues` array.
        fn search(&self, jql: &str, fields: &str, max: u32) -> Result<Vec<Value>, String> {
            let mr = max.to_string();
            let resp = self
                .http
                .get(format!("{}/rest/api/3/search/jql", self.base))
                .header("Authorization", &self.auth_header)
                .query(&[("jql", jql), ("maxResults", mr.as_str()), ("fields", fields)])
                .send()
                .map_err(|e| e.to_string())?;
            let status = resp.status();
            let v: Value = resp.json().map_err(|e| e.to_string())?;
            if !status.is_success() {
                return Err(format!("search {status}: {v}"));
            }
            Ok(v["issues"].as_array().cloned().unwrap_or_default())
        }

        /// Read-only: epics in a project, most recently updated first.
        pub fn list_epics(&self, project: &str) -> Result<Vec<EpicRef>, String> {
            let jql = format!("project = \"{project}\" AND issuetype = Epic ORDER BY updated DESC");
            Ok(self
                .search(&jql, "summary,status", 200)?
                .iter()
                .map(|i| EpicRef {
                    key: i["key"].as_str().unwrap_or("").to_string(),
                    summary: i["fields"]["summary"].as_str().unwrap_or("").to_string(),
                    status: i["fields"]["status"]["name"].as_str().unwrap_or("").to_string(),
                })
                .collect())
        }

        /// Read-only: the children (stories/tasks/sub-tasks) under an issue (epic),
        /// oldest first.
        pub fn list_children(&self, parent: &str) -> Result<Vec<ChildRef>, String> {
            let jql = format!("parent = \"{parent}\" ORDER BY created ASC");
            Ok(self
                .search(&jql, "summary,issuetype,status", 300)?
                .iter()
                .map(|i| ChildRef {
                    key: i["key"].as_str().unwrap_or("").to_string(),
                    summary: i["fields"]["summary"].as_str().unwrap_or("").to_string(),
                    issue_type: i["fields"]["issuetype"]["name"].as_str().unwrap_or("").to_string(),
                    status: i["fields"]["status"]["name"].as_str().unwrap_or("").to_string(),
                })
                .collect())
        }

        /// Read-only: one issue's full content as a conversation (description + comments).
        pub fn fetch_issue(&self, key: &str) -> Result<IssueDetail, String> {
            let resp = self
                .http
                .get(format!("{}/rest/api/3/issue/{key}", self.base))
                .header("Authorization", &self.auth_header)
                .query(&[("fields", "summary,description,status,issuetype,parent,comment,created,reporter,assignee")])
                .send()
                .map_err(|e| e.to_string())?;
            let status = resp.status();
            let v: Value = resp.json().map_err(|e| e.to_string())?;
            if !status.is_success() {
                return Err(format!("issue {key} {status}: {v}"));
            }
            let f = &v["fields"];
            let comments = f["comment"]["comments"]
                .as_array()
                .map(|cs| {
                    cs.iter()
                        .map(|c| {
                            (
                                c["author"]["displayName"].as_str().unwrap_or("?").to_string(),
                                crate::format::adf_to_mrkdwn(&c["body"]),
                                c["created"].as_str().unwrap_or("").to_string(),
                            )
                        })
                        .collect()
                })
                .unwrap_or_default();
            Ok(IssueDetail {
                key: v["key"].as_str().unwrap_or(key).to_string(),
                summary: f["summary"].as_str().unwrap_or("").to_string(),
                description: if f["description"].is_null() { String::new() } else { crate::format::adf_to_mrkdwn(&f["description"]) },
                status: f["status"]["name"].as_str().unwrap_or("").to_string(),
                issue_type: f["issuetype"]["name"].as_str().unwrap_or("").to_string(),
                parent_key: f["parent"]["key"].as_str().unwrap_or("").to_string(),
                parent_summary: f["parent"]["fields"]["summary"].as_str().unwrap_or("").to_string(),
                author: f["reporter"]["displayName"].as_str().unwrap_or("JIRA").to_string(),
                created: f["created"].as_str().unwrap_or("").to_string(),
                assignee: f["assignee"]["displayName"].as_str().unwrap_or("").to_string(),
                assignee_id: f["assignee"]["accountId"].as_str().unwrap_or("").to_string(),
                comments,
            })
        }

        /// Upload a file as a JIRA attachment on an issue. Returns
        /// (jira attachment id, filename, mime, size). Requires the
        /// `X-Atlassian-Token: no-check` header for the attachment endpoint.
        pub fn add_attachment(&self, key: &str, filename: &str, bytes: Vec<u8>, mime: &str) -> Result<(String, String, String, i64), String> {
            let size = bytes.len() as i64;
            let part = reqwest::blocking::multipart::Part::bytes(bytes)
                .file_name(filename.to_string())
                .mime_str(if mime.is_empty() { "application/octet-stream" } else { mime })
                .map_err(|e| e.to_string())?;
            let form = reqwest::blocking::multipart::Form::new().part("file", part);
            let resp = self
                .http
                .post(format!("{}/rest/api/3/issue/{key}/attachments", self.base))
                .header("Authorization", &self.auth_header)
                .header("X-Atlassian-Token", "no-check")
                .multipart(form)
                .send()
                .map_err(|e| e.to_string())?;
            let status = resp.status();
            let v: Value = resp.json().map_err(|e| e.to_string())?;
            if !status.is_success() {
                return Err(format!("attachment upload {status}: {v}"));
            }
            let a = v.as_array().and_then(|a| a.first()).ok_or("empty attachment response")?;
            Ok((
                a["id"].as_str().unwrap_or("").to_string(),
                a["filename"].as_str().unwrap_or(filename).to_string(),
                a["mimeType"].as_str().unwrap_or(mime).to_string(),
                a["size"].as_i64().unwrap_or(size),
            ))
        }

        /// Download an attachment's bytes (for the gateway's auth proxy): (bytes, mime).
        pub fn fetch_attachment(&self, jira_id: &str) -> Result<(Vec<u8>, String), String> {
            let resp = self
                .http
                .get(format!("{}/rest/api/3/attachment/content/{jira_id}", self.base))
                .header("Authorization", &self.auth_header)
                .send()
                .map_err(|e| e.to_string())?;
            if !resp.status().is_success() {
                return Err(format!("attachment fetch failed: {}", resp.status()));
            }
            let mime = resp
                .headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("application/octet-stream")
                .to_string();
            let bytes = resp.bytes().map_err(|e| e.to_string())?.to_vec();
            Ok((bytes, mime))
        }

        /// Search users for mention autocomplete: up to `max` (accountId, display).
        pub fn search_users(&self, query: &str, max: usize) -> Result<Vec<(String, String)>, String> {
            let resp = self
                .http
                .get(format!("{}/rest/api/3/user/search", self.base))
                .header("Authorization", &self.auth_header)
                .query(&[("query", query)])
                .send()
                .map_err(|e| e.to_string())?;
            let status = resp.status();
            let v: Value = resp.json().map_err(|e| e.to_string())?;
            if !status.is_success() {
                return Err(format!("user search {status}: {v}"));
            }
            Ok(v.as_array()
                .map(|a| {
                    a.iter()
                        .filter(|u| u["accountType"].as_str() != Some("app"))
                        .filter_map(|u| {
                            Some((u["accountId"].as_str()?.to_string(), u["displayName"].as_str()?.to_string()))
                        })
                        .take(max)
                        .collect()
                })
                .unwrap_or_default())
        }

        /// Add or remove a single label on an issue (used to mirror a Weavn pin as
        /// the `weavn_pinned` JIRA label). Idempotent on JIRA's side.
        pub fn set_label(&self, key: &str, label: &str, on: bool) -> Result<(), String> {
            let op = if on { "add" } else { "remove" };
            let resp = self
                .http
                .put(format!("{}/rest/api/3/issue/{key}", self.base))
                .header("Authorization", &self.auth_header)
                .json(&json!({ "update": { "labels": [ { op: label } ] } }))
                .send()
                .map_err(|e| e.to_string())?;
            if !resp.status().is_success() {
                return Err(format!("set_label {} failed: {}", op, resp.status()));
            }
            Ok(())
        }
    }

    impl Backend for RealBackend {
        fn create_issue(
            &mut self,
            project: &str,
            channel_id: &ChannelId,
            summary: &str,
            _body_md: &str,
            body_adf: &Value,
        ) -> Result<CreatedIssue, String> {
            let mut fields = json!({
                "project": { "key": project },
                "issuetype": { "name": "Task" },
                "summary": summary,
                "description": body_adf,
            });
            if let (Value::Object(a), Value::Object(b)) =
                (&mut fields, self.channel_fields(channel_id))
            {
                a.extend(b);
            }
            let resp = self
                .http
                .post(format!("{}/rest/api/3/issue", self.base))
                .header("Authorization", &self.auth_header)
                .json(&json!({ "fields": fields }))
                .send()
                .map_err(|e| e.to_string())?;
            let status = resp.status();
            let body: Value = resp.json().map_err(|e| e.to_string())?;
            if !status.is_success() {
                return Err(format!("create_issue {status}: {body}"));
            }
            let key = body["key"]
                .as_str()
                .ok_or("no key in JIRA response")?
                .to_string();
            Ok(CreatedIssue { key })
        }

        fn set_description(
            &mut self,
            key: &IssueKey,
            _body_md: &str,
            body_adf: &Value,
        ) -> Result<(), String> {
            let resp = self
                .http
                .put(format!("{}/rest/api/3/issue/{key}", self.base))
                .header("Authorization", &self.auth_header)
                .json(&json!({ "fields": { "description": body_adf } }))
                .send()
                .map_err(|e| e.to_string())?;
            if !resp.status().is_success() {
                return Err(format!("append_to_description failed: {}", resp.status()));
            }
            Ok(())
        }

        fn add_comment(
            &mut self,
            key: &IssueKey,
            _author: &str,
            _body_md: &str,
            body_adf: &Value,
        ) -> Result<CreatedComment, String> {
            let resp = self
                .http
                .post(format!("{}/rest/api/3/issue/{key}/comment", self.base))
                .header("Authorization", &self.auth_header)
                .json(&json!({ "body": body_adf }))
                .send()
                .map_err(|e| e.to_string())?;
            let status = resp.status();
            let body: Value = resp.json().map_err(|e| e.to_string())?;
            if !status.is_success() {
                return Err(format!("add_comment {status}: {body}"));
            }
            Ok(CreatedComment {
                id: body["id"].as_str().unwrap_or("?").to_string(),
            })
        }

        fn update_fields(&mut self, key: &IssueKey, up: &TicketUpdate) -> Result<Vec<String>, String> {
            let mut applied = Vec::new();

            // priority, story points, label adds go in one PUT /issue
            let mut fields = serde_json::Map::new();
            let mut update = serde_json::Map::new();
            if let Some(pr) = &up.priority {
                fields.insert("priority".into(), json!({ "name": pr }));
                applied.push(format!("priority → {pr}"));
            }
            if let Some(p) = up.story_points {
                let field = std::env::var("WEAVN_POINTS_FIELD").map_err(|_| {
                    "story points need WEAVN_POINTS_FIELD (the Story Points custom field id)"
                        .to_string()
                })?;
                fields.insert(field, json!(p));
                applied.push(format!("points → {p}"));
            }
            if !up.add_labels.is_empty() {
                let ops: Vec<Value> = up.add_labels.iter().map(|l| json!({ "add": l })).collect();
                update.insert("labels".into(), Value::Array(ops));
                for l in &up.add_labels {
                    applied.push(format!("label + {l}"));
                }
            }
            if !fields.is_empty() || !update.is_empty() {
                let mut body = serde_json::Map::new();
                if !fields.is_empty() {
                    body.insert("fields".into(), Value::Object(fields));
                }
                if !update.is_empty() {
                    body.insert("update".into(), Value::Object(update));
                }
                let resp = self
                    .http
                    .put(format!("{}/rest/api/3/issue/{key}", self.base))
                    .header("Authorization", &self.auth_header)
                    .json(&Value::Object(body))
                    .send()
                    .map_err(|e| e.to_string())?;
                let status = resp.status();
                if !status.is_success() {
                    let b: Value = resp.json().unwrap_or(json!({}));
                    return Err(format!("field update {status}: {b}"));
                }
            }

            // assignee: resolve to an accountId, then PUT the assignee endpoint
            if let Some(q) = &up.assignee {
                let account = self.resolve_account_id(q)?;
                let resp = self
                    .http
                    .put(format!("{}/rest/api/3/issue/{key}/assignee", self.base))
                    .header("Authorization", &self.auth_header)
                    .json(&json!({ "accountId": account }))
                    .send()
                    .map_err(|e| e.to_string())?;
                if !resp.status().is_success() {
                    return Err(format!("assign failed: {}", resp.status()));
                }
                applied.push(format!("assignee → {q}"));
            }

            // status: apply a matching workflow transition
            if let Some(s) = &up.status {
                self.apply_transition(key, s)?;
                applied.push(format!("status → {s}"));
            }

            Ok(applied)
        }
    }
}
