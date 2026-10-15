//! `weavn` — a terminal client for the Weavn prototype.
//!
//! Default build talks to an in-process mock JIRA, snapshotted to
//! ./.weavn/state.json so send/read/reply persist across runs.
//! `weavn demo` runs a self-contained scenario (no state file) that shows
//! coalescing, threading, hierarchy and the JIRA-side mapping in one shot.

use std::path::PathBuf;

use weavn::engine::{Engine, SendOutcome, Thread};
use weavn::jira::{Backend, MockBackend};
use weavn::proto::ChannelPath;
use weavn::store::Store;
use weavn::vault::{DmLine, Vault, VaultState};
use serde::{Deserialize, Serialize};

const COALESCE_WINDOW: u64 = 3;

#[derive(Default, Serialize, Deserialize)]
struct State {
    store: Store,
    mock: MockBackend,
    #[serde(default)]
    vault: VaultState,
}

fn state_path() -> PathBuf {
    PathBuf::from(".weavn/state.json")
}

fn load() -> State {
    let p = state_path();
    match std::fs::read_to_string(&p) {
        Ok(s) => {
            let mut st: State = serde_json::from_str(&s).unwrap_or_default();
            st.store.reindex();
            st
        }
        Err(_) => State::default(),
    }
}

fn save(st: &State) -> Result<(), String> {
    let p = state_path();
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let s = serde_json::to_string_pretty(st).map_err(|e| e.to_string())?;
    std::fs::write(&p, s).map_err(|e| e.to_string())
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().map(String::as_str).unwrap_or("help");
    let rest = &args.get(1..).unwrap_or(&[]);

    let result = match cmd {
        "parse" => cmd_parse(rest),
        "send" => cmd_send(rest),
        "reply" => cmd_reply(rest),
        "read" => cmd_read(rest),
        "tree" => cmd_tree(),
        "demo" => cmd_demo(),
        "dm" => cmd_dm(rest),
        "dm-demo" => cmd_dm_demo(),
        "jira-comment" => cmd_jira_comment(rest),
        "sync" => cmd_sync(),
        "sync-demo" => cmd_sync_demo(),
        "refine" => cmd_refine(rest),
        "show" => cmd_show(rest),
        #[cfg(feature = "real-jira")]
        "real-refine" => cmd_real_refine(rest),
        #[cfg(feature = "real-jira")]
        "jira-verify" => cmd_jira_verify(),
        #[cfg(feature = "real-jira")]
        "real-check" => cmd_real_check(rest),
        #[cfg(feature = "real-jira")]
        "real-demo" => cmd_real_demo(),
        "help" | "-h" | "--help" => {
            usage();
            Ok(())
        }
        other => Err(format!("unknown command {other:?}\n\n{USAGE}")),
    };

    if let Err(e) = result {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

const USAGE: &str = "\
weavn — Weavn prototype CLI

  weavn demo                              run a self-contained scenario (no state file)
  weavn parse <path>                      show how a channel path canonicalizes
  weavn send  <channel> <author> <body>   post a top-level message
  weavn reply <ISSUE-KEY> <author> <body> reply to a conversation (a JIRA comment)
  weavn read  [--subtree] <channel>       show a channel (or folder) as a board
  weavn tree                              print the channel tree

  weavn jira-comment <ISSUE> <who> <body> simulate a comment typed directly in JIRA
  weavn sync                              pull JIRA-side changes into chat (reconcile)
  weavn sync-demo                         self-contained bidirectional-sync scenario

  weavn refine <ISSUE> <slash>...         refine a ticket: /assign @x /points 5 /status s /priority p /label l
  weavn show   <ISSUE>                    show a ticket's fields

  weavn dm-demo                           self-contained encrypted-DM scenario
  weavn dm send    <a,b> <from> <body>    send an encrypted DM
  weavn dm read    <a,b> <as-user>        read a DM (decrypts as that user)
  weavn dm recover <a,b>                  org-escrow recovery of a DM
  weavn dm raw     <a,b>                  show ciphertext as JIRA/an admin sees it

Channels are hierarchical paths: \"eng / backend / deploys\" (slashes group,
spaces allowed). State persists in ./.weavn/state.json.";

fn usage() {
    println!("{USAGE}");
}

fn cmd_parse(rest: &[String]) -> Result<(), String> {
    let raw = rest.join(" ");
    if raw.trim().is_empty() {
        return Err("usage: weavn parse <path>".into());
    }
    let p = ChannelPath::parse(&raw)?;
    println!("display : {}", p.display());
    print!("segments:");
    for s in &p.segments {
        print!(" [{} | key={}]", s.display, s.key);
    }
    println!();
    Ok(())
}

fn split_cmd<'a>(rest: &'a [String], n: usize, usage: &str) -> Result<(Vec<&'a str>, String), String> {
    if rest.len() < n + 1 {
        return Err(usage.into());
    }
    let head: Vec<&str> = rest[..n].iter().map(String::as_str).collect();
    let body = rest[n..].join(" ");
    Ok((head, body))
}

