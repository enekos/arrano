//! Minimal markdown -> styled Lines renderer for comment bodies, PR
//! descriptions, and claude output. Handles the subset that actually shows
//! up in PR comments: fenced code blocks (incl. GitHub `suggestion` blocks),
//! inline code, **bold**, ~~strikethrough~~, [links](url), images, bare URLs,
//! headings, quotes, bullets, task lists, tables, and horizontal rules.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Seg {
    Plain,
    Code,
    Bold,
    Strike,
    Link,
}

const CODE_FG: Color = Color::Rgb(229, 192, 123);
const FENCE_FG: Color = Color::Rgb(152, 175, 130);
const SUGGESTION_FG: Color = Color::Rgb(140, 200, 140);
const LINK_FG: Color = Color::Rgb(97, 175, 239);

fn seg_style(base: Style, seg: Seg) -> Style {
    match seg {
        Seg::Plain => base,
        Seg::Code => base.fg(CODE_FG),
        Seg::Bold => base.add_modifier(Modifier::BOLD),
        Seg::Strike => base.fg(Color::DarkGray).add_modifier(Modifier::CROSSED_OUT),
        Seg::Link => base.fg(LINK_FG).add_modifier(Modifier::UNDERLINED),
    }
}

fn starts_with_at(chars: &[char], i: usize, pat: &str) -> bool {
    pat.chars().enumerate().all(|(k, pc)| chars.get(i + k) == Some(&pc))
}

/// Parse a `[text](url)` / `![alt](url)` starting at `i` (at '[' or '!').
/// Returns (display_text, chars_consumed).
fn parse_link(chars: &[char], i: usize) -> Option<(String, usize)> {
    let image = chars[i] == '!';
    let open = if image { i + 1 } else { i };
    if chars.get(open) != Some(&'[') {
        return None;
    }
    let close = (open + 1..chars.len()).find(|&k| chars[k] == ']')?;
    if chars.get(close + 1) != Some(&'(') {
        return None;
    }
    let end = (close + 2..chars.len()).find(|&k| chars[k] == ')')?;
    let text: String = chars[open + 1..close].iter().collect();
    let url: String = chars[close + 2..end].iter().collect();
    let display = if image {
        if text.is_empty() { "⧉".to_string() } else { format!("⧉ {text}") }
    } else if text.trim().is_empty() {
        url
    } else {
        text
    };
    Some((display, end + 1 - i))
}

/// Split a line into styled segments: `code`, **bold**, ~~strike~~, links.
fn parse_inline(s: &str) -> Vec<(String, Seg)> {
    let chars: Vec<char> = s.chars().collect();
    let mut segs: Vec<(String, Seg)> = Vec::new();
    let mut cur = String::new();
    let mut code = false;
    let mut bold = false;
    let mut strike = false;
    let style = |code: bool, bold: bool, strike: bool| {
        if code {
            Seg::Code
        } else if strike {
            Seg::Strike
        } else if bold {
            Seg::Bold
        } else {
            Seg::Plain
        }
    };
    let mut i = 0;
    while i < chars.len() {
        let st = style(code, bold, strike);
        if chars[i] == '`' {
            if !cur.is_empty() {
                segs.push((std::mem::take(&mut cur), st));
            }
            code = !code;
            i += 1;
        } else if !code && starts_with_at(&chars, i, "**") {
            if !cur.is_empty() {
                segs.push((std::mem::take(&mut cur), st));
            }
            bold = !bold;
            i += 2;
        } else if !code && starts_with_at(&chars, i, "~~") {
            if !cur.is_empty() {
                segs.push((std::mem::take(&mut cur), st));
            }
            strike = !strike;
            i += 2;
        } else if !code && (chars[i] == '[' || (chars[i] == '!' && chars.get(i + 1) == Some(&'['))) {
            if let Some((display, consumed)) = parse_link(&chars, i) {
                if !cur.is_empty() {
                    segs.push((std::mem::take(&mut cur), st));
                }
                segs.push((display, Seg::Link));
                i += consumed;
            } else {
                cur.push(chars[i]);
                i += 1;
            }
        } else if !code
            && (starts_with_at(&chars, i, "http://") || starts_with_at(&chars, i, "https://"))
            && (i == 0 || !chars[i - 1].is_alphanumeric())
        {
            if !cur.is_empty() {
                segs.push((std::mem::take(&mut cur), st));
            }
            let mut j = i;
            while j < chars.len() && !chars[j].is_whitespace() {
                j += 1;
            }
            // strip common trailing punctuation from prose
            let mut url: String = chars[i..j].iter().collect();
            while url.ends_with([')', '.', ',', ';', ':']) && !url.ends_with("()") {
                url.pop();
                j -= 1;
            }
            segs.push((url, Seg::Link));
            i = j;
        } else {
            cur.push(chars[i]);
            i += 1;
        }
    }
    if !cur.is_empty() {
        segs.push((cur, style(code, bold, strike)));
    }
    segs
}

