use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;

use crate::app::{App, CBlock, Compose, Focus, Input, Lane, Modal, Tab, View, TABS};
use crate::linear::LinearIssue;
use crate::md;
use crate::model::{rel_time, Check, Ci, Comment, Pr};
use crate::syntax;

const SPINNER: [&str; 4] = ["⠋", "⠙", "⠸", "⠴"];

pub fn draw(f: &mut Frame, app: &mut App) {
    let [main, status] =
        *Layout::vertical([Constraint::Min(3), Constraint::Length(1)]).split(f.area())
    else {
        return;
    };
    let [left, right] =
        *Layout::horizontal([Constraint::Percentage(36), Constraint::Min(20)]).split(main)
    else {
        return;
    };

    app.areas = crate::app::Areas { left, right };
    match app.view {
        View::Prs => {
            draw_list(f, app, left);
            draw_detail(f, app, right);
        }
        View::Linear => {
            draw_linear_list(f, app, left);
            draw_linear_detail(f, app, right);
        }
    }
    draw_status(f, app, status);

    match app.modal {
        Some(Modal::Merge) => draw_choice_modal(
            f,
            "merge PR",
            &[("s", "squash"), ("m", "merge commit"), ("b", "rebase")],
        ),
        Some(Modal::Update) => draw_choice_modal(
            f,
            "update branch from base",
            &[("b", "rebase"), ("m", "merge base in")],
        ),
        Some(Modal::Review) => {
            let n = app
                .selected_pr()
                .and_then(|pr| app.pending.get(&(pr.repo.clone(), pr.number)))
                .map(|v| v.len())
                .unwrap_or(0);
            let title = if n > 0 {
                format!("submit review — {n} queued comment(s) attached")
            } else {
                "submit review".to_string()
            };
            let mut choices =
                vec![("a", "approve"), ("r", "request changes"), ("c", "comment")];
            if n > 0 {
                choices.push(("d", "discard queued comments"));
            }
            draw_choice_modal(f, &title, &choices);
        }
        Some(Modal::ConfirmPost) => draw_choice_modal(
            f,
            "post claude review as PR comment?",
            &[("y", "post it"), ("esc", "cancel")],
        ),
        Some(Modal::Links) => draw_links(f, app),
        Some(Modal::Help) => draw_help(f),
        None => {}
    }

    if let Some(Input::Compose(c)) = &app.input {
        draw_composer(f, app, c);
    }
}

fn border_style(active: bool) -> Style {
    if active {
        Style::new().fg(Color::Cyan)
    } else {
        Style::new().fg(Color::DarkGray)
    }
}

fn spinner(app: &App) -> &'static str {
    SPINNER[(app.tick / 2) as usize % SPINNER.len()]
}

fn ci_span(ci: Ci) -> Span<'static> {
    match ci {
        Ci::Pass => Span::styled("✓", Style::new().fg(Color::Green)),
        Ci::Fail => Span::styled("✗", Style::new().fg(Color::Red)),
        Ci::Pending => Span::styled("●", Style::new().fg(Color::Yellow)),
        Ci::None => Span::styled("·", Style::new().fg(Color::DarkGray)),
    }
}

fn review_span(decision: &str) -> Span<'static> {
    match decision {
        "APPROVED" => Span::styled("✓", Style::new().fg(Color::Green)),
        "CHANGES_REQUESTED" => Span::styled("±", Style::new().fg(Color::Red)),
        "REVIEW_REQUIRED" => Span::styled("○", Style::new().fg(Color::Yellow)),
        _ => Span::styled(" ", Style::new()),
    }
}

fn short_repo(repo: &str) -> &str {
    repo.rsplit_once('/').map(|(_, r)| r).unwrap_or(repo)
}

// ---- left: PR list ----

fn draw_list(f: &mut Frame, app: &App, area: Rect) {
    let ls = app.lane_state();
    let brand = match &app.org {
        Some(o) => format!(" arrano · {o} "),
        None => " arrano ".to_string(),
    };
    let mut title_spans = vec![Span::styled(brand, Style::new().add_modifier(Modifier::BOLD))];
    for lane in [Lane::Mine, Lane::Review, Lane::Search] {
        let n = app.lanes[lane as usize].prs.len();
        let label = format!("[{}] {} ({}) ", lane as usize + 1, lane.title(), n);
        let style = if lane == app.lane {
            Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD)
        } else {
            Style::new().fg(Color::DarkGray)
        };
        title_spans.push(Span::styled(label, style));
    }
    if !app.effective_filter().is_empty() {
        title_spans.push(Span::styled(
            format!("/{} ", app.effective_filter()),
            Style::new().fg(Color::Yellow),
        ));
    }
    if ls.loading {
        title_spans.push(Span::styled(spinner(app), Style::new().fg(Color::Yellow)));
    }
    let mut block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style(app.focus == Focus::List))
        .title(Line::from(title_spans));
    if let Some(at) = ls.fetched_at {
        let secs = at.elapsed().as_secs();
        let age = if secs < 60 { format!(" synced {secs}s ago ") } else { format!(" synced {}m ago ", secs / 60) };
        block = block.title_bottom(Line::from(Span::styled(age, Style::new().fg(Color::DarkGray))));
    }
    let inner = block.inner(area);
    f.render_widget(block, area);

    let mut lines: Vec<Line> = Vec::new();
    let mut sel_line: usize = 0;
    let vis = app.visible();
    if let Some(err) = &ls.error {
        lines.push(Line::from(Span::styled(
            format!("error: {err}"),
            Style::new().fg(Color::Red),
        )));
    } else if ls.prs.is_empty() {
        let msg = if app.login.is_none() || ls.loading {
            "loading…"
        } else if app.lane == Lane::Search {
            "press / to search — qualifiers pass through:\n  author:x  is:merged  repo:o/r\nbare words search text (ABC-123);\nmerged/closed/draft work bare"
        } else {
            "no open PRs"
        };
        for l in msg.lines() {
            lines.push(Line::from(Span::styled(l.to_string(), Style::new().fg(Color::DarkGray))));
        }
    } else if vis.is_empty() {
        lines.push(Line::from(Span::styled(
            "no matches — enter searches GitHub, esc clears",
            Style::new().fg(Color::DarkGray),
        )));
    } else {
        let mut last_repo = "";
        for (pos, &idx) in vis.iter().enumerate() {
            let pr = &ls.prs[idx];
            if pr.repo != last_repo {
                last_repo = &pr.repo;
                let header = if app.org.is_some() { short_repo(&pr.repo) } else { &pr.repo };
                lines.push(Line::from(Span::styled(
                    format!(" {header}"),
                    Style::new().fg(Color::Magenta).add_modifier(Modifier::BOLD),
                )));
            }
            if pos == ls.sel {
                sel_line = lines.len();
            }
            lines.push(pr_row(pr, pos == ls.sel, app.lane != Lane::Mine, inner.width));
        }
    }

    let height = inner.height as usize;
    let scroll = if sel_line >= height { (sel_line + 1 - height) as u16 } else { 0 };
    f.render_widget(Paragraph::new(Text::from(lines)).scroll((scroll, 0)), inner);
}

