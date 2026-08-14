use std::collections::{HashMap, HashSet};
use std::sync::mpsc::Sender;
use std::thread;
use std::time::{Duration, Instant};

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;

use crate::claude::ReviewCtx;
use crate::linear::LinearIssue;
use crate::model::{map_diff, DiffTarget, PendingComment, Pr, PrDetail};
use crate::{claude, gh, linear, llm, repo};

pub const LANES: usize = 3;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Lane {
    Mine = 0,
    Review = 1,
    Search = 2,
}

impl Lane {
    pub fn title(self) -> &'static str {
        match self {
            Lane::Mine => "my PRs",
            Lane::Review => "review requested",
            Lane::Search => "search",
        }
    }
    fn search(self, login: &str, org: Option<&str>) -> String {
        let scope = org.map(|o| format!(" org:{o}")).unwrap_or_default();
        match self {
            Lane::Mine => {
                format!("is:pr is:open author:{login} archived:false sort:updated-desc{scope}")
            }
            Lane::Review => format!(
                "is:pr is:open review-requested:{login} archived:false sort:updated-desc{scope}"
            ),
            Lane::Search => String::new(),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    List,
    Detail,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum View {
    Prs,
    Linear,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tab {
    Overview = 0,
    Diff = 1,
    Checks = 2,
    Comments = 3,
    Claude = 4,
}

pub const TABS: [Tab; 5] = [Tab::Overview, Tab::Diff, Tab::Checks, Tab::Comments, Tab::Claude];

impl Tab {
    pub fn title(self) -> &'static str {
        match self {
            Tab::Overview => "overview",
            Tab::Diff => "diff",
            Tab::Checks => "checks",
            Tab::Comments => "comments",
            Tab::Claude => "claude",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Modal {
    Merge,
    Update,
    ConfirmPost,
    Review,
    Links,
    Help,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ReviewEvent {
    Approve,
    RequestChanges,
    Comment,
}

impl ReviewEvent {
    pub fn rest(self) -> &'static str {
        match self {
            ReviewEvent::Approve => "APPROVE",
            ReviewEvent::RequestChanges => "REQUEST_CHANGES",
            ReviewEvent::Comment => "COMMENT",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            ReviewEvent::Approve => "approve",
            ReviewEvent::RequestChanges => "request changes",
            ReviewEvent::Comment => "comment review",
        }
    }
}

/// A selectable block in the comments tab, in display order.
#[derive(Clone)]
pub enum CBlock {
    Comment(usize),
    Review(usize),
    Thread(usize),
}

#[derive(Clone)]
pub enum ComposeTarget {
    PrComment { repo: String, number: u64 },
    ThreadReply { repo: String, number: u64, comment_id: i64, loc: String },
    Inline { repo: String, number: u64, target: DiffTarget },
    Review { repo: String, number: u64, event: ReviewEvent },
}

impl ComposeTarget {
    pub fn describe(&self) -> String {
        match self {
            ComposeTarget::PrComment { repo, number } => format!("comment · {repo}#{number}"),
            ComposeTarget::ThreadReply { loc, .. } => format!("reply · {loc}"),
            ComposeTarget::Inline { target, .. } => format!(
                "queue for review · {}:{} ({})",
                target.path,
                target.line,
                target.side.to_lowercase()
            ),
            ComposeTarget::Review { event, .. } => format!("review · {}", event.label()),
        }
    }
    pub fn allows_empty(&self) -> bool {
        matches!(self, ComposeTarget::Review { event: ReviewEvent::Approve, .. })
    }
}

pub struct Compose {
    pub lines: Vec<String>,
    pub row: usize,
    pub col: usize,
    pub target: ComposeTarget,
    pub rewriting: bool,
    pub prev: Option<Vec<String>>,
}

impl Compose {
    fn new(target: ComposeTarget) -> Self {
        Compose { lines: vec![String::new()], row: 0, col: 0, target, rewriting: false, prev: None }
    }
    pub fn text(&self) -> String {
        self.lines.join("\n")
    }
    fn byte_col(&self) -> usize {
        let line = &self.lines[self.row];
        line.char_indices().nth(self.col).map(|(b, _)| b).unwrap_or(line.len())
    }
    fn line_chars(&self, row: usize) -> usize {
        self.lines[row].chars().count()
    }
    fn insert(&mut self, ch: char) {
        let b = self.byte_col();
        self.lines[self.row].insert(b, ch);
        self.col += 1;
    }
    fn newline(&mut self) {
        let b = self.byte_col();
        let rest = self.lines[self.row].split_off(b);
        self.lines.insert(self.row + 1, rest);
        self.row += 1;
        self.col = 0;
    }
    fn backspace(&mut self) {
        if self.col > 0 {
            self.col -= 1;
            let b = self.byte_col();
            self.lines[self.row].remove(b);
        } else if self.row > 0 {
            let cur = self.lines.remove(self.row);
            self.row -= 1;
            self.col = self.line_chars(self.row);
            self.lines[self.row].push_str(&cur);
        }
    }
    fn move_cursor(&mut self, code: KeyCode) {
        match code {
            KeyCode::Left => {
                if self.col > 0 {
                    self.col -= 1;
                } else if self.row > 0 {
                    self.row -= 1;
                    self.col = self.line_chars(self.row);
                }
            }
            KeyCode::Right => {
                if self.col < self.line_chars(self.row) {
                    self.col += 1;
                } else if self.row + 1 < self.lines.len() {
                    self.row += 1;
                    self.col = 0;
                }
            }
            KeyCode::Up => {
                if self.row > 0 {
                    self.row -= 1;
                    self.col = self.col.min(self.line_chars(self.row));
                }
            }
            KeyCode::Down => {
                if self.row + 1 < self.lines.len() {
                    self.row += 1;
                    self.col = self.col.min(self.line_chars(self.row));
                }
            }
            _ => {}
        }
    }
    fn set_text(&mut self, text: &str) {
        self.lines = text.lines().map(str::to_string).collect();
        if self.lines.is_empty() {
            self.lines.push(String::new());
        }
        self.row = self.lines.len() - 1;
        self.col = self.line_chars(self.row);
    }
}

pub enum Input {
    Filter { buf: String },
    Compose(Compose),
}

pub enum ActionTag {
    ReviewSubmitted((String, u64)),
}

pub enum AppEvent {
    Login(Result<String, String>),
    Lists(Lane, Result<Vec<Pr>, String>),
    Detail(String, u64, Result<PrDetail, String>),
    Diff(String, u64, Result<String, String>),
    ActionDone(Option<ActionTag>, Result<String, String>),
    ClaudeLine(String, u64, String),
    ClaudeDone(String, u64, Result<(), String>),
    Rewrite(Result<String, String>),
    LinearIssues(Result<Vec<LinearIssue>, String>),
    LinearJump(String, Result<LinearIssue, String>),
    /// CI states fetched in batches after a lane loads
    Ci(Vec<(String, u64, crate::model::Ci)>),
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum AnimTarget {
    Overview,
    Diff,
    Claude,
    LinearDetail,
}

/// A smooth scroll in flight: eased interpolation from `from` to `to`.
pub struct Anim {
    pub target: AnimTarget,
    pub from: f32,
    pub to: f32,
    pub started: Instant,
    pub dur_ms: u32,
}

#[derive(Default, Clone, Copy)]
pub struct Areas {
    pub left: Rect,
    pub right: Rect,
}

#[derive(Default)]
pub struct LinearState {
    pub issues: Vec<LinearIssue>,
    pub sel: usize,
    pub loading: bool,
    pub error: Option<String>,
    pub fetched_at: Option<Instant>,
    /// identifier to select once data arrives (set by `L` from a PR)
    pub jump: Option<String>,
}

#[derive(Default)]
pub struct LaneState {
    pub prs: Vec<Pr>,
    pub sel: usize,
    pub loading: bool,
    pub error: Option<String>,
    pub fetched_at: Option<Instant>,
}

#[derive(Clone)]
pub struct DiffData {
    pub lines: Vec<String>,
    pub targets: Vec<Option<DiffTarget>>,
}

/// Cached PR detail, valid while the PR's updatedAt matches `for_updated`.
pub struct CacheEntry {
    pub detail: Option<PrDetail>,
    pub diff: Option<DiffData>,
    pub for_updated: String,
    pub at: Instant,
}

#[derive(Default)]
pub struct DetailState {
    pub key: Option<(String, u64)>,
    pub data: Option<PrDetail>,
    pub diff: Option<DiffData>,
    pub loading: bool,
    pub diff_loading: bool,
    pub error: Option<String>,
    pub blocks: Vec<CBlock>,
    /// explicit fold overrides per thread index (default: folded if resolved/outdated)
    pub folds: std::collections::HashMap<usize, bool>,
}

#[derive(Default)]
pub struct ClaudeState {
    pub key: Option<(String, u64)>,
    pub text: String,
    pub running: bool,
    pub follow: bool,
}

pub struct App {
    pub login: Option<String>,
    pub org: Option<String>,
    pub view: View,
    pub lane: Lane,
    pub lanes: [LaneState; LANES],
    pub linear: LinearState,
    pub linear_scroll: u16,
    pub focus: Focus,
    pub tab: Tab,
    pub detail: DetailState,
    pub claude: ClaudeState,

    pub filter: String,
    pub input: Option<Input>,
    pub last_search: Option<String>,

    pub overview_scroll: u16,
    pub diff_sel: usize,
    pub claude_scroll: u16,
    pub checks_sel: usize,
    pub block_sel: usize,

    /// monochrome rendering for e-ink displays
    pub eink: bool,
    pub modal: Option<Modal>,
    pub toasts: Vec<(String, Instant, bool)>,
    pub pending_actions: usize,
    pub quit: bool,
    pub tick: u64,

    /// detail cache keyed by (repo, number)
    cache: HashMap<(String, u64), CacheEntry>,
    in_flight: HashSet<(String, u64)>,
    /// queued inline review comments per PR, flushed by `v`
    pub pending: HashMap<(String, u64), Vec<PendingComment>>,
    refresh_secs: u64,

    /// links collected for the picker modal: (label, url)
    pub links: Vec<(String, String)>,
    pub links_sel: usize,

    /// pane rectangles from the last frame, for mouse routing
    pub areas: Areas,
    /// content viewport height from the last frame, for paging
    pub page_rows: u16,
    pub anim: Option<Anim>,

    sel_changed_at: Option<Instant>,
    tx: Sender<AppEvent>,
}

/// Every whitespace token must substring-match the issue's fields.
pub fn issue_matches(i: &LinearIssue, filter: &str) -> bool {
    if filter.is_empty() {
        return true;
    }
    let labels: String = i.labels.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>().join(" ");
    let hay = format!(
        "{} {} {} {} {} {}",
        i.identifier, i.title, i.state, i.project, labels, i.priority_label
    )
    .to_lowercase();
    filter.split_whitespace().all(|t| hay.contains(t.trim_start_matches('#')))
}

/// Every whitespace token must substring-match repo/number/title/author/branch.
pub fn pr_matches(pr: &Pr, filter: &str) -> bool {
    if filter.is_empty() {
        return true;
    }
    let hay = format!(
        "{} #{} {} {} {}",
        pr.repo, pr.number, pr.title, pr.author, pr.head_ref
    )
    .to_lowercase();
    filter
        .split_whitespace()
        .all(|t| hay.contains(t.trim_start_matches('#')))
}

/// Expand a user search into a GitHub search query. Qualifiers pass through,
/// bare `merged`/`closed`/`draft` become state qualifiers, `@me` becomes the
/// login, `#123` becomes a plain text term, and the --org scope is appended
/// unless the query already scopes itself.
pub fn expand_search(raw: &str, login: &str, org: Option<&str>) -> String {
    let mut parts: Vec<String> = vec!["is:pr".into()];
    let mut scoped = org.is_none();
    let mut text: Vec<String> = Vec::new();
    for tok in raw.split_whitespace() {
        let tok = tok.replace("@me", login);
        match tok.as_str() {
            "merged" => parts.push("is:merged".into()),
            "closed" => parts.push("is:closed".into()),
            "draft" => parts.push("draft:true".into()),
            t if t.contains(':') => {
                if t.starts_with("repo:") || t.starts_with("org:") || t.starts_with("user:") {
                    scoped = true;
                }
                parts.push(t.to_string());
            }
            t => text.push(t.trim_start_matches('#').to_string()),
        }
    }
    if !scoped {
        if let Some(o) = org {
            parts.push(format!("org:{o}"));
        }
    }
    parts.push("archived:false".into());
    parts.push("sort:updated-desc".into());
    parts.extend(text);
    parts.join(" ")
}

impl App {
    pub fn new(tx: Sender<AppEvent>, org: Option<String>) -> Self {
        let app = App {
            login: None,
            org,
            view: View::Prs,
            lane: Lane::Mine,
            lanes: Default::default(),
            linear: Default::default(),
            linear_scroll: 0,
            focus: Focus::List,
            tab: Tab::Overview,
            detail: Default::default(),
            claude: Default::default(),
            filter: String::new(),
            input: None,
            last_search: None,
            overview_scroll: 0,
            diff_sel: 0,
            claude_scroll: 0,
            checks_sel: 0,
            block_sel: 0,
            eink: std::env::var("ARRANO_EINK").map(|v| v != "0").unwrap_or(false),
            modal: None,
            toasts: Vec::new(),
            pending_actions: 0,
            quit: false,
            tick: 0,
            cache: HashMap::new(),
            in_flight: HashSet::new(),
            pending: HashMap::new(),
            refresh_secs: std::env::var("ARRANO_REFRESH_SECS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(180),
            links: Vec::new(),
            links_sel: 0,
            areas: Areas::default(),
            page_rows: 20,
            anim: None,
            sel_changed_at: None,
            tx,
        };
        let tx = app.tx.clone();
        thread::spawn(move || {
            let _ = tx.send(AppEvent::Login(gh::viewer_login()));
        });
        app
    }

    pub fn lane_state(&self) -> &LaneState {
        &self.lanes[self.lane as usize]
    }

    /// The filter being applied right now (live while typing).
    pub fn effective_filter(&self) -> &str {
        match &self.input {
            Some(Input::Filter { buf }) => buf,
            _ => &self.filter,
        }
    }

    fn visible_for(&self, lane: Lane, filter: &str) -> Vec<usize> {
        let f = filter.to_lowercase();
        self.lanes[lane as usize]
            .prs
            .iter()
            .enumerate()
            .filter(|(_, pr)| pr_matches(pr, &f))
            .map(|(i, _)| i)
            .collect()
    }

    /// Indices (into the lane's prs) that pass the current filter.
    pub fn visible(&self) -> Vec<usize> {
        self.visible_for(self.lane, self.effective_filter())
    }

    pub fn selected_pr(&self) -> Option<&Pr> {
        let vis = self.visible();
        let ls = self.lane_state();
        vis.get(ls.sel).map(|&i| &ls.prs[i])
    }

    pub fn linear_visible(&self) -> Vec<usize> {
        let f = self.effective_filter().to_lowercase();
        self.linear
            .issues
            .iter()
            .enumerate()
            .filter(|(_, i)| issue_matches(i, &f))
            .map(|(i, _)| i)
            .collect()
    }

    pub fn selected_issue(&self) -> Option<&LinearIssue> {
        let vis = self.linear_visible();
        vis.get(self.linear.sel).map(|&i| &self.linear.issues[i])
    }

    pub fn refresh_linear(&mut self) {
        if self.linear.loading {
            return;
        }
        self.linear.loading = true;
        self.linear.error = None;
        let tx = self.tx.clone();
        thread::spawn(move || {
            let _ = tx.send(AppEvent::LinearIssues(linear::my_issues()));
        });
    }

    fn open_linear(&mut self, jump: Option<String>) {
        self.view = View::Linear;
        self.focus = Focus::List;
        if self.linear.issues.is_empty() && !self.linear.loading {
            self.refresh_linear();
        }
        if let Some(id) = jump {
            self.filter.clear();
            let pos = self
                .linear_visible()
                .iter()
                .position(|&i| self.linear.issues[i].identifier == id);
            match pos {
                Some(p) => {
                    self.linear.sel = p;
                    self.linear_scroll = 0;
                }
                None => {
                    self.linear.jump = Some(id.clone());
                    let tx = self.tx.clone();
                    thread::spawn(move || {
                        let _ = tx.send(AppEvent::LinearJump(
                            id.clone(),
                            linear::issue_by_identifier(&id),
                        ));
                    });
                }
            }
        }
    }

    /// Gather every link reachable from the current context into the picker.
    fn open_link_picker(&mut self) {
        let mut links: Vec<(String, String)> = Vec::new();
        let mut add_body = |author: &str, body: &str| {
            links.extend(crate::md::extract_links(&crate::md::clean_comment(author, body)));
        };
        match self.view {
            View::Linear => {
                if let Some(i) = self.selected_issue() {
                    let (desc, url, comments) =
                        (i.description.clone(), i.url.clone(), i.comments.clone());
                    add_body("", &desc);
                    for c in &comments {
                        add_body(&c.author, &c.body);
                    }
                    links.push(("open issue on Linear".into(), url));
                }
            }
            View::Prs => {
                match self.tab {
                    Tab::Comments => {
                        if let (Some(d), Some(block)) =
                            (self.detail.data.as_ref(), self.detail.blocks.get(self.block_sel))
                        {
                            match block {
                                CBlock::Comment(i) => {
                                    if let Some(c) = d.comments.get(*i) {
                                        let (a, b, u) =
                                            (c.author.clone(), c.body.clone(), c.url.clone());
                                        add_body(&a, &b);
                                        links.push(("open comment on GitHub".into(), u));
                                    }
                                }
                                CBlock::Review(i) => {
                                    if let Some(r) = d.reviews.get(*i) {
                                        let (a, b, u) =
                                            (r.author.clone(), r.body.clone(), r.url.clone());
                                        add_body(&a, &b);
                                        links.push(("open review on GitHub".into(), u));
                                    }
                                }
                                CBlock::Thread(i) => {
                                    if let Some(t) = d.threads.get(*i) {
                                        let comments = t.comments.clone();
                                        for c in &comments {
                                            add_body(&c.author, &c.body);
                                        }
                                        if let Some(u) =
                                            comments.first().map(|c| c.url.clone())
                                        {
                                            links.push(("open thread on GitHub".into(), u));
                                        }
                                    }
                                }
                            }
                        }
                    }
                    Tab::Claude => {
                        let text = self.claude.text.clone();
                        add_body("", &text);
                    }
                    _ => {
                        if let Some(d) = self.detail.data.as_ref() {
                            let body = d.body.clone();
                            add_body("", &body);
                        }
                    }
                }
                if let Some(pr) = self.selected_pr() {
                    let url = pr.url.clone();
                    if !links.iter().any(|(_, u)| *u == url) {
                        links.push(("open PR on GitHub".into(), url));
                    }
                }
            }
        }
        // dedupe by url, keep first label
        let mut seen: Vec<String> = Vec::new();
        links.retain(|(_, u)| {
            if seen.contains(u) {
                false
            } else {
                seen.push(u.clone());
                true
            }
        });
        links.truncate(20);
        if links.is_empty() {
            self.toast("no links here", true);
            return;
        }
        self.links = links;
        self.links_sel = 0;
        self.modal = Some(Modal::Links);
    }

    fn clamp_sel_linear(&mut self) {
        let n = self.linear_visible().len();
        if self.linear.sel >= n {
            self.linear.sel = n.saturating_sub(1);
        }
    }

    pub fn thread_folded(&self, ti: usize) -> bool {
        self.detail.folds.get(&ti).copied().unwrap_or_else(|| {
            self.detail
                .data
                .as_ref()
                .and_then(|d| d.threads.get(ti))
                .map(|t| t.resolved || t.outdated)
                .unwrap_or(false)
        })
    }

    pub fn busy(&self) -> bool {
        self.pending_actions > 0
            || self.lanes.iter().any(|l| l.loading)
            || self.detail.loading
            || self.detail.diff_loading
    }

    fn toast(&mut self, msg: impl Into<String>, is_error: bool) {
        self.toasts.push((msg.into(), Instant::now(), is_error));
    }

    pub fn expire_toasts(&mut self) {
        let ttl = Duration::from_secs(5);
        self.toasts.retain(|(_, t, _)| t.elapsed() < ttl);
    }

    fn clamp_sel(&mut self) {
        match self.view {
            View::Prs => {
                let n = self.visible().len();
                let ls = &mut self.lanes[self.lane as usize];
                if ls.sel >= n {
                    ls.sel = n.saturating_sub(1);
                }
                self.sel_changed_at = Some(Instant::now());
            }
            View::Linear => {
                let n = self.linear_visible().len();
                if self.linear.sel >= n {
                    self.linear.sel = n.saturating_sub(1);
                }
            }
        }
    }

    // ---- background fetches ----

    pub fn refresh_lane(&mut self, lane: Lane) {
        if self.lanes[lane as usize].loading {
            return;
        }
        let q = if lane == Lane::Search {
            let Some(raw) = self.last_search.clone() else { return };
            let Some(login) = self.login.clone() else { return };
            expand_search(&raw, &login, self.org.as_deref())
        } else {
            let Some(login) = self.login.clone() else { return };
            lane.search(&login, self.org.as_deref())
        };
        let ls = &mut self.lanes[lane as usize];
        ls.loading = true;
        ls.error = None;
        let tx = self.tx.clone();
        thread::spawn(move || {
            let _ = tx.send(AppEvent::Lists(lane, gh::search_prs(&q)));
        });
    }

    pub fn refresh_all(&mut self) {
        self.refresh_lane(Lane::Mine);
        self.refresh_lane(Lane::Review);
        self.refresh_lane(Lane::Search);
        self.refetch_current();
    }

    /// Kick off a background fetch that fills the cache (and the visible
    /// detail pane if it is still showing this PR when results arrive).
    fn spawn_fetch(&mut self, repo: String, number: u64, updated: String) {
        let key = (repo.clone(), number);
        if !self.in_flight.insert(key.clone()) {
            return;
        }
        if self.cache.len() > 64 {
            if let Some(oldest) = self.cache.iter().min_by_key(|(_, e)| e.at).map(|(k, _)| k.clone())
            {
                self.cache.remove(&oldest);
            }
        }
        self.cache.insert(
            key,
            CacheEntry { detail: None, diff: None, for_updated: updated, at: Instant::now() },
        );
        let tx = self.tx.clone();
        thread::spawn(move || {
            let d = gh::pr_detail(&repo, number);
            let _ = tx.send(AppEvent::Detail(repo.clone(), number, d));
            let diff = gh::pr_diff(&repo, number);
            let _ = tx.send(AppEvent::Diff(repo, number, diff));
        });
    }

    fn pr_updated_at(&self, repo: &str, number: u64) -> String {
        self.lanes
            .iter()
            .flat_map(|l| l.prs.iter())
            .find(|p| p.repo == repo && p.number == number)
            .map(|p| p.updated_at.clone())
            .unwrap_or_default()
    }

    /// Point the detail pane at a PR: instant from cache when fresh,
    /// fetched otherwise.
    fn show_detail(&mut self, repo: String, number: u64) {
        let key = (repo.clone(), number);
        if self.detail.key.as_ref() != Some(&key) {
            self.detail = DetailState { key: Some(key.clone()), ..Default::default() };
            self.overview_scroll = 0;
            self.diff_sel = 0;
            self.checks_sel = 0;
            self.block_sel = 0;
        }
        let updated = self.pr_updated_at(&repo, number);
        if let Some(e) = self.cache.get(&key) {
            if e.for_updated == updated || self.in_flight.contains(&key) {
                if self.detail.data.is_none() {
                    if let Some(d) = &e.detail {
                        self.detail.blocks = build_blocks(d);
                        self.detail.data = Some(d.clone());
                    }
                }
                if self.detail.diff.is_none() {
                    self.detail.diff = e.diff.clone();
                }
                let complete = self.detail.data.is_some() && self.detail.diff.is_some();
                self.detail.loading = self.detail.data.is_none();
                self.detail.diff_loading = self.detail.diff.is_none();
                if complete || self.in_flight.contains(&key) {
                    return;
                }
            }
        }
        self.detail.loading = self.detail.data.is_none();
        self.detail.diff_loading = self.detail.diff.is_none();
        self.spawn_fetch(repo, number, updated);
    }

    fn fetch_selected_detail(&mut self) {
        if let Some(pr) = self.selected_pr() {
            let (repo, number) = (pr.repo.clone(), pr.number);
            self.show_detail(repo, number);
        }
    }

    /// Silently refresh the current PR in the background (old content stays
    /// visible, scroll positions survive).
    fn refetch_current(&mut self) {
        if let Some((repo, number)) = self.detail.key.clone() {
            self.cache.remove(&(repo.clone(), number));
            let updated = self.pr_updated_at(&repo, number);
            self.spawn_fetch(repo, number, updated);
        }
    }

    /// Warm the cache for the PRs adjacent to the selection.
    fn prefetch_neighbors(&mut self) {
        let vis = self.visible();
        let sel = self.lane_state().sel;
        let mut targets: Vec<(String, u64, String)> = Vec::new();
        for pos in [sel + 1, sel.wrapping_sub(1)] {
            if let Some(&idx) = vis.get(pos) {
                let pr = &self.lane_state().prs[idx];
                let key = (pr.repo.clone(), pr.number);
                let fresh = self
                    .cache
                    .get(&key)
                    .map(|e| e.for_updated == pr.updated_at && e.detail.is_some() && e.diff.is_some())
                    .unwrap_or(false);
                if !fresh && !self.in_flight.contains(&key) {
                    targets.push((pr.repo.clone(), pr.number, pr.updated_at.clone()));
                }
            }
        }
        for (repo, number, updated) in targets {
            self.spawn_fetch(repo, number, updated);
        }
    }

    pub fn poll_debounce(&mut self) {
        if self.view != View::Prs {
            return;
        }
        if let Some(at) = self.sel_changed_at {
            if at.elapsed() >= Duration::from_millis(250) {
                self.sel_changed_at = None;
                self.fetch_selected_detail();
                self.prefetch_neighbors();
            }
        }
    }

    // ---- smooth scrolling & mouse ----

    fn start_anim(&mut self, target: AnimTarget, from: f32, to: f32) {
        if (to - from).abs() < 0.5 {
            return;
        }
        self.anim = Some(Anim { target, from, to, started: Instant::now(), dur_ms: 220 });
    }

    /// Advance the in-flight scroll animation (ease-out cubic).
    pub fn tick_anim(&mut self) {
        let Some(a) = &self.anim else { return };
        let t = a.started.elapsed().as_millis() as f32 / a.dur_ms as f32;
        let done = t >= 1.0;
        let eased = 1.0 - (1.0 - t.clamp(0.0, 1.0)).powi(3);
        let v = a.from + (a.to - a.from) * eased;
        let target = a.target;
        if done {
            self.anim = None;
        }
        match target {
            AnimTarget::Overview => self.overview_scroll = v.round() as u16,
            AnimTarget::Diff => self.diff_sel = v.round() as usize,
            AnimTarget::Claude => self.claude_scroll = v.round() as u16,
            AnimTarget::LinearDetail => self.linear_scroll = v.round() as u16,
        }
    }

    /// Ctrl-F / Ctrl-B: one viewport forward or back, animated for content
    /// panes, instant selection jumps for lists.
    pub fn page_scroll(&mut self, dir: f32) {
        let page = (self.page_rows.saturating_sub(2)).max(3) as f32;
        let delta = page * dir;
        self.anim = None;
        match (self.view, self.focus) {
            (View::Linear, Focus::Detail) => {
                let from = self.linear_scroll as f32;
                self.start_anim(AnimTarget::LinearDetail, from, (from + delta).max(0.0));
            }
            (View::Linear, Focus::List) => {
                let n = self.linear_visible().len();
                let sel = self.linear.sel as i64 + delta as i64;
                self.linear.sel = sel.clamp(0, n.saturating_sub(1) as i64) as usize;
            }
            (View::Prs, Focus::List) => {
                let n = self.visible().len();
                let ls = &mut self.lanes[self.lane as usize];
                let sel = ls.sel as i64 + delta as i64;
                ls.sel = sel.clamp(0, n.saturating_sub(1) as i64) as usize;
                self.sel_changed_at = Some(Instant::now());
            }
            (View::Prs, Focus::Detail) => match self.tab {
                Tab::Overview => {
                    let from = self.overview_scroll as f32;
                    self.start_anim(AnimTarget::Overview, from, (from + delta).max(0.0));
                }
                Tab::Diff => {
                    let n = self.detail.diff.as_ref().map(|d| d.lines.len()).unwrap_or(0);
                    let from = self.diff_sel as f32;
                    let to = (from + delta).clamp(0.0, n.saturating_sub(1) as f32);
                    self.start_anim(AnimTarget::Diff, from, to);
                }
                Tab::Claude => {
                    self.claude.follow = false;
                    let from = self.claude_scroll as f32;
                    self.start_anim(AnimTarget::Claude, from, (from + delta).max(0.0));
                }
                Tab::Comments => {
                    let n = self.detail.blocks.len();
                    let sel = self.block_sel as i64 + (3 * dir as i64);
                    self.block_sel = sel.clamp(0, n.saturating_sub(1) as i64) as usize;
                }
                Tab::Checks => {
                    let n = self.detail.data.as_ref().map(|d| d.checks.len()).unwrap_or(0);
                    let sel = self.checks_sel as i64 + delta as i64;
                    self.checks_sel = sel.clamp(0, n.saturating_sub(1) as i64) as usize;
                }
            },
        }
    }

    /// Mouse wheel: routed by which pane the pointer is over.
    pub fn on_mouse_scroll(&mut self, down: bool, x: u16, y: u16) {
        if self.input.is_some() || self.modal.is_some() {
            return;
        }
        self.anim = None;
        let hit = |r: Rect| {
            x >= r.x && x < r.x.saturating_add(r.width) && y >= r.y && y < r.y.saturating_add(r.height)
        };
        let step: i64 = if down { 3 } else { -3 };
        if hit(self.areas.left) {
            match self.view {
                View::Prs => {
                    let n = self.visible().len();
                    let ls = &mut self.lanes[self.lane as usize];
                    let sel = ls.sel as i64 + step;
                    let new = sel.clamp(0, n.saturating_sub(1) as i64) as usize;
                    if new != ls.sel {
                        ls.sel = new;
                        self.sel_changed_at = Some(Instant::now());
                    }
                }
                View::Linear => {
                    let n = self.linear_visible().len();
                    let sel = self.linear.sel as i64 + step;
                    self.linear.sel = sel.clamp(0, n.saturating_sub(1) as i64) as usize;
                }
            }
        } else if hit(self.areas.right) {
            match self.view {
                View::Linear => {
                    self.linear_scroll =
                        (self.linear_scroll as i64 + step).max(0) as u16;
                }
                View::Prs => match self.tab {
                    Tab::Overview => {
                        self.overview_scroll = (self.overview_scroll as i64 + step).max(0) as u16;
                    }
                    Tab::Diff => {
                        let n = self.detail.diff.as_ref().map(|d| d.lines.len()).unwrap_or(0);
                        let sel = self.diff_sel as i64 + step;
                        self.diff_sel = sel.clamp(0, n.saturating_sub(1) as i64) as usize;
                    }
                    Tab::Claude => {
                        self.claude.follow = false;
                        self.claude_scroll = (self.claude_scroll as i64 + step).max(0) as u16;
                    }
                    Tab::Comments => {
                        let n = self.detail.blocks.len();
                        let sel = self.block_sel as i64 + step.signum();
                        self.block_sel = sel.clamp(0, n.saturating_sub(1) as i64) as usize;
                    }
                    Tab::Checks => {
                        let n = self.detail.data.as_ref().map(|d| d.checks.len()).unwrap_or(0);
                        let sel = self.checks_sel as i64 + step.signum();
                        self.checks_sel = sel.clamp(0, n.saturating_sub(1) as i64) as usize;
                    }
                },
            }
        }
    }

    /// Background lane refresh every `refresh_secs` (0 disables).
    pub fn poll_auto_refresh(&mut self) {
        if self.refresh_secs == 0 || self.login.is_none() {
            return;
        }
        for lane in [Lane::Mine, Lane::Review] {
            let ls = &self.lanes[lane as usize];
            let due = ls
                .fetched_at
                .map(|t| t.elapsed().as_secs() >= self.refresh_secs)
                .unwrap_or(false);
            if due && !ls.loading {
                self.refresh_lane(lane);
            }
        }
        let linear_due = self
            .linear
            .fetched_at
            .map(|t| t.elapsed().as_secs() >= self.refresh_secs)
            .unwrap_or(false);
        if linear_due && !self.linear.loading {
            self.refresh_linear();
        }
    }

    fn spawn_action<F>(&mut self, f: F)
    where
        F: FnOnce() -> Result<String, String> + Send + 'static,
    {
        self.spawn_action_tagged(None, f);
    }

    fn spawn_action_tagged<F>(&mut self, tag: Option<ActionTag>, f: F)
    where
        F: FnOnce() -> Result<String, String> + Send + 'static,
    {
        self.pending_actions += 1;
        let tx = self.tx.clone();
        thread::spawn(move || {
            let _ = tx.send(AppEvent::ActionDone(tag, f()));
        });
    }

    // ---- app events ----

    pub fn on_app_event(&mut self, ev: AppEvent) {
        match ev {
            AppEvent::Login(Ok(login)) => {
                self.login = Some(login);
                self.refresh_lane(Lane::Mine);
                self.refresh_lane(Lane::Review);
                self.refresh_linear();
            }
            AppEvent::Login(Err(e)) => {
                self.toast(format!("gh auth failed: {e}"), true);
            }
            AppEvent::Lists(lane, res) => {
                let filter = self.effective_filter().to_string();
                let prev = {
                    let vis = self.visible_for(lane, &filter);
                    let ls = &self.lanes[lane as usize];
                    vis.get(ls.sel).map(|&i| (ls.prs[i].repo.clone(), ls.prs[i].number))
                };
                let ls = &mut self.lanes[lane as usize];
                ls.loading = false;
                ls.fetched_at = Some(Instant::now());
                match res {
                    Ok(prs) => {
                        ls.prs = prs;
                        let vis = self.visible_for(lane, &filter);
                        let ls = &mut self.lanes[lane as usize];
                        ls.sel = prev
                            .and_then(|(r, n)| {
                                vis.iter().position(|&i| {
                                    ls.prs[i].repo == r && ls.prs[i].number == n
                                })
                            })
                            .unwrap_or(0);
                        // CI states load separately, in cheap batched queries
                        let keys: Vec<(String, u64)> = self.lanes[lane as usize]
                            .prs
                            .iter()
                            .map(|p| (p.repo.clone(), p.number))
                            .collect();
                        for chunk in keys.chunks(20) {
                            let chunk = chunk.to_vec();
                            let tx = self.tx.clone();
                            thread::spawn(move || {
                                if let Ok(states) = gh::fetch_ci(&chunk) {
                                    let _ = tx.send(AppEvent::Ci(states));
                                }
                            });
                        }
                        if lane == self.lane && self.detail.key.is_none() {
                            self.fetch_selected_detail();
                        }
                        // the PR on screen moved forward on GitHub → silent refetch
                        if let Some((r, n)) = self.detail.key.clone() {
                            let newer = self.lanes[lane as usize]
                                .prs
                                .iter()
                                .find(|p| p.repo == r && p.number == n)
                                .map(|p| p.updated_at.clone());
                            if let Some(updated) = newer {
                                let stale = self
                                    .cache
                                    .get(&(r.clone(), n))
                                    .map(|e| e.for_updated != updated)
                                    .unwrap_or(false);
                                if stale {
                                    self.spawn_fetch(r, n, updated);
                                }
                            }
                        }
                    }
                    Err(e) => ls.error = Some(e),
                }
            }
            AppEvent::Detail(repo, number, res) => {
                let key = (repo, number);
                let current = self.detail.key.as_ref() == Some(&key);
                match res {
                    Ok(d) => {
                        if let Some(e) = self.cache.get_mut(&key) {
                            e.detail = Some(d.clone());
                        }
                        if current {
                            self.detail.loading = false;
                            self.detail.error = None;
                            self.detail.blocks = build_blocks(&d);
                            self.block_sel = self.block_sel.min(self.detail.blocks.len().saturating_sub(1));
                            self.detail.data = Some(d);
                        }
                    }
                    Err(e) => {
                        if current {
                            self.detail.loading = false;
                            self.detail.error = Some(e);
                        }
                    }
                }
            }
            AppEvent::Diff(repo, number, res) => {
                let key = (repo, number);
                self.in_flight.remove(&key);
                let current = self.detail.key.as_ref() == Some(&key);
                match res {
                    Ok(d) => {
                        let lines: Vec<String> = d.lines().map(str::to_string).collect();
                        let targets = map_diff(&lines);
                        let diff = DiffData { lines, targets };
                        if let Some(e) = self.cache.get_mut(&key) {
                            e.diff = Some(diff.clone());
                        }
                        if current {
                            self.detail.diff_loading = false;
                            self.diff_sel =
                                self.diff_sel.min(diff.lines.len().saturating_sub(1));
                            self.detail.diff = Some(diff);
                        }
                    }
                    Err(e) => {
                        if current {
                            self.detail.diff_loading = false;
                            self.detail.error = Some(e);
                        }
                    }
                }
            }
            AppEvent::ActionDone(tag, res) => {
                self.pending_actions = self.pending_actions.saturating_sub(1);
                match res {
                    Ok(msg) => {
                        if let Some(ActionTag::ReviewSubmitted(key)) = tag {
                            self.pending.remove(&key);
                        }
                        self.toast(msg, false);
                        self.refresh_lane(self.lane);
                        self.refetch_current();
                    }
                    Err(e) => self.toast(e, true),
                }
            }
            AppEvent::ClaudeLine(repo, number, line) => {
                if self.claude.key.as_ref() == Some(&(repo, number)) {
                    if !self.claude.text.is_empty() {
                        self.claude.text.push('\n');
                    }
                    self.claude.text.push_str(&line);
                }
            }
            AppEvent::ClaudeDone(repo, number, res) => {
                if self.claude.key.as_ref() == Some(&(repo, number)) {
                    self.claude.running = false;
                    if let Err(e) = res {
                        self.toast(format!("claude review failed: {e}"), true);
                    } else {
                        self.toast("claude review done — P to post as comment", false);
                    }
                }
            }
            AppEvent::Ci(states) => {
                for (repo, number, ci) in states {
                    for lane in self.lanes.iter_mut() {
                        for pr in lane.prs.iter_mut() {
                            if pr.repo == repo && pr.number == number {
                                pr.ci = ci;
                            }
                        }
                    }
                }
            }
            AppEvent::LinearIssues(res) => {
                self.linear.loading = false;
                self.linear.fetched_at = Some(Instant::now());
                match res {
                    Ok(issues) => {
                        let prev = self.selected_issue().map(|i| i.identifier.clone());
                        self.linear.issues = issues;
                        let want = self.linear.jump.clone().or(prev);
                        if let Some(id) = want {
                            let pos = self
                                .linear_visible()
                                .iter()
                                .position(|&i| self.linear.issues[i].identifier == id);
                            if let Some(p) = pos {
                                self.linear.sel = p;
                                self.linear.jump = None;
                            }
                        }
                        self.clamp_sel_linear();
                    }
                    Err(e) => self.linear.error = Some(e),
                }
            }
            AppEvent::LinearJump(ident, res) => {
                if self.linear.jump.as_deref() != Some(ident.as_str()) {
                    return;
                }
                self.linear.jump = None;
                match res {
                    Ok(issue) => {
                        if !self.linear.issues.iter().any(|i| i.identifier == issue.identifier) {
                            self.linear.issues.insert(0, issue);
                        }
                        if let Some(p) = self
                            .linear_visible()
                            .iter()
                            .position(|&i| self.linear.issues[i].identifier == ident)
                        {
                            self.linear.sel = p;
                            self.linear_scroll = 0;
                        }
                    }
                    Err(e) => self.toast(e, true),
                }
            }
            AppEvent::Rewrite(res) => {
                let mut err: Option<String> = None;
                if let Some(Input::Compose(c)) = &mut self.input {
                    if c.rewriting {
                        c.rewriting = false;
                        match res {
                            Ok(text) => c.set_text(&text),
                            Err(e) => err = Some(e),
                        }
                    }
                }
                if let Some(e) = err {
                    self.toast(format!("rewrite failed: {e}"), true);
                }
            }
        }
    }

    // ---- keys ----

    pub fn on_key(&mut self, key: KeyEvent) {
        if self.input.is_some() {
            self.on_input_key(key);
            return;
        }
        if let Some(modal) = self.modal {
            self.on_modal_key(modal, key);
            return;
        }
        if self.view == View::Linear {
            self.on_linear_key(key);
            return;
        }
        match key.code {
            KeyCode::Char('q') => {
                self.quit = true;
                return;
            }
            KeyCode::Char('?') => {
                self.modal = Some(Modal::Help);
                return;
            }
            KeyCode::Char('r') => {
                self.refresh_all();
                return;
            }
            KeyCode::Char('/') => {
                self.input = Some(Input::Filter { buf: self.filter.clone() });
                return;
            }
            KeyCode::Char('1') => {
                self.switch_lane(Lane::Mine);
                return;
            }
            KeyCode::Char('2') => {
                self.switch_lane(Lane::Review);
                return;
            }
            KeyCode::Char('3') => {
                self.switch_lane(Lane::Search);
                return;
            }
            KeyCode::Tab => {
                self.focus = match self.focus {
                    Focus::List => Focus::Detail,
                    Focus::Detail => Focus::List,
                };
                return;
            }
            KeyCode::Char('o')
                if !(self.focus == Focus::Detail
                    && matches!(self.tab, Tab::Checks | Tab::Comments)) =>
            {
                if let Some(pr) = self.selected_pr() {
                    gh::open_url(&pr.url);
                }
                return;
            }
            KeyCode::Char('m') => {
                if self.selected_pr().is_some() {
                    self.modal = Some(Modal::Merge);
                }
                return;
            }
            KeyCode::Char('u') if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                if self.selected_pr().is_some() {
                    self.modal = Some(Modal::Update);
                }
                return;
            }
            KeyCode::Char('v') => {
                if self.selected_pr().is_some() {
                    self.modal = Some(Modal::Review);
                }
                return;
            }
            KeyCode::Char('c') => {
                self.open_composer();
                return;
            }
            KeyCode::Char('w') => {
                if let Some(pr) = self.selected_pr() {
                    let (repo, branch) = (pr.repo.clone(), pr.head_ref.clone());
                    self.toast(format!("preparing worktree for {branch}…"), false);
                    self.spawn_action(move || {
                        repo::ensure_worktree(&repo, &branch)
                            .map(|p| format!("worktree ready: {}", p.display()))
                    });
                }
                return;
            }
            KeyCode::Char('4') => {
                self.open_linear(None);
                return;
            }
            KeyCode::Char('L') => {
                if let Some(pr) = self.selected_pr() {
                    match extract_ticket(&pr.title).or_else(|| extract_ticket(&pr.head_ref)) {
                        Some(ticket) => self.open_linear(Some(ticket)),
                        None => self.toast("no ticket id found in title or branch", true),
                    }
                }
                return;
            }
            KeyCode::Char('f') if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.open_link_picker();
                return;
            }
            KeyCode::Char('R') => {
                self.start_claude_review();
                return;
            }
            KeyCode::Char('P') => {
                if self.claude.key.is_some() && !self.claude.running && !self.claude.text.is_empty()
                {
                    self.modal = Some(Modal::ConfirmPost);
                } else {
                    self.toast("no finished claude review to post", true);
                }
                return;
            }
            _ => {}
        }
        match self.focus {
            Focus::List => self.on_list_key(key),
            Focus::Detail => self.on_detail_key(key),
        }
    }

    fn on_linear_key(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Char('?') => self.modal = Some(Modal::Help),
            KeyCode::Char('r') => self.refresh_linear(),
            KeyCode::Char('/') => {
                self.input = Some(Input::Filter { buf: self.filter.clone() });
            }
            KeyCode::Char('1') => {
                self.view = View::Prs;
                self.switch_lane(Lane::Mine);
            }
            KeyCode::Char('2') => {
                self.view = View::Prs;
                self.switch_lane(Lane::Review);
            }
            KeyCode::Char('3') => {
                self.view = View::Prs;
                self.switch_lane(Lane::Search);
            }
            KeyCode::Tab => {
                self.focus = match self.focus {
                    Focus::List => Focus::Detail,
                    Focus::Detail => Focus::List,
                };
            }
            KeyCode::Char('o') => {
                if let Some(i) = self.selected_issue() {
                    gh::open_url(&i.url);
                }
            }
            KeyCode::Enter => self.focus = Focus::Detail,
            KeyCode::Esc => match self.focus {
                Focus::Detail => self.focus = Focus::List,
                Focus::List => self.view = View::Prs,
            },
            KeyCode::Char('j') | KeyCode::Down => match self.focus {
                Focus::List => {
                    let n = self.linear_visible().len();
                    if n > 0 && self.linear.sel + 1 < n {
                        self.linear.sel += 1;
                        self.linear_scroll = 0;
                    }
                }
                Focus::Detail => self.linear_scroll += 1,
            },
            KeyCode::Char('k') | KeyCode::Up => match self.focus {
                Focus::List => {
                    if self.linear.sel > 0 {
                        self.linear.sel -= 1;
                        self.linear_scroll = 0;
                    }
                }
                Focus::Detail => self.linear_scroll = self.linear_scroll.saturating_sub(1),
            },
            KeyCode::Char('d') if ctrl => self.linear_scroll += 20,
            KeyCode::Char('u') if ctrl => {
                self.linear_scroll = self.linear_scroll.saturating_sub(20)
            }
            KeyCode::Char('f') if ctrl => self.page_scroll(1.0),
            KeyCode::Char('b') if ctrl => self.page_scroll(-1.0),
            KeyCode::Char('f') => self.open_link_picker(),
            KeyCode::Char('g') => match self.focus {
                Focus::List => {
                    self.linear.sel = 0;
                    self.linear_scroll = 0;
                }
                Focus::Detail => self.linear_scroll = 0,
            },
            KeyCode::Char('G') => {
                if self.focus == Focus::List {
                    let n = self.linear_visible().len();
                    self.linear.sel = n.saturating_sub(1);
                    self.linear_scroll = 0;
                }
            }
            _ => {}
        }
    }

    fn switch_lane(&mut self, lane: Lane) {
        if self.lane != lane {
            self.lane = lane;
            self.focus = Focus::List;
            self.clamp_sel();
            if lane == Lane::Search && self.lanes[Lane::Search as usize].prs.is_empty() {
                if self.last_search.is_none() {
                    self.toast("press / and search — e.g. `author:x merged` or `ABC-123`", false);
                }
            }
        }
    }

    // ---- filter input ----

    fn on_input_key(&mut self, key: KeyEvent) {
        match &self.input {
            Some(Input::Filter { .. }) => self.on_filter_key(key),
            Some(Input::Compose(_)) => self.on_compose_key(key),
            None => {}
        }
    }

    fn on_filter_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.input = None;
                self.filter.clear();
                self.clamp_sel();
            }
            KeyCode::Enter => {
                let buf = match &self.input {
                    Some(Input::Filter { buf }) => buf.trim().to_string(),
                    _ => return,
                };
                self.commit_filter(buf);
            }
            KeyCode::Backspace => {
                if let Some(Input::Filter { buf }) = &mut self.input {
                    buf.pop();
                }
                self.clamp_sel();
            }
            KeyCode::Char(ch) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                if let Some(Input::Filter { buf }) = &mut self.input {
                    buf.push(ch);
                }
                self.clamp_sel();
            }
            _ => {}
        }
    }

    fn commit_filter(&mut self, buf: String) {
        // linear view: purely local filtering
        if self.view == View::Linear {
            self.input = None;
            self.filter = buf;
            self.clamp_sel();
            return;
        }
        let has_qualifier = buf
            .split_whitespace()
            .any(|t| t.contains(':') || matches!(t, "merged" | "closed" | "draft"));
        let no_local_hits = !buf.is_empty() && self.visible().is_empty();
        self.input = None;
        if has_qualifier || no_local_hits {
            self.filter.clear();
            self.run_search(buf);
        } else {
            self.filter = buf;
            self.clamp_sel();
        }
    }

    fn run_search(&mut self, raw: String) {
        if raw.is_empty() {
            return;
        }
        if self.login.is_none() {
            self.toast("still resolving gh login — try again in a second", true);
            return;
        }
        self.last_search = Some(raw);
        self.lane = Lane::Search;
        self.focus = Focus::List;
        self.lanes[Lane::Search as usize].sel = 0;
        self.lanes[Lane::Search as usize].prs.clear();
        self.refresh_lane(Lane::Search);
    }

    // ---- composer ----

    fn open_composer(&mut self) {
        // reply to the selected review thread?
        if self.focus == Focus::Detail && self.tab == Tab::Comments {
            if let (Some(d), Some(CBlock::Thread(ti))) =
                (self.detail.data.as_ref(), self.detail.blocks.get(self.block_sel))
            {
                if let Some(t) = d.threads.get(*ti) {
                    if let Some(cid) = t.comments.first().and_then(|c| c.db_id) {
                        let loc = match t.line {
                            Some(l) => format!("{}:{}", t.path, l),
                            None => t.path.clone(),
                        };
                        self.input = Some(Input::Compose(Compose::new(ComposeTarget::ThreadReply {
                            repo: d.repo.clone(),
                            number: d.number,
                            comment_id: cid,
                            loc,
                        })));
                        return;
                    }
                }
            }
        }
        // inline comment on the selected diff line? (queued, sent with `v`)
        if self.focus == Focus::Detail && self.tab == Tab::Diff {
            let Some(d) = self.detail.data.as_ref() else {
                self.toast("detail still loading", true);
                return;
            };
            let target = self
                .detail
                .diff
                .as_ref()
                .and_then(|diff| diff.targets.get(self.diff_sel).cloned().flatten());
            match target {
                Some(t) => {
                    self.input = Some(Input::Compose(Compose::new(ComposeTarget::Inline {
                        repo: d.repo.clone(),
                        number: d.number,
                        target: t,
                    })));
                }
                None => self.toast("not a commentable diff line", true),
            }
            return;
        }
        // plain PR comment
        if let Some(pr) = self.selected_pr() {
            self.input = Some(Input::Compose(Compose::new(ComposeTarget::PrComment {
                repo: pr.repo.clone(),
                number: pr.number,
            })));
        } else {
            self.toast("no PR selected", true);
        }
    }

    fn open_review_composer(&mut self, event: ReviewEvent) {
        if let Some(pr) = self.selected_pr() {
            self.input = Some(Input::Compose(Compose::new(ComposeTarget::Review {
                repo: pr.repo.clone(),
                number: pr.number,
                event,
            })));
        }
    }

    fn on_compose_key(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => {
                self.input = None;
                return;
            }
            KeyCode::Char('s') if ctrl => {
                self.submit_compose();
                return;
            }
            KeyCode::Char('r') if ctrl => {
                self.rewrite_compose();
                return;
            }
            KeyCode::Char('z') if ctrl => {
                if let Some(Input::Compose(c)) = &mut self.input {
                    if !c.rewriting {
                        if let Some(p) = c.prev.take() {
                            c.lines = p;
                            c.row = c.lines.len() - 1;
                            c.col = c.line_chars(c.row);
                        }
                    }
                }
                return;
            }
            _ => {}
        }
        if let Some(Input::Compose(c)) = &mut self.input {
            if c.rewriting {
                return;
            }
            match key.code {
                KeyCode::Enter => c.newline(),
                KeyCode::Backspace => c.backspace(),
                KeyCode::Tab => {
                    c.insert(' ');
                    c.insert(' ');
                }
                KeyCode::Left | KeyCode::Right | KeyCode::Up | KeyCode::Down => {
                    c.move_cursor(key.code)
                }
                KeyCode::Char(ch) if !ctrl => c.insert(ch),
                _ => {}
            }
        }
    }

    fn submit_compose(&mut self) {
        let (body, target, rewriting) = match &self.input {
            Some(Input::Compose(c)) => (c.text().trim().to_string(), c.target.clone(), c.rewriting),
            _ => return,
        };
        if rewriting {
            self.toast("rewrite in progress — wait or esc", true);
            return;
        }
        if body.is_empty() && !target.allows_empty() {
            self.toast("empty comment", true);
            return;
        }
        self.input = None;
        match target {
            ComposeTarget::PrComment { repo, number } => {
                self.spawn_action(move || gh::post_comment(&repo, number, &body));
            }
            ComposeTarget::ThreadReply { repo, number, comment_id, .. } => {
                self.spawn_action(move || gh::reply_thread(&repo, number, comment_id, &body));
            }
            ComposeTarget::Inline { repo, number, target } => {
                let key = (repo, number);
                let queue = self.pending.entry(key).or_default();
                queue.push(PendingComment {
                    path: target.path,
                    line: target.line,
                    side: target.side.to_string(),
                    body,
                });
                let n = queue.len();
                self.toast(format!("queued for review ({n}) — v to submit"), false);
            }
            ComposeTarget::Review { repo, number, event } => {
                let key = (repo.clone(), number);
                let comments = self.pending.get(&key).cloned().unwrap_or_default();
                self.spawn_action_tagged(Some(ActionTag::ReviewSubmitted(key)), move || {
                    gh::submit_review(&repo, number, event.rest(), &body, &comments)
                });
            }
        }
    }

    fn rewrite_compose(&mut self) {
        let mut msg: Option<&str> = None;
        if let Some(Input::Compose(c)) = &mut self.input {
            if c.rewriting {
                msg = Some("rewrite already running");
            } else if c.text().trim().is_empty() {
                msg = Some("nothing to rewrite");
            } else {
                c.prev = Some(c.lines.clone());
                c.rewriting = true;
                llm::spawn_rewrite(self.tx.clone(), c.text());
            }
        }
        if let Some(m) = msg {
            self.toast(m, true);
        }
    }

    // ---- modals ----

    fn on_modal_key(&mut self, modal: Modal, key: KeyEvent) {
        match modal {
            Modal::Help => {
                self.modal = None;
            }
            Modal::Merge => match key.code {
                KeyCode::Char('s') | KeyCode::Char('m') | KeyCode::Char('b') => {
                    let method = match key.code {
                        KeyCode::Char('s') => "squash",
                        KeyCode::Char('m') => "merge",
                        _ => "rebase",
                    };
                    self.modal = None;
                    if let Some(pr) = self.selected_pr() {
                        let (repo, number) = (pr.repo.clone(), pr.number);
                        let method = method.to_string();
                        self.toast(format!("merging {repo}#{number} ({method})…"), false);
                        self.spawn_action(move || gh::merge_pr(&repo, number, &method));
                    }
                }
                _ => self.modal = None,
            },
            Modal::Update => match key.code {
                KeyCode::Char('b') | KeyCode::Char('m') => {
                    let method = if key.code == KeyCode::Char('b') { "REBASE" } else { "MERGE" };
                    self.modal = None;
                    if let Some(pr) = self.selected_pr() {
                        let id = pr.id.clone();
                        let base = pr.base_ref.clone();
                        let method = method.to_string();
                        self.toast(
                            format!("updating branch onto {base} ({})…", method.to_lowercase()),
                            false,
                        );
                        self.spawn_action(move || gh::update_branch(&id, &method));
                    }
                }
                _ => self.modal = None,
            },
            Modal::Review => match key.code {
                KeyCode::Char('a') => {
                    self.modal = None;
                    self.open_review_composer(ReviewEvent::Approve);
                }
                KeyCode::Char('r') => {
                    self.modal = None;
                    self.open_review_composer(ReviewEvent::RequestChanges);
                }
                KeyCode::Char('c') => {
                    self.modal = None;
                    self.open_review_composer(ReviewEvent::Comment);
                }
                KeyCode::Char('d') => {
                    self.modal = None;
                    if let Some(pr) = self.selected_pr() {
                        let key = (pr.repo.clone(), pr.number);
                        if let Some(dropped) = self.pending.remove(&key) {
                            self.toast(
                                format!("discarded {} pending comment(s)", dropped.len()),
                                false,
                            );
                        }
                    }
                }
                _ => self.modal = None,
            },
            Modal::Links => match key.code {
                KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('f') => self.modal = None,
                KeyCode::Char('j') | KeyCode::Down => {
                    if self.links_sel + 1 < self.links.len() {
                        self.links_sel += 1;
                    }
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    self.links_sel = self.links_sel.saturating_sub(1)
                }
                KeyCode::Enter | KeyCode::Char('o') => {
                    if let Some((_, url)) = self.links.get(self.links_sel) {
                        gh::open_url(url);
                    }
                    self.modal = None;
                }
                KeyCode::Char(c @ '1'..='9') => {
                    let idx = c as usize - '1' as usize;
                    if let Some((_, url)) = self.links.get(idx) {
                        gh::open_url(url);
                        self.modal = None;
                    }
                }
                _ => {}
            },
            Modal::ConfirmPost => match key.code {
                KeyCode::Char('y') => {
                    self.modal = None;
                    if let Some((repo, number)) = self.claude.key.clone() {
                        let body = format!("## arrano review\n\n{}", self.claude.text);
                        self.spawn_action(move || gh::post_comment(&repo, number, &body));
                    }
                }
                _ => self.modal = None,
            },
        }
    }

    // ---- list / detail navigation ----

    fn on_list_key(&mut self, key: KeyEvent) {
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('f') => {
                    self.page_scroll(1.0);
                    return;
                }
                KeyCode::Char('b') => {
                    self.page_scroll(-1.0);
                    return;
                }
                _ => {}
            }
        }
        let len = self.visible().len();
        let ls = &mut self.lanes[self.lane as usize];
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => {
                if len > 0 && ls.sel + 1 < len {
                    ls.sel += 1;
                    self.sel_changed_at = Some(Instant::now());
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                if ls.sel > 0 {
                    ls.sel -= 1;
                    self.sel_changed_at = Some(Instant::now());
                }
            }
            KeyCode::Char('g') => {
                if ls.sel != 0 {
                    ls.sel = 0;
                    self.sel_changed_at = Some(Instant::now());
                }
            }
            KeyCode::Char('G') => {
                if len > 0 && ls.sel != len - 1 {
                    ls.sel = len - 1;
                    self.sel_changed_at = Some(Instant::now());
                }
            }
            KeyCode::Enter => {
                self.fetch_selected_detail();
                self.focus = Focus::Detail;
            }
            _ => {}
        }
    }

    fn on_detail_key(&mut self, key: KeyEvent) {
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('f') => {
                    self.page_scroll(1.0);
                    return;
                }
                KeyCode::Char('b') => {
                    self.page_scroll(-1.0);
                    return;
                }
                _ => {}
            }
        }
        match key.code {
            KeyCode::Char('h') | KeyCode::Left => {
                let i = self.tab as usize;
                self.tab = TABS[(i + TABS.len() - 1) % TABS.len()];
                return;
            }
            KeyCode::Char('l') | KeyCode::Right => {
                let i = self.tab as usize;
                self.tab = TABS[(i + 1) % TABS.len()];
                return;
            }
            KeyCode::Esc => {
                self.focus = Focus::List;
                return;
            }
            _ => {}
        }
        let page = 20;
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match self.tab {
            Tab::Overview => match key.code {
                KeyCode::Char('j') | KeyCode::Down => self.overview_scroll += 1,
                KeyCode::Char('d') if ctrl => self.overview_scroll += page as u16,
                KeyCode::Char('k') | KeyCode::Up => {
                    self.overview_scroll = self.overview_scroll.saturating_sub(1)
                }
                KeyCode::Char('u') if ctrl => {
                    self.overview_scroll = self.overview_scroll.saturating_sub(page as u16)
                }
                KeyCode::Char('g') => self.overview_scroll = 0,
                _ => {}
            },
            Tab::Diff => {
                let n = self.detail.diff.as_ref().map(|d| d.lines.len()).unwrap_or(0);
                match key.code {
                    KeyCode::Char('j') | KeyCode::Down => {
                        if n > 0 && self.diff_sel + 1 < n {
                            self.diff_sel += 1;
                        }
                    }
                    KeyCode::Char('k') | KeyCode::Up => {
                        self.diff_sel = self.diff_sel.saturating_sub(1)
                    }
                    KeyCode::Char('d') if ctrl => {
                        self.diff_sel = (self.diff_sel + page).min(n.saturating_sub(1))
                    }
                    KeyCode::Char('u') if ctrl => self.diff_sel = self.diff_sel.saturating_sub(page),
                    KeyCode::Char('g') => self.diff_sel = 0,
                    KeyCode::Char('G') => self.diff_sel = n.saturating_sub(1),
                    _ => {}
                }
            }
            Tab::Checks => {
                let n = self.detail.data.as_ref().map(|d| d.checks.len()).unwrap_or(0);
                match key.code {
                    KeyCode::Char('j') | KeyCode::Down => {
                        if n > 0 && self.checks_sel + 1 < n {
                            self.checks_sel += 1;
                        }
                    }
                    KeyCode::Char('k') | KeyCode::Up => {
                        self.checks_sel = self.checks_sel.saturating_sub(1)
                    }
                    KeyCode::Enter | KeyCode::Char('o') => {
                        if let Some(c) = self
                            .detail
                            .data
                            .as_ref()
                            .and_then(|d| d.checks.get(self.checks_sel))
                        {
                            if !c.url.is_empty() {
                                gh::open_url(&c.url);
                            }
                        }
                    }
                    _ => {}
                }
            }
            Tab::Comments => {
                let n = self.detail.blocks.len();
                match key.code {
                    KeyCode::Char('j') | KeyCode::Down => {
                        if n > 0 && self.block_sel + 1 < n {
                            self.block_sel += 1;
                        }
                    }
                    KeyCode::Char('k') | KeyCode::Up => {
                        self.block_sel = self.block_sel.saturating_sub(1)
                    }
                    KeyCode::Char('x') => self.toggle_selected_thread(),
                    KeyCode::Char('z') => {
                        if let Some(CBlock::Thread(ti)) =
                            self.detail.blocks.get(self.block_sel).cloned()
                        {
                            let folded = self.thread_folded(ti);
                            self.detail.folds.insert(ti, !folded);
                        }
                    }
                    KeyCode::Char('o') | KeyCode::Enter => {
                        let url = self.detail.data.as_ref().and_then(|d| {
                            match self.detail.blocks.get(self.block_sel)? {
                                CBlock::Comment(i) => d.comments.get(*i).map(|c| c.url.clone()),
                                CBlock::Review(i) => d.reviews.get(*i).map(|r| r.url.clone()),
                                CBlock::Thread(i) => d
                                    .threads
                                    .get(*i)
                                    .and_then(|t| t.comments.first())
                                    .map(|c| c.url.clone()),
                            }
                        });
                        match url.filter(|u| !u.is_empty()) {
                            Some(u) => gh::open_url(&u),
                            None => {
                                if let Some(pr) = self.selected_pr() {
                                    gh::open_url(&pr.url);
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
            Tab::Claude => match key.code {
                KeyCode::Char('j') | KeyCode::Down => {
                    self.claude_scroll += 1;
                    self.claude.follow = false;
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    self.claude_scroll = self.claude_scroll.saturating_sub(1);
                    self.claude.follow = false;
                }
                KeyCode::Char('d') if ctrl => {
                    self.claude_scroll += page as u16;
                    self.claude.follow = false;
                }
                KeyCode::Char('u') if ctrl => {
                    self.claude_scroll = self.claude_scroll.saturating_sub(page as u16);
                    self.claude.follow = false;
                }
                KeyCode::Char('G') => self.claude.follow = true,
                KeyCode::Char('g') => {
                    self.claude_scroll = 0;
                    self.claude.follow = false;
                }
                _ => {}
            },
        }
    }

    fn toggle_selected_thread(&mut self) {
        let Some(CBlock::Thread(ti)) = self.detail.blocks.get(self.block_sel).cloned() else {
            self.toast("not a review thread — nothing to resolve", true);
            return;
        };
        let Some(t) = self.detail.data.as_ref().and_then(|d| d.threads.get(ti)) else {
            return;
        };
        let id = t.id.clone();
        let resolve = !t.resolved;
        self.spawn_action(move || gh::set_thread_resolved(&id, resolve));
    }

    fn start_claude_review(&mut self) {
        if self.claude.running {
            self.toast("claude review already running", true);
            return;
        }
        let Some(d) = self.detail.data.as_ref() else {
            self.toast("no PR detail loaded yet", true);
            return;
        };
        let Some(diff) = self.detail.diff.as_ref() else {
            self.toast("diff not loaded yet", true);
            return;
        };
        self.claude = ClaudeState {
            key: Some((d.repo.clone(), d.number)),
            text: String::new(),
            running: true,
            follow: true,
        };
        self.claude_scroll = 0;
        self.tab = Tab::Claude;
        self.focus = Focus::Detail;
        // best available local context: a checkout of the PR branch, else the
        // repo's main checkout, else diff-only
        let ctx = repo::branch_checkout(&d.repo, &d.head_ref)
            .map(ReviewCtx::Branch)
            .or_else(|| repo::local_repo_path(&d.repo).map(ReviewCtx::Repo))
            .unwrap_or(ReviewCtx::None);
        claude::spawn_review(
            self.tx.clone(),
            d.repo.clone(),
            d.number,
            d.title.clone(),
            d.body.clone(),
            d.base_ref.clone(),
            d.head_ref.clone(),
            diff.lines.join("\n"),
            ctx,
        );
    }
}

/// Find a Linear-style ticket id (ABC-123) in a title or branch name.
/// Case-insensitive; returns it uppercased.
pub fn extract_ticket(s: &str) -> Option<String> {
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        // word boundary before the team prefix
        if i > 0 && (chars[i - 1].is_ascii_alphanumeric() || chars[i - 1] == '-') {
            i += 1;
            continue;
        }
        let letters = chars[i..].iter().take_while(|c| c.is_ascii_alphabetic()).count();
        if (2..=10).contains(&letters) && chars.get(i + letters) == Some(&'-') {
            let digits = chars[i + letters + 1..]
                .iter()
                .take_while(|c| c.is_ascii_digit())
                .count();
            let after = chars.get(i + letters + 1 + digits);
            let boundary = after.map(|c| !c.is_ascii_alphanumeric()).unwrap_or(true);
            if (1..=6).contains(&digits) && boundary {
                return Some(
                    chars[i..i + letters + 1 + digits]
                        .iter()
                        .collect::<String>()
                        .to_uppercase(),
                );
            }
        }
        i += 1;
    }
    None
}

fn build_blocks(d: &PrDetail) -> Vec<CBlock> {
    let mut blocks: Vec<(String, CBlock)> = Vec::new();
    for (i, c) in d.comments.iter().enumerate() {
        blocks.push((c.created_at.clone(), CBlock::Comment(i)));
    }
    for (i, r) in d.reviews.iter().enumerate() {
        blocks.push((r.created_at.clone(), CBlock::Review(i)));
    }
    for (i, t) in d.threads.iter().enumerate() {
        let at = t.comments.first().map(|c| c.created_at.clone()).unwrap_or_default();
        blocks.push((at, CBlock::Thread(i)));
    }
    blocks.sort_by(|a, b| a.0.cmp(&b.0));
    blocks.into_iter().map(|(_, b)| b).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Ci;

    fn pr(repo: &str, number: u64, title: &str, head: &str) -> Pr {
        Pr {
            id: "x".into(),
            repo: repo.into(),
            number,
            title: title.into(),
            url: String::new(),
            author: "alice".into(),
            is_draft: false,
            updated_at: String::new(),
            base_ref: "master".into(),
            head_ref: head.into(),
            review_decision: String::new(),
            ci: Ci::None,
        }
    }

    #[test]
    fn filter_matches_number_ticket_and_branch() {
        let p = pr("acme/api", 7073, "fix: ABC-123 delete zero-width chars", "abc-123-import");
        assert!(pr_matches(&p, "7073"));
        assert!(pr_matches(&p, "#7073"));
        assert!(pr_matches(&p, "abc-123"));
        assert!(pr_matches(&p, "api zero-width"));
        assert!(!pr_matches(&p, "gateway"));
    }

    #[test]
    fn ticket_extraction() {
        assert_eq!(extract_ticket("fix: ABC-123 delete zero-width chars"), Some("ABC-123".into()));
        assert_eq!(extract_ticket("abc-123-import-channel-framing"), Some("ABC-123".into()));
        // "channel-framing" must not match: prefix follows a '-'
        assert_eq!(extract_ticket("import-channel-framing"), None);
        assert_eq!(extract_ticket("plain title without ticket"), None);
        assert_eq!(extract_ticket("feat/ABC-1"), Some("ABC-1".into()));
    }

    #[test]
    fn search_expansion() {
        let q = expand_search("author:@me merged ABC-123", "alice", Some("acme"));
        assert!(q.contains("author:alice"));
        assert!(q.contains("is:merged"));
        assert!(q.contains("org:acme"));
        assert!(q.ends_with("ABC-123"));
        // explicit repo scope suppresses the org default
        let q = expand_search("repo:foo/bar #123", "alice", Some("acme"));
        assert!(!q.contains("org:acme"));
        assert!(q.ends_with("123"));
    }
}
