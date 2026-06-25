//! Integration test for the render stage: JSON tree on disk -> Markdown/CSV.

use std::fs;
use std::path::Path;

use notion_backup::render::MarkdownRenderer;
use serde_json::{json, Value};
use tempfile::tempdir;

fn title_prop(text: &str) -> Value {
    json!({"Name": {"type": "title", "title": [{"plain_text": text}]}})
}

fn write(path: &Path, payload: &Value) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, serde_json::to_string(payload).unwrap()).unwrap();
}

fn build_json_tree(json_dir: &Path) {
    write(
        &json_dir.join("My Page").join("page.json"),
        &json!({
            "object": "page",
            "data": {"id": "p1", "url": "https://notion.so/p1", "properties": title_prop("My Page")},
            "blocks": [{"type": "paragraph", "paragraph": {"rich_text": [{"plain_text": "Hello", "annotations": {}}]}}]
        }),
    );
    write(
        &json_dir.join("Tasks").join("database.json"),
        &json!({"object": "database", "data": {"id": "db1", "title": [{"plain_text": "Tasks"}]}, "row_order": ["Row A", "Row B"]}),
    );
    for (name, status) in [("Row A", "Open"), ("Row B", "Done")] {
        let mut props = title_prop(name);
        props["Status"] = json!({"type": "select", "select": {"name": status}});
        write(
            &json_dir.join("Tasks").join(name).join("page.json"),
            &json!({"object": "page", "data": {"id": name, "url": format!("https://notion.so/{name}"), "properties": props}, "blocks": []}),
        );
    }
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap()
}

#[test]
fn render_produces_markdown_and_csv() {
    let tmp = tempdir().unwrap();
    let json_dir = tmp.path().join("json");
    let markdown_dir = tmp.path().join("markdown");
    build_json_tree(&json_dir);

    MarkdownRenderer::new(json_dir, markdown_dir.clone())
        .run()
        .unwrap();

    let page_md = read(&markdown_dir.join("My Page").join("index.md"));
    assert!(page_md.contains("# My Page"));
    assert!(page_md.contains("Hello"));
    assert!(page_md.contains("id: \"p1\""));

    let csv_text = read(&markdown_dir.join("Tasks").join("_index.csv"));
    let lines: Vec<&str> = csv_text.lines().collect();
    assert_eq!(lines[0], "_url,Name,Status");
    assert_eq!(lines[1], "https://notion.so/Row A,Row A,Open");
    assert_eq!(lines[2], "https://notion.so/Row B,Row B,Done");

    // Rows are also rendered as their own Markdown pages.
    assert!(markdown_dir
        .join("Tasks")
        .join("Row A")
        .join("index.md")
        .exists());

    // A top-level README links to each root page/database.
    let readme = read(&markdown_dir.join("README.md"));
    assert!(readme.contains("# Notion backup"));
    assert!(readme.contains("[My Page](My%20Page/index.md)"));
    assert!(readme.contains("[Tasks](Tasks/index.md)"));

    // The database index.md renders rows as a Markdown table with URL-encoded links.
    let db_md = read(&markdown_dir.join("Tasks").join("index.md"));
    assert!(db_md.contains("| Name | Status |"));
    assert!(db_md.contains("| --- | --- |"));
    assert!(db_md.contains("[Row A](Row%20A/index.md)"));
    assert!(db_md.contains("| Open |"));
}

#[test]
fn resolved_links_are_url_encoded() {
    let tmp = tempdir().unwrap();
    let json_dir = tmp.path().join("json");
    let markdown_dir = tmp.path().join("markdown");
    let id_a = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let id_b = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    write(
        &json_dir.join("A").join("page.json"),
        &json!({"object": "page", "data": {"id": id_a, "object": "page", "properties": title_prop("A")},
         "blocks": [{"type": "paragraph", "paragraph": {"rich_text": [
             {"plain_text": "B", "annotations": {}, "href": format!("https://notion.so/{id_b}")}]}}]}),
    );
    write(
        &json_dir.join("B Folder").join("page.json"),
        &json!({"object": "page", "data": {"id": id_b, "object": "page", "properties": title_prop("B")}, "blocks": []}),
    );

    MarkdownRenderer::new(json_dir, markdown_dir.clone())
        .run()
        .unwrap();
    let md_a = read(&markdown_dir.join("A").join("index.md"));
    assert!(md_a.contains("[B](../B%20Folder/index.md)"));
}

#[test]
fn frontmatter_includes_properties_and_links_are_relative() {
    let tmp = tempdir().unwrap();
    let json_dir = tmp.path().join("json");
    let markdown_dir = tmp.path().join("markdown");
    let id_a = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let id_b = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    let mut props_a = title_prop("Page A");
    props_a["Status"] = json!({"type": "select", "select": {"name": "Active"}});
    write(
        &json_dir.join("A").join("page.json"),
        &json!({
            "object": "page",
            "data": {"id": id_a, "object": "page", "url": "https://notion.so/A", "properties": props_a},
            "blocks": [{"type": "paragraph", "paragraph": {"rich_text": [
                {"plain_text": "go to B", "annotations": {}, "href": format!("https://notion.so/B-{id_b}")}
            ]}}]
        }),
    );
    write(
        &json_dir.join("B").join("page.json"),
        &json!({"object": "page", "data": {"id": id_b, "object": "page", "url": "https://notion.so/B", "properties": title_prop("Page B")}, "blocks": []}),
    );

    MarkdownRenderer::new(json_dir, markdown_dir.clone())
        .run()
        .unwrap();
    let md_a = read(&markdown_dir.join("A").join("index.md"));

    assert!(md_a.contains("properties:"));
    assert!(md_a.contains("\"Status\": \"Active\""));
    assert!(md_a.contains("[go to B](../B/index.md)"));
}

#[test]
fn render_without_json_tree_raises() {
    let tmp = tempdir().unwrap();
    let result = MarkdownRenderer::new(tmp.path().join("missing"), tmp.path().join("md")).run();
    assert!(result.is_err());
}
