//! Render the model's Markdown into Slack Block Kit blocks. The model emits
//! standard CommonMark/GFM, but Slack's `text` field only speaks Slack
//! *mrkdwn* — so `**bold**`, `# headings`, `[links](url)`, lists, code, and
//! tables render wrong. We parse the Markdown with `pulldown-cmark` and map
//! each top-level block to a Block Kit block:
//!
//! - heading            → `header` (plain_text, ≤150 chars)
//! - paragraph          → `rich_text` › `rich_text_section`
//! - list (nested)      → `rich_text` › `rich_text_list` (one per indent run)
//! - block quote        → `rich_text` › `rich_text_quote`
//! - fenced/indent code → `rich_text` › `rich_text_preformatted`
//! - thematic break     → `divider`
//! - GFM table          → `table` (see [`table`])
//!
//! Inline runs use `rich_text` element style flags (bold/italic/strike/code)
//! and `link` elements, so Slack handles all escaping of `& < >` — we never
//! rewrite Markdown into mrkdwn strings. Everything here is pure and unit
//! tested; the caller ([`super`]) sends the blocks with a `text` fallback.

use pulldown_cmark::{Alignment, Event, Options, Parser, Tag, TagEnd};
use serde_json::{Map, Value, json};

/// Inline style flags carried down through nested emphasis/strong/etc.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
struct Style {
    bold: bool,
    italic: bool,
    strike: bool,
    code: bool,
}

impl Style {
    fn with_bold(mut self) -> Self {
        self.bold = true;
        self
    }
    fn with_italic(mut self) -> Self {
        self.italic = true;
        self
    }
    fn with_strike(mut self) -> Self {
        self.strike = true;
        self
    }
    fn with_code(mut self) -> Self {
        self.code = true;
        self
    }

    /// The Block Kit `style` object, or `None` when no flag is set (so plain
    /// text elements stay free of an empty `style`).
    fn to_json(self) -> Option<Value> {
        if self == Self::default() {
            return None;
        }
        let mut m = Map::new();
        if self.bold {
            m.insert("bold".into(), json!(true));
        }
        if self.italic {
            m.insert("italic".into(), json!(true));
        }
        if self.strike {
            m.insert("strike".into(), json!(true));
        }
        if self.code {
            m.insert("code".into(), json!(true));
        }
        Some(Value::Object(m))
    }
}

/// The plain-text fallback for Slack's `text` field: the trimmed Markdown. Used
/// for notifications and clients that don't render blocks.
pub fn fallback_text(markdown: &str) -> String {
    markdown.trim().to_string()
}

/// Convert `markdown` into a list of Slack Block Kit blocks. Empty/whitespace
/// input yields an empty list (the caller decides what to post in that case).
pub fn to_blocks(markdown: &str) -> Vec<Value> {
    let opts = Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH;
    let events: Vec<Event> = Parser::new_ext(markdown, opts).collect();
    let mut blocks = Vec::new();
    let mut i = 0;
    while i < events.len() {
        match &events[i] {
            Event::Start(Tag::Heading { .. }) => {
                i += 1;
                let text = collect_text(&events, &mut i, |e| matches!(e, TagEnd::Heading(_)));
                push_header(&mut blocks, &text);
            }
            Event::Start(Tag::Paragraph) => {
                i += 1;
                let mut section = Vec::new();
                push_inline(
                    &events,
                    &mut i,
                    Some(&TagEnd::Paragraph),
                    Style::default(),
                    &mut section,
                );
                if !section.is_empty() {
                    blocks.push(rich_text(vec![rich_text_section(section)]));
                }
            }
            Event::Start(Tag::List(start)) => {
                let start = *start;
                let mut elements = Vec::new();
                render_list(&events, &mut i, start, 0, &mut elements);
                if !elements.is_empty() {
                    blocks.push(rich_text(elements));
                }
            }
            Event::Start(Tag::BlockQuote(_)) => {
                i += 1;
                let elements = render_quote(&events, &mut i);
                if !elements.is_empty() {
                    blocks.push(rich_text(vec![rich_text_quote(elements)]));
                }
            }
            Event::Start(Tag::CodeBlock(_)) => {
                i += 1;
                let code = collect_code(&events, &mut i);
                if !code.is_empty() {
                    blocks.push(rich_text(vec![rich_text_preformatted(&code)]));
                }
            }
            Event::Start(Tag::Table(aligns)) => {
                let aligns = aligns.clone();
                i += 1;
                if let Some(block) = table(&events, &mut i, &aligns) {
                    blocks.push(block);
                }
            }
            Event::Rule => {
                blocks.push(json!({ "type": "divider" }));
                i += 1;
            }
            _ => i += 1,
        }
    }
    blocks
}

