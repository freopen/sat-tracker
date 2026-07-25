use durable_actions::HandlerError;
use frankenstein::{AsyncTelegramApi, client_reqwest::Bot, methods::SendMessageParams};

use crate::{config::Config, state::Audience};

const TELEGRAM_CHUNK: usize = 4000;

#[derive(Clone)]
pub(crate) struct Telegram {
    bot: Bot,
    owner_chat_id: i64,
    safety_chat_id: i64,
}

impl Telegram {
    pub(crate) fn new(config: &Config) -> anyhow::Result<Self> {
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
        Ok(Self {
            bot: Bot::builder().api_url(api_url).client(client).build(),
            owner_chat_id: config.owner_chat_id,
            safety_chat_id: config.safety_chat_id,
        })
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
