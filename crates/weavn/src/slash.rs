//! Slash-command parsing for in-chat ticket refinement (§13).
//!
//! `/assign @alex`, `/points 5`, `/status in review`, `/priority high`,
//! `/label backend` → a `TicketUpdate` the backend applies to the issue.

use crate::proto::TicketUpdate;

/// Parse a list of slash commands into one `TicketUpdate`.
pub fn parse(commands: &[String]) -> Result<TicketUpdate, String> {
    let mut up = TicketUpdate::default();
    for raw in commands {
        let c = raw.trim();
        let rest = c
            .strip_prefix('/')
            .ok_or_else(|| format!("not a slash command: {c:?}"))?;
        let (cmd, arg) = match rest.split_once(char::is_whitespace) {
            Some((cmd, arg)) => (cmd, arg.trim()),
            None => (rest, ""),
        };
        let need = |arg: &str| {
            if arg.is_empty() {
                Err(format!("/{cmd} needs an argument"))
            } else {
                Ok(arg.to_string())
            }
        };
        match cmd.to_ascii_lowercase().as_str() {
            "assign" => up.assignee = Some(need(arg.trim_start_matches('@'))?),
            "status" => up.status = Some(need(arg)?),
            "priority" => up.priority = Some(need(arg)?),
            "label" => up.add_labels.push(need(arg)?),
            "points" => {
                up.story_points = Some(
                    arg.parse::<f64>()
                        .map_err(|_| format!("/points needs a number, got {arg:?}"))?,
                )
            }
            other => return Err(format!("unknown command /{other}")),
        }
    }
    Ok(up)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parses_common_commands() {
        let up = parse(&v(&["/assign @alex", "/points 5", "/status In Review", "/label backend"])).unwrap();
        assert_eq!(up.assignee.as_deref(), Some("alex"));
        assert_eq!(up.story_points, Some(5.0));
        assert_eq!(up.status.as_deref(), Some("In Review"));
        assert_eq!(up.add_labels, vec!["backend"]);
    }

    #[test]
    fn rejects_bad_input() {
        assert!(parse(&v(&["points 5"])).is_err()); // no slash
        assert!(parse(&v(&["/points five"])).is_err()); // not a number
        assert!(parse(&v(&["/assign"])).is_err()); // missing arg
        assert!(parse(&v(&["/wat x"])).is_err()); // unknown
    }
}
