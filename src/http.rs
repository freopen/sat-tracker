use std::{sync::Arc, time::SystemTime};

use axum::{
    Router,
    body::Bytes,
    extract::{DefaultBodyLimit, Json, State},
    http::StatusCode,
    routing::{get, post},
};
use frankenstein::updates::Update;
use tracing::{error, info, warn};

use crate::{
    actions::{ProcessMail, ProcessTelegram},
    app::App,
    state::RawMail,
};

pub fn router(app: Arc<App>) -> Router {
    Router::new()
        .route("/mail", post(mail))
        .route("/tg", post(telegram))
        .route("/healthz", get(health))
        .layer(DefaultBodyLimit::max(1024 * 1024))
        .with_state(app)
}

async fn health() -> StatusCode {
    StatusCode::NO_CONTENT
}

async fn mail(State(app): State<Arc<App>>, body: Bytes) -> StatusCode {
    if body.is_empty() {
        warn!("rejected empty mail event");
        return StatusCode::BAD_REQUEST;
    }
    info!(bytes = body.len(), "new mail event");
    let raw = RawMail {
        bytes: body.to_vec(),
        received_at: SystemTime::now(),
    };
    match app.handle.enqueue::<ProcessMail>(&raw).await {
        Ok(_) => StatusCode::NO_CONTENT,
        Err(error) => {
            error!(%error, bytes = body.len(), "failed to durably enqueue mail");
            StatusCode::INTERNAL_SERVER_ERROR
        }
    }
}

async fn telegram(State(app): State<Arc<App>>, Json(update): Json<Update>) -> StatusCode {
    info!(update_id = update.update_id, "new Telegram update");
    match app.handle.enqueue::<ProcessTelegram>(&update).await {
        Ok(_) => StatusCode::NO_CONTENT,
        Err(error) => {
            error!(
                %error,
                update_id = update.update_id,
                "failed to durably enqueue Telegram update"
            );
            StatusCode::INTERNAL_SERVER_ERROR
        }
    }
}

#[cfg(test)]
pub(crate) async fn submit(app: Arc<App>, body: Bytes) -> StatusCode {
    mail(State(app), body).await
}
