//! Pure helpers shared by the dump (JSON) and render (Markdown/CSV) stages.

use std::sync::OnceLock;

use regex::Regex;
use serde_json::Value;

use crate::blocks::plain_text;

const MAX_SLUG_LEN: usize = 80;

fn unsafe_chars() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r#"[\\/:*?"<>|\x00-\x1f]"#).unwrap())
}

fn whitespace() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\s+").unwrap())
}

/// Extract a Notion object id (32 hex chars, dashes stripped) from an id or URL.
///
/// Handles raw UUIDs, full page URLs ending in the id, and links like
/// `/Page-Title-<id>?v=…`. Returns `None` if no id can be found.
pub fn normalize_notion_id(value: Option<&str>) -> Option<String> {
    let value = value?;
    if value.is_empty() {
        return None;
    }
    let head = value.split('?').next().unwrap_or("");
    let head = head.split('#').next().unwrap_or("");
    let hex_only: String = head.chars().filter(|c| c.is_ascii_hexdigit()).collect();
    if hex_only.len() >= 32 {
        Some(hex_only[hex_only.len() - 32..].to_lowercase())
    } else {
        None
    }
}

/// Turn a Notion title into a filesystem-safe directory name.
pub fn slugify(title: &str) -> String {
    let cleaned = unsafe_chars().replace_all(title, " ");
    let collapsed = whitespace().replace_all(&cleaned, " ");
    let trimmed = collapsed.trim().trim_matches('.');
    let truncated: String = trimmed.chars().take(MAX_SLUG_LEN).collect();
    let result = truncated.trim();
    if result.is_empty() {
        "untitled".to_string()
    } else {
        result.to_string()
    }
}

/// Extract the title of a page from its properties.
pub fn page_title(page: &Value) -> String {
    if let Some(props) = page.get("properties").and_then(Value::as_object) {
        for prop in props.values() {
            if prop.get("type").and_then(Value::as_str) == Some("title") {
                return plain_text(prop.get("title"));
            }
        }
    }
    String::new()
}

/// Extract the title of a database.
pub fn database_title(db: &Value) -> String {
    plain_text(db.get("title"))
}

/// Flatten a single Notion property value to a CSV-friendly string.
///
/// Every branch is defensive: the value may arrive in an unexpected shape, and
/// rendering is best-effort, so it degrades to a string rather than panicking.
pub fn property_to_text(prop: &Value) -> String {
    let ptype = prop.get("type").and_then(Value::as_str).unwrap_or("");
    let value = prop.get(ptype);

    match ptype {
        "title" | "rich_text" => plain_text(value),
        "number" => value.map(scalar_str).unwrap_or_default(),
        "select" | "status" => obj_field(value, "name"),
        "multi_select" => join_field(value, "name", ", "),
        "date" => date_to_text(value),
        "checkbox" => bool_str(value.and_then(Value::as_bool).unwrap_or(false)),
        "url" | "email" | "phone_number" => value.and_then(Value::as_str).unwrap_or("").to_string(),
        "people" => join_name_or_id(value),
        "files" => join_field(value, "name", ", "),
        "relation" => join_field(value, "id", ", "),
        "created_time" | "last_edited_time" => {
            value.and_then(Value::as_str).unwrap_or("").to_string()
        }
        "created_by" | "last_edited_by" => name_or_id(value),
        "unique_id" => unique_id_to_text(value),
        "formula" => formula_to_text(value),
        "rollup" => rollup_to_text(value),
        "verification" => obj_field(value, "state"),
        _ => match value {
            None => String::new(),
            Some(v) if v.is_null() => String::new(),
            Some(v) => scalar_str(v),
        },
    }
}

/// Render a scalar value as Python's `str()` would: numbers bare, booleans
/// capitalised, strings verbatim.
fn scalar_str(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => {
            if *b {
                "True".to_string()
            } else {
                "False".to_string()
            }
        }
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

fn bool_str(value: bool) -> String {
    if value { "true" } else { "false" }.to_string()
}

/// Return only the object items of an array value (drops anything unexpected).
fn dicts(value: Option<&Value>) -> Vec<&Value> {
    match value.and_then(Value::as_array) {
        Some(arr) => arr.iter().filter(|item| item.is_object()).collect(),
        None => Vec::new(),
    }
}

fn obj_field(value: Option<&Value>, field: &str) -> String {
    value
        .and_then(|v| v.get(field))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

fn join_field(value: Option<&Value>, field: &str, sep: &str) -> String {
    dicts(value)
        .iter()
        .map(|item| item.get(field).and_then(Value::as_str).unwrap_or(""))
        .collect::<Vec<_>>()
        .join(sep)
}

fn name_or_id(value: Option<&Value>) -> String {
    value
        .map(|v| {
            v.get("name")
                .and_then(Value::as_str)
                .or_else(|| v.get("id").and_then(Value::as_str))
                .unwrap_or("")
                .to_string()
        })
        .filter(|_| value.map(Value::is_object).unwrap_or(false))
        .unwrap_or_default()
}

fn join_name_or_id(value: Option<&Value>) -> String {
    dicts(value)
        .iter()
        .map(|item| {
            item.get("name")
                .and_then(Value::as_str)
                .or_else(|| item.get("id").and_then(Value::as_str))
                .unwrap_or("")
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn unique_id_to_text(value: Option<&Value>) -> String {
    let Some(obj) = value.filter(|v| v.is_object()) else {
        return String::new();
    };
    let prefix = obj.get("prefix").and_then(Value::as_str).unwrap_or("");
    let number = obj
        .get("number")
        .filter(|v| !v.is_null())
        .map(scalar_str)
        .unwrap_or_default();
    format!("{prefix}{number}")
}

fn date_to_text(value: Option<&Value>) -> String {
    let Some(obj) = value.filter(|v| v.is_object()) else {
        return String::new();
    };
    let start = obj.get("start").and_then(Value::as_str).unwrap_or("");
    match obj.get("end").and_then(Value::as_str) {
        Some(end) if !end.is_empty() => format!("{start} \u{2192} {end}"),
        _ => start.to_string(),
    }
}

fn formula_to_text(value: Option<&Value>) -> String {
    let Some(obj) = value.filter(|v| v.is_object()) else {
        return String::new();
    };
    let ftype = obj.get("type").and_then(Value::as_str).unwrap_or("");
    let inner = obj.get(ftype);
    match ftype {
        "date" => date_to_text(inner),
        "boolean" => bool_str(inner.and_then(Value::as_bool).unwrap_or(false)),
        _ => match inner {
            None => String::new(),
            Some(v) if v.is_null() => String::new(),
            Some(v) => scalar_str(v),
        },
    }
}

fn rollup_to_text(value: Option<&Value>) -> String {
    let Some(obj) = value.filter(|v| v.is_object()) else {
        return String::new();
    };
    let rtype = obj.get("type").and_then(Value::as_str).unwrap_or("");
    match rtype {
        "array" => obj
            .get("array")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter(|item| item.is_object())
                    .map(property_to_text)
                    .collect::<Vec<_>>()
                    .join("; ")
            })
            .unwrap_or_default(),
        "date" => date_to_text(obj.get("date")),
        _ => match obj.get(rtype) {
            None => String::new(),
            Some(v) if v.is_null() => String::new(),
            Some(v) => scalar_str(v),
        },
    }
}
