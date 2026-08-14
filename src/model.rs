use serde_json::Value;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Ci {
    Pass,
    Fail,
    Pending,
    None,
}

#[derive(Clone, Debug)]
pub struct Pr {
    pub id: String,
    pub repo: String,
    pub number: u64,
    pub title: String,
    pub url: String,
    pub author: String,
    pub is_draft: bool,
    pub updated_at: String,
    pub base_ref: String,
    pub head_ref: String,
    pub review_decision: String,
    pub ci: Ci,
}

#[derive(Clone, Debug)]
pub struct Comment {
    pub author: String,
    pub body: String,
    pub created_at: String,
    /// REST id — present only on review-thread comments; needed for replies.
    pub db_id: Option<i64>,
    pub url: String,
    /// (emoji, count), only non-zero groups
    pub reactions: Vec<(String, i64)>,
}

#[derive(Clone, Debug)]
pub struct Review {
    pub author: String,
    pub state: String,
    pub body: String,
    pub created_at: String,
    pub url: String,
}

#[derive(Clone, Debug)]
pub struct Thread {
    pub id: String,
    pub path: String,
    pub line: Option<i64>,
    pub resolved: bool,
    pub outdated: bool,
    /// diff context of the first comment, for display
    pub hunk: String,
    pub comments: Vec<Comment>,
}

#[derive(Clone, Debug)]
pub struct Check {
    pub name: String,
    pub status: String,
    pub conclusion: String,
    pub url: String,
}

#[derive(Clone, Debug)]
pub struct PrDetail {
    pub repo: String,
    pub number: u64,
    pub title: String,
    pub body: String,
    pub url: String,
    pub author: String,
    pub state: String,
    pub is_draft: bool,
    pub base_ref: String,
    pub head_ref: String,
    pub additions: i64,
    pub deletions: i64,
    pub changed_files: i64,
    pub mergeable: String,
    pub merge_state: String,
    pub review_decision: String,
    pub comments: Vec<Comment>,
    pub reviews: Vec<Review>,
    pub threads: Vec<Thread>,
    pub checks: Vec<Check>,
}

fn s(v: &Value, ptr: &str) -> String {
    v.pointer(ptr).and_then(|x| x.as_str()).unwrap_or("").to_string()
}

fn i(v: &Value, ptr: &str) -> i64 {
    v.pointer(ptr).and_then(|x| x.as_i64()).unwrap_or(0)
}

fn b(v: &Value, ptr: &str) -> bool {
    v.pointer(ptr).and_then(|x| x.as_bool()).unwrap_or(false)
}

fn ci_from(state: Option<&str>) -> Ci {
    match state {
        Some("SUCCESS") => Ci::Pass,
        Some("FAILURE") | Some("ERROR") => Ci::Fail,
        Some("PENDING") | Some("EXPECTED") => Ci::Pending,
        _ => Ci::None,
    }
}

pub fn parse_search(root: &Value) -> Vec<Pr> {
    let mut out: Vec<Pr> = Vec::new();
    let nodes = root
        .pointer("/data/search/nodes")
        .and_then(|n| n.as_array())
        .cloned()
        .unwrap_or_default();
    for n in &nodes {
        if n.get("number").is_none() {
            continue;
        }
        let ci = ci_from(
            n.pointer("/commits/nodes/0/commit/statusCheckRollup/state")
                .and_then(|x| x.as_str()),
        );
        out.push(Pr {
            id: s(n, "/id"),
            repo: s(n, "/repository/nameWithOwner"),
            number: i(n, "/number") as u64,
            title: s(n, "/title"),
            url: s(n, "/url"),
            author: s(n, "/author/login"),
            is_draft: b(n, "/isDraft"),
            updated_at: s(n, "/updatedAt"),
            base_ref: s(n, "/baseRefName"),
            head_ref: s(n, "/headRefName"),
            review_decision: s(n, "/reviewDecision"),
            ci,
        });
    }
    out.sort_by(|a, b| a.repo.cmp(&b.repo).then(b.updated_at.cmp(&a.updated_at)));
    out
}

