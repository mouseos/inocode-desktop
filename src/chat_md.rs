//! The answers of the assistant as markdown blocks on the screen: converts the rows of
//! markdown.rs into the screen's rows, keeps them up to date while an answer is written (only the
//! last, changing part is redone), opens links and copies code.

use crate::markdown::{self, Cache, Kind, Row};
use crate::state::STATE;
use crate::view::messages_with;
use crate::*;
use slint::{Model, StyledText};
use std::cell::{Cell, RefCell};
use std::time::{Duration, Instant};

/// An answer being written is drawn again at most this often.
const SYNC_EVERY: Duration = Duration::from_millis(80);

thread_local! {
    static STREAM: RefCell<Cache> = RefCell::new(Cache::default());
    static LAST_SYNC: Cell<Option<Instant>> = const { Cell::new(None) };
    static DEMO_STARTED: Cell<bool> = const { Cell::new(false) };
    static DEMO_TIMERS: RefCell<Vec<slint::Timer>> = const { RefCell::new(Vec::new()) };
}

fn styled(md: &str) -> StyledText {
    StyledText::from_markdown(md).unwrap_or_else(|_| StyledText::from_plain_text(md))
}

pub(crate) fn to_md_row(r: &Row) -> MdRow {
    let kind = match r.kind {
        Kind::Paragraph => 0,
        Kind::Heading => 1,
        Kind::Item => 2,
        Kind::Code => 3,
        Kind::Table => 4,
        Kind::Rule => 5,
    };
    let text = match r.kind {
        // headings are bold (StyledText has no weight of its own)
        Kind::Heading => styled(&format!("**{}**", r.md)),
        _ => styled(&r.md),
    };
    let lines: Vec<MdLine> = if r.kind == Kind::Code {
        crate::highlight::highlight(&r.lang, &r.code)
            .iter()
            .map(|l| {
                let plain: String = l.iter().map(|t| t.0.replace(' ', "\u{a0}")).collect();
                let md = markdown::code_line_md(l);
                MdLine { text: StyledText::from_markdown(&md).unwrap_or_else(|_| StyledText::from_plain_text(&plain)) }
            })
            .collect()
    } else {
        Vec::new()
    };
    let cells: Vec<StyledText> =
        r.cells.iter().map(|c| if r.level == 1 { styled(&format!("**{c}**")) } else { styled(c) }).collect();
    MdRow {
        kind,
        level: r.level,
        depth: r.depth,
        quote: r.quote,
        marker: r.marker.as_str().into(),
        text,
        lang: r.lang.as_str().into(),
        code: r.code.as_str().into(),
        lines: ModelRc::from(Rc::new(VecModel::from(lines))),
        cells: ModelRc::from(Rc::new(VecModel::from(cells))),
        est_lines: markdown::est_lines(&r.cells),
    }
}

/// The blocks of a whole answer (history).
pub(crate) fn blocks_of(text: &str) -> ModelRc<MdRow> {
    let rows: Vec<MdRow> = markdown::parse_rows(text).iter().map(to_md_row).collect();
    ModelRc::from(Rc::new(VecModel::from(rows)))
}

/// A new answer starts (or the conversation was replaced).
pub(crate) fn reset() {
    STREAM.with(|c| *c.borrow_mut() = Cache::default());
    LAST_SYNC.with(|l| l.set(None));
}

/// Bring the blocks of the last answer up to date with its text. While the answer is written
/// this is done every 80 ms; `force` at the end of a text (a tool row, the end of the turn).
pub(crate) fn sync_last(app: &AppWindow, force: bool) {
    let now = Instant::now();
    if !force && LAST_SYNC.with(|l| l.get()).map(|t| now.duration_since(t) < SYNC_EVERY).unwrap_or(false) {
        return;
    }
    messages_with(app, |model| {
        let Some(i) = (0..model.row_count()).rev().find(|i| model.row_data(*i).map(|m| m.role == "assistant").unwrap_or(false)) else { return };
        let Some(mut msg) = model.row_data(i) else { return };
        let text = msg.text.to_string();
        let (from, rows) = STREAM.with(|c| c.borrow_mut().update_tail(&text));
        let blocks = msg.blocks.clone();
        let Some(vec) = blocks.as_any().downcast_ref::<VecModel<MdRow>>() else {
            // the first blocks of this answer: own model for the row
            let all: Vec<MdRow> = STREAM.with(|c| c.borrow_mut().update(&text)).iter().map(to_md_row).collect();
            msg.blocks = ModelRc::from(Rc::new(VecModel::from(all)));
            model.set_row_data(i, msg);
            LAST_SYNC.with(|l| l.set(Some(now)));
            return;
        };
        if vec.row_count() < from {
            // not what was expected: all again
            let all = STREAM.with(|c| c.borrow_mut().update(&text));
            while vec.row_count() > 0 {
                vec.remove(vec.row_count() - 1);
            }
            for r in &all {
                vec.push(to_md_row(r));
            }
        } else {
            // the first `from` rows are final and already there; the others are written again
            for (k, r) in rows.iter().enumerate() {
                let new = to_md_row(r);
                let idx = from + k;
                if idx < vec.row_count() {
                    vec.set_row_data(idx, new);
                } else {
                    vec.push(new);
                }
            }
            while vec.row_count() > from + rows.len() {
                vec.remove(vec.row_count() - 1);
            }
        }
        LAST_SYNC.with(|l| l.set(Some(now)));
    });
}

