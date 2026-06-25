//! Unit tests for the shared pure helpers.

use notion_backup::util::{normalize_notion_id, page_title, property_to_text, slugify};
use serde_json::json;

#[test]
fn normalize_id_from_uuid() {
    assert_eq!(
        normalize_notion_id(Some("20a938a1-68e1-8045-a43f-c5dafa63e1cc")).as_deref(),
        Some("20a938a168e18045a43fc5dafa63e1cc")
    );
}

#[test]
fn normalize_id_from_url_strips_query() {
    let url = "https://www.notion.so/My-Page-20a938a168e18045a43fc5dafa63e1cc?v=abc123";
    assert_eq!(
        normalize_notion_id(Some(url)).as_deref(),
        Some("20a938a168e18045a43fc5dafa63e1cc")
    );
}

#[test]
fn normalize_id_returns_none_for_non_id() {
    assert_eq!(normalize_notion_id(Some("just text")), None);
    assert_eq!(normalize_notion_id(Some("")), None);
    assert_eq!(normalize_notion_id(None), None);
}

#[test]
fn slugify_replaces_unsafe_chars() {
    assert_eq!(slugify("Q3/Q4: Plans?"), "Q3 Q4 Plans");
}

#[test]
fn slugify_empty_defaults_to_untitled() {
    assert_eq!(slugify("   "), "untitled");
}

#[test]
fn property_select() {
    let prop = json!({"type": "select", "select": {"name": "In progress"}});
    assert_eq!(property_to_text(&prop), "In progress");
}

#[test]
fn property_multi_select() {
    let prop = json!({"type": "multi_select", "multi_select": [{"name": "a"}, {"name": "b"}]});
    assert_eq!(property_to_text(&prop), "a, b");
}

#[test]
fn property_date_range() {
    let prop = json!({"type": "date", "date": {"start": "2026-01-01", "end": "2026-01-31"}});
    assert_eq!(property_to_text(&prop), "2026-01-01 \u{2192} 2026-01-31");
}

#[test]
fn property_checkbox() {
    assert_eq!(
        property_to_text(&json!({"type": "checkbox", "checkbox": true})),
        "true"
    );
}

#[test]
fn property_number_zero() {
    assert_eq!(
        property_to_text(&json!({"type": "number", "number": 0})),
        "0"
    );
}

#[test]
fn property_empty_select_is_blank() {
    assert_eq!(
        property_to_text(&json!({"type": "select", "select": null})),
        ""
    );
}

#[test]
fn relation_with_schema_shaped_value_does_not_crash() {
    let prop =
        json!({"type": "relation", "relation": {"single_property": {}, "type": "single_property"}});
    assert_eq!(property_to_text(&prop), "");
}

#[test]
fn verification_renders_state() {
    let prop =
        json!({"type": "verification", "verification": {"state": "verified", "verified_by": null}});
    assert_eq!(property_to_text(&prop), "verified");
}

#[test]
fn people_empty_dict_is_blank() {
    assert_eq!(
        property_to_text(&json!({"type": "people", "people": {}})),
        ""
    );
}

#[test]
fn relation_list_of_ids_still_works() {
    let prop = json!({"type": "relation", "relation": [{"id": "a"}, {"id": "b"}]});
    assert_eq!(property_to_text(&prop), "a, b");
}

#[test]
fn page_title_extracted_from_title_property() {
    let page = json!({
        "properties": {
            "Name": {"type": "title", "title": [{"plain_text": "My Page"}]},
            "Other": {"type": "rich_text", "rich_text": []}
        }
    });
    assert_eq!(page_title(&page), "My Page");
}