fn hard_wrap(s: &str, width: usize) -> Vec<String> {
    let width = width.max(4);
    if s.chars().count() <= width {
        return vec![s.to_string()];
    }
    let chars: Vec<char> = s.chars().collect();
    chars.chunks(width).map(|c| c.iter().collect()).collect()
}

/// Word-wrap inline segments into Lines, with a styled first-line prefix and
/// a hanging indent for continuation lines.
fn wrap_segs(
    segs: Vec<(String, Seg)>,
    width: usize,
    first_prefix: Vec<Span<'static>>,
    cont_indent: usize,
    base: Style,
) -> Vec<Line<'static>> {
    let width = width.max(cont_indent + 8);
    let prefix_w: usize = first_prefix.iter().map(|s| s.content.chars().count()).sum();
    let mut lines: Vec<Line> = Vec::new();
    let mut spans: Vec<Span> = first_prefix;
    let mut used = prefix_w;
    let mut line_has_word = false;

    let mut flush = |spans: &mut Vec<Span<'static>>, used: &mut usize, has: &mut bool| {
        lines.push(Line::from(std::mem::take(spans)));
        spans.push(Span::raw(" ".repeat(cont_indent)));
        *used = cont_indent;
        *has = false;
    };

    for (text, seg) in segs {
        let style = seg_style(base, seg);
        for word in text.split(' ').filter(|w| !w.is_empty()) {
            for chunk in hard_wrap(word, width.saturating_sub(cont_indent + 1)) {
                let wl = chunk.chars().count();
                let sep = usize::from(line_has_word);
                if line_has_word && used + sep + wl > width {
                    flush(&mut spans, &mut used, &mut line_has_word);
                }
                if line_has_word {
                    spans.push(Span::raw(" "));
                    used += 1;
                }
                used += wl;
                spans.push(Span::styled(chunk, style));
                line_has_word = true;
            }
        }
    }
    lines.push(Line::from(spans));
    lines
}

fn is_table_separator(s: &str) -> bool {
    let t = s.trim();
    t.starts_with('|') && !t.is_empty() && t.chars().all(|c| matches!(c, '|' | '-' | ':' | ' '))
}

/// Render one table row: dim pipes, inline-styled cells, no wrapping.
fn table_row(s: &str, header: bool) -> Line<'static> {
    let base = if header { Style::new().add_modifier(Modifier::BOLD) } else { Style::new() };
    let pipe = Style::new().fg(Color::DarkGray);
    let mut spans: Vec<Span> = Vec::new();
    for (i, cell) in s.split('|').enumerate() {
        if i > 0 {
            spans.push(Span::styled("│", pipe));
        }
        for (text, seg) in parse_inline(cell) {
            spans.push(Span::styled(text, seg_style(base, seg)));
        }
    }
    Line::from(spans)
}

