//! Server errors for the screen: one short line in Japanese; the whole text is kept for a
//! "show details" view.

/// The server does not know the method (an older `inocode.exe`).
pub(crate) fn unsupported(e: &str) -> bool {
    let l = e.to_lowercase();
    l.contains("unknown variant") || l.contains("-32601") || l.contains("method not found")
}

/// The method name the server did not know, from "unknown variant `inocode/accounts/list`, ...".
pub(crate) fn method_of(e: &str) -> Option<String> {
    let a = e.find('`')? + 1;
    let b = e[a..].find('`')? + a;
    Some(e[a..b].to_string())
}

pub(crate) const OLD_SERVER: &str = "サーバーが未対応です。inocode.exe を新しいビルドにしてください";

/// One line: the first sentence of the error, at most 120 characters.
pub(crate) fn short(e: &str) -> String {
    if unsupported(e) {
        return match method_of(e) {
            Some(m) => format!("サーバーが、この機能({m})にまだ対応していません"),
            None => OLD_SERVER.to_string(),
        };
    }
    let line = e.lines().next().unwrap_or("").trim();
    let first = line.split('。').next().unwrap_or(line);
    let first = first.split(". ").next().unwrap_or(first);
    let mut out: String = first.chars().take(120).collect();
    if first.chars().count() > 120 {
        out.push('…');
    }
    out
}
