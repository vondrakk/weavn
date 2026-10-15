//! The gateway logic: resolve a channel, send (with coalescing), reply, read.

use crate::jira::Backend;
use crate::proto::{self, ChannelId, ChannelPath};
use crate::store::Store;

pub struct Engine<B: Backend> {
    pub store: Store,
    pub backend: B,
    /// Storage project key (the physical shard). One shard in the prototype.
    pub project: String,
    /// Coalescing window, in logical ticks (one tick per send). See §9 #1.
    pub coalesce_window: u64,
}

#[derive(Debug)]
pub enum SendOutcome {
    NewIssue { issue: String },
    Coalesced { issue: String },
    Comment { issue: String, comment: String },
}

/// A top-level conversation as the board shows it: a root issue, any coalesced
/// fragments merged into its body, and its replies.
#[derive(Debug)]
pub struct Thread {
    pub issue_key: String,
    pub author: String,
    pub lines: Vec<String>,
    pub replies: Vec<Reply>,
}

#[derive(Debug)]
pub struct Reply {
    pub author: String,
    pub body: String,
    pub from_jira: bool,
}

impl<B: Backend> Engine<B> {
    pub fn new(store: Store, backend: B, project: impl Into<String>, coalesce_window: u64) -> Self {
        Self {
            store,
            backend,
            project: project.into(),
            coalesce_window,
        }
    }

    /// Send a top-level message to a channel path (created if new).
    pub fn send(&mut self, path_str: &str, author: &str, body: &str) -> Result<SendOutcome, String> {
        let path = ChannelPath::parse(path_str)?;
        let channel_id = self.store.resolve_path(&path);
        let adf = crate::format::mrkdwn_to_adf(body);

        let (now, target) = self
            .store
            .coalesce_target(&channel_id, author, self.coalesce_window);

        if let Some(issue_key) = target {
            self.store
                .add_fragment(&channel_id, &issue_key, author, body, now);
            // Send the full merged body, not just this fragment, so a real JIRA
            // description is updated correctly rather than overwritten.
            let merged = self.store.issue_body(&issue_key);
            let merged_adf = crate::format::mrkdwn_to_adf(&merged);
            self.backend
                .set_description(&issue_key, &merged, &merged_adf)?;
            return Ok(SendOutcome::Coalesced { issue: issue_key });
        }

        let summary = proto::summarize(body, 80);
        let created = self
            .backend
            .create_issue(&self.project, &channel_id, &summary, body, &adf)?;
        self.store
            .add_toplevel(&channel_id, &created.key, author, body, now);
        Ok(SendOutcome::NewIssue { issue: created.key })
    }

    /// Reply to an existing conversation (adds a JIRA comment).
    pub fn reply(&mut self, issue_key: &str, author: &str, body: &str) -> Result<SendOutcome, String> {
        let channel_id = self
            .store
            .channel_of_issue(issue_key)
            .ok_or_else(|| format!("unknown issue {issue_key}"))?;
        let adf = crate::format::mrkdwn_to_adf(body);
        let comment = self
            .backend
            .add_comment(&issue_key.to_string(), author, body, &adf)?;
        let now = self.store.clock + 1;
        self.store.clock = now;
        self.store
            .add_reply(&channel_id, issue_key, author, body, now, false);
        Ok(SendOutcome::Comment {
            issue: issue_key.to_string(),
            comment: comment.id,
        })
    }

    /// Refine a conversation's ticket from in-chat slash commands (§13), e.g.
    /// `/assign @alex`, `/points 5`, `/status in review`. Returns what changed.
    pub fn refine(&mut self, issue_key: &str, commands: &[String]) -> Result<Vec<String>, String> {
        let update = crate::slash::parse(commands)?;
        if update.is_empty() {
            return Err("no fields to update".to_string());
        }
        self.backend.update_fields(&issue_key.to_string(), &update)
    }

    /// Reconcile the projection from JIRA (the pull equivalent of webhook sync,
    /// docs/DESIGN.md §5): pull in any comments that exist on JIRA issues but not
    /// yet in the projection — e.g. a comment typed directly in the JIRA UI.
    /// Returns how many messages were pulled in. Idempotent: comments already
    /// mirrored are counted and skipped. Issues the projection has never seen
    /// (created directly in JIRA) are left for a fuller sync — out of scope here.
    pub fn sync(&mut self) -> usize {
        let mut pulled = 0;
        for issue in self.backend.dump() {
            let Some(channel) = self.store.channel_of_issue(&issue.key) else {
                continue;
            };
            let have = self
                .store
                .messages
                .iter()
                .filter(|m| m.parent_issue.as_deref() == Some(issue.key.as_str()))
                .count();
            for c in issue.comments.iter().skip(have) {
                let now = self.store.clock + 1;
                self.store.clock = now;
                self.store
                    .add_reply(&channel, &issue.key, &c.author, &c.body, now, true);
                pulled += 1;
            }
        }
        pulled
    }

