use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use durable_actions::{Handle, HandlerError};
use frankenstein::{
    AsyncTelegramApi,
    client_reqwest::Bot,
    methods::{DeleteWebhookParams, GetUpdatesParams, SendMessageParams, SetWebhookParams},
    types::AllowedUpdate,
};
use tokio::task::AbortHandle;
use tracing::error;

use crate::{actions::ProcessTelegram, config::Config, state::Audience};

const TELEGRAM_CHUNK: usize = 4000;
const LONG_POLL_TIMEOUT: u32 = 30;
const RETRY_DELAY: Duration = Duration::from_secs(1);

#[derive(Clone)]
pub(crate) struct Telegram {
    bot: Bot,
    owner_chat_id: i64,
    safety_chat_id: i64,
    polling_enabled: bool,
    listener: Arc<Mutex<Option<AbortHandle>>>,
}

impl Telegram {
    pub(crate) async fn new(config: &Config) -> anyhow::Result<Self> {
        let api_url = format!(
            "{}/bot{}",
            config.telegram_api_url.trim_end_matches('/'),
            config.telegram_bot_token
        );
        let host = frankenstein::reqwest::Url::parse(&api_url)?
            .host_str()
            .ok_or_else(|| anyhow::anyhow!("TELEGRAM_API_URL has no host"))?
            .to_owned();
        let retry = frankenstein::reqwest::retry::for_host(host)
            .max_retries_per_request(3)
            .classify_fn(|request| {
                if request.error().is_some()
                    || request.status().is_some_and(|status| {
                        status == frankenstein::reqwest::StatusCode::TOO_MANY_REQUESTS
                            || status.is_server_error()
                    })
                {
                    request.retryable()
                } else {
                    request.success()
                }
            });
        let client = frankenstein::reqwest::Client::builder()
            .retry(retry)
            .build()?;
        let telegram = Self {
            bot: Bot::builder().api_url(api_url).client(client).build(),
            owner_chat_id: config.owner_chat_id,
            safety_chat_id: config.safety_chat_id,
            polling_enabled: config.telegram_webhook_url.trim().is_empty(),
            listener: Arc::new(Mutex::new(None)),
        };
        telegram
            .configure_webhook(&config.telegram_webhook_url)
            .await?;
        Ok(telegram)
    }

    pub(crate) async fn send(&self, audience: Audience, text: &str) -> Result<(), HandlerError> {
        let chat_id = match audience {
            Audience::Owner => self.owner_chat_id,
            Audience::Safety => self.safety_chat_id,
        };
        for chunk in split(text) {
            self.bot
                .send_message(
                    &SendMessageParams::builder()
                        .chat_id(chat_id)
                        .text(chunk)
                        .build(),
                )
                .await
                .map_err(|error| Box::new(error) as HandlerError)?;
        }
        Ok(())
    }

    async fn configure_webhook(&self, url: &str) -> anyhow::Result<()> {
        if url.trim().is_empty() {
            self.bot
                .delete_webhook(&DeleteWebhookParams::builder().build())
                .await?;
        } else {
            self.bot
                .set_webhook(
                    &SetWebhookParams::builder()
                        .url(url.trim().to_owned())
                        .build(),
                )
                .await?;
        }
        Ok(())
    }

    pub(crate) fn start_listener(&self, handle: Handle) {
        if !self.polling_enabled {
            return;
        }
        let telegram = self.clone();
        let listener = tokio::spawn(async move { telegram.listen(handle).await });
        let abort_handle = listener.abort_handle();
        drop(listener);
        self.listener.lock().unwrap().replace(abort_handle);
    }

    pub(crate) fn abort_listener(&self) {
        if let Some(listener) = self.listener.lock().unwrap().take() {
            listener.abort();
        }
    }

    async fn listen(&self, handle: Handle) {
        let mut offset = 0_i64;
        loop {
            let params = GetUpdatesParams::builder()
                .offset(offset)
                .limit(100)
                .timeout(LONG_POLL_TIMEOUT)
                .allowed_updates(vec![AllowedUpdate::Message])
                .build();
            let updates = match self.bot.get_updates(&params).await {
                Ok(response) => response.result,
                Err(error) => {
                    error!(%error, "telegram long polling failed");
                    tokio::time::sleep(RETRY_DELAY).await;
                    continue;
                }
            };

            for update in updates {
                let update_id = update.update_id;
                if let Err(error) = handle.enqueue::<ProcessTelegram>(&update).await {
                    error!(%error, update_id, "failed to durably enqueue Telegram update");
                    tokio::time::sleep(RETRY_DELAY).await;
                    break;
                }
                offset = i64::from(update_id) + 1;
            }
        }
    }
}

pub(crate) fn split(text: &str) -> Vec<String> {
    if text.is_empty() {
        return vec!["(empty message body)".to_owned()];
    }
    let mut chunks = Vec::new();
    let mut rest = text;
    while rest.chars().count() > TELEGRAM_CHUNK {
        let byte = rest
            .char_indices()
            .nth(TELEGRAM_CHUNK)
            .map(|(index, _)| index)
            .unwrap_or(rest.len());
        let preferred = rest[..byte]
            .rfind('\n')
            .filter(|index| *index > byte / 2)
            .unwrap_or(byte);
        chunks.push(rest[..preferred].to_owned());
        rest = rest[preferred..].trim_start_matches('\n');
    }
    if !rest.is_empty() {
        chunks.push(rest.to_owned());
    }
    chunks
}
