//! Text fields are Markdown. hoja parses them once, here, into a small tree
//! that templates render through `hoja.typ` (no text is ever evaluated as
//! Typst) and that the HTML export and the editor preview render as escaped
//! HTML. Raw HTML in a field stays literal text; only http(s) and mailto
//! links survive.

use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag};
use serde_json::{Map, Value, json};

enum Frame {
    Root,
    Paragraph,
    Heading(u8),
    List {
        ordered: bool,
        start: u64,
    },
    Item,
    Quote,
    CodeBlock,
    HtmlBlock,
    Inline(&'static str),
    Link(Option<String>),
    /// Anything else: its children join the parent.
    Transparent,
}

fn is_inline(node: &Value) -> bool {
    match node {
        Value::String(_) => true,
        Value::Object(map) => matches!(
            map.get("t").and_then(Value::as_str),
            Some("em" | "strong" | "strike" | "code" | "link" | "br")
        ),
        _ => false,
    }
}

fn push(children: &mut Vec<Value>, node: Value) {
    if let (Some(Value::String(last)), Value::String(next)) = (children.last_mut(), &node) {
        last.push_str(next);
        return;
    }
    children.push(node);
}

/// Wraps runs of inline nodes (a tight list item, stray text) into paragraphs.
fn blocks(children: Vec<Value>) -> Vec<Value> {
    let mut out = Vec::new();
    let mut run: Vec<Value> = Vec::new();
    for child in children {
        if is_inline(&child) {
            push(&mut run, child);
        } else {
            if !run.is_empty() {
                out.push(json!({"t": "p", "c": std::mem::take(&mut run)}));
            }
            out.push(child);
        }
    }
    if !run.is_empty() {
        out.push(json!({"t": "p", "c": run}));
    }
    out
}

fn safe_url(url: &str) -> Option<String> {
    let lower = url.trim().to_ascii_lowercase();
    ["http://", "https://", "mailto:"]
        .iter()
        .any(|scheme| lower.starts_with(scheme))
        .then(|| url.trim().to_string())
}

fn text_of(children: &[Value]) -> String {
    children
        .iter()
        .map(|c| match c {
            Value::String(s) => s.clone(),
            other => other["text"].as_str().unwrap_or_default().to_string(),
        })
        .collect()
}

/// The parsed form of a Markdown text: a list of blocks.
pub fn tree(text: &str) -> Value {
    let options = Options::ENABLE_STRIKETHROUGH;
    let mut stack: Vec<(Frame, Vec<Value>)> = vec![(Frame::Root, Vec::new())];
    for event in Parser::new_ext(text, options) {
        match event {
            Event::Start(tag) => {
                let frame = match tag {
                    Tag::Paragraph => Frame::Paragraph,
                    Tag::Heading { level, .. } => Frame::Heading(level as u8),
                    Tag::List(start) => Frame::List {
                        ordered: start.is_some(),
                        start: start.unwrap_or(1),
                    },
                    Tag::Item => Frame::Item,
                    Tag::BlockQuote(_) => Frame::Quote,
                    Tag::CodeBlock(CodeBlockKind::Fenced(_) | CodeBlockKind::Indented) => {
                        Frame::CodeBlock
                    }
                    Tag::HtmlBlock => Frame::HtmlBlock,
                    Tag::Emphasis => Frame::Inline("em"),
                    Tag::Strong => Frame::Inline("strong"),
                    Tag::Strikethrough => Frame::Inline("strike"),
                    Tag::Link { dest_url, .. } => Frame::Link(safe_url(&dest_url)),
                    _ => Frame::Transparent,
                };
                stack.push((frame, Vec::new()));
            }
            Event::End(_) => {
                if stack.len() == 1 {
                    continue;
                }
                let Some((frame, children)) = stack.pop() else {
                    continue;
                };
                let parent = &mut stack.last_mut().expect("root stays").1;
                match frame {
                    Frame::Root => {}
                    Frame::Paragraph => parent.push(json!({"t": "p", "c": children})),
                    Frame::Heading(level) => {
                        parent.push(json!({"t": "h", "level": level, "c": children}))
                    }
                    Frame::List { ordered, start } => parent.push(
                        json!({"t": "list", "ordered": ordered, "start": start, "items": children}),
                    ),
                    Frame::Item => parent.push(Value::Array(blocks(children))),
                    Frame::Quote => parent.push(json!({"t": "quote", "c": blocks(children)})),
                    Frame::CodeBlock => {
                        let text = text_of(&children);
                        parent.push(json!({"t": "codeblock", "text": text.trim_end_matches('\n')}))
                    }
                    Frame::HtmlBlock => {
                        let text = text_of(&children);
                        parent.push(json!({"t": "p", "c": [text.trim_end_matches('\n')]}))
                    }
                    Frame::Inline(t) => parent.push(json!({"t": t, "c": children})),
                    Frame::Link(Some(url)) => {
                        parent.push(json!({"t": "link", "url": url, "c": children}))
                    }
                    Frame::Link(None) | Frame::Transparent => {
                        for child in children {
                            push(parent, child);
                        }
                    }
                }
            }
            Event::Text(text) | Event::Html(text) | Event::InlineHtml(text) => {
                let top = &mut stack.last_mut().expect("root stays").1;
                push(top, Value::String(text.to_string()));
            }
            Event::Code(code) => {
                let top = &mut stack.last_mut().expect("root stays").1;
                top.push(json!({"t": "code", "text": code.to_string()}));
            }
            Event::SoftBreak => {
                let top = &mut stack.last_mut().expect("root stays").1;
                push(top, Value::String(" ".into()));
            }
            Event::HardBreak => {
                let top = &mut stack.last_mut().expect("root stays").1;
                top.push(json!({"t": "br"}));
            }
            Event::Rule => {
                let top = &mut stack.last_mut().expect("root stays").1;
                top.push(json!({"t": "hr"}));
            }
            _ => {}
        }
    }
    let root = stack.into_iter().next().map(|(_, c)| c).unwrap_or_default();
    Value::Array(blocks(root))
}

/// The `_md` map for a set of fields: the parsed form of each text field.
pub fn trees<'a>(
    fields: &Map<String, Value>,
    text_fields: impl Iterator<Item = &'a String>,
) -> Map<String, Value> {
    text_fields
        .filter_map(|name| {
            let text = fields.get(name)?.as_str()?;
            Some((name.clone(), tree(text)))
        })
        .collect()
}

