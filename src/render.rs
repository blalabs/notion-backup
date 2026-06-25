//! Render a dumped JSON tree into Markdown pages and CSV database indexes.
//!
//! Consumes the output of [`crate::dump`]; it never touches the network, so it
//! can be re-run offline after tweaking the converter. The Markdown tree mirrors
//! the JSON tree: every node gets an `index.md` (with YAML front-matter holding
//! the object's properties) and every `database.json` additionally produces
//! `_index.csv`. Links between pages are rewritten to relative paths so the
//! rendered tree is self-navigable.

use std::collections::HashMap;
use std::fs;
use std::path::{Component, Path, PathBuf};

use anyhow::{anyhow, Result};
use serde_json::Value;

use crate::blocks::{blocks_to_md, Resolver};
use crate::dump::{DATABASE_FILE, PAGE_FILE};
use crate::util::{database_title, normalize_notion_id, page_title, property_to_text};

struct NodeInfo {
    path: PathBuf,
    title: String,
}

/// Reads a JSON tree from disk and writes a Markdown/CSV mirror.
pub struct MarkdownRenderer {
    json_dir: PathBuf,
    markdown_dir: PathBuf,
    // normalized object id -> {path to index.md, title}
    nodes: HashMap<String, NodeInfo>,
}

impl MarkdownRenderer {
    pub fn new(json_dir: PathBuf, markdown_dir: PathBuf) -> Self {
        MarkdownRenderer {
            json_dir,
            markdown_dir,
            nodes: HashMap::new(),
        }
    }

    pub fn run(&mut self) -> Result<()> {
        if !self.json_dir.exists() {
            return Err(anyhow!(
                "No JSON tree at {}; run the dump stage first.",
                self.json_dir.display()
            ));
        }
        if self.markdown_dir.exists() {
            fs::remove_dir_all(&self.markdown_dir)?;
        }
        fs::create_dir_all(&self.markdown_dir)?;

        // First pass: map every object id to where its index.md will live, so
        // links between pages can be resolved to relative paths in the second.
        self.build_index();
        for node in child_node_dirs(&self.json_dir) {
            let dst = self.markdown_dir.join(node.file_name().unwrap());
            self.render_node(&node, &dst)?;
        }
        self.write_readme()
    }

    // -- link index --------------------------------------------------------

    fn build_index(&mut self) {
        for (node_file, dst_dir) in self.iter_nodes() {
            let data = match load_json(&node_file) {
                Ok(payload) => payload.get("data").cloned().unwrap_or(Value::Null),
                Err(_) => continue,
            };
            let obj_id = match normalize_notion_id(data.get("id").and_then(Value::as_str)) {
                Some(id) => id,
                None => continue,
            };
            let mut title = page_title(&data);
            if title.is_empty() {
                title = database_title(&data);
            }
            if title.is_empty() {
                title = "untitled".to_string();
            }
            self.nodes.insert(
                obj_id,
                NodeInfo {
                    path: dst_dir.join("index.md"),
                    title,
                },
            );
        }
    }

    /// Yield (json node file, corresponding markdown dir) for every node.
    fn iter_nodes(&self) -> Vec<(PathBuf, PathBuf)> {
        let mut out: Vec<(PathBuf, PathBuf)> = Vec::new();
        for filename in [PAGE_FILE, DATABASE_FILE] {
            collect_node_files(&self.json_dir, filename, &mut |node_file| {
                let parent = node_file.parent().unwrap_or(&self.json_dir);
                let rel = parent.strip_prefix(&self.json_dir).unwrap_or(parent);
                out.push((node_file.to_path_buf(), self.markdown_dir.join(rel)));
            });
        }
        out
    }

