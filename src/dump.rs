//! Dump an accessible Notion workspace to a raw JSON tree.
//!
//! This is the durable backup: every page and database is written verbatim as
//! the JSON returned by the API. The directory tree mirrors the Notion
//! hierarchy so the (optional) render stage can reconstruct it without any API
//! calls.
//!
//! Layout (under `json_dir`); every node directory is named `<id>-<slug>`:
//!
//! ```text
//! <id>-<Page Slug>/
//!     page.json               # {"object": "page", "data": <page>, "blocks": [...]}
//!     <id>-<Child Page>/page.json
//! <id>-<Database Slug>/
//!     database.json           # {"object": "database", "data": <db>, "row_order": [...]}
//!     <id>-<Row Slug>/page.json   # each row is a page node (properties + body)
//! ```
//!
//! ## Incremental dumps
//!
//! When a prior JSON tree exists, the dump reuses it: every page/database
//! carries a `last_edited_time` that bumps whenever its own content changes, so
//! unchanged objects keep their stored `blocks` instead of re-fetching them.
//! Discovery still enumerates the whole workspace, so deletions and moves are
//! reconciled. Pass `incremental = false` (`--full`) to force a clean re-fetch.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Result;
use log::{info, warn};
use serde_json::{json, Map, Value};

use crate::blocks::CHILD_KEY;
use crate::client::Api;
use crate::util::{database_title, page_title, slugify};

pub const PAGE_FILE: &str = "page.json";
pub const DATABASE_FILE: &str = "database.json";

// Block types that represent separately-exported objects: do not recurse into
// their children when fetching a page's block tree.
const SEPARATE_OBJECT_BLOCKS: [&str; 2] = ["child_page", "child_database"];

struct PrevEntry {
    path: PathBuf,
    last_edited_time: String,
}

/// Walks the accessible workspace and writes a raw JSON tree.
pub struct JsonDumper<'a, A: Api> {
    client: &'a A,
    root: PathBuf,
    incremental: bool,
    visited: HashSet<String>,
    search_index: HashMap<String, Value>,
    // database id -> list of its data_source search objects (2025-09-03 model).
    sources_by_db: HashMap<String, Vec<Value>>,
    db_order: Vec<String>,
    // database id -> retrieved database object (or None if inaccessible).
    db_cache: HashMap<String, Option<Value>>,
    // Previous dump state: object id -> {path, last_edited_time}.
    prev: HashMap<String, PrevEntry>,
    seen_dirs: HashSet<PathBuf>,
    reused: usize,
    fetched: usize,
    processed: usize,
    total: usize,
}

impl<'a, A: Api> JsonDumper<'a, A> {
    pub fn new(client: &'a A, json_dir: PathBuf, incremental: bool) -> Self {
        JsonDumper {
            client,
            root: json_dir,
            incremental,
            visited: HashSet::new(),
            search_index: HashMap::new(),
            sources_by_db: HashMap::new(),
            db_order: Vec::new(),
            db_cache: HashMap::new(),
            prev: HashMap::new(),
            seen_dirs: HashSet::new(),
            reused: 0,
            fetched: 0,
            processed: 0,
            total: 0,
        }
    }

    pub fn run(&mut self) -> Result<()> {
        fs::create_dir_all(&self.root)?;

        let incremental = self.incremental && self.has_prev_tree();
        if incremental {
            self.prev = self.scan_prev_tree();
            info!(
                "Incremental dump: {} previously-known objects",
                self.prev.len()
            );
        } else {
            if self.incremental {
                info!("No prior dump found; performing a full fetch");
            }
            clean_tree(&self.root)?;
        }

        let objects = self.client.search().map_err(anyhow::Error::new)?;
        self.search_index = objects
            .iter()
            .filter_map(|obj| {
                obj.get("id")
                    .and_then(Value::as_str)
                    .map(|id| (id.to_string(), obj.clone()))
            })
            .collect();

        let (root_pages, root_db_ids) = self.split_roots(&objects)?;

        // Progress denominator: every page node + every database node. Database
        // rows are added to this total as each database is queried.
        let page_count = objects
            .iter()
            .filter(|o| o.get("object").and_then(Value::as_str) == Some("page"))
            .count();
        self.total = page_count + self.sources_by_db.len();
        info!(
            "Enumerated {} pages and {} databases ({}/{} at the top level)",
            page_count,
            self.sources_by_db.len(),
            root_pages.len(),
            root_db_ids.len(),
        );

        let root = self.root.clone();
        for db_id in &root_db_ids {
            self.dump_database(db_id, &root)?;
        }
        for page in &root_pages {
            self.dump_page(page, &root)?;
        }

        // Completeness sweep: anything search knows about that recursion did not
        // reach is dumped at the root rather than silently dropped.
        self.sweep_unvisited()?;

        if incremental {
            self.prune()?;
        }
        info!(
            "Dumped {} objects ({} reused, {} fetched)",
            self.reused + self.fetched,
            self.reused,
            self.fetched,
        );
        Ok(())
    }

