//! A small syntax highlighter for code blocks and diffs: keywords, strings,
//! numbers, comments, types and function calls, per line, for the languages
//! Monaged's projects use most. Colours are theme roles, so every theme keeps
//! its own quiet palette (red is never used: it means "needs you").

use std::ops::Range;

use gpui::{HighlightStyle, SharedString, StyledText, rgb};

use crate::theme;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Lang {
    Rust,
    Python,
    Js,
    Go,
    CLike,
    Json,
    Shell,
    Toml,
    Yaml,
    Sql,
    Html,
    Css,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Token {
    Keyword,
    Str,
    Number,
    Comment,
    Type,
    Function,
}

pub fn lang_from_name(name: &str) -> Option<Lang> {
    Some(match name.trim().to_lowercase().as_str() {
        "rust" | "rs" => Lang::Rust,
        "python" | "py" => Lang::Python,
        "js" | "jsx" | "ts" | "tsx" | "javascript" | "typescript" | "mjs" | "cjs" => Lang::Js,
        "go" | "golang" => Lang::Go,
        "c" | "h" | "cpp" | "cc" | "hpp" | "java" | "cs" | "csharp" | "kt" | "kotlin" | "swift" | "dart" => Lang::CLike,
        "json" | "jsonc" => Lang::Json,
        "sh" | "bash" | "zsh" | "shell" | "console" | "ps1" | "powershell" => Lang::Shell,
        "toml" => Lang::Toml,
        "yaml" | "yml" => Lang::Yaml,
        "sql" => Lang::Sql,
        "html" | "htm" | "xml" | "svg" | "vue" => Lang::Html,
        "css" | "scss" | "less" => Lang::Css,
        _ => return None,
    })
}

/// From a file path's extension (or name, for Dockerfile-like files).
pub fn lang_from_path(path: &str) -> Option<Lang> {
    let name = path.rsplit('/').next().unwrap_or(path);
    if name == "Makefile" || name.ends_with(".mk") {
        return Some(Lang::Shell);
    }
    name.rsplit_once('.').and_then(|(_, ext)| lang_from_name(ext))
}

fn keywords(lang: Lang) -> &'static [&'static str] {
    match lang {
        Lang::Rust => &[
            "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum", "extern", "false", "fn", "for", "if", "impl", "in", "let",
            "loop", "match", "mod", "move", "mut", "pub", "ref", "return", "self", "Self", "static", "struct", "super", "trait", "true", "type", "unsafe",
            "use", "where", "while",
        ],
        Lang::Python => &[
            "and", "as", "assert", "async", "await", "break", "class", "continue", "def", "del", "elif", "else", "except", "False", "finally", "for", "from",
            "global", "if", "import", "in", "is", "lambda", "None", "nonlocal", "not", "or", "pass", "raise", "return", "self", "True", "try", "while", "with",
            "yield",
        ],
        Lang::Js => &[
            "async", "await", "break", "case", "catch", "class", "const", "continue", "default", "delete", "do", "else", "export", "extends", "false",
            "finally", "for", "from", "function", "if", "import", "in", "instanceof", "interface", "let", "new", "null", "of", "return", "static", "super",
            "switch", "this", "throw", "true", "try", "type", "typeof", "undefined", "var", "void", "while", "yield",
        ],
        Lang::Go => &[
            "break", "case", "chan", "const", "continue", "default", "defer", "else", "fallthrough", "false", "for", "func", "go", "goto", "if", "import",
            "interface", "map", "nil", "package", "range", "return", "select", "struct", "switch", "true", "type", "var",
        ],
        Lang::CLike => &[
            "auto", "bool", "break", "case", "catch", "char", "class", "const", "continue", "default", "delete", "do", "double", "else", "enum", "extern",
            "false", "final", "float", "for", "fun", "if", "import", "int", "interface", "let", "long", "namespace", "new", "null", "override", "package",
            "private", "protected", "public", "return", "short", "static", "struct", "switch", "this", "throw", "true", "try", "typedef", "using", "val",
            "var", "void", "while",
        ],
        Lang::Shell => &[
            "if", "then", "else", "elif", "fi", "for", "in", "do", "done", "while", "case", "esac", "function", "return", "export", "local", "echo", "cd",
            "exit", "set", "unset", "source",
        ],
        Lang::Sql => &[
            "select", "from", "where", "insert", "into", "values", "update", "set", "delete", "create", "table", "index", "on", "join", "left", "right",
            "inner", "outer", "group", "by", "order", "limit", "and", "or", "not", "null", "primary", "key", "references", "as", "distinct", "having",
            "union", "all", "default", "integer", "text", "real",
        ],
        Lang::Json | Lang::Toml | Lang::Yaml => &["true", "false", "null"],
        Lang::Html | Lang::Css => &[],
    }
}

fn line_comment(lang: Lang) -> &'static [&'static str] {
    match lang {
        Lang::Python | Lang::Shell | Lang::Toml | Lang::Yaml => &["#"],
        Lang::Sql => &["--"],
        Lang::Json | Lang::Html => &[],
        _ => &["//"],
    }
}

