use crate::{
    entity::{settings, tracker},
    state::{Audience, DateTimeUtc, Event, Phase, ReminderMinutes, SettingsPosition},
    telegram::{
        common::{self, Client},
        template,
    },
};
use frankenstein::{
    AsyncTelegramApi,
    methods::SendRichMessageParams,
    rich_message::InputRichMessage,
    types::{KeyboardButton, ReplyKeyboardMarkup, ReplyMarkup},
};
use serde::Serialize;
use serde_json::json;

pub(super) async fn send(client: &Client, audience: Audience, text: &str) -> anyhow::Result<()> {
    let chat_id = match audience {
        Audience::Owner => client.owner_chat_id,
        Audience::Safety => client.safety_chat_id,
    };
    client
        .bot
        .send_rich_message(
            &SendRichMessageParams::builder()
                .chat_id(chat_id)
                .rich_message(InputRichMessage::builder().markdown(text).build())
                .build(),
        )
        .await?;
    Ok(())
}

async fn send_owner(
    client: &Client,
    text: &str,
    phase: Phase,
    disable_notification: bool,
) -> anyhow::Result<()> {
    send_owner_with_keyboard(client, text, owner_keyboard(phase), disable_notification).await
}

async fn send_owner_with_keyboard(
    client: &Client,
    text: &str,
    keyboard: ReplyMarkup,
    disable_notification: bool,
) -> anyhow::Result<()> {
    client
        .bot
        .send_rich_message(
            &SendRichMessageParams::builder()
                .chat_id(client.owner_chat_id)
                .rich_message(InputRichMessage::builder().markdown(text).build())
                .disable_notification(disable_notification)
                .reply_markup(keyboard)
                .build(),
        )
        .await?;
    Ok(())
}

async fn send_owner_template<S: Serialize>(
    client: &Client,
    renderer: &template::Renderer,
    id: template::TemplateId,
    context: &S,
    phase: Phase,
    disable_notification: bool,
) -> anyhow::Result<()> {
    let text = renderer.render(id, context)?;
    send_owner(client, &text, phase, disable_notification).await
}

pub(super) async fn main_menu(
    client: &Client,
    renderer: &template::Renderer,
    phase: Phase,
) -> anyhow::Result<()> {
    send_owner_template(
        client,
        renderer,
        template::TemplateId::OwnerMainMenu,
        &json!({}),
        phase,
        false,
    )
    .await
}

pub(super) async fn version(
    client: &Client,
    renderer: &template::Renderer,
    phase: Phase,
) -> anyhow::Result<()> {
    let info = crate::build_info();
    send_owner_template(
        client,
        renderer,
        template::TemplateId::OwnerVersion,
        &json!({
            "version": info.version,
            "build_time": info.build_time,
            "git_commit": info.git_commit,
            "git_dirty": info.git_dirty,
        }),
        phase,
        false,
    )
    .await
}

pub(super) async fn settings_menu(
    client: &Client,
    renderer: &template::Renderer,
) -> anyhow::Result<()> {
    let text = renderer.render(template::TemplateId::OwnerSettingsMenu, &json!({}))?;
    send_owner_with_keyboard(client, &text, settings_keyboard(), false).await
}

pub(super) async fn settings_unavailable(
    client: &Client,
    renderer: &template::Renderer,
    phase: Phase,
) -> anyhow::Result<()> {
    send_owner_template(
        client,
        renderer,
        template::TemplateId::OwnerSettingsUnavailable,
        &json!({}),
        phase,
        false,
    )
    .await
}

pub(super) async fn setting_prompt(
    client: &Client,
    renderer: &template::Renderer,
    position: SettingsPosition,
    value: &ReminderMinutes,
) -> anyhow::Result<()> {
    let text = renderer.render(
        template::TemplateId::SettingPrompt,
        &json!({"label": setting_label(position), "value": value.to_string()}),
    )?;
    send_owner_with_keyboard(client, &text, setting_prompt_keyboard(), false).await
}

pub(super) async fn setting_updated(
    client: &Client,
    renderer: &template::Renderer,
    position: SettingsPosition,
    value: &ReminderMinutes,
) -> anyhow::Result<()> {
    let text = renderer.render(
        template::TemplateId::SettingUpdated,
        &json!({"label": setting_label(position), "value": value.to_string()}),
    )?;
    send_owner_with_keyboard(client, &text, settings_keyboard(), false).await
}

pub(super) async fn setting_invalid(
    client: &Client,
    renderer: &template::Renderer,
    position: SettingsPosition,
    current: &ReminderMinutes,
) -> anyhow::Result<()> {
    let text = renderer.render(
        template::TemplateId::SettingInvalid,
        &json!({"label": setting_label(position), "value": current.to_string()}),
    )?;
    send_owner_with_keyboard(client, &text, setting_prompt_keyboard(), false).await
}