/// Walk an inline run, appending `rich_text` elements (text/link) to `out`.
/// Stops on the matching `end` (consuming it) or, when `end` is `None`, on any
/// `End` or block-level `Start` it doesn't own (without consuming) — which lets
/// the same routine serve both wrapped paragraphs and bare tight-list items.
fn push_inline(
    events: &[Event],
    i: &mut usize,
    end: Option<&TagEnd>,
    style: Style,
    out: &mut Vec<Value>,
) {
    while *i < events.len() {
        match &events[*i] {
            Event::End(e) => {
                if end == Some(e) {
                    *i += 1;
                }
                return;
            }
            Event::Text(t) => {
                push_text(out, t, style);
                *i += 1;
            }
            Event::Code(t) => {
                push_text(out, t, style.with_code());
                *i += 1;
            }
            Event::SoftBreak | Event::HardBreak => {
                push_text(out, "\n", style);
                *i += 1;
            }
            Event::Start(Tag::Emphasis) => {
                *i += 1;
                push_inline(events, i, Some(&TagEnd::Emphasis), style.with_italic(), out);
            }
            Event::Start(Tag::Strong) => {
                *i += 1;
                push_inline(events, i, Some(&TagEnd::Strong), style.with_bold(), out);
            }
            Event::Start(Tag::Strikethrough) => {
                *i += 1;
                push_inline(events, i, Some(&TagEnd::Strikethrough), style.with_strike(), out);
            }
            Event::Start(Tag::Link { dest_url, .. }) => {
                let url = dest_url.to_string();
                *i += 1;
                let text = collect_text(events, i, |e| matches!(e, TagEnd::Link));
                out.push(link_element(&url, &text, style));
            }
            Event::Start(Tag::Image { dest_url, .. }) => {
                let url = dest_url.to_string();
                *i += 1;
                let alt = collect_text(events, i, |e| matches!(e, TagEnd::Image));
                let label = if alt.is_empty() { &url } else { &alt };
                out.push(link_element(&url, label, style));
            }
            // A block-level start we don't handle inline: stop without consuming
            // so the caller's block loop sees it.
            Event::Start(_) => return,
            _ => *i += 1,
        }
    }
}