    // -- discovery ---------------------------------------------------------

    /// Split search results into top-level pages and database ids.
    ///
    /// Under API 2025-09-03 `/search` returns `page` and `data_source` objects
    /// (never `database`). Data sources are grouped by their parent database;
    /// the database is the node we dump. An object is a root when its parent is
    /// the workspace, or when its parent is not itself accessible.
    fn split_roots(&mut self, objects: &[Value]) -> Result<(Vec<Value>, Vec<String>)> {
        let accessible_ids: HashSet<String> = self.search_index.keys().cloned().collect();

        self.sources_by_db.clear();
        self.db_order.clear();
        for obj in objects {
            if obj.get("object").and_then(Value::as_str) != Some("data_source") {
                continue;
            }
            if let Some(db_id) = obj
                .get("parent")
                .and_then(|p| p.get("database_id"))
                .and_then(Value::as_str)
            {
                if !self.sources_by_db.contains_key(db_id) {
                    self.db_order.push(db_id.to_string());
                }
                self.sources_by_db
                    .entry(db_id.to_string())
                    .or_default()
                    .push(obj.clone());
            }
        }

        let pages: Vec<Value> = objects
            .iter()
            .filter(|o| o.get("object").and_then(Value::as_str) == Some("page"))
            .filter(|o| is_root(o, &accessible_ids))
            .cloned()
            .collect();

        let mut root_db_ids: Vec<String> = Vec::new();
        for db_id in self.db_order.clone() {
            if let Some(db) = self.get_database(&db_id)? {
                if is_root(&db, &accessible_ids) {
                    root_db_ids.push(db_id);
                }
            }
        }
        Ok((pages, root_db_ids))
    }

    fn sweep_unvisited(&mut self) -> Result<()> {
        let root = self.root.clone();
        for db_id in self.db_order.clone() {
            if !self.visited.contains(&db_id) {
                self.dump_database(&db_id, &root)?;
            }
        }
        let unvisited_pages: Vec<Value> = self
            .search_index
            .values()
            .filter(|obj| obj.get("object").and_then(Value::as_str) == Some("page"))
            .filter(|obj| {
                obj.get("id")
                    .and_then(Value::as_str)
                    .map(|id| !self.visited.contains(id))
                    .unwrap_or(false)
            })
            .cloned()
            .collect();
        for page in &unvisited_pages {
            self.dump_page(page, &root)?;
        }
        Ok(())
    }

    // -- pages -------------------------------------------------------------

    fn dump_page(&mut self, page: &Value, parent_dir: &Path) -> Result<Option<PathBuf>> {
        let page_id = match page.get("id").and_then(Value::as_str) {
            Some(id) => id.to_string(),
            None => return Ok(None),
        };
        if self.visited.contains(&page_id) {
            return Ok(None);
        }
        self.visited.insert(page_id.clone());

        let mut title = page_title(page);
        if title.is_empty() {
            title = "untitled".to_string();
        }
        let page_dir = node_dir(parent_dir, &title, &page_id);
        fs::create_dir_all(&page_dir)?;

        let last_edited = page
            .get("last_edited_time")
            .and_then(Value::as_str)
            .map(str::to_string);
        let (blocks, action) = self.blocks_for(&page_id, last_edited.as_deref())?;
        write_json(
            &page_dir.join(PAGE_FILE),
            json!({ "object": "page", "data": page, "blocks": blocks }),
        )?;
        self.seen_dirs.insert(page_dir.clone());
        self.log_progress(action, "page", &title);

        self.dump_child_objects(&blocks, &page_dir)?;
        Ok(Some(page_dir))
    }

