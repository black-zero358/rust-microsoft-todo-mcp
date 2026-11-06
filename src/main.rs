use rust_microsoft_todo_mcp::{
    auth::Auth,
    config::Config,
    db::Db,
    graph::GraphClient,
    server::{StateData, router},
    sync::Sync,
    tools::Tools,
};
use std::sync::Arc;
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
    let stop = shutdown.clone();
    axum::serve(listener, router(state, shutdown))
        .with_graceful_shutdown(async move {
            shutdown_signal().await;
            stop.cancel();
        })
        .await?;
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