/// Render markdown-ish text to lines that fit `width` columns.
pub fn render(text: &str, width: usize) -> Vec<Line<'static>> {
    let width = width.max(12);
    let mut out: Vec<Line> = Vec::new();
    let mut in_fence = false;
    let mut suggestion = false;
    let mut fence_lang: Option<&'static crate::syntax::LangDef> = None;
    let fence_border = Style::new().fg(Color::DarkGray);
    let src: Vec<&str> = text.lines().collect();

    for (idx, raw) in src.iter().enumerate() {
        let line = raw.trim_end();
        let stripped = line.trim_start();

        // fenced code blocks
        if stripped.starts_with("```") {
            if in_fence {
                in_fence = false;
                out.push(Line::from(Span::styled("╰", fence_border)));
            } else {
                in_fence = true;
                let lang = stripped.trim_start_matches('`').trim();
                suggestion = lang == "suggestion";
                fence_lang = if suggestion || lang.is_empty() {
                    None
                } else {
                    crate::syntax::detect(lang)
                };
                let head = if suggestion {
                    "╭ suggested change".to_string()
                } else if lang.is_empty() {
                    "╭".to_string()
                } else {
                    format!("╭ {lang}")
                };
                let head_style = if suggestion {
                    Style::new().fg(SUGGESTION_FG).add_modifier(Modifier::BOLD)
                } else {
                    fence_border
                };
                out.push(Line::from(Span::styled(head, head_style)));
            }
            continue;
        }
        if in_fence {
            let body_fg = if suggestion { SUGGESTION_FG } else { FENCE_FG };
            for chunk in hard_wrap(line, width.saturating_sub(2)) {
                let mut spans = vec![Span::styled("│ ", fence_border)];
                match fence_lang {
                    Some(def) => {
                        for (text, kind) in crate::syntax::highlight(&chunk, def) {
                            let style = match kind {
                                crate::syntax::Kind::Text => Style::new().fg(FENCE_FG),
                                k => crate::syntax::style(k),
                            };
                            spans.push(Span::styled(text, style));
                        }
                    }
                    None => spans.push(Span::styled(chunk, Style::new().fg(body_fg))),
                }
                out.push(Line::from(spans));
            }
            continue;
        }

        // blank
        if stripped.is_empty() {
            out.push(Line::default());
            continue;
        }

        // tables
        if stripped.starts_with('|') {
            if is_table_separator(stripped) {
                continue; // header underline row — the header itself is bolded
            }
            let header = src.get(idx + 1).map(|n| is_table_separator(n)).unwrap_or(false);
            out.push(table_row(stripped, header));
            continue;
        }

        // horizontal rule
        if stripped.len() >= 3
            && (stripped.chars().all(|c| c == '-') || stripped.chars().all(|c| c == '*'))
        {
            out.push(Line::from(Span::styled(
                "─".repeat(width.min(60)),
                Style::new().fg(Color::DarkGray),
            )));
            continue;
        }

        // headings
        if let Some(rest) = stripped.strip_prefix('#') {
            let level = 1 + rest.chars().take_while(|c| *c == '#').count();
            let content = stripped.trim_start_matches('#').trim();
            let style = if level <= 2 {
                Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD)
            } else {
                Style::new().add_modifier(Modifier::BOLD)
            };
            out.extend(wrap_segs(parse_inline(content), width, vec![], 0, style));
            continue;
        }

        // quote
        if let Some(rest) = stripped.strip_prefix('>') {
            let content = rest.trim_start();
            let base = Style::new().fg(Color::DarkGray).add_modifier(Modifier::ITALIC);
            out.extend(wrap_segs(
                parse_inline(content),
                width,
                vec![Span::styled("▎ ", Style::new().fg(Color::DarkGray))],
                2,
                base,
            ));
            continue;
        }

        // bullets & task lists (preserve nesting depth)
        let depth = line.len() - stripped.len();
        let bullet = ["- [ ] ", "- [x] ", "- ", "* ", "+ "]
            .iter()
            .find(|p| stripped.starts_with(**p))
            .copied();
        if let Some(marker) = bullet {
            let content = &stripped[marker.len()..];
            let indent = " ".repeat(depth.min(8));
            let glyph = match marker {
                "- [ ] " => "☐ ",
                "- [x] " => "☑ ",
                _ => "• ",
            };
            let prefix = vec![
                Span::raw(indent.clone()),
                Span::styled(glyph, Style::new().fg(Color::Cyan)),
            ];
            out.extend(wrap_segs(
                parse_inline(content),
                width,
                prefix,
                indent.len() + 2,
                Style::new(),
            ));
            continue;
        }

        // numbered list
        if let Some(dot) = stripped.find(". ") {
            if dot <= 3 && stripped[..dot].chars().all(|c| c.is_ascii_digit()) {
                let indent = " ".repeat(depth.min(8));
                let num = format!("{}. ", &stripped[..dot]);
                let cont = indent.len() + num.chars().count();
                let prefix = vec![
                    Span::raw(indent),
                    Span::styled(num, Style::new().fg(Color::Cyan)),
                ];
                out.extend(wrap_segs(
                    parse_inline(&stripped[dot + 2..]),
                    width,
                    prefix,
                    cont,
                    Style::new(),
                ));
                continue;
            }
        }

        // plain paragraph line
        out.extend(wrap_segs(parse_inline(line), width, vec![], 0, Style::new()));
    }
    if in_fence {
        out.push(Line::from(Span::styled("╰", fence_border)));
    }
    out
}

