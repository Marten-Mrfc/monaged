//! Turns an agent's Markdown reply into simple blocks the UI can draw:
//! headings, paragraphs, lists (task lists too), quotes, code blocks, tables
//! and rules, with bold, italic, strikethrough, inline code and links inside
//! the text (GitHub's flavour, which is what agents write).

use std::ops::Range;

use pulldown_cmark::{Alignment, Event, HeadingLevel, Options, Parser, Tag, TagEnd};

#[derive(Clone, Copy, Default, PartialEq)]
pub struct Style {
    pub bold: bool,
    pub italic: bool,
    pub code: bool,
    pub link: bool,
    pub strike: bool,
}

#[derive(Clone, Default)]
pub struct Inline {
    pub text: String,
    /// Sorted, non-overlapping ranges with their combined style.
    pub styles: Vec<(Range<usize>, Style)>,
}

impl Inline {
    fn push(&mut self, text: &str, style: Style) {
        if text.is_empty() {
            return;
        }
        let start = self.text.len();
        self.text.push_str(text);
        if style != Style::default() {
            self.styles.push((start..self.text.len(), style));
        }
    }

    fn is_empty(&self) -> bool {
        self.text.trim().is_empty()
    }

    /// The text without its styling (for sizing table columns).
    pub fn plain_len(&self) -> usize {
        self.text.chars().count()
    }
}

/// A table while it's being read: alignment, rows so far, the row being read.
type TableState = (Vec<Align>, Vec<Vec<Inline>>, Vec<Inline>);

/// How a table column lines up.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Align {
    Left,
    Center,
    Right,
}

#[derive(Clone)]
pub enum Block {
    Heading(u8, Inline),
    Paragraph(Inline),
    /// `check` is Some for a task list item: ticked or not.
    Item { depth: usize, marker: String, content: Inline, check: Option<bool> },
    Quote(Inline),
    /// (code, the fence's language, e.g. "rust")
    Code(String, String),
    /// The first row is the header.
    Table { align: Vec<Align>, rows: Vec<Vec<Inline>> },
    Rule,
}

/// Whether text someone typed uses Markdown (lists, code, emphasis, tables),
/// so it's shown formatted; plain text stays exactly as typed.
pub fn looks_like_markdown(text: &str) -> bool {
    text.contains('`')
        || text.contains("**")
        || text.contains("](")
        || text.lines().any(|l| {
            let t = l.trim_start();
            t.starts_with("- ") || t.starts_with("* ") || t.starts_with("# ") || t.starts_with("## ") || t.starts_with("> ") || t.starts_with('|')
                || t.split_once(". ").is_some_and(|(n, _)| !n.is_empty() && n.len() <= 3 && n.chars().all(|c| c.is_ascii_digit()))
        })
}

