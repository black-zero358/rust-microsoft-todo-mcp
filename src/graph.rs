//! Bounded, credential-safe Microsoft Graph transport.
//! Mutation retries are deliberately owned by the durable operation ledger.
use crate::{Error, Result, auth::Auth};
use reqwest::{Client, Method, StatusCode, Url, header::RETRY_AFTER};
use serde_json::Value;
use std::{collections::HashSet, sync::Arc, time::Duration};

const GRAPH_ROOT: &str = "https://graph.microsoft.com/v1.0/";
const MAX_RESPONSE: usize = 4 * 1024 * 1024;
const MAX_PAGES: usize = 1000;
const MAX_ITEMS: usize = 100_000;
const MAX_COLLECTION_BYTES: usize = 32 * 1024 * 1024;
const READ_ATTEMPTS: usize = 4;
const MAX_RETRY_WAIT: u64 = 30;

pub struct GraphClient {
    client: Client,
    auth: Option<Arc<Auth>>,
    root: Url,
    #[cfg(test)]
    test_token: Option<String>,
    #[cfg(test)]
    retry_delay: Option<Duration>,
}

impl GraphClient {
    pub fn new(auth: Arc<Auth>) -> Result<Self> {
        Ok(Self {
            client: Client::builder()
                .connect_timeout(Duration::from_secs(10))
                .timeout(Duration::from_secs(45))
                .redirect(reqwest::redirect::Policy::none())
                .user_agent(concat!(
                    "rust-microsoft-todo-mcp/",
                    env!("CARGO_PKG_VERSION")
                ))
                .build()
                .map_err(|_| {
                    Error::new("configuration", "Failed to initialize Graph HTTP client")
                })?,
            auth: Some(auth),
            root: Url::parse(GRAPH_ROOT).expect("constant URL"),
            #[cfg(test)]
            test_token: None,
            #[cfg(test)]
            retry_delay: None,
        })
    }

