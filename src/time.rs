use chrono::{DateTime, Utc};

pub(crate) type DateTimeUtc = DateTime<Utc>;

#[cfg(not(feature = "e2e"))]
mod imp {
    use super::DateTimeUtc;
    use chrono::{Timelike, Utc};
    use std::time::Duration;

    pub(crate) fn now() -> DateTimeUtc {
        normalize(Utc::now())
    }

    pub(crate) fn normalize(time: DateTimeUtc) -> DateTimeUtc {
        time.with_nanosecond(time.timestamp_subsec_millis() * 1_000_000)
            .expect("millisecond precision is valid for every chrono timestamp")
    }

    pub(crate) async fn sleep(duration: Duration) {
        tokio::time::sleep(duration).await;
    }

    pub(crate) async fn sleep_until(deadline: Option<DateTimeUtc>) {
        match deadline {
            Some(deadline) => {
                let delay = deadline
                    .signed_duration_since(now())
                    .to_std()
                    .unwrap_or_default();
                sleep(delay).await;
            }
            None => std::future::pending::<()>().await,
        }
    }
}

#[cfg(feature = "e2e")]
mod imp {
    use super::DateTimeUtc;
    use crate::app::App;
    use axum::{
        extract::{Query, State},
        http::StatusCode,
        response::IntoResponse,
    };
    use serde::Deserialize;
    use std::sync::{Arc, Mutex, OnceLock};
    use std::time::Duration;
    use tokio::sync::Notify;

    pub(crate) const CLOCK_BACKWARDS: &str = "test clock cannot move backwards";
    pub(crate) const E2E_START_TIME_ENV: &str = "SAT_TRACKER_E2E_START_TIME";

    struct E2eState {
        now: Mutex<DateTimeUtc>,
        changed: Notify,
    }

    static E2E: OnceLock<E2eState> = OnceLock::new();

    fn e2e() -> &'static E2eState {
        E2E.get_or_init(|| E2eState {
            now: Mutex::new(initial_e2e_time()),
            changed: Notify::new(),
        })
    }

    fn initial_e2e_time() -> DateTimeUtc {
        #[cfg(test)]
        let value = std::env::var(E2E_START_TIME_ENV).unwrap_or_else(|_| "0".to_owned());
        #[cfg(not(test))]
        let value = std::env::var(E2E_START_TIME_ENV)
            .expect("e2e binary requires SAT_TRACKER_E2E_START_TIME");
        let millis = value
            .parse::<i64>()
            .expect("SAT_TRACKER_E2E_START_TIME must be an integer Unix timestamp in milliseconds");
        DateTimeUtc::from_timestamp_millis(millis)
            .expect("SAT_TRACKER_E2E_START_TIME must be a valid Unix timestamp in milliseconds")
    }

    pub(crate) fn now() -> DateTimeUtc {
        *e2e().now.lock().expect("test clock mutex is not poisoned")
    }

    #[derive(Deserialize)]
    pub(crate) struct TimeQuery {
        ts: i64,
    }

    pub(crate) async fn route(
        State(app): State<Arc<App>>,
        Query(query): Query<TimeQuery>,
    ) -> impl IntoResponse {
        let Some(now) = DateTimeUtc::from_timestamp_millis(query.ts) else {
            return StatusCode::BAD_REQUEST;
        };
        match set(now) {
            Ok(()) => StatusCode::NO_CONTENT,
            Err(CLOCK_BACKWARDS) => StatusCode::BAD_REQUEST,
            Err(error) => {
                app.report_fatal(anyhow::anyhow!(error));
                StatusCode::INTERNAL_SERVER_ERROR
            }
        }
    }

    pub(crate) fn normalize(time: DateTimeUtc) -> DateTimeUtc {
        use chrono::Timelike;

        time.with_nanosecond(time.timestamp_subsec_millis() * 1_000_000)
            .expect("millisecond precision is valid for every chrono timestamp")
    }

    pub(crate) async fn sleep(duration: Duration) {
        let duration = chrono::Duration::from_std(duration)
            .expect("sleep duration must fit in a chrono duration");
        sleep_until(Some(now() + duration)).await;
    }

    pub(crate) async fn sleep_until(deadline: Option<DateTimeUtc>) {
        let Some(deadline) = deadline else {
            std::future::pending::<()>().await;
            return;
        };
        let state = e2e();
        loop {
            let notified = state.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if now() >= deadline {
                return;
            }
            notified.await;
        }
    }

    pub(crate) fn set(value: DateTimeUtc) -> Result<(), &'static str> {
        let value = normalize(value);
        let state = e2e();
        let mut current = state.now.lock().expect("test clock mutex is not poisoned");
        if value < *current {
            return Err(CLOCK_BACKWARDS);
        }
        *current = value;
        drop(current);
        state.changed.notify_waiters();
        Ok(())
    }
}

pub(crate) use imp::*;
