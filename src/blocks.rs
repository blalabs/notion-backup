//! Convert Notion block JSON into Markdown.
//!
//! Pure functions only (no network/disk I/O) so they are trivially testable.
//! Child blocks are expected to be attached to their parent under the
//! [`CHILD_KEY`] key by the caller (see [`crate::dump`]); this keeps the
//! converter free of API calls.
//!
//! Page references (inline links, mentions, `child_page`/`child_database` and
//! `link_to_page` blocks) are localised through an optional `resolve` callback.
//! Given a Notion id or URL it returns `(relative_url, title)` for the target's
//! rendered Markdown file, or `None` when the target is unknown; passing no
//! resolver keeps the original Notion URLs / plain titles.

use serde_json::{Map, Value};

pub const CHILD_KEY: &str = "_children";
const INDENT: &str = "  ";

/// `resolve(notion_id_or_url) -> (relative_url, title)`.
pub type Resolver<'a> = &'a dyn Fn(&str) -> Option<(String, String)>;

/// Render a Notion rich-text array to Markdown, honoring annotations/links.
pub fn rich_text_to_md(rich_text: Option<&Value>, resolve: Option<Resolver>) -> String {
    let mut parts: Vec<String> = Vec::new();
    for span in rich_text.and_then(Value::as_array).into_iter().flatten() {
        let text = span.get("plain_text").and_then(Value::as_str).unwrap_or("");
        if text.is_empty() {
            continue;
        }
        let mut out = text.to_string();
        let ann = span.get("annotations");
        // Apply code first so the surrounding markers wrap the backticks.
        if ann_flag(ann, "code") {
            out = format!("`{out}`");
        }
        if ann_flag(ann, "bold") {
            out = format!("**{out}**");
        }
        if ann_flag(ann, "italic") {
            out = format!("*{out}*");
        }
        if ann_flag(ann, "strikethrough") {
            out = format!("~~{out}~~");
        }
        if let Some(target) = link_target(span, resolve) {
            out = format!("[{out}]({target})");
        }
        parts.push(out);
    }
    parts.concat()
}

fn ann_flag(ann: Option<&Value>, name: &str) -> bool {
    ann.and_then(|a| a.get(name))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// Return the URL a rich-text span should link to (localised if possible).
fn link_target(span: &Value, resolve: Option<Resolver>) -> Option<String> {
    let href = span.get("href").and_then(Value::as_str);
    // Page mentions carry the id under mention.page even when href is absent.
    let mention = span.get("mention");
    let mut reference = href;
    if mention.and_then(|m| m.get("type")).and_then(Value::as_str) == Some("page") {
        reference = mention
            .and_then(|m| m.get("page"))
            .and_then(|p| p.get("id"))
            .and_then(Value::as_str)
            .or(href);
    }
    let reference = reference?;
    if let Some(resolve) = resolve {
        if let Some((rel, _)) = resolve(reference) {
            return Some(rel);
        }
    }
    href.map(str::to_string)
}

/// Render a rich-text array to unformatted plain text.
pub fn plain_text(rich_text: Option<&Value>) -> String {
    rich_text
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|span| span.get("plain_text").and_then(Value::as_str))
        .collect()
}