fn pr_row(pr: &Pr, selected: bool, show_author: bool, width: u16) -> Line<'static> {
    let mut spans: Vec<Span> = Vec::new();
    spans.push(Span::raw(if selected { "▶ " } else { "  " }));
    spans.push(ci_span(pr.ci));
    spans.push(review_span(&pr.review_decision));
    let num_style = if pr.is_draft {
        Style::new().fg(Color::DarkGray)
    } else {
        Style::new().fg(Color::Yellow)
    };
    spans.push(Span::styled(format!(" #{:<5}", pr.number), num_style));

    let meta = if show_author {
        format!(" {} {}", pr.author, rel_time(&pr.updated_at))
    } else {
        format!(" {}", rel_time(&pr.updated_at))
    };
    let used: usize = spans.iter().map(|s| s.content.chars().count()).sum::<usize>()
        + meta.chars().count();
    let avail = (width as usize).saturating_sub(used + 1);
    let mut title: String = pr.title.chars().take(avail).collect();
    if title.chars().count() < pr.title.chars().count() && !title.is_empty() {
        title.pop();
        title.push('…');
    }
    let pad = avail.saturating_sub(title.chars().count());
    let title_style = if selected {
        Style::new().add_modifier(Modifier::BOLD)
    } else if pr.is_draft {
        Style::new().fg(Color::DarkGray)
    } else {
        Style::new()
    };
    spans.push(Span::styled(title, title_style));
    spans.push(Span::raw(" ".repeat(pad)));
    spans.push(Span::styled(meta, Style::new().fg(Color::DarkGray)));

    let mut line = Line::from(spans);
    if selected {
        line = line.style(Style::new().bg(Color::Rgb(40, 44, 52)));
    }
    line
}

// ---- right: detail tabs ----

fn draw_detail(f: &mut Frame, app: &mut App, area: Rect) {
    let mut tab_spans: Vec<Span> = vec![Span::raw(" ")];
    for t in TABS {
        let mut label = format!(" {} ", t.title());
        if t == Tab::Claude && app.claude.running {
            label = format!(" {} {}", t.title(), spinner(app));
        }
        let style = if t == app.tab {
            Style::new().fg(Color::Black).bg(Color::Cyan).add_modifier(Modifier::BOLD)
        } else {
            Style::new().fg(Color::DarkGray)
        };
        tab_spans.push(Span::styled(label, style));
        tab_spans.push(Span::raw(" "));
    }
    let title = match &app.detail.key {
        Some((repo, n)) => format!(" {}#{} ", short_repo(repo), n),
        None => " no PR selected ".into(),
    };
    let mut bottom = vec![Span::styled(title, Style::new().fg(Color::DarkGray))];
    if let Some(key) = &app.detail.key {
        if let Some(pending) = app.pending.get(key) {
            if !pending.is_empty() {
                bottom.push(Span::styled(
                    format!("· ● {} queued for review — v to submit ", pending.len()),
                    Style::new().fg(Color::Yellow),
                ));
            }
        }
    }
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style(app.focus == Focus::Detail))
        .title(Line::from(tab_spans))
        .title_bottom(Line::from(bottom));
    let inner = block.inner(area);
    app.page_rows = inner.height;
    f.render_widget(block, area);

    if let Some(err) = &app.detail.error {
        f.render_widget(
            Paragraph::new(Span::styled(format!("error: {err}"), Style::new().fg(Color::Red))),
            inner,
        );
        return;
    }

    match app.tab {
        Tab::Overview => {
            draw_overview(f, app, inner);
        }
        Tab::Diff => draw_diff(f, app, inner),
        Tab::Checks => draw_checks(f, app, inner),
        Tab::Comments => draw_comments(f, app, inner),
        Tab::Claude => draw_claude(f, app, inner),
    }
}

fn loading_line(app: &App) -> Line<'static> {
    Line::from(Span::styled(
        format!("{} loading…", spinner(app)),
        Style::new().fg(Color::Yellow),
    ))
}

fn wrap(s: &str, width: usize) -> Vec<String> {
    let width = width.max(10);
    let mut out = Vec::new();
    for raw in s.lines() {
        if raw.chars().count() <= width {
            out.push(raw.to_string());
            continue;
        }
        let mut cur = String::new();
        for word in raw.split(' ') {
            let wlen = word.chars().count();
            let clen = cur.chars().count();
            if clen == 0 {
                cur = word.to_string();
            } else if clen + 1 + wlen <= width {
                cur.push(' ');
                cur.push_str(word);
            } else {
                out.push(std::mem::take(&mut cur));
                cur = word.to_string();
            }
            // hard-break single words longer than the width
            while cur.chars().count() > width {
                let head: String = cur.chars().take(width).collect();
                let tail: String = cur.chars().skip(width).collect();
                out.push(head);
                cur = tail;
            }
        }
        out.push(cur);
    }
    if out.is_empty() {
        out.push(String::new());
    }
    out
}

fn draw_overview(f: &mut Frame, app: &mut App, area: Rect) {
    let Some(d) = &app.detail.data else {
        f.render_widget(Paragraph::new(loading_line(app)), area);
        return;
    };
    let w = area.width as usize;
    let mut lines: Vec<Line> = Vec::new();
    for l in wrap(&d.title, w) {
        lines.push(Line::from(Span::styled(l, Style::new().add_modifier(Modifier::BOLD))));
    }
    lines.push(Line::from(Span::styled(d.url.clone(), Style::new().fg(Color::Blue))));
    lines.push(Line::default());
    let state = if d.is_draft { "DRAFT".to_string() } else { d.state.clone() };
    lines.push(Line::from(vec![
        Span::styled(state, Style::new().fg(Color::Yellow)),
        Span::raw("  "),
        Span::styled(format!("{} → {}", d.head_ref, d.base_ref), Style::new().fg(Color::Cyan)),
        Span::raw("  by "),
        Span::styled(d.author.clone(), Style::new().fg(Color::Magenta)),
    ]));
    lines.push(Line::from(vec![
        Span::styled(format!("+{}", d.additions), Style::new().fg(Color::Green)),
        Span::raw(" "),
        Span::styled(format!("−{}", d.deletions), Style::new().fg(Color::Red)),
        Span::raw(format!("  {} files", d.changed_files)),
    ]));
    let (pass, fail, pending) = check_counts(&d.checks);
    lines.push(Line::from(vec![
        Span::raw("checks: "),
        Span::styled(format!("{pass} ✓"), Style::new().fg(Color::Green)),
        Span::raw("  "),
        Span::styled(format!("{fail} ✗"), Style::new().fg(Color::Red)),
        Span::raw("  "),
        Span::styled(format!("{pending} ●"), Style::new().fg(Color::Yellow)),
        Span::raw(format!(
            "   review: {}   mergeable: {} ({})",
            pretty_enum(&d.review_decision),
            pretty_enum(&d.mergeable),
            pretty_enum(&d.merge_state),
        )),
    ]));
    let open_threads = d.threads.iter().filter(|t| !t.resolved).count();
    lines.push(Line::from(Span::raw(format!(
        "threads: {} open / {} total   comments: {}   reviews: {}",
        open_threads,
        d.threads.len(),
        d.comments.len(),
        d.reviews.len()
    ))));
    lines.push(Line::default());
    lines.push(Line::from(Span::styled(
        "─".repeat(w.min(80)),
        Style::new().fg(Color::DarkGray),
    )));
    let body = md::clean_comment("", &d.body);
    if body.trim().is_empty() {
        lines.push(Line::from(Span::styled("(no description)", Style::new().fg(Color::DarkGray))));
    } else {
        lines.extend(md::render(&body, w));
    }
    let scroll = clamp_scroll(app.overview_scroll, lines.len(), area.height);
    app.overview_scroll = scroll;
    f.render_widget(Paragraph::new(Text::from(lines)).scroll((scroll, 0)), area);
}

