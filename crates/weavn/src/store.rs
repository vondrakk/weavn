//! The projection: a channel tree plus an ordered message log.
//!
//! This is a cache that can be rebuilt from JIRA (docs/DESIGN.md §10). The
//! prototype keeps it in memory and lets `weavn` snapshot it to JSON.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::proto::{ChannelId, ChannelPath, IssueKey};

/// One node in the channel tree. `parent = None` is a top-level channel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Channel {
    pub id: ChannelId,
    pub parent: Option<ChannelId>,
    pub segment: String, // display
    pub key: String,     // fold key, unique within a parent
}

/// A message in the log. `parent_issue = Some` means it is a reply (a JIRA
/// comment); `None` means it is top-level (lives on an issue of its own, unless
/// `coalesced`, in which case it was appended to the previous issue).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub seq: u64,
    pub channel_id: ChannelId,
    pub issue_key: IssueKey,
    pub parent_issue: Option<IssueKey>,
    pub author: String,
    pub body: String,
    pub coalesced: bool,
    /// True if this message was pulled in from JIRA by sync, not sent via chat.
    #[serde(default)]
    pub from_jira: bool,
}

#[derive(Debug, Clone)]
struct LastPost {
    author: String,
    issue_key: IssueKey,
    at: u64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Store {
    pub channels: HashMap<ChannelId, Channel>,
    pub messages: Vec<Message>,
    pub chan_counter: u64,
    pub clock: u64,
    // Rebuilt from `channels` on load; Option<String> keys don't serialize as a map.
    #[serde(skip)]
    children: HashMap<Option<ChannelId>, Vec<ChannelId>>,
    // Coalescing is a live, per-process concern; not persisted.
    #[serde(skip)]
    last_post: HashMap<ChannelId, LastPost>,
}

impl Store {
    pub fn new() -> Self {
        Self::default()
    }

    /// Rebuild the `children` index after deserializing. Children keep insertion
    /// order via the numeric id suffix.
    pub fn reindex(&mut self) {
        self.children.clear();
        let mut ids: Vec<&ChannelId> = self.channels.keys().collect();
        ids.sort_by_key(|id| id_num(id));
        for id in ids {
            let parent = self.channels[id].parent.clone();
            self.children.entry(parent).or_default().push(id.clone());
        }
    }

    fn tick(&mut self) -> u64 {
        self.clock += 1;
        self.clock
    }

    /// Resolve a path, creating any missing tree nodes, and return the leaf id.
    pub fn resolve_path(&mut self, path: &ChannelPath) -> ChannelId {
        let mut parent: Option<ChannelId> = None;
        for seg in &path.segments {
            let id = self.resolve_segment(parent.clone(), &seg.key, &seg.display);
            parent = Some(id);
        }
        parent.expect("ChannelPath::parse guarantees >= 1 segment")
    }

    /// Resolve a path without creating anything; `None` if it doesn't exist.
    pub fn lookup_path(&self, path: &ChannelPath) -> Option<ChannelId> {
        let mut parent: Option<ChannelId> = None;
        for seg in &path.segments {
            let kids = self.children.get(&parent)?;
            let found = kids.iter().find(|cid| self.channels[*cid].key == seg.key)?;
            parent = Some(found.clone());
        }
        parent
    }

    fn resolve_segment(
        &mut self,
        parent: Option<ChannelId>,
        key: &str,
        display: &str,
    ) -> ChannelId {
        if let Some(kids) = self.children.get(&parent) {
            for cid in kids {
                if self.channels[cid].key == key {
                    return cid.clone();
                }
            }
        }
        self.chan_counter += 1;
        let id = format!("c{}", self.chan_counter);
        self.channels.insert(
            id.clone(),
            Channel {
                id: id.clone(),
                parent: parent.clone(),
                segment: display.to_string(),
                key: key.to_string(),
            },
        );
        self.children.entry(parent).or_default().push(id.clone());
        id
    }

