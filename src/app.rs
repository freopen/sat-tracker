use crate::{
    bot,
    config::Config,
    db::inbox::IngressSource,
    db::{self, inbox, runtime, settings, tracker},
    mail, menu, notify, template,
    time::{self, DateTimeUtc},
};
use anyhow::Context;
use axum::{
    Router,
    body::Bytes,
    extract::{DefaultBodyLimit, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
};
use frankenstein::{
    AsyncTelegramApi,
    methods::{DeleteWebhookParams, GetUpdatesParams, SetWebhookParams},
    updates::Update,
};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, DatabaseTransaction, EntityTrait,
    IntoActiveModel, QueryFilter, QueryOrder, Set, SqliteTransactionMode, TransactionOptions,
    TransactionTrait,
};
use std::{
    future::{Future, IntoFuture},
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration as StdDuration,
};
use tokio::sync::{Notify, watch};
use tracing::{error, info};

const INBOX_CLEANUP_INTERVAL: StdDuration = StdDuration::from_secs(24 * 60 * 60);

pub(crate) struct Ctx {
    pub(crate) config: crate::config::Config,
    pub(crate) bot: bot::Bot,
    pub(crate) templates: minijinja::Environment<'static>,
    pub(crate) tracker: crate::db::tracker::ActiveModel,
    pub(crate) runtime: crate::db::runtime::ActiveModel,
    pub(crate) settings: crate::db::settings::ActiveModel,
    pub(crate) inbox: Option<crate::db::inbox::Model>,
    pub(crate) now: DateTimeUtc,
}

#[cfg(test)]
pub(crate) fn make_test_ctx() -> Ctx {
    let config = Config {
        ok_regex: regex::Regex::new("^OK$").unwrap(),
        finished_regex: regex::Regex::new("^FINISHED$").unwrap(),
        owner_chat_id: 10,
        safety_chat_id: 20,
        telegram_api_url: "https://example.test".to_owned(),
        telegram_webhook_url: String::new(),
        telegram_bot_token: "test".to_owned(),
        listen_address: "127.0.0.1:0".parse().unwrap(),
    };
    let bot = bot::Bot::new();
    let tracker = tracker::Model {
        id: 1,
        active: false,
        started_at: None,
        started_location: None,
        last_event_at: None,
        last_ok_at: None,
        last_alert: None,
        location: None,
        finished_at: DateTimeUtc::from_timestamp_millis(0).unwrap(),
        owner_reminders_sent: 0,
        safety_reminders_sent: 0,
        safety_alerted: false,
    };
    let runtime = runtime::Model {
        id: 1,
        settings_position: crate::db::runtime::SettingsPosition::Main,
        last_tick_at: DateTimeUtc::from_timestamp_millis(0).unwrap(),
        next_tick_at: None,
        last_processed_inbox_id: 0,
    };
    let settings = settings::Model {
        id: 1,
        owner_reminder_minutes: crate::db::settings::ReminderMinutes(vec![30]),
        safety_reminder_minutes: crate::db::settings::ReminderMinutes(vec![60]),
        safety_alert_template: template::DEFAULT_SAFETY_ALERT_TEMPLATE.to_owned(),
        safety_recovery_template: template::DEFAULT_SAFETY_RECOVERY_TEMPLATE.to_owned(),
    };
    Ctx {
        templates: template::new_environment(
            &settings.safety_alert_template,
            &settings.safety_recovery_template,
        )
        .unwrap(),
        config,
        bot,
        tracker: tracker.into_active_model(),
        runtime: runtime.into_active_model(),
        settings: settings.into_active_model(),
        inbox: None,
        now: DateTimeUtc::from_timestamp_millis(1_700_000_000_000).unwrap(),
    }
}

pub struct App {
    pub(crate) db: DatabaseConnection,
    pub(crate) config: Config,
    pub(crate) bot: bot::Bot,
    wake: Notify,
    shutdown: watch::Sender<bool>,
    started: AtomicBool,
    fatal: Mutex<Option<anyhow::Error>>,
}

impl App {
    pub async fn new() -> anyhow::Result<Self> {
        let config = Config::load()?;
        Self::from_config(config, Path::new("sat-tracker.sqlite")).await
    }

    #[cfg(test)]
    pub(crate) async fn new_for_test(
        config: Config,
        path: impl AsRef<Path>,
    ) -> anyhow::Result<Self> {
        Self::from_config(config, path.as_ref()).await
    }

