//! Syntax colours for code blocks: Rust, Python, JS/TS, JSON, TOML, shell and diff. A small
//! scanner per language family (comments, strings, numbers, keywords), not a full grammar:
//! it has to be fast and must never fail, also on code that is cut off while streaming.

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Tok {
    Plain,
    Keyword,
    Str,
    Comment,
    Number,
    Type,
    Add,
    Del,
    Meta,
}

impl Tok {
    /// Colours that read on both the light and the dark page.
    pub fn color(self) -> Option<&'static str> {
        match self {
            Tok::Plain => None,
            Tok::Keyword => Some("#b062c8"),
            Tok::Str => Some("#c4743f"),
            Tok::Comment => Some("#7f8c7d"),
            Tok::Number => Some("#3f8fc4"),
            Tok::Type => Some("#2f9e8f"),
            Tok::Add => Some("#2ea043"),
            Tok::Del => Some("#e5484d"),
            Tok::Meta => Some("#7a8bd0"),
        }
    }
}

struct Lang {
    line_comments: &'static [&'static str],
    block_comment: Option<(&'static str, &'static str)>,
    /// Quote characters of strings (a triple quote of the same character is a multi-line string).
    quotes: &'static [char],
    keywords: &'static [&'static str],
    /// A capitalised word is a type name.
    types: bool,
    /// `$name` is a variable (shell).
    dollar_vars: bool,
    /// `'x` is a lifetime, not a string (Rust).
    lifetimes: bool,
}

const RUST: Lang = Lang {
    line_comments: &["//"],
    block_comment: Some(("/*", "*/")),
    quotes: &['"', '\''],
    keywords: &[
        "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum", "extern", "false", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod",
        "move", "mut", "pub", "ref", "return", "self", "Self", "static", "struct", "super", "trait", "true", "type", "unsafe", "use", "where", "while",
    ],
    types: true,
    dollar_vars: false,
    lifetimes: true,
};

const PYTHON: Lang = Lang {
    line_comments: &["#"],
    block_comment: None,
    quotes: &['"', '\''],
    keywords: &[
        "and", "as", "assert", "async", "await", "break", "class", "continue", "def", "del", "elif", "else", "except", "False", "finally", "for", "from", "global", "if", "import",
        "in", "is", "lambda", "None", "nonlocal", "not", "or", "pass", "raise", "return", "True", "try", "while", "with", "yield", "self",
    ],
    types: true,
    dollar_vars: false,
    lifetimes: false,
};

const JS: Lang = Lang {
    line_comments: &["//"],
    block_comment: Some(("/*", "*/")),
    quotes: &['"', '\'', '`'],
    keywords: &[
        "async", "await", "break", "case", "catch", "class", "const", "continue", "default", "delete", "do", "else", "enum", "export", "extends", "false", "finally", "for", "from",
        "function", "if", "implements", "import", "in", "instanceof", "interface", "let", "new", "null", "of", "private", "public", "readonly", "return", "static", "super", "switch",
        "this", "throw", "true", "try", "type", "typeof", "undefined", "var", "void", "while", "yield",
    ],
    types: true,
    dollar_vars: false,
    lifetimes: false,
};

const JSON: Lang = Lang {
    line_comments: &[],
    block_comment: None,
    quotes: &['"'],
    keywords: &["true", "false", "null"],
    types: false,
    dollar_vars: false,
    lifetimes: false,
};

const TOML: Lang = Lang {
    line_comments: &["#"],
    block_comment: None,
    quotes: &['"', '\''],
    keywords: &["true", "false"],
    types: false,
    dollar_vars: false,
    lifetimes: false,
};

const SHELL: Lang = Lang {
    line_comments: &["#"],
    block_comment: None,
    quotes: &['"', '\''],
    keywords: &[
        "if", "then", "else", "elif", "fi", "for", "while", "until", "do", "done", "case", "esac", "in", "function", "return", "exit", "export", "local", "set", "unset", "echo",
        "cd", "source", "sudo", "Write-Host", "param",
    ],
    types: false,
    dollar_vars: true,
    lifetimes: false,
};

fn lang_of(name: &str) -> Option<&'static Lang> {
    match name.to_ascii_lowercase().as_str() {
        "rust" | "rs" => Some(&RUST),
        "python" | "py" | "python3" => Some(&PYTHON),
        "js" | "javascript" | "jsx" | "ts" | "typescript" | "tsx" | "mjs" | "cjs" => Some(&JS),
        "json" | "jsonc" | "json5" => Some(&JSON),
        "toml" | "ini" | "yaml" | "yml" => Some(&TOML),
        "sh" | "bash" | "shell" | "zsh" | "console" | "powershell" | "ps1" | "pwsh" | "bat" | "cmd" => Some(&SHELL),
        _ => None,
    }
}

fn is_ident_start(c: char) -> bool {
    c.is_alphabetic() || c == '_'
}