fn pretty_enum(s: &str) -> String {
    if s.is_empty() {
        "—".into()
    } else {
        s.to_lowercase().replace('_', " ")
    }
}

fn check_counts(checks: &[Check]) -> (usize, usize, usize) {
    let mut pass = 0;
    let mut fail = 0;
    let mut pending = 0;
    for c in checks {
        match c.conclusion.as_str() {
            "SUCCESS" | "NEUTRAL" | "SKIPPED" => pass += 1,
            "FAILURE" | "ERROR" | "TIMED_OUT" | "CANCELLED" | "STARTUP_FAILURE" => fail += 1,
            _ => pending += 1,
        }
    }
    (pass, fail, pending)
}

fn draw_diff(f: &mut Frame, app: &App, area: Rect) {
    let Some(diff) = &app.detail.diff else {
        f.render_widget(Paragraph::new(loading_line(app)), area);
        return;
    };
    let height = area.height as usize;
    let total = diff.lines.len();
    let sel = app.diff_sel.min(total.saturating_sub(1));
    let cursor_active = app.focus == Focus::Detail && app.tab == Tab::Diff;
    let scroll = if sel + 1 > height { sel + 1 - height } else { 0 };
    // rows with a comment queued for the pending review
    let queued: std::collections::HashSet<(String, u64, &'static str)> = app
        .detail
        .key
        .as_ref()
        .and_then(|k| app.pending.get(k))
        .map(|v| {
            v.iter()
                .map(|p| (p.path.clone(), p.line, if p.side == "LEFT" { "LEFT" } else { "RIGHT" }))
                .collect()
        })
        .unwrap_or_default();
    const ADDED_BG: Color = Color::Rgb(24, 46, 28);
    const REMOVED_BG: Color = Color::Rgb(52, 27, 30);
    const SEL_BG: Color = Color::Rgb(45, 50, 62);
    let eink = app.eink;
    let mut lines: Vec<Line> = Vec::with_capacity(height);
    for (i, raw) in diff.lines.iter().enumerate().skip(scroll).take(height) {
        let selected = cursor_active && i == sel;
        let target = diff.targets.get(i).and_then(|t| t.as_ref());
        let has_queued = target
            .map(|t| queued.contains(&(t.path.clone(), t.line, t.side)))
            .unwrap_or(false);

        // header / hunk lines keep their single-style rendering
        let header_style = if raw.starts_with("diff --git") {
            Some(Style::new().fg(Color::White).bg(Color::Rgb(50, 55, 65)).add_modifier(Modifier::BOLD))
        } else if raw.starts_with("+++") || raw.starts_with("---") {
            Some(Style::new().fg(Color::White).add_modifier(Modifier::BOLD))
        } else if raw.starts_with("@@") {
            Some(if eink {
                Style::new().add_modifier(Modifier::UNDERLINED)
            } else {
                Style::new().fg(Color::Cyan)
            })
        } else {
            None
        };

        let mut spans: Vec<Span> = Vec::new();
        // gutter
        if selected {
            spans.push(Span::styled("▶", Style::new().fg(Color::Cyan).bg(SEL_BG)));
        } else if has_queued {
            spans.push(Span::styled("●", Style::new().fg(Color::Yellow)));
        } else {
            spans.push(Span::raw(" "));
        }

        let (line_bg, weight): (Option<Color>, Modifier) = if raw.starts_with('+') {
            if eink {
                (None, Modifier::BOLD)
            } else {
                (Some(ADDED_BG), Modifier::empty())
            }
        } else if raw.starts_with('-') {
            if eink {
                (None, Modifier::CROSSED_OUT)
            } else {
                (Some(REMOVED_BG), Modifier::empty())
            }
        } else {
            (None, Modifier::empty())
        };
        let bg = if selected { Some(SEL_BG) } else { line_bg };
        let apply = |mut s: Style| {
            if let Some(b) = bg {
                s = s.bg(b);
            }
            s.add_modifier(weight)
        };

        if let Some(hs) = header_style {
            spans.push(Span::styled(raw.clone(), if selected { hs.bg(SEL_BG) } else { hs }));
        } else if let (Some(t), false) = (target, eink) {
            // syntax-highlighted code line: marker char, then tokens
            let (marker, rest) = raw.split_at(raw.len().min(1));
            let marker_style = apply(if raw.starts_with('+') {
                Style::new().fg(Color::Green)
            } else if raw.starts_with('-') {
                Style::new().fg(Color::Red)
            } else {
                Style::new().fg(Color::DarkGray)
            });
            spans.push(Span::styled(marker.to_string(), marker_style));
            match syntax::detect(&t.path) {
                Some(def) => {
                    for (text, kind) in syntax::highlight(rest, def) {
                        spans.push(Span::styled(text, apply(syntax::style(kind))));
                    }
                }
                None => spans.push(Span::styled(rest.to_string(), apply(Style::new()))),
            }
        } else {
            // e-ink or unmapped line: weight instead of color
            spans.push(Span::styled(raw.clone(), apply(Style::new())));
        }

        // pad tinted/selected rows so the background spans the full width
        if bg.is_some() {
            let used = 1 + raw.chars().count();
            let pad = (area.width as usize).saturating_sub(used);
            spans.push(Span::styled(" ".repeat(pad), apply(Style::new())));
        }
        lines.push(Line::from(spans));
    }
    if total == 0 {
        lines.push(Line::from(Span::styled("(empty diff)", Style::new().fg(Color::DarkGray))));
    }
    f.render_widget(Paragraph::new(Text::from(lines)), area);
}

fn draw_checks(f: &mut Frame, app: &App, area: Rect) {
    let Some(d) = &app.detail.data else {
        f.render_widget(Paragraph::new(loading_line(app)), area);
        return;
    };
    let mut lines: Vec<Line> = Vec::new();
    if d.checks.is_empty() {
        lines.push(Line::from(Span::styled(
            "no checks reported on head commit",
            Style::new().fg(Color::DarkGray),
        )));
    }
    for (i, c) in d.checks.iter().enumerate() {
        let (glyph, color) = match c.conclusion.as_str() {
            "SUCCESS" => ("✓", Color::Green),
            "NEUTRAL" | "SKIPPED" => ("−", Color::DarkGray),
            "FAILURE" | "ERROR" | "TIMED_OUT" | "STARTUP_FAILURE" => ("✗", Color::Red),
            "CANCELLED" => ("⊘", Color::Red),
            _ => ("●", Color::Yellow),
        };
        let status = if c.status == "COMPLETED" || c.status.is_empty() {
            pretty_enum(&c.conclusion)
        } else {
            pretty_enum(&c.status)
        };
        let sel = i == app.checks_sel && app.tab == Tab::Checks;
        let mut spans = vec![
            Span::raw(if sel { "▶ " } else { "  " }),
            Span::styled(format!("{glyph} "), Style::new().fg(color)),
            Span::styled(
                c.name.clone(),
                if sel { Style::new().add_modifier(Modifier::BOLD) } else { Style::new() },
            ),
            Span::styled(format!("  {status}"), Style::new().fg(color)),
        ];
        if sel && !c.url.is_empty() {
            spans.push(Span::styled("  (enter/o: open)", Style::new().fg(Color::DarkGray)));
        }
        lines.push(Line::from(spans));
    }
    let scroll = if app.checks_sel + 1 > area.height as usize {
        (app.checks_sel + 1 - area.height as usize) as u16
    } else {
        0
    };
    f.render_widget(Paragraph::new(Text::from(lines)).scroll((scroll, 0)), area);
}

fn author_spans(name: &str, pr_author: &str) -> Vec<Span<'static>> {
    let mut spans = vec![Span::styled(
        name.to_string(),
        Style::new().fg(Color::Magenta).add_modifier(Modifier::BOLD),
    )];
    if name == pr_author {
        spans.push(Span::styled(" (author)", Style::new().fg(Color::DarkGray)));
    }
    spans
}

