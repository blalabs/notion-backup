//! Unit tests for the block -> Markdown converter.

use notion_backup::blocks::{
    block_to_md, blocks_to_md, plain_text, rich_text_to_md, Resolver, CHILD_KEY,
};
use serde_json::{json, Value};

fn block(btype: &str, data: Value) -> Value {
    json!({ "type": btype, btype: data })
}

#[test]
fn rich_text_annotations() {
    let spans = json!([
        {"plain_text": "bold", "annotations": {"bold": true}},
        {"plain_text": " and ", "annotations": {}},
        {"plain_text": "code", "annotations": {"code": true}}
    ]);
    assert_eq!(rich_text_to_md(Some(&spans), None), "**bold** and `code`");
}

#[test]
fn rich_text_link() {
    let spans = json!([{"plain_text": "SPL", "annotations": {}, "href": "https://spl.nl"}]);
    assert_eq!(rich_text_to_md(Some(&spans), None), "[SPL](https://spl.nl)");
}

#[test]
fn plain_text_strips_formatting() {
    let spans = json!([{"plain_text": "Hello "}, {"plain_text": "world"}]);
    assert_eq!(plain_text(Some(&spans)), "Hello world");
}

#[test]
fn headings() {
    let b = block(
        "heading_2",
        json!({"rich_text": [{"plain_text": "Title", "annotations": {}}]}),
    );
    assert_eq!(block_to_md(&b, 0, None), "## Title");
}

#[test]
fn bulleted_list_item() {
    let b = block(
        "bulleted_list_item",
        json!({"rich_text": [{"plain_text": "item", "annotations": {}}]}),
    );
    assert_eq!(block_to_md(&b, 0, None), "- item");
}

#[test]
fn to_do_checked() {
    let b = block(
        "to_do",
        json!({"checked": true, "rich_text": [{"plain_text": "done", "annotations": {}}]}),
    );
    assert_eq!(block_to_md(&b, 0, None), "- [x] done");
}

#[test]
fn code_block_fenced() {
    let b = block(
        "code",
        json!({"language": "python", "rich_text": [{"plain_text": "print(1)"}]}),
    );
    assert_eq!(block_to_md(&b, 0, None), "```python\nprint(1)\n```");
}

#[test]
fn nested_children_indented() {
    let mut parent = block(
        "bulleted_list_item",
        json!({"rich_text": [{"plain_text": "parent", "annotations": {}}]}),
    );
    parent[CHILD_KEY] = json!([block(
        "bulleted_list_item",
        json!({"rich_text": [{"plain_text": "child", "annotations": {}}]})
    )]);
    assert_eq!(block_to_md(&parent, 0, None), "- parent\n  - child");
}

#[test]
fn table_renders_with_separator() {
    let mut table = block("table", json!({"has_column_header": true}));
    table[CHILD_KEY] = json!([
        {"type": "table_row", "table_row": {"cells": [
            [{"plain_text": "A", "annotations": {}}],
            [{"plain_text": "B", "annotations": {}}]
        ]}},
        {"type": "table_row", "table_row": {"cells": [
            [{"plain_text": "1", "annotations": {}}],
            [{"plain_text": "2", "annotations": {}}]
        ]}}
    ]);
    assert_eq!(
        block_to_md(&table, 0, None),
        "| A | B |\n| --- | --- |\n| 1 | 2 |"
    );
}

fn resolver() -> Resolver<'static> {
    // Pretend any reference mentioning "tid" is a known local page.
    &|reference: &str| {
        if reference.contains("tid") {
            Some(("../Target/index.md".to_string(), "Target Page".to_string()))
        } else {
            None
        }
    }
}

#[test]
fn rich_text_link_localized_with_resolver() {
    let spans =
        json!([{"plain_text": "see", "annotations": {}, "href": "https://notion.so/x-tid"}]);
    assert_eq!(
        rich_text_to_md(Some(&spans), Some(resolver())),
        "[see](../Target/index.md)"
    );
}

#[test]
fn rich_text_link_external_kept_without_resolver() {
    let spans = json!([{"plain_text": "see", "annotations": {}, "href": "https://example.com/x"}]);
    assert_eq!(
        rich_text_to_md(Some(&spans), None),
        "[see](https://example.com/x)"
    );
}

#[test]
fn child_page_links_with_resolver() {
    let b = json!({"type": "child_page", "id": "tid", "child_page": {"title": "Sub"}});
    assert_eq!(
        block_to_md(&b, 0, Some(resolver())),
        "- \u{1F4C4} [Sub](../Target/index.md)"
    );
}

#[test]
fn child_page_plain_without_resolver() {
    let b = json!({"type": "child_page", "id": "tid", "child_page": {"title": "Sub"}});
    assert_eq!(block_to_md(&b, 0, None), "- \u{1F4C4} Sub");
}

#[test]
fn link_to_page_uses_target_title() {
    let b = json!({"type": "link_to_page", "link_to_page": {"type": "page_id", "page_id": "tid"}});
    assert_eq!(
        block_to_md(&b, 0, Some(resolver())),
        "- \u{1F517} [Target Page](../Target/index.md)"
    );
}

#[test]
fn unknown_block_falls_back_to_comment() {
    let b = block("some_future_type", json!({"foo": "bar"}));
    assert_eq!(
        block_to_md(&b, 0, None),
        "<!-- unhandled block type: some_future_type -->"
    );
}

#[test]
fn blocks_to_md_joins_and_skips_empty() {
    let blocks = vec![
        block(
            "paragraph",
            json!({"rich_text": [{"plain_text": "one", "annotations": {}}]}),
        ),
        block("paragraph", json!({"rich_text": []})), // empty -> skipped
        block(
            "paragraph",
            json!({"rich_text": [{"plain_text": "two", "annotations": {}}]}),
        ),
    ];
    assert_eq!(blocks_to_md(&blocks, 0, None), "one\ntwo");
}
