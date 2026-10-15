//! The DM vault engine (docs/DESIGN.md §7).
//!
//! A DM conversation (participant set + time window) is one Jira issue in a
//! single "DM Vault" project; each message is a comment whose body is base64
//! ciphertext. Privacy lives here (and in `keys`), never in Jira ACLs.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::jira::{Backend, CommentRec};
use crate::keys::{self, KeyPair};

/// Persistent vault state: everyone's keypair, the org escrow key, and the
/// conversation -> issue index. In production the keyring lives in a key service,
/// not on disk; this is the prototype's stand-in.
#[derive(Serialize, Deserialize)]
pub struct VaultState {
    pub keyring: BTreeMap<String, KeyPair>,
    pub escrow: KeyPair,
    pub index: BTreeMap<String, String>, // conv_key -> issue key
    pub project: String,
}

impl Default for VaultState {
    fn default() -> Self {
        Self {
            keyring: BTreeMap::new(),
            escrow: keys::generate(),
            index: BTreeMap::new(),
            project: "DMV".to_string(),
        }
    }
}

impl VaultState {
    pub fn new() -> Self {
        Self::default()
    }

    fn ensure_user(&mut self, user: &str) -> String {
        self.keyring
            .entry(normalize(user))
            .or_insert_with(keys::generate)
            .public
            .clone()
    }
}

/// A conversation identity: sorted, de-duplicated, lowercased participants.
/// (A real deployment also folds in a time window to cap comments per issue.)
pub fn conv_key(participants: &[String]) -> String {
    let mut p: Vec<String> = participants
        .iter()
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty())
        .collect();
    p.sort();
    p.dedup();
    p.join(",")
}

/// One decrypted (or locked) message as read back.
pub struct DmLine {
    pub from: String,
    pub text: String, // plaintext, or "[locked]" if the reader is not a recipient
}

pub struct Vault<'a, B: Backend> {
    pub backend: &'a mut B,
    pub state: &'a mut VaultState,
}

impl<'a, B: Backend> Vault<'a, B> {
    pub fn new(backend: &'a mut B, state: &'a mut VaultState) -> Self {
        Self { backend, state }
    }

    /// Encrypt and store a DM. Returns the vault issue key.
    pub fn send(&mut self, participants: &[String], from: &str, body: &str) -> Result<String, String> {
        let mut parts = participants.to_vec();
        if !parts.iter().any(|p| p.eq_ignore_ascii_case(from)) {
            parts.push(from.to_string());
        }
        for p in &parts {
            self.state.ensure_user(p);
        }
        let ck = conv_key(&parts);

        // recipients = every participant + the org escrow key
        let mut recipients: Vec<String> = ck
            .split(',')
            .filter_map(|u| self.state.keyring.get(u).map(|k| k.public.clone()))
            .collect();
        recipients.push(self.state.escrow.public.clone());

        let ciphertext = keys::encrypt_to(&recipients, body.as_bytes())?;

        let issue = match self.state.index.get(&ck) {
            Some(k) => k.clone(),
            None => {
                let adf = crate::format::mrkdwn_to_adf("(encrypted DM)");
                let created = self.backend.create_issue(
                    &self.state.project,
                    &ck,
                    "(encrypted DM)",
                    "(encrypted DM)",
                    &adf,
                )?;
                self.state.index.insert(ck.clone(), created.key.clone());
                created.key
            }
        };

        // Comment author = sender (metadata the prototype leaves in the clear);
        // comment body = ciphertext. A real build would encrypt the author too.
        let adf = crate::format::mrkdwn_to_adf("");
        self.backend.add_comment(&issue, from, &ciphertext, &adf)?;
        Ok(issue)
    }

    fn comments_for(&self, issue: &str) -> Vec<CommentRec> {
        self.backend
            .dump()
            .into_iter()
            .find(|i| i.key == issue)
            .map(|i| i.comments)
            .unwrap_or_default()
    }

    /// Read a conversation, decrypting with `as_user`'s identity. Messages the
    /// reader cannot open come back as `[locked]`.
    pub fn read_as(&mut self, participants: &[String], as_user: &str) -> Result<Vec<DmLine>, String> {
        self.state.ensure_user(as_user); // an outsider gets a key — just not a recipient
        let ck = conv_key(participants);
        let issue = self
            .state
            .index
            .get(&ck)
            .cloned()
            .ok_or_else(|| format!("no DM conversation for [{ck}]"))?;
        let secret = self.state.keyring[&normalize(as_user)].secret.clone();
        Ok(self.decode(&issue, &secret))
    }

    /// Org-escrow recovery: decrypt the whole conversation with the escrow key.
    pub fn recover(&self, participants: &[String]) -> Result<Vec<DmLine>, String> {
        let ck = conv_key(participants);
        let issue = self
            .state
            .index
            .get(&ck)
            .cloned()
            .ok_or_else(|| format!("no DM conversation for [{ck}]"))?;
        Ok(self.decode(&issue, &self.state.escrow.secret))
    }

    /// Raw ciphertext as Jira (or an admin) would see it.
    pub fn raw(&self, participants: &[String]) -> Result<Vec<String>, String> {
        let ck = conv_key(participants);
        let issue = self
            .state
            .index
            .get(&ck)
            .cloned()
            .ok_or_else(|| format!("no DM conversation for [{ck}]"))?;
        Ok(self
            .comments_for(&issue)
            .into_iter()
            .map(|c| format!("{}|{}", c.author, c.body))
            .collect())
    }

    fn decode(&self, issue: &str, secret: &str) -> Vec<DmLine> {
        self.comments_for(issue)
            .iter()
            .map(|c| {
                let text = match keys::decrypt_with(secret, &c.body) {
                    Ok(bytes) => String::from_utf8_lossy(&bytes).to_string(),
                    Err(_) => "[locked]".to_string(),
                };
                DmLine {
                    from: c.author.clone(),
                    text,
                }
            })
            .collect()
    }
}

fn normalize(user: &str) -> String {
    user.trim().to_lowercase()
}