fn reaction_emoji(content: &str) -> &'static str {
    match content {
        "THUMBS_UP" => "👍",
        "THUMBS_DOWN" => "👎",
        "LAUGH" => "😄",
        "HOORAY" => "🎉",
        "CONFUSED" => "😕",
        "HEART" => "❤️",
        "ROCKET" => "🚀",
        "EYES" => "👀",
        _ => "·",
    }
}

fn parse_reactions(v: Option<&Value>) -> Vec<(String, i64)> {
    v.and_then(|x| x.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|g| {
                    let n = g.pointer("/reactors/totalCount").and_then(|x| x.as_i64())?;
                    if n > 0 {
                        Some((reaction_emoji(&s(g, "/content")).to_string(), n))
                    } else {
                        None
                    }
                })
                .collect()
        })
        .unwrap_or_default()
}

fn parse_comments(v: Option<&Value>) -> Vec<Comment> {
    v.and_then(|x| x.as_array())
        .map(|arr| {
            arr.iter()
                .map(|c| Comment {
                    author: s(c, "/author/login"),
                    body: s(c, "/body"),
                    created_at: s(c, "/createdAt"),
                    db_id: c.pointer("/databaseId").and_then(|x| x.as_i64()),
                    url: s(c, "/url"),
                    reactions: parse_reactions(c.pointer("/reactionGroups")),
                })
                .collect()
        })
        .unwrap_or_default()
}

pub fn parse_detail(root: &Value, repo: &str) -> Option<PrDetail> {
    let pr = root.pointer("/data/repository/pullRequest")?;
    if pr.is_null() {
        return None;
    }

    let reviews = pr
        .pointer("/reviews/nodes")
        .and_then(|x| x.as_array())
        .map(|arr| {
            arr.iter()
                .map(|r| Review {
                    author: s(r, "/author/login"),
                    state: s(r, "/state"),
                    body: s(r, "/body"),
                    created_at: s(r, "/createdAt"),
                    url: s(r, "/url"),
                })
                .filter(|r| !(r.body.is_empty() && r.state == "COMMENTED"))
                .collect()
        })
        .unwrap_or_default();

    let threads = pr
        .pointer("/reviewThreads/nodes")
        .and_then(|x| x.as_array())
        .map(|arr| {
            arr.iter()
                .map(|t| Thread {
                    id: s(t, "/id"),
                    path: s(t, "/path"),
                    line: t.pointer("/line").and_then(|x| x.as_i64()),
                    resolved: b(t, "/isResolved"),
                    outdated: b(t, "/isOutdated"),
                    hunk: s(t, "/comments/nodes/0/diffHunk"),
                    comments: parse_comments(t.pointer("/comments/nodes")),
                })
                .collect()
        })
        .unwrap_or_default();

    let mut checks: Vec<Check> = Vec::new();
    if let Some(ctxs) = pr
        .pointer("/commits/nodes/0/commit/statusCheckRollup/contexts/nodes")
        .and_then(|x| x.as_array())
    {
        for c in ctxs {
            let typename = s(c, "/__typename");
            if typename == "CheckRun" {
                let wf = s(c, "/checkSuite/workflowRun/workflow/name");
                let name = s(c, "/name");
                let prefixed = if wf.is_empty() || name.contains(" / ") || name.starts_with(&wf) {
                    name
                } else {
                    format!("{wf} / {name}")
                };
                checks.push(Check {
                    name: prefixed,
                    status: s(c, "/status"),
                    conclusion: s(c, "/conclusion"),
                    url: s(c, "/detailsUrl"),
                });
            } else if typename == "StatusContext" {
                checks.push(Check {
                    name: s(c, "/context"),
                    status: "COMPLETED".into(),
                    conclusion: s(c, "/state"),
                    url: s(c, "/targetUrl"),
                });
            }
        }
    }

    Some(PrDetail {
        repo: repo.to_string(),
        number: i(pr, "/number") as u64,
        title: s(pr, "/title"),
        body: s(pr, "/body"),
        url: s(pr, "/url"),
        author: s(pr, "/author/login"),
        state: s(pr, "/state"),
        is_draft: b(pr, "/isDraft"),
        base_ref: s(pr, "/baseRefName"),
        head_ref: s(pr, "/headRefName"),
        additions: i(pr, "/additions"),
        deletions: i(pr, "/deletions"),
        changed_files: i(pr, "/changedFiles"),
        mergeable: s(pr, "/mergeable"),
        merge_state: s(pr, "/mergeStateStatus"),
        review_decision: s(pr, "/reviewDecision"),
        comments: parse_comments(pr.pointer("/comments/nodes")),
        reviews,
        threads,
        checks,
    })
}