fn reactions_line(reactions: &[(String, i64)]) -> Option<Line<'static>> {
    if reactions.is_empty() {
        return None;
    }
    let mut spans: Vec<Span> = Vec::new();
    for (emoji, n) in reactions {
        spans.push(Span::raw(format!("{emoji} ")));
        spans.push(Span::styled(format!("{n}  "), Style::new().fg(Color::DarkGray)));
    }
    Some(Line::from(spans))
}

/// Tail of the review thread's diff hunk, diff-colored but dimmed.
fn hunk_lines(hunk: &str, w: usize, max: usize) -> Vec<Line<'static>> {
    let all: Vec<&str> = hunk.lines().collect();
    let tail = &all[all.len().saturating_sub(max)..];
    let border = Style::new().fg(Color::Rgb(70, 75, 85));
    tail.iter()
        .map(|raw| {
            let fg = if raw.starts_with('+') {
                Color::Rgb(90, 140, 90)
            } else if raw.starts_with('-') {
                Color::Rgb(150, 90, 90)
            } else if raw.starts_with("@@") {
                Color::Rgb(80, 120, 130)
            } else {
                Color::Rgb(120, 125, 135)
            };
            let mut text: String = raw.chars().take(w.saturating_sub(2)).collect();
            if text.len() < raw.len() {
                text.push('…');
            }
            Line::from(vec![Span::styled("┆ ", border), Span::styled(text, Style::new().fg(fg))])
        })
        .collect()
}

/// Prefix every line of a block with a colored gutter bar.
fn gutter(lines: Vec<Line<'static>>, selected: bool) -> Vec<Line<'static>> {
    let style = if selected {
        Style::new().fg(Color::Cyan)
    } else {
        Style::new().fg(Color::Rgb(60, 64, 72))
    };
    lines
        .into_iter()
        .map(|mut l| {
            l.spans.insert(0, Span::styled("▎ ", style));
            l
        })
        .collect()
}

fn comment_card(c: &Comment, pr_author: &str, label: &str, w: usize) -> Vec<Line<'static>> {
    let mut out: Vec<Line> = Vec::new();
    let mut head = author_spans(&c.author, pr_author);
    head.push(Span::styled(
        format!(" {label} · {}", rel_time(&c.created_at)),
        Style::new().fg(Color::DarkGray),
    ));
    out.push(Line::from(head));
    let body = md::clean_comment(&c.author, &c.body);
    out.extend(md::render(&body, w.saturating_sub(4)));
    out.extend(reactions_line(&c.reactions));
    out
}

