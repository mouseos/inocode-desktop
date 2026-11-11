//! A unified diff as the rows of the chat: one heading per file (its name, lines added and
//! removed), then the lines with their numbers; the `diff --git`, `index`, `---` and `+++` lines
//! are not shown, and a gap between two hunks is a thin separator.

use crate::*;

/// Rows shown at most (a longer diff ends with a note).
const MAX_ROWS: usize = 600;

fn row(kind: i32, num: &str, text: &str) -> DiffRow {
    DiffRow { kind, num: num.into(), text: text.into(), added: "".into(), removed: "".into() }
}

/// The rows of a diff: kind 0 context, 1 added, 2 removed, 3 file heading, 4 gap between hunks,
/// 5 note.
pub(crate) fn rows(diff: &str) -> Vec<DiffRow> {
    let mut out: Vec<DiffRow> = Vec::new();
    // Index of the heading of the file being read, and its counts.
    let mut head: Option<usize> = None;
    let (mut added, mut removed) = (0usize, 0usize);
    let (mut old, mut new) = (0i64, 0i64);
    let mut in_header = false;
    let mut hunks = 0usize;
    let finish = |out: &mut Vec<DiffRow>, head: Option<usize>, added: usize, removed: usize| {
        if let Some(i) = head {
            out[i].added = format!("+{added}").into();
            out[i].removed = format!("\u{2212}{removed}").into();
        }
    };
    let mut cut = 0usize;
    for line in diff.lines() {
        if let Some(rest) = line.strip_prefix("diff --git ") {
            finish(&mut out, head, added, removed);
            // "a/<path> b/<path>": the name is the b side.
            let name = rest.rsplit_once(" b/").map(|(_, b)| b).unwrap_or(rest).trim();
            out.push(row(3, "", name));
            head = Some(out.len() - 1);
            (added, removed, old, new, hunks) = (0, 0, 0, 0, 0);
            in_header = true;
            continue;
        }
        if in_header {
            if let Some(h) = line.strip_prefix("@@") {
                in_header = false;
                (old, new) = hunk_start(h);
                hunks += 1;
                continue;
            }
            // index, mode, rename, ---, +++ and the like.
            continue;
        }
        if let Some(h) = line.strip_prefix("@@") {
            (old, new) = hunk_start(h);
            hunks += 1;
            if hunks > 1 {
                out.push(row(4, "", ""));
            }
            continue;
        }
        if line.starts_with('\\') {
            continue;
        }
        if out.len() >= MAX_ROWS {
            cut += 1;
            if line.starts_with('+') {
                added += 1;
            } else if line.starts_with('-') {
                removed += 1;
            }
            continue;
        }
        if let Some(t) = line.strip_prefix('+') {
            out.push(row(1, &new.to_string(), t));
            new += 1;
            added += 1;
        } else if let Some(t) = line.strip_prefix('-') {
            out.push(row(2, &old.to_string(), t));
            old += 1;
            removed += 1;
        } else {
            let t = line.strip_prefix(' ').unwrap_or(line);
            out.push(row(0, &new.to_string(), t));
            new += 1;
            old += 1;
        }
    }
    finish(&mut out, head, added, removed);
    if cut > 0 {
        out.push(row(5, "", &format!("以降 {cut} 行を省略")));
    }
    out
}

/// "@@ -a,b +c,d @@" -> (a, c).
fn hunk_start(h: &str) -> (i64, i64) {
    let mut old = 0;
    let mut new = 0;
    for part in h.split_whitespace() {
        let num = |p: &str| p.split(',').next().and_then(|n| n.parse::<i64>().ok()).unwrap_or(0);
        if let Some(p) = part.strip_prefix('-') {
            old = num(p);
        } else if let Some(p) = part.strip_prefix('+') {
            new = num(p);
        }
    }
    (old, new)
}

/// Whether a row's body is a diff that these rows can show.
pub(crate) fn is_diff(text: &str) -> bool {
    text.starts_with("diff --git ")
}
