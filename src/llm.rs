//! Local-LLM comment rewriting. Tries, in order: `ollama` (if on PATH),
//! then llama.cpp's `llama-cli` with a GGUF found via $ARRANO_LLM_GGUF or
//! scanning the HuggingFace / LM Studio caches. No network, no API keys.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::Sender;
use std::thread;

use crate::app::AppEvent;

const SYSTEM: &str = "You rewrite rough GitHub PR review comments in the author's own voice: \
informal, direct, as short as possible, kind without being soft.\n\
Style rules:\n\
- No greeting, no sign-off, no filler. Never 'Hey', 'I noticed', 'It would be great if', \
'Just a thought'. Start with the point.\n\
- Shorter is better. If a sentence adds no information, drop it. The rewrite is usually \
shorter than the draft.\n\
- State problems as plain facts about the code with the reason attached: \
'this breaks when X. Y' / 'this will blow up in prod with no trace'. Plain full stops between \
clauses, never dashes. Never blame the person, \
and drop anything that keeps score against them ('again', 'second time', 'as I already said').\n\
- Fix typos and grammar in passing.\n\
- Suggestions read 'I would X, <short reason>'. Genuine doubts read as a direct question: \
'are we sure this wont leak to the user?'.\n\
- Casual register: contractions, lowercase start is fine, at most a single ':)' or '👍' to \
soften — never exclamation marks, never other emojis unless the draft had them.\n\
- Keep all technical content: backticks, file paths, commit SHAs, links, numbers. Keep \
verdicts too — if the draft approves ('lgtm', 'ship it'), the rewrite must still approve.\n\
- Keep the language of the draft.\n\
Examples:\n\
draft: wtf is this, you cant just swallow the error in parseImport, this will blow up in prod \
and nobody will know why\n\
rewrite: `parseImport` swallows the error. this will blow up in prod with no trace, log it or \
rethrow\n\
draft: maybe it would perhaps be a good idea to consider possibly extracting this retry logic \
since it appears to be duplicated in several places?\n\
rewrite: I would extract the retry logic, it's copy-pasted in 3 places now\n\
draft: I think there might be an issue where when the input is null this could potentially crash?\n\
rewrite: doesn't this crash when input is null?\n\
draft: NO. this is wrong, salary is in cents everywhere else, you forgot to multiply by 100 \
again, second time this sprint\n\
rewrite: salary is in cents everywhere else. this needs the x100\n\
Output ONLY the rewritten comment, nothing else.";

/// Fixed user-turn text — llama-cli echoes it back as "> {USER_MARK}", which
/// is how we find where the generated text starts. The trailing reminder is
/// deliberate: small models follow the most recent instruction best.
const USER_MARK: &str =
    "Rewrite it now. No blame or score-keeping: drop words like 'again' / 'as I said'.";

pub fn spawn_rewrite(tx: Sender<AppEvent>, draft: String) {
    thread::spawn(move || {
        let _ = tx.send(AppEvent::Rewrite(rewrite(&draft)));
    });
}

fn on_path(bin: &str) -> bool {
    std::env::var_os("PATH")
        .map(|paths| {
            std::env::split_paths(&paths).any(|d| d.join(bin).is_file())
        })
        .unwrap_or(false)
}

fn rewrite(draft: &str) -> Result<String, String> {
    if on_path("ollama") {
        return ollama(draft);
    }
    if on_path("llama-cli") {
        return llama_cli(draft);
    }
    Err("no local LLM found — install ollama, or llama.cpp + a GGUF (set $ARRANO_LLM_GGUF)".into())
}

// ---- ollama ----

fn ollama_model() -> Result<String, String> {
    if let Ok(m) = std::env::var("ARRANO_LLM_MODEL") {
        return Ok(m);
    }
    let out = Command::new("ollama")
        .arg("list")
        .output()
        .map_err(|e| e.to_string())?;
    let text = String::from_utf8_lossy(&out.stdout);
    text.lines()
        .skip(1)
        .filter_map(|l| l.split_whitespace().next())
        .find(|m| !m.contains("embed"))
        .map(str::to_string)
        .ok_or_else(|| "ollama has no models pulled — set $ARRANO_LLM_MODEL".into())
}

