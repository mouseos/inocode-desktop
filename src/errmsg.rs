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

/// The port named in a "cannot listen on the callback port ..." error.
fn port_of(e: &str) -> Option<String> {
    let mut run = String::new();
    let mut runs: Vec<String> = Vec::new();
    for c in e.chars().chain(std::iter::once(' ')) {
        if c.is_ascii_digit() {
            run.push(c);
        } else if !run.is_empty() {
            runs.push(std::mem::take(&mut run));
        }
    }
    runs.into_iter().find(|r| (4..=5).contains(&r.len()) && r != "10048")
}

/// The callback port of the sign-in is taken (another sign-in still listens).
pub(crate) fn port_in_use(e: &str) -> bool {
    let l = e.to_lowercase();
    l.contains("10048") || l.contains("callback port") || l.contains("address already in use")
}

/// A sign-in error as one line.
pub(crate) fn login_short(e: &str) -> String {
    let l = e.to_lowercase();
    if port_in_use(e) {
        return match port_of(e) {
            Some(p) => format!("ログインできませんでした: ポート {p} が使われています。コードを貼り付けてログインしてください"),
            None => "ログインできませんでした: ポートが使われています。コードを貼り付けてログインしてください".to_string(),
        };
    }
    if l.contains("state did not match") {
        return "ログインできませんでした: ログインの確認が一致しませんでした。もう一度試してください".to_string();
    }
    format!("ログインできませんでした: {}", short(e))
}