fn draw_comments(f: &mut Frame, app: &App, area: Rect) {
    let Some(d) = &app.detail.data else {
        f.render_widget(Paragraph::new(loading_line(app)), area);
        return;
    };
    let w = area.width as usize;
    let mut lines: Vec<Line> = Vec::new();
    let mut sel_start = 0usize;
    let mut sel_end = 0usize;

    if app.detail.blocks.is_empty() {
        lines.push(Line::from(Span::styled(
            "no comments or review threads",
            Style::new().fg(Color::DarkGray),
        )));
    }

    for (bi, block) in app.detail.blocks.iter().enumerate() {
        let selected = bi == app.block_sel && app.focus == Focus::Detail && app.tab == Tab::Comments;
        if selected {
            sel_start = lines.len();
        }
        let mut card: Vec<Line> = Vec::new();
        match block {
            CBlock::Comment(i) => {
                if let Some(c) = d.comments.get(*i) {
                    card = comment_card(c, &d.author, "commented", w);
                }
            }
            CBlock::Review(i) => {
                if let Some(r) = d.reviews.get(*i) {
                    let (label, color) = match r.state.as_str() {
                        "APPROVED" => ("approved ✓", Color::Green),
                        "CHANGES_REQUESTED" => ("requested changes ±", Color::Red),
                        "DISMISSED" => ("review dismissed", Color::DarkGray),
                        _ => ("reviewed", Color::Yellow),
                    };
                    let mut head = author_spans(&r.author, &d.author);
                    head.push(Span::raw(" "));
                    head.push(Span::styled(label, Style::new().fg(color).add_modifier(Modifier::BOLD)));
                    head.push(Span::styled(
                        format!(" · {}", rel_time(&r.created_at)),
                        Style::new().fg(Color::DarkGray),
                    ));
                    card.push(Line::from(head));
                    let body = md::clean_comment(&r.author, &r.body);
                    if !body.trim().is_empty() {
                        card.extend(md::render(&body, w.saturating_sub(4)));
                    }
                }
            }
            CBlock::Thread(i) => {
                if let Some(t) = d.threads.get(*i) {
                    let folded = app.thread_folded(*i);
                    let (state, color) = if t.resolved {
                        ("resolved ✓", Color::Green)
                    } else if t.outdated {
                        ("outdated", Color::DarkGray)
                    } else {
                        ("open ●", Color::Yellow)
                    };
                    let loc = match t.line {
                        Some(l) => format!("{}:{}", t.path, l),
                        None => t.path.clone(),
                    };
                    let arrow = if folded { "▸ " } else { "▾ " };
                    let mut head = vec![
                        Span::styled(arrow, Style::new().fg(color)),
                        Span::styled(loc, Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
                        Span::raw("  "),
                        Span::styled(state, Style::new().fg(color)),
                        Span::styled(
                            format!(
                                " · {} comment{}",
                                t.comments.len(),
                                if t.comments.len() == 1 { "" } else { "s" }
                            ),
                            Style::new().fg(Color::DarkGray),
                        ),
                    ];
                    if selected {
                        head.push(Span::styled(
                            if folded { "  z expand · x resolve" } else { "  z fold · x resolve · c reply" },
                            Style::new().fg(Color::DarkGray),
                        ));
                    }
                    card.push(Line::from(head));
                    if !folded {
                        if !t.hunk.is_empty() {
                            card.extend(hunk_lines(&t.hunk, w.saturating_sub(2), 4));
                        }
                        for (ci, c) in t.comments.iter().enumerate() {
                            let label = if ci == 0 { "" } else { "replied" };
                            let mut sub = comment_card(c, &d.author, label, w.saturating_sub(2));
                            if ci > 0 {
                                sub = md::indent(sub, "  ");
                            }
                            card.extend(sub);
                        }
                    }
                }
            }
        }
        lines.extend(gutter(card, selected));
        if selected {
            sel_end = lines.len();
        }
        lines.push(Line::default());
    }

    // keep the selected block in view
    let height = area.height as usize;
    let mut scroll = 0usize;
    if sel_end > height {
        scroll = sel_start.min(lines.len().saturating_sub(height));
        if sel_end - sel_start < height {
            scroll = scroll.min(sel_end.saturating_sub(height).max(sel_start));
            scroll = sel_start.min(scroll.max(sel_end.saturating_sub(height)));
        }
    }
    f.render_widget(
        Paragraph::new(Text::from(lines)).scroll((scroll as u16, 0)),
        area,
    );
}

fn draw_claude(f: &mut Frame, app: &mut App, area: Rect) {
    let w = area.width as usize;
    let mut lines: Vec<Line> = Vec::new();
    match (&app.claude.key, app.claude.text.is_empty(), app.claude.running) {
        (None, _, _) => {
            lines.push(Line::from(Span::styled(
                "R: run a claude code review on the selected PR's diff",
                Style::new().fg(Color::DarkGray),
            )));
            lines.push(Line::from(Span::styled(
                "P: post the finished review as a PR comment",
                Style::new().fg(Color::DarkGray),
            )));
        }
        (Some((repo, n)), true, true) => {
            lines.push(Line::from(Span::styled(
                format!("{} claude is reviewing {}#{}…", spinner(app), short_repo(repo), n),
                Style::new().fg(Color::Yellow),
            )));
        }
        (Some((repo, n)), _, running) => {
            let head = if running {
                format!("{} reviewing {}#{}", spinner(app), short_repo(repo), n)
            } else {
                format!("review of {}#{} — P to post as comment", short_repo(repo), n)
            };
            lines.push(Line::from(Span::styled(head, Style::new().fg(Color::Yellow))));
            lines.push(Line::default());
            lines.extend(md::render(&app.claude.text, w));
        }
    }
    let total = lines.len();
    if app.claude.follow {
        app.claude_scroll = total.saturating_sub(area.height as usize) as u16;
    }
    let scroll = clamp_scroll(app.claude_scroll, total, area.height);
    app.claude_scroll = scroll;
    f.render_widget(Paragraph::new(Text::from(lines)).scroll((scroll, 0)), area);
}

fn clamp_scroll(scroll: u16, total: usize, height: u16) -> u16 {
    let max = total.saturating_sub(height as usize) as u16;
    scroll.min(max)
}

// ---- e-ink mode ----

enum EinkClass {
    Plain,
    Dim,
    Strong,
    Mark,
}

/// Map a color to its monochrome intent: attention → bold, secondary → dim,
/// highlights/links → underline.
fn eink_classify(fg: Color) -> EinkClass {
    match fg {
        Color::DarkGray | Color::Gray => EinkClass::Dim,
        Color::Red | Color::LightRed => EinkClass::Strong,
        Color::Yellow | Color::LightYellow => EinkClass::Mark,
        Color::Blue | Color::LightBlue => EinkClass::Mark,
        Color::Rgb(r, g, b) => {
            let (max, min) = (r.max(g).max(b), r.min(g).min(b));
            if max - min < 30 {
                if max < 170 { EinkClass::Dim } else { EinkClass::Plain }
            } else if r > g.saturating_add(40) && r > b.saturating_add(40) {
                EinkClass::Strong
            } else if b > r.saturating_add(30) && b > g.saturating_add(20) {
                EinkClass::Mark
            } else if r > 140 && g > 110 && b < 90 {
                EinkClass::Mark
            } else {
                EinkClass::Plain
            }
        }
        _ => EinkClass::Plain,
    }
}

/// Post-process a rendered frame into pure monochrome: every color becomes a
/// font treatment (bold/dim/underline/reverse), so the UI reads on grayscale
/// e-ink panels.
pub fn eink_remap(buf: &mut ratatui::buffer::Buffer) {
    for cell in buf.content.iter_mut() {
        let mut m = cell.modifier;
        if cell.bg != Color::Reset {
            m.insert(Modifier::REVERSED);
        }
        match eink_classify(cell.fg) {
            EinkClass::Dim => m.insert(Modifier::DIM),
            EinkClass::Strong => m.insert(Modifier::BOLD),
            EinkClass::Mark => m.insert(Modifier::UNDERLINED),
            EinkClass::Plain => {}
        }
        cell.modifier = m;
        cell.fg = Color::Reset;
        cell.bg = Color::Reset;
        cell.underline_color = Color::Reset;
    }
}

// ---- linear view ----

/// "#f2c94c" -> Color::Rgb
fn hex_color(s: &str) -> Color {
    let h = s.trim_start_matches('#');
    if h.len() == 6 {
        if let (Ok(r), Ok(g), Ok(b)) = (
            u8::from_str_radix(&h[0..2], 16),
            u8::from_str_radix(&h[2..4], 16),
            u8::from_str_radix(&h[4..6], 16),
        ) {
            return Color::Rgb(r, g, b);
        }
    }
    Color::DarkGray
}

fn priority_glyph(p: i64) -> Span<'static> {
    match p {
        1 => Span::styled("‼ ", Style::new().fg(Color::Red).add_modifier(Modifier::BOLD)),
        2 => Span::styled("▲ ", Style::new().fg(Color::Rgb(255, 150, 60))),
        3 => Span::styled("■ ", Style::new().fg(Color::Yellow)),
        4 => Span::styled("▽ ", Style::new().fg(Color::DarkGray)),
        _ => Span::styled("· ", Style::new().fg(Color::DarkGray)),
    }
}