fn is_ident(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// The tokens of `code` (concatenated they are `code`), without the line structure.
fn scan(lang: &Lang, code: &str) -> Vec<(String, Tok)> {
    let chars: Vec<char> = code.chars().collect();
    let mut out: Vec<(String, Tok)> = Vec::new();
    let mut push = |s: String, t: Tok| {
        if s.is_empty() {
            return;
        }
        if let Some(last) = out.last_mut() {
            if last.1 == t {
                last.0.push_str(&s);
                return;
            }
        }
        out.push((s, t));
    };
    let starts = |chars: &[char], i: usize, pat: &str| -> bool {
        let p: Vec<char> = pat.chars().collect();
        !p.is_empty() && chars.len() >= i + p.len() && chars[i..i + p.len()] == p[..]
    };
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        // comments
        if let Some(lc) = lang.line_comments.iter().find(|p| starts(&chars, i, p)) {
            let _ = lc;
            let mut j = i;
            while j < chars.len() && chars[j] != '\n' {
                j += 1;
            }
            push(chars[i..j].iter().collect(), Tok::Comment);
            i = j;
            continue;
        }
        if let Some((open, close)) = lang.block_comment {
            if starts(&chars, i, open) {
                let mut j = i + open.chars().count();
                while j < chars.len() && !starts(&chars, j, close) {
                    j += 1;
                }
                j = (j + close.chars().count()).min(chars.len());
                push(chars[i..j].iter().collect(), Tok::Comment);
                i = j;
                continue;
            }
        }
        // strings (an unterminated one runs to the end of the line, or of the code for a
        // triple-quoted one: a block that is cut off while streaming stays one colour)
        if lang.quotes.contains(&c) {
            if c == '\'' && lang.lifetimes {
                // 'a is a lifetime; 'a' and '\n' are characters
                let is_char = chars.get(i + 2) == Some(&'\'') || chars.get(i + 1) == Some(&'\\');
                if !is_char {
                    let mut j = i + 1;
                    while j < chars.len() && is_ident(chars[j]) {
                        j += 1;
                    }
                    push(chars[i..j].iter().collect(), Tok::Type);
                    i = j;
                    continue;
                }
            }
            let triple = chars.get(i + 1) == Some(&c) && chars.get(i + 2) == Some(&c) && (c == '"' || c == '\'');
            let mut j = i + if triple { 3 } else { 1 };
            while j < chars.len() {
                if chars[j] == '\\' && !triple {
                    j += 2;
                    continue;
                }
                if triple {
                    if chars[j] == c && chars.get(j + 1) == Some(&c) && chars.get(j + 2) == Some(&c) {
                        j += 3;
                        break;
                    }
                } else if chars[j] == c {
                    j += 1;
                    break;
                } else if chars[j] == '\n' && c != '`' {
                    break;
                }
                j += 1;
            }
            let j = j.min(chars.len());
            push(chars[i..j].iter().collect(), Tok::Str);
            i = j;
            continue;
        }
        if lang.dollar_vars && c == '$' {
            let mut j = i + 1;
            if chars.get(j) == Some(&'{') {
                while j < chars.len() && chars[j] != '}' && chars[j] != '\n' {
                    j += 1;
                }
                j = (j + 1).min(chars.len());
            } else {
                while j < chars.len() && is_ident(chars[j]) {
                    j += 1;
                }
            }
            push(chars[i..j].iter().collect(), Tok::Type);
            i = j;
            continue;
        }
        if c.is_ascii_digit() {
            let mut j = i + 1;
            while j < chars.len() && (chars[j].is_ascii_alphanumeric() || chars[j] == '.' || chars[j] == '_') {
                j += 1;
            }
            push(chars[i..j].iter().collect(), Tok::Number);
            i = j;
            continue;
        }
        if is_ident_start(c) {
            let mut j = i + 1;
            while j < chars.len() && (is_ident(chars[j]) || (lang.dollar_vars && chars[j] == '-')) {
                j += 1;
            }
            let word: String = chars[i..j].iter().collect();
            let tok = if lang.keywords.contains(&word.as_str()) {
                Tok::Keyword
            } else if lang.types && word.chars().next().map(|c| c.is_uppercase()).unwrap_or(false) {
                Tok::Type
            } else {
                Tok::Plain
            };
            push(word, tok);
            i = j;
            continue;
        }
        push(c.to_string(), Tok::Plain);
        i += 1;
    }
    out
}

/// Split tokens at line breaks: one list of tokens per line.
fn by_lines(tokens: Vec<(String, Tok)>) -> Vec<Vec<(String, Tok)>> {
    let mut lines: Vec<Vec<(String, Tok)>> = vec![Vec::new()];
    for (text, tok) in tokens {
        let mut parts = text.split('\n');
        if let Some(first) = parts.next() {
            if !first.is_empty() {
                lines.last_mut().unwrap().push((first.to_string(), tok));
            }
        }
        for part in parts {
            lines.push(Vec::new());
            if !part.is_empty() {
                lines.last_mut().unwrap().push((part.to_string(), tok));
            }
        }
    }
    lines
}

