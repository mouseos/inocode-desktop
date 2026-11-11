//! Markdown of the assistant's answers. Slint's own styled text (StyledText) draws only inline
//! styles, paragraphs and lists; headings, code blocks, tables, quotes and rules are not in it.
//! So the blocks are made here (pulldown-cmark) and flattened into rows; the text inside a row is
//! inline markdown that StyledText draws (bold, italic, strikethrough, inline code, links).
//!
//! An answer that is still being written is parsed again at every update, but only from the last
//! stable point: everything before the last blank line (outside a code fence) that is followed by
//! a line that cannot continue the block before it stays as it was.

use crate::highlight::Tok;
use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Kind {
    Paragraph,
    Heading,
    Item,
    Code,
    Table,
    Rule,
}

/// One block of the answer, without any Slint type (tested).
#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub kind: Kind,
    /// Heading level (1-6); table header row: 1.
    pub level: i32,
    /// List nesting (0 = not in a list).
    pub depth: i32,
    /// Block quote nesting.
    pub quote: i32,
    /// "•", "1." ... of the first paragraph of a list item.
    pub marker: String,
    /// Inline markdown of the text (paragraph, heading, item).
    pub md: String,
    pub lang: String,
    /// The code of a code block, as written (what "copy" copies).
    pub code: String,
    /// Inline markdown of the cells of a table row.
    pub cells: Vec<String>,
}

impl Row {
    fn new(kind: Kind) -> Row {
        Row { kind, level: 0, depth: 0, quote: 0, marker: String::new(), md: String::new(), lang: String::new(), code: String::new(), cells: Vec::new() }
    }
}

fn options() -> Options {
    Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS
}