    fn resolver_for<'b>(
        &'b self,
        current_dir: &'b Path,
    ) -> impl Fn(&str) -> Option<(String, String)> + 'b {
        move |reference: &str| {
            let node_id = normalize_notion_id(Some(reference))?;
            let info = self.nodes.get(&node_id)?;
            let rel = relative_path(current_dir, &info.path);
            Some((encode_path(&rel), info.title.clone()))
        }
    }

    // -- rendering ---------------------------------------------------------

    /// Write an index README linking to each top-level (root) page/database.
    fn write_readme(&self) -> Result<()> {
        let mut entries: Vec<(String, &str, String)> = Vec::new(); // (title, icon, link)
        for node in child_node_dirs(&self.json_dir) {
            let name = node.file_name().and_then(|n| n.to_str()).unwrap_or("");
            let (icon, title) = if node.join(PAGE_FILE).exists() {
                let data = node_data(&node.join(PAGE_FILE));
                ("\u{1F4C4}", or_untitled(page_title(&data), "untitled"))
            } else if node.join(DATABASE_FILE).exists() {
                let data = node_data(&node.join(DATABASE_FILE));
                (
                    "\u{1F4CA}",
                    or_untitled(database_title(&data), "untitled-database"),
                )
            } else {
                continue;
            };
            entries.push((title, icon, encode_path(&format!("{name}/index.md"))));
        }

        entries.sort_by(|a, b| a.0.to_lowercase().cmp(&b.0.to_lowercase()));
        let mut lines = vec![
            "# Notion backup".to_string(),
            String::new(),
            "Markdown render of the Notion workspace. Root pages:".to_string(),
            String::new(),
        ];
        if entries.is_empty() {
            lines.push("_No pages found._".to_string());
        } else {
            for (title, icon, link) in &entries {
                lines.push(format!("- {icon} [{}]({link})", md_label(title)));
            }
        }
        fs::write(
            self.markdown_dir.join("README.md"),
            format!("{}\n", lines.join("\n")),
        )?;
        Ok(())
    }

    fn render_node(&self, src: &Path, dst: &Path) -> Result<()> {
        fs::create_dir_all(dst)?;

        let page_file = src.join(PAGE_FILE);
        let db_file = src.join(DATABASE_FILE);
        if page_file.exists() {
            self.render_page(&load_json(&page_file)?, dst)?;
        } else if db_file.exists() {
            self.render_database(&load_json(&db_file)?, src, dst)?;
        }

        for child in child_node_dirs(src) {
            let child_dst = dst.join(child.file_name().unwrap());
            self.render_node(&child, &child_dst)?;
        }
        Ok(())
    }

    fn render_page(&self, node: &Value, dst: &Path) -> Result<()> {
        let page = node.get("data").cloned().unwrap_or(Value::Null);
        let blocks = node
            .get("blocks")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let title = or_untitled(page_title(&page), "untitled");
        let resolver = self.resolver_for(dst);
        let resolve: Resolver = &resolver;
        let body = blocks_to_md(&blocks, 0, Some(resolve));
        let front_matter = front_matter(&page, &title);
        fs::write(
            dst.join("index.md"),
            format!("{front_matter}\n# {title}\n\n{body}\n"),
        )?;
        Ok(())
    }

    fn render_database(&self, node: &Value, src: &Path, dst: &Path) -> Result<()> {
        let db = node.get("data").cloned().unwrap_or(Value::Null);
        let title = or_untitled(database_title(&db), "untitled-database");

        // Rows are child page nodes; row_order preserves the queried order.
        let mut rows: Vec<(String, Value)> = Vec::new();
        for row_name in node
            .get("row_order")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
        {
            let row_file = src.join(row_name).join(PAGE_FILE);
            if row_file.exists() {
                rows.push((row_name.to_string(), node_data(&row_file)));
            }
        }

        write_csv(
            &dst.join("_index.csv"),
            &rows
                .iter()
                .map(|(_, data)| data.clone())
                .collect::<Vec<_>>(),
        )?;

        let front_matter = front_matter(&db, &title);
        let intro = format!(
            "{} rows - full data in [`_index.csv`](_index.csv).",
            rows.len()
        );
        let table = render_table(&rows);
        fs::write(
            dst.join("index.md"),
            format!("{front_matter}\n# {title}\n\n{intro}\n\n{table}\n"),
        )?;
        Ok(())
    }
}

