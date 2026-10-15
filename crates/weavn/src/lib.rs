//! Weavn prototype library.
//!
//! The walking skeleton proves the core thesis end to end:
//!   parse a hierarchical channel path -> resolve the channel tree ->
//!   send a message (coalescing bursts) -> write to a JIRA backend ->
//!   read it back as a board view.
//!
//! Modules map to the production crate split in docs/DESIGN.md §11.

pub mod proto; // shared types: channel paths, ids
pub mod format; // mrkdwn -> ADF (Atlassian Document Format)
pub mod jira; // backend trait + mock (+ real behind the `real-jira` feature)
pub mod store; // projection: channel tree + message log
pub mod engine; // the gateway logic: resolve / send / reply / read
pub mod keys; // DM vault crypto (age): content key wrapped per participant + escrow
pub mod vault; // DM vault engine: encrypted conversations as vault issues
pub mod slash; // slash-command parsing for in-chat ticket refinement
