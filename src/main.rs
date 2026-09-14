use anyhow::Result;
use sat_tracker::App;
use std::sync::Arc;
use tokio::signal::unix::{Signal, SignalKind, signal};
use tracing::info;
use tracing_subscriber::EnvFilter;

async fn shutdown_signal(app: Arc<App>, mut terminate: Signal) -> Result<()> {
    let ctrl_c = tokio::signal::ctrl_c();
    tokio::select! {
        result = ctrl_c => result?,
        _ = terminate.recv() => {},
    }
    app.shutdown();
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| "sat_tracker=info".into()),
        )
        .init();

    info!(
        version = env!("CARGO_PKG_VERSION"),
        build_time = option_env!("VERGEN_BUILD_TIMESTAMP").unwrap_or("unknown"),
        git_commit = option_env!("VERGEN_GIT_SHA").unwrap_or("unknown"),
        git_dirty = option_env!("VERGEN_GIT_DIRTY").unwrap_or("unknown"),
        "sat-tracker version"
    );

    let terminate = signal(SignalKind::terminate())?;
    let app = Arc::new(App::new().await?);
    let signal_task = tokio::spawn(shutdown_signal(Arc::clone(&app), terminate));
    let result = app.serve().await;
    signal_task.abort();
    result
}
