use crate::{
    App,
    entity::runtime,
    state::{DateTimeUtc, normalize},
    telegram::{listen, retry_delay, safe_error},
};
use anyhow::Context;
use chrono::{DateTime, Utc};
use sea_orm::EntityTrait;
use std::sync::Arc;

impl App {
    /// Run the single scheduler and optional Telegram polling listener.
    pub async fn run(self: Arc<Self>) -> anyhow::Result<()> {
        self.telegram
            .configure(&self.config.telegram_webhook_url)
            .await?;
        if self.config.telegram_webhook_url.trim().is_empty() {
            tokio::try_join!(self.clone().schedule(), listen(self.clone()))?;
            Ok(())
        } else {
            self.schedule().await
        }
    }
    async fn schedule(self: Arc<Self>) -> anyhow::Result<()> {
        let persisted = runtime::Entity::find_by_id(1)
            .one(&self.db)
            .await?
            .context("missing runtime")?;
        let mut previous = persisted.last_tick_at;
        let mut scheduled = None;
        let mut shutdown = self.shutdown.subscribe();
        loop {
            if *shutdown.borrow() {
                return Ok(());
            }
            let now = effective_time(Utc::now(), previous, scheduled);
            previous = Some(now); // Failed attempts also advance the process-local clock.
            match self.tick(now).await {
                Ok(next) => {
                    scheduled = tokio::select! {
                        _ = shutdown.changed() => return Ok(()),
                        _ = self.wake.notified() => None,
                        _ = wait_until(next) => next,
                    };
                }
                Err(error) => {
                    tracing::error!(error = %safe_error(&error), "tick rolled back; retrying");
                    let delay = retry_delay(&error);
                    // A wakeup cannot bypass Telegram's retry_after. Keep its permit for later.
                    tokio::select! {
                        _ = shutdown.changed() => return Ok(()),
                        _ = tokio::time::sleep(delay.to_std().unwrap_or_default()) => {},
                    }
                    scheduled = Some(
                        now.checked_add_signed(delay)
                            .context("retry deadline overflow")?,
                    );
                }
            }
        }
    }
}
async fn wait_until(deadline: Option<DateTimeUtc>) {
    match deadline {
        Some(deadline) => {
            tokio::time::sleep(
                deadline
                    .signed_duration_since(Utc::now())
                    .to_std()
                    .unwrap_or_default(),
            )
            .await
        }
        None => std::future::pending::<()>().await,
    }
}
fn effective_time(
    now: DateTimeUtc,
    previous: Option<DateTimeUtc>,
    scheduled: Option<DateTimeUtc>,
) -> DateTimeUtc {
    normalize(
        previous
            .into_iter()
            .chain(scheduled)
            .fold(now, DateTime::max),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn clock_clamps_scheduled_and_failed_attempt_times() {
        let time = |n| DateTime::<Utc>::from_timestamp_millis(n).unwrap();
        let failed = effective_time(time(1000), None, Some(time(2000)));
        assert_eq!(failed, time(2000));
        assert_eq!(effective_time(time(500), Some(failed), None), failed);
        assert_eq!(
            effective_time(time(500), Some(failed), Some(time(3000))),
            time(3000)
        );
        assert_eq!(effective_time(time(4000), Some(failed), None), time(4000));
    }
}
