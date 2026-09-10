use std::{future::IntoFuture, net::SocketAddr, sync::Arc};

use anyhow::Result;
use sat_tracker::{App, Config, build_info, router};
use tokio::net::TcpListener;
use tokio::signal::unix::{SignalKind, signal};
use tracing::info;
use tracing_subscriber::EnvFilter;

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    let terminate = async {
        let mut signal =
            signal(SignalKind::terminate()).expect("failed to install SIGTERM handler");
        signal.recv().await;
    };

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| "sat_tracker=info".into()),
        )
        .init();

    let build = build_info();
    info!(
        version = build.version,
        build_time = build.build_time,
        git_commit = build.git_commit,
        git_dirty = build.git_dirty,
        "sat-tracker version"
    );

    let config = Config::load()?;

    let app = Arc::new(App::open(config, "sat-tracker.sqlite").await?);
    let runner = app.clone().run();

    let router = router(Arc::clone(&app));
    let address: SocketAddr = "0.0.0.0:8080".parse().unwrap();
    let listener = TcpListener::bind(address).await?;
    info!(%address, "listening");
    let server = axum::serve(listener, router)
        .with_graceful_shutdown(shutdown_signal())
        .into_future();
    tokio::pin!(server);
    tokio::pin!(runner);
    tokio::select! {
        result = &mut server => {
            app.shutdown();
            let runner_result = runner.await;
            result?;
            runner_result?;
        }
        result = &mut runner => match result {
            Ok(()) => anyhow::bail!("tick scheduler stopped unexpectedly"),
            Err(error) => return Err(error),
        },
    }
    Ok(())
}