// ---- comment sanitizing ----
// PR comments from bots arrive wrapped in HTML: hidden markers, <details>
// blocks, button <div>s with base64 deep links, badge images, footers.
// clean_comment() reduces them to the part a reviewer actually reads.

/// Drop every `open…close` span (content included).
fn remove_between(s: &str, open: &str, close: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find(open) {
        out.push_str(&rest[..i]);
        match rest[i + open.len()..].find(close) {
            Some(j) => rest = &rest[i + open.len() + j + close.len()..],
            None => {
                rest = "";
                break;
            }
        }
    }
    out.push_str(rest);
    out
}

/// `<a href="U">T</a>` -> `[T](U)`; anchors with empty text (button images
/// already stripped) are dropped entirely.
fn anchors_to_md(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find("<a ") {
        out.push_str(&rest[..i]);
        let Some(tag_end) = rest[i..].find('>') else {
            rest = &rest[i..];
            break;
        };
        let tag = &rest[i..i + tag_end];
        let href = tag.find("href=\"").and_then(|h| {
            let v = &tag[h + 6..];
            v.find('"').map(|e| v[..e].to_string())
        });
        let after = &rest[i + tag_end + 1..];
        let Some(close) = after.find("</a>") else {
            rest = &rest[i..];
            break;
        };
        let text = after[..close].trim();
        if !text.is_empty() {
            match &href {
                Some(u) => out.push_str(&format!("[{text}]({u})")),
                None => out.push_str(text),
            }
        }
        rest = &after[close + 4..];
    }
    out.push_str(rest);
    out
}

/// Bugbot hides file locations inside an HTML comment — surface them as
/// `↳ path:lines` before comments get stripped.
fn locations_to_text(s: &str) -> String {
    let Some(start) = s.find("<!-- LOCATIONS START") else {
        return s.to_string();
    };
    let Some(end_rel) = s[start..].find("LOCATIONS END -->") else {
        return s.to_string();
    };
    let inner = &s[start + "<!-- LOCATIONS START".len()..start + end_rel];
    let locs: Vec<String> = inner
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(|l| format!("↳ `{}`", l.replace("#L", ":").replace("-L", "-")))
        .collect();
    let mut out = String::new();
    out.push_str(&s[..start]);
    out.push_str(&locs.join("\n"));
    out.push_str(&s[start + end_rel + "LOCATIONS END -->".len()..]);
    out
}

/// linear-code[bot] linkbacks embed the whole issue description; compact to
/// the ticket link (+ the Linear review link when present).
fn linear_bot_compact(body: &str) -> Option<String> {
    let sum_start = body.find("<summary>")? + "<summary>".len();
    let sum_end = body.find("</summary>")?;
    let anchor = anchors_to_md(&body[sum_start..sum_end]);
    let mut out = format!("◈ {}", anchor.trim());
    if let Some(r) = body.find("linear-review-link") {
        let tail = anchors_to_md(&body[r..]);
        if let Some(link_start) = tail.find('[') {
            if let Some(link_end) = tail[link_start..].find(')') {
                out.push('\n');
                out.push_str(&tail[link_start..link_start + link_end + 1]);
            }
        }
    }
    Some(out)
}

/// HTML element names that are safe to strip when unconverted — anything
/// else in angle brackets (e.g. `Vec<String>` in prose) is left alone.
const STRIP_TAGS: &[&str] = &[
    "div", "span", "p", "details", "summary", "table", "thead", "tbody", "tfoot", "tr", "td",
    "th", "ul", "ol", "li", "a", "img", "picture", "source", "video", "sup", "sub", "br", "hr",
    "center", "font", "section", "article", "u", "small", "big", "em", "i", "b", "strong",
    "code", "pre", "blockquote", "kbd", "tt", "input", "dl", "dt", "dd", "g-emoji", "del",
    "ins", "mark", "abbr", "figure", "figcaption",
];