// -- module-level helpers ---------------------------------------------------

fn or_untitled(title: String, fallback: &str) -> String {
    if title.is_empty() {
        fallback.to_string()
    } else {
        title
    }
}

fn node_data(path: &Path) -> Value {
    load_json(path)
        .ok()
        .and_then(|p| p.get("data").cloned())
        .unwrap_or(Value::Null)
}

fn child_node_dirs(directory: &Path) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = match fs::read_dir(directory) {
        Ok(entries) => entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.is_dir()
                    && p.file_name()
                        .and_then(|n| n.to_str())
                        .map(|n| !n.starts_with('.'))
                        .unwrap_or(false)
            })
            .collect(),
        Err(_) => Vec::new(),
    };
    dirs.sort();
    dirs
}

/// Recursively collect files named `filename`, visiting them in sorted order.
fn collect_node_files(dir: &Path, filename: &str, visit: &mut dyn FnMut(&Path)) {
    let mut entries: Vec<PathBuf> = match fs::read_dir(dir) {
        Ok(e) => e.flatten().map(|e| e.path()).collect(),
        Err(_) => return,
    };
    entries.sort();
    for path in entries {
        if path.is_dir() {
            collect_node_files(&path, filename, visit);
        } else if path.file_name().and_then(|n| n.to_str()) == Some(filename) {
            visit(&path);
        }
    }
}

fn load_json(path: &Path) -> Result<Value> {
    let text = fs::read_to_string(path)?;
    Ok(serde_json::from_str(&text)?)
}

/// Compute a relative path from `base_dir` to `target`, using `/` separators.
fn relative_path(base_dir: &Path, target: &Path) -> String {
    let base: Vec<&str> = path_components(base_dir);
    let target_parts: Vec<&str> = path_components(target);

    let common = base
        .iter()
        .zip(target_parts.iter())
        .take_while(|(a, b)| a == b)
        .count();

    let mut parts: Vec<String> = Vec::new();
    for _ in common..base.len() {
        parts.push("..".to_string());
    }
    for part in &target_parts[common..] {
        parts.push((*part).to_string());
    }
    if parts.is_empty() {
        ".".to_string()
    } else {
        parts.join("/")
    }
}

fn path_components(path: &Path) -> Vec<&str> {
    path.components()
        .filter_map(|c| match c {
            Component::Normal(s) => s.to_str(),
            Component::RootDir => Some("/"),
            _ => None,
        })
        .collect()
}

