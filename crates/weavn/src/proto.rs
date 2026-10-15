//! Shared types.

use serde::{Deserialize, Serialize};

pub type ChannelId = String;
pub type IssueKey = String;

/// A human channel name as a hierarchical path, e.g. `eng / backend / deploys`.
///
/// `/` separates segments; spaces are allowed inside a segment. Identity within a
/// parent folds case and whitespace (`Eng/Backend` == `eng / backend`) while the
/// display casing from first creation is preserved. See docs/DESIGN.md §1.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelPath {
    pub segments: Vec<Segment>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Segment {
    /// As typed: trimmed, inner whitespace collapsed to single spaces.
    pub display: String,
    /// Fold key used for identity within a parent (lowercased display).
    pub key: String,
}

impl ChannelPath {
    /// Parse and canonicalize. Rejects empty segments (`//`, leading/trailing `/`).
    pub fn parse(input: &str) -> Result<ChannelPath, String> {
        let mut segments = Vec::new();
        for raw in input.split('/') {
            let display = collapse_ws(raw);
            if display.is_empty() {
                return Err(format!(
                    "invalid channel path {input:?}: empty segment (no //, leading or trailing /)"
                ));
            }
            let key = display.to_lowercase();
            segments.push(Segment { display, key });
        }
        if segments.is_empty() {
            return Err("empty channel path".to_string());
        }
        Ok(ChannelPath { segments })
    }

    /// Canonical display form, e.g. `eng / backend / deploys`.
    pub fn display(&self) -> String {
        self.segments
            .iter()
            .map(|s| s.display.as_str())
            .collect::<Vec<_>>()
            .join(" / ")
    }
}

/// Trim and collapse any run of whitespace to a single space.
fn collapse_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A set of ticket-field changes to apply to a conversation-issue. Every field is
/// optional; refinement (§13) typically sets one or two at a time.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct TicketUpdate {
    pub assignee: Option<String>, // a name/email query, resolved to an account by the backend
    pub status: Option<String>,   // target status name (applied as a workflow transition)
    pub story_points: Option<f64>,
    pub priority: Option<String>,
    pub add_labels: Vec<String>,
}

impl TicketUpdate {
    pub fn is_empty(&self) -> bool {
        self.assignee.is_none()
            && self.status.is_none()
            && self.story_points.is_none()
            && self.priority.is_none()
            && self.add_labels.is_empty()
    }
}

/// First line of a message body, trimmed to `max` chars, used as the issue summary.
pub fn summarize(body: &str, max: usize) -> String {
    let line = body.lines().next().unwrap_or("").trim();
    let line = if line.is_empty() { "(message)" } else { line };
    let mut out: String = line.chars().take(max).collect();
    if line.chars().count() > max {
        out.push('…');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folds_case_and_whitespace() {
        let a = ChannelPath::parse("Eng/Backend").unwrap();
        let b = ChannelPath::parse("  eng /   backend  ").unwrap();
        let akeys: Vec<_> = a.segments.iter().map(|s| s.key.clone()).collect();
        let bkeys: Vec<_> = b.segments.iter().map(|s| s.key.clone()).collect();
        assert_eq!(akeys, bkeys);
        assert_eq!(a.segments[0].display, "Eng"); // display casing preserved
    }

    #[test]
    fn allows_spaces_in_segments() {
        let p = ChannelPath::parse("eng / release planning").unwrap();
        assert_eq!(p.segments.len(), 2);
        assert_eq!(p.segments[1].display, "release planning");
    }

    #[test]
    fn rejects_empty_segments() {
        assert!(ChannelPath::parse("eng//deploys").is_err());
        assert!(ChannelPath::parse("/eng").is_err());
        assert!(ChannelPath::parse("eng/").is_err());
    }

    #[test]
    fn summary_truncates() {
        assert_eq!(summarize("hello world", 5), "hello…");
        assert_eq!(summarize("one\ntwo", 10), "one");
    }
}