/// Render a list (and any nested lists) as a flat sequence of `rich_text_list`
/// elements appended to `out`, each carrying its `indent`. Nesting is expressed
/// the Block Kit way: a deeper list is a sibling element with a higher indent,
/// in document order — so a parent level is split around its nested children.
fn render_list(
    events: &[Event],
    i: &mut usize,
    start: Option<u64>,
    indent: u8,
    out: &mut Vec<Value>,
) {
    let style = if start.is_some() { "ordered" } else { "bullet" };
    *i += 1; // consume Start(List)
    let mut sections: Vec<Value> = Vec::new();
    while *i < events.len() {
        match &events[*i] {
            Event::End(TagEnd::List(_)) => {
                *i += 1;
                break;
            }
            Event::Start(Tag::Item) => {
                *i += 1;
                let mut sec: Vec<Value> = Vec::new();
                while *i < events.len() {
                    match &events[*i] {
                        Event::End(TagEnd::Item) => {
                            *i += 1;
                            break;
                        }
                        Event::Start(Tag::List(s2)) => {
                            // Close this level (including the current item) before
                            // descending, so the nested list lands after its parent.
                            let s2 = *s2;
                            if !sec.is_empty() {
                                sections.push(rich_text_section(std::mem::take(&mut sec)));
                            }
                            if !sections.is_empty() {
                                let taken = std::mem::take(&mut sections);
                                out.push(rich_text_list(style, indent, taken));
                            }
                            render_list(events, i, s2, indent + 1, out);
                        }
                        Event::Start(Tag::Paragraph) => {
                            *i += 1;
                            push_inline(
                                events,
                                i,
                                Some(&TagEnd::Paragraph),
                                Style::default(),
                                &mut sec,
                            );
                        }
                        Event::Start(Tag::CodeBlock(_)) => {
                            *i += 1;
                            let code = collect_code(events, i);
                            if !code.is_empty() {
                                push_text(&mut sec, &code, Style::default().with_code());
                            }
                        }
                        _ => {
                            let before = *i;
                            push_inline(events, i, None, Style::default(), &mut sec);
                            if *i == before {
                                *i += 1; // guarantee progress on anything unhandled
                            }
                        }
                    }
                }
                if !sec.is_empty() {
                    sections.push(rich_text_section(sec));
                }
            }
            _ => *i += 1,
        }
    }
    if !sections.is_empty() {
        out.push(rich_text_list(style, indent, sections));
    }
}

/// Collect a block quote's inline content into a flat element list (paragraphs
/// joined by newlines). Stops after the matching `End(BlockQuote)`.
fn render_quote(events: &[Event], i: &mut usize) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    while *i < events.len() {
        match &events[*i] {
            Event::End(TagEnd::BlockQuote(_)) => {
                *i += 1;
                break;
            }
            Event::Start(Tag::Paragraph) => {
                *i += 1;
                if !out.is_empty() {
                    push_text(&mut out, "\n", Style::default());
                }
                push_inline(events, i, Some(&TagEnd::Paragraph), Style::default(), &mut out);
            }
            _ => {
                let before = *i;
                push_inline(events, i, None, Style::default(), &mut out);
                if *i == before {
                    *i += 1;
                }
            }
        }
    }
    out
}

/// Concatenate the `Text` of a code block up to (and consuming) its
/// `End(CodeBlock)`, dropping the single trailing newline pulldown appends.
fn collect_code(events: &[Event], i: &mut usize) -> String {
    let mut code = String::new();
    while *i < events.len() {
        match &events[*i] {
            Event::End(TagEnd::CodeBlock) => {
                *i += 1;
                break;
            }
            Event::Text(t) => {
                code.push_str(t);
                *i += 1;
            }
            _ => *i += 1,
        }
    }
    while code.ends_with('\n') {
        code.pop();
    }
    code
}

/// Flatten inline content to plain text up to (and consuming) the first `End`
/// matching `is_end`. Used for headings (no inline styles) and link/image text.
fn collect_text(events: &[Event], i: &mut usize, is_end: impl Fn(&TagEnd) -> bool) -> String {
    let mut s = String::new();
    while *i < events.len() {
        match &events[*i] {
            Event::End(e) if is_end(e) => {
                *i += 1;
                break;
            }
            Event::Text(t) | Event::Code(t) => {
                s.push_str(t);
                *i += 1;
            }
            Event::SoftBreak | Event::HardBreak => {
                s.push(' ');
                *i += 1;
            }
            _ => *i += 1,
        }
    }
    s
}

/// Push a text element, merging the style. Skips empty strings (which Slack
/// rejects).
fn push_text(out: &mut Vec<Value>, text: &str, style: Style) {
    if text.is_empty() {
        return;
    }
    let mut obj = Map::new();
    obj.insert("type".into(), json!("text"));
    obj.insert("text".into(), json!(text));
    if let Some(s) = style.to_json() {
        obj.insert("style".into(), s);
    }
    out.push(Value::Object(obj));
}