    /// Recurse into `child_page` / `child_database` blocks of a page.
    fn dump_child_objects(&mut self, block_tree: &[Value], parent_dir: &Path) -> Result<()> {
        for block in block_tree {
            let btype = block.get("type").and_then(Value::as_str).unwrap_or("");
            let block_id = match block.get("id").and_then(Value::as_str) {
                Some(id) => id.to_string(),
                None => continue,
            };
            if btype == "child_page" {
                let child = match self.search_index.get(&block_id) {
                    Some(c) => Some(c.clone()),
                    None => self.get_page(&block_id)?,
                };
                if let Some(child) = child {
                    self.dump_page(&child, parent_dir)?;
                }
            } else if btype == "child_database" {
                self.dump_database(&block_id, parent_dir)?;
            }
        }
        Ok(())
    }

    // -- databases ---------------------------------------------------------

    fn dump_database(&mut self, db_id: &str, parent_dir: &Path) -> Result<()> {
        if self.visited.contains(db_id) {
            return Ok(());
        }
        self.visited.insert(db_id.to_string());

        let full_db = match self.get_database(db_id)? {
            Some(db) => db,
            None => {
                warn!("Skipping inaccessible database {db_id}");
                return Ok(());
            }
        };

        let mut title = database_title(&full_db);
        if title.is_empty() {
            title = "untitled-database".to_string();
        }
        let db_dir = node_dir(parent_dir, &title, db_id);
        fs::create_dir_all(&db_dir)?;

        // Prefer the database's authoritative data_sources; fall back to the
        // ones discovered via /search. Rows are always re-queried (cheap, and
        // needed for deletion detection); only each row's body is skipped when
        // unchanged.
        let sources: Vec<Value> = match full_db.get("data_sources").and_then(Value::as_array) {
            Some(arr) if !arr.is_empty() => arr.clone(),
            _ => self.sources_by_db.get(db_id).cloned().unwrap_or_default(),
        };
        if sources.is_empty() {
            warn!("Database {title:?} has no accessible data sources");
        }

        // Materialise rows up front so the progress total accounts for them
        // before they are dumped (rows are not returned by /search).
        let mut rows_by_source: Vec<Vec<Value>> = Vec::new();
        for src in &sources {
            let src_id = src.get("id").and_then(Value::as_str).unwrap_or("");
            rows_by_source.push(self.query_rows(src_id)?);
        }
        let row_count: usize = rows_by_source.iter().map(Vec::len).sum();
        self.total += row_count;
        self.log_progress("dumped", "database", &format!("{title} ({row_count} rows)"));

        let mut row_order: Vec<String> = Vec::new();
        for rows in &rows_by_source {
            for row in rows {
                match row.get("object").and_then(Value::as_str) {
                    Some("page") => {
                        if let Some(row_dir) = self.dump_page(row, &db_dir)? {
                            if let Some(name) = row_dir.file_name().and_then(|n| n.to_str()) {
                                row_order.push(name.to_string());
                            }
                        }
                    }
                    Some("data_source") => {
                        // A nested/linked sub-database can surface as a query
                        // result. Dump its owning database as a child node
                        // instead of writing the schema object into a page row.
                        if let Some(sub_db_id) = row
                            .get("parent")
                            .and_then(|p| p.get("database_id"))
                            .and_then(Value::as_str)
                        {
                            let sub_db_id = sub_db_id.to_string();
                            self.dump_database(&sub_db_id, &db_dir)?;
                        }
                    }
                    other => warn!("Skipping unexpected {other:?} object in {title:?} rows"),
                }
            }
        }

        write_json(
            &db_dir.join(DATABASE_FILE),
            json!({ "object": "database", "data": full_db, "row_order": row_order }),
        )?;
        self.seen_dirs.insert(db_dir);
        Ok(())
    }

    // -- block tree (with incremental reuse) -------------------------------

    /// Return a page's block tree and whether it was reused or fetched.
    fn blocks_for(
        &mut self,
        page_id: &str,
        last_edited_time: Option<&str>,
    ) -> Result<(Vec<Value>, &'static str)> {
        if let (Some(prev), Some(let_now)) = (self.prev.get(page_id), last_edited_time) {
            if prev.last_edited_time == let_now {
                if let Some(blocks) = load_prev_blocks(&prev.path) {
                    self.reused += 1;
                    return Ok((blocks, "reused"));
                }
            }
        }
        self.fetched += 1;
        Ok((self.fetch_block_tree(page_id)?, "fetched"))
    }