/// Render a single block (and its attached `_children`) to Markdown.
pub fn block_to_md(block: &Value, depth: usize, resolve: Option<Resolver>) -> String {
    let btype = block.get("type").and_then(Value::as_str).unwrap_or("");
    let empty = Value::Object(Map::new());
    let data = match block.get(btype) {
        Some(v) if v.is_object() => v,
        _ => &empty,
    };
    let indent = INDENT.repeat(depth);

    let rich = |d: &Value| rich_text_to_md(d.get("rich_text"), resolve);

    let mut line: String = match btype {
        "paragraph" => format!("{indent}{}", rich(data)),
        "heading_1" => format!("# {}", rich(data)),
        "heading_2" => format!("## {}", rich(data)),
        "heading_3" => format!("### {}", rich(data)),
        "bulleted_list_item" => format!("{indent}- {}", rich(data)),
        "numbered_list_item" => format!("{indent}1. {}", rich(data)),
        "to_do" => {
            let checked = if data
                .get("checked")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                "x"
            } else {
                " "
            };
            format!("{indent}- [{checked}] {}", rich(data))
        }
        "toggle" => format!("{indent}- {}", rich(data)),
        "quote" => format!("{indent}> {}", rich(data)),
        "callout" => {
            let icon = data.get("icon");
            let emoji = if icon.and_then(|i| i.get("type")).and_then(Value::as_str) == Some("emoji")
            {
                icon.and_then(|i| i.get("emoji"))
                    .and_then(Value::as_str)
                    .unwrap_or("")
            } else {
                ""
            };
            let prefix = if emoji.is_empty() {
                String::new()
            } else {
                format!("{emoji} ")
            };
            format!("{indent}> {prefix}{}", rich(data))
        }
        "code" => {
            let language = data.get("language").and_then(Value::as_str).unwrap_or("");
            let code = plain_text(data.get("rich_text"));
            format!("```{language}\n{code}\n```")
        }
        "divider" => "---".to_string(),
        "image" | "file" | "video" | "pdf" => media_to_md(btype, data),
        "bookmark" => {
            let url = data.get("url").and_then(Value::as_str).unwrap_or("");
            let mut caption = rich_text_to_md(data.get("caption"), resolve);
            if caption.is_empty() {
                caption = url.to_string();
            }
            if url.is_empty() {
                String::new()
            } else {
                format!("[{caption}]({url})")
            }
        }
        "equation" => {
            let expr = data.get("expression").and_then(Value::as_str).unwrap_or("");
            format!("$$\n{expr}\n$$")
        }
        "table" => return table_to_md(block, resolve),
        "child_page" => ref_link(
            &indent,
            "\u{1F4C4}",
            block.get("id").and_then(Value::as_str),
            data.get("title").and_then(Value::as_str).unwrap_or(""),
            resolve,
        ),
        "child_database" => ref_link(
            &indent,
            "\u{1F4CA}",
            block.get("id").and_then(Value::as_str),
            data.get("title").and_then(Value::as_str).unwrap_or(""),
            resolve,
        ),
        "link_to_page" => {
            let ref_id = data
                .get("page_id")
                .and_then(Value::as_str)
                .or_else(|| data.get("database_id").and_then(Value::as_str));
            ref_link(&indent, "\u{1F517}", ref_id, "", resolve)
        }
        "unsupported" => format!("{indent}<!-- unsupported block -->"),
        _ => format!("{indent}<!-- unhandled block type: {btype} -->"),
    };

    if let Some(children) = block.get(CHILD_KEY).and_then(Value::as_array) {
        if !children.is_empty() && btype != "table" {
            let child_md = blocks_to_md_slice(children, depth + 1, resolve);
            if !child_md.is_empty() {
                line = if line.is_empty() {
                    child_md
                } else {
                    format!("{line}\n{child_md}")
                };
            }
        }
    }
    line
}

/// Render a reference to another page/database as a Markdown list item.
fn ref_link(
    indent: &str,
    icon: &str,
    notion_id: Option<&str>,
    title: &str,
    resolve: Option<Resolver>,
) -> String {
    if let (Some(resolve), Some(id)) = (resolve, notion_id) {
        if let Some((rel, target_title)) = resolve(id) {
            let label = if !title.is_empty() {
                title.to_string()
            } else if !target_title.is_empty() {
                target_title
            } else {
                "Untitled".to_string()
            };
            return format!("{indent}- {icon} [{label}]({rel})");
        }
    }
    if title.is_empty() {
        String::new()
    } else {
        format!("{indent}- {icon} {title}")
    }
}

fn media_to_md(btype: &str, data: &Value) -> String {
    let inner_type = data.get("type").and_then(Value::as_str);
    let file_obj = inner_type.and_then(|t| data.get(t));
    let url = file_obj
        .filter(|f| f.is_object())
        .and_then(|f| f.get("url"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let mut caption = plain_text(data.get("caption"));
    if caption.is_empty() {
        caption = btype.to_string();
    }
    if url.is_empty() {
        return format!("<!-- {btype} (no url) -->");
    }
    if btype == "image" {
        format!("![{caption}]({url})")
    } else {
        format!("[{caption}]({url})")
    }
}

/// Render a table block from its attached `table_row` children.
fn table_to_md(block: &Value, resolve: Option<Resolver>) -> String {
    let rows = block.get(CHILD_KEY).and_then(Value::as_array);
    let mut rendered: Vec<Vec<String>> = Vec::new();
    for row in rows.into_iter().flatten() {
        let cells = row
            .get("table_row")
            .and_then(|tr| tr.get("cells"))
            .and_then(Value::as_array);
        let row_cells = cells
            .into_iter()
            .flatten()
            .map(|cell| rich_text_to_md(Some(cell), resolve))
            .collect();
        rendered.push(row_cells);
    }
    if rendered.is_empty() {
        return String::new();
    }

    let width = rendered.iter().map(Vec::len).max().unwrap_or(0);
    let mut lines: Vec<String> = Vec::new();
    for (i, cells) in rendered.iter().enumerate() {
        let mut padded = cells.clone();
        padded.resize(width, String::new());
        lines.push(format!("| {} |", padded.join(" | ")));
        if i == 0 {
            // Always emit a separator after the first row so it renders as a
            // table; if there's no real header, the first row acts as one.
            let sep = vec!["---"; width].join(" | ");
            lines.push(format!("| {sep} |"));
        }
    }
    lines.join("\n")
}

/// Render a list of sibling blocks to a Markdown document fragment.
pub fn blocks_to_md(blocks: &[Value], depth: usize, resolve: Option<Resolver>) -> String {
    blocks_to_md_slice(blocks, depth, resolve)
}

fn blocks_to_md_slice(blocks: &[Value], depth: usize, resolve: Option<Resolver>) -> String {
    blocks
        .iter()
        .map(|block| block_to_md(block, depth, resolve))
        .filter(|chunk| !chunk.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}
