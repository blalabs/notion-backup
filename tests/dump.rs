//! Tests for the JSON dumper: the 2025-09-03 data-source model, error
//! resilience, and incremental reuse/change-detection/deletion.
#![allow(clippy::field_reassign_with_default)]

use std::cell::RefCell;
use std::collections::HashMap;
use std::fs;
use std::path::Path;

use notion_backup::client::{Api, ClientError};
use notion_backup::dump::JsonDumper;
use serde_json::{json, Value};
use tempfile::tempdir;

/// Configurable in-memory stand-in covering pages, databases, data sources.
#[derive(Default)]
struct FakeApi {
    search_results: Vec<Value>,
    pages: HashMap<String, Value>,
    databases: HashMap<String, Value>,
    data_sources: HashMap<String, Vec<Value>>,
    blocks: HashMap<String, Vec<Value>>,
    fail_blocks: Vec<String>,
    block_calls: RefCell<Vec<String>>,
}

impl Api for FakeApi {
    fn search(&self) -> Result<Vec<Value>, ClientError> {
        Ok(self.search_results.clone())
    }

    fn get_block_children(&self, block_id: &str) -> Result<Vec<Value>, ClientError> {
        self.block_calls.borrow_mut().push(block_id.to_string());
        if self.fail_blocks.iter().any(|b| b == block_id) {
            return Err(ClientError::Http(404));
        }
        Ok(self.blocks.get(block_id).cloned().unwrap_or_default())
    }

    fn get_page(&self, page_id: &str) -> Result<Value, ClientError> {
        Ok(self.pages.get(page_id).cloned().unwrap())
    }

    fn get_database(&self, database_id: &str) -> Result<Value, ClientError> {
        Ok(self.databases.get(database_id).cloned().unwrap())
    }

    fn query_data_source(&self, data_source_id: &str) -> Result<Vec<Value>, ClientError> {
        Ok(self
            .data_sources
            .get(data_source_id)
            .cloned()
            .unwrap_or_default())
    }
}

fn row(page_id: &str, title: &str, ds_id: &str) -> Value {
    json!({
        "object": "page", "id": page_id, "last_edited_time": "2026-01-01T00:00:00.000Z",
        "url": format!("https://notion.so/{page_id}"),
        "parent": {"type": "data_source_id", "data_source_id": ds_id},
        "properties": {"Name": {"type": "title", "title": [{"plain_text": title}]}}
    })
}

fn read_json(path: &Path) -> Value {
    serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap()
}

#[test]
fn data_source_is_dumped_as_database_not_page() {
    let tmp = tempdir().unwrap();
    let json_dir = tmp.path().join("json");
    let (ds_id, db_id) = ("ds1", "db1");
    let mut api = FakeApi::default();
    // /search returns a data_source object (never a database) under 2025-09-03.
    api.search_results = vec![
        json!({"object": "data_source", "id": ds_id, "parent": {"type": "database_id", "database_id": db_id}}),
    ];
    api.databases.insert(
        db_id.to_string(),
        json!({
            "object": "database", "id": db_id, "title": [{"plain_text": "Tasks"}],
            "parent": {"type": "workspace", "workspace": true},
            "data_sources": [{"id": ds_id, "name": "Tasks"}]
        }),
    );
    api.data_sources
        .insert(ds_id.to_string(), vec![row("r1", "First", ds_id)]);
    api.blocks.insert("r1".to_string(), vec![]);

    JsonDumper::new(&api, json_dir.clone(), false)
        .run()
        .unwrap();

    assert!(json_dir.join("db1-Tasks").join("database.json").exists());
    assert!(json_dir
        .join("db1-Tasks")
        .join("r1-First")
        .join("page.json")
        .exists());
    // The data source id must never be treated as a block (that was the 404 bug).
    assert!(!api.block_calls.borrow().contains(&ds_id.to_string()));
}