    /// Emit a per-object progress line as the dump proceeds.
    fn log_progress(&mut self, action: &str, kind: &str, title: &str) {
        self.processed += 1;
        info!(
            "[{}/{}] {action} {kind}: {}",
            self.processed,
            self.total,
            shorten(title, 60),
        );
    }

    /// Fetch direct children of a block, recursing into nested children.
    ///
    /// Child pages/databases are not recursed here (they become their own
    /// nodes), but the block stub is kept so links survive. Inaccessible blocks
    /// are skipped with a warning so a single bad block never aborts the backup.
    fn fetch_block_tree(&mut self, block_id: &str) -> Result<Vec<Value>> {
        let children = match self.client.get_block_children(block_id) {
            Ok(c) => c,
            Err(e) => match e.status() {
                Some(status) => {
                    warn!("Skipping blocks for {block_id}: HTTP {status}");
                    return Ok(Vec::new());
                }
                None => return Err(anyhow::Error::new(e)),
            },
        };
        let mut tree: Vec<Value> = Vec::new();
        for mut block in children {
            let btype = block.get("type").and_then(Value::as_str).unwrap_or("");
            let has_children = block
                .get("has_children")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            if has_children && !SEPARATE_OBJECT_BLOCKS.contains(&btype) {
                if let Some(child_id) = block.get("id").and_then(Value::as_str) {
                    let child_id = child_id.to_string();
                    let nested = self.fetch_block_tree(&child_id)?;
                    if let Some(obj) = block.as_object_mut() {
                        obj.insert(CHILD_KEY.to_string(), Value::Array(nested));
                    }
                }
            }
            tree.push(block);
        }
        Ok(tree)
    }

    // -- resilient client wrappers -----------------------------------------

    fn get_page(&self, page_id: &str) -> Result<Option<Value>> {
        match self.client.get_page(page_id) {
            Ok(page) => Ok(Some(page)),
            Err(e) => match e.status() {
                Some(status) => {
                    warn!("Skipping inaccessible page {page_id}: HTTP {status}");
                    Ok(None)
                }
                None => Err(anyhow::Error::new(e)),
            },
        }
    }

    fn get_database(&mut self, db_id: &str) -> Result<Option<Value>> {
        if let Some(cached) = self.db_cache.get(db_id) {
            return Ok(cached.clone());
        }
        let db = match self.client.get_database(db_id) {
            Ok(db) => Some(db),
            Err(e) => match e.status() {
                Some(status) => {
                    warn!("Cannot retrieve database {db_id}: HTTP {status}");
                    None
                }
                None => return Err(anyhow::Error::new(e)),
            },
        };
        self.db_cache.insert(db_id.to_string(), db.clone());
        Ok(db)
    }

    fn query_rows(&self, data_source_id: &str) -> Result<Vec<Value>> {
        match self.client.query_data_source(data_source_id) {
            Ok(rows) => Ok(rows),
            Err(e) => match e.status() {
                Some(status) => {
                    warn!("Skipping data source {data_source_id}: HTTP {status}");
                    Ok(Vec::new())
                }
                None => Err(anyhow::Error::new(e)),
            },
        }
    }

    // -- previous-tree bookkeeping -----------------------------------------

    fn has_prev_tree(&self) -> bool {
        if !self.root.exists() {
            return false;
        }
        let mut found = false;
        let _ = walk_node_files(&self.root, &mut |_, _| {
            found = true;
            false // stop early
        });
        found
    }

    /// Index the existing tree by object id -> {path, last_edited_time}.
    fn scan_prev_tree(&self) -> HashMap<String, PrevEntry> {
        let mut index: HashMap<String, PrevEntry> = HashMap::new();
        let _ = walk_node_files(&self.root, &mut |node_file, _| {
            if let Ok(payload) = load_json(node_file) {
                if let Some(data) = payload.get("data") {
                    if let Some(obj_id) = data.get("id").and_then(Value::as_str) {
                        index.insert(
                            obj_id.to_string(),
                            PrevEntry {
                                path: node_file.parent().unwrap_or(node_file).to_path_buf(),
                                last_edited_time: data
                                    .get("last_edited_time")
                                    .and_then(Value::as_str)
                                    .unwrap_or("")
                                    .to_string(),
                            },
                        );
                    }
                }
            }
            true
        });
        index
    }

