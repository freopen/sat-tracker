use crate::{
    App, Config,
    app::{enqueue_inbox, schedule_tick},
    state::{DateTimeUtc, IngressSource, normalize},
    telegram::common::{self, Client},
};
use chrono::Utc;
use frankenstein::{
    AsyncTelegramApi,
    client_reqwest::Bot,
    methods::{DeleteWebhookParams, GetUpdatesParams, SetWebhookParams},
    types::AllowedUpdate,
    updates::Update,
};
use sea_orm::DatabaseTransaction;
use std::{sync::Arc, time::Duration};

pub(super) fn new_client(config: &Config) -> anyhow::Result<Client> {
    let http = frankenstein::reqwest::Client::builder()
        .retry(frankenstein::reqwest::retry::never())
        .timeout(Duration::from_secs(40))
        .connect_timeout(Duration::from_secs(10))
        .build()?;
    Ok(Client {
        bot: Bot::builder()
            .api_url(format!(
                "{}/bot{}",
                config.telegram_api_url.trim_end_matches('/'),
                config.telegram_bot_token
            ))
            .client(http)
            .build(),
        owner_chat_id: config.owner_chat_id,
        safety_chat_id: config.safety_chat_id,
    })
}

pub(super) async fn configure(client: &Client, url: &str) -> anyhow::Result<()> {
    if url.trim().is_empty() {
        client
            .bot
            .delete_webhook(&DeleteWebhookParams::builder().build())
            .await?;
    } else {
        client
            .bot
            .set_webhook(
                &SetWebhookParams::builder()
                    .url(url.trim().to_owned())
                    .build(),
            )
            .await?;
    }
    Ok(())
}

pub(super) async fn accept_update(
    tx: &DatabaseTransaction,
    update: Update,
    received_at: DateTimeUtc,
    polled: bool,
) -> anyhow::Result<()> {
    let now = normalize(received_at);
    let update_id = update.update_id;
    enqueue_inbox(
        tx,
        IngressSource::Telegram,
        Some(update_id.to_string()),
        serde_json::to_vec(&update)?,
        now,
    )
    .await?;
    schedule_tick(tx, now).await?;
    if polled {
        common::advance_poll_offset(tx, i64::from(update_id) + 1).await?;
    }
    Ok(())
}

pub(super) async fn listen(client: &Client, app: Arc<App>) -> anyhow::Result<()> {
    let mut shutdown = app.shutdown.subscribe();
    loop {
        if *shutdown.borrow() {
            return Ok(());
        }
        tokio::select! {
            _ = shutdown.changed() => return Ok(()),
            result = poll(client, &app) => if let Err(error) = result {
                tracing::error!(error = %common::safe_error(&error), "Telegram polling failed");
                tokio::select! {
                    _ = shutdown.changed() => return Ok(()),
                    _ = tokio::time::sleep(common::retry_delay(&error).to_std().unwrap_or_default()) => {},
                }
            }
        }
    }
}

async fn poll(client: &Client, app: &App) -> anyhow::Result<()> {
    let offset = common::poll_offset(&app.db).await?;
    let updates = client
        .bot
        .get_updates(
            &GetUpdatesParams::builder()
                .offset(offset)
                .limit(100)
                .timeout(30)
                .allowed_updates(vec![AllowedUpdate::Message])
                .build(),
        )
        .await?
        .result;
    for update in updates {
        tracing::info!(update_id = update.update_id, "new Telegram update");
        app.accept_update(update, Utc::now(), true).await?;
    }
    Ok(())
}
