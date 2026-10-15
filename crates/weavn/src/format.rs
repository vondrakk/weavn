//! Slack-style mrkdwn ⇆ ADF (Atlassian Document Format) conversion.
//!
//! Outbound (`mrkdwn_to_adf`): paragraphs, ATX headings, fenced ``` code blocks,
//! bullet/ordered lists, blockquotes, horizontal rules, and inline `code`,
//! *bold*, _italic_, ~strike~, and links (`[text](url)` and `<url|text>`).
//! Inbound (`adf_to_mrkdwn`): the reverse, used when projecting JIRA content.
//! `mrkdwn_to_adf_mentions` additionally turns resolved `@Display` runs into ADF
//! mention nodes (anywhere in the tree). Covered by the test corpus below.

use serde_json::{json, Value};

/// Inline emphasis delimiters, applied outermost-first.
const DELIMS: &[(char, &str)] = &[('*', "strong"), ('_', "em"), ('~', "strike")];

/// Convert a mrkdwn string to an ADF `doc` node.
pub fn mrkdwn_to_adf(md: &str) -> Value {
    let lines: Vec<&str> = md.lines().collect();
    let mut content: Vec<Value> = Vec::new();
    let mut para: Vec<String> = Vec::new();
    let mut i = 0;

    while i < lines.len() {
        let line = lines[i];
        let t = line.trim_start();

        if t.starts_with("```") {
            flush_paragraph(&mut para, &mut content);
            i += 1;
            let mut code: Vec<String> = Vec::new();
            while i < lines.len() && !lines[i].trim_start().starts_with("```") {
                code.push(lines[i].to_string());
                i += 1;
            }
            if i < lines.len() {
                i += 1; // consume closing fence
            }
            let inner = if code.is_empty() {
                vec![]
            } else {
                vec![json!({ "type": "text", "text": code.join("\n") })]
            };
            content.push(json!({ "type": "codeBlock", "content": inner }));
        } else if let Some((level, rest)) = heading(t) {
            flush_paragraph(&mut para, &mut content);
            content.push(json!({ "type": "heading", "attrs": { "level": level }, "content": rich_inline(rest) }));
            i += 1;
        } else if t == "---" || t == "***" {
            flush_paragraph(&mut para, &mut content);
            content.push(json!({ "type": "rule" }));
            i += 1;
        } else if t.starts_with("> ") || t == ">" {
            flush_paragraph(&mut para, &mut content);
            let mut quote: Vec<String> = Vec::new();
            while i < lines.len() {
                let q = lines[i].trim_start();
                if let Some(r) = q.strip_prefix("> ") {
                    quote.push(r.to_string());
                    i += 1;
                } else if q == ">" {
                    quote.push(String::new());
                    i += 1;
                } else {
                    break;
                }
            }
            content.push(json!({ "type": "blockquote", "content": [{ "type": "paragraph", "content": rich_inline(&quote.join("\n")) }] }));
        } else if is_bullet(t) {
            flush_paragraph(&mut para, &mut content);
            let mut items = Vec::new();
            while i < lines.len() && is_bullet(lines[i].trim_start()) {
                let item = &lines[i].trim_start()[2..];
                items.push(json!({ "type": "listItem", "content": [{ "type": "paragraph", "content": rich_inline(item) }] }));
                i += 1;
            }
            content.push(json!({ "type": "bulletList", "content": items }));
        } else if ordered_marker(t).is_some() {
            flush_paragraph(&mut para, &mut content);
            let mut items = Vec::new();
            while i < lines.len() {
                if let Some(rest) = ordered_marker(lines[i].trim_start()) {
                    items.push(json!({ "type": "listItem", "content": [{ "type": "paragraph", "content": rich_inline(rest) }] }));
                    i += 1;
                } else {
                    break;
                }
            }
            content.push(json!({ "type": "orderedList", "content": items }));
        } else if t.is_empty() {
            flush_paragraph(&mut para, &mut content);
            i += 1;
        } else {
            para.push(line.to_string());
            i += 1;
        }
    }
    flush_paragraph(&mut para, &mut content);

    if content.is_empty() {
        content.push(json!({ "type": "paragraph", "content": [] }));
    }
    json!({ "type": "doc", "version": 1, "content": content })
}

fn heading(t: &str) -> Option<(u8, &str)> {
    for (prefix, level) in [("### ", 3u8), ("## ", 2), ("# ", 1)] {
        if let Some(rest) = t.strip_prefix(prefix) {
            return Some((level, rest));
        }
    }
    None
}