fn diff_lines(code: &str) -> Vec<Vec<(String, Tok)>> {
    code.split('\n')
        .map(|l| {
            let tok = if l.starts_with("+++") || l.starts_with("---") || l.starts_with("@@") || l.starts_with("diff ") || l.starts_with("index ") {
                Tok::Meta
            } else if l.starts_with('+') {
                Tok::Add
            } else if l.starts_with('-') {
                Tok::Del
            } else {
                Tok::Plain
            };
            if l.is_empty() {
                vec![]
            } else {
                vec![(l.to_string(), tok)]
            }
        })
        .collect()
}

/// One entry per line of `code`; each line is a list of (text, kind).
pub fn highlight(lang: &str, code: &str) -> Vec<Vec<(String, Tok)>> {
    let code = code.strip_suffix('\n').unwrap_or(code);
    let name = lang.to_ascii_lowercase();
    if name == "diff" || name == "patch" {
        return diff_lines(code);
    }
    let Some(l) = lang_of(&name) else {
        return code.split('\n').map(|l| if l.is_empty() { vec![] } else { vec![(l.to_string(), Tok::Plain)] }).collect();
    };
    let mut lines = by_lines(scan(l, code));
    // TOML section headers: "[table]" lines
    if name == "toml" {
        let src: Vec<&str> = code.split('\n').collect();
        for (i, line) in lines.iter_mut().enumerate() {
            if src.get(i).map(|s| s.trim_start().starts_with('[')).unwrap_or(false) {
                let text: String = line.iter().map(|t| t.0.as_str()).collect();
                *line = vec![(text, Tok::Type)];
            }
        }
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(lang: &str, code: &str) -> Vec<(String, Tok)> {
        highlight(lang, code).into_iter().flatten().collect()
    }

    #[test]
    fn rust_keywords_strings_comments_and_numbers() {
        let t = kinds("rust", "fn main() { let x = 42; // hi\n println!(\"a\"); }");
        assert!(t.contains(&("fn".to_string(), Tok::Keyword)));
        assert!(t.contains(&("42".to_string(), Tok::Number)));
        assert!(t.contains(&("// hi".to_string(), Tok::Comment)));
        assert!(t.contains(&("\"a\"".to_string(), Tok::Str)));
        // a lifetime is not a string
        let l = kinds("rust", "fn f<'a>(x: &'a str) {}");
        assert!(l.iter().all(|(_, k)| *k != Tok::Str));
    }

    #[test]
    fn concatenated_tokens_are_the_code() {
        let code = "def f(x):\n    \"\"\"doc\n    more\"\"\"\n    return x  # c\n";
        for lang in ["python", "rust", "js", "json", "toml", "sh", "diff", "unknown"] {
            let lines = highlight(lang, code);
            let joined: Vec<String> = lines.iter().map(|l| l.iter().map(|t| t.0.as_str()).collect::<String>()).collect();
            assert_eq!(joined.join("\n"), code.trim_end_matches('\n'), "{lang}");
        }
    }

    #[test]
    fn a_multi_line_string_stays_one_colour_per_line() {
        let lines = highlight("python", "x = \"\"\"a\nb\"\"\"\ny = 1");
        assert_eq!(lines.len(), 3);
        assert!(lines[1].iter().all(|t| t.1 == Tok::Str));
        assert!(lines[2].iter().any(|t| t.1 == Tok::Number));
    }

    #[test]
    fn code_cut_off_while_streaming_does_not_fail() {
        for cut in ["\"abc", "/* open", "'", "\"\"\"x", "$", "${x"] {
            let _ = highlight("rust", cut);
            let _ = highlight("sh", cut);
            let _ = highlight("python", cut);
        }
    }

    #[test]
    fn diff_lines_are_coloured_by_their_first_character() {
        let l = highlight("diff", "--- a\n+++ b\n@@ -1 +1 @@\n-old\n+new\n same");
        let first: Vec<Tok> = l.iter().map(|line| line.first().map(|t| t.1).unwrap_or(Tok::Plain)).collect();
        assert_eq!(first, vec![Tok::Meta, Tok::Meta, Tok::Meta, Tok::Del, Tok::Add, Tok::Plain]);
    }

    #[test]
    fn json_and_toml_and_shell() {
        let j = kinds("json", "{\"a\": [1, true, null]}");
        assert!(j.contains(&("\"a\"".to_string(), Tok::Str)) && j.contains(&("true".to_string(), Tok::Keyword)));
        let t = kinds("toml", "[package]\nname = \"x\" # c");
        assert!(t.contains(&("[package]".to_string(), Tok::Type)));
        let s = kinds("sh", "if [ -n \"$HOME\" ]; then echo $HOME; fi");
        assert!(s.contains(&("if".to_string(), Tok::Keyword)) && s.contains(&("$HOME".to_string(), Tok::Type)));
    }
}