/// A `link` rich-text element. Falls back to the URL as the label when the link
/// text is empty.
fn link_element(url: &str, text: &str, style: Style) -> Value {
    let mut obj = Map::new();
    obj.insert("type".into(), json!("link"));
    obj.insert("url".into(), json!(url));
    let label = if text.is_empty() { url } else { text };
    obj.insert("text".into(), json!(label));
    if let Some(s) = style.to_json() {
        obj.insert("style".into(), s);
    }
    Value::Object(obj)
}

/// Append a `header` block for `text`, truncated to Slack's 150-char limit.
/// Skips blank headings.
fn push_header(blocks: &mut Vec<Value>, text: &str) {
    let text = text.trim();
    if text.is_empty() {
        return;
    }
    let truncated: String = text.chars().take(150).collect();
    blocks.push(json!({
        "type": "header",
        "text": { "type": "plain_text", "text": truncated, "emoji": true }
    }));
}

/// A `rich_text` block wrapping `elements` (sections, lists, quotes, …).
fn rich_text(elements: Vec<Value>) -> Value {
    json!({ "type": "rich_text", "elements": elements })
}

fn rich_text_section(elements: Vec<Value>) -> Value {
    json!({ "type": "rich_text_section", "elements": elements })
}

fn rich_text_quote(elements: Vec<Value>) -> Value {
    json!({ "type": "rich_text_quote", "elements": elements })
}

fn rich_text_preformatted(code: &str) -> Value {
    json!({
        "type": "rich_text_preformatted",
        "elements": [ { "type": "text", "text": code } ]
    })
}

fn rich_text_list(style: &str, indent: u8, sections: Vec<Value>) -> Value {
    json!({
        "type": "rich_text_list",
        "style": style,
        "indent": indent,
        "elements": sections
    })
}

/// Slack's `table` block caps (rows include the header). Beyond these the table
/// is dropped rather than rejected by the API.
const MAX_TABLE_ROWS: usize = 100;
const MAX_TABLE_COLS: usize = 10;

/// Map a GFM table to Slack's native `table` block: the head + body rows become
/// `rows`, GFM alignment becomes `column_settings`, and each cell is a
/// `raw_text` (plain) or `rich_text` (links/styles) value. `*i` enters just
/// after `Start(Table)` and leaves just after `End(Table)`. Returns `None` for
/// an empty or oversized table (the caller drops it).
fn table(events: &[Event], i: &mut usize, aligns: &[Alignment]) -> Option<Value> {
    let mut rows: Vec<Value> = Vec::new();
    while *i < events.len() {
        match &events[*i] {
            Event::End(TagEnd::Table) => {
                *i += 1;
                break;
            }
            Event::Start(Tag::TableHead) => {
                *i += 1;
                rows.push(read_row(events, i, TagEnd::TableHead));
            }
            Event::Start(Tag::TableRow) => {
                *i += 1;
                rows.push(read_row(events, i, TagEnd::TableRow));
            }
            _ => *i += 1,
        }
    }
    if rows.is_empty() || rows.len() > MAX_TABLE_ROWS || aligns.len() > MAX_TABLE_COLS {
        return None;
    }
    let settings: Vec<Value> = aligns.iter().map(column_setting).collect();
    Some(json!({ "type": "table", "column_settings": settings, "rows": rows }))
}

/// Read one table row (its cells) up to and consuming `End(end)`.
fn read_row(events: &[Event], i: &mut usize, end: TagEnd) -> Value {
    let mut cells = Vec::new();
    while *i < events.len() {
        match &events[*i] {
            Event::End(e) if *e == end => {
                *i += 1;
                break;
            }
            Event::Start(Tag::TableCell) => {
                *i += 1;
                let mut elems = Vec::new();
                push_inline(events, i, Some(&TagEnd::TableCell), Style::default(), &mut elems);
                cells.push(cell(elems));
            }
            _ => *i += 1,
        }
    }
    Value::Array(cells)
}