fn level_of(l: HeadingLevel) -> i32 {
    match l {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

fn bullet(depth: usize) -> &'static str {
    match depth {
        0 | 1 => "•",
        2 => "◦",
        _ => "▪",
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Ctx {
    Paragraph,
    Heading(i32),
    Item,
    Cell,
}

/// Blocks of `src` as rows.
pub fn parse_rows(src: &str) -> Vec<Row> {
    let src = src.replace('\u{e541}', "");
    let mut rows: Vec<Row> = Vec::new();
    let mut ctx: Vec<Ctx> = Vec::new();
    // (next number or None for a bullet list)
    let mut lists: Vec<Option<u64>> = Vec::new();
    let mut quote = 0;
    let mut inline: Option<(usize, usize)> = None;
    let mut pending_marker: Option<String> = None;
    let mut code: Option<(String, String)> = None;
    let mut table: Vec<(bool, Vec<String>)> = Vec::new();
    let mut in_table = false;
    let mut head = false;
    let mut cells: Vec<String> = Vec::new();

    let slice = |r: (usize, usize)| src[r.0..r.1].trim().to_string();
    macro_rules! flush {
        () => {
            if let Some(r) = inline.take() {
                let text = slice(r);
                if !text.is_empty() {
                    let in_item = ctx.contains(&Ctx::Item);
                    let heading = ctx.iter().rev().find_map(|c| if let Ctx::Heading(l) = c { Some(*l) } else { None });
                    let mut row = Row::new(if let Some(_) = heading {
                        Kind::Heading
                    } else if in_item {
                        Kind::Item
                    } else {
                        Kind::Paragraph
                    });
                    row.level = heading.unwrap_or(0);
                    row.depth = lists.len() as i32;
                    row.quote = quote;
                    row.md = strip_quote_markers(&text, quote);
                    if in_item {
                        row.marker = pending_marker.take().unwrap_or_default();
                    }
                    rows.push(row);
                }
            }
        };
    }
    let extend = |inline: &mut Option<(usize, usize)>, r: std::ops::Range<usize>| {
        *inline = Some(match *inline {
            Some((s, e)) => (s.min(r.start), e.max(r.end)),
            None => (r.start, r.end),
        });
    };

    for (ev, range) in Parser::new_ext(&src, options()).into_offset_iter() {
        match ev {
            Event::Start(tag) => match tag {
                Tag::Paragraph => {
                    flush!();
                    ctx.push(Ctx::Paragraph);
                }
                Tag::Heading { level, .. } => {
                    flush!();
                    ctx.push(Ctx::Heading(level_of(level)));
                }
                Tag::BlockQuote(_) => {
                    flush!();
                    quote += 1;
                }
                Tag::CodeBlock(kind) => {
                    flush!();
                    let lang = match kind {
                        CodeBlockKind::Fenced(info) => info.split_whitespace().next().unwrap_or("").to_string(),
                        CodeBlockKind::Indented => String::new(),
                    };
                    code = Some((lang, String::new()));
                }
                Tag::List(start) => {
                    flush!();
                    lists.push(start);
                }
                Tag::Item => {
                    flush!();
                    let marker = match lists.last_mut() {
                        Some(Some(n)) => {
                            let m = format!("{n}.");
                            *n += 1;
                            m
                        }
                        _ => bullet(lists.len()).to_string(),
                    };
                    pending_marker = Some(marker);
                    ctx.push(Ctx::Item);
                }
                Tag::Table(_) => {
                    flush!();
                    in_table = true;
                    table.clear();
                }
                Tag::TableHead => {
                    head = true;
                    cells.clear();
                }
                Tag::TableRow => cells.clear(),
                Tag::TableCell => {
                    inline = None;
                    ctx.push(Ctx::Cell);
                }
                // inline tags: the text between is taken from the source
                _ => extend(&mut inline, range.clone()),
            },
            Event::End(end) => match end {
                TagEnd::Paragraph | TagEnd::Heading(_) => {
                    flush!();
                    ctx.pop();
                }
                TagEnd::Item => {
                    flush!();
                    ctx.pop();
                    pending_marker = None;
                }
                TagEnd::List(_) => {
                    flush!();
                    lists.pop();
                }
                TagEnd::BlockQuote(_) => {
                    flush!();
                    quote -= 1;
                }
                TagEnd::CodeBlock => {
                    if let Some((lang, text)) = code.take() {
                        let mut row = Row::new(Kind::Code);
                        row.lang = lang;
                        row.code = text.strip_suffix('\n').unwrap_or(&text).to_string();
                        row.depth = lists.len() as i32;
                        row.quote = quote;
                        rows.push(row);
                    }
                }
                TagEnd::TableCell => {
                    ctx.pop();
                    cells.push(inline.take().map(|r| slice(r)).unwrap_or_default());
                }
                TagEnd::TableHead => {
                    table.push((true, std::mem::take(&mut cells)));
                    head = false;
                }
                TagEnd::TableRow => table.push((false, std::mem::take(&mut cells))),
                TagEnd::Table => {
                    in_table = false;
                    for (header, c) in table.drain(..) {
                        let mut row = Row::new(Kind::Table);
                        row.level = header as i32;
                        row.cells = c;
                        row.depth = lists.len() as i32;
                        row.quote = quote;
                        rows.push(row);
                    }
                }
                _ => {}
            },
            Event::Text(t) => {
                if let Some((_, text)) = code.as_mut() {
                    text.push_str(&t);
                } else {
                    extend(&mut inline, range);
                }
            }
            Event::Code(_) | Event::SoftBreak | Event::HardBreak | Event::InlineHtml(_) | Event::Html(_) | Event::InlineMath(_) | Event::DisplayMath(_) | Event::FootnoteReference(_) => {
                if let Some((_, text)) = code.as_mut() {
                    if let Event::Html(h) = &ev {
                        text.push_str(h);
                    }
                } else {
                    extend(&mut inline, range);
                }
            }
            Event::TaskListMarker(done) => {
                pending_marker = Some(if done { "☑" } else { "☐" }.to_string());
            }
            Event::Rule => {
                flush!();
                let mut row = Row::new(Kind::Rule);
                row.quote = quote;
                row.depth = lists.len() as i32;
                rows.push(row);
            }
        }
    }
    flush!();
    let _ = (in_table, head);
    rows
}

// ------------------------------------------------------------------ streaming

/// Byte offset up to which the text is final: the start of a line that follows a blank line
/// (outside a code fence) and that cannot continue the block before it (it starts at column 0
/// and is not a list item, a quote or a table row that follows a list, a quote or a table).
/// 0 when there is none.
pub fn stable_end(text: &str) -> usize {
    let mut fence: Option<(char, usize)> = None;
    let mut best = 0;
    let mut pos = 0;
    let mut blank_before = false;
    // the block before is a list, a quote or a table (a list item goes on after a blank line)
    let mut prev_listish = false;
    for line in text.split_inclusive('\n') {
        let trimmed = line.trim_end_matches(['\n', '\r']);
        let t = trimmed.trim_start();
        let indent = trimmed.len() - t.len();
        if let Some((c, n)) = fence {
            // a closing fence: the same character, at least as long, nothing else
            if indent <= 3 && t.starts_with(c) && t.chars().take_while(|x| *x == c).count() >= n && t.trim_matches(c).trim().is_empty() {
                fence = None;
            }
            blank_before = false;
        } else if t.is_empty() {
            blank_before = true;
        } else {
            let first = t.chars().next().unwrap_or(' ');
            let opens_fence = indent <= 3 && (t.starts_with("```") || t.starts_with("~~~"));
            let list_like = matches!(first, '-' | '+' | '*' | '>' | '|') || t.chars().take_while(|c| c.is_ascii_digit()).count() > 0 && t.contains(['.', ')']);
            if blank_before && indent == 0 && (!list_like || !prev_listish) {
                best = pos;
            }
            prev_listish = list_like || indent > 0 || (prev_listish && !blank_before);
            if opens_fence {
                let c = first;
                fence = Some((c, t.chars().take_while(|x| *x == c).count()));
            }
            blank_before = false;
        }
        pos += line.len();
    }
    // a blank line inside an unclosed fence is not a boundary: `best` was only set outside fences
    best
}

/// Rows of an answer being written: the rows before the last stable point are kept.
#[derive(Default)]
pub struct Cache {
    stable: usize,
    rows: Vec<Row>,
}

impl Cache {
    /// (n, rows): the rows that were final before this call are the first n rows and have not
    /// changed; `rows` are the rows from there on (the ones that became final now, and the tail).
    pub fn update_tail(&mut self, text: &str) -> (usize, Vec<Row>) {
        // a text that no longer starts with what was cached is another text
        if self.stable > text.len() || !text.is_char_boundary(self.stable) {
            *self = Cache::default();
        }
        let from = self.rows.len();
        let end = stable_end(text);
        if end > self.stable {
            let mut more = parse_rows(&text[self.stable..end]);
            self.rows.append(&mut more);
            self.stable = end;
        }
        let mut rows = self.rows[from..].to_vec();
        rows.extend(parse_rows(&text[self.stable..]));
        (from, rows)
    }

    /// All rows of the text.
    pub fn update(&mut self, text: &str) -> Vec<Row> {
        let (from, rows) = self.update_tail(text);
        let mut all = self.rows[..from].to_vec();
        all.extend(rows);
        all
    }

    #[cfg(test)]
    /// Number of rows that are final (the rest is the tail that is parsed again).
    pub fn stable_rows(&self) -> usize {
        self.rows.len()
    }
}

/// How many lines a table row needs: the longest cell against a share of about 85 columns
/// (a wide character counts two). A guess: the cells are laid out by the screen.
pub fn est_lines(cells: &[String]) -> i32 {
    if cells.is_empty() {
        return 1;
    }
    let share = (85 / cells.len()).max(6);
    cells
        .iter()
        .map(|c| {
            let plain: String = c.chars().filter(|ch| !matches!(ch, '*' | '`' | '~' | '[' | ']')).collect();
            let units: usize = plain.chars().map(|ch| if ch.is_ascii() { 1 } else { 2 }).sum();
            units.div_ceil(share).max(1) as i32
        })
        .max()
        .unwrap_or(1)
}

/// The text of a paragraph that is inside a quote, without the `>` markers that start its
/// continuation lines.
pub fn strip_quote_markers(text: &str, depth: i32) -> String {
    if depth <= 0 {
        return text.to_string();
    }
    text.lines()
        .map(|line| {
            let mut l = line.trim_start();
            for _ in 0..depth {
                match l.strip_prefix('>') {
                    Some(rest) => l = rest.strip_prefix(' ').unwrap_or(rest),
                    None => break,
                }
            }
            l
        })
        .collect::<Vec<_>>()
        .join("\n")
}

// ------------------------------------------------------------------ code block lines

/// Inline markdown of one line of a code block: every punctuation character escaped, spaces
/// non-breaking (so that indentation survives and the line does not wrap), colours by `<font>`.
pub fn code_line_md(tokens: &[(String, Tok)]) -> String {
    let mut out = String::new();
    for (text, tok) in tokens {
        let mut s = String::new();
        for c in text.chars() {
            match c {
                ' ' => s.push('\u{a0}'),
                '\t' => s.push_str("\u{a0}\u{a0}\u{a0}\u{a0}"),
                '\r' => {}
                c if c.is_ascii_punctuation() => {
                    s.push('\\');
                    s.push(c);
                }
                c => s.push(c),
            }
        }
        match tok.color() {
            Some(color) => out.push_str(&format!("<font color='{color}'>{s}</font>")),
            None => out.push_str(&s),
        }
    }
    if out.is_empty() {
        out.push('\u{a0}');
    }
    out
}

#[cfg(test)]
/// The lines of a code block as inline markdown, coloured for `lang`.
pub fn code_lines(lang: &str, code: &str) -> Vec<String> {
    crate::highlight::highlight(lang, code).iter().map(|l| code_line_md(l)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(rows: &[Row]) -> Vec<Kind> {
        rows.iter().map(|r| r.kind).collect()
    }

    #[test]
    fn headings_paragraphs_and_rules() {
        let rows = parse_rows("# Title\n\nSome **bold** and *it* text.\n\n---\n\n## Sub\n");
        assert_eq!(kinds(&rows), vec![Kind::Heading, Kind::Paragraph, Kind::Rule, Kind::Heading]);
        assert_eq!((rows[0].level, rows[0].md.as_str()), (1, "Title"));
        assert_eq!(rows[1].md, "Some **bold** and *it* text.");
        assert_eq!(rows[3].level, 2);
    }

    #[test]
    fn nested_and_numbered_lists() {
        let rows = parse_rows("- a\n  - b\n  - c\n- d\n\n1. one\n2. two\n");
        let items: Vec<(&str, i32, &str)> = rows.iter().map(|r| (r.marker.as_str(), r.depth, r.md.as_str())).collect();
        assert_eq!(items, vec![("•", 1, "a"), ("◦", 2, "b"), ("◦", 2, "c"), ("•", 1, "d"), ("1.", 1, "one"), ("2.", 1, "two")]);
        assert!(rows.iter().all(|r| r.kind == Kind::Item));
        let tasks = parse_rows("- [x] done\n- [ ] todo\n");
        assert_eq!((tasks[0].marker.as_str(), tasks[1].marker.as_str()), ("☑", "☐"));
    }

    #[test]
    fn quotes_code_and_tables() {
        let rows = parse_rows("> quoted *text*\n\n```rust\nfn main() {}\n```\n\n| a | b |\n|---|---|\n| 1 | **2** |\n");
        assert_eq!(kinds(&rows), vec![Kind::Paragraph, Kind::Code, Kind::Table, Kind::Table]);
        assert_eq!(rows[0].quote, 1);
        assert_eq!((rows[1].lang.as_str(), rows[1].code.as_str()), ("rust", "fn main() {}"));
        assert_eq!((rows[2].level, rows[2].cells.clone()), (1, vec!["a".to_string(), "b".to_string()]));
        assert_eq!(rows[3].cells, vec!["1".to_string(), "**2**".to_string()]);
    }

    #[test]
    fn code_in_a_list_item_and_loose_lists() {
        let rows = parse_rows("1. step\n\n   ```sh\n   ls\n   ```\n\n2. next\n");
        assert_eq!(kinds(&rows), vec![Kind::Item, Kind::Code, Kind::Item]);
        assert_eq!(rows[1].depth, 1);
        assert_eq!(rows[2].marker, "2.");
        assert_eq!(rows[1].code, "ls");
    }

    #[test]
    fn unfinished_markdown_does_not_break() {
        // a code fence that is not closed yet, an open emphasis, a cut table
        let rows = parse_rows("text **open\n\n```py\nx = 1\nprint(");
        assert_eq!(kinds(&rows), vec![Kind::Paragraph, Kind::Code]);
        assert_eq!(rows[1].code, "x = 1\nprint(");
        let _ = parse_rows("| a | b\n|--");
        let _ = parse_rows("");
        let _ = parse_rows("\u{e541}x");
    }

    #[test]
    fn a_quote_has_no_markers_in_its_text() {
        let rows = parse_rows("> one\n> two\n>\n> > nested\n");
        assert_eq!(rows[0].md, "one\ntwo");
        assert_eq!(rows[0].quote, 1);
        assert_eq!(strip_quote_markers(">> a\n> > b", 2), "a\nb");
        assert_eq!(strip_quote_markers("x", 0), "x");
    }

    #[test]
    fn table_rows_are_guessed_to_need_one_or_more_lines() {
        assert_eq!(est_lines(&["a".into(), "b".into()]), 1);
        let long = "x".repeat(120);
        assert_eq!(est_lines(&["a".into(), long]), 3);
        assert_eq!(est_lines(&[]), 1);
    }

    #[test]
    fn links_and_inline_code_stay_inline_markdown() {
        let rows = parse_rows("see [docs](https://x.y/z) and `code`, ~~gone~~\n");
        assert_eq!(rows[0].md, "see [docs](https://x.y/z) and `code`, ~~gone~~");
    }

    #[test]
    fn stable_points_follow_blank_lines_outside_fences() {
        let t = "para one\n\npara two\n\n```\ncode\n\nmore\n```\n\nlast";
        let end = stable_end(t);
        assert_eq!(&t[end..], "last");
        // inside an unclosed fence there is no new stable point
        let open = "para\n\nnext\n\n```\ncode\n\nstill";
        assert_eq!(&open[stable_end(open)..], "```\ncode\n\nstill");
        // a list that goes on after a blank line is not cut
        let list = "intro\n\n- a\n\n- b\n";
        assert_eq!(&list[stable_end(list)..], "- a\n\n- b\n");
        assert_eq!(stable_end("one line"), 0);
    }

    #[test]
    fn the_cache_gives_the_same_rows_as_a_full_parse() {
        let full = "# T\n\nfirst paragraph\n\n- a\n- b\n\nsecond **para**\n\n```rust\nlet x = 1;\n\nlet y = 2;\n```\n\nend\n";
        let mut cache = Cache::default();
        let mut last = Vec::new();
        // streamed in pieces, also cut in the middle of lines
        let cuts: Vec<usize> = (1..full.len()).filter(|i| full.is_char_boundary(*i)).collect();
        for c in cuts {
            last = cache.update(&full[..c]);
            assert_eq!(last, parse_rows(&full[..c]), "cut at {c}");
        }
        assert_eq!(cache.update(full), parse_rows(full));
        assert!(cache.stable_rows() > 0);
        let _ = last;
        // another text: the cache starts over
        assert_eq!(cache.update("# other"), parse_rows("# other"));
    }

    #[test]
    fn a_model_kept_with_update_tail_is_right_even_when_updates_are_skipped() {
        let full = "# T

first

- a
- b

second

```json
{ \"a\": 1,
  \"ok\": true }
```

last paragraph

> q

end
";
        for step in [1usize, 3, 7, 13, 40] {
            let mut cache = Cache::default();
            let mut model: Vec<Row> = Vec::new();
            let mut at = 0;
            while at < full.len() {
                at = (at + step).min(full.len());
                while !full.is_char_boundary(at) {
                    at += 1;
                }
                let text = &full[..at];
                let (from, rows) = cache.update_tail(text);
                assert!(model.len() >= from, "step {step} at {at}");
                model.truncate(from);
                model.extend(rows);
                assert_eq!(model, parse_rows(text), "step {step} at {at}");
            }
        }
    }

    #[test]
    fn code_lines_are_escaped_coloured_and_keep_indentation() {
        let lines = code_lines("rust", "fn f() {\n    // *x*\n}");
        assert_eq!(lines.len(), 3);
        assert!(lines[0].contains("<font color='#b062c8'>fn</font>"));
        assert!(lines[1].starts_with("\u{a0}\u{a0}\u{a0}\u{a0}"));
        assert!(lines[1].contains("\\/\\/"));
        assert!(lines[1].contains("\\*x\\*"));
        assert_eq!(code_lines("", "\n")[0], "\u{a0}");
        // plain text with markdown characters is only escaped
        assert_eq!(code_line_md(&[("# a_b".to_string(), Tok::Plain)]), "\\#\u{a0}a\\_b");
    }
}
