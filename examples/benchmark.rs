//! Reproducible, offline 10k-task cache benchmark. No Microsoft credentials.
use rust_microsoft_todo_mcp::db::{Db, SearchQuery};
use serde_json::json;
use std::time::Instant;
fn main() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let started = Instant::now();
    let db = Db::open(&dir.path().join("benchmark.db"))?;
    let open_ms = started.elapsed().as_secs_f64() * 1000.;
    let a = db.upsert_account("synthetic", "Synthetic account", "not-a-real-token")?;
    db.put_list(
        &a.id,
        a.generation,
        &json!({"id":"list","displayName":"Synthetic"}),
    )?;
    db.sync_cursor(&a.id, a.generation, "tasks:list", "initial")?;
    let records:Vec<_>=(0..10_000).map(|i|json!({"id":format!("task-{i:05}"),"title":format!("中文任务 Project {i}"),"body":{"contentType":"html","content":format!("<p>项目检查与交付 item {i}</p>")},"status":"notStarted"})).collect();
    let load = Instant::now();
    db.apply_sync_page(
        &a.id,
        a.generation,
        "tasks:list",
        &records,
        None,
        Some("final"),
    )?;
    db.set_sync_status(&a.id, a.generation, "ready", None)?;
    let load_ms = load.elapsed().as_secs_f64() * 1000.;
    let mut findings = Vec::new();
    for text in ["中文任务", "项目", "Project 9876"] {
        let q = SearchQuery {
            text: text.into(),
            limit: 25,
            ..Default::default()
        };
        let mut times = Vec::new();
        let mut response_bytes = 0;
        let mut wire_bytes = 0;
        for _ in 0..100 {
            let now = Instant::now();
            let value = db.search(&q)?;
            times.push(now.elapsed().as_secs_f64() * 1000.);
            response_bytes = serde_json::to_vec(&value)?.len();
            wire_bytes=serde_json::to_vec(&json!({"jsonrpc":"2.0","id":1,"result":rmcp::model::CallToolResult::structured(json!({"data":value}))}))?.len();
        }
        times.sort_by(f64::total_cmp);
        findings.push(json!({"query":text,"p50_ms":times[50],"p95_ms":times[95],"json_data_bytes":response_bytes,"json_rpc_wire_bytes":wire_bytes,"graph_requests":0}));
    }
    println!(
        "{}",
        serde_json::to_string_pretty(
            &json!({"tasks":10_000,"iterations":100,"database_open_ms":open_ms,"fixture_load_ms":load_ms,"searches":findings})
        )?
    );
    Ok(())
}