#[test]
fn data_source_in_query_results_is_not_dumped_as_a_page() {
    let tmp = tempdir().unwrap();
    let json_dir = tmp.path().join("json");
    let (ds, db) = ("ds1", "db1");
    let (sub_ds, sub_db) = ("subds", "subdb");
    let mut api = FakeApi::default();
    api.search_results = vec![
        json!({"object": "data_source", "id": ds, "parent": {"type": "database_id", "database_id": db}}),
    ];
    api.databases.insert(
        db.to_string(),
        json!({
            "object": "database", "id": db, "title": [{"plain_text": "Operations"}],
            "parent": {"type": "workspace", "workspace": true},
            "data_sources": [{"id": ds, "name": "Operations"}]
        }),
    );
    api.databases.insert(
        sub_db.to_string(),
        json!({
            "object": "database", "id": sub_db, "title": [{"plain_text": "Sub"}],
            "parent": {"type": "page_id", "page_id": "x"},
            "data_sources": [{"id": sub_ds, "name": "Sub"}]
        }),
    );
    // The query returns a normal page row AND a nested data_source object.
    api.data_sources.insert(ds.to_string(), vec![
        row("r1", "Real Row", ds),
        json!({"object": "data_source", "id": sub_ds, "parent": {"type": "database_id", "database_id": sub_db}}),
    ]);
    api.data_sources
        .insert(sub_ds.to_string(), vec![row("r2", "Sub Row", sub_ds)]);
    api.blocks.insert("r1".to_string(), vec![]);
    api.blocks.insert("r2".to_string(), vec![]);

    JsonDumper::new(&api, json_dir.clone(), false)
        .run()
        .unwrap();

    // The page row is dumped; the nested data source becomes a database node.
    assert!(json_dir
        .join("db1-Operations")
        .join("r1-Real Row")
        .join("page.json")
        .exists());
    assert!(json_dir
        .join("db1-Operations")
        .join("subdb-Sub")
        .join("database.json")
        .exists());
    // No page.json anywhere holds a data_source object.
    let mut stack = vec![json_dir.clone()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.file_name().and_then(|n| n.to_str()) == Some("page.json") {
                assert_eq!(read_json(&path)["data"]["object"], json!("page"));
            }
        }
    }
}

#[test]
fn inaccessible_blocks_are_skipped_not_fatal() {
    let tmp = tempdir().unwrap();
    let json_dir = tmp.path().join("json");
    let mut api = FakeApi::default();
    let page = json!({
        "object": "page", "id": "p1", "last_edited_time": "2026-01-01T00:00:00.000Z",
        "url": "https://notion.so/p1", "parent": {"type": "workspace", "workspace": true},
        "properties": {"Name": {"type": "title", "title": [{"plain_text": "Broken"}]}}
    });
    api.search_results = vec![page.clone()];
    api.pages.insert("p1".to_string(), page);
    api.fail_blocks.push("p1".to_string()); // get_block_children raises 404

    // Must not error; the node is still written with an empty block list.
    JsonDumper::new(&api, json_dir.clone(), false)
        .run()
        .unwrap();

    let node = read_json(&json_dir.join("p1-Broken").join("page.json"));
    assert_eq!(node["blocks"], json!([]));
}

// -- incremental dumping -----------------------------------------------------

fn page(page_id: &str, title: &str, let_: &str) -> Value {
    json!({
        "object": "page", "id": page_id, "last_edited_time": let_,
        "url": format!("https://notion.so/{page_id}"),
        "parent": {"type": "workspace", "workspace": true},
        "properties": {"Name": {"type": "title", "title": [{"plain_text": title}]}}
    })
}

fn para(text: &str) -> Value {
    json!({"type": "paragraph", "has_children": false,
           "paragraph": {"rich_text": [{"plain_text": text, "annotations": {}}]}})
}

fn let_of(node_file: &Path) -> String {
    read_json(node_file)["data"]["last_edited_time"]
        .as_str()
        .unwrap()
        .to_string()
}

/// FakeApi where `search` returns the page map's values (mirrors the workspace).
fn page_api(pages: &HashMap<String, Value>, blocks: &HashMap<String, Vec<Value>>) -> FakeApi {
    let mut api = FakeApi::default();
    api.search_results = pages.values().cloned().collect();
    api.pages = pages.clone();
    api.blocks = blocks.clone();
    api
}