fn cmd_send(rest: &[String]) -> Result<(), String> {
    let (head, body) = split_cmd(rest, 2, "usage: weavn send <channel> <author> <body>")?;
    let (channel, author) = (head[0], head[1]);
    let mut st = load();
    let mut engine = Engine::new(st.store, st.mock, "BC", COALESCE_WINDOW);
    let outcome = engine.send(channel, author, &body)?;
    st.store = engine.store;
    st.mock = engine.backend;
    save(&st)?;
    report(&outcome);
    Ok(())
}

fn cmd_reply(rest: &[String]) -> Result<(), String> {
    let (head, body) = split_cmd(rest, 2, "usage: weavn reply <ISSUE-KEY> <author> <body>")?;
    let (issue, author) = (head[0], head[1]);
    let mut st = load();
    let mut engine = Engine::new(st.store, st.mock, "BC", COALESCE_WINDOW);
    let outcome = engine.reply(issue, author, &body)?;
    st.store = engine.store;
    st.mock = engine.backend;
    save(&st)?;
    report(&outcome);
    Ok(())
}

fn report(outcome: &SendOutcome) {
    match outcome {
        SendOutcome::NewIssue { issue } => println!("created new conversation → issue {issue}"),
        SendOutcome::Coalesced { issue } => {
            println!("coalesced into recent burst → issue {issue} (no new issue)")
        }
        SendOutcome::Comment { issue, comment } => {
            println!("replied on {issue} → comment {comment}")
        }
    }
}