    async fn from_config(config: Config, path: &Path) -> anyhow::Result<Self> {
        let db = db::open(path).await?;
        let bot = bot::make_bot(&config)?;
        let (shutdown, _) = watch::channel(false);
        Ok(Self {
            db,
            config,
            bot,
            wake: Notify::new(),
            shutdown,
            started: AtomicBool::new(false),
            fatal: Mutex::new(None),
        })
    }

    pub fn shutdown(&self) {
        self.shutdown.send_replace(true);
    }

    pub async fn serve(self: Arc<Self>) -> anyhow::Result<()> {
        if self.started.swap(true, Ordering::AcqRel) {
            anyhow::bail!("App::serve may only be called once");
        }

        let mut ctx = self.load_ctx().await?;
        self.cleanup_inbox(time::now()).await?;
        let listener = tokio::net::TcpListener::bind(self.config.listen_address).await?;
        let address = listener.local_addr()?;
        info!(%address, "listening");
        let webhook_enabled = !self.config.telegram_webhook_url.trim().is_empty();
        let router = router(Arc::clone(&self));
        let shutdown = self.shutdown.subscribe();
        let shutdown_bot = self.bot.clone();
        let server = axum::serve(listener, router)
            .with_graceful_shutdown(async move {
                wait_shutdown(shutdown).await;
                if webhook_enabled
                    && let Err(error) = shutdown_bot
                        .delete_webhook(&DeleteWebhookParams::builder().build())
                        .await
                {
                    error!(
                        reason = %error,
                        "failed to delete Telegram webhook before shutdown"
                    );
                }
            })
            .into_future();
        tokio::pin!(server);

        let configure_webhook = async {
            if webhook_enabled {
                self.bot
                    .set_webhook(
                        &SetWebhookParams::builder()
                            .url(self.config.telegram_webhook_url.trim().to_owned())
                            .build(),
                    )
                    .await?;
            } else {
                self.bot
                    .delete_webhook(&DeleteWebhookParams::builder().build())
                    .await?;
            }
            anyhow::Ok(())
        };
        tokio::select! {
            result = &mut server => {
                self.shutdown();
                result?;
                return Ok(());
            }
            result = configure_webhook => result?,
        }

        let tick = self.clone().run_ticks(&mut ctx);
        let poller = self.clone().run_poller();
        let cleanup = self.clone().run_cleanup();
        let server = async {
            (&mut server).await?;
            anyhow::Ok(())
        };
        let (server_result, tick_result, poller_result, cleanup_result) = tokio::join!(
            self.shutdown_on_error(server),
            self.shutdown_on_error(tick),
            self.shutdown_on_error(poller),
            self.shutdown_on_error(cleanup),
        );
        server_result?;
        tick_result?;
        poller_result?;
        cleanup_result?;
        self.take_fatal()
    }

    async fn run_cleanup(self: Arc<Self>) -> anyhow::Result<()> {
        loop {
            tokio::select! {
                _ = self.shutdown_changed() => return Ok(()),
                _ = time::sleep(INBOX_CLEANUP_INTERVAL) => self.cleanup_inbox(time::now()).await?,
            }
        }
    }

