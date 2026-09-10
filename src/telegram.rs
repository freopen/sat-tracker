use crate::{
    App, Config,
    entity::runtime,
    state::{Audience, Phase},
};
use chrono::{Duration as ChronoDuration, Utc};
use frankenstein::{
    AsyncTelegramApi,
    client_reqwest::Bot,
    methods::{DeleteWebhookParams, GetUpdatesParams, SendMessageParams, SetWebhookParams},
    types::{AllowedUpdate, KeyboardButton, ReplyKeyboardMarkup, ReplyMarkup},
};
use sea_orm::EntityTrait;
use std::{sync::Arc, time::Duration};

pub(crate) struct Telegram {
    bot: Bot,
    owner_chat_id: i64,
    safety_chat_id: i64,
}
impl Telegram {
    pub(crate) fn new(config: &Config) -> anyhow::Result<Self> {
        let client = frankenstein::reqwest::Client::builder()
            .retry(frankenstein::reqwest::retry::never())
            .timeout(Duration::from_secs(40))
            .connect_timeout(Duration::from_secs(10))
            .build()?;
        Ok(Self {
            bot: Bot::builder()
                .api_url(format!(
                    "{}/bot{}",
                    config.telegram_api_url.trim_end_matches('/'),
                    config.telegram_bot_token
                ))
                .client(client)
                .build(),
            owner_chat_id: config.owner_chat_id,
            safety_chat_id: config.safety_chat_id,
        })
    }
    pub(crate) async fn configure(&self, url: &str) -> anyhow::Result<()> {
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
    pub(crate) async fn send(&self, audience: Audience, text: &str) -> anyhow::Result<()> {
        let chat_id = match audience {
            Audience::Owner => self.owner_chat_id,
            Audience::Safety => self.safety_chat_id,
        };
        self.bot
            .send_message(
                &SendMessageParams::builder()
                    .chat_id(chat_id)
                    .text(text)
                    .build(),
            )
            .await?;
        Ok(())
    }

    pub(crate) async fn send_owner(
        &self,
        text: &str,
        phase: Phase,
        disable_notification: bool,
    ) -> anyhow::Result<()> {
        self.bot
            .send_message(
                &SendMessageParams::builder()
                    .chat_id(self.owner_chat_id)
                    .text(text)
                    .disable_notification(disable_notification)
                    .reply_markup(owner_keyboard(phase))
                    .build(),
            )
            .await?;
        Ok(())
    }
}

pub(crate) fn owner_keyboard(phase: Phase) -> ReplyMarkup {
    let texts: &[&str] = match phase {
        Phase::Active => &["OK", "FINISHED"],
        Phase::Idle | Phase::Finished => &["Start hike"],
    };
    let keyboard = texts
        .iter()
        .map(|text| KeyboardButton::builder().text(*text).build())
        .collect();
    ReplyMarkup::ReplyKeyboardMarkup(
        ReplyKeyboardMarkup::builder()
            .keyboard(vec![keyboard])
            .is_persistent(true)
            .resize_keyboard(true)
            .build(),
    )
}

pub(crate) async fn listen(app: Arc<App>) -> anyhow::Result<()> {
    let mut shutdown = app.shutdown.subscribe();
    loop {
        if *shutdown.borrow() {
            return Ok(());
        }
        tokio::select! {
            _ = shutdown.changed() => return Ok(()),
            result = poll(&app) => if let Err(error) = result {
                tracing::error!(error = %safe_error(&error), "Telegram polling failed");
                tokio::select! {
                    _ = shutdown.changed() => return Ok(()),
                    _ = tokio::time::sleep(retry_delay(&error).to_std().unwrap_or_default()) => {},
                }
            }
        }
    }
}
async fn poll(app: &App) -> anyhow::Result<()> {
    let offset = runtime::Entity::find_by_id(1)
        .one(&app.db)
        .await?
        .ok_or_else(|| anyhow::anyhow!("missing runtime"))?
        .telegram_poll_offset;
    let updates = app
        .telegram
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
pub(crate) fn retry_delay(error: &anyhow::Error) -> ChronoDuration {
    let seconds = match error.downcast_ref::<frankenstein::Error>() {
        Some(frankenstein::Error::Api(response)) => response
            .parameters
            .and_then(|p| p.retry_after)
            .map(u64::from)
            .unwrap_or(0),
        _ => 0,
    };
    ChronoDuration::seconds(i64::try_from(seconds.max(5)).unwrap_or(i64::MAX))
}
// Telegram errors can contain the token-bearing URL or raw response bodies.
pub(crate) fn safe_error(error: &anyhow::Error) -> String {
    match error.downcast_ref::<frankenstein::Error>() {
        Some(frankenstein::Error::Api(response)) => {
            format!("Telegram API error {}", response.error_code)
        }
        Some(_) => "Telegram request failed".to_owned(),
        None => error.to_string(),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Command {
    Start,
    StartHike,
    Ok,
    Finished,
    Version,
}
pub(crate) fn command(text: &str) -> Option<Command> {
    match text.trim() {
        "Start hike" => return Some(Command::StartHike),
        "OK" => return Some(Command::Ok),
        "FINISHED" => return Some(Command::Finished),
        _ => {}
    }
    let first = text.split_whitespace().next()?;
    let name = match first.split_once('@') {
        Some((name, suffix)) if !suffix.is_empty() => name,
        Some(_) => return None,
        None => first,
    };
    match name {
        "/start" => Some(Command::Start),
        "/version" => Some(Command::Version),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owner_commands_use_friendly_labels_and_keep_version() {
        assert_eq!(command("/start"), Some(Command::Start));
        assert_eq!(command("/start@tracker"), Some(Command::Start));
        assert_eq!(command("Start hike"), Some(Command::StartHike));
        assert_eq!(command(" OK "), Some(Command::Ok));
        assert_eq!(command("FINISHED"), Some(Command::Finished));
        assert_eq!(command("/version"), Some(Command::Version));
        assert_eq!(command("/ok"), None);
        assert_eq!(command("/finished"), None);
    }

    #[test]
    fn owner_keyboard_serializes_phase_specific_one_time_buttons() {
        let inactive = serde_json::to_value(owner_keyboard(Phase::Finished)).unwrap();
        assert_eq!(
            inactive["keyboard"],
            serde_json::json!([[{"text": "Start hike"}]])
        );
        assert_eq!(inactive["is_persistent"], true);
        assert_eq!(inactive["resize_keyboard"], true);

        let active = serde_json::to_value(owner_keyboard(Phase::Active)).unwrap();
        assert_eq!(
            active["keyboard"],
            serde_json::json!([[{"text": "OK"}, {"text": "FINISHED"}]])
        );
    }

    #[test]
    fn telegram_retry_after_is_a_floor_and_errors_do_not_leak_payloads() {
        let error: anyhow::Error =
            frankenstein::Error::Api(frankenstein::response::ErrorResponse {
                ok: false,
                description: "private message".to_owned(),
                error_code: 429,
                parameters: Some(frankenstein::response::ResponseParameters {
                    migrate_to_chat_id: None,
                    retry_after: Some(12),
                }),
            })
            .into();
        assert_eq!(retry_delay(&error), ChronoDuration::seconds(12));
        assert_eq!(safe_error(&error), "Telegram API error 429");
        assert_eq!(
            retry_delay(&anyhow::anyhow!("database unavailable")),
            ChronoDuration::seconds(5)
        );
    }
}
