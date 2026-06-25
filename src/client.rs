//! Thin Notion REST API client.
//!
//! Wraps `ureq` with the auth/version headers, transparent pagination, and
//! rate-limit handling (HTTP 429 with `Retry-After`). Uses the 2025-09-03 API
//! where a database contains one or more *data sources*.

use std::thread;
use std::time::Duration;

use log::warn;
use serde_json::{json, Value};

pub const API_BASE: &str = "https://api.notion.com/v1";

// Notion allows roughly 3 requests/second on average. A small fixed delay
// between calls keeps us well under the limit without needing a token bucket.
const REQUEST_INTERVAL: Duration = Duration::from_millis(340);
const MAX_RETRIES: u32 = 5;
const DEFAULT_RETRY_AFTER: f64 = 1.0;

/// Error surfaced by API calls. `Http` carries a status code the caller can
/// inspect to decide whether to skip the object; `Other` is fatal.
#[derive(Debug)]
pub enum ClientError {
    Http(u16),
    Other(String),
}

impl ClientError {
    /// HTTP status code, when this is an HTTP error.
    pub fn status(&self) -> Option<u16> {
        match self {
            ClientError::Http(code) => Some(*code),
            ClientError::Other(_) => None,
        }
    }
}

impl std::fmt::Display for ClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClientError::Http(code) => write!(f, "HTTP {code}"),
            ClientError::Other(msg) => f.write_str(msg),
        }
    }
}

impl std::error::Error for ClientError {}

/// The Notion endpoints the backup needs. Abstracted as a trait so the dumper
/// can be exercised against an in-memory fake.
pub trait Api {
    fn search(&self) -> Result<Vec<Value>, ClientError>;
    fn get_block_children(&self, block_id: &str) -> Result<Vec<Value>, ClientError>;
    fn get_database(&self, database_id: &str) -> Result<Value, ClientError>;
    fn query_data_source(&self, data_source_id: &str) -> Result<Vec<Value>, ClientError>;
    fn get_page(&self, page_id: &str) -> Result<Value, ClientError>;
}

/// Minimal client for the Notion API endpoints we need for backups.
pub struct NotionClient {
    agent: ureq::Agent,
    base_url: String,
    token: String,
    api_version: String,
}

impl NotionClient {
    pub fn new(token: impl Into<String>, api_version: impl Into<String>) -> Self {
        Self::with_base_url(token, api_version, API_BASE)
    }

    pub fn with_base_url(
        token: impl Into<String>,
        api_version: impl Into<String>,
        base_url: &str,
    ) -> Self {
        NotionClient {
            agent: ureq::AgentBuilder::new().build(),
            base_url: base_url.trim_end_matches('/').to_string(),
            token: token.into(),
            api_version: api_version.into(),
        }
    }

    // -- low-level request with retry/backoff ------------------------------

    fn request(
        &self,
        method: &str,
        path: &str,
        body: Option<&Value>,
    ) -> Result<Value, ClientError> {
        let url = format!("{}{}", self.base_url, path);
        for attempt in 1..=MAX_RETRIES {
            thread::sleep(REQUEST_INTERVAL);

            let req = self
                .agent
                .request(method, &url)
                .set("Authorization", &format!("Bearer {}", self.token))
                .set("Notion-Version", &self.api_version)
                .set("Content-Type", "application/json");

            let result = match body {
                Some(payload) => req.send_json(payload),
                None => req.call(),
            };

            match result {
                Ok(response) => {
                    return response
                        .into_json::<Value>()
                        .map_err(|e| ClientError::Other(format!("invalid JSON body: {e}")));
                }
                Err(ureq::Error::Status(429, response)) => {
                    let retry_after = response
                        .header("Retry-After")
                        .and_then(|v| v.parse::<f64>().ok())
                        .unwrap_or(DEFAULT_RETRY_AFTER);
                    warn!(
                        "Rate limited on {method} {path} (attempt {attempt}/{MAX_RETRIES}); \
                         sleeping {retry_after:.1}s"
                    );
                    thread::sleep(Duration::from_secs_f64(retry_after));
                }
                Err(ureq::Error::Status(code, _)) if code >= 500 => {
                    let backoff = (1u64 << attempt).min(30);
                    warn!(
                        "Server error {code} on {method} {path} (attempt {attempt}/{MAX_RETRIES}); \
                         retrying in {backoff}s"
                    );
                    thread::sleep(Duration::from_secs(backoff));
                }
                Err(ureq::Error::Status(code, _)) => return Err(ClientError::Http(code)),
                Err(ureq::Error::Transport(t)) => {
                    return Err(ClientError::Other(format!("transport error: {t}")))
                }
            }
        }
        Err(ClientError::Other(format!(
            "Exhausted retries for {method} {path}"
        )))
    }

    /// Yield every result across all pages of a paginated endpoint.
    fn paginate(
        &self,
        method: &str,
        path: &str,
        body: Option<Value>,
    ) -> Result<Vec<Value>, ClientError> {
        let mut body = body.unwrap_or_else(|| json!({}));
        let mut path = path.to_string();
        let mut out: Vec<Value> = Vec::new();
        loop {
            let payload = if method == "GET" {
                self.request(method, &path, None)?
            } else {
                self.request(method, &path, Some(&body))?
            };
            if let Some(results) = payload.get("results").and_then(Value::as_array) {
                out.extend(results.iter().cloned());
            }
            if !payload
                .get("has_more")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                return Ok(out);
            }
            let cursor = payload
                .get("next_cursor")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            if method == "GET" {
                // GET endpoints take the cursor as a query parameter.
                let base = path.split('?').next().unwrap_or("").to_string();
                let sep = if path.contains('?') { '&' } else { '?' };
                path = format!("{base}{sep}start_cursor={cursor}");
            } else {
                body["start_cursor"] = Value::String(cursor);
            }
        }
    }
}

impl Api for NotionClient {
    /// Yield every page and data source the integration can access.
    fn search(&self) -> Result<Vec<Value>, ClientError> {
        self.paginate("POST", "/search", Some(json!({ "page_size": 100 })))
    }

    /// Yield the direct child blocks of a block or page.
    fn get_block_children(&self, block_id: &str) -> Result<Vec<Value>, ClientError> {
        self.paginate(
            "GET",
            &format!("/blocks/{block_id}/children?page_size=100"),
            None,
        )
    }

    /// Retrieve a database object, including its `data_sources` array.
    fn get_database(&self, database_id: &str) -> Result<Value, ClientError> {
        self.request("GET", &format!("/databases/{database_id}"), None)
    }

    /// Yield every page (row) in a data source.
    fn query_data_source(&self, data_source_id: &str) -> Result<Vec<Value>, ClientError> {
        self.paginate(
            "POST",
            &format!("/data_sources/{data_source_id}/query"),
            Some(json!({ "page_size": 100 })),
        )
    }

    /// Retrieve a page object (properties, title, metadata).
    fn get_page(&self, page_id: &str) -> Result<Value, ClientError> {
        self.request("GET", &format!("/pages/{page_id}"), None)
    }
}
