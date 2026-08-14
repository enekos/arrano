use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::sync::mpsc::Sender;
use std::thread;

use crate::app::AppEvent;

const MAX_DIFF_CHARS: usize = 150_000;

/// Where the review runs: inside a checkout of the PR branch, inside the
/// repo's main checkout (branch may differ), or nowhere special.
pub enum ReviewCtx {
    Branch(std::path::PathBuf),
    Repo(std::path::PathBuf),
    None,
}

fn context_note(ctx: &ReviewCtx) -> &'static str {
    match ctx {
        ReviewCtx::Branch(_) => {
            "You are running inside a checkout of the PR's branch. Use Read/Grep/Glob to \
             inspect surrounding code where the diff lacks context (callers, types, tests). \
             Do not modify anything.\n"
        }
        ReviewCtx::Repo(_) => {
            "You are running inside the repository's main checkout — the checked-out branch \
             may differ from the PR branch, so trust the diff over the working tree where \
             they disagree. Use Read/Grep/Glob for surrounding context. Do not modify \
             anything.\n"
        }
        ReviewCtx::None => "Do not use any tools.\n",
    }
}

fn build_prompt(
    repo: &str,
    number: u64,
    title: &str,
    body: &str,
    base: &str,
    head: &str,
    diff: &str,
    ctx: &ReviewCtx,
) -> String {
    let mut d = diff.to_string();
    if d.len() > MAX_DIFF_CHARS {
        let mut cut = MAX_DIFF_CHARS;
        while !d.is_char_boundary(cut) {
            cut -= 1;
        }
        d.truncate(cut);
        d.push_str("\n\n[diff truncated — review what is shown]");
    }
    format!(
        "You are reviewing a GitHub pull request. Produce a concise, high-signal code review in plain text (no markdown headers). {}\
         Structure:\n\
         1. One-line verdict (ship it / needs changes / needs discussion) with a one-sentence reason.\n\
         2. Findings ranked by severity, each with file:line reference and a concrete failure scenario. Real bugs, risky changes, and missing tests only — skip style nits unless they hide a bug.\n\
         3. Open questions for the author, if any.\n\
         If the diff looks fine, say so briefly instead of inventing findings.\n\n\
         PR: {repo}#{number} — {title}\n\
         Base: {base} <- Head: {head}\n\n\
         Description:\n{body}\n\n\
         Diff:\n{d}",
        context_note(ctx)
    )
}

pub fn spawn_review(
    tx: Sender<AppEvent>,
    repo: String,
    number: u64,
    title: String,
    body: String,
    base: String,
    head: String,
    diff: String,
    ctx: ReviewCtx,
) {
    thread::spawn(move || {
        let prompt = build_prompt(&repo, number, &title, &body, &base, &head, &diff, &ctx);
        let mut cmd = Command::new("claude");
        cmd.arg("-p");
        match &ctx {
            ReviewCtx::Branch(dir) | ReviewCtx::Repo(dir) => {
                cmd.current_dir(dir);
                cmd.args(["--allowedTools", "Read Grep Glob"]);
            }
            ReviewCtx::None => {}
        }
        let child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn();
        let mut child = match child {
            Ok(c) => c,
            Err(e) => {
                let _ = tx.send(AppEvent::ClaudeDone(
                    repo,
                    number,
                    Err(format!("failed to spawn claude: {e}")),
                ));
                return;
            }
        };
        {
            let mut stdin = child.stdin.take().unwrap();
            if let Err(e) = stdin.write_all(prompt.as_bytes()) {
                let _ = child.kill();
                let _ = tx.send(AppEvent::ClaudeDone(
                    repo,
                    number,
                    Err(format!("failed to write prompt: {e}")),
                ));
                return;
            }
        }
        let stdout = child.stdout.take().unwrap();
        for line in BufReader::new(stdout).lines() {
            match line {
                Ok(l) => {
                    let _ = tx.send(AppEvent::ClaudeLine(repo.clone(), number, l));
                }
                Err(_) => break,
            }
        }
        let out = child.wait_with_output();
        let result = match out {
            Ok(o) if o.status.success() => Ok(()),
            Ok(o) => Err(format!(
                "claude exited with {}: {}",
                o.status,
                String::from_utf8_lossy(&o.stderr).trim()
            )),
            Err(e) => Err(e.to_string()),
        };
        let _ = tx.send(AppEvent::ClaudeDone(repo, number, result));
    });
}