/// An inline comment queued locally, submitted as part of one review.
#[derive(Clone, Debug)]
pub struct PendingComment {
    pub path: String,
    pub line: u64,
    pub side: String,
    pub body: String,
}

/// Where a rendered diff row lands for the GitHub review-comment API.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffTarget {
    pub path: String,
    pub line: u64,
    /// "RIGHT" for added/context lines, "LEFT" for removed lines
    pub side: &'static str,
}

/// For each unified-diff line, the (path, line, side) an inline review
/// comment on that row should attach to. None for headers/hunk markers.
pub fn map_diff(lines: &[String]) -> Vec<Option<DiffTarget>> {
    let mut out = Vec::with_capacity(lines.len());
    let mut path: Option<String> = None;
    let mut old_ln: u64 = 0;
    let mut new_ln: u64 = 0;
    for l in lines {
        if l.starts_with("diff --git") {
            path = None;
            out.push(None);
        } else if let Some(rest) = l.strip_prefix("+++ b/") {
            path = Some(rest.to_string());
            out.push(None);
        } else if l.starts_with("+++") || l.starts_with("---") || l.starts_with('\\') {
            out.push(None);
        } else if let Some(rest) = l.strip_prefix("@@ -") {
            let nums = |s: &str| s.split(',').next().and_then(|x| x.parse::<u64>().ok()).unwrap_or(1);
            let mut parts = rest.splitn(2, " +");
            old_ln = nums(parts.next().unwrap_or("1"));
            new_ln = parts
                .next()
                .and_then(|p| p.split(' ').next())
                .map(nums)
                .unwrap_or(1);
            out.push(None);
        } else if let Some(p) = &path {
            if l.starts_with('+') {
                out.push(Some(DiffTarget { path: p.clone(), line: new_ln, side: "RIGHT" }));
                new_ln += 1;
            } else if l.starts_with('-') {
                out.push(Some(DiffTarget { path: p.clone(), line: old_ln, side: "LEFT" }));
                old_ln += 1;
            } else {
                out.push(Some(DiffTarget { path: p.clone(), line: new_ln, side: "RIGHT" }));
                old_ln += 1;
                new_ln += 1;
            }
        } else {
            out.push(None);
        }
    }
    out
}