    #[cfg(test)]
    pub(crate) fn for_test(root: &str) -> Self {
        let root = Url::parse(root).expect("test Graph root");
        assert_eq!(root.host_str(), Some("127.0.0.1"));
        assert_eq!(root.scheme(), "http");
        assert_eq!(root.path(), "/v1.0/");
        Self {
            client: Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(3))
                .build()
                .unwrap(),
            auth: None,
            root,
            test_token: Some("synthetic-token".into()),
            retry_delay: Some(Duration::ZERO),
        }
    }

    #[cfg(test)]
    pub(crate) fn for_test_with_auth(root: &str, auth: Arc<Auth>) -> Self {
        let mut client = Self::for_test(root);
        client.auth = Some(auth);
        client.test_token = None;
        client
    }

    /// Validate before obtaining credentials. Absolute cursors must retain the
    /// configured Graph origin and version, including after URL normalization.
    fn endpoint(&self, path: &str) -> Result<Url> {
        if path.len() > 32_768 || path.chars().any(char::is_control) || path.contains('\\') {
            return Err(invalid_url());
        }
        // URL parsers normalize literal and encoded dot segments. Reject them
        // before parsing so an entity ID can never change the addressed resource.
        let raw_path = path.split(['?', '#']).next().unwrap_or(path);
        if raw_path.split('/').any(|part| {
            let normalized = part.to_ascii_lowercase().replace("%2e", ".");
            normalized == "." || normalized == ".."
        }) {
            return Err(invalid_url());
        }
        let url = if path.starts_with("https://") || path.starts_with("http://") {
            Url::parse(path).map_err(|_| invalid_url())?
        } else {
            if path.starts_with("//") || path.contains("://") {
                return Err(invalid_url());
            }
            let path = path.strip_prefix('/').unwrap_or(path);
            let path = path.strip_prefix("v1.0/").unwrap_or(path);
            self.root.join(path).map_err(|_| invalid_url())?
        };
        if url.scheme() != self.root.scheme()
            || url.host_str() != self.root.host_str()
            || url.port_or_known_default() != self.root.port_or_known_default()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
            || !url.path().starts_with("/v1.0/")
        {
            return Err(invalid_url());
        }
        Ok(url)
    }

    /// Public cursor check lets synchronization reject a bad persisted cursor
    /// before starting a new round. The returned string is normalized.
    pub fn validate_url(&self, path: &str) -> Result<String> {
        Ok(self.endpoint(path)?.to_string())
    }

    async fn token(&self, account: &str, generation: Option<i64>) -> Result<String> {
        #[cfg(test)]
        if let Some(token) = &self.test_token {
            return Ok(token.clone());
        }
        let auth = self
            .auth
            .as_ref()
            .ok_or_else(|| Error::new("configuration", "Graph authentication unavailable"))?;
        match generation {
            Some(g) => auth.access_token_for_generation(account, g).await,
            None => auth.access_token(account).await,
        }
    }

    pub async fn get(&self, account_id: &str, path_or_url: &str) -> Result<Value> {
        self.request(account_id, Method::GET, path_or_url, None, None)
            .await
    }

    pub async fn write(
        &self,
        account_id: &str,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<Value> {
        if !matches!(method, Method::POST | Method::PATCH | Method::DELETE) {
            return Err(Error::new(
                "invalid_argument",
                "Graph write requires POST, PATCH or DELETE",
            ));
        }
        self.request(account_id, method, path, body, None).await
    }

    pub async fn write_at_generation(
        &self,
        account_id: &str,
        generation: i64,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<Value> {
        if !matches!(method, Method::POST | Method::PATCH | Method::DELETE) {
            return Err(Error::new(
                "invalid_argument",
                "Graph write requires POST, PATCH or DELETE",
            ));
        }
        self.request(account_id, method, path, body, Some(generation))
            .await
    }

    async fn request(
        &self,
        account_id: &str,
        method: Method,
        path: &str,
        body: Option<Value>,
        generation: Option<i64>,
    ) -> Result<Value> {
        let url = self.endpoint(path)?;
        let read = method == Method::GET;
        // Serialization is bounded before transmitting a mutation.
        if let Some(ref value) = body
            && serde_json::to_vec(value)
                .map_err(|_| Error::new("invalid_argument", "Invalid JSON body"))?
                .len()
                > MAX_RESPONSE
        {
            return Err(Error::new(
                "invalid_argument",
                "Graph request exceeds 4 MiB",
            ));
        }
        let attempts = if read { READ_ATTEMPTS } else { 1 };
        let mut refreshed_after_401 = false;
        'attempts: for attempt in 0..attempts {
            // Refresh is checked on every safe read attempt.
            let token = self.token(account_id, generation).await?;
            let mut request = self
                .client
                .request(method.clone(), url.clone())
                .bearer_auth(&token)
                .header("Accept", "application/json");
            if let Some(ref value) = body {
                request = request.json(value);
            }
            let response = request.send().await;
            let mut response = match response {
                Ok(response) => response,
                Err(_) if read && attempt + 1 < attempts => {
                    self.pause(Duration::from_millis(250 * (1 << attempt)))
                        .await;
                    continue;
                }
                Err(_) => {
                    return Err(if read {
                        Error::new("graph_unavailable", "Graph request failed or timed out")
                    } else {
                        unknown()
                    });
                }
            };
            let status = response.status();
            if status == StatusCode::UNAUTHORIZED
                && let Some(auth) = &self.auth
            {
                auth.invalidate_access_token(account_id, &token).await;
                if read && !refreshed_after_401 && attempt + 1 < attempts {
                    refreshed_after_401 = true;
                    continue;
                }
            }

            if read && retryable(status) && attempt + 1 < attempts {
                let delay = retry_after(
                    response
                        .headers()
                        .get(RETRY_AFTER)
                        .and_then(|v| v.to_str().ok()),
                    attempt,
                );
                // Never retry earlier than a server's long Retry-After.
                if let Some(delay) = delay {
                    drop(response);
                    self.pause(delay).await;
                    continue;
                }
            }
            if !status.is_success() {
                return Err(status_error(status, read));
            }
            if status == StatusCode::NO_CONTENT {
                return Ok(Value::Null);
            }
            if response
                .content_length()
                .is_some_and(|n| n > MAX_RESPONSE as u64)
            {
                return Err(response_error(read, "Graph response exceeds 4 MiB"));
            }
            let mut bytes = Vec::new();
            loop {
                match response.chunk().await {
                    Ok(Some(chunk)) => {
                        if chunk.len() > MAX_RESPONSE - bytes.len() {
                            return Err(response_error(read, "Graph response exceeds 4 MiB"));
                        }
                        bytes.extend_from_slice(&chunk);
                    }
                    Ok(None) => break,
                    Err(_) if read && attempt + 1 < attempts => {
                        self.pause(Duration::from_millis(250 * (1 << attempt)))
                            .await;
                        continue 'attempts;
                    }
                    Err(_) => return Err(response_error(read, "Graph response interrupted")),
                }
            }
            if bytes.is_empty() {
                return Ok(Value::Null);
            }
            return serde_json::from_slice(&bytes)
                .map_err(|_| response_error(read, "Graph returned invalid JSON"));
        }
        Err(Error::new(
            "graph_unavailable",
            "Graph retry budget exhausted",
        ))
    }

    async fn pause(&self, duration: Duration) {
        #[cfg(test)]
        if let Some(delay) = self.retry_delay {
            tokio::time::sleep(delay).await;
            return;
        }
        tokio::time::sleep(duration).await;
    }

    /// Small collection helper. Delta synchronization uses `get` and persists
    /// each page rather than buffering an entire account in memory.
    pub async fn collection(&self, account_id: &str, path: &str) -> Result<Vec<Value>> {
        let mut next = Some(self.validate_url(path)?);
        let mut seen = HashSet::new();
        let mut values = Vec::new();
        let mut bytes = 0usize;
        while let Some(url) = next.take() {
            if !seen.insert(url.clone()) || seen.len() > MAX_PAGES {
                return Err(Error::new(
                    "pagination_limit",
                    "Graph pagination cycle or page limit reached",
                ));
            }
            let page = self.get(account_id, &url).await?;
            let items = page.get("value").and_then(Value::as_array).ok_or_else(|| {
                Error::new(
                    "graph_response",
                    "Graph collection is missing its value array",
                )
            })?;
            bytes += serde_json::to_vec(items)
                .map_err(|_| Error::new("graph_response", "Invalid Graph collection"))?
                .len();
            if values.len() + items.len() > MAX_ITEMS || bytes > MAX_COLLECTION_BYTES {
                return Err(Error::new(
                    "pagination_limit",
                    "Graph collection exceeds bounded collection budget",
                ));
            }
            values.extend(items.iter().cloned());
            if let Some(link) = page.get("@odata.nextLink") {
                let link = link.as_str().ok_or_else(|| {
                    Error::new("graph_response", "Graph pagination link must be a string")
                })?;
                next = Some(self.validate_url(link)?);
            }
        }
        Ok(values)
    }
}

