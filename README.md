# arrano

Eagle view over your GitHub PRs and Linear queue. A lazygit-style TUI on top
of the `gh` CLI: every open PR you authored and every review request, across
all repos, in one screen — diff, checks, review threads, a full review
workflow, your assigned Linear issues, and Claude Code reviews built in.

```
curl -fsSL https://raw.githubusercontent.com/enekos/arrano/master/install.sh | bash
```

(or `cargo install --path .` from a checkout)

```
arrano                  # everything, all orgs
arrano --org my-org     # scope both lanes to one org/owner
```

Requires `gh` (authenticated). For Claude reviews: the `claude` CLI. For the
Linear view: `$LINEAR_API_KEY` (or the `linear` CLI). For comment humanizing:
any local LLM — `ollama`, or llama.cpp's `llama-cli` plus a GGUF
(auto-discovered in the HF / LM Studio caches).

## Lanes & views

- **[1] my PRs** — `is:pr is:open author:<you>`, grouped by repo
- **[2] review requested** — `is:pr is:open review-requested:<you>`
- **[3] search** — results of your last `/` search
- **[4] linear** — your assigned issues, grouped by workflow state in
  Linear's own colors; full detail (state, priority, estimate, cycle, due,
  labels, description, comments) on the right

Lanes auto-refresh (`$ARRANO_REFRESH_SECS`, default 180, 0 disables) and show
a "synced Xm ago" age. PR details are cached and prefetched around the
selection, so browsing is instant.

## Filter & search (`/`)

Typing filters the current lane live — every word must match repo, `#number`,
title, author, or branch, so `ABC-123` and `#7076` just work. On enter:

- plain words with local hits → stays a local filter
- qualifiers (`author:x`, `is:merged`, `repo:o/r`, bare `merged`/`closed`/
  `draft`, `@me`) or zero local hits → GitHub search into lane 3
- your `--org` scope is applied unless the query scopes itself
- in the linear view, filtering is always local

## Keys

| key | action |
| --- | --- |
| `1`–`4` | switch lane / view |
| `/` | filter / search |
| `j` / `k`, `g` / `G` | move / scroll |
| `ctrl-f` / `ctrl-b` | page forward / back (smooth scroll) |
| mouse wheel | scroll the pane under the pointer |
| `enter`, `tab`, `esc` | detail, toggle focus, back |
| `h` / `l` | switch detail tab |
| `o` | open in browser (PR, check run, comment, or issue) |
| `c` | comment — PR comment; diff line: queue for review; thread: reply |
| `v` | submit review: approve / request changes / comment |
| `m` | merge — squash / merge commit / rebase |
| `u` | update branch from base — rebase or merge |
| `x` / `z` | resolve thread / fold thread (comments tab) |
| `w` | create or reuse a local worktree for the PR branch |
| `L` | jump to the PR's Linear ticket (in-app) |
| `R` | run a Claude Code review on the diff |
| `P` | post the finished Claude review as a PR comment |
| `r` | refresh everything |
| `?` | help |

## Reviews

Inline diff comments (`c` on a diff line) queue locally — marked `●` in the
diff gutter — and are submitted together by `v` as **one** review through the
REST reviews API, so the author gets a single notification. `d` in the review
modal discards the queue. Replies (`c` on a thread) and PR comments post
immediately.

## Composer

`c` and `v` open a small editor. `ctrl-s` posts. `ctrl-r` rewrites your rough
draft with a **local** LLM into your own review voice — informal, direct, no
filler, kind (blame and score-keeping are stripped, verdicts like "lgtm"
survive); `ctrl-z` restores the original. Nothing leaves the machine.

## Claude reviews

`R` pipes the PR description + diff into `claude -p` and streams the review
into the claude tab. When a local checkout of the repo exists, claude runs
inside it (a worktree on the PR branch if present — press `w` first for the
strongest context) with read-only tools, so it can chase callers and tests
beyond the diff. Nothing is posted unless you press `P` and confirm.

## Rendering

Comment bodies, descriptions, and claude output render a practical markdown
subset: fenced code (incl. GitHub `suggestion` blocks), inline code, bold,
strikethrough, links, tables, quotes, task lists. Bot noise is stripped
before rendering: Linear linkback comments compact to the ticket link, Cursor
bugbot findings lose their buttons/footers and gain clean `file:line`
locations, HTML wrappers and badge images are removed for everyone. Review
threads show their diff hunk; resolved threads start folded.

## Environment

| var | meaning |
| --- | --- |
| `ARRANO_REPO_ROOTS` | colon-separated dirs containing local checkouts (default: `~/src:~/code:~/projects:~/dev`) |
| `ARRANO_REFRESH_SECS` | auto-refresh cadence, 0 disables (default 180) |
| `ARRANO_LLM_GGUF` / `ARRANO_LLM_MODEL` | model for `ctrl-r` humanize (llama.cpp path / ollama name) |
| `LINEAR_API_KEY` | Linear GraphQL auth for the linear view |
| `ZZ_WORKTREE_DIR` | overrides where `w` places worktrees |