fn estimate_str(e: Option<f64>) -> String {
    match e {
        Some(v) if v.fract() == 0.0 => format!("{}pt", v as i64),
        Some(v) => format!("{v}pt"),
        None => String::new(),
    }
}

fn due_span(due: &str) -> Option<Span<'static>> {
    if due.is_empty() {
        return None;
    }
    let today = chrono::Utc::now().date_naive().to_string();
    let style = if due < &today[..] {
        Style::new().fg(Color::Red).add_modifier(Modifier::BOLD)
    } else if due == today {
        Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD)
    } else {
        Style::new().fg(Color::DarkGray)
    };
    Some(Span::styled(format!("due {due}"), style))
}

fn draw_linear_list(f: &mut Frame, app: &App, area: Rect) {
    let vis = app.linear_visible();
    let mut title_spans = vec![
        Span::styled(" arrano ", Style::new().add_modifier(Modifier::BOLD)),
        Span::styled(
            format!("◈ linear ({}) ", app.linear.issues.len()),
            Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        ),
        Span::styled("[1-3] PRs ", Style::new().fg(Color::DarkGray)),
    ];
    if !app.effective_filter().is_empty() {
        title_spans.push(Span::styled(
            format!("/{} ", app.effective_filter()),
            Style::new().fg(Color::Yellow),
        ));
    }
    if app.linear.loading {
        title_spans.push(Span::styled(spinner(app), Style::new().fg(Color::Yellow)));
    }
    let mut block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style(app.focus == Focus::List))
        .title(Line::from(title_spans));
    if let Some(at) = app.linear.fetched_at {
        let secs = at.elapsed().as_secs();
        let age = if secs < 60 { format!(" synced {secs}s ago ") } else { format!(" synced {}m ago ", secs / 60) };
        block = block.title_bottom(Line::from(Span::styled(age, Style::new().fg(Color::DarkGray))));
    }
    let inner = block.inner(area);
    f.render_widget(block, area);

    let mut lines: Vec<Line> = Vec::new();
    let mut sel_line = 0usize;
    if let Some(err) = &app.linear.error {
        lines.push(Line::from(Span::styled(format!("error: {err}"), Style::new().fg(Color::Red))));
    } else if app.linear.issues.is_empty() {
        let msg = if app.linear.loading { "loading…" } else { "no assigned issues" };
        lines.push(Line::from(Span::styled(msg, Style::new().fg(Color::DarkGray))));
    } else if vis.is_empty() {
        lines.push(Line::from(Span::styled("no matches — esc clears", Style::new().fg(Color::DarkGray))));
    } else {
        let mut last_state = "";
        for (pos, &idx) in vis.iter().enumerate() {
            let issue = &app.linear.issues[idx];
            if issue.state != last_state {
                last_state = &issue.state;
                let color = hex_color(&issue.state_color);
                let count = vis.iter().filter(|&&i| app.linear.issues[i].state == issue.state).count();
                lines.push(Line::from(vec![
                    Span::styled(" ● ", Style::new().fg(color)),
                    Span::styled(
                        issue.state.clone(),
                        Style::new().fg(color).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(format!(" ({count})"), Style::new().fg(Color::DarkGray)),
                ]));
            }
            if pos == app.linear.sel {
                sel_line = lines.len();
            }
            lines.push(issue_row(issue, pos == app.linear.sel, inner.width));
        }
    }
    let height = inner.height as usize;
    let scroll = if sel_line >= height { (sel_line + 1 - height) as u16 } else { 0 };
    f.render_widget(Paragraph::new(Text::from(lines)).scroll((scroll, 0)), inner);
}

fn issue_row(issue: &LinearIssue, selected: bool, width: u16) -> Line<'static> {
    let mut spans: Vec<Span> = Vec::new();
    spans.push(Span::raw(if selected { "▶ " } else { "  " }));
    spans.push(priority_glyph(issue.priority));
    spans.push(Span::styled(
        format!("{:<9}", issue.identifier),
        Style::new().fg(Color::Yellow),
    ));
    let est = estimate_str(issue.estimate);
    let meta = if est.is_empty() {
        format!(" {}", rel_time(&issue.updated_at))
    } else {
        format!(" {est} {}", rel_time(&issue.updated_at))
    };
    let used: usize =
        spans.iter().map(|s| s.content.chars().count()).sum::<usize>() + meta.chars().count();
    let avail = (width as usize).saturating_sub(used + 1);
    let mut title: String = issue.title.chars().take(avail).collect();
    if title.chars().count() < issue.title.chars().count() && !title.is_empty() {
        title.pop();
        title.push('…');
    }
    let pad = avail.saturating_sub(title.chars().count());
    let title_style =
        if selected { Style::new().add_modifier(Modifier::BOLD) } else { Style::new() };
    spans.push(Span::styled(title, title_style));
    spans.push(Span::raw(" ".repeat(pad)));
    spans.push(Span::styled(meta, Style::new().fg(Color::DarkGray)));
    let mut line = Line::from(spans);
    if selected {
        line = line.style(Style::new().bg(Color::Rgb(40, 44, 52)));
    }
    line
}

