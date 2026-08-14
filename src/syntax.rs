//! Zero-dependency, line-oriented syntax highlighting for diffs and code
//! fences. Per-line tokenization (comments, strings, numbers, keywords,
//! types, calls) — imperfect across multi-line constructs, which is fine for
//! diff hunks and short fences.

use ratatui::style::{Color, Modifier, Style};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Text,
    Keyword,
    Str,
    Comment,
    Number,
    Type,
    Func,
}

pub fn style(kind: Kind) -> Style {
    match kind {
        Kind::Text => Style::new(),
        Kind::Keyword => Style::new().fg(Color::Rgb(198, 120, 221)),
        Kind::Str => Style::new().fg(Color::Rgb(152, 195, 121)),
        Kind::Comment => Style::new().fg(Color::Rgb(106, 115, 125)).add_modifier(Modifier::DIM),
        Kind::Number => Style::new().fg(Color::Rgb(209, 154, 102)),
        Kind::Type => Style::new().fg(Color::Rgb(229, 192, 123)),
        Kind::Func => Style::new().fg(Color::Rgb(97, 175, 239)),
    }
}

pub struct LangDef {
    keywords: &'static [&'static str],
    comments: &'static [&'static str],
    strings: &'static [char],
    case_insensitive: bool,
}

const RUST: LangDef = LangDef {
    keywords: &[
        "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum",
        "extern", "false", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod",
        "move", "mut", "pub", "ref", "return", "self", "Self", "static", "struct", "super",
        "trait", "true", "type", "unsafe", "use", "where", "while",
    ],
    comments: &["//"],
    strings: &['"'],
    case_insensitive: false,
};

const TS: LangDef = LangDef {
    keywords: &[
        "abstract", "any", "as", "async", "await", "boolean", "break", "case", "catch", "class",
        "const", "continue", "default", "delete", "do", "else", "enum", "export", "extends",
        "false", "finally", "for", "from", "function", "if", "implements", "import", "in",
        "instanceof", "interface", "let", "new", "null", "number", "of", "private", "protected",
        "public", "readonly", "return", "static", "string", "super", "switch", "this", "throw",
        "true", "try", "type", "typeof", "undefined", "var", "void", "while", "yield",
    ],
    comments: &["//"],
    strings: &['"', '\'', '`'],
    case_insensitive: false,
};

const PY: LangDef = LangDef {
    keywords: &[
        "and", "as", "assert", "async", "await", "break", "class", "continue", "def", "del",
        "elif", "else", "except", "False", "finally", "for", "from", "global", "if", "import",
        "in", "is", "lambda", "None", "not", "or", "pass", "raise", "return", "True", "try",
        "while", "with", "yield", "self",
    ],
    comments: &["#"],
    strings: &['"', '\''],
    case_insensitive: false,
};

const GO: LangDef = LangDef {
    keywords: &[
        "break", "case", "chan", "const", "continue", "default", "defer", "else", "fallthrough",
        "false", "for", "func", "go", "goto", "if", "import", "interface", "map", "nil",
        "package", "range", "return", "select", "struct", "switch", "true", "type", "var",
    ],
    comments: &["//"],
    strings: &['"', '`'],
    case_insensitive: false,
};

const SWIFT: LangDef = LangDef {
    keywords: &[
        "as", "break", "case", "catch", "class", "continue", "default", "defer", "do", "else",
        "enum", "extension", "false", "for", "func", "guard", "if", "import", "in", "init",
        "let", "nil", "private", "protocol", "public", "return", "self", "static", "struct",
        "switch", "throw", "throws", "true", "try", "var", "weak", "where", "while",
    ],
    comments: &["//"],
    strings: &['"'],
    case_insensitive: false,
};

const SHELL: LangDef = LangDef {
    keywords: &[
        "case", "do", "done", "elif", "else", "esac", "exit", "export", "fi", "for", "function",
        "if", "in", "local", "return", "then", "while",
    ],
    comments: &["#"],
    strings: &['"', '\''],
    case_insensitive: false,
};

const SQL: LangDef = LangDef {
    keywords: &[
        "select", "from", "where", "insert", "into", "values", "update", "set", "delete",
        "create", "table", "index", "alter", "drop", "join", "left", "right", "inner", "outer",
        "on", "as", "and", "or", "not", "null", "in", "exists", "group", "by", "order", "limit",
        "having", "distinct", "union", "all", "case", "when", "then", "else", "end", "begin",
        "commit", "rollback", "primary", "key", "foreign", "references", "default", "unique",
    ],
    comments: &["--"],
    strings: &['\''],
    case_insensitive: true,
};

const CONF: LangDef = LangDef {
    keywords: &["true", "false", "null"],
    comments: &["#"],
    strings: &['"', '\''],
    case_insensitive: false,
};

const PROTO: LangDef = LangDef {
    keywords: &[
        "syntax", "package", "import", "option", "message", "enum", "service", "rpc", "returns",
        "repeated", "optional", "required", "map", "oneof", "reserved", "stream", "true",
        "false", "string", "bool", "bytes", "int32", "int64", "uint32", "uint64", "float",
        "double",
    ],
    comments: &["//"],
    strings: &['"'],
    case_insensitive: false,
};

const CLIKE: LangDef = LangDef {
    keywords: &[
        "auto", "break", "case", "catch", "class", "const", "continue", "default", "delete",
        "do", "else", "enum", "extern", "false", "for", "if", "namespace", "new", "nullptr",
        "private", "protected", "public", "return", "sizeof", "static", "struct", "switch",
        "template", "this", "throw", "true", "try", "typedef", "union", "using", "virtual",
        "void", "while",
    ],
    comments: &["//"],
    strings: &['"', '\''],
    case_insensitive: false,
};