pub(super) async fn safety_template_prompt(
    client: &Client,
    renderer: &template::Renderer,
    source: &str,
) -> anyhow::Result<()> {
    safety_template_prompt_for(client, renderer, "Safety alert template", source).await
}

pub(super) async fn safety_recovery_template_prompt(
    client: &Client,
    renderer: &template::Renderer,
    source: &str,
) -> anyhow::Result<()> {
    safety_template_prompt_for(client, renderer, "Safety recovery template", source).await
}

async fn safety_template_prompt_for(
    client: &Client,
    renderer: &template::Renderer,
    label: &str,
    source: &str,
) -> anyhow::Result<()> {
    let text = renderer.render(
        template::TemplateId::SafetyTemplatePrompt,
        &json!({"label": label, "source": source}),
    )?;
    send_owner_with_keyboard(client, &text, safety_template_keyboard(), false).await
}

pub(super) async fn safety_template_invalid(
    client: &Client,
    renderer: &template::Renderer,
    error: &anyhow::Error,
) -> anyhow::Result<()> {
    safety_template_invalid_for(client, renderer, "Safety alert template", error).await
}

pub(super) async fn safety_recovery_template_invalid(
    client: &Client,
    renderer: &template::Renderer,
    error: &anyhow::Error,
) -> anyhow::Result<()> {
    safety_template_invalid_for(client, renderer, "Safety recovery template", error).await
}

async fn safety_template_invalid_for(
    client: &Client,
    renderer: &template::Renderer,
    label: &str,
    error: &anyhow::Error,
) -> anyhow::Result<()> {
    let text = renderer.render(
        template::TemplateId::SafetyTemplateInvalid,
        &json!({"label": label, "error": concise_error(error)}),
    )?;
    send_owner_with_keyboard(client, &text, safety_template_keyboard(), false).await
}

pub(super) async fn safety_template_updated(
    client: &Client,
    renderer: &template::Renderer,
) -> anyhow::Result<()> {
    safety_template_updated_for(client, renderer, "Safety alert template").await
}

async fn safety_template_updated_for(
    client: &Client,
    renderer: &template::Renderer,
    label: &str,
) -> anyhow::Result<()> {
    let text = renderer.render(
        template::TemplateId::SafetyTemplateUpdated,
        &json!({"label": label}),
    )?;
    send_owner_with_keyboard(client, &text, settings_keyboard(), false).await
}

pub(super) async fn safety_template_examples(
    client: &Client,
    renderer: &template::Renderer,
) -> anyhow::Result<()> {
    let (mail, overdue) = renderer.example_alerts()?;
    send_owner_with_keyboard(client, &mail, safety_template_keyboard(), false).await?;
    send_owner_with_keyboard(client, &overdue, safety_template_keyboard(), false).await
}

pub(super) async fn safety_template_examples_for_source(
    client: &Client,
    renderer: &template::Renderer,
    source: &str,
) -> anyhow::Result<()> {
    let candidate = renderer.candidate_safety_alert(source)?;
    safety_template_examples(client, &candidate).await
}

pub(super) async fn safety_recovery_template_examples(
    client: &Client,
    renderer: &template::Renderer,
) -> anyhow::Result<()> {
    let recovery = renderer.example_recovery()?;
    send_owner_with_keyboard(client, &recovery, safety_template_keyboard(), false).await
}

pub(super) async fn safety_recovery_template_examples_for_source(
    client: &Client,
    renderer: &template::Renderer,
    source: &str,
) -> anyhow::Result<()> {
    let candidate = renderer.candidate_safety_recovery(source)?;
    safety_recovery_template_examples(client, &candidate).await
}

pub(super) async fn safety_recovery_template_updated(
    client: &Client,
    renderer: &template::Renderer,
) -> anyhow::Result<()> {
    safety_template_updated_for(client, renderer, "Safety recovery template").await
}

pub(super) async fn guide(client: &Client, renderer: &template::Renderer) -> anyhow::Result<()> {
    let guide = renderer.guide()?;
    send_owner_with_keyboard(client, &guide, safety_template_keyboard(), false).await
}

pub(super) async fn notify_started(
    client: &Client,
    renderer: &template::Renderer,
    _event: &Event,
) -> anyhow::Result<()> {
    send_owner_template(
        client,
        renderer,
        template::TemplateId::OwnerStarted,
        &json!({}),
        Phase::Active,
        false,
    )
    .await
}