fn is_bullet(t: &str) -> bool {
    t.starts_with("- ") || t.starts_with("* ")
}

/// `N. rest` → `Some("rest")`.
fn ordered_marker(t: &str) -> Option<&str> {
    let b = t.as_bytes();
    let mut j = 0;
    while j < b.len() && b[j].is_ascii_digit() {
        j += 1;
    }
    if j > 0 && b.get(j) == Some(&b'.') && b.get(j + 1) == Some(&b' ') {
        Some(&t[j + 2..])
    } else {
        None
    }
}

fn flush_paragraph(para: &mut Vec<String>, content: &mut Vec<Value>) {
    if para.is_empty() {
        return;
    }
    let text = para.join("\n");
    content.push(json!({ "type": "paragraph", "content": rich_inline(&text) }));
    para.clear();
}

/// Inline nodes for a text run: `code` spans first (odd backtick segments),
/// then links + emphasis on the rest.
fn rich_inline(text: &str) -> Vec<Value> {
    let mut nodes = Vec::new();
    for (i, seg) in text.split('`').enumerate() {
        if seg.is_empty() {
            continue;
        }
        if i % 2 == 1 {
            nodes.push(json!({ "type": "text", "text": seg, "marks": [{ "type": "code" }] }));
        } else {
            nodes.extend(links_and_emphasis(seg));
        }
    }
    nodes
}

fn links_and_emphasis(seg: &str) -> Vec<Value> {
    let mut out = Vec::new();
    let mut rest = seg;
    loop {
        match find_link(rest) {
            Some((start, end, label, url)) => {
                if start > 0 {
                    out.extend(emphasis(&rest[..start], &[], DELIMS));
                }
                out.push(json!({ "type": "text", "text": label, "marks": [{ "type": "link", "attrs": { "href": url } }] }));
                rest = &rest[end..];
            }
            None => {
                out.extend(emphasis(rest, &[], DELIMS));
                break;
            }
        }
    }
    out
}

fn emphasis(text: &str, active: &[&str], delims: &[(char, &str)]) -> Vec<Value> {
    if delims.is_empty() {
        if text.is_empty() {
            return vec![];
        }
        return vec![marked(text, active)];
    }
    let (ch, mark) = delims[0];
    let rest = &delims[1..];
    // Need a matched pair to treat as emphasis; otherwise the char is literal.
    if text.matches(ch).count() < 2 {
        return emphasis(text, active, rest);
    }
    let mut out = Vec::new();
    for (i, part) in text.split(ch).enumerate() {
        if part.is_empty() {
            continue;
        }
        let mut na = active.to_vec();
        if i % 2 == 1 {
            na.push(mark);
        }
        out.extend(emphasis(part, &na, rest));
    }
    out
}

fn marked(text: &str, marks: &[&str]) -> Value {
    if marks.is_empty() {
        json!({ "type": "text", "text": text })
    } else {
        let m: Vec<Value> = marks.iter().map(|x| json!({ "type": x })).collect();
        json!({ "type": "text", "text": text, "marks": m })
    }
}

/// Earliest link in `s`: markdown `[text](url)` or Slack `<url|text>` / `<url>`.
fn find_link(s: &str) -> Option<(usize, usize, String, String)> {
    match (find_md_link(s), find_slack_link(s)) {
        (Some(a), Some(b)) => Some(if a.0 <= b.0 { a } else { b }),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    }
}

fn find_md_link(s: &str) -> Option<(usize, usize, String, String)> {
    let mut from = 0;
    while let Some(rel) = s[from..].find('[') {
        let lb = from + rel;
        if let Some(mid_rel) = s[lb..].find("](") {
            let rb = lb + mid_rel; // position of ']'
            let url_start = rb + 2;
            if let Some(cp_rel) = s[url_start..].find(')') {
                let cp = url_start + cp_rel;
                let label = &s[lb + 1..rb];
                let url = &s[url_start..cp];
                if !url.is_empty() && !label.is_empty() {
                    return Some((lb, cp + 1, label.to_string(), url.to_string()));
                }
            }
        }
        from = lb + 1;
    }
    None
}

