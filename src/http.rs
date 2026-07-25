use std::{sync::Arc, time::SystemTime};

use axum::{
    Router,
    body::Bytes,
    extract::{DefaultBodyLimit, State},
    http::StatusCode,
    routing::{get, post},
};
use tracing::error;

use crate::{actions::ProcessMail, app::App, state::RawMail};

pub fn router(app: Arc<App>) -> Router {
    Router::new()
        .route("/mail", post(mail))
        .route("/healthz", get(health))
        .layer(DefaultBodyLimit::max(1024 * 1024))
        .with_state(app)
}

async fn health() -> StatusCode {
    StatusCode::NO_CONTENT
}

async fn mail(State(app): State<Arc<App>>, body: Bytes) -> StatusCode {
    if body.is_empty() {
        return StatusCode::BAD_REQUEST;
    }
    let raw = RawMail {
        bytes: body.to_vec(),
        received_at: SystemTime::now(),
    };
    match app.handle.enqueue::<ProcessMail>(&raw).await {
        Ok(_) => StatusCode::NO_CONTENT,
        Err(error) => {
            error!(%error, "failed to durably enqueue mail");
            StatusCode::INTERNAL_SERVER_ERROR
        }
    }
}

#[cfg(test)]
pub(crate) async fn submit(app: Arc<App>, body: Bytes) -> StatusCode {
    mail(State(app), body).await
}