    /// Read a channel (or, with `subtree`, a folder and all its descendants) as
    /// an ordered list of threads — the board view.
    pub fn read(&self, path_str: &str, subtree: bool) -> Result<Vec<Thread>, String> {
        let path = ChannelPath::parse(path_str)?;
        let channel_id = self
            .store
            .lookup_path(&path)
            .ok_or_else(|| format!("no such channel: {}", path.display()))?;

        let ids: Vec<ChannelId> = if subtree {
            self.store.descendants(&channel_id)
        } else {
            vec![channel_id]
        };

        // Group messages by issue, in order of first appearance.
        let mut order: Vec<String> = Vec::new();
        let mut threads: std::collections::HashMap<String, Thread> = std::collections::HashMap::new();

        let mut msgs: Vec<&crate::store::Message> = self
            .store
            .messages
            .iter()
            .filter(|m| ids.contains(&m.channel_id))
            .collect();
        msgs.sort_by_key(|m| m.seq);

        for m in msgs {
            let t = threads.entry(m.issue_key.clone()).or_insert_with(|| {
                order.push(m.issue_key.clone());
                Thread {
                    issue_key: m.issue_key.clone(),
                    author: m.author.clone(),
                    lines: Vec::new(),
                    replies: Vec::new(),
                }
            });
            match &m.parent_issue {
                None => t.lines.push(m.body.clone()),
                Some(_) => t.replies.push(Reply {
                    author: m.author.clone(),
                    body: m.body.clone(),
                    from_jira: m.from_jira,
                }),
            }
        }

        Ok(order.into_iter().map(|k| threads.remove(&k).unwrap()).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jira::MockBackend;

    fn engine() -> Engine<MockBackend> {
        Engine::new(Store::new(), MockBackend::new(), "BC", 1)
    }

    #[test]
    fn burst_from_same_author_coalesces_into_one_issue() {
        let mut e = engine();
        let a = e.send("eng/deploys", "alice", "rolling out v2").unwrap();
        let b = e.send("eng/deploys", "alice", "...almost done").unwrap();
        assert!(matches!(a, SendOutcome::NewIssue { .. }));
        assert!(matches!(b, SendOutcome::Coalesced { .. }));
        // One issue on the JIRA side, not two.
        assert_eq!(e.backend.dump().len(), 1);
    }

    #[test]
    fn different_author_breaks_coalescing() {
        let mut e = engine();
        e.send("eng/deploys", "alice", "shipping").unwrap();
        let b = e.send("eng/deploys", "bob", "nice").unwrap();
        assert!(matches!(b, SendOutcome::NewIssue { .. }));
        assert_eq!(e.backend.dump().len(), 2);
    }

    #[test]
    fn reply_becomes_a_comment_not_an_issue() {
        let mut e = engine();
        let SendOutcome::NewIssue { issue } = e.send("eng/deploys", "alice", "deploy failed").unwrap()
        else {
            panic!("expected new issue");
        };
        e.reply(&issue, "bob", "on it").unwrap();
        let dump = e.backend.dump();
        assert_eq!(dump.len(), 1);
        assert_eq!(dump[0].comments.len(), 1);
    }

    #[test]
    fn sync_pulls_a_jira_side_comment_into_chat() {
        let mut e = engine();
        let SendOutcome::NewIssue { issue } = e.send("eng/deploys", "alice", "deploy failed").unwrap()
        else {
            panic!("expected new issue");
        };
        // Someone comments directly in the JIRA UI (bypasses the engine).
        e.backend
            .add_comment(&issue, "ops-oncall", "restarted the worker", &serde_json::json!({}))
            .unwrap();
        // Chat doesn't know yet.
        assert_eq!(e.read("eng/deploys", false).unwrap()[0].replies.len(), 0);
        // Sync reconciles; now it's visible and flagged as JIRA-originated.
        assert_eq!(e.sync(), 1);
        let threads = e.read("eng/deploys", false).unwrap();
        assert_eq!(threads[0].replies.len(), 1);
        assert!(threads[0].replies[0].from_jira);
        // Idempotent.
        assert_eq!(e.sync(), 0);
    }

    #[test]
    fn refine_updates_ticket_fields() {
        let mut e = engine();
        let SendOutcome::NewIssue { issue } = e.send("eng/deploys", "alice", "deploy failed").unwrap()
        else {
            panic!("expected new issue");
        };
        let applied = e
            .refine(
                &issue,
                &[
                    "/assign @bob".into(),
                    "/points 3".into(),
                    "/priority High".into(),
                    "/label backend".into(),
                ],
            )
            .unwrap();
        assert_eq!(applied.len(), 4);
        let f = &e.backend.dump()[0].fields;
        assert_eq!(f.assignee.as_deref(), Some("bob"));
        assert_eq!(f.story_points, Some(3.0));
        assert_eq!(f.priority.as_deref(), Some("High"));
        assert_eq!(f.labels, vec!["backend"]);
    }

    #[test]
    fn hierarchy_and_subtree_read() {
        let mut e = engine();
        e.send("eng / backend / deploys", "alice", "a").unwrap();
        e.send("eng / backend / alerts", "bob", "b").unwrap();
        // leaf read sees one thread; subtree read at the folder sees both
        assert_eq!(e.read("eng/backend/deploys", false).unwrap().len(), 1);
        assert_eq!(e.read("eng/backend", true).unwrap().len(), 2);
    }
}
