use crate::{
    entity::settings,
    state::{DateTimeUtc, Event, IngressSource, Phase, ReminderMinutes, SettingsPosition, Signal},
    telegram::{common, reply, template},
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
    SafetyAlertTemplate,
    SafetyRecoveryTemplate,
    RenderedExamples,
    Guide,
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
    SafetyTemplatePrompt {
        source: String,
    },
    SafetyRecoveryTemplatePrompt {
        source: String,
    },
    SafetyTemplateExamples,
    SafetyRecoveryTemplateExamples,
    SafetyTemplateGuide,
    SafetyTemplateSubmitted {
        source: String,
    },
    SafetyRecoveryTemplateSubmitted {
        source: String,
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
    renderer: &template::Renderer,
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
            reply::main_menu(client, renderer, phase).await?;
            tracing::info!("Telegram start command handled");
        }
        Decision::Version => {
            common::set_position(tx, SettingsPosition::Main).await?;
            reply::version(client, renderer, phase).await?;
            tracing::info!("Telegram version command handled");
        }
        Decision::SettingsMenu => {
            common::set_position(tx, SettingsPosition::Settings).await?;
            reply::settings_menu(client, renderer).await?;
        }
        Decision::SettingsUnavailable => {
            common::set_position(tx, SettingsPosition::Main).await?;
            reply::settings_unavailable(client, renderer, phase).await?;
        }
        Decision::SettingPrompt { position, value } => {
            common::set_position(tx, position).await?;
            reply::setting_prompt(client, renderer, position, &value).await?;
        }
        Decision::SafetyTemplatePrompt { source } => {
            common::set_position(tx, SettingsPosition::SafetyAlertTemplate).await?;
            reply::safety_template_prompt(client, renderer, &source).await?;
        }
        Decision::SafetyRecoveryTemplatePrompt { source } => {
            common::set_position(tx, SettingsPosition::SafetyRecoveryTemplate).await?;
            reply::safety_recovery_template_prompt(client, renderer, &source).await?;
        }
        Decision::SafetyTemplateExamples => {
            reply::safety_template_examples(client, renderer).await?;
        }
        Decision::SafetyRecoveryTemplateExamples => {
            reply::safety_recovery_template_examples(client, renderer).await?;
        }
        Decision::SafetyTemplateGuide => {
            reply::guide(client, renderer).await?;
        }
        Decision::BackToSettings => {
            common::set_position(tx, SettingsPosition::Settings).await?;
            reply::settings_menu(client, renderer).await?;
        }
        Decision::BackToMain => {
            common::set_position(tx, SettingsPosition::Main).await?;
            reply::main_menu(client, renderer, phase).await?;
        }
        Decision::SettingUpdated { position, value } => {
            common::set_reminder_minutes(tx, position, value.clone()).await?;
            common::set_position(tx, SettingsPosition::Settings).await?;
            reply::setting_updated(client, renderer, position, &value).await?;
            tracing::info!(setting = setting_key(position), "Telegram setting updated");
        }
        Decision::SettingInvalid { position, current } => {
            reply::setting_invalid(client, renderer, position, &current).await?;
            tracing::info!(
                setting = setting_key(position),
                "invalid Telegram setting value"
            );
        }
        Decision::SafetyTemplateSubmitted { source } => {
            match template::validate_safety_template(renderer, &source) {
                Ok(()) => {
                    // Send both samples before committing the replacement.
                    // Ordinary transport failures roll back; a formatting or
                    // length rejection is reported as an invalid template.
                    match reply::safety_template_examples_for_source(client, renderer, &source)
                        .await
                    {
                        Ok(()) => {}
                        Err(error) if common::is_message_rejection(&error) => {
                            reply::safety_template_invalid(
                                client,
                                renderer,
                                &anyhow::anyhow!("template sample was rejected by Telegram"),
                            )
                            .await?;
                            tracing::info!(
                                reason = "template_sample_rejected",
                                "invalid safety alert template"
                            );
                            return Ok(None);
                        }
                        Err(error) => return Err(error),
                    }
                    common::set_safety_alert_template(tx, source.clone()).await?;
                    renderer.set_safety_alert_source(&source)?;
                    common::set_position(tx, SettingsPosition::Settings).await?;
                    reply::safety_template_updated(client, renderer).await?;
                    tracing::info!("Telegram safety alert template updated");
                }
                Err(error) => {
                    reply::safety_template_invalid(client, renderer, &error).await?;
                    tracing::info!(
                        reason = "template_validation_failed",
                        "invalid safety alert template"
                    );
                }
            }
        }
        Decision::SafetyRecoveryTemplateSubmitted { source } => {
            match template::validate_safety_recovery_template(renderer, &source) {
                Ok(()) => {
                    match reply::safety_recovery_template_examples_for_source(
                        client, renderer, &source,
                    )
                    .await
                    {
                        Ok(()) => {}
                        Err(error) if common::is_message_rejection(&error) => {
                            reply::safety_recovery_template_invalid(
                                client,
                                renderer,
                                &anyhow::anyhow!("template sample was rejected by Telegram"),
                            )
                            .await?;
                            tracing::info!(
                                reason = "recovery_template_sample_rejected",
                                "invalid safety recovery template"
                            );
                            return Ok(None);
                        }
                        Err(error) => return Err(error),
                    }
                    common::set_safety_recovery_template(tx, source.clone()).await?;
                    renderer.set_safety_recovery_source(&source)?;
                    common::set_position(tx, SettingsPosition::Settings).await?;
                    reply::safety_recovery_template_updated(client, renderer).await?;
                    tracing::info!("Telegram safety recovery template updated");
                }
                Err(error) => {
                    reply::safety_recovery_template_invalid(client, renderer, &error).await?;
                    tracing::info!(
                        reason = "recovery_template_validation_failed",
                        "invalid safety recovery template"
                    );
                }
            }
        }
        Decision::Event { signal, date, body } => {
            common::set_position(tx, SettingsPosition::Main).await?;
            return Ok(Some((
                signal,
                Event {
                    source: IngressSource::Telegram,
                    event_at: common::event_time(date, now),
                    received_at: now,
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
        Some(Command::SafetyAlertTemplate) => {
            if inactive(phase) {
                Ok(Decision::SafetyTemplatePrompt {
                    source: settings.safety_alert_template.clone(),
                })
            } else {
                Ok(Decision::SettingsUnavailable)
            }
        }
        Some(Command::SafetyRecoveryTemplate) => {
            if inactive(phase) {
                Ok(Decision::SafetyRecoveryTemplatePrompt {
                    source: settings.safety_recovery_template.clone(),
                })
            } else {
                Ok(Decision::SettingsUnavailable)
            }
        }
        Some(Command::RenderedExamples) => {
            if inactive(phase) {
                match position {
                    SettingsPosition::SafetyAlertTemplate => Ok(Decision::SafetyTemplateExamples),
                    SettingsPosition::SafetyRecoveryTemplate => {
                        Ok(Decision::SafetyRecoveryTemplateExamples)
                    }
                    _ => Ok(Decision::Ignore),
                }
            } else {
                Ok(Decision::Ignore)
            }
        }
        Some(Command::Guide) => {
            if inactive(phase)
                && matches!(
                    position,
                    SettingsPosition::SafetyAlertTemplate
                        | SettingsPosition::SafetyRecoveryTemplate
                )
            {
                Ok(Decision::SafetyTemplateGuide)
            } else {
                Ok(Decision::Ignore)
            }
        }
        Some(Command::Back) => {
            if !inactive(phase) {
                Ok(Decision::MainMenu)
            } else {
                match position {
                    SettingsPosition::OwnerReminderTimes
                    | SettingsPosition::SafetyReminderTimes
                    | SettingsPosition::SafetyAlertTemplate
                    | SettingsPosition::SafetyRecoveryTemplate => Ok(Decision::BackToSettings),
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
                SettingsPosition::SafetyAlertTemplate => Ok(Decision::SafetyTemplateSubmitted {
                    source: message.text.clone(),
                }),
                SettingsPosition::SafetyRecoveryTemplate => {
                    Ok(Decision::SafetyRecoveryTemplateSubmitted {
                        source: message.text.clone(),
                    })
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
        "Safety alert template" => return Some(Command::SafetyAlertTemplate),
        "Safety recovery template" => return Some(Command::SafetyRecoveryTemplate),
        "Rendered examples" => return Some(Command::RenderedExamples),
        "Guide" => return Some(Command::Guide),
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
        SettingsPosition::Main
        | SettingsPosition::Settings
        | SettingsPosition::SafetyAlertTemplate
        | SettingsPosition::SafetyRecoveryTemplate => None,
    }
}

pub(super) fn setting_key(position: SettingsPosition) -> &'static str {
    match position {
        SettingsPosition::OwnerReminderTimes => "owner_reminder_minutes",
        SettingsPosition::SafetyReminderTimes => "safety_reminder_minutes",
        SettingsPosition::SafetyAlertTemplate => "safety_alert_template",
        SettingsPosition::SafetyRecoveryTemplate => "safety_recovery_template",
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
            safety_alert_template: template::DEFAULT_SAFETY_ALERT_TEMPLATE.to_owned(),
            safety_recovery_template: template::DEFAULT_SAFETY_RECOVERY_TEMPLATE.to_owned(),
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