    /// Display path for a channel, e.g. `eng / backend / deploys`.
    pub fn path_of(&self, id: &ChannelId) -> String {
        let mut segs = Vec::new();
        let mut cur = Some(id.clone());
        while let Some(cid) = cur {
            let ch = &self.channels[&cid];
            segs.push(ch.segment.clone());
            cur = ch.parent.clone();
        }
        segs.reverse();
        segs.join(" / ")
    }

    /// All descendant channel ids of `id`, inclusive — used for subtree boards.
    pub fn descendants(&self, id: &ChannelId) -> Vec<ChannelId> {
        let mut out = vec![id.clone()];
        if let Some(kids) = self.children.get(&Some(id.clone())) {
            for k in kids {
                out.extend(self.descendants(k));
            }
        }
        out
    }

    pub fn children_of(&self, parent: Option<&ChannelId>) -> Vec<ChannelId> {
        self.children
            .get(&parent.cloned())
            .cloned()
            .unwrap_or_default()
    }

    /// The merged top-level body of an issue: all its non-reply messages
    /// (root + coalesced fragments) joined in sequence order.
    pub fn issue_body(&self, issue_key: &str) -> String {
        let mut msgs: Vec<&Message> = self
            .messages
            .iter()
            .filter(|m| m.issue_key == issue_key && m.parent_issue.is_none())
            .collect();
        msgs.sort_by_key(|m| m.seq);
        msgs.iter()
            .map(|m| m.body.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub fn channel_of_issue(&self, issue_key: &str) -> Option<ChannelId> {
        self.messages
            .iter()
            .find(|m| m.issue_key == issue_key)
            .map(|m| m.channel_id.clone())
    }

    // -- recording messages ------------------------------------------------

    /// Should a new top-level post by `author` in `channel` coalesce into the
    /// previous issue? Returns the issue key if so.
    pub fn coalesce_target(
        &mut self,
        channel: &ChannelId,
        author: &str,
        window: u64,
    ) -> (u64, Option<IssueKey>) {
        let now = self.tick();
        let target = self.last_post.get(channel).and_then(|lp| {
            if lp.author == author && now.saturating_sub(lp.at) <= window {
                Some(lp.issue_key.clone())
            } else {
                None
            }
        });
        (now, target)
    }

    pub fn add_toplevel(
        &mut self,
        channel: &ChannelId,
        issue_key: &str,
        author: &str,
        body: &str,
        at: u64,
    ) {
        self.messages.push(Message {
            seq: at,
            channel_id: channel.clone(),
            issue_key: issue_key.to_string(),
            parent_issue: None,
            author: author.to_string(),
            body: body.to_string(),
            coalesced: false,
            from_jira: false,
        });
        self.last_post.insert(
            channel.clone(),
            LastPost {
                author: author.to_string(),
                issue_key: issue_key.to_string(),
                at,
            },
        );
    }

    pub fn add_fragment(
        &mut self,
        channel: &ChannelId,
        issue_key: &str,
        author: &str,
        body: &str,
        at: u64,
    ) {
        self.messages.push(Message {
            seq: at,
            channel_id: channel.clone(),
            issue_key: issue_key.to_string(),
            parent_issue: None,
            author: author.to_string(),
            body: body.to_string(),
            coalesced: true,
            from_jira: false,
        });
        if let Some(lp) = self.last_post.get_mut(channel) {
            lp.at = at; // extend the window across the burst
        }
    }

    pub fn add_reply(
        &mut self,
        channel: &ChannelId,
        issue_key: &str,
        author: &str,
        body: &str,
        at: u64,
        from_jira: bool,
    ) {
        self.messages.push(Message {
            seq: at,
            channel_id: channel.clone(),
            issue_key: issue_key.to_string(),
            parent_issue: Some(issue_key.to_string()),
            author: author.to_string(),
            body: body.to_string(),
            coalesced: false,
            from_jira,
        });
        // A reply closes the coalescing window on that issue.
        if self
            .last_post
            .get(channel)
            .is_some_and(|lp| lp.issue_key == issue_key)
        {
            self.last_post.remove(channel);
        }
    }
}

fn id_num(id: &str) -> u64 {
    id.trim_start_matches('c').parse().unwrap_or(0)
}
