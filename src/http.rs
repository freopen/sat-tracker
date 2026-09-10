use std::sync::Arc;

use axum::{
    Router,
    body::Bytes,
    extract::{DefaultBodyLimit, Json, State},
    http::StatusCode,
    routing::{get, post},
};
use chrono::Utc;
use frankenstein::updates::Update;
use tracing::{error, info, warn};

use crate::app::App;

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
    match app.accept_mail(body.to_vec(), Utc::now()).await {
        Ok(_) => StatusCode::NO_CONTENT,
        Err(error) => {
            error!(%error, bytes = body.len(), "failed to durably enqueue mail");
            StatusCode::INTERNAL_SERVER_ERROR
        }
    }
}

async fn telegram(State(app): State<Arc<App>>, Json(update): Json<Update>) -> StatusCode {
    info!(update_id = update.update_id, "new Telegram update");
    match app.accept_telegram(update.clone(), Utc::now()).await {
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