/// A table cell: `raw_text` when the content is unstyled plain text, otherwise a
/// `rich_text` cell wrapping a section (so links/styles survive).
fn cell(elements: Vec<Value>) -> Value {
    let all_plain = elements.iter().all(|e| e["type"] == "text" && e.get("style").is_none());
    if all_plain {
        let text: String = elements.iter().filter_map(|e| e["text"].as_str()).collect();
        json!({ "type": "raw_text", "text": text })
    } else {
        json!({ "type": "rich_text", "elements": [ rich_text_section(elements) ] })
    }
}

/// A `column_settings` entry: wrap long content, and carry GFM alignment when
/// the column specifies one.
fn column_setting(align: &Alignment) -> Value {
    let mut m = Map::new();
    m.insert("is_wrapped".into(), json!(true));
    let align = match align {
        Alignment::Left => Some("left"),
        Alignment::Center => Some("center"),
        Alignment::Right => Some("right"),
        Alignment::None => None,
    };
    if let Some(a) = align {
        m.insert("align".into(), json!(a));
    }
    Value::Object(m)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_heading_becomes_a_header_block_truncated() {
        let long = format!("# {}", "x".repeat(200));
        let blocks = to_blocks(&long);
        assert_eq!(blocks[0]["type"], "header");
        assert_eq!(blocks[0]["text"]["type"], "plain_text");
        let text = blocks[0]["text"]["text"].as_str().unwrap();
        assert_eq!(text.chars().count(), 150, "truncated to 150");
    }

    #[test]
    fn a_paragraph_carries_inline_styles_and_links() {
        let blocks = to_blocks("a **b** _i_ `c` and [lnk](https://x.test).");
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0]["type"], "rich_text");
        let els = &blocks[0]["elements"][0]["elements"];
        // Find the bold, italic, code, and link elements.
        let has = |pred: &dyn Fn(&Value) -> bool| els.as_array().unwrap().iter().any(pred);
        assert!(has(&|e| e["text"] == "b" && e["style"]["bold"] == true), "bold: {els}");
        assert!(has(&|e| e["text"] == "i" && e["style"]["italic"] == true), "italic: {els}");
        assert!(has(&|e| e["text"] == "c" && e["style"]["code"] == true), "code: {els}");
        assert!(
            has(&|e| e["type"] == "link" && e["url"] == "https://x.test" && e["text"] == "lnk"),
            "link: {els}"
        );
    }

    #[test]
    fn plain_text_carries_no_style_object() {
        let blocks = to_blocks("just words");
        let el = &blocks[0]["elements"][0]["elements"][0];
        assert_eq!(el["text"], "just words");
        assert!(el.get("style").is_none(), "no empty style: {el}");
    }

    #[test]
    fn a_nested_list_splits_into_indented_rich_text_lists() {
        let md = "- a\n  - a1\n- b";
        let blocks = to_blocks(md);
        assert_eq!(blocks.len(), 1);
        let els = blocks[0]["elements"].as_array().unwrap();
        // Three list runs: [a] @0, [a1] @1, [b] @0.
        assert_eq!(els.len(), 3, "{els:#?}");
        assert_eq!(els[0]["type"], "rich_text_list");
        assert_eq!(els[0]["style"], "bullet");
        assert_eq!(els[0]["indent"], 0);
        assert_eq!(els[1]["indent"], 1);
        assert_eq!(els[2]["indent"], 0);
        // The first run holds item "a".
        assert_eq!(els[0]["elements"][0]["elements"][0]["text"], "a");
        assert_eq!(els[1]["elements"][0]["elements"][0]["text"], "a1");
    }

    #[test]
    fn an_ordered_list_uses_the_ordered_style() {
        let blocks = to_blocks("1. one\n2. two");
        assert_eq!(blocks[0]["elements"][0]["style"], "ordered");
    }

    #[test]
    fn a_blockquote_becomes_a_rich_text_quote() {
        let blocks = to_blocks("> quoted line");
        assert_eq!(blocks[0]["type"], "rich_text");
        assert_eq!(blocks[0]["elements"][0]["type"], "rich_text_quote");
        assert_eq!(blocks[0]["elements"][0]["elements"][0]["text"], "quoted line");
    }

    #[test]
    fn a_fenced_code_block_becomes_preformatted_without_trailing_newline() {
        let blocks = to_blocks("```\nlet x = 1;\n```");
        assert_eq!(blocks[0]["elements"][0]["type"], "rich_text_preformatted");
        assert_eq!(blocks[0]["elements"][0]["elements"][0]["text"], "let x = 1;");
    }

    #[test]
    fn a_thematic_break_becomes_a_divider() {
        let blocks = to_blocks("a\n\n---\n\nb");
        assert!(blocks.iter().any(|b| b["type"] == "divider"), "{blocks:#?}");
    }

    #[test]
    fn a_mixed_document_keeps_block_order() {
        let md = "# Title\n\nIntro para.\n\n- one\n- two\n\n```\ncode\n```";
        let blocks = to_blocks(md);
        let kinds: Vec<&str> = blocks.iter().map(|b| b["type"].as_str().unwrap()).collect();
        assert_eq!(kinds, vec!["header", "rich_text", "rich_text", "rich_text"]);
        // Second rich_text is the list, third is the code.
        assert_eq!(blocks[2]["elements"][0]["type"], "rich_text_list");
        assert_eq!(blocks[3]["elements"][0]["type"], "rich_text_preformatted");
    }

    #[test]
    fn a_gfm_table_becomes_a_table_block_with_alignment() {
        let md = "\
| Name | Age |
|:-----|----:|
| Ann  | 30  |
| Bob  | 25  |";
        let blocks = to_blocks(md);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0]["type"], "table");
        // Alignment from :--- and ---:
        let settings = blocks[0]["column_settings"].as_array().unwrap();
        assert_eq!(settings.len(), 2);
        assert_eq!(settings[0]["align"], "left");
        assert_eq!(settings[1]["align"], "right");
        assert_eq!(settings[0]["is_wrapped"], true);
        // Header + two body rows, each two cells.
        let rows = blocks[0]["rows"].as_array().unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0][0], json!({"type": "raw_text", "text": "Name"}));
        assert_eq!(rows[1][0], json!({"type": "raw_text", "text": "Ann"}));
    }

    #[test]
    fn a_table_cell_with_a_link_becomes_a_rich_text_cell() {
        let md = "\
| Site |
|------|
| [x](https://x.test) |";
        let blocks = to_blocks(md);
        let link_cell = &blocks[0]["rows"][1][0];
        assert_eq!(link_cell["type"], "rich_text");
        let el = &link_cell["elements"][0]["elements"][0];
        assert_eq!(el["type"], "link");
        assert_eq!(el["url"], "https://x.test");
    }

    #[test]
    fn a_table_interleaves_with_surrounding_prose() {
        let md = "Before.\n\n| A | B |\n|---|---|\n| 1 | 2 |\n\nAfter.";
        let blocks = to_blocks(md);
        let kinds: Vec<&str> = blocks.iter().map(|b| b["type"].as_str().unwrap()).collect();
        assert_eq!(kinds, vec!["rich_text", "table", "rich_text"]);
    }

    #[test]
    fn empty_input_yields_no_blocks() {
        assert!(to_blocks("").is_empty());
        assert!(to_blocks("   \n  \n").is_empty());
    }

    #[test]
    fn fallback_text_is_the_trimmed_markdown() {
        assert_eq!(fallback_text("  # Hi\n\n"), "# Hi");
    }
}
