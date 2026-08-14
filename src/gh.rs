use std::io::Write;
use std::process::{Command, Stdio};

use serde_json::Value;

use crate::model::{parse_detail, parse_search, Pr, PrDetail};

pub type GhResult<T> = Result<T, String>;

fn run(args: &[&str]) -> GhResult<String> {
    let out = Command::new("gh")
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("failed to spawn gh: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        let err = String::from_utf8_lossy(&out.stderr);
        let err = err.trim();
        let err = if err.is_empty() {
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        } else {
            err.to_string()
        };
        Err(format!("gh {}: {}", args.first().unwrap_or(&""), err))
    }
}

fn run_stdin(args: &[&str], input: &str) -> GhResult<String> {
    let mut child = Command::new("gh")
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("failed to spawn gh: {e}"))?;
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .map_err(|e| e.to_string())?;
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

/// GitHub intermittently 502/503s heavier GraphQL queries — worth retrying.
fn is_transient(err: &str) -> bool {
    let e = err.to_lowercase();
    e.contains("http 50")
        || e.contains("service unavailable")
        || e.contains("bad gateway")
        || e.contains("something went wrong")
        || e.contains("timeout")
        || e.contains("timed out")
}

fn graphql(query: &str, fields: &[(&str, &str)], ints: &[(&str, u64)]) -> GhResult<Value> {
    let mut args: Vec<String> = vec!["api".into(), "graphql".into(), "-f".into(), format!("query={query}")];
    for (k, v) in fields {
        args.push("-f".into());
        args.push(format!("{k}={v}"));
    }
    for (k, v) in ints {
        args.push("-F".into());
        args.push(format!("{k}={v}"));
    }
    let refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    let mut delay = std::time::Duration::from_millis(400);
    let mut attempt = 0;
    loop {
        match run(&refs) {
            Ok(out) => {
                return serde_json::from_str(&out).map_err(|e| format!("bad gh json: {e}"))
            }
            Err(e) if attempt < 2 && is_transient(&e) => {
                std::thread::sleep(delay);
                delay *= 2;
                attempt += 1;
            }
            Err(e) => return Err(e),
        }
    }
}

pub fn viewer_login() -> GhResult<String> {
    Ok(run(&["api", "user", "-q", ".login"])?.trim().to_string())
}

// statusCheckRollup inside a search is the classic GitHub GraphQL timeout
// trigger — CI states are fetched separately in small batches (fetch_ci).
const LIST_QUERY: &str = r#"
query($q: String!) {
  search(query: $q, type: ISSUE, first: 50) {
    nodes {
      ... on PullRequest {
        id number title url isDraft updatedAt headRefName baseRefName
        additions deletions reviewDecision
        repository { nameWithOwner }
        author { login }
      }
    }
  }
}
"#;

pub fn search_prs(search: &str) -> GhResult<Vec<Pr>> {
    let v = graphql(LIST_QUERY, &[("q", search)], &[])?;
    Ok(parse_search(&v))
}

/// Batched CI lookup: one aliased query per chunk of PRs, far cheaper than
/// asking the search endpoint to compute rollups.
pub fn fetch_ci(keys: &[(String, u64)]) -> GhResult<Vec<(String, u64, crate::model::Ci)>> {
    if keys.is_empty() {
        return Ok(Vec::new());
    }
    let mut q = String::from("query {");
    for (i, (repo, number)) in keys.iter().enumerate() {
        let Some((owner, name)) = repo.split_once('/') else { continue };
        q.push_str(&format!(
            " p{i}: repository(owner: \"{owner}\", name: \"{name}\") {{ \
             pullRequest(number: {number}) {{ \
             commits(last: 1) {{ nodes {{ commit {{ statusCheckRollup {{ state }} }} }} }} }} }}"
        ));
    }
    q.push('}');
    let v = graphql(&q, &[], &[])?;
    Ok(keys
        .iter()
        .enumerate()
        .map(|(i, (repo, number))| {
            let state = v
                .pointer(&format!(
                    "/data/p{i}/pullRequest/commits/nodes/0/commit/statusCheckRollup/state"
                ))
                .and_then(|x| x.as_str());
            (repo.clone(), *number, crate::model::ci_from(state))
        })
        .collect())
}

const DETAIL_QUERY: &str = r#"
query($owner: String!, $name: String!, $number: Int!) {
  repository(owner: $owner, name: $name) {
    pullRequest(number: $number) {
      id number title body url isDraft state baseRefName headRefName
      additions deletions changedFiles
      mergeable mergeStateStatus reviewDecision updatedAt
      author { login }
      comments(first: 50) { nodes {
        author { login } body createdAt url
        reactionGroups { content reactors { totalCount } }
      } }
      reviews(first: 50) { nodes { author { login } state body createdAt url } }
      reviewThreads(first: 100) {
        nodes {
          id isResolved isOutdated path line
          comments(first: 50) { nodes {
            author { login } body createdAt databaseId url diffHunk
            reactionGroups { content reactors { totalCount } }
          } }
        }
      }
      commits(last: 1) { nodes { commit { statusCheckRollup {
        state
        contexts(first: 100) { nodes {
          __typename
          ... on CheckRun { name status conclusion detailsUrl checkSuite { workflowRun { workflow { name } } } }
          ... on StatusContext { context state targetUrl }
        } }
      } } } }
    }
  }
}
"#;