/// Percent-encode a Graph identifier as exactly one URL path component.
pub fn segment(value: &str) -> String {
    let mut result = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'~') {
            result.push(*byte as char);
        } else {
            use std::fmt::Write;
            let _ = write!(result, "%{byte:02X}");
        }
    }
    result
}

fn invalid_url() -> Error {
    Error::new(
        "invalid_graph_url",
        "Graph URL must stay on the configured Graph v1.0 origin",
    )
}
fn unknown() -> Error {
    Error::new(
        "write_unknown",
        "Graph mutation outcome is unknown; reconcile the original operation key before retrying",
    )
}
fn response_error(read: bool, message: &str) -> Error {
    if read {
        Error::new("graph_response", message)
    } else {
        unknown()
    }
}
fn retryable(status: StatusCode) -> bool {
    status == StatusCode::TOO_MANY_REQUESTS || matches!(status.as_u16(), 500 | 502 | 503 | 504)
}
fn status_error(status: StatusCode, read: bool) -> Error {
    if !read && (status.is_server_error() || status == StatusCode::REQUEST_TIMEOUT) {
        return unknown();
    }
    match status.as_u16() {
        401 => Error::new(
            "account_auth_required",
            "Microsoft authorization expired or was revoked; reconnect this account",
        ),
        403 => Error::new(
            "graph_forbidden",
            "Microsoft Graph denied the operation; check account permission and list restrictions",
        ),
        404 => Error::new(
            "not_found",
            "The requested Microsoft To Do item was not found",
        ),
        409 | 412 => Error::new(
            "graph_conflict",
            "Microsoft Graph reported a concurrent change or conflict",
        ),
        410 => Error::new(
            "sync_cursor_expired",
            "Graph delta cursor expired; a full synchronization is required",
        ),
        429 => Error::new(
            "graph_throttled",
            "Microsoft Graph rate limit reached; retry this operation later",
        ),
        400 | 422 => Error::new(
            "invalid_argument",
            "Microsoft Graph rejected the request fields or values",
        ),
        300..=399 => Error::new("graph_redirect", "Microsoft Graph redirect refused"),
        _ => Error::new(
            "graph_unavailable",
            format!("Microsoft Graph returned HTTP {}", status.as_u16()),
        ),
    }
}
fn retry_after(value: Option<&str>, attempt: usize) -> Option<Duration> {
    if let Some(value) = value {
        let seconds = value.parse::<u64>().ok().or_else(|| {
            chrono::DateTime::parse_from_rfc2822(value)
                .ok()
                .map(|when| (when.timestamp() - chrono::Utc::now().timestamp()).max(0) as u64)
        });
        if let Some(seconds) = seconds {
            return (seconds <= MAX_RETRY_WAIT).then(|| Duration::from_secs(seconds));
        }
    }
    Some(Duration::from_millis(250 * (1 << attempt.min(5))))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Router, extract::State, http::HeaderMap, response::IntoResponse, routing::any};
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn client(root: &str) -> GraphClient {
        let mut graph = GraphClient::for_test("http://127.0.0.1:1/v1.0/");
        graph.root = Url::parse(root).unwrap();
        graph
    }
    async fn serve(router: Router) -> (GraphClient, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let root = format!("http://{}/v1.0/", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        (client(&root), task)
    }
    #[test]
    fn rejects_dot_segments_before_url_normalization() {
        let client = GraphClient::for_test("http://127.0.0.1:1/v1.0/");
        for path in [
            "/me/todo/lists/a/tasks/..",
            "/me/todo/lists/a/tasks/%2E%2E",
            "/me/todo/lists/%2e./tasks",
            "/me/./todo/lists",
        ] {
            assert!(client.validate_url(path).is_err());
        }
    }
    #[test]
    fn validates_urls_before_credentials() {
        let graph = client(GRAPH_ROOT);
        assert_eq!(
            graph.validate_url("/me/todo/lists").unwrap(),
            "https://graph.microsoft.com/v1.0/me/todo/lists"
        );
        for bad in [
            "https://evil.example/v1.0/me",
            "http://graph.microsoft.com/v1.0/me",
            "https://graph.microsoft.com.evil/v1.0/",
            "https://u:p@graph.microsoft.com/v1.0/me",
            "https://graph.microsoft.com/beta/me",
            "https://graph.microsoft.com/v1.0/../beta/me",
            "//evil.example/v1.0/",
            "https://graph.microsoft.com:444/v1.0/me",
            "/me/todo/lists#secret",
            "https://graph.microsoft.com\\@evil.example/v1.0/",
        ] {
            assert!(graph.validate_url(bad).is_err(), "accepted {bad}");
        }
        assert_eq!(segment("a/b+=中文?"), "a%2Fb%2B%3D%E4%B8%AD%E6%96%87%3F");
    }
    #[test]
    fn respects_retry_after_budget() {
        assert_eq!(retry_after(Some("2"), 0), Some(Duration::from_secs(2)));
        assert_eq!(retry_after(Some("120"), 0), None);
        assert_eq!(
            status_error(StatusCode::GONE, true).code,
            "sync_cursor_expired"
        );
    }
    #[tokio::test]
    async fn reads_retry_but_ambiguous_writes_do_not() {
        async fn handler(
            State(n): State<Arc<AtomicUsize>>,
            headers: HeaderMap,
        ) -> impl IntoResponse {
            assert_eq!(headers["authorization"], "Bearer synthetic-token");
            n.fetch_add(1, Ordering::SeqCst);
            (StatusCode::SERVICE_UNAVAILABLE, "private upstream body")
        }
        let n = Arc::new(AtomicUsize::new(0));
        let (graph, task) = serve(
            Router::new()
                .route("/v1.0/me/todo/lists", any(handler))
                .with_state(n.clone()),
        )
        .await;
        assert_eq!(
            graph.get("a", "/me/todo/lists").await.unwrap_err().code,
            "graph_unavailable"
        );
        assert_eq!(n.load(Ordering::SeqCst), READ_ATTEMPTS);
        assert_eq!(
            graph
                .write(
                    "a",
                    Method::POST,
                    "/me/todo/lists",
                    Some(serde_json::json!({"displayName":"x"}))
                )
                .await
                .unwrap_err()
                .code,
            "write_unknown"
        );
        assert_eq!(n.load(Ordering::SeqCst), READ_ATTEMPTS + 1);
        task.abort();
    }
    #[tokio::test]
    async fn classifies_unauthorized_not_found_and_rejected_writes() {
        let n = Arc::new(AtomicUsize::new(0));
        async fn rejected(State(n): State<Arc<AtomicUsize>>) -> StatusCode {
            n.fetch_add(1, Ordering::SeqCst);
            StatusCode::BAD_REQUEST
        }
        let router = Router::new()
            .route("/v1.0/missing", any(|| async { StatusCode::NOT_FOUND }))
            .route(
                "/v1.0/unauthorized",
                any(|| async { StatusCode::UNAUTHORIZED }),
            )
            .route("/v1.0/rejected", any(rejected))
            .with_state(n.clone());
        let (graph, task) = serve(router).await;
        assert_eq!(
            graph.get("a", "/missing").await.unwrap_err().code,
            "not_found"
        );
        assert_eq!(
            graph.get("a", "/unauthorized").await.unwrap_err().code,
            "account_auth_required"
        );
        assert_eq!(
            graph
                .write("a", Method::POST, "/rejected", None)
                .await
                .unwrap_err()
                .code,
            "invalid_argument"
        );
        assert_eq!(n.load(Ordering::SeqCst), 1);
        task.abort();
    }
    #[tokio::test]
    async fn follows_pages_and_rejects_foreign_next_link() {
        async fn page(headers: HeaderMap) -> impl IntoResponse {
            let host = headers["host"].to_str().unwrap();
            axum::Json(
                serde_json::json!({"value":[{"id":"1"}],"@odata.nextLink":format!("http://{host}/v1.0/page2")}),
            )
        }
        let router = Router::new().route("/v1.0/me/todo/lists", any(page))
            .route("/v1.0/page2", any(|| async { axum::Json(serde_json::json!({"value":[{"id":"2"}]})) }))
            .route("/v1.0/evil", any(|| async { axum::Json(serde_json::json!({"value":[],"@odata.nextLink":"https://evil.example/v1.0/me"})) }));
        let (graph, task) = serve(router).await;
        assert_eq!(
            graph.collection("a", "/me/todo/lists").await.unwrap().len(),
            2
        );
        assert_eq!(
            graph.collection("a", "/evil").await.unwrap_err().code,
            "invalid_graph_url"
        );
        task.abort();
    }

    #[tokio::test]
    async fn retries_throttled_read_and_refuses_redirects() {
        async fn throttled(State(n): State<Arc<AtomicUsize>>) -> axum::response::Response {
            if n.fetch_add(1, Ordering::SeqCst) == 0 {
                (
                    StatusCode::TOO_MANY_REQUESTS,
                    [("retry-after", "0")],
                    "secret",
                )
                    .into_response()
            } else {
                axum::Json(serde_json::json!({"value":[]})).into_response()
            }
        }
        let n = Arc::new(AtomicUsize::new(0));
        let router = Router::new()
            .route("/v1.0/throttle", any(throttled))
            .with_state(n.clone())
            .route(
                "/v1.0/redirect",
                any(|| async {
                    (
                        StatusCode::FOUND,
                        [("location", "https://evil.example/collect")],
                    )
                }),
            );
        let (graph, task) = serve(router).await;
        assert_eq!(
            graph.get("a", "/throttle").await.unwrap(),
            serde_json::json!({"value":[]})
        );
        assert_eq!(n.load(Ordering::SeqCst), 2);
        assert_eq!(
            graph.get("a", "/redirect").await.unwrap_err().code,
            "graph_redirect"
        );
        task.abort();
    }
    #[tokio::test]
    async fn rejects_large_and_malformed_responses_without_replaying_writes() {
        let router = Router::new()
            .route(
                "/v1.0/large",
                any(|| async { "x".repeat(MAX_RESPONSE + 1) }),
            )
            .route("/v1.0/malformed", any(|| async { "not json" }))
            .route("/v1.0/empty", any(|| async { StatusCode::NO_CONTENT }));
        let (graph, task) = serve(router).await;
        assert_eq!(
            graph.get("a", "/large").await.unwrap_err().code,
            "graph_response"
        );
        assert_eq!(
            graph
                .write("a", Method::PATCH, "/malformed", None)
                .await
                .unwrap_err()
                .code,
            "write_unknown"
        );
        assert_eq!(
            graph
                .write("a", Method::DELETE, "/empty", None)
                .await
                .unwrap(),
            Value::Null
        );
        task.abort();
    }
}