#[test]
fn incremental_reuses_unchanged_and_refetches_changed() {
    let tmp = tempdir().unwrap();
    let json_dir = tmp.path().join("json");
    let mut pages = HashMap::new();
    pages.insert(
        "a".to_string(),
        page("a", "Alpha", "2026-01-01T00:00:00.000Z"),
    );
    pages.insert(
        "b".to_string(),
        page("b", "Beta", "2026-01-01T00:00:00.000Z"),
    );
    let mut blocks = HashMap::new();
    blocks.insert("a".to_string(), vec![para("alpha v1")]);
    blocks.insert("b".to_string(), vec![para("beta v1")]);

    // First run: full dump, both bodies fetched.
    let api = page_api(&pages, &blocks);
    JsonDumper::new(&api, json_dir.clone(), true).run().unwrap();
    let mut fetched = api.block_calls.borrow().clone();
    fetched.sort();
    assert_eq!(fetched, vec!["a", "b"]);

    // Page B is edited (new last_edited_time + new content); A is untouched.
    pages.get_mut("b").unwrap()["last_edited_time"] = json!("2026-02-01T00:00:00.000Z");
    blocks.insert("b".to_string(), vec![para("beta v2")]);

    let api = page_api(&pages, &blocks);
    JsonDumper::new(&api, json_dir.clone(), true).run().unwrap();

    // Only B's body is re-fetched; A is reused from disk.
    assert_eq!(*api.block_calls.borrow(), vec!["b".to_string()]);
    assert_eq!(
        let_of(&json_dir.join("b-Beta").join("page.json")),
        "2026-02-01T00:00:00.000Z"
    );
    assert!(
        fs::read_to_string(json_dir.join("b-Beta").join("page.json"))
            .unwrap()
            .contains("beta v2")
    );
    assert!(
        fs::read_to_string(json_dir.join("a-Alpha").join("page.json"))
            .unwrap()
            .contains("alpha v1")
    );
}

#[test]
fn incremental_removes_deleted_pages() {
    let tmp = tempdir().unwrap();
    let json_dir = tmp.path().join("json");
    let mut pages = HashMap::new();
    pages.insert(
        "a".to_string(),
        page("a", "Alpha", "2026-01-01T00:00:00.000Z"),
    );
    pages.insert(
        "b".to_string(),
        page("b", "Beta", "2026-01-01T00:00:00.000Z"),
    );
    let mut blocks = HashMap::new();
    blocks.insert("a".to_string(), vec![]);
    blocks.insert("b".to_string(), vec![]);

    let api = page_api(&pages, &blocks);
    JsonDumper::new(&api, json_dir.clone(), true).run().unwrap();
    assert!(json_dir.join("b-Beta").exists());

    // Beta is deleted from Notion (no longer returned by search).
    pages.remove("b");
    let api = page_api(&pages, &blocks);
    JsonDumper::new(&api, json_dir.clone(), true).run().unwrap();

    assert!(json_dir.join("a-Alpha").exists());
    assert!(!json_dir.join("b-Beta").exists());
}

#[test]
fn node_dirs_are_prefixed_with_id() {
    let tmp = tempdir().unwrap();
    let json_dir = tmp.path().join("json");
    let mut pages = HashMap::new();
    pages.insert(
        "abc-123".to_string(),
        page("abc-123", "My Page", "2026-01-01T00:00:00.000Z"),
    );
    let mut blocks = HashMap::new();
    blocks.insert("abc-123".to_string(), vec![]);

    let api = page_api(&pages, &blocks);
    JsonDumper::new(&api, json_dir.clone(), true).run().unwrap();

    // Dashes are stripped from the id and it always prefixes the slug.
    assert!(json_dir.join("abc123-My Page").join("page.json").exists());
}

#[test]
fn full_mode_refetches_everything() {
    let tmp = tempdir().unwrap();
    let json_dir = tmp.path().join("json");
    let mut pages = HashMap::new();
    pages.insert(
        "a".to_string(),
        page("a", "Alpha", "2026-01-01T00:00:00.000Z"),
    );
    let mut blocks = HashMap::new();
    blocks.insert("a".to_string(), vec![]);

    let api = page_api(&pages, &blocks);
    JsonDumper::new(&api, json_dir.clone(), true).run().unwrap();

    // incremental=false forces a full re-fetch even though nothing changed.
    let api = page_api(&pages, &blocks);
    JsonDumper::new(&api, json_dir.clone(), false)
        .run()
        .unwrap();
    assert_eq!(*api.block_calls.borrow(), vec!["a".to_string()]);
}