pub fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn inline_html(node: &Value, out: &mut String) {
    let kids = |out: &mut String| {
        for child in node["c"].as_array().into_iter().flatten() {
            inline_html(child, out);
        }
    };
    match node {
        Value::String(s) => out.push_str(&escape(s)),
        _ => match node["t"].as_str().unwrap_or_default() {
            tag @ ("em" | "strong") => {
                out.push_str(&format!("<{tag}>"));
                kids(out);
                out.push_str(&format!("</{tag}>"));
            }
            "strike" => {
                out.push_str("<s>");
                kids(out);
                out.push_str("</s>");
            }
            "code" => out.push_str(&format!(
                "<code>{}</code>",
                escape(node["text"].as_str().unwrap_or_default())
            )),
            "link" => {
                out.push_str(&format!(
                    "<a href=\"{}\" rel=\"noreferrer noopener\">",
                    escape(node["url"].as_str().unwrap_or_default())
                ));
                kids(out);
                out.push_str("</a>");
            }
            "br" => out.push_str("<br>"),
            _ => kids(out),
        },
    }
}

fn blocks_html(nodes: &Value, heading_offset: u8, out: &mut String) {
    for node in nodes.as_array().into_iter().flatten() {
        let inlines = |out: &mut String| {
            for child in node["c"].as_array().into_iter().flatten() {
                inline_html(child, out);
            }
        };
        match node["t"].as_str().unwrap_or_default() {
            "p" => {
                out.push_str("<p>");
                inlines(out);
                out.push_str("</p>\n");
            }
            "h" => {
                let level = (node["level"].as_u64().unwrap_or(1) as u8 + heading_offset).min(6);
                out.push_str(&format!("<h{level}>"));
                inlines(out);
                out.push_str(&format!("</h{level}>\n"));
            }
            "list" => {
                let ordered = node["ordered"].as_bool().unwrap_or(false);
                let start = node["start"].as_u64().unwrap_or(1);
                out.push_str(&if ordered && start != 1 {
                    format!("<ol start=\"{start}\">\n")
                } else if ordered {
                    "<ol>\n".into()
                } else {
                    "<ul>\n".into()
                });
                for item in node["items"].as_array().into_iter().flatten() {
                    out.push_str("<li>");
                    blocks_html(item, heading_offset, out);
                    out.push_str("</li>\n");
                }
                out.push_str(if ordered { "</ol>\n" } else { "</ul>\n" });
            }
            "quote" => {
                out.push_str("<blockquote>\n");
                blocks_html(&node["c"], heading_offset, out);
                out.push_str("</blockquote>\n");
            }
            "codeblock" => out.push_str(&format!(
                "<pre><code>{}</code></pre>\n",
                escape(node["text"].as_str().unwrap_or_default())
            )),
            "hr" => out.push_str("<hr>\n"),
            _ => {}
        }
    }
}

/// Markdown as escaped HTML. Markdown headings start `heading_offset`
/// levels down, below the page's own headings.
pub fn html(text: &str, heading_offset: u8) -> String {
    let mut out = String::new();
    blocks_html(&tree(text), heading_offset, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_blocks_and_inlines() {
        let t =
            tree("# Scope\n\nWe ship *two* things:\n\n- a **form**\n- `code`\n\n1. one\n2. two");
        assert_eq!(
            t,
            json!([
                {"t": "h", "level": 1, "c": ["Scope"]},
                {"t": "p", "c": ["We ship ", {"t": "em", "c": ["two"]}, " things:"]},
                {"t": "list", "ordered": false, "start": 1, "items": [
                    [{"t": "p", "c": ["a ", {"t": "strong", "c": ["form"]}]}],
                    [{"t": "p", "c": [{"t": "code", "text": "code"}]}],
                ]},
                {"t": "list", "ordered": true, "start": 1, "items": [
                    [{"t": "p", "c": ["one"]}],
                    [{"t": "p", "c": ["two"]}],
                ]},
            ])
        );
    }

    #[test]
    fn plain_text_stays_one_paragraph_per_block() {
        assert_eq!(
            tree("Line one\nline two\n\nNext"),
            json!([{"t": "p", "c": ["Line one line two"]}, {"t": "p", "c": ["Next"]}])
        );
        assert_eq!(tree(""), json!([]));
    }

    #[test]
    fn html_is_escaped_and_only_safe_links_survive() {
        let out = html(
            "<script>alert(1)</script>\n\nSee [docs](https://example.com) and [x](javascript:alert(1)) <b>bold</b>",
            1,
        );
        assert!(!out.contains("<script>"), "{out}");
        assert!(out.contains("&lt;script&gt;"), "{out}");
        assert!(out.contains("&lt;b&gt;bold&lt;/b&gt;"), "{out}");
        assert!(out.contains("<a href=\"https://example.com\""), "{out}");
        assert!(!out.contains("javascript"), "{out}");
        assert!(html("# H", 1).contains("<h2>H</h2>"));
    }
}