// ------------------------------------------------------------------ links

#[derive(Debug, PartialEq)]
pub(crate) enum Target {
    Web(String),
    File(String),
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out: Vec<u8> = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Some(v) = s.get(i + 1..i + 3).and_then(|h| u8::from_str_radix(h, 16).ok()) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

/// A clicked link: a web address, or a file (an absolute path, a path under the working folder,
/// `file://`), without a `#L10` or `:10:5` position.
pub(crate) fn link_target(link: &str, cwd: &str) -> Option<Target> {
    let l = link.trim();
    if l.is_empty() {
        return None;
    }
    let lower = l.to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") || lower.starts_with("mailto:") {
        return Some(Target::Web(l.to_string()));
    }
    let mut path = if let Some(rest) = l.strip_prefix("file:///").or_else(|| l.strip_prefix("file://")) {
        let p = percent_decode(rest);
        // file:///C:/x on Windows, file:///home/x elsewhere
        if p.chars().nth(1) == Some(':') { p } else { format!("/{p}") }
    } else if lower.contains("://") {
        return None;
    } else {
        percent_decode(l)
    };
    if let Some(i) = path.find('#') {
        path.truncate(i);
    }
    // a trailing ":line" or ":line:column"
    for _ in 0..2 {
        if let Some(i) = path.rfind(':') {
            let tail = &path[i + 1..];
            if i > 1 && !tail.is_empty() && tail.chars().all(|c| c.is_ascii_digit()) {
                path.truncate(i);
            }
        }
    }
    if path.is_empty() {
        return None;
    }
    let p = std::path::Path::new(&path);
    let full = if p.is_absolute() || path.chars().nth(1) == Some(':') || cwd.is_empty() {
        path
    } else {
        std::path::Path::new(cwd).join(p).to_string_lossy().to_string()
    };
    Some(Target::File(full))
}

fn open_with_default_app(target: &str) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let _ = std::process::Command::new("rundll32").args(["url.dll,FileProtocolHandler", target]).creation_flags(0x0800_0000).spawn();
    }
    #[cfg(not(windows))]
    {
        let _ = std::process::Command::new("xdg-open").arg(target).spawn();
    }
}

fn status(app: &AppWindow, text: &str) {
    app.global::<AppState>().set_status(text.into());
    app.global::<AppState>().set_status_level(2);
}