    async fn cleanup_inbox(&self, now: DateTimeUtc) -> anyhow::Result<()> {
        let tx = self.begin_write().await?;
        let cursor = runtime::Entity::find_by_id(1)
            .one(&tx)
            .await?
            .context("missing runtime")?
            .last_processed_inbox_id;
        inbox::Entity::delete_many()
            .filter(inbox::Column::ReceivedAt.lt(now - chrono::Duration::days(6)))
            .filter(inbox::Column::Id.lt(cursor))
            .exec(&tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    async fn run_ticks(self: Arc<Self>, ctx: &mut Ctx) -> anyhow::Result<()> {
        self.wake.notify_one();
        loop {
            if *self.shutdown.borrow() {
                return Ok(());
            }
            let notified = self.wake.notified();
            let next_tick_at = *ctx.runtime.next_tick_at.as_ref();
            tokio::select! {
                _ = self.shutdown_changed() => return Ok(()),
                _ = notified => {
                    let cursor = *ctx.runtime.last_processed_inbox_id.as_ref();
                    let Some(inbox) = inbox::Entity::find()
                        .filter(inbox::Column::Id.gt(cursor))
                        .order_by_asc(inbox::Column::Id)
                        .one(&self.db)
                        .await?
                    else {
                        continue;
                    };
                    ctx.inbox = Some(inbox);
                    self.wake.notify_one();
                }
                _ = time::sleep_until(next_tick_at) => {
                    if let Some(next_tick_at) = next_tick_at {
                        ctx.now = ctx.now.max(next_tick_at);
                    }
                }
            }
            ctx.now = time::normalize(ctx.now.max(time::now()));
            ctx.runtime.last_tick_at.set_ne(ctx.now);
            tick(ctx).await?;
            self.commit(ctx).await?;
        }
    }

    async fn commit(&self, ctx: &mut Ctx) -> anyhow::Result<()> {
        let tx = self.begin_write().await?;
        if ctx.tracker.is_changed() {
            ctx.tracker = std::mem::take(&mut ctx.tracker).save(&tx).await?;
        }
        if ctx.runtime.is_changed() {
            ctx.runtime = std::mem::take(&mut ctx.runtime).save(&tx).await?;
        }
        if ctx.settings.is_changed() {
            ctx.settings = std::mem::take(&mut ctx.settings).save(&tx).await?;
        }
        tx.commit().await?;
        ctx.inbox = None;
        Ok(())
    }

    async fn load_ctx(&self) -> anyhow::Result<Ctx> {
        let tracker = tracker::Entity::find_by_id(1)
            .one(&self.db)
            .await?
            .context("missing tracker")?;
        let runtime = runtime::Entity::find_by_id(1)
            .one(&self.db)
            .await?
            .context("missing runtime")?;
        let settings = settings::Entity::find_by_id(1)
            .one(&self.db)
            .await?
            .context("missing settings")?;
        let now = runtime.last_tick_at;
        let templates = template::new_environment(
            &settings.safety_alert_template,
            &settings.safety_recovery_template,
        )?;
        Ok(Ctx {
            config: self.config.clone(),
            bot: self.bot.clone(),
            templates,
            tracker: tracker.into_active_model(),
            runtime: runtime.into_active_model(),
            settings: settings.into_active_model(),
            inbox: None,
            now,
        })
    }

    async fn begin_write(&self) -> anyhow::Result<DatabaseTransaction> {
        Ok(self
            .db
            .begin_with_options(TransactionOptions {
                sqlite_transaction_mode: Some(SqliteTransactionMode::Immediate),
                ..Default::default()
            })
            .await?)
    }

    async fn admit_mail(&self, payload: Vec<u8>, received_at: DateTimeUtc) -> anyhow::Result<()> {
        anyhow::ensure!(!payload.is_empty(), "empty mail");
        let external_id = mail_message_id(&payload);
        let tx = self.begin_write().await?;
        enqueue(
            &tx,
            IngressSource::Mail,
            external_id,
            payload,
            time::normalize(received_at),
        )
        .await?;
        tx.commit().await?;
        self.wake.notify_one();
        Ok(())
    }

    async fn admit_telegram(&self, update: Update, received_at: DateTimeUtc) -> anyhow::Result<()> {
        let payload = serde_json::to_vec(&update)?;
        let tx = self.begin_write().await?;
        enqueue(
            &tx,
            IngressSource::Telegram,
            Some(update.update_id.to_string()),
            payload,
            time::normalize(received_at),
        )
        .await?;
        tx.commit().await?;
        self.wake.notify_one();
        Ok(())
    }

    async fn poll(self: Arc<Self>) -> anyhow::Result<()> {
        let mut offset = None;
        let mut shutdown = self.shutdown.subscribe();
        loop {
            if *shutdown.borrow() {
                return Ok(());
            }
            let params = if let Some(offset) = offset {
                GetUpdatesParams::builder()
                    .offset(offset)
                    .limit(100)
                    .timeout(30)
                    .allowed_updates(vec![frankenstein::types::AllowedUpdate::Message])
                    .build()
            } else {
                GetUpdatesParams::builder()
                    .limit(100)
                    .timeout(30)
                    .allowed_updates(vec![frankenstein::types::AllowedUpdate::Message])
                    .build()
            };
            let result = tokio::select! {
                _ = shutdown.changed() => return Ok(()),
                result = self.bot.get_updates(&params) => result?,
            };
            for update in result.result {
                let update_id = update.update_id;
                self.admit_telegram(update, time::now()).await?;
                offset = Some(i64::from(update_id).saturating_add(1));
            }
        }
    }

    async fn run_poller(self: Arc<Self>) -> anyhow::Result<()> {
        if self.config.telegram_webhook_url.trim().is_empty() {
            self.poll().await
        } else {
            self.shutdown_changed().await;
            Ok(())
        }
    }

    async fn shutdown_on_error<F>(&self, job: F) -> anyhow::Result<()>
    where
        F: Future<Output = anyhow::Result<()>>,
    {
        let result = job.await;
        if result.is_err() {
            self.shutdown();
        }
        result
    }

    pub(crate) fn report_fatal(&self, error: anyhow::Error) {
        let mut fatal = self.fatal.lock().expect("fatal mutex is not poisoned");
        if fatal.is_none() {
            *fatal = Some(error);
            self.shutdown();
        }
    }

    fn take_fatal(&self) -> anyhow::Result<()> {
        self.fatal
            .lock()
            .expect("fatal mutex is not poisoned")
            .take()
            .map_or(Ok(()), Err)
    }

    async fn shutdown_changed(&self) {
        let mut receiver = self.shutdown.subscribe();
        if !*receiver.borrow() {
            let _ = receiver.changed().await;
        }
    }
}

async fn tick(ctx: &mut Ctx) -> anyhow::Result<()> {
    if let Some(source) = ctx.inbox.as_ref().map(|row| row.source) {
        match source {
            IngressSource::Mail => mail::process(ctx).await?,
            IngressSource::Telegram => menu::process(ctx).await?,
        }
        let row_id = ctx
            .inbox
            .as_ref()
            .context("pending inbox row missing after processing")?
            .id;
        ctx.runtime.last_processed_inbox_id.set_ne(row_id);
    }
    notify::send_due_reminders(ctx).await?;
    Ok(())
}

async fn enqueue(
    tx: &DatabaseTransaction,
    source: IngressSource,
    external_id: Option<String>,
    payload: Vec<u8>,
    received_at: DateTimeUtc,
) -> anyhow::Result<()> {
    if let Some(external_id) = external_id.as_deref() {
        let duplicate = inbox::Entity::find()
            .filter(inbox::Column::Source.eq(source))
            .filter(inbox::Column::ExternalId.eq(external_id))
            .one(tx)
            .await?
            .is_some();
        if duplicate {
            info!(source = ?source, "ingress event already recorded");
            return Ok(());
        }
    }
    inbox::ActiveModel {
        source: Set(source),
        external_id: Set(external_id),
        received_at: Set(received_at),
        payload: Set(Some(payload)),
        processed_at: Set(None),
        ..Default::default()
    }
    .insert(tx)
    .await?;
    Ok(())
}

fn mail_message_id(payload: &[u8]) -> Option<String> {
    mail_parser::MessageParser::default()
        .parse(payload)
        .and_then(|mail| mail.message_id().map(str::to_owned))
}

async fn wait_shutdown(mut shutdown: watch::Receiver<bool>) {
    while !*shutdown.borrow() {
        if shutdown.changed().await.is_err() {
            break;
        }
    }
}

fn router(app: Arc<App>) -> Router {
    let router = Router::new()
        .route("/mail", post(mail))
        .route("/tg", post(telegram))
        .route("/healthz", get(health))
        .layer(DefaultBodyLimit::max(1024 * 1024));
    #[cfg(feature = "e2e")]
    let router = router.route("/time", get(time::route));
    router.with_state(app)
}

async fn health() -> StatusCode {
    StatusCode::NO_CONTENT
}

async fn mail(State(app): State<Arc<App>>, body: Bytes) -> impl IntoResponse {
    if body.is_empty() {
        return StatusCode::BAD_REQUEST;
    }
    match app.admit_mail(body.to_vec(), time::now()).await {
        Ok(()) => StatusCode::NO_CONTENT,
        Err(error) => {
            error!(reason = %bot::safe_error(&error), bytes = body.len(), "failed to durably enqueue mail");
            app.report_fatal(error);
            StatusCode::INTERNAL_SERVER_ERROR
        }
    }
}

async fn telegram(State(app): State<Arc<App>>, body: Bytes) -> impl IntoResponse {
    let update: Update = match serde_json::from_slice(&body) {
        Ok(update) => update,
        Err(_) => return StatusCode::BAD_REQUEST,
    };
    match app.admit_telegram(update, time::now()).await {
        Ok(()) => StatusCode::NO_CONTENT,
        Err(error) => {
            error!(reason = %bot::safe_error(&error), "failed to durably enqueue Telegram update");
            app.report_fatal(error);
            StatusCode::INTERNAL_SERVER_ERROR
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use frankenstein::{response::MethodResponse, types::Message};
    use regex::Regex;
    use sea_orm::{ConnectionTrait, EntityTrait};

    fn config() -> Config {
        Config {
            ok_regex: Regex::new("^OK$").unwrap(),
            finished_regex: Regex::new("^FINISHED$").unwrap(),
            owner_chat_id: 10,
            safety_chat_id: 20,
            telegram_api_url: "https://example.test".to_owned(),
            telegram_webhook_url: String::new(),
            telegram_bot_token: "test".to_owned(),
            listen_address: "127.0.0.1:0".parse().unwrap(),
        }
    }

    fn telegram_success() -> MethodResponse<Message> {
        serde_json::from_value(serde_json::json!({
            "ok": true,
            "result": {
                "message_id": 1,
                "date": 1700000000,
                "chat": {"id": 10, "type": "private"},
                "text": "sent"
            }
        }))
        .unwrap()
    }

    #[tokio::test]
    async fn processing_commits_cursor_without_mutating_the_inbox_row() {
        let directory = tempfile::TempDir::new().unwrap();
        let mut app = App::new_for_test(config(), directory.path().join("test.sqlite"))
            .await
            .unwrap();
        app.bot.expect_clone().returning(crate::bot::Bot::new);
        let received = Utc.timestamp_millis_opt(1_700_000_000_000).unwrap();
        app.admit_mail(
            b"Message-ID: <start>\r\nContent-Type: text/plain\r\n\r\nOK".to_vec(),
            received,
        )
        .await
        .unwrap();
        let mut ctx = app.load_ctx().await.unwrap();
        ctx.bot
            .expect_send_rich_message()
            .times(2)
            .returning(|_| Ok(telegram_success()));
        ctx.now = received;
        ctx.inbox = inbox::Entity::find_by_id(1).one(&app.db).await.unwrap();
        tick(&mut ctx).await.unwrap();
        app.commit(&mut ctx).await.unwrap();

        let row = inbox::Entity::find_by_id(1)
            .one(&app.db)
            .await
            .unwrap()
            .unwrap();
        assert!(row.payload.is_some());
        assert!(row.processed_at.is_none());
        assert_eq!(
            runtime::Entity::find_by_id(1)
                .one(&app.db)
                .await
                .unwrap()
                .unwrap()
                .last_processed_inbox_id,
            1
        );
        assert!(
            tracker::Entity::find_by_id(1)
                .one(&app.db)
                .await
                .unwrap()
                .unwrap()
                .active
        );
    }

    #[tokio::test]
    async fn inbox_cleanup_removes_only_old_rows_below_cursor() {
        let directory = tempfile::TempDir::new().unwrap();
        let app = App::new_for_test(config(), directory.path().join("test.sqlite"))
            .await
            .unwrap();
        app.db
            .execute_unprepared(
                "INSERT INTO inbox (id, source, external_id, received_at, payload) VALUES
                    (1, 'mail', 'old-1', '2023-11-01 00:00:00', X'4F4B'),
                    (2, 'mail', 'old-2', '2023-11-01 00:00:00', X'4F4B'),
                    (3, 'mail', 'old-3', '2023-11-01 00:00:00', X'4F4B'),
                    (4, 'mail', 'old-4', '2023-11-01 00:00:00', X'4F4B');
                 UPDATE runtime SET last_processed_inbox_id = 3 WHERE id = 1;",
            )
            .await
            .unwrap();

        app.cleanup_inbox(Utc.timestamp_millis_opt(1_700_000_000_000).unwrap())
            .await
            .unwrap();

        assert!(
            inbox::Entity::find_by_id(1)
                .one(&app.db)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            inbox::Entity::find_by_id(2)
                .one(&app.db)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            inbox::Entity::find_by_id(3)
                .one(&app.db)
                .await
                .unwrap()
                .is_some()
        );
        assert!(
            inbox::Entity::find_by_id(4)
                .one(&app.db)
                .await
                .unwrap()
                .is_some()
        );
    }
}