/// "2026-08-13T09:00:00Z" -> "3h" (relative to now); empty on parse failure.
pub fn rel_time(iso: &str) -> String {
    let Ok(dt) = chrono::DateTime::parse_from_rfc3339(iso) else {
        return String::new();
    };
    let secs = chrono::Utc::now()
        .signed_duration_since(dt.with_timezone(&chrono::Utc))
        .num_seconds()
        .max(0);
    match secs {
        0..=59 => format!("{secs}s"),
        60..=3599 => format!("{}m", secs / 60),
        3600..=86399 => format!("{}h", secs / 3600),
        _ => format!("{}d", secs / 86400),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_search_sorts_and_skips_non_prs() {
        let root: Value = serde_json::from_str(
            r#"{"data":{"search":{"nodes":[
                {},
                {"id":"PR_b","number":2,"title":"b","url":"u","isDraft":false,
                 "updatedAt":"2026-08-01T00:00:00Z","baseRefName":"master","reviewDecision":null,
                 "repository":{"nameWithOwner":"o/zeta"},"author":{"login":"me"},
                 "commits":{"nodes":[{"commit":{"statusCheckRollup":{"state":"FAILURE"}}}]}},
                {"id":"PR_a","number":1,"title":"a","url":"u","isDraft":true,
                 "updatedAt":"2026-08-02T00:00:00Z","baseRefName":"main","reviewDecision":"APPROVED",
                 "repository":{"nameWithOwner":"o/alpha"},"author":{"login":"me"},
                 "commits":{"nodes":[{"commit":{"statusCheckRollup":null}}]}}
            ]}}}"#,
        )
        .unwrap();
        let prs = parse_search(&root);
        assert_eq!(prs.len(), 2);
        assert_eq!(prs[0].repo, "o/alpha");
        assert_eq!(prs[0].ci, Ci::None);
        assert!(prs[0].is_draft);
        assert_eq!(prs[1].ci, Ci::Fail);
        assert_eq!(prs[1].review_decision, "");
    }

    #[test]
    fn parse_detail_threads_and_checks() {
        let root: Value = serde_json::from_str(
            r#"{"data":{"repository":{"pullRequest":{
                "id":"PR_x","number":7,"title":"t","body":"","url":"u","isDraft":false,
                "state":"OPEN","baseRefName":"master","headRefName":"feat",
                "additions":10,"deletions":2,"changedFiles":3,
                "mergeable":"MERGEABLE","mergeStateStatus":"CLEAN","reviewDecision":"APPROVED",
                "updatedAt":"2026-08-01T00:00:00Z","author":{"login":"me"},
                "comments":{"nodes":[{"author":{"login":"a"},"body":"hi","createdAt":"2026-08-01T00:00:00Z"}]},
                "reviews":{"nodes":[
                    {"author":{"login":"b"},"state":"COMMENTED","body":"","createdAt":"x"},
                    {"author":{"login":"c"},"state":"APPROVED","body":"lgtm","createdAt":"x"}
                ]},
                "reviewThreads":{"nodes":[{
                    "id":"T_1","isResolved":false,"isOutdated":false,"path":"src/a.rs","line":12,
                    "comments":{"nodes":[{"author":{"login":"d"},"body":"why?","createdAt":"x"}]}
                }]},
                "commits":{"nodes":[{"commit":{"statusCheckRollup":{
                    "state":"SUCCESS",
                    "contexts":{"nodes":[
                        {"__typename":"CheckRun","name":"build / test","status":"COMPLETED","conclusion":"SUCCESS",
                         "detailsUrl":"http://x","checkSuite":{"workflowRun":{"workflow":{"name":"CI"}}}},
                        {"__typename":"StatusContext","context":"ci/circleci: lint","state":"FAILURE","targetUrl":"http://y"}
                    ]}
                }}}]}
            }}}}"#,
        )
        .unwrap();
        let d = parse_detail(&root, "o/r").unwrap();
        assert_eq!(d.number, 7);
        // empty COMMENTED review is dropped, APPROVED kept
        assert_eq!(d.reviews.len(), 1);
        assert_eq!(d.threads.len(), 1);
        assert_eq!(d.threads[0].line, Some(12));
        assert_eq!(d.checks.len(), 2);
        // name already contains " / " -> no workflow prefix
        assert_eq!(d.checks[0].name, "build / test");
        assert_eq!(d.checks[1].conclusion, "FAILURE");
        assert_eq!(d.checks[1].status, "COMPLETED");
    }

    #[test]
    fn map_diff_tracks_lines_and_sides() {
        let diff: Vec<String> = [
            "diff --git a/src/a.rs b/src/a.rs",
            "index 123..456 100644",
            "--- a/src/a.rs",
            "+++ b/src/a.rs",
            "@@ -10,3 +20,4 @@ fn main() {",
            " context",
            "-removed",
            "+added one",
            "+added two",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let map = map_diff(&diff);
        assert!(map[0].is_none() && map[4].is_none());
        assert_eq!(map[5], Some(DiffTarget { path: "src/a.rs".into(), line: 20, side: "RIGHT" }));
        assert_eq!(map[6], Some(DiffTarget { path: "src/a.rs".into(), line: 11, side: "LEFT" }));
        assert_eq!(map[7], Some(DiffTarget { path: "src/a.rs".into(), line: 21, side: "RIGHT" }));
        assert_eq!(map[8], Some(DiffTarget { path: "src/a.rs".into(), line: 22, side: "RIGHT" }));
    }

    #[test]
    fn wrap_never_panics_on_unicode() {
        assert_eq!(rel_time("not a date"), "");
        assert!(!rel_time("2026-08-01T00:00:00Z").is_empty());
    }
}