fn is_strippable_tag(inner: &str) -> bool {
    let name: String = inner
        .trim_start_matches('/')
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '-')
        .collect::<String>()
        .to_lowercase();
    !name.is_empty() && STRIP_TAGS.contains(&name.as_str())
}

/// Drop remaining known HTML tags; leave unknown angle-bracket text intact.
fn strip_known_tags(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '<' {
            if let Some(end) = (i + 1..chars.len().min(i + 300)).find(|&k| chars[k] == '>') {
                let inner: String = chars[i + 1..end].iter().collect();
                if is_strippable_tag(&inner) {
                    i = end + 1;
                    continue;
                }
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

/// Convert one non-code text fragment: common HTML to markdown, whitelist
/// stripping for the rest, entity decoding.
fn html_fragment_to_md(s: &str) -> String {
    if !s.contains('<') && !s.contains('&') {
        return s.to_string();
    }
    let mut t = s.to_string();
    for (a, b) in [
        ("<b>", "**"), ("</b>", "**"), ("<strong>", "**"), ("</strong>", "**"),
        ("<code>", "`"), ("</code>", "`"), ("<tt>", "`"), ("</tt>", "`"),
        ("<kbd>", "`"), ("</kbd>", "`"),
        ("<pre>", "\n```\n"), ("</pre>", "\n```\n"),
        ("<summary>", "**"), ("</summary>", "**\n"),
        ("<li>", "\n- "), ("</li>", ""),
        ("<blockquote>", "\n> "), ("</blockquote>", "\n"),
        ("<hr>", "\n---\n"), ("<hr/>", "\n---\n"), ("<hr />", "\n---\n"),
        ("<tr>", "\n| "), ("</td>", " | "), ("</th>", " | "),
        ("<p>", "\n"), ("</p>", "\n"),
        ("<br>", "\n"), ("<br/>", "\n"), ("<br />", "\n"),
    ] {
        if t.contains(a) {
            t = t.replace(a, b);
        }
    }
    for i in 1..=6 {
        t = t.replace(&format!("<h{i}>"), &format!("\n{} ", "#".repeat(i)));
        t = t.replace(&format!("</h{i}>"), "\n");
    }
    t = strip_known_tags(&t);
    for (a, b) in [
        ("&lt;", "<"), ("&gt;", ">"), ("&quot;", "\""), ("&#39;", "'"), ("&apos;", "'"),
        ("&nbsp;", " "), ("&hellip;", "…"), ("&mdash;", "—"), ("&ndash;", "–"),
        // decode last so `&amp;lt;` becomes `&lt;` (literal), not `<`
        ("&amp;", "&"),
    ] {
        if t.contains(a) {
            t = t.replace(a, b);
        }
    }
    t
}

/// Apply `f` to text outside fenced code blocks and inline backtick spans,
/// so HTML handling never mangles code like `Vec<String>`.
fn map_outside_code(s: &str, f: impl Fn(&str) -> String) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_fence = false;
    for (idx, line) in s.lines().enumerate() {
        if idx > 0 {
            out.push('\n');
        }
        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
            out.push_str(line);
            continue;
        }
        if in_fence {
            out.push_str(line);
            continue;
        }
        let mut in_code = false;
        for (i, part) in line.split('`').enumerate() {
            if i > 0 {
                out.push('`');
            }
            if in_code {
                out.push_str(part);
            } else {
                out.push_str(&f(part));
            }
            in_code = !in_code;
        }
    }
    out
}

fn collapse_blanks(s: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    let mut blank = false;
    for l in s.lines() {
        if l.trim().is_empty() {
            if !blank && !out.is_empty() {
                out.push("");
            }
            blank = true;
        } else {
            out.push(l);
            blank = false;
        }
    }
    while out.last().is_some_and(|l| l.is_empty()) {
        out.pop();
    }
    out.join("\n")
}

/// Strip bot/HTML noise from a comment body before markdown rendering.
pub fn clean_comment(author: &str, body: &str) -> String {
    if author.starts_with("linear-code") {
        if let Some(compact) = linear_bot_compact(body) {
            return compact;
        }
    }
    let mut b = locations_to_text(body);
    b = remove_between(&b, "<picture", "</picture>");
    b = remove_between(&b, "<sup", "</sup>");
    b = remove_between(&b, "<!--", "-->");
    b = remove_between(&b, "<img", ">");
    b = anchors_to_md(&b);
    b = map_outside_code(&b, html_fragment_to_md);
    if author.starts_with("cursor") {
        b = b
            .replace("**High Severity**", "**‼ high severity**")
            .replace("**Medium Severity**", "**▲ medium severity**")
            .replace("**Low Severity**", "**▽ low severity**");
    }
    collapse_blanks(&b)
}

/// Collect every link in a (cleaned) markdown text: `[label](url)` pairs and
/// bare URLs, in order of appearance, deduplicated by URL.
pub fn extract_links(text: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    let mut push = |label: String, url: String| {
        if url.starts_with("http") && !seen.contains(&url) {
            seen.push(url.clone());
            out.push((label, url));
        }
    };
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '[' || (chars[i] == '!' && chars.get(i + 1) == Some(&'[')) {
            let open = if chars[i] == '!' { i + 1 } else { i };
            if let Some(close) = (open + 1..chars.len()).find(|&k| chars[k] == ']') {
                if chars.get(close + 1) == Some(&'(') {
                    if let Some(end) = (close + 2..chars.len()).find(|&k| chars[k] == ')') {
                        let label: String = chars[open + 1..close].iter().collect();
                        let url: String = chars[close + 2..end].iter().collect();
                        let label = if label.trim().is_empty() { url.clone() } else { label };
                        push(label, url);
                        i = end + 1;
                        continue;
                    }
                }
            }
        }
        if (starts_with_at(&chars, i, "http://") || starts_with_at(&chars, i, "https://"))
            && (i == 0 || !chars[i - 1].is_alphanumeric())
        {
            let mut j = i;
            while j < chars.len() && !chars[j].is_whitespace() && chars[j] != ')' {
                j += 1;
            }
            let mut url: String = chars[i..j].iter().collect();
            while url.ends_with(['.', ',', ';', ':']) {
                url.pop();
            }
            push(url.clone(), url);
            i = j;
            continue;
        }
        i += 1;
    }
    out
}