fn cmd_read(rest: &[String]) -> Result<(), String> {
    let (subtree, path_args): (bool, Vec<&String>) = match rest.split_first() {
        Some((flag, tail)) if flag == "--subtree" => (true, tail.iter().collect()),
        _ => (false, rest.iter().collect()),
    };
    let channel = path_args
        .iter()
        .map(|s| s.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    if channel.trim().is_empty() {
        return Err("usage: weavn read [--subtree] <channel>".into());
    }
    let st = load();
    let engine = Engine::new(st.store, st.mock, "BC", COALESCE_WINDOW);
    let threads = engine.read(&channel, subtree)?;
    print_board(&channel, subtree, &threads);
    Ok(())
}

fn print_board(channel: &str, subtree: bool, threads: &[Thread]) {
    let scope = if subtree { " (subtree)" } else { "" };
    println!("# {channel}{scope} — {} conversation(s)\n", threads.len());
    for t in threads {
        println!("┌─ {} · {}", t.issue_key, t.author);
        for line in &t.lines {
            for (i, l) in line.lines().enumerate() {
                println!("│  {}{l}", if i == 0 { "" } else { "  " });
            }
        }
        for r in &t.replies {
            let tag = if r.from_jira { "  (from JIRA)" } else { "" };
            println!("│    ↳ {}: {}{tag}", r.author, r.body);
        }
        println!("└─");
    }
}

fn cmd_refine(rest: &[String]) -> Result<(), String> {
    if rest.len() < 2 {
        return Err("usage: weavn refine <ISSUE-KEY> <slash command>...  e.g. weavn refine BC-1 \"/assign @bob\" \"/points 5\"".into());
    }
    let issue = &rest[0];
    let commands = rest[1..].to_vec();
    let mut st = load();
    let applied = {
        let mut engine = Engine::new(st.store, st.mock, "BC", COALESCE_WINDOW);
        let r = engine.refine(issue, &commands);
        st.store = engine.store;
        st.mock = engine.backend;
        r?
    };
    save(&st)?;
    println!("refined {issue}:");
    for a in &applied {
        println!("  {a}");
    }
    Ok(())
}

fn cmd_show(rest: &[String]) -> Result<(), String> {
    let issue = rest.first().ok_or("usage: weavn show <ISSUE-KEY>")?;
    let st = load();
    let found = st.mock.dump().into_iter().find(|i| &i.key == issue);
    let i = found.ok_or_else(|| format!("no such issue {issue}"))?;
    println!("{} — {}", i.key, i.summary);
    let f = &i.fields;
    println!("  assignee : {}", f.assignee.as_deref().unwrap_or("—"));
    println!("  status   : {}", f.status.as_deref().unwrap_or("—"));
    println!(
        "  points   : {}",
        f.story_points.map(|p| p.to_string()).unwrap_or_else(|| "—".into())
    );
    println!("  priority : {}", f.priority.as_deref().unwrap_or("—"));
    println!("  labels   : {}", if f.labels.is_empty() { "—".to_string() } else { f.labels.join(", ") });
    Ok(())
}

fn cmd_tree() -> Result<(), String> {
    let st = load();
    print_tree(&st.store);
    Ok(())
}

fn print_tree(store: &Store) {
    fn walk(store: &Store, parent: Option<&String>, depth: usize) {
        for id in store.children_of(parent) {
            let ch = &store.channels[&id];
            let has_msgs = store.messages.iter().any(|m| m.channel_id == id && m.parent_issue.is_none());
            let kind = if has_msgs { "#" } else { "📁" };
            println!("{}{kind} {}", "  ".repeat(depth), ch.segment);
            walk(store, Some(&id), depth + 1);
        }
    }
    if store.channels.is_empty() {
        println!("(no channels yet — try `weavn send \"eng / backend / deploys\" alice \"hello\"`)");
        return;
    }
    println!("channel tree ( # = has messages, 📁 = folder ):");
    walk(store, None, 0);
}

fn cmd_demo() -> Result<(), String> {
    let mut engine = Engine::new(Store::new(), MockBackend::new(), "BC", COALESCE_WINDOW);

    // A burst from one author collapses into a single conversation.
    let first = engine.send("eng / backend / deploys", "alice", "Starting the *v2.3* rollout now")?;
    engine.send("eng / backend / deploys", "alice", "canary looks healthy")?;
    engine.send("eng / backend / deploys", "alice", "promoting to 100%")?;

    // A different author starts a new conversation in the same channel.
    let migration = engine.send("eng / backend / deploys", "bob", "Did the `schema` migration run?")?;

    // Replies become comments on that conversation's issue.
    if let SendOutcome::NewIssue { issue } = &migration {
        engine.reply(issue, "alice", "yes — ran at 14:02, all green")?;
        engine.reply(issue, "bob", "thanks")?;
    }

    // Sibling channels and spaces in segment names.
    engine.send("eng / backend / alerts", "carol", "p99 latency spike on checkout")?;
    engine.send("eng / release planning", "dave", "cutting the 2.4 branch Friday")?;

    let root = match &first {
        SendOutcome::NewIssue { issue } => issue.clone(),
        _ => "BC-1".to_string(),
    };

    println!("=== Weavn demo ===\n");
    print_tree(&engine.store);

    println!("\n--- board: eng / backend  (subtree) ---\n");
    let threads = engine.read("eng / backend", true)?;
    print_board("eng / backend", true, &threads);

    println!("\n--- what JIRA actually stores ---\n");
    for issue in engine.backend.dump() {
        println!("{} [{}]  channel={}", issue.key, issue.project, issue.channel_id);
        println!("  summary: {}", issue.summary);
        for (i, line) in issue.description.lines().enumerate() {
            println!("  {} {line}", if i == 0 { "desc :" } else { "      " });
        }
        for c in &issue.comments {
            println!("  comment ({}): {}", c.author, c.body);
        }
        println!();
    }

    println!("note: the {root} conversation holds alice's 3-message burst as one issue");
    println!("      (coalescing, §9 #1) — not three issues.");
    Ok(())
}

fn cmd_jira_comment(rest: &[String]) -> Result<(), String> {
    let (head, body) = split_cmd(rest, 2, "usage: weavn jira-comment <ISSUE-KEY> <who> <body>")?;
    let (issue, who) = (head[0], head[1]);
    let mut st = load();
    // Write straight to the mock JIRA, bypassing the engine — exactly what a
    // comment typed in the JIRA web UI would do. The projection doesn't know yet.
    let adf = serde_json::json!({});
    st.mock.add_comment(&issue.to_string(), who, &body, &adf)?;
    save(&st)?;
    println!("added a JIRA-side comment on {issue} by {who} (run `weavn sync` to pull it into chat)");
    Ok(())
}

fn cmd_sync() -> Result<(), String> {
    let mut st = load();
    let pulled = {
        let mut engine = Engine::new(st.store, st.mock, "BC", COALESCE_WINDOW);
        let n = engine.sync();
        st.store = engine.store;
        st.mock = engine.backend;
        n
    };
    save(&st)?;
    println!("synced: pulled {pulled} JIRA-side message(s) into the projection");
    Ok(())
}

fn cmd_sync_demo() -> Result<(), String> {
    let mut engine = Engine::new(Store::new(), MockBackend::new(), "BC", COALESCE_WINDOW);

    let issue = match engine.send("eng / backend / deploys", "alice", "deploy to prod failed")? {
        SendOutcome::NewIssue { issue } => issue,
        _ => unreachable!(),
    };
    engine.reply(&issue, "bob", "looking now")?;

    println!("=== Weavn bidirectional-sync demo ===\n");
    println!("--- chat view before anyone touches JIRA ---\n");
    print_board("eng / backend / deploys", false, &engine.read("eng / backend / deploys", false)?);

    // Someone comments directly in the JIRA web UI (bypasses Weavn).
    engine
        .backend
        .add_comment(&issue, "ops-oncall (in JIRA)", "rolled back, worker restarted", &serde_json::json!({}))?;

    println!("\n--- a teammate comments on the ticket directly in JIRA ---");
    println!("--- chat view BEFORE sync (JIRA comment not visible yet) ---\n");
    print_board("eng / backend / deploys", false, &engine.read("eng / backend / deploys", false)?);

    let pulled = engine.sync();

    println!("\n--- `weavn sync` pulled {pulled} message(s); chat view AFTER sync ---\n");
    print_board("eng / backend / deploys", false, &engine.read("eng / backend / deploys", false)?);

    println!("note: a change made in JIRA surfaces in chat after sync — the bidirectional");
    println!("      truth claim (§5). In production this is webhook-driven (push); the");
    println!("      same reconcile is also the recovery path if events are missed.");
    Ok(())
}

fn parse_participants(arg: &str) -> Vec<String> {
    arg.split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

fn cmd_dm(rest: &[String]) -> Result<(), String> {
    let sub = rest.first().map(String::as_str).unwrap_or("");
    let args = rest.get(1..).unwrap_or(&[]);
    match sub {
        "send" => {
            let (head, body) = split_cmd(args, 2, "usage: weavn dm send <a,b> <from> <body>")?;
            let participants = parse_participants(head[0]);
            let from = head[1];
            let mut st = load();
            let issue = {
                let mut v = Vault::new(&mut st.mock, &mut st.vault);
                v.send(&participants, from, &body)?
            };
            save(&st)?;
            println!("encrypted DM stored on vault issue {issue} (JIRA holds ciphertext)");
            Ok(())
        }
        "read" => {
            if args.len() < 2 {
                return Err("usage: weavn dm read <a,b> <as-user>".into());
            }
            let participants = parse_participants(&args[0]);
            let as_user = &args[1];
            let mut st = load();
            let lines = {
                let mut v = Vault::new(&mut st.mock, &mut st.vault);
                v.read_as(&participants, as_user)?
            };
            save(&st)?; // read may have minted a key for an outsider
            print_dm(&format!("{} reading [{}]", as_user, args[0]), &lines);
            Ok(())
        }
        "recover" => {
            if args.is_empty() {
                return Err("usage: weavn dm recover <a,b>".into());
            }
            let participants = parse_participants(&args[0]);
            let mut st = load();
            let lines = {
                let v = Vault::new(&mut st.mock, &mut st.vault);
                v.recover(&participants)?
            };
            print_dm(&format!("ORG-ESCROW recovery of [{}]", args[0]), &lines);
            Ok(())
        }
        "raw" => {
            if args.is_empty() {
                return Err("usage: weavn dm raw <a,b>".into());
            }
            let participants = parse_participants(&args[0]);
            let mut st = load();
            let raw = {
                let v = Vault::new(&mut st.mock, &mut st.vault);
                v.raw(&participants)?
            };
            println!("# what JIRA stores for [{}] — {} comment(s)\n", args[0], raw.len());
            for (i, c) in raw.iter().enumerate() {
                let shown: String = c.chars().take(72).collect();
                println!("  comment {}: {shown}…", i + 1);
            }
            Ok(())
        }
        other => Err(format!("unknown dm subcommand {other:?}\n\n{USAGE}")),
    }
}

fn print_dm(title: &str, lines: &[DmLine]) {
    println!("# {title} — {} message(s)\n", lines.len());
    for l in lines {
        println!("  {}: {}", l.from, l.text);
    }
}

fn cmd_dm_demo() -> Result<(), String> {
    let mut mock = MockBackend::new();
    let mut vault = VaultState::new();
    let participants = vec!["alice".to_string(), "bob".to_string()];

    {
        let mut v = Vault::new(&mut mock, &mut vault);
        v.send(&participants, "alice", "the board login still broken?")?;
        v.send(&participants, "bob", "yeah — fix going out at 5")?;
    }

    println!("=== Weavn encrypted-DM demo (org-escrow) ===\n");

    println!("--- what JIRA actually stores (ciphertext) ---\n");
    {
        let v = Vault::new(&mut mock, &mut vault);
        for (i, c) in v.raw(&participants)?.iter().enumerate() {
            let shown: String = c.chars().take(64).collect();
            println!("  comment {}: {shown}…", i + 1);
        }
    }

    println!("\n--- bob (a participant) reads ---\n");
    {
        let mut v = Vault::new(&mut mock, &mut vault);
        print_dm("bob", &v.read_as(&participants, "bob")?);
    }

    println!("\n--- carol (an outsider) tries to read ---\n");
    {
        let mut v = Vault::new(&mut mock, &mut vault);
        print_dm("carol", &v.read_as(&participants, "carol")?);
    }

    println!("\n--- org-escrow recovery (compliance) ---\n");
    {
        let v = Vault::new(&mut mock, &mut vault);
        print_dm("escrow", &v.recover(&participants)?);
    }

    println!("\nnote: JIRA (and any admin) sees only ciphertext; participants decrypt;");
    println!("      outsiders get [locked]; the org escrow key recovers for compliance.");
    Ok(())
}

#[cfg(feature = "real-jira")]
fn cmd_jira_verify() -> Result<(), String> {
    use weavn::jira::real::RealBackend;
    let backend = RealBackend::from_env()?;
    let who = backend.verify()?;
    println!("auth OK — authenticated as: {who}");
    let project = project_from_env()?;
    let name = backend.check_project(&project)?;
    println!("project OK — {project}: {name}");
    println!("read-only verify passed; nothing was created.");
    Ok(())
}

#[cfg(feature = "real-jira")]
fn cmd_real_check(rest: &[String]) -> Result<(), String> {
    use weavn::jira::real::RealBackend;
    let (head, body) = split_cmd(rest, 2, "usage: weavn real-check <channel> <author> <body>")?;
    let (channel, author) = (head[0], head[1]);
    let backend = RealBackend::from_env()?;
    let mut engine = Engine::new(Store::new(), backend, project_from_env()?, COALESCE_WINDOW);
    let outcome = engine.send(channel, author, &body)?;
    println!("posted to real JIRA:");
    report(&outcome);
    Ok(())
}

#[cfg(feature = "real-jira")]
fn cmd_real_demo() -> Result<(), String> {
    use weavn::jira::real::RealBackend;
    let backend = RealBackend::from_env()?;
    let project = project_from_env()?;
    let mut engine = Engine::new(Store::new(), backend, project, COALESCE_WINDOW);
    let ch = "eng / backend / deploys";

    let issue = match engine.send(ch, "alice", "Rolling out *v2.3* to prod now")? {
        SendOutcome::NewIssue { issue } => issue,
        other => return Err(format!("expected a new issue, got {other:?}")),
    };
    engine.send(ch, "alice", "canary healthy")?; // coalesces into the burst
    engine.send(ch, "alice", "promoted to 100%")?; // coalesces
    engine.reply(&issue, "bob", "nice — watching the dashboards")?;

    println!("real-demo wrote to JIRA ({ch}):");
    println!("  thread issue : {issue}");
    println!("  description  : alice's 3-message burst, merged (coalescing)");
    println!("  comment      : 1 reply from bob");
    println!("open {issue} in JIRA to confirm the merged description and the comment.");
    Ok(())
}

#[cfg(feature = "real-jira")]
fn cmd_real_refine(rest: &[String]) -> Result<(), String> {
    use weavn::jira::real::RealBackend;
    if rest.len() < 2 {
        return Err("usage: weavn real-refine <ISSUE-KEY> <slash command>...".into());
    }
    let issue = &rest[0];
    let commands = rest[1..].to_vec();
    let backend = RealBackend::from_env()?;
    let mut engine = Engine::new(Store::new(), backend, project_from_env()?, COALESCE_WINDOW);
    let applied = engine.refine(issue, &commands)?;
    println!("refined {issue} on real JIRA:");
    for a in &applied {
        println!("  {a}");
    }
    Ok(())
}

#[cfg(feature = "real-jira")]
fn project_from_env() -> Result<String, String> {
    std::env::var("WEAVN_PROJECT").map_err(|_| "set WEAVN_PROJECT (e.g. BC)".into())
}