/// Resolve a language from a file path or a fence tag.
pub fn detect(path_or_tag: &str) -> Option<&'static LangDef> {
    let tag = path_or_tag.rsplit('/').next().unwrap_or(path_or_tag);
    let ext = tag.rsplit('.').next().unwrap_or(tag).to_lowercase();
    match ext.as_str() {
        "rs" | "rust" => Some(&RUST),
        "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" | "typescript" | "javascript" => Some(&TS),
        "py" | "python" => Some(&PY),
        "go" | "golang" => Some(&GO),
        "swift" => Some(&SWIFT),
        "sh" | "bash" | "zsh" | "shell" => Some(&SHELL),
        "sql" => Some(&SQL),
        "json" | "yaml" | "yml" | "toml" | "conf" | "ini" | "env" => Some(&CONF),
        "proto" => Some(&PROTO),
        "c" | "h" | "cpp" | "hpp" | "cc" | "java" | "kt" | "kts" | "cs" => Some(&CLIKE),
        _ => None,
    }
}

fn is_ident(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Tokenize one line into (text, kind) runs.
pub fn highlight(line: &str, def: &LangDef) -> Vec<(String, Kind)> {
    let chars: Vec<char> = line.chars().collect();
    let mut out: Vec<(String, Kind)> = Vec::new();
    let mut plain = String::new();
    let mut i = 0;

    let flush = |plain: &mut String, out: &mut Vec<(String, Kind)>| {
        if !plain.is_empty() {
            out.push((std::mem::take(plain), Kind::Text));
        }
    };

    'outer: while i < chars.len() {
        // line comments
        for marker in def.comments {
            let m: Vec<char> = marker.chars().collect();
            if chars[i..].starts_with(&m) {
                flush(&mut plain, &mut out);
                out.push((chars[i..].iter().collect(), Kind::Comment));
                break 'outer;
            }
        }
        let c = chars[i];
        // strings
        if def.strings.contains(&c) {
            flush(&mut plain, &mut out);
            let quote = c;
            let mut s = String::from(c);
            i += 1;
            while i < chars.len() {
                s.push(chars[i]);
                if chars[i] == '\\' && i + 1 < chars.len() {
                    s.push(chars[i + 1]);
                    i += 2;
                    continue;
                }
                if chars[i] == quote {
                    i += 1;
                    break;
                }
                i += 1;
            }
            out.push((s, Kind::Str));
            continue;
        }
        // numbers
        if c.is_ascii_digit() {
            flush(&mut plain, &mut out);
            let mut s = String::new();
            while i < chars.len()
                && (chars[i].is_ascii_alphanumeric() || chars[i] == '.' || chars[i] == '_')
            {
                s.push(chars[i]);
                i += 1;
            }
            out.push((s, Kind::Number));
            continue;
        }
        // identifiers
        if c.is_alphabetic() || c == '_' {
            flush(&mut plain, &mut out);
            let mut s = String::new();
            while i < chars.len() && is_ident(chars[i]) {
                s.push(chars[i]);
                i += 1;
            }
            let kw = if def.case_insensitive {
                let lower = s.to_lowercase();
                def.keywords.contains(&lower.as_str())
            } else {
                def.keywords.contains(&s.as_str())
            };
            let next_nonspace = chars[i..].iter().find(|c| !c.is_whitespace());
            let kind = if kw {
                Kind::Keyword
            } else if s.chars().next().is_some_and(|c| c.is_uppercase()) {
                Kind::Type
            } else if next_nonspace == Some(&'(') {
                Kind::Func
            } else {
                Kind::Text
            };
            out.push((s, kind));
            continue;
        }
        plain.push(c);
        i += 1;
    }
    if !plain.is_empty() {
        out.push((plain, Kind::Text));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(line: &str, def: &LangDef) -> Vec<(String, Kind)> {
        highlight(line, def)
    }

    #[test]
    fn rust_tokens() {
        let toks = kinds("let x = foo(\"hi\") + 42; // done", &RUST);
        assert!(toks.contains(&("let".into(), Kind::Keyword)));
        assert!(toks.contains(&("foo".into(), Kind::Func)));
        assert!(toks.contains(&("\"hi\"".into(), Kind::Str)));
        assert!(toks.contains(&("42".into(), Kind::Number)));
        assert!(toks.iter().any(|(t, k)| t.starts_with("// done") && *k == Kind::Comment));
    }

    #[test]
    fn types_and_sql_case() {
        let toks = kinds("impl PrDetail for Thing", &RUST);
        assert!(toks.contains(&("PrDetail".into(), Kind::Type)));
        let toks = kinds("SELECT id FROM users", &SQL);
        assert!(toks.contains(&("SELECT".into(), Kind::Keyword)));
        assert!(toks.contains(&("FROM".into(), Kind::Keyword)));
    }

    #[test]
    fn detection() {
        assert!(detect("server/modules/UseCase.ts").is_some());
        assert!(detect("src/main.rs").is_some());
        assert!(detect("rust").is_some());
        assert!(detect("Makefile").is_none());
    }

    #[test]
    fn comment_marker_inside_string_stays_string() {
        let toks = kinds("x = \"http://a\"", &RUST);
        assert!(toks.contains(&("\"http://a\"".into(), Kind::Str)));
        assert!(!toks.iter().any(|(_, k)| *k == Kind::Comment));
    }
}