pub(super) async fn notify_ok(
    client: &Client,
    renderer: &template::Renderer,
    phase: Phase,
) -> anyhow::Result<()> {
    send_owner_template(
        client,
        renderer,
        template::TemplateId::OwnerOk,
        &json!({}),
        phase,
        true,
    )
    .await
}

pub(super) async fn notify_recovery(
    client: &Client,
    renderer: &template::Renderer,
    event: &Event,
) -> anyhow::Result<()> {
    send_owner_template(
        client,
        renderer,
        template::TemplateId::OwnerRecovery,
        &json!({"event_at": event.event_at.timestamp()}),
        Phase::Active,
        false,
    )
    .await
}

pub(super) async fn notify_safety_recovery(
    client: &Client,
    renderer: &template::Renderer,
    hike: &tracker::Model,
    settings: &settings::Model,
    event: &Event,
    at: DateTimeUtc,
) -> anyhow::Result<()> {
    let rendered = renderer.render_recovery(hike, settings, event, at);
    let (text, render_failed) = match rendered {
        Ok(text) => (text, false),
        Err(error) => {
            tracing::warn!(
                reason = "recovery_template_render_failed",
                kind = %error,
                "using built-in safety recovery"
            );
            (fallback_recovery(at), true)
        }
    };
    let transport_fallback = match send(client, Audience::Safety, &text).await {
        Ok(()) => false,
        Err(error) if common::is_message_rejection(&error) => {
            tracing::warn!(
                reason = "recovery_template_message_rejected",
                "using built-in safety recovery"
            );
            send(client, Audience::Safety, &fallback_recovery(at)).await?;
            true
        }
        Err(error) => return Err(error),
    };
    if render_failed || transport_fallback {
        send_owner_template(
            client,
            renderer,
            template::TemplateId::OwnerFallback,
            &json!({}),
            Phase::Active,
            false,
        )
        .await?;
    }
    Ok(())
}

pub(super) async fn notify_finished(
    client: &Client,
    renderer: &template::Renderer,
    _event: &Event,
) -> anyhow::Result<()> {
    send_owner_template(
        client,
        renderer,
        template::TemplateId::OwnerFinished,
        &json!({}),
        Phase::Finished,
        false,
    )
    .await
}

pub(super) async fn notify_alert(
    client: &Client,
    renderer: &template::Renderer,
    hike: &tracker::Model,
    settings: &settings::Model,
    event: &Event,
    at: DateTimeUtc,
) -> anyhow::Result<()> {
    let rendered = renderer.render_alert(hike, settings, Some(event), "alert_mail", at, None);
    let (text, render_failed) = match rendered {
        Ok(text) => (text, false),
        Err(error) => {
            tracing::warn!(reason = "template_render_failed", kind = %error, "using built-in safety alert");
            (fallback_alert(event, at), true)
        }
    };
    let transport_fallback = match send(client, Audience::Safety, &text).await {
        Ok(()) => false,
        Err(error) if common::is_message_rejection(&error) => {
            tracing::warn!(
                reason = "template_message_rejected",
                "using built-in safety alert"
            );
            send(client, Audience::Safety, &fallback_alert(event, at)).await?;
            true
        }
        Err(error) => return Err(error),
    };
    if render_failed || transport_fallback {
        send_owner_template(
            client,
            renderer,
            template::TemplateId::OwnerFallback,
            &json!({}),
            Phase::Active,
            false,
        )
        .await?;
    }
    Ok(())
}

pub(super) async fn notify_reminder(
    client: &Client,
    renderer: &template::Renderer,
    hike: &tracker::Model,
    settings: &settings::Model,
    audience: Audience,
    minutes: i64,
    at: DateTimeUtc,
) -> anyhow::Result<()> {
    match audience {
        Audience::Owner => {
            let text = renderer.render(
                template::TemplateId::OwnerReminder,
                &json!({"minutes": minutes}),
            )?;
            send_owner(client, &text, Phase::Active, false).await
        }
        Audience::Safety => {
            let text = renderer.render_alert(hike, settings, None, "overdue", at, Some(minutes));
            match text {
                Ok(text) => match send(client, Audience::Safety, &text).await {
                    Ok(()) => Ok(()),
                    Err(error) if common::is_message_rejection(&error) => {
                        tracing::warn!(
                            reason = "template_message_rejected",
                            "using built-in overdue safety alert"
                        );
                        send(client, Audience::Safety, &fallback_overdue(hike, minutes)).await?;
                        send_owner_template(
                            client,
                            renderer,
                            template::TemplateId::OwnerFallback,
                            &json!({}),
                            Phase::Active,
                            false,
                        )
                        .await
                    }
                    Err(error) => Err(error),
                },
                Err(error) => {
                    tracing::warn!(reason = "template_render_failed", kind = %error, "using built-in overdue safety alert");
                    send(client, Audience::Safety, &fallback_overdue(hike, minutes)).await?;
                    send_owner_template(
                        client,
                        renderer,
                        template::TemplateId::OwnerFallback,
                        &json!({}),
                        Phase::Active,
                        false,
                    )
                    .await
                }
            }
        }
    }
}