fn ollama(draft: &str) -> Result<String, String> {
    use std::io::Write;
    let model = ollama_model()?;
    let prompt = format!("{SYSTEM}\n\nComment:\n{draft}");
    let mut child = Command::new("ollama")
        .args(["run", &model])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("failed to spawn ollama: {e}"))?;
    child
        .stdin
        .take()
        .unwrap()
        .write_all(prompt.as_bytes())
        .map_err(|e| e.to_string())?;
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(format!("ollama failed: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if text.is_empty() {
        Err("ollama returned nothing".into())
    } else {
        Ok(text)
    }
}

// ---- llama.cpp ----

fn find_gguf() -> Result<PathBuf, String> {
    if let Ok(p) = std::env::var("ARRANO_LLM_GGUF") {
        let p = PathBuf::from(p);
        return if p.is_file() {
            Ok(p)
        } else {
            Err(format!("$ARRANO_LLM_GGUF does not exist: {}", p.display()))
        };
    }
    let home = std::env::var("HOME").map_err(|_| "no $HOME".to_string())?;
    let roots = [
        format!("{home}/.cache/huggingface/hub"),
        format!("{home}/.cache/llama.cpp"),
        format!("{home}/.lmstudio/models"),
    ];
    let mut found: Vec<PathBuf> = Vec::new();
    for root in &roots {
        walk_gguf(Path::new(root), 0, &mut found);
    }
    // newest first — most recently downloaded model wins
    found.sort_by_key(|p| {
        std::cmp::Reverse(p.metadata().and_then(|m| m.modified()).ok())
    });
    found
        .into_iter()
        .next()
        .ok_or_else(|| "no .gguf model found in HF/llama.cpp/LM Studio caches — set $ARRANO_LLM_GGUF".into())
}

fn walk_gguf(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    if depth > 6 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            walk_gguf(&p, depth + 1, out);
        } else if p.extension().is_some_and(|x| x == "gguf")
            // multi-part models (…-00002-of-00003.gguf) must load from part 1
            && !p
                .file_stem()
                .and_then(|s| s.to_str())
                .is_some_and(|s| s.contains("-of-") && !s.contains("00001-of-"))
        {
            out.push(p);
        }
    }
}

fn llama_cli(draft: &str) -> Result<String, String> {
    let gguf = find_gguf()?;
    let system = format!("{SYSTEM}\n\nComment:\n{draft}");
    let out = Command::new("llama-cli")
        .args(["-m"])
        .arg(&gguf)
        .args([
            "--jinja",
            "-st",
            "--simple-io",
            "--no-display-prompt",
            "-n",
            "2048",
            "--temp",
            "0.3",
            "-sys",
            &system,
            "-p",
            USER_MARK,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("failed to spawn llama-cli: {e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(format!(
            "llama-cli failed: {}",
            err.lines().last().unwrap_or("unknown error")
        ));
    }
    parse_llama_output(&String::from_utf8_lossy(&out.stdout))
}

/// llama-cli prints a banner, echoes the user turn as "> {USER_MARK}", then
/// the generation, then a "[ Prompt: … t/s ]" stats line.
fn parse_llama_output(stdout: &str) -> Result<String, String> {
    let mut in_response = false;
    let mut resp: Vec<&str> = Vec::new();
    for line in stdout.lines() {
        if !in_response {
            if line.starts_with("> ") {
                in_response = true;
            }
            continue;
        }
        if line.starts_with("[ Prompt:") || line.starts_with("Exiting") {
            break;
        }
        resp.push(line);
    }
    let text = resp.join("\n").trim().to_string();
    if text.is_empty() {
        Err("llama-cli returned nothing".into())
    } else {
        Ok(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_llama_cli_output() {
        let raw = "\n\nLoading model... \nbuild : x\n\navailable commands:\n  /exit\n\n> Rewrite it now.\nThis breaks when input is null.\nAlso off by one.\n\n[ Prompt: 345 t/s | Generation: 137 t/s ]\n\nExiting...\n";
        assert_eq!(
            parse_llama_output(raw).unwrap(),
            "This breaks when input is null.\nAlso off by one."
        );
    }
}
