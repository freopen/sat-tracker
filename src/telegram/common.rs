use crate::{
    entity::{runtime, settings},
    state::{DateTimeUtc, Phase, ReminderMinutes, SettingsPosition, normalize},
};
use anyhow::Context;
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use frankenstein::client_reqwest::Bot;
use sea_orm::{ActiveModelTrait, DatabaseConnection, DatabaseTransaction, EntityTrait, Set};

pub(super) struct Client {
    pub(super) bot: Bot,
    pub(super) owner_chat_id: i64,
    pub(super) safety_chat_id: i64,
}

pub(super) async fn runtime(tx: &DatabaseTransaction) -> anyhow::Result<runtime::Model> {
    runtime::Entity::find_by_id(1)
        .one(tx)
        .await?
        .context("missing runtime")
}

pub(super) async fn settings(tx: &DatabaseTransaction) -> anyhow::Result<settings::Model> {
    settings::Entity::find_by_id(1)
        .one(tx)
        .await?
        .context("missing settings")
}

pub(super) async fn set_position(
    tx: &DatabaseTransaction,
    position: SettingsPosition,
) -> anyhow::Result<()> {
    runtime::ActiveModel {
        id: Set(1),
        settings_position: Set(position),
        ..Default::default()
    }
    .update(tx)
    .await?;
    Ok(())
}

pub(super) async fn set_reminder_minutes(
    tx: &DatabaseTransaction,
    position: SettingsPosition,
    value: ReminderMinutes,
) -> anyhow::Result<()> {
    let mut update = settings::ActiveModel {
        id: Set(1),
        ..Default::default()
    };
    match position {
        SettingsPosition::OwnerReminderTimes => update.owner_reminder_minutes = Set(value),
        SettingsPosition::SafetyReminderTimes => update.safety_reminder_minutes = Set(value),
        SettingsPosition::Main | SettingsPosition::Settings => {
            unreachable!("settings position does not select a reminder schedule")
        }
        SettingsPosition::SafetyAlertTemplate => {
            unreachable!("settings position does not select a reminder schedule")
        }
        SettingsPosition::SafetyRecoveryTemplate => {
            unreachable!("settings position does not select a reminder schedule")
        }
    }
    update.update(tx).await?;
    Ok(())
}

pub(super) async fn set_safety_alert_template(
    tx: &DatabaseTransaction,
    value: String,
) -> anyhow::Result<()> {
    settings::ActiveModel {
        id: Set(1),
        safety_alert_template: Set(value),
        ..Default::default()
    }
    .update(tx)
    .await?;
    Ok(())
}

pub(super) async fn set_safety_recovery_template(
    tx: &DatabaseTransaction,
    value: String,
) -> anyhow::Result<()> {
    settings::ActiveModel {
        id: Set(1),
        safety_recovery_template: Set(value),
        ..Default::default()
    }
    .update(tx)
    .await?;
    Ok(())
}

pub(super) async fn sync_phase(tx: &DatabaseTransaction, phase: Phase) -> anyhow::Result<()> {
    if phase == Phase::Active {
        set_position(tx, SettingsPosition::Main).await?;
    }
    Ok(())
}

pub(super) async fn advance_poll_offset(
    tx: &DatabaseTransaction,
    offset: i64,
) -> anyhow::Result<()> {
    let current = runtime(tx).await?;
    if current.telegram_poll_offset >= offset {
        return Ok(());
    }
    runtime::ActiveModel {
        id: Set(1),
        telegram_poll_offset: Set(offset),
        ..Default::default()
    }
    .update(tx)
    .await?;
    Ok(())
}

pub(super) async fn poll_offset(db: &DatabaseConnection) -> anyhow::Result<i64> {
    Ok(runtime::Entity::find_by_id(1)
        .one(db)
        .await?
        .context("missing runtime")?
        .telegram_poll_offset)
}

pub(super) fn event_time(message_date: u64, fallback: DateTimeUtc) -> DateTimeUtc {
    i64::try_from(message_date)
        .ok()
        .and_then(|seconds| DateTime::<Utc>::from_timestamp(seconds, 0))
        .map(normalize)
        .unwrap_or(fallback)
}

pub(crate) fn retry_delay(error: &anyhow::Error) -> ChronoDuration {
    let seconds = match error.downcast_ref::<frankenstein::Error>() {
        Some(frankenstein::Error::Api(response)) => response
            .parameters
            .and_then(|parameters| parameters.retry_after)
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

/// Telegram reports malformed rich Markdown and overlong message bodies as
/// client errors. Those are caused by the configured alert template and can be
/// handled with the built-in alert; permission, rate-limit, and other API
/// errors must retain the normal transaction/retry path.
pub(crate) fn is_message_rejection(error: &anyhow::Error) -> bool {
    let Some(frankenstein::Error::Api(response)) = error.downcast_ref::<frankenstein::Error>()
    else {
        return false;
    };
    if response.error_code != 400 {
        return false;
    }
    let description = response.description.to_ascii_lowercase();
    description.contains("can't parse entities")
        || description.contains("can't parse rich message")
        || description.contains("message is too long")
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let malformed: anyhow::Error =
            frankenstein::Error::Api(frankenstein::response::ErrorResponse {
                ok: false,
                description: "Bad Request: can't parse entities".to_owned(),
                error_code: 400,
                parameters: None,
            })
            .into();
        assert!(is_message_rejection(&malformed));
        assert!(!is_message_rejection(&anyhow::anyhow!(
            "database unavailable"
        )));
    }
}
