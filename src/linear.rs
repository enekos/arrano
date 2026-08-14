//! Linear GraphQL client (curl + $LINEAR_API_KEY, falling back to the
//! `linear` CLI's `api` subcommand) and the issue model for the Linear view.

use std::io::Write;
use std::process::{Command, Stdio};

use serde_json::Value;

use crate::model::Comment;

#[derive(Clone, Debug)]
pub struct LinearIssue {
    pub identifier: String,
    pub title: String,
    pub url: String,
    pub priority: i64,
    pub priority_label: String,
    pub estimate: Option<f64>,
    pub due_date: String,
    pub updated_at: String,
    pub state: String,
    pub state_color: String,
    pub state_type: String,
    pub assignee: String,
    pub labels: Vec<(String, String)>,
    pub project: String,
    pub cycle: String,
    pub description: String,
    pub comments: Vec<Comment>,
}

const ISSUE_FIELDS: &str = "identifier title url priority priorityLabel estimate dueDate updatedAt \
state { name color type } assignee { displayName } labels(first: 10) { nodes { name color } } \
project { name } cycle { number name } description \
comments(first: 25) { nodes { body createdAt user { displayName } } }";

fn api(query: &str) -> Result<Value, String> {
    let body = serde_json::json!({ "query": query }).to_string();
    let out = if let Ok(key) = std::env::var("LINEAR_API_KEY") {
        let mut child = Command::new("curl")
            .args([
                "-sf",
                "--max-time",
                "20",
                "https://api.linear.app/graphql",
                "-H",
                &format!("Authorization: {key}"),
                "-H",
                "Content-Type: application/json",
                "-d",
                "@-",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("failed to spawn curl: {e}"))?;
        child.stdin.take().unwrap().write_all(body.as_bytes()).map_err(|e| e.to_string())?;
        let out = child.wait_with_output().map_err(|e| e.to_string())?;
        if !out.status.success() {
            return Err("linear api unreachable (curl failed)".into());
        }
        String::from_utf8_lossy(&out.stdout).into_owned()
    } else {
        let out = Command::new("linear")
            .args(["api", query])
            .output()
            .map_err(|_| "no $LINEAR_API_KEY and no `linear` CLI on PATH".to_string())?;
        if !out.status.success() {
            return Err(format!(
                "linear api: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    let v: Value = serde_json::from_str(&out).map_err(|e| format!("bad linear json: {e}"))?;
    if let Some(errs) = v.get("errors").and_then(|e| e.as_array()) {
        let msg = errs
            .first()
            .and_then(|e| e.pointer("/message"))
            .and_then(|m| m.as_str())
            .unwrap_or("unknown error");
        return Err(format!("linear: {msg}"));
    }
    Ok(v)
}

fn s(v: &Value, ptr: &str) -> String {
    v.pointer(ptr).and_then(|x| x.as_str()).unwrap_or("").to_string()
}

fn parse_issue(n: &Value) -> LinearIssue {
    let labels = n
        .pointer("/labels/nodes")
        .and_then(|x| x.as_array())
        .map(|arr| arr.iter().map(|l| (s(l, "/name"), s(l, "/color"))).collect())
        .unwrap_or_default();
    let comments = n
        .pointer("/comments/nodes")
        .and_then(|x| x.as_array())
        .map(|arr| {
            arr.iter()
                .map(|c| Comment {
                    author: s(c, "/user/displayName"),
                    body: s(c, "/body"),
                    created_at: s(c, "/createdAt"),
                    db_id: None,
                    url: String::new(),
                    reactions: Vec::new(),
                })
                .collect()
        })
        .unwrap_or_default();
    let cycle = match (
        n.pointer("/cycle/number").and_then(|x| x.as_i64()),
        s(n, "/cycle/name"),
    ) {
        (Some(num), name) if !name.is_empty() => format!("{num} · {name}"),
        (Some(num), _) => num.to_string(),
        _ => String::new(),
    };
    LinearIssue {
        identifier: s(n, "/identifier"),
        title: s(n, "/title"),
        url: s(n, "/url"),
        priority: n.pointer("/priority").and_then(|x| x.as_i64()).unwrap_or(0),
        priority_label: s(n, "/priorityLabel"),
        estimate: n.pointer("/estimate").and_then(|x| x.as_f64()),
        due_date: s(n, "/dueDate"),
        updated_at: s(n, "/updatedAt"),
        state: s(n, "/state/name"),
        state_color: s(n, "/state/color"),
        state_type: s(n, "/state/type"),
        assignee: s(n, "/assignee/displayName"),
        labels,
        project: s(n, "/project/name"),
        cycle,
        description: s(n, "/description"),
        comments,
    }
}

fn state_rank(state_type: &str) -> u8 {
    match state_type {
        "started" => 0,
        "unstarted" => 1,
        "triage" => 2,
        "backlog" => 3,
        _ => 4,
    }
}

/// Urgent(1) < High(2) < Medium(3) < Low(4) < None(0).
fn priority_rank(p: i64) -> i64 {
    if p == 0 {
        5
    } else {
        p
    }
}

pub fn my_issues() -> Result<Vec<LinearIssue>, String> {
    let q = format!(
        "query {{ viewer {{ assignedIssues(first: 50, orderBy: updatedAt, \
         filter: {{ state: {{ type: {{ nin: [\"completed\",\"canceled\"] }} }} }}) \
         {{ nodes {{ {ISSUE_FIELDS} }} }} }} }}"
    );
    let v = api(&q)?;
    let mut issues: Vec<LinearIssue> = v
        .pointer("/data/viewer/assignedIssues/nodes")
        .and_then(|x| x.as_array())
        .map(|arr| arr.iter().map(parse_issue).collect())
        .unwrap_or_default();
    issues.sort_by(|a, b| {
        state_rank(&a.state_type)
            .cmp(&state_rank(&b.state_type))
            .then(a.state.cmp(&b.state))
            .then(priority_rank(a.priority).cmp(&priority_rank(b.priority)))
            .then(b.updated_at.cmp(&a.updated_at))
    });
    Ok(issues)
}

/// Fetch one issue by its identifier, e.g. "ABC-123".
pub fn issue_by_identifier(ident: &str) -> Result<LinearIssue, String> {
    let (team, number) = ident
        .split_once('-')
        .and_then(|(t, n)| n.parse::<u64>().ok().map(|n| (t, n)))
        .ok_or_else(|| format!("bad ticket id: {ident}"))?;
    let q = format!(
        "query {{ issues(first: 1, filter: {{ team: {{ key: {{ eq: \"{team}\" }} }}, \
         number: {{ eq: {number} }} }}) {{ nodes {{ {ISSUE_FIELDS} }} }} }}"
    );
    let v = api(&q)?;
    v.pointer("/data/issues/nodes/0")
        .filter(|n| !n.is_null())
        .map(parse_issue)
        .ok_or_else(|| format!("{ident} not found in Linear"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn priority_none_sorts_last() {
        assert!(priority_rank(0) > priority_rank(4));
        assert!(priority_rank(1) < priority_rank(2));
    }

    #[test]
    fn parses_issue_shape() {
        let n: Value = serde_json::from_str(
            r##"{"identifier":"ABC-1","title":"t","url":"u","priority":2,
                "priorityLabel":"High","estimate":3,"dueDate":"2026-08-20",
                "updatedAt":"2026-08-14T00:00:00Z",
                "state":{"name":"In Progress","color":"#f2c94c","type":"started"},
                "assignee":{"displayName":"Alice"},
                "labels":{"nodes":[{"name":"Data","color":"#4ea7fc"}]},
                "project":{"name":"Jobs 4.0"},"cycle":{"number":29,"name":""},
                "description":"body",
                "comments":{"nodes":[{"body":"hi","createdAt":"x","user":{"displayName":"K"}}]}}"##,
        )
        .unwrap();
        let i = parse_issue(&n);
        assert_eq!(i.identifier, "ABC-1");
        assert_eq!(i.estimate, Some(3.0));
        assert_eq!(i.cycle, "29");
        assert_eq!(i.labels[0].0, "Data");
        assert_eq!(i.comments[0].author, "K");
    }
}
