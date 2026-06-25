//! Tests for stripping volatile fields (request_id, signed file URLs/expiry).

use notion_backup::dump::sanitize;
use serde_json::json;

#[test]
fn removes_request_id_anywhere() {
    let payload =
        json!({"object": "page", "request_id": "abc", "data": {"id": "1", "request_id": "x"}});
    assert_eq!(
        sanitize(payload),
        json!({"object": "page", "data": {"id": "1"}})
    );
}

#[test]
fn strips_signed_file_url_and_expiry() {
    let block = json!({"type": "image", "image": {"type": "file", "file": {
        "url": "https://prod-files.s3/...?X-Amz-Signature=deadbeef",
        "expiry_time": "2026-06-25T12:00:00.000Z"
    }}});
    assert_eq!(
        sanitize(block),
        json!({"type": "image", "image": {"type": "file", "file": {}}})
    );
}

#[test]
fn strips_signed_url_in_files_property() {
    let prop = json!({"type": "files", "files": [
        {"name": "a.pdf", "type": "file", "file": {"url": "https://s3/x?sig", "expiry_time": "t"}}
    ]});
    assert_eq!(
        sanitize(prop)["files"][0],
        json!({"name": "a.pdf", "type": "file", "file": {}})
    );
}

#[test]
fn keeps_external_url_without_expiry() {
    let cover = json!({"type": "external", "external": {"url": "https://example.com/banner.png"}});
    assert_eq!(sanitize(cover.clone()), cover);
}

#[test]
fn keeps_page_permalink_url() {
    // The page-level url has no expiry_time sibling, so it must survive.
    let data = json!({"id": "1", "url": "https://notion.so/My-Page-1"});
    assert_eq!(sanitize(data.clone()), data);
}