fn draw_linear_detail(f: &mut Frame, app: &mut App, area: Rect) {
    let title = match app.selected_issue() {
        Some(i) => format!(" {} ", i.identifier),
        None => " no issue selected ".into(),
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style(app.focus == Focus::Detail))
        .title(Line::from(Span::styled(" issue ", Style::new().fg(Color::Black).bg(Color::Cyan).add_modifier(Modifier::BOLD))))
        .title_bottom(Line::from(Span::styled(title, Style::new().fg(Color::DarkGray))));
    let inner = block.inner(area);
    app.page_rows = inner.height;
    f.render_widget(block, area);

    let Some(issue) = app.selected_issue() else {
        let msg = if app.linear.loading || app.linear.jump.is_some() {
            loading_line(app)
        } else {
            Line::from(Span::styled("nothing selected", Style::new().fg(Color::DarkGray)))
        };
        f.render_widget(Paragraph::new(msg), inner);
        return;
    };
    let w = inner.width as usize;
    let mut lines: Vec<Line> = Vec::new();

    // header: identifier + state pill + priority + estimate + cycle + due
    let state_color = hex_color(&issue.state_color);
    let mut head: Vec<Span> = vec![
        Span::styled(
            issue.identifier.clone(),
            Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(
            format!(" {} ", issue.state),
            Style::new().fg(Color::Black).bg(state_color).add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        priority_glyph(issue.priority),
        Span::styled(issue.priority_label.clone(), Style::new().fg(Color::DarkGray)),
    ];
    let est = estimate_str(issue.estimate);
    if !est.is_empty() {
        head.push(Span::styled(format!("  {est}"), Style::new().fg(Color::Cyan)));
    }
    if !issue.cycle.is_empty() {
        head.push(Span::styled(format!("  cycle {}", issue.cycle), Style::new().fg(Color::DarkGray)));
    }
    if let Some(d) = due_span(&issue.due_date) {
        head.push(Span::raw("  "));
        head.push(d);
    }
    lines.push(Line::from(head));
    lines.push(Line::default());

    for l in wrap(&issue.title, w) {
        lines.push(Line::from(Span::styled(l, Style::new().add_modifier(Modifier::BOLD))));
    }
    lines.push(Line::default());

    let mut meta: Vec<Span> = Vec::new();
    if !issue.assignee.is_empty() {
        meta.push(Span::styled(issue.assignee.clone(), Style::new().fg(Color::Magenta)));
    }
    if !issue.project.is_empty() {
        meta.push(Span::styled(format!("  ◆ {}", issue.project), Style::new().fg(Color::Cyan)));
    }
    for (name, color) in &issue.labels {
        meta.push(Span::styled("  ● ", Style::new().fg(hex_color(color))));
        meta.push(Span::styled(name.clone(), Style::new().fg(hex_color(color))));
    }
    meta.push(Span::styled(
        format!("  · updated {}", rel_time(&issue.updated_at)),
        Style::new().fg(Color::DarkGray),
    ));
    lines.push(Line::from(meta));
    lines.push(Line::from(Span::styled("─".repeat(w.min(80)), Style::new().fg(Color::DarkGray))));

    let desc = md::clean_comment("", &issue.description);
    if desc.trim().is_empty() {
        lines.push(Line::from(Span::styled("(no description)", Style::new().fg(Color::DarkGray))));
    } else {
        lines.extend(md::render(&desc, w));
    }

    if !issue.comments.is_empty() {
        lines.push(Line::default());
        lines.push(Line::from(Span::styled(
            format!("── comments ({}) ", issue.comments.len()),
            Style::new().fg(Color::DarkGray).add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::default());
        for c in &issue.comments {
            let card = comment_card(c, "", "", w.saturating_sub(2));
            lines.extend(gutter(card, false));
            lines.push(Line::default());
        }
    }

    let scroll = clamp_scroll(app.linear_scroll, lines.len(), inner.height);
    app.linear_scroll = scroll;
    f.render_widget(Paragraph::new(Text::from(lines)).scroll((scroll, 0)), inner);
}

// ---- status bar, modals ----

fn draw_status(f: &mut Frame, app: &App, area: Rect) {
    if let Some(Input::Filter { buf }) = &app.input {
        let n = app.visible().len();
        let spans = vec![
            Span::styled(" /", Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled(buf.clone(), Style::new().add_modifier(Modifier::BOLD)),
            Span::styled("█", Style::new().fg(Color::Yellow)),
            Span::styled(
                format!("  {n} local · enter: apply (or search GitHub) · esc: clear"),
                Style::new().fg(Color::DarkGray),
            ),
        ];
        f.render_widget(Paragraph::new(Line::from(spans)), area);
        return;
    }
    if let Some((msg, _, is_err)) = app.toasts.last() {
        let style = if *is_err {
            Style::new().fg(Color::White).bg(Color::Red)
        } else {
            Style::new().fg(Color::Black).bg(Color::Green)
        };
        f.render_widget(Paragraph::new(Span::styled(format!(" {msg} "), style)), area);
        return;
    }
    let keys: &[(&str, &str)] = if app.view == View::Linear {
        match app.focus {
            Focus::List => &[
                ("j/k", "move"),
                ("enter", "detail"),
                ("o", "open"),
                ("/", "filter"),
                ("r", "refresh"),
                ("1/2/3", "PRs"),
                ("esc", "back"),
                ("?", "help"),
            ],
            Focus::Detail => &[
                ("j/k", "scroll"),
                ("ctrl-d/u", "page"),
                ("o", "open"),
                ("esc", "list"),
                ("?", "help"),
            ],
        }
    } else {
        match (app.focus, app.tab) {
        (Focus::List, _) => &[
            ("j/k", "move"),
            ("enter", "open"),
            ("/", "filter"),
            ("1/2/3", "lane"),
            ("c", "comment"),
            ("v", "review"),
            ("m", "merge"),
            ("u", "update"),
            ("R", "claude"),
            ("?", "help"),
        ],
        (Focus::Detail, Tab::Diff) => &[
            ("h/l", "tab"),
            ("j/k", "line"),
            ("c", "comment line"),
            ("v", "review"),
            ("esc", "list"),
        ],
        (Focus::Detail, Tab::Comments) => &[
            ("h/l", "tab"),
            ("j/k", "block"),
            ("c", "reply"),
            ("x", "resolve"),
            ("z", "fold"),
            ("f", "links"),
            ("o", "open"),
            ("esc", "list"),
        ],
        (Focus::Detail, Tab::Checks) => &[
            ("h/l", "tab"),
            ("j/k", "check"),
            ("o", "open run"),
            ("esc", "list"),
            ("?", "help"),
        ],
        (Focus::Detail, Tab::Claude) => &[
            ("h/l", "tab"),
            ("j/k", "scroll"),
            ("G", "follow"),
            ("P", "post"),
            ("esc", "list"),
        ],
        (Focus::Detail, _) => &[
            ("h/l", "tab"),
            ("j/k", "scroll"),
            ("c", "comment"),
            ("v", "review"),
            ("m", "merge"),
            ("u", "update"),
            ("R", "claude"),
            ("esc", "list"),
        ],
        }
    };
    let mut spans: Vec<Span> = Vec::new();
    if app.busy() {
        spans.push(Span::styled(format!(" {} ", spinner(app)), Style::new().fg(Color::Yellow)));
    }
    for (k, v) in keys {
        spans.push(Span::styled(format!(" {k} "), Style::new().fg(Color::Black).bg(Color::DarkGray)));
        spans.push(Span::styled(format!(" {v}  "), Style::new().fg(Color::DarkGray)));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn centered(width: u16, height: u16, r: Rect) -> Rect {
    let w = width.min(r.width);
    let h = height.min(r.height);
    Rect::new(r.x + (r.width - w) / 2, r.y + (r.height - h) / 2, w, h)
}

fn draw_choice_modal(f: &mut Frame, title: &str, choices: &[(&str, &str)]) {
    let h = choices.len() as u16 + 2;
    let w = (title.len() as u16 + 6).max(34);
    let area = centered(w, h, f.area());
    f.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::new().fg(Color::Cyan))
        .title(format!(" {title} "));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let lines: Vec<Line> = choices
        .iter()
        .map(|(k, v)| {
            Line::from(vec![
                Span::styled(format!("  {k} "), Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
                Span::raw(format!(" {v}")),
            ])
        })
        .collect();
    f.render_widget(Paragraph::new(Text::from(lines)), inner);
}

fn draw_composer(f: &mut Frame, app: &App, c: &Compose) {
    let fa = f.area();
    let area = centered(fa.width.saturating_sub(6).min(96), fa.height.saturating_sub(4).min(18), fa);
    f.render_widget(Clear, area);
    let border = if c.rewriting { Color::Yellow } else { Color::Cyan };
    let title = if c.rewriting {
        format!(" {} — {} humanizing with local llm… ", c.target.describe(), spinner(app))
    } else {
        format!(" {} ", c.target.describe())
    };
    let hint = if c.target.allows_empty() {
        " ctrl-s submit (empty ok) · ctrl-r humanize · ctrl-z undo · esc discard "
    } else {
        " ctrl-s post · ctrl-r humanize (local llm) · ctrl-z undo · esc discard "
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::new().fg(border))
        .title(title)
        .title_bottom(Line::from(Span::styled(hint, Style::new().fg(Color::DarkGray))));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let height = inner.height as usize;
    let top = (c.row + 1).saturating_sub(height);
    let text_style = if c.rewriting {
        Style::new().fg(Color::DarkGray)
    } else {
        Style::new()
    };
    let mut lines: Vec<Line> = Vec::new();
    for (r, line) in c.lines.iter().enumerate().skip(top).take(height) {
        if r == c.row && !c.rewriting {
            let chars: Vec<char> = line.chars().collect();
            let before: String = chars.iter().take(c.col).collect();
            let at: String = chars.get(c.col).map(|ch| ch.to_string()).unwrap_or(" ".into());
            let after: String = chars.iter().skip(c.col + 1).collect();
            lines.push(Line::from(vec![
                Span::styled(before, text_style),
                Span::styled(at, Style::new().add_modifier(Modifier::REVERSED)),
                Span::styled(after, text_style),
            ]));
        } else {
            lines.push(Line::from(Span::styled(line.clone(), text_style)));
        }
    }
    f.render_widget(Paragraph::new(Text::from(lines)), inner);
}

fn draw_links(f: &mut Frame, app: &App) {
    let fa = f.area();
    let w = fa.width.saturating_sub(8).min(90);
    let h = (app.links.len() as u16 + 2).min(fa.height.saturating_sub(4));
    let area = centered(w, h, fa);
    f.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::new().fg(Color::Cyan))
        .title(" links ")
        .title_bottom(Line::from(Span::styled(
            " enter/1-9 open · j/k move · esc close ",
            Style::new().fg(Color::DarkGray),
        )));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let iw = inner.width as usize;
    let mut lines: Vec<Line> = Vec::new();
    for (i, (label, url)) in app.links.iter().enumerate() {
        let sel = i == app.links_sel;
        let num = if i < 9 { format!("{} ", i + 1) } else { "  ".into() };
        let mut label = label.trim().replace('\n', " ");
        if label.chars().count() > 40 {
            label = label.chars().take(39).collect::<String>() + "…";
        }
        let used = 2 + 2 + label.chars().count() + 2;
        let mut short_url = url.clone();
        let avail = iw.saturating_sub(used);
        if short_url.chars().count() > avail {
            short_url = short_url.chars().take(avail.saturating_sub(1)).collect::<String>() + "…";
        }
        let mut spans = vec![
            Span::raw(if sel { "▶ " } else { "  " }),
            Span::styled(num, Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
            Span::styled(
                label,
                if sel {
                    Style::new().fg(Color::Rgb(97, 175, 239)).add_modifier(Modifier::BOLD)
                } else {
                    Style::new().fg(Color::Rgb(97, 175, 239))
                },
            ),
            Span::raw("  "),
            Span::styled(short_url, Style::new().fg(Color::DarkGray)),
        ];
        if sel {
            spans = spans
                .into_iter()
                .map(|s| {
                    let style = s.style.bg(Color::Rgb(40, 44, 52));
                    Span::styled(s.content, style)
                })
                .collect();
        }
        lines.push(Line::from(spans));
    }
    let scroll = if app.links_sel as u16 + 1 > inner.height {
        app.links_sel as u16 + 1 - inner.height
    } else {
        0
    };
    f.render_widget(Paragraph::new(Text::from(lines)).scroll((scroll, 0)), inner);
}

fn draw_help(f: &mut Frame) {
    let entries: &[(&str, &str)] = &[
        ("1 / 2 / 3", "lanes: my PRs / review requested / search"),
        ("/", "filter list; qualifiers or no local hits → GitHub search"),
        ("j / k, g / G", "move / scroll, top / bottom"),
        ("enter", "open PR detail"),
        ("tab", "toggle list ↔ detail focus"),
        ("h / l", "switch detail tab"),
        ("o", "open in browser (PR, or check run in checks tab)"),
        ("f", "link picker — open any link from the current comment/body"),
        ("c", "comment (PR · queue on diff line · reply in comments tab)"),
        ("v", "submit review (approve / request changes / comment)"),
        ("", "  — queued diff comments are attached and sent as ONE review"),
        ("w", "create/reuse a local worktree for the PR branch"),
        ("4", "linear view — your assigned issues, grouped by state"),
        ("L", "jump to the PR's Linear ticket in the linear view"),
        ("m", "merge PR (squash / merge / rebase)"),
        ("u", "update branch from base (rebase / merge)"),
        ("x", "resolve / unresolve review thread (comments tab)"),
        ("z", "fold / unfold thread — resolved ones start folded"),
        ("o (comments)", "open the selected comment/thread on GitHub"),
        ("R", "run claude code review on the diff"),
        ("P", "post claude review as PR comment"),
        ("in composer", "ctrl-s post · ctrl-r humanize (local llm) · ctrl-z undo"),
        ("ctrl-f / ctrl-b", "page forward / back (smooth scroll)"),
        ("mouse wheel", "scroll whatever pane is under the pointer"),
        ("r", "refresh everything"),
        ("q", "quit"),
    ];
    let h = entries.len() as u16 + 2;
    let area = centered(64, h, f.area());
    f.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::new().fg(Color::Cyan))
        .title(" arrano — keys ");
    let inner = block.inner(area);
    f.render_widget(block, area);
    let lines: Vec<Line> = entries
        .iter()
        .map(|(k, v)| {
            Line::from(vec![
                Span::styled(format!(" {k:<16}"), Style::new().fg(Color::Cyan)),
                Span::raw(v.to_string()),
            ])
        })
        .collect();
    f.render_widget(Paragraph::new(Text::from(lines)), inner);
}
