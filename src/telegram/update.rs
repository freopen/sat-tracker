use crate::{
    entity::settings,
    state::{DateTimeUtc, Event, Phase, ReminderMinutes, SettingsPosition, Signal},
    telegram::{common, reply},
};
use anyhow::Context;
use frankenstein::updates::{Update, UpdateContent};
use sea_orm::DatabaseTransaction;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Command {
    Start,
    StartHike,
    Ok,
    Finished,
    Version,
    Settings,
    OwnerReminderTimes,
    SafetyReminderTimes,
    Back,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Message {
    date: u64,
    text: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Decision {
    Ignore,
    IgnoreActive,
    MainMenu,
    Version,
    SettingsMenu,
    SettingsUnavailable,
    SettingPrompt {
        position: SettingsPosition,
        value: ReminderMinutes,
    },
    BackToSettings,
    BackToMain,
    SettingUpdated {
        position: SettingsPosition,
        value: ReminderMinutes,
    },
    SettingInvalid {
        position: SettingsPosition,
        current: ReminderMinutes,
    },
    Event {
        signal: Signal,
        date: u64,
        body: String,
    },
}

pub(super) async fn handle_update(
    client: &common::Client,
    tx: &DatabaseTransaction,
    payload: &[u8],
    now: DateTimeUtc,
    phase: Phase,
) -> anyhow::Result<Option<(Signal, Event)>> {
    let Some(message) = parse_update(payload, client.owner_chat_id)? else {
        return Ok(None);
    };
    let runtime = common::runtime(tx).await?;
    let settings = common::settings(tx).await?;
    let decision = decide(&message, phase, runtime.settings_position, &settings)?;
    match decision {
        Decision::Ignore => tracing::info!("ignored unknown Telegram command"),
        Decision::IgnoreActive => {
            common::set_position(tx, SettingsPosition::Main).await?;
            tracing::info!("ignored Telegram message while hike is active");
        }
        Decision::MainMenu => {
            common::set_position(tx, SettingsPosition::Main).await?;
            reply::main_menu(client, phase).await?;
            tracing::info!("Telegram start command handled");
        }
        Decision::Version => {
            common::set_position(tx, SettingsPosition::Main).await?;
            reply::version(client, phase).await?;
            tracing::info!("Telegram version command handled");
        }
        Decision::SettingsMenu => {
            common::set_position(tx, SettingsPosition::Settings).await?;
            reply::settings_menu(client).await?;
        }
        Decision::SettingsUnavailable => {
            common::set_position(tx, SettingsPosition::Main).await?;
            reply::settings_unavailable(client, phase).await?;
        }
        Decision::SettingPrompt { position, value } => {
            common::set_position(tx, position).await?;
            reply::setting_prompt(client, position, &value).await?;
        }
        Decision::BackToSettings => {
            common::set_position(tx, SettingsPosition::Settings).await?;
            reply::settings_menu(client).await?;
        }
        Decision::BackToMain => {
            common::set_position(tx, SettingsPosition::Main).await?;
            reply::main_menu(client, phase).await?;
        }
        Decision::SettingUpdated { position, value } => {
            common::set_reminder_minutes(tx, position, value.clone()).await?;
            common::set_position(tx, SettingsPosition::Settings).await?;
            reply::setting_updated(client, position, &value).await?;
            tracing::info!(setting = setting_key(position), "Telegram setting updated");
        }
        Decision::SettingInvalid { position, current } => {
            reply::setting_invalid(client, position, &current).await?;
            tracing::info!(
                setting = setting_key(position),
                "invalid Telegram setting value"
            );
        }
        Decision::Event { signal, date, body } => {
            common::set_position(tx, SettingsPosition::Main).await?;
            return Ok(Some((
                signal,
                Event {
                    event_at: common::event_time(date, now),
                    body,
                    location: None,
                },
            )));
        }
    }
    Ok(None)
}

fn parse_update(payload: &[u8], owner_chat_id: i64) -> anyhow::Result<Option<Message>> {
    let update: Update = serde_json::from_slice(payload)?;
    let UpdateContent::Message(message) = update.content else {
        tracing::info!("ignored Telegram update without a message");
        return Ok(None);
    };
    if message.chat.id != owner_chat_id {
        tracing::info!("ignored Telegram message from a non-owner chat");
        return Ok(None);
    }
    let Some(text) = message.text else {
        tracing::info!("ignored Telegram message without text");
        return Ok(None);
    };
    Ok(Some(Message {
        date: message.date,
        text,
    }))
}

fn decide(
    message: &Message,
    phase: Phase,
    position: SettingsPosition,
    settings: &settings::Model,
) -> anyhow::Result<Decision> {
    let command = parse_command(&message.text);
    match command {
        Some(Command::Start) => Ok(Decision::MainMenu),
        Some(Command::Version) => Ok(Decision::Version),
        Some(Command::Settings) => {
            if inactive(phase) {
                Ok(Decision::SettingsMenu)
            } else {
                Ok(Decision::SettingsUnavailable)
            }
        }
        Some(Command::OwnerReminderTimes) | Some(Command::SafetyReminderTimes) => {
            if !inactive(phase) {
                return Ok(Decision::SettingsUnavailable);
            }
            let position = match command {
                Some(Command::OwnerReminderTimes) => SettingsPosition::OwnerReminderTimes,
                Some(Command::SafetyReminderTimes) => SettingsPosition::SafetyReminderTimes,
                _ => unreachable!(),
            };
            let value = setting_value(settings, position)
                .context("settings position does not select a reminder schedule")?
                .clone();
            Ok(Decision::SettingPrompt { position, value })
        }
        Some(Command::Back) => {
            if !inactive(phase) {
                Ok(Decision::MainMenu)
            } else {
                match position {
                    SettingsPosition::OwnerReminderTimes
                    | SettingsPosition::SafetyReminderTimes => Ok(Decision::BackToSettings),
                    SettingsPosition::Settings | SettingsPosition::Main => Ok(Decision::BackToMain),
                }
            }
        }
        Some(Command::StartHike) | Some(Command::Ok) | Some(Command::Finished) => {
            let signal = match command {
                Some(Command::StartHike) | Some(Command::Ok) => Signal::Ok,
                Some(Command::Finished) => Signal::Finished,
                _ => unreachable!(),
            };
            Ok(Decision::Event {
                signal,
                date: message.date,
                body: message.text.clone(),
            })
        }
        None => {
            if !inactive(phase) {
                return Ok(Decision::IgnoreActive);
            }
            match position {
                SettingsPosition::OwnerReminderTimes | SettingsPosition::SafetyReminderTimes => {
                    let current = setting_value(settings, position)
                        .context("settings position does not select a reminder schedule")?
                        .clone();
                    match ReminderMinutes::parse(&message.text) {
                        Ok(value) => Ok(Decision::SettingUpdated { position, value }),
                        Err(_) => Ok(Decision::SettingInvalid { position, current }),
                    }
                }
                SettingsPosition::Main | SettingsPosition::Settings => Ok(Decision::Ignore),
            }
        }
    }
}

fn inactive(phase: Phase) -> bool {
    matches!(phase, Phase::Idle | Phase::Finished)
}

pub(super) fn parse_command(text: &str) -> Option<Command> {
    match text.trim() {
        "Start hike" => return Some(Command::StartHike),
        "OK" => return Some(Command::Ok),
        "FINISHED" => return Some(Command::Finished),
        "Settings" => return Some(Command::Settings),
        "Owner reminder times" => return Some(Command::OwnerReminderTimes),
        "Safety reminder times" => return Some(Command::SafetyReminderTimes),
        "Back" => return Some(Command::Back),
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

fn setting_value(
    settings: &settings::Model,
    position: SettingsPosition,
) -> Option<&ReminderMinutes> {
    match position {
        SettingsPosition::OwnerReminderTimes => Some(&settings.owner_reminder_minutes),
        SettingsPosition::SafetyReminderTimes => Some(&settings.safety_reminder_minutes),
        SettingsPosition::Main | SettingsPosition::Settings => None,
    }
}

pub(super) fn setting_key(position: SettingsPosition) -> &'static str {
    match position {
        SettingsPosition::OwnerReminderTimes => "owner_reminder_minutes",
        SettingsPosition::SafetyReminderTimes => "safety_reminder_minutes",
        SettingsPosition::Main | SettingsPosition::Settings => "none",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings() -> settings::Model {
        settings::Model {
            id: 1,
            owner_reminder_minutes: ReminderMinutes(vec![30]),
            safety_reminder_minutes: ReminderMinutes(vec![60]),
        }
    }

    #[test]
    fn owner_commands_use_friendly_labels_and_keep_version() {
        assert_eq!(parse_command("/start"), Some(Command::Start));
        assert_eq!(parse_command("/start@tracker"), Some(Command::Start));
        assert_eq!(parse_command("Start hike"), Some(Command::StartHike));
        assert_eq!(parse_command(" OK "), Some(Command::Ok));
        assert_eq!(parse_command("FINISHED"), Some(Command::Finished));
        assert_eq!(parse_command("Settings"), Some(Command::Settings));
        assert_eq!(
            parse_command("Owner reminder times"),
            Some(Command::OwnerReminderTimes)
        );
        assert_eq!(
            parse_command("Safety reminder times"),
            Some(Command::SafetyReminderTimes)
        );
        assert_eq!(parse_command("Back"), Some(Command::Back));
        assert_eq!(parse_command("/version"), Some(Command::Version));
        assert_eq!(parse_command("/ok"), None);
        assert_eq!(parse_command("/finished"), None);
    }

    #[test]
    fn parse_update_filters_non_owner_and_non_text_messages() {
        let payload = |chat: i64, text: Option<&str>| {
            serde_json::to_vec(&serde_json::json!({
                "update_id": 1,
                "message": {
                    "message_id": 1,
                    "date": 1_700_000_000,
                    "chat": {"id": chat, "type": "private"},
                    "text": text,
                }
            }))
            .unwrap()
        };

        assert!(
            parse_update(&payload(99, Some("OK")), 10)
                .unwrap()
                .is_none()
        );
        assert!(parse_update(&payload(10, None), 10).unwrap().is_none());
        assert_eq!(
            parse_update(&payload(10, Some("OK")), 10).unwrap(),
            Some(Message {
                date: 1_700_000_000,
                text: "OK".to_owned(),
            })
        );
    }

    #[test]
    fn decisions_route_phase_specific_commands_and_settings() {
        let settings = settings();
        let message = |text: &str| Message {
            date: 1_700_000_000,
            text: text.to_owned(),
        };

        assert_eq!(
            decide(
                &message("Settings"),
                Phase::Idle,
                SettingsPosition::Main,
                &settings
            )
            .unwrap(),
            Decision::SettingsMenu
        );
        assert_eq!(
            decide(
                &message("Settings"),
                Phase::Active,
                SettingsPosition::Main,
                &settings
            )
            .unwrap(),
            Decision::SettingsUnavailable
        );
        assert_eq!(
            decide(
                &message("45, 60"),
                Phase::Finished,
                SettingsPosition::OwnerReminderTimes,
                &settings
            )
            .unwrap(),
            Decision::SettingUpdated {
                position: SettingsPosition::OwnerReminderTimes,
                value: ReminderMinutes(vec![45, 60]),
            }
        );
        assert_eq!(
            decide(
                &message("15, 30"),
                Phase::Idle,
                SettingsPosition::SafetyReminderTimes,
                &settings
            )
            .unwrap(),
            Decision::SettingUpdated {
                position: SettingsPosition::SafetyReminderTimes,
                value: ReminderMinutes(vec![15, 30]),
            }
        );
        assert!(matches!(
            decide(
                &message("60, 45"),
                Phase::Finished,
                SettingsPosition::OwnerReminderTimes,
                &settings
            )
            .unwrap(),
            Decision::SettingInvalid {
                position: SettingsPosition::OwnerReminderTimes,
                ..
            }
        ));
        assert_eq!(
            decide(
                &message("unknown"),
                Phase::Active,
                SettingsPosition::OwnerReminderTimes,
                &settings
            )
            .unwrap(),
            Decision::IgnoreActive
        );
    }

    #[test]
    fn decisions_emit_shared_events_and_handle_back_navigation() {
        let settings = settings();
        let message = |text: &str| Message {
            date: 1_700_000_000,
            text: text.to_owned(),
        };

        assert_eq!(
            decide(
                &message("OK"),
                Phase::Active,
                SettingsPosition::Main,
                &settings
            )
            .unwrap(),
            Decision::Event {
                signal: Signal::Ok,
                date: 1_700_000_000,
                body: "OK".to_owned(),
            }
        );
        assert_eq!(
            decide(
                &message("FINISHED"),
                Phase::Active,
                SettingsPosition::Main,
                &settings
            )
            .unwrap(),
            Decision::Event {
                signal: Signal::Finished,
                date: 1_700_000_000,
                body: "FINISHED".to_owned(),
            }
        );
        assert_eq!(
            decide(
                &message("Back"),
                Phase::Finished,
                SettingsPosition::OwnerReminderTimes,
                &settings
            )
            .unwrap(),
            Decision::BackToSettings
        );
        assert_eq!(
            decide(
                &message("Back"),
                Phase::Idle,
                SettingsPosition::SafetyReminderTimes,
                &settings
            )
            .unwrap(),
            Decision::BackToSettings
        );
        assert_eq!(
            decide(
                &message("Back"),
                Phase::Finished,
                SettingsPosition::Settings,
                &settings
            )
            .unwrap(),
            Decision::BackToMain
        );
    }
}