/// Percent-encode a relative link path so spaces/specials are Markdown-safe.
/// Mirrors Python's `urllib.parse.quote(path, safe="/")`.
fn encode_path(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for byte in path.bytes() {
        let keep = byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-' | b'~' | b'/');
        if keep {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// Escape a value for use inside a Markdown table cell.
fn cell(text: &str) -> String {
    text.replace(['\r', '\n'], " ").replace('|', "\\|")
}

/// Escape brackets so a value is safe as Markdown link text.
fn md_label(text: &str) -> String {
    text.replace('[', "\\[").replace(']', "\\]")
}

/// Ordered union of non-title property names across all rows.
fn value_columns(rows: &[(String, Value)]) -> Vec<String> {
    let mut columns: Vec<String> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for (_, data) in rows {
        if let Some(props) = data.get("properties").and_then(Value::as_object) {
            for (name, prop) in props {
                if prop.get("type").and_then(Value::as_str) == Some("title") {
                    continue;
                }
                if seen.insert(name.clone()) {
                    columns.push(name.clone());
                }
            }
        }
    }
    columns
}

/// Render database rows as a Markdown table, linking each row to its page.
fn render_table(rows: &[(String, Value)]) -> String {
    if rows.is_empty() {
        return "_No rows._".to_string();
    }
    let columns = value_columns(rows);
    let mut header = vec!["Name".to_string()];
    header.extend(columns.iter().cloned());
    let mut lines = vec![
        format!(
            "| {} |",
            header
                .iter()
                .map(|h| cell(h))
                .collect::<Vec<_>>()
                .join(" | ")
        ),
        format!(
            "| {} |",
            header.iter().map(|_| "---").collect::<Vec<_>>().join(" | ")
        ),
    ];
    for (row_name, data) in rows {
        let title = or_untitled(page_title(data), "Untitled");
        let link = encode_path(&format!("{row_name}/index.md"));
        let mut cells = vec![format!("[{}]({link})", md_label(&cell(&title)))];
        let props = data.get("properties");
        for c in &columns {
            let value = props
                .and_then(|p| p.get(c))
                .map(|prop| cell(&property_to_text(prop)))
                .unwrap_or_default();
            cells.push(value);
        }
        lines.push(format!("| {} |", cells.join(" | ")));
    }
    lines.join("\n")
}

/// Render a value as a safely double-quoted YAML scalar.
fn yaml_scalar(value: &str) -> String {
    let escaped = value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\r', "\\r")
        .replace('\n', "\\n")
        .replace('\t', "\\t");
    format!("\"{escaped}\"")
}

fn front_matter(data: &Value, title: &str) -> String {
    let field = |key: &str| yaml_scalar(data.get(key).and_then(Value::as_str).unwrap_or(""));
    let mut lines = vec![
        "---".to_string(),
        format!("title: {}", yaml_scalar(title)),
        format!("id: {}", field("id")),
        format!("object: {}", field("object")),
        format!("url: {}", field("url")),
        format!("created_time: {}", field("created_time")),
        format!("last_edited_time: {}", field("last_edited_time")),
    ];

    // Pages carry property values worth surfacing; the title-typed one is
    // already the front-matter title, so skip it. Databases hold only a schema.
    if data.get("object").and_then(Value::as_str) == Some("page") {
        if let Some(props) = data.get("properties").and_then(Value::as_object) {
            let mut prop_lines: Vec<String> = Vec::new();
            for (name, prop) in props {
                if prop.get("type").and_then(Value::as_str) == Some("title") {
                    continue;
                }
                prop_lines.push(format!(
                    "  {}: {}",
                    yaml_scalar(name),
                    yaml_scalar(&property_to_text(prop))
                ));
            }
            if !prop_lines.is_empty() {
                lines.push("properties:".to_string());
                lines.extend(prop_lines);
            }
        }
    }

    lines.push("---".to_string());
    lines.join("\n")
}

fn write_csv(path: &Path, rows: &[Value]) -> Result<()> {
    if rows.is_empty() {
        fs::write(path, "")?;
        return Ok(());
    }
    // Union of all property names preserves columns even if some rows omit a
    // value; iteration order gives a stable column ordering.
    let mut columns: Vec<String> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for row in rows {
        if let Some(props) = row.get("properties").and_then(Value::as_object) {
            for name in props.keys() {
                if seen.insert(name.clone()) {
                    columns.push(name.clone());
                }
            }
        }
    }

    let mut out = String::new();
    let mut header = vec!["_url".to_string()];
    header.extend(columns.iter().cloned());
    out.push_str(&csv_row(&header));
    for row in rows {
        let mut record = vec![row
            .get("url")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()];
        let props = row.get("properties");
        for name in &columns {
            let value = props
                .and_then(|p| p.get(name))
                .map(property_to_text)
                .unwrap_or_default();
            record.push(value);
        }
        out.push_str(&csv_row(&record));
    }
    fs::write(path, out)?;
    Ok(())
}

/// Format one CSV record, RFC 4180 quoting and `\r\n` terminated (matching
/// Python's `csv.writer` defaults).
fn csv_row(fields: &[String]) -> String {
    let encoded: Vec<String> = fields.iter().map(|f| csv_field(f)).collect();
    format!("{}\r\n", encoded.join(","))
}

fn csv_field(field: &str) -> String {
    if field.contains(',') || field.contains('"') || field.contains('\n') || field.contains('\r') {
        format!("\"{}\"", field.replace('"', "\"\""))
    } else {
        field.to_string()
    }
}