fn find_slack_link(s: &str) -> Option<(usize, usize, String, String)> {
    let mut from = 0;
    while let Some(rel) = s[from..].find('<') {
        let lt = from + rel;
        if let Some(gt_rel) = s[lt..].find('>') {
            let gt = lt + gt_rel;
            let inside = &s[lt + 1..gt];
            if inside.starts_with("http") {
                let (url, label) = match inside.split_once('|') {
                    Some((u, l)) => (u.to_string(), l.to_string()),
                    None => (inside.to_string(), inside.to_string()),
                };
                return Some((lt, gt + 1, label, url));
            }
        }
        from = lt + 1;
    }
    None
}

/// Like [`mrkdwn_to_adf`], but turns `@Display Name` runs matching a supplied
/// `(display, accountId)` pair into ADF `mention` nodes anywhere in the document.
pub fn mrkdwn_to_adf_mentions(md: &str, mentions: &[(String, String)]) -> Value {
    let mut doc = mrkdwn_to_adf(md);
    if mentions.is_empty() {
        return doc;
    }
    if let Some(c) = doc.get_mut("content").and_then(|c| c.as_array_mut()) {
        expand_mentions_nodes(c, mentions);
    }
    doc
}

fn expand_mentions_nodes(nodes: &mut Vec<Value>, mentions: &[(String, String)]) {
    // recurse into children first
    for node in nodes.iter_mut() {
        if let Some(child) = node.get_mut("content").and_then(|c| c.as_array_mut()) {
            expand_mentions_nodes(child, mentions);
        }
    }
    // then expand plain text nodes at this level
    let mut expanded: Vec<Value> = Vec::new();
    for n in nodes.drain(..) {
        if n["type"] == "text" && n.get("marks").is_none() {
            if let Some(t) = n["text"].as_str() {
                expanded.extend(split_text_mentions(t, mentions));
                continue;
            }
        }
        expanded.push(n);
    }
    *nodes = expanded;
}

fn split_text_mentions(text: &str, mentions: &[(String, String)]) -> Vec<Value> {
    let mut out = Vec::new();
    let mut rest = text;
    loop {
        let mut best: Option<(usize, &str, &str)> = None;
        for (disp, id) in mentions {
            if let Some(pos) = rest.find(&format!("@{disp}")) {
                if best.map_or(true, |(b, _, _)| pos < b) {
                    best = Some((pos, disp.as_str(), id.as_str()));
                }
            }
        }
        match best {
            Some((pos, disp, id)) => {
                if pos > 0 {
                    out.push(json!({ "type": "text", "text": &rest[..pos] }));
                }
                out.push(json!({ "type": "mention", "attrs": { "id": id, "text": format!("@{disp}") } }));
                rest = &rest[pos + 1 + disp.len()..];
            }
            None => {
                if !rest.is_empty() {
                    out.push(json!({ "type": "text", "text": rest }));
                }
                break;
            }
        }
    }
    out
}

// --- inbound: ADF -> mrkdwn ---------------------------------------------------

/// Render an ADF `doc` (or any node) back to mrkdwn. Used when projecting JIRA
/// descriptions/comments so formatting survives a backfill.
pub fn adf_to_mrkdwn(node: &Value) -> String {
    let mut blocks: Vec<String> = Vec::new();
    if let Some(content) = node["content"].as_array() {
        for n in content {
            blocks.push(block_to_md(n));
        }
    }
    blocks.join("\n\n").trim().to_string()
}

fn block_to_md(n: &Value) -> String {
    match n["type"].as_str().unwrap_or("") {
        "paragraph" => inline_to_md(&n["content"]),
        "heading" => {
            let level = n["attrs"]["level"].as_u64().unwrap_or(1).clamp(1, 3) as usize;
            format!("{} {}", "#".repeat(level), inline_to_md(&n["content"]))
        }
        "codeBlock" => format!("```\n{}\n```", inline_to_md(&n["content"])),
        "rule" => "---".to_string(),
        "blockquote" => n["content"]
            .as_array()
            .map(|a| a.iter().map(|c| format!("> {}", block_to_md(c))).collect::<Vec<_>>().join("\n"))
            .unwrap_or_default(),
        "bulletList" => list_to_md(n, false),
        "orderedList" => list_to_md(n, true),
        _ => inline_to_md(&n["content"]),
    }
}

fn list_to_md(n: &Value, ordered: bool) -> String {
    let mut out = Vec::new();
    if let Some(items) = n["content"].as_array() {
        for (i, item) in items.iter().enumerate() {
            let body = item["content"]
                .as_array()
                .and_then(|a| a.first())
                .map(block_to_md)
                .unwrap_or_default();
            if ordered {
                out.push(format!("{}. {}", i + 1, body));
            } else {
                out.push(format!("- {}", body));
            }
        }
    }
    out.join("\n")
}