    /// Remove previously-dumped nodes that were not written this run.
    ///
    /// Covers deletions (object gone) and moves/renames (object written at a new
    /// path). Removing a parent directory also removes its descendants, so the
    /// existence guard skips entries already gone.
    fn prune(&self) -> Result<()> {
        for entry in self.prev.values() {
            if !self.seen_dirs.contains(&entry.path) && entry.path.exists() {
                let rel = entry
                    .path
                    .strip_prefix(&self.root)
                    .unwrap_or(&entry.path)
                    .display();
                info!("Removing stale node {rel}");
                fs::remove_dir_all(&entry.path)?;
            }
        }
        Ok(())
    }
}

// -- module-level helpers ---------------------------------------------------

fn is_root(obj: &Value, accessible_ids: &HashSet<String>) -> bool {
    let parent = obj.get("parent");
    let ptype = parent.and_then(|p| p.get("type")).and_then(Value::as_str);
    if ptype == Some("workspace") {
        return true;
    }
    let parent_id = ptype.and_then(|t| parent.and_then(|p| p.get(t)).and_then(Value::as_str));
    match parent_id {
        Some(id) => !accessible_ids.contains(id),
        None => true,
    }
}

/// Return the directory for an object: `<id>-<slug>`.
///
/// Prefixing the id makes every node name unique by construction and keeps the
/// directory greppable by id.
fn node_dir(parent: &Path, title: &str, obj_id: &str) -> PathBuf {
    parent.join(format!("{}-{}", obj_id.replace('-', ""), slugify(title)))
}

fn shorten(text: &str, limit: usize) -> String {
    let text = text.replace('\n', " ");
    let text = text.trim();
    if text.chars().count() <= limit {
        text.to_string()
    } else {
        let head: String = text.chars().take(limit - 1).collect();
        format!("{head}\u{2026}")
    }
}

/// Strip fields that change on every fetch, so unchanged pages don't churn.
///
/// Removed: the API `request_id` envelope (present anywhere), and the signed
/// `url` + `expiry_time` of Notion-hosted file objects (which always carry
/// both). Stable `external` URLs and page permalinks have no `expiry_time`
/// sibling, so they are left untouched.
pub fn sanitize(value: Value) -> Value {
    match value {
        Value::Object(map) => {
            let is_signed_file = map.contains_key("url") && map.contains_key("expiry_time");
            let mut out = Map::new();
            for (key, item) in map {
                if key == "request_id" {
                    continue;
                }
                if is_signed_file && (key == "url" || key == "expiry_time") {
                    continue;
                }
                out.insert(key, sanitize(item));
            }
            Value::Object(out)
        }
        Value::Array(items) => Value::Array(items.into_iter().map(sanitize).collect()),
        other => other,
    }
}

fn write_json(path: &Path, payload: Value) -> Result<()> {
    let sanitized = sanitize(payload);
    let mut text = serde_json::to_string_pretty(&sanitized)?;
    text.push('\n');
    fs::write(path, text)?;
    Ok(())
}

fn load_json(path: &Path) -> Result<Value> {
    let text = fs::read_to_string(path)?;
    Ok(serde_json::from_str(&text)?)
}

fn load_prev_blocks(node_dir: &Path) -> Option<Vec<Value>> {
    let node_file = node_dir.join(PAGE_FILE);
    if !node_file.exists() {
        return None;
    }
    let payload = load_json(&node_file).ok()?;
    Some(
        payload
            .get("blocks")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default(),
    )
}

/// Walk the tree, invoking `visit(node_file, is_database)` for every `page.json`
/// and `database.json`. Returning `false` from `visit` stops the walk early.
fn walk_node_files(dir: &Path, visit: &mut dyn FnMut(&Path, bool) -> bool) -> bool {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return true,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if !walk_node_files(&path, visit) {
                return false;
            }
        } else if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
            if (name == PAGE_FILE || name == DATABASE_FILE) && !visit(&path, name == DATABASE_FILE)
            {
                return false;
            }
        }
    }
    true
}

/// Remove a previously-generated tree so deletions in Notion propagate.
/// Preserves dotfiles (e.g. `.git`) so the directory can double as a repo.
fn clean_tree(root: &Path) -> Result<()> {
    if !root.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(root)?.flatten() {
        let name = entry.file_name();
        if name.to_str().map(|n| n.starts_with('.')).unwrap_or(false) {
            continue;
        }
        let path = entry.path();
        if path.is_dir() {
            fs::remove_dir_all(&path)?;
        } else {
            fs::remove_file(&path)?;
        }
    }
    Ok(())
}