pub fn pr_detail(repo: &str, number: u64) -> GhResult<PrDetail> {
    let (owner, name) = repo
        .split_once('/')
        .ok_or_else(|| format!("bad repo: {repo}"))?;
    let v = graphql(DETAIL_QUERY, &[("owner", owner), ("name", name)], &[("number", number)])?;
    parse_detail(&v, repo).ok_or_else(|| format!("PR {repo}#{number} not found"))
}

pub fn pr_diff(repo: &str, number: u64) -> GhResult<String> {
    run(&["pr", "diff", &number.to_string(), "--repo", repo])
}

/// method: "squash" | "merge" | "rebase"
pub fn merge_pr(repo: &str, number: u64, method: &str) -> GhResult<String> {
    run(&[
        "pr",
        "merge",
        &number.to_string(),
        "--repo",
        repo,
        &format!("--{method}"),
    ])?;
    Ok(format!("merged {repo}#{number} ({method})"))
}

const UPDATE_BRANCH_MUTATION: &str = r#"
mutation($id: ID!, $m: PullRequestUpdateMethod!) {
  updatePullRequestBranch(input: { pullRequestId: $id, updateMethod: $m }) {
    pullRequest { number }
  }
}
"#;

/// method: "REBASE" | "MERGE"
pub fn update_branch(pr_id: &str, method: &str) -> GhResult<String> {
    graphql(UPDATE_BRANCH_MUTATION, &[("id", pr_id), ("m", method)], &[])?;
    Ok(format!(
        "branch update requested ({})",
        method.to_lowercase()
    ))
}

const RESOLVE_MUTATION: &str = r#"
mutation($id: ID!) {
  resolveReviewThread(input: { threadId: $id }) { thread { isResolved } }
}
"#;

const UNRESOLVE_MUTATION: &str = r#"
mutation($id: ID!) {
  unresolveReviewThread(input: { threadId: $id }) { thread { isResolved } }
}
"#;

pub fn set_thread_resolved(thread_id: &str, resolved: bool) -> GhResult<String> {
    let q = if resolved { RESOLVE_MUTATION } else { UNRESOLVE_MUTATION };
    graphql(q, &[("id", thread_id)], &[])?;
    Ok(if resolved {
        "thread resolved".into()
    } else {
        "thread unresolved".into()
    })
}

pub fn post_comment(repo: &str, number: u64, body: &str) -> GhResult<String> {
    run_stdin(
        &[
            "pr",
            "comment",
            &number.to_string(),
            "--repo",
            repo,
            "--body-file",
            "-",
        ],
        body,
    )?;
    Ok(format!("comment posted on {repo}#{number}"))
}

/// Reply to an existing review thread (REST: replies to its first comment).
pub fn reply_thread(repo: &str, number: u64, comment_id: i64, body: &str) -> GhResult<String> {
    let payload = serde_json::json!({ "body": body }).to_string();
    run_stdin(
        &[
            "api",
            "-X",
            "POST",
            &format!("repos/{repo}/pulls/{number}/comments/{comment_id}/replies"),
            "--input",
            "-",
        ],
        &payload,
    )?;
    Ok(format!("reply posted on {repo}#{number}"))
}

/// Submit one review (REST) with any queued inline comments attached, so the
/// colleague gets a single notification instead of one per comment.
/// event: "APPROVE" | "REQUEST_CHANGES" | "COMMENT"
pub fn submit_review(
    repo: &str,
    number: u64,
    event: &str,
    body: &str,
    comments: &[crate::model::PendingComment],
) -> GhResult<String> {
    let comments_json: Vec<serde_json::Value> = comments
        .iter()
        .map(|c| {
            serde_json::json!({
                "path": c.path,
                "line": c.line,
                "side": c.side,
                "body": c.body,
            })
        })
        .collect();
    let payload = serde_json::json!({
        "event": event,
        "body": body,
        "comments": comments_json,
    })
    .to_string();
    run_stdin(
        &[
            "api",
            "-X",
            "POST",
            &format!("repos/{repo}/pulls/{number}/reviews"),
            "--input",
            "-",
        ],
        &payload,
    )?;
    let with = if comments.is_empty() {
        String::new()
    } else {
        format!(" with {} inline comment(s)", comments.len())
    };
    Ok(format!(
        "review submitted ({}){} on {repo}#{number}",
        event.to_lowercase().replace('_', " "),
        with
    ))
}

pub fn open_url(url: &str) {
    let _ = Command::new("open")
        .arg(url)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
}