fn fallback_alert(event: &Event, at: DateTimeUtc) -> String {
    let prefix = format!("**SAFETY ALERT: alert mail**\nAt: {}\n\n", time_entity(at));
    bounded_message(&prefix, &template::markdown_escape(&event.body))
}

fn fallback_overdue(hike: &tracker::Model, minutes: i64) -> String {
    let base = hike
        .last_ok_at
        .map(time_entity)
        .unwrap_or_else(|| "unknown".to_owned());
    let minutes = template::markdown_escape(&minutes.to_string());
    format!("**SAFETY ALERT: overdue**\nNo OK for {minutes} minutes.\nLast contact: {base}")
}

fn fallback_recovery(at: DateTimeUtc) -> String {
    format!("**SAFETY CONTACT RESUMED**\nAt: {}", time_entity(at))
}

fn bounded_message(prefix: &str, body: &str) -> String {
    let available = template::MAX_MESSAGE_CHARS.saturating_sub(prefix.chars().count());
    let mut body = body.to_owned();
    if body.chars().count() > available {
        body = body.chars().take(available).collect();
        while body.ends_with('\\') {
            body.pop();
        }
    }
    format!("{prefix}{body}")
}

fn time_entity(value: DateTimeUtc) -> String {
    format!(
        "![{}](tg://time?unix={}&format=wDT)",
        template::markdown_escape(&value.to_rfc3339()),
        value.timestamp()
    )
}

fn owner_keyboard(phase: Phase) -> ReplyMarkup {
    let rows = match phase {
        Phase::Active => vec![vec!["OK", "FINISHED"]],
        Phase::Idle | Phase::Finished => vec![vec!["Start hike", "Settings"]],
    };
    reply_keyboard(rows)
}

fn settings_keyboard() -> ReplyMarkup {
    reply_keyboard(vec![
        vec!["Owner reminder times"],
        vec!["Safety reminder times"],
        vec!["Safety alert template"],
        vec!["Safety recovery template"],
        vec!["Back"],
    ])
}

fn setting_prompt_keyboard() -> ReplyMarkup {
    reply_keyboard(vec![vec!["Back"]])
}

fn safety_template_keyboard() -> ReplyMarkup {
    reply_keyboard(vec![vec!["Rendered examples", "Guide"], vec!["Back"]])
}

fn reply_keyboard(rows: Vec<Vec<&'static str>>) -> ReplyMarkup {
    let keyboard = rows
        .into_iter()
        .map(|row| {
            row.into_iter()
                .map(|text| KeyboardButton::builder().text(text).build())
                .collect()
        })
        .collect();
    ReplyMarkup::ReplyKeyboardMarkup(
        ReplyKeyboardMarkup::builder()
            .keyboard(keyboard)
            .is_persistent(true)
            .resize_keyboard(true)
            .build(),
    )
}

fn setting_label(position: SettingsPosition) -> &'static str {
    match position {
        SettingsPosition::OwnerReminderTimes => "Owner reminder times",
        SettingsPosition::SafetyReminderTimes => "Safety reminder times",
        SettingsPosition::Main
        | SettingsPosition::Settings
        | SettingsPosition::SafetyAlertTemplate
        | SettingsPosition::SafetyRecoveryTemplate => {
            unreachable!("settings position does not select a reminder schedule")
        }
    }
}

fn concise_error(error: &anyhow::Error) -> String {
    error
        .to_string()
        .split(':')
        .next()
        .unwrap_or("template validation failed")
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn settings_keyboard_contains_the_template_editor() {
        let value = serde_json::to_value(settings_keyboard()).unwrap();
        assert_eq!(
            value["keyboard"],
            json!([
                [{"text": "Owner reminder times"}],
                [{"text": "Safety reminder times"}],
                [{"text": "Safety alert template"}],
                [{"text": "Safety recovery template"}],
                [{"text": "Back"}]
            ])
        );
    }

    #[test]
    fn template_keyboard_uses_examples_and_guide_labels() {
        let value = serde_json::to_value(safety_template_keyboard()).unwrap();
        assert_eq!(
            value["keyboard"],
            json!([
                [{"text": "Rendered examples"}, {"text": "Guide"}],
                [{"text": "Back"}]
            ])
        );
    }
}