fn tokens(line: &str, lang: Lang) -> Vec<(Range<usize>, Token)> {
    let mut out = Vec::new();
    let bytes = line.as_bytes();
    let keywords = keywords(lang);
    let case_insensitive = lang == Lang::Sql;
    let mut i = 0;
    while i < bytes.len() {
        let rest = &line[i..];
        // Comments run to the end of the line.
        if line_comment(lang).iter().any(|c| rest.starts_with(c)) || (matches!(lang, Lang::Html) && rest.starts_with("<!--")) || (matches!(lang, Lang::Css | Lang::CLike | Lang::Js | Lang::Rust | Lang::Go) && rest.starts_with("/*")) {
            out.push((i..line.len(), Token::Comment));
            break;
        }
        let c = bytes[i] as char;
        if c == '"' || c == '\'' || (c == '`' && matches!(lang, Lang::Js | Lang::Go | Lang::Shell)) {
            // Rust lifetimes ('a) aren't strings.
            if c == '\'' && lang == Lang::Rust && rest.len() > 2 && rest.as_bytes()[2] != b'\'' && !rest[1..].starts_with('\\') {
                i += 1;
                continue;
            }
            let mut j = i + 1;
            while j < bytes.len() {
                if bytes[j] == b'\\' {
                    j += 2;
                    continue;
                }
                if bytes[j] as char == c {
                    j += 1;
                    break;
                }
                j += 1;
            }
            let end = j.min(bytes.len());
            out.push((i..end, Token::Str));
            i = end;
            continue;
        }
        if c.is_ascii_digit() && (i == 0 || !(bytes[i - 1] as char).is_alphanumeric() && bytes[i - 1] != b'_') {
            let mut j = i;
            while j < bytes.len() && ((bytes[j] as char).is_ascii_alphanumeric() || bytes[j] == b'.' || bytes[j] == b'_') {
                j += 1;
            }
            out.push((i..j, Token::Number));
            i = j;
            continue;
        }
        if c.is_alphabetic() || c == '_' || (c == '$' && lang == Lang::Shell) {
            let mut j = i + 1;
            while j < bytes.len() && ((bytes[j] as char).is_alphanumeric() || bytes[j] == b'_' || bytes[j] == b'-' && matches!(lang, Lang::Css | Lang::Yaml | Lang::Toml)) {
                j += 1;
            }
            // Multi-byte letters: move to a char boundary.
            while j < line.len() && !line.is_char_boundary(j) {
                j += 1;
            }
            let word = &line[i..j];
            let is_keyword = if case_insensitive { keywords.iter().any(|k| k.eq_ignore_ascii_case(word)) } else { keywords.contains(&word) };
            let next = line[j..].trim_start().chars().next();
            if is_keyword {
                out.push((i..j, Token::Keyword));
            } else if matches!(lang, Lang::Json | Lang::Yaml | Lang::Toml) && matches!(next, Some(':') | Some('=')) {
                out.push((i..j, Token::Type));
            } else if next == Some('(') || (lang == Lang::Rust && next == Some('!')) {
                out.push((i..j, Token::Function));
            } else if word.chars().next().is_some_and(char::is_uppercase) && !matches!(lang, Lang::Sql | Lang::Shell | Lang::Html) {
                out.push((i..j, Token::Type));
            }
            i = j;
            continue;
        }
        // Tags in HTML/XML.
        if lang == Lang::Html && c == '<' {
            let mut j = i + 1;
            if j < bytes.len() && bytes[j] == b'/' {
                j += 1;
            }
            let start = j;
            while j < bytes.len() && ((bytes[j] as char).is_alphanumeric() || bytes[j] == b'-') {
                j += 1;
            }
            if j > start {
                out.push((start..j, Token::Keyword));
            }
            i = j.max(i + 1);
            continue;
        }
        i += c.len_utf8();
        while i < line.len() && !line.is_char_boundary(i) {
            i += 1;
        }
    }
    out
}

fn style(token: Token) -> HighlightStyle {
    let color = match token {
        Token::Keyword => theme::syn_keyword(),
        Token::Str => theme::syn_string(),
        Token::Number => theme::syn_number(),
        Token::Comment => theme::syn_comment(),
        Token::Type => theme::syn_type(),
        Token::Function => theme::syn_function(),
    };
    let mut h = HighlightStyle { color: Some(rgb(color).into()), ..Default::default() };
    if token == Token::Comment {
        h.font_style = Some(gpui::FontStyle::Italic);
    }
    h
}

/// One line of code, highlighted when the language is known.
pub fn line(text: &str, lang: Option<Lang>) -> StyledText {
    let text: SharedString = if text.is_empty() { " ".into() } else { text.replace('\t', "    ").into() };
    let Some(lang) = lang else { return StyledText::new(text) };
    let highlights: Vec<(Range<usize>, HighlightStyle)> = tokens(&text, lang).into_iter().filter(|(r, _)| r.start < r.end).map(|(r, t)| (r, style(t))).collect();
    StyledText::new(text).with_highlights(highlights)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_tokens() {
        let t = tokens("let x = \"hi\"; // note", Lang::Rust);
        assert!(t.iter().any(|(r, k)| *k == Token::Keyword && *r == (0..3)));
        assert!(t.iter().any(|(_, k)| *k == Token::Str));
        assert!(t.iter().any(|(_, k)| *k == Token::Comment));
        let p = tokens("def total(items):  # sum", Lang::Python);
        assert_eq!(p[0], (0..3, Token::Keyword));
        assert_eq!(p[1], (4..9, Token::Function));
        assert_eq!(lang_from_path("src/app/main.tsx"), Some(Lang::Js));
        assert_eq!(lang_from_path("README"), None);
    }

    #[test]
    fn never_splits_characters() {
        // Non-ASCII text must not panic or cut a character.
        let t = tokens("naïve = 'café' # ok", Lang::Python);
        for (r, _) in t {
            assert!("naïve = 'café' # ok".is_char_boundary(r.start));
            assert!("naïve = 'café' # ok".is_char_boundary(r.end));
        }
    }
}