/// Mock only (INOCODE_GUI_DEMO=md): an answer that is written piece by piece, with every kind of
/// block, so that the screen can be looked at without a server.
fn demo(app: &AppWindow) {
    let sample = "# 見出し 1

通常の段落です。**太字**、*斜体*、~~取り消し~~、`inline_code`、そして [リンク](https://example.com) があります。

## 見出し 2

- 箇条書き A
  - 入れ子 A-1
  - 入れ子 A-2
- 箇条書き B

1. 手順 一
2. 手順 二

> 引用された文章。
> 二行目。

| 名前 | 値 |
|---|---|
| alpha | **1** |
| beta | `2` |

---

```rust
fn main() {
    // コメント
    let x: Vec<u32> = vec![1, 2, 3];
    println!(\"sum = {}\", x.iter().sum::<u32>());
}
```

```diff
--- a/src/main.rs
+++ b/src/main.rs
@@ -1,3 +1,3 @@
-    println!(\"hello\");
+    println!(\"hello, world\");
```

```python
def add(a, b):
    \"\"\"足す\"\"\"
    return a + b  # 結果
```

```json
{ \"name\": \"x\", \"n\": 42, \"ok\": true }
```

最後の段落。";
    let chars: Vec<char> = sample.chars().collect();
    let pos = Rc::new(Cell::new(0usize));
    let weak = app.as_weak();
    let timer = slint::Timer::default();
    let start = std::time::Duration::from_secs(6);
    let t2 = slint::Timer::default();
    t2.start(slint::TimerMode::SingleShot, start, {
        let weak = weak.clone();
        move || {
            if let Some(app) = weak.upgrade() {
                STATE.with(|s| crate::events::apply(&app, s, Event::AssistantStart));
            }
        }
    });
    timer.start(slint::TimerMode::Repeated, std::time::Duration::from_millis(40), move || {
        if pos.get() == 0 && DEMO_STARTED.with(|d| !d.get()) {
            // wait for the answer to be started (6 s)
            let _ = ();
        }
        let Some(app) = weak.upgrade() else { return };
        if !DEMO_STARTED.with(|d| d.get()) {
            return;
        }
        let i = pos.get();
        if i >= chars.len() {
            STATE.with(|s| crate::events::apply(&app, s, Event::TurnDone { interrupted: false }));
            return;
        }
        let j = (i + 7).min(chars.len());
        let piece: String = chars[i..j].iter().collect();
        pos.set(j);
        STATE.with(|s| crate::events::apply(&app, s, Event::Delta(piece)));
    });
    DEMO_TIMERS.with(|t| t.borrow_mut().extend([timer, t2]));
    let _ = DEMO_STARTED.with(|d| d.set(false));
    let weak2 = app.as_weak();
    let t3 = slint::Timer::default();
    t3.start(slint::TimerMode::SingleShot, start, move || {
        if weak2.upgrade().is_some() {
            DEMO_STARTED.with(|d| d.set(true));
        }
    });
    DEMO_TIMERS.with(|t| t.borrow_mut().push(t3));
}

pub(crate) fn register(app: &AppWindow) {
    if std::env::var("INOCODE_GUI_DEMO").map(|v| v == "md").unwrap_or(false) {
        demo(app);
    }
    let weak = app.as_weak();
    app.global::<MdState>().on_link(move |link| {
        let Some(app) = weak.upgrade() else { return };
        let cwd = STATE.with(|s| s.borrow().active_cwd.clone());
        match link_target(&link, &cwd) {
            Some(Target::Web(url)) => open_with_default_app(&url),
            Some(Target::File(path)) => {
                if std::path::Path::new(&path).exists() {
                    open_with_default_app(&path);
                } else {
                    status(&app, &format!("ファイルが見つかりません: {path}"));
                }
            }
            None => {}
        }
    });
    let weak = app.as_weak();
    app.global::<MdState>().on_copy_code(move |code| {
        let Some(app) = weak.upgrade() else { return };
        if let Err(e) = arboard::Clipboard::new().and_then(|mut c| c.set_text(code.to_string())) {
            status(&app, &format!("コピーできませんでした: {e}"));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn web_and_file_links() {
        assert_eq!(link_target("https://example.com/a?b=1#c", ""), Some(Target::Web("https://example.com/a?b=1#c".into())));
        assert_eq!(link_target("mailto:a@b.c", ""), Some(Target::Web("mailto:a@b.c".into())));
        assert_eq!(link_target("D:\\work\\a.rs", "x"), Some(Target::File("D:\\work\\a.rs".into())));
        assert_eq!(link_target("file:///C:/dir/a%20b.txt", ""), Some(Target::File("C:/dir/a b.txt".into())));
        // a position after the path is dropped
        assert_eq!(link_target("D:\\work\\a.rs:12:5", ""), Some(Target::File("D:\\work\\a.rs".into())));
        assert_eq!(link_target("/home/u/a.rs#L10", ""), Some(Target::File("/home/u/a.rs".into())));
        // relative to the working folder
        let rel = link_target("src/main.rs:42", "D:\\proj").unwrap();
        assert!(matches!(&rel, Target::File(p) if p.starts_with("D:\\proj") && p.ends_with("main.rs")), "{rel:?}");
        assert_eq!(link_target("ftp://x", ""), None);
        assert_eq!(link_target("  ", ""), None);
    }

    #[test]
    fn rows_become_screen_rows() {
        let rows = markdown::parse_rows("# Hi\n\n- **a** [l](https://x.y)\n\n```rust\nfn f() {}\n```\n\n| a | b |\n|---|---|\n| 1 | 2 |\n");
        let md: Vec<MdRow> = rows.iter().map(to_md_row).collect();
        let kinds: Vec<i32> = md.iter().map(|r| r.kind).collect();
        assert_eq!(kinds, vec![1, 2, 3, 4, 4]);
        assert_eq!(md[2].lines.row_count(), 1);
        assert_eq!(md[3].cells.row_count(), 2);
        assert_eq!(md[2].code.as_str(), "fn f() {}");
    }
}
