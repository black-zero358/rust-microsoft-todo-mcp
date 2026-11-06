//! Resumable Graph delta synchronization with account-generation fences.
use crate::{
    Error, Result,
    db::Db,
    graph::{GraphClient, segment},
};
use serde_json::Value;
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::Duration,
};
use tokio::sync::Mutex;

pub struct Sync {
    db: Arc<Db>,
    graph: Arc<GraphClient>,
    locks: Mutex<HashMap<String, Arc<Mutex<()>>>>,
}
impl Sync {
    pub fn new(db: Arc<Db>, graph: Arc<GraphClient>) -> Self {
        Self {
            db,
            graph,
            locks: Mutex::new(HashMap::new()),
        }
    }
    pub async fn refresh(&self, account: &str) -> Result<Value> {
        self.db.account(account)?;
        let lock = self
            .locks
            .lock()
            .await
            .entry(account.to_owned())
            .or_default()
            .clone();
        let _guard = lock.lock().await;
        let generation = self.db.account(account)?.generation;
        self.db
            .set_sync_status(account, generation, "syncing", None)?;
        let result = self.refresh_inner(account, generation).await;
        match result {
            Ok(()) => {
                self.db
                    .set_sync_status(account, generation, "ready", None)?;
                self.db.sync_status(account)
            }
            Err(e) => {
                // A revoked/replaced account must never be recreated by failure bookkeeping.
                let _ = self
                    .db
                    .set_sync_status(account, generation, "error", Some(&e.code));
                Err(e)
            }
        }
    }
    async fn refresh_inner(&self, account: &str, generation: i64) -> Result<()> {
        self.stream(account, generation, "lists", "/me/todo/lists/delta")
            .await?;
        for list in self.db.lists(account)? {
            let id = list.get("id").and_then(Value::as_str).ok_or_else(|| {
                Error::new("invalid_graph_response", "A synchronized list has no ID")
            })?;
            let initial = format!("/me/todo/lists/{}/tasks/delta", segment(id));
            self.stream(account, generation, &format!("tasks:{id}"), &initial)
                .await?;
        }
        Ok(())
    }
    async fn stream(
        &self,
        account: &str,
        generation: i64,
        stream: &str,
        initial: &str,
    ) -> Result<()> {
        let mut seen = HashSet::new();
        let mut resets = 0;
        for _ in 0..10_000 {
            let cursor = self.db.sync_cursor(account, generation, stream, initial)?;
            if !seen.insert(cursor.url.clone()) {
                return Err(Error::new(
                    "invalid_graph_response",
                    "Graph returned a repeated synchronization continuation",
                ));
            }
            let page = match self.graph.get(account, &cursor.url).await {
                Ok(page) => page,
                Err(e) if e.code == "sync_cursor_expired" && resets == 0 => {
                    resets += 1;
                    seen.clear();
                    self.db.reset_cursor(account, generation, stream, initial)?;
                    continue;
                }
                Err(e) => return Err(e),
            };
            let values = page.get("value").and_then(Value::as_array).ok_or_else(|| {
                Error::new(
                    "invalid_graph_response",
                    "Graph delta response has no value array",
                )
            })?;
            let next = page.get("@odata.nextLink").and_then(Value::as_str);
            let delta = page.get("@odata.deltaLink").and_then(Value::as_str);
            if next.is_some() == delta.is_some() {
                return Err(Error::new(
                    "invalid_graph_response",
                    "Graph delta response must contain exactly one continuation or delta link",
                ));
            }
            for link in [next, delta].into_iter().flatten() {
                self.graph.validate_url(link)?;
            }
            self.db
                .apply_sync_page(account, generation, stream, values, next, delta)?;
            if next.is_none() {
                return Ok(());
            }
            tokio::task::yield_now().await;
        }
        Err(Error::new(
            "sync_page_limit",
            "Synchronization reached its page budget; progress is saved for the next refresh",
        ))
    }
    pub async fn run(self: Arc<Self>, interval_secs: u64) {
        let mut interval = tokio::time::interval(Duration::from_secs(interval_secs.max(15)));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            match self.db.accounts() {
                Ok(accounts) => {
                    for account in accounts {
                        if let Err(e) = self.refresh(&account.id).await {
                            tracing::warn!(code = %e.code, "Background To Do synchronization did not complete");
                        }
                    }
                    let existing: HashSet<String> = self
                        .db
                        .accounts()
                        .unwrap_or_default()
                        .into_iter()
                        .map(|a| a.id)
                        .collect();
                    self.locks
                        .lock()
                        .await
                        .retain(|id, lock| existing.contains(id) || Arc::strong_count(lock) > 1);
                }
                Err(e) => {
                    tracing::warn!(code = %e.code, "Could not read background synchronization accounts")
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Json, Router,
        extract::State,
        http::{StatusCode, Uri},
        routing::get,
    };
    use serde_json::json;
    use std::collections::VecDeque;
    type Reply = (String, StatusCode, Value);
    async fn mock(
        State(queue): State<Arc<Mutex<VecDeque<Reply>>>>,
        uri: Uri,
    ) -> (StatusCode, Json<Value>) {
        let (path, status, value) = queue
            .lock()
            .await
            .pop_front()
            .expect("Unexpected Graph call");
        assert_eq!(uri.path_and_query().unwrap().as_str(), path);
        (status, Json(value))
    }
    async fn graph(
        replies: Vec<Reply>,
    ) -> (
        Arc<GraphClient>,
        Arc<Mutex<VecDeque<Reply>>>,
        tokio::task::JoinHandle<()>,
    ) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let root = format!("http://{}/v1.0/", listener.local_addr().unwrap());
        let queue = Arc::new(Mutex::new(VecDeque::from(replies)));
        let router = Router::new().fallback(get(mock)).with_state(queue.clone());
        let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        (Arc::new(GraphClient::for_test(&root)), queue, task)
    }
    fn ok(path: &str, value: Value) -> Reply {
        (path.into(), StatusCode::OK, value)
    }
    #[tokio::test]
    async fn full_sync_empty_page_and_delta_update() {
        let dir = tempfile::tempdir().unwrap();
        let db = Arc::new(Db::open(&dir.path().join("cache.db")).unwrap());
        db.upsert_account("a", "A", "encrypted").unwrap();
        let (graph,queue,server)=graph(vec![
            ok("/v1.0/me/todo/lists/delta",json!({"value":[],"@odata.nextLink":"/me/todo/lists/delta?page=2"})),
            ok("/v1.0/me/todo/lists/delta?page=2",json!({"value":[{"id":"list"}],"@odata.deltaLink":"/me/todo/lists/delta?token=l1"})),
            ok("/v1.0/me/todo/lists/list/tasks/delta",json!({"value":[{"id":"task","title":"中文任务"}],"@odata.deltaLink":"/me/todo/lists/list/tasks/delta?token=t1"})),
            ok("/v1.0/me/todo/lists/delta?token=l1",json!({"value":[],"@odata.deltaLink":"/me/todo/lists/delta?token=l2"})),
            ok("/v1.0/me/todo/lists/list/tasks/delta?token=t1",json!({"value":[{"id":"task","status":"completed"}],"@odata.deltaLink":"/me/todo/lists/list/tasks/delta?token=t2"})),
        ]).await;
        let sync = Sync::new(db.clone(), graph);
        assert_eq!(db.sync_status("a").unwrap()["complete"], false);
        assert_eq!(sync.refresh("a").await.unwrap()["state"], "ready");
        assert_eq!(db.task("a", "list", "task").unwrap()["title"], "中文任务");
        sync.refresh("a").await.unwrap();
        let task = db.task("a", "list", "task").unwrap();
        assert_eq!(task["status"], "completed");
        assert_eq!(task["title"], "中文任务");
        assert!(queue.lock().await.is_empty());
        server.abort();
    }
    #[tokio::test]
    async fn expired_cursor_rebuild_replaces_stale_tasks() {
        let dir = tempfile::tempdir().unwrap();
        let db = Arc::new(Db::open(&dir.path().join("cache.db")).unwrap());
        let a = db.upsert_account("a", "A", "encrypted").unwrap();
        db.sync_cursor("a", a.generation, "lists", "/me/todo/lists/delta")
            .unwrap();
        db.apply_sync_page(
            "a",
            a.generation,
            "lists",
            &[json!({"id":"list"})],
            None,
            Some("/me/todo/lists/delta?token=old"),
        )
        .unwrap();
        db.sync_cursor(
            "a",
            a.generation,
            "tasks:list",
            "/me/todo/lists/list/tasks/delta",
        )
        .unwrap();
        db.apply_sync_page(
            "a",
            a.generation,
            "tasks:list",
            &[json!({"id":"stale"})],
            None,
            Some("/me/todo/lists/list/tasks/delta?token=old"),
        )
        .unwrap();
        let (graph,queue,server)=graph(vec![
            ok("/v1.0/me/todo/lists/delta?token=old",json!({"value":[],"@odata.deltaLink":"/me/todo/lists/delta?token=new"})),
            ("/v1.0/me/todo/lists/list/tasks/delta?token=old".into(),StatusCode::GONE,json!({"error":{"code":"syncStateNotFound"}})),
            ok("/v1.0/me/todo/lists/list/tasks/delta",json!({"value":[{"id":"fresh","title":"重建"}],"@odata.deltaLink":"/me/todo/lists/list/tasks/delta?token=new"})),
        ]).await;
        Sync::new(db.clone(), graph).refresh("a").await.unwrap();
        assert!(db.task("a", "list", "stale").is_err());
        assert_eq!(db.task("a", "list", "fresh").unwrap()["title"], "重建");
        assert!(queue.lock().await.is_empty());
        server.abort();
    }
    #[tokio::test]
    async fn malformed_cycle_keeps_saved_continuation_and_reports_error() {
        let dir = tempfile::tempdir().unwrap();
        let db = Arc::new(Db::open(&dir.path().join("cache.db")).unwrap());
        let a = db.upsert_account("a", "A", "encrypted").unwrap();
        let (graph, _, server) = graph(vec![
            ok(
                "/v1.0/me/todo/lists/delta",
                json!({"value":[{"id":"staged"}],"@odata.nextLink":"/me/todo/lists/delta?page=2"}),
            ),
            ok(
                "/v1.0/me/todo/lists/delta?page=2",
                json!({"value":[],"@odata.nextLink":"/me/todo/lists/delta?page=2"}),
            ),
        ])
        .await;
        let err = Sync::new(db.clone(), graph).refresh("a").await.unwrap_err();
        assert_eq!(err.code, "invalid_graph_response");
        assert!(db.lists("a").unwrap().is_empty());
        assert_eq!(db.sync_status("a").unwrap()["state"], "error");
        assert_eq!(
            db.sync_cursor("a", a.generation, "lists", "initial")
                .unwrap()
                .url,
            "/me/todo/lists/delta?page=2"
        );
        server.abort();
    }
    #[tokio::test]
    async fn account_revocation_fences_inflight_http_page() {
        let dir = tempfile::tempdir().unwrap();
        let db = Arc::new(Db::open(&dir.path().join("cache.db")).unwrap());
        db.upsert_account("a", "A", "encrypted").unwrap();
        let started = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let root = format!("http://{}/v1.0/", listener.local_addr().unwrap());
        let s = started.clone();
        let r = release.clone();
        let router=Router::new().fallback(get(move||{let s=s.clone();let r=r.clone();async move{s.notify_one();r.notified().await;Json(json!({"value":[{"id":"late"}],"@odata.deltaLink":"/me/todo/lists/delta?token=d"}))}}));
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let sync = Arc::new(Sync::new(
            db.clone(),
            Arc::new(GraphClient::for_test(&root)),
        ));
        let work = tokio::spawn(async move { sync.refresh("a").await });
        started.notified().await;
        db.remove_account("a").unwrap();
        db.upsert_account("a", "Replacement", "new-encrypted")
            .unwrap();
        release.notify_one();
        assert_eq!(work.await.unwrap().unwrap_err().code, "account_changed");
        assert!(db.lists("a").unwrap().is_empty());
        server.abort();
    }
}
