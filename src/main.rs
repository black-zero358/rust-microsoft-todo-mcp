use rust_microsoft_todo_mcp::{
    auth::Auth,
    config::Config,
    db::Db,
    graph::GraphClient,
    server::{StateData, WorkTracker, router_with_tracker},
    sync::Sync,
    tools::Tools,
};
use std::{future::IntoFuture, sync::Arc, time::Duration};
use tokio_util::sync::CancellationToken;
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    let config = Arc::new(Config::from_env()?);
    std::fs::create_dir_all(&config.data_dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&config.data_dir, std::fs::Permissions::from_mode(0o700))?;
    }
    let db = Arc::new(Db::open(&config.data_dir.join("todo.db"))?);
    let auth = Arc::new(Auth::new(config.clone(), db.clone())?);
    let graph = Arc::new(GraphClient::new(auth.clone())?);
    let sync = Arc::new(Sync::new(db.clone(), graph.clone()));
    let tools = Arc::new(Tools {
        db: db.clone(),
        graph,
        sync: sync.clone(),
    });
    let state = StateData {
        config: config.clone(),
        auth,
        db,
        tools,
    };
    let shutdown = CancellationToken::new();
    let background = tokio::spawn(sync.run(config.sync_interval_secs));
    let listener = tokio::net::TcpListener::bind(config.bind).await?;
    tracing::info!(address=%config.bind,"Microsoft To Do MCP listening");
    let work = WorkTracker::new();
    let stop = shutdown.clone();
    let server = axum::serve(
        listener,
        router_with_tracker(state, shutdown.clone(), work.clone()),
    )
    .with_graceful_shutdown(stop.cancelled_owned())
    .into_future();
    tokio::pin!(server);
    tokio::select! {
        result=&mut server=>{result?;},
        _=shutdown_signal()=>{
            work.close();shutdown.cancel();
            match tokio::time::timeout(Duration::from_secs(30),&mut server).await {
                Ok(result) => result?,
                Err(_) => tracing::warn!("HTTP shutdown deadline reached; draining admitted work"),
            }
        }
    }
    if !work.drain(Duration::from_secs(10)).await {
        tracing::warn!("Tool shutdown deadline reached; unfinished writes will recover as unknown");
    }
    background.abort();
    Ok(())
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("register SIGTERM handler");
        tokio::select! { _=tokio::signal::ctrl_c()=>{}, _=terminate.recv()=>{} }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}