fn inline_to_md(content: &Value) -> String {
    let Some(nodes) = content.as_array() else { return String::new() };
    let mut out = String::new();
    for n in nodes {
        match n["type"].as_str().unwrap_or("") {
            "text" => {
                let text = n["text"].as_str().unwrap_or("");
                let marks: Vec<&str> = n["marks"].as_array().map(|a| a.iter().filter_map(|m| m["type"].as_str()).collect()).unwrap_or_default();
                let mut s = text.to_string();
                if let Some(href) = n["marks"].as_array().and_then(|a| a.iter().find(|m| m["type"] == "link")).and_then(|m| m["attrs"]["href"].as_str()) {
                    s = format!("[{s}]({href})");
                }
                if marks.contains(&"code") {
                    s = format!("`{s}`");
                }
                if marks.contains(&"strike") {
                    s = format!("~{s}~");
                }
                if marks.contains(&"em") {
                    s = format!("_{s}_");
                }
                if marks.contains(&"strong") {
                    s = format!("*{s}*");
                }
                out.push_str(&s);
            }
            "mention" => out.push_str(n["attrs"]["text"].as_str().unwrap_or("@?")),
            "hardBreak" => out.push('\n'),
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_paragraph() {
        let adf = mrkdwn_to_adf("hello there");
        assert_eq!(adf["content"][0]["type"], "paragraph");
        assert_eq!(adf["content"][0]["content"][0]["text"], "hello there");
    }

    #[test]
    fn code_fence_becomes_code_block() {
        let adf = mrkdwn_to_adf("before\n```\nlet x = 1;\n```\nafter");
        let types: Vec<&str> = adf["content"].as_array().unwrap().iter().map(|n| n["type"].as_str().unwrap()).collect();
        assert_eq!(types, vec!["paragraph", "codeBlock", "paragraph"]);
    }

    #[test]
    fn inline_marks() {
        let adf = mrkdwn_to_adf("run `cargo test`, *ship* it, _soon_, ~never~");
        let s = serde_json::to_string(&adf).unwrap();
        assert!(s.contains("\"code\"") && s.contains("\"strong\"") && s.contains("\"em\"") && s.contains("\"strike\""));
    }

    #[test]
    fn lone_asterisk_is_literal() {
        let adf = mrkdwn_to_adf("2 * 3 = 6");
        let s = serde_json::to_string(&adf["content"][0]["content"]).unwrap();
        assert!(!s.contains("strong"), "a single * must not become bold: {s}");
    }

    #[test]
    fn headings_lists_quote_rule() {
        let adf = mrkdwn_to_adf("# Title\n\n- a\n- b\n\n1. one\n2. two\n\n> quoted\n\n---");
        let types: Vec<&str> = adf["content"].as_array().unwrap().iter().map(|n| n["type"].as_str().unwrap()).collect();
        assert_eq!(types, vec!["heading", "bulletList", "orderedList", "blockquote", "rule"]);
        assert_eq!(adf["content"][0]["attrs"]["level"], 1);
        assert_eq!(adf["content"][1]["content"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn links_both_styles() {
        let md = mrkdwn_to_adf("see [docs](https://x.test/a) and <https://y.test|y>");
        let s = serde_json::to_string(&md).unwrap();
        assert!(s.contains("https://x.test/a") && s.contains("https://y.test"));
        assert!(s.matches("\"link\"").count() == 2);
    }

    #[test]
    fn round_trip_preserves_structure() {
        let md = "# Heading\n\nsome *bold* and `code`\n\n- one\n- two";
        let back = adf_to_mrkdwn(&mrkdwn_to_adf(md));
        let reparsed = mrkdwn_to_adf(&back);
        let types: Vec<&str> = reparsed["content"].as_array().unwrap().iter().map(|n| n["type"].as_str().unwrap()).collect();
        assert_eq!(types, vec!["heading", "paragraph", "bulletList"]);
    }

    #[test]
    fn mentions_in_list_item() {
        let adf = mrkdwn_to_adf_mentions("- ping @Dana now", &[("Dana".into(), "acc-1".into())]);
        let s = serde_json::to_string(&adf).unwrap();
        assert!(s.contains("\"mention\"") && s.contains("acc-1"));
    }
}