pub fn parse(source: &str) -> Vec<Block> {
    let mut blocks = Vec::new();
    let mut inline = Inline::default();
    let mut style = Style::default();
    let mut lists: Vec<Option<u64>> = Vec::new();
    let mut item_marker: Option<String> = None;
    let mut heading: Option<u8> = None;
    let mut quote_depth = 0usize;
    let mut code: Option<String> = None;
    let mut code_lang = String::new();
    let mut check: Option<bool> = None;
    // A table being read: its alignment, the rows so far, and the row being read.
    let mut table: Option<TableState> = None;

    // Ends the current item/paragraph/heading and pushes it as a block.
    let flush = |blocks: &mut Vec<Block>,
                 inline: &mut Inline,
                 item_marker: &mut Option<String>,
                 check: &mut Option<bool>,
                 heading: Option<u8>,
                 lists: &Vec<Option<u64>>,
                 quote_depth: usize| {
        let content = std::mem::take(inline);
        if let Some(marker) = item_marker.take() {
            blocks.push(Block::Item { depth: lists.len().saturating_sub(1), marker, content, check: check.take() });
        } else if content.is_empty() {
        } else if let Some(level) = heading {
            blocks.push(Block::Heading(level, content));
        } else if quote_depth > 0 {
            blocks.push(Block::Quote(content));
        } else {
            blocks.push(Block::Paragraph(content));
        }
    };

    let options = Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    for event in Parser::new_ext(source, options) {
        if let Some(buffer) = code.as_mut() {
            match event {
                Event::Text(text) => buffer.push_str(&text),
                Event::End(TagEnd::CodeBlock) => {
                    blocks.push(Block::Code(code.take().unwrap_or_default().trim_end().to_string(), std::mem::take(&mut code_lang)));
                }
                _ => {}
            }
            continue;
        }
        // Inside a table, text goes into cells.
        if table.is_some() {
            match event {
                Event::Start(Tag::TableHead) | Event::Start(Tag::TableRow) => {
                    if let Some((_, _, row)) = table.as_mut() {
                        row.clear();
                    }
                }
                Event::Start(Tag::TableCell) => inline = Inline::default(),
                Event::End(TagEnd::TableCell) => {
                    if let Some((_, _, row)) = table.as_mut() {
                        row.push(std::mem::take(&mut inline));
                    }
                }
                Event::End(TagEnd::TableHead) | Event::End(TagEnd::TableRow) => {
                    if let Some((_, rows, row)) = table.as_mut() {
                        rows.push(std::mem::take(row));
                    }
                }
                Event::End(TagEnd::Table) => {
                    if let Some((align, rows, _)) = table.take() {
                        blocks.push(Block::Table { align, rows });
                    }
                }
                Event::Start(Tag::Strong) => style.bold = true,
                Event::End(TagEnd::Strong) => style.bold = false,
                Event::Start(Tag::Emphasis) => style.italic = true,
                Event::End(TagEnd::Emphasis) => style.italic = false,
                Event::Start(Tag::Strikethrough) => style.strike = true,
                Event::End(TagEnd::Strikethrough) => style.strike = false,
                Event::Start(Tag::Link { .. }) => style.link = true,
                Event::End(TagEnd::Link) => style.link = false,
                Event::Code(text) => inline.push(&text, Style { code: true, ..style }),
                Event::Text(text) | Event::Html(text) | Event::InlineHtml(text) => inline.push(&text, style),
                Event::SoftBreak | Event::HardBreak => inline.push(" ", style),
                _ => {}
            }
            continue;
        }
        match event {
            Event::Start(Tag::Table(aligns)) => {
                flush(&mut blocks, &mut inline, &mut item_marker, &mut check, heading, &lists, quote_depth);
                let align = aligns
                    .iter()
                    .map(|a| match a {
                        Alignment::Center => Align::Center,
                        Alignment::Right => Align::Right,
                        _ => Align::Left,
                    })
                    .collect();
                table = Some((align, Vec::new(), Vec::new()));
            }
            Event::Start(Tag::Heading { level, .. }) => {
                heading = Some(match level {
                    HeadingLevel::H1 => 1,
                    HeadingLevel::H2 => 2,
                    _ => 3,
                });
            }
            Event::End(TagEnd::Heading(_)) => {
                flush(&mut blocks, &mut inline, &mut item_marker, &mut check, heading, &lists, quote_depth);
                heading = None;
            }
            Event::Start(Tag::Paragraph) => {}
            Event::End(TagEnd::Paragraph) => {
                if item_marker.is_some() {
                    inline.push("\n", Style::default());
                } else {
                    flush(&mut blocks, &mut inline, &mut item_marker, &mut check, heading, &lists, quote_depth);
                }
            }
            Event::Start(Tag::BlockQuote(_)) => quote_depth += 1,
            Event::End(TagEnd::BlockQuote(_)) => quote_depth = quote_depth.saturating_sub(1),
            Event::Start(Tag::CodeBlock(kind)) => {
                flush(&mut blocks, &mut inline, &mut item_marker, &mut check, heading, &lists, quote_depth);
                code_lang = match kind {
                    pulldown_cmark::CodeBlockKind::Fenced(info) => info.split_whitespace().next().unwrap_or_default().to_string(),
                    pulldown_cmark::CodeBlockKind::Indented => String::new(),
                };
                code = Some(String::new());
            }
            Event::Start(Tag::List(start)) => {
                // A nested list ends the text of the item that contains it.
                if item_marker.is_some() {
                    let trimmed = inline.text.trim_end().len();
                    inline.text.truncate(trimmed);
                    flush(&mut blocks, &mut inline, &mut item_marker, &mut check, None, &lists, quote_depth);
                }
                lists.push(start);
            }
            Event::End(TagEnd::List(_)) => {
                lists.pop();
            }
            Event::Start(Tag::Item) => {
                let marker = match lists.last_mut() {
                    Some(Some(n)) => {
                        let m = format!("{n}.");
                        *n += 1;
                        m
                    }
                    _ => "•".to_string(),
                };
                item_marker = Some(marker);
            }
            Event::End(TagEnd::Item) => {
                if item_marker.is_some() {
                    let trimmed = inline.text.trim_end().len();
                    inline.text.truncate(trimmed);
                    flush(&mut blocks, &mut inline, &mut item_marker, &mut check, None, &lists, quote_depth);
                }
            }
            Event::Start(Tag::Strong) => style.bold = true,
            Event::End(TagEnd::Strong) => style.bold = false,
            Event::Start(Tag::Emphasis) => style.italic = true,
            Event::End(TagEnd::Emphasis) => style.italic = false,
            Event::Start(Tag::Strikethrough) => style.strike = true,
            Event::End(TagEnd::Strikethrough) => style.strike = false,
            Event::Start(Tag::Link { .. }) => style.link = true,
            Event::End(TagEnd::Link) => style.link = false,
            Event::Code(text) => inline.push(&text, Style { code: true, ..style }),
            Event::Text(text) | Event::Html(text) | Event::InlineHtml(text) => inline.push(&text, style),
            Event::SoftBreak => inline.push(" ", style),
            Event::HardBreak => inline.push("\n", style),
            Event::Rule => blocks.push(Block::Rule),
            Event::TaskListMarker(done) => check = Some(done),
            _ => {}
        }
    }
    flush(&mut blocks, &mut inline, &mut item_marker, &mut check, heading, &lists, quote_depth);
    // A table the text ended inside (an agent still writing it).
    if let Some((align, mut rows, row)) = table.take() {
        if !row.is_empty() {
            rows.push(row);
        }
        blocks.push(Block::Table { align, rows });
    }
    // Clamp ranges that a trailing trim may have cut.
    for block in &mut blocks {
        let inline = match block {
            Block::Heading(_, i) | Block::Paragraph(i) | Block::Quote(i) => i,
            Block::Item { content, .. } => content,
            Block::Table { rows, .. } => {
                for cell in rows.iter_mut().flatten() {
                    let len = cell.text.len();
                    cell.styles.retain_mut(|(range, _)| {
                        range.end = range.end.min(len);
                        range.start < range.end
                    });
                }
                continue;
            }
            _ => continue,
        };
        let len = inline.text.len();
        inline.styles.retain_mut(|(range, _)| {
            range.end = range.end.min(len);
            range.start < range.end
        });
    }
    blocks
}
