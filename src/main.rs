use std::{future::IntoFuture, net::SocketAddr, sync::Arc};

use anyhow::Result;
use sat_tracker::{App, Config, router};
use tokio::net::TcpListener;
use tracing::info;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| "sat_tracker=info".into()),
        )
        .init();

    let config = Config::load()?;

    let (app, runner) = App::start(config, "sat-tracker.sqlite").await?;
    let app = Arc::new(app);

    let router = router(Arc::clone(&app));
    let address: SocketAddr = "0.0.0.0:8080".parse().unwrap();
    let listener = TcpListener::bind(address).await?;
    info!(%address, "listening");
    let server = axum::serve(listener, router)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
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
            Ok(()) => anyhow::bail!("durable action runner stopped unexpectedly"),
            Err(error) => return Err(error.into()),
        },
    }
    Ok(())
}