/// Prepend a fixed indent to every rendered line.
pub fn indent(lines: Vec<Line<'static>>, pad: &str) -> Vec<Line<'static>> {
    lines
        .into_iter()
        .map(|mut l| {
            l.spans.insert(0, Span::raw(pad.to_string()));
            l
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flat(lines: &[Line]) -> Vec<String> {
        lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect::<String>())
            .collect()
    }

    #[test]
    fn fences_and_inline() {
        let lines = render("intro `code` here\n```rust\nlet x = 1;\n```\ndone", 40);
        let f = flat(&lines);
        assert_eq!(f[0], "intro code here");
        assert_eq!(f[1], "╭ rust");
        assert_eq!(f[2], "│ let x = 1;");
        assert_eq!(f[3], "╰");
        assert_eq!(f[4], "done");
    }

    #[test]
    fn bullets_wrap_with_hanging_indent() {
        let lines = render("- one two three four five six seven", 16);
        let f = flat(&lines);
        assert!(f[0].starts_with("• one"));
        assert!(f[1].starts_with("  "), "continuation should hang: {:?}", f[1]);
        let b = flat(&render("**hi** there", 40));
        assert_eq!(b[0], "hi there");
    }

    #[test]
    fn links_images_and_strike() {
        let f = flat(&render("see [the docs](https://x.io/d) and ~~old~~ new", 60));
        assert_eq!(f[0], "see the docs and old new");
        let f = flat(&render("![screenshot](https://x.io/i.png) attached", 60));
        assert_eq!(f[0], "⧉ screenshot attached");
        // bare URL kept, trailing period stripped from the link segment
        let lines = render("look at https://x.io/path.", 60);
        let link = lines[0]
            .spans
            .iter()
            .find(|s| s.style.add_modifier.contains(Modifier::UNDERLINED))
            .unwrap();
        assert_eq!(link.content.as_ref(), "https://x.io/path");
    }

    #[test]
    fn tables_render_with_dim_pipes() {
        let f = flat(&render("| a | b |\n|---|---|\n| 1 | 2 |", 40));
        assert_eq!(f.len(), 2, "separator row is dropped: {f:?}");
        assert!(f[0].contains('a') && f[0].contains('│'));
        assert!(f[1].contains('1'));
    }

    #[test]
    fn suggestion_fence_labeled() {
        let f = flat(&render("```suggestion\nlet y = 2;\n```", 40));
        assert_eq!(f[0], "╭ suggested change");
        assert_eq!(f[1], "│ let y = 2;");
    }

    #[test]
    fn linear_bot_comments_compact_to_ticket_link() {
        let body = "<!-- linear-linkback -->\n<details>\n<summary><a href=\"https://linear.app/acme/issue/ABC-123/task-e1\">ABC-123 Task E.1</a></summary>\n<p>\n\n**TEP:** long description here\n</p>\n</details>\n<!-- linear-review-link -->\n<p><a href=\"https://linear.app/acme/review/xyz\">Review in Linear</a></p>";
        let c = clean_comment("linear-code[bot]", body);
        assert_eq!(
            c,
            "◈ [ABC-123 Task E.1](https://linear.app/acme/issue/ABC-123/task-e1)\n[Review in Linear](https://linear.app/acme/review/xyz)"
        );
    }

    #[test]
    fn bugbot_comments_lose_buttons_and_keep_locations() {
        let body = "### Transport failures still counted as parse\n\n**Medium Severity**\n\n<!-- DESCRIPTION START -->\nreal description text\n<!-- DESCRIPTION END -->\n\n<!-- BUGBOT_BUG_ID: af18 -->\n\n<!-- LOCATIONS START\nserver/modules/UseCase.ts#L322-L341\nLOCATIONS END -->\n<div><a href=\"https://cursor.com/open?link=eyJ2\" target=\"_blank\"><picture><img alt=\"Fix in Cursor\" src=\"x\"></picture></a></div>\n\n<sup>Reviewed by [Cursor Bugbot](https://cursor.com/bugbot) for commit 501212a.</sup>";
        let c = clean_comment("cursor[bot]", body);
        assert!(c.contains("### Transport failures"));
        assert!(c.contains("**▲ medium severity**"));
        assert!(c.contains("real description text"));
        assert!(c.contains("↳ `server/modules/UseCase.ts:322-341`"));
        assert!(!c.contains("cursor.com"), "buttons/footer must be gone: {c}");
        assert!(!c.contains("<!--"));
        assert!(!c.contains("<div"));
    }

    #[test]
    fn html_converts_to_markdown() {
        let c = clean_comment("", "<b>bold</b> and <code>x()</code> here");
        assert_eq!(c, "**bold** and `x()` here");
        let c = clean_comment("", "<ul><li>one</li><li>two</li></ul>");
        assert_eq!(c, "- one\n- two");
        let c = clean_comment("", "<h3>Steps</h3>reproduce &amp; verify &lt;3");
        assert_eq!(c, "### Steps\nreproduce & verify <3");
        let c = clean_comment(
            "",
            "<table><tr><th>col</th></tr><tr><td>val</td></tr></table>",
        );
        assert!(c.contains("| col |"), "{c}");
        assert!(c.contains("| val |"), "{c}");
    }

    #[test]
    fn html_stripping_spares_code_and_generics() {
        // generics in prose are not real tags — leave them alone
        let c = clean_comment("", "returns Vec<String> from parse");
        assert_eq!(c, "returns Vec<String> from parse");
        // inline code spans are never touched
        let c = clean_comment("", "wrap in `Option<div>` and <b>note</b> it");
        assert_eq!(c, "wrap in `Option<div>` and **note** it");
        // fenced blocks are never touched
        let c = clean_comment("", "```rust\nlet x: Vec<b> = vec![];\n&amp;\n```");
        assert!(c.contains("Vec<b>"), "{c}");
        assert!(c.contains("&amp;"), "{c}");
    }

    #[test]
    fn extracts_links_in_order_without_dupes() {
        let links = extract_links(
            "see [docs](https://x.io/d) and https://y.io/raw. also [again](https://x.io/d)",
        );
        assert_eq!(
            links,
            vec![
                ("docs".to_string(), "https://x.io/d".to_string()),
                ("https://y.io/raw".to_string(), "https://y.io/raw".to_string()),
            ]
        );
    }

    #[test]
    fn long_words_hard_break() {
        let word = "x".repeat(50);
        let lines = render(&word, 20);
        assert!(lines.len() > 1);
    }
}
