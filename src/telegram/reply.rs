use crate::{
    entity::tracker,
    state::{Audience, Event, Phase, ReminderMinutes, SettingsPosition, format_time},
    telegram::common::Client,
};
use frankenstein::{
    AsyncTelegramApi,
    methods::SendMessageParams,
    types::{KeyboardButton, ReplyKeyboardMarkup, ReplyMarkup},
};

pub(super) async fn send(client: &Client, audience: Audience, text: &str) -> anyhow::Result<()> {
    let chat_id = match audience {
        Audience::Owner => client.owner_chat_id,
        Audience::Safety => client.safety_chat_id,
    };
    client
        .bot
        .send_message(
            &SendMessageParams::builder()
                .chat_id(chat_id)
                .text(text)
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
        .send_message(
            &SendMessageParams::builder()
                .chat_id(client.owner_chat_id)
                .text(text)
                .disable_notification(disable_notification)
                .reply_markup(keyboard)
                .build(),
        )
        .await?;
    Ok(())
}

pub(super) async fn main_menu(client: &Client, phase: Phase) -> anyhow::Result<()> {
    send_owner(client, "Choose an action.", phase, false).await
}

pub(super) async fn version(client: &Client, phase: Phase) -> anyhow::Result<()> {
    send_owner(client, &crate::build_info().message(), phase, false).await
}

pub(super) async fn settings_menu(client: &Client) -> anyhow::Result<()> {
    send_owner_with_keyboard(client, "Choose a setting.", settings_keyboard(), false).await
}

pub(super) async fn settings_unavailable(client: &Client, phase: Phase) -> anyhow::Result<()> {
    send_owner(
        client,
        "Settings are unavailable while a hike is active.",
        phase,
        false,
    )
    .await
}

pub(super) async fn setting_prompt(
    client: &Client,
    position: SettingsPosition,
    value: &crate::state::ReminderMinutes,
) -> anyhow::Result<()> {
    send_owner_with_keyboard(
        client,
        &setting_prompt_text(position, value),
        setting_prompt_keyboard(),
        false,
    )
    .await
}

pub(super) async fn setting_updated(
    client: &Client,
    position: SettingsPosition,
    value: &crate::state::ReminderMinutes,
) -> anyhow::Result<()> {
    send_owner_with_keyboard(
        client,
        &format!("{} updated to {}.", setting_label(position), value),
        settings_keyboard(),
        false,
    )
    .await
}

pub(super) async fn setting_invalid(
    client: &Client,
    position: SettingsPosition,
    current: &crate::state::ReminderMinutes,
) -> anyhow::Result<()> {
    send_owner_with_keyboard(
        client,
        &format!(
            "Invalid reminder times.\n\n{}",
            setting_prompt_text(position, current)
        ),
        setting_prompt_keyboard(),
        false,
    )
    .await
}

pub(super) async fn notify_started(client: &Client, event: &Event) -> anyhow::Result<()> {
    send_owner(client, &started(event), Phase::Active, false).await
}

pub(super) async fn notify_ok(client: &Client, phase: Phase) -> anyhow::Result<()> {
    send_owner(client, "OK received.", phase, true).await
}

pub(super) async fn notify_recovery(
    client: &Client,
    audience: Audience,
    event: &Event,
) -> anyhow::Result<()> {
    let text = recovery(event);
    match audience {
        Audience::Owner => send_owner(client, &text, Phase::Active, false).await,
        Audience::Safety => send(client, Audience::Safety, &text).await,
    }
}

pub(super) async fn notify_finished(
    client: &Client,
    audience: Audience,
    event: &Event,
) -> anyhow::Result<()> {
    let text = finished(event);
    match audience {
        Audience::Owner => send_owner(client, &text, Phase::Finished, false).await,
        Audience::Safety => send(client, Audience::Safety, &text).await,
    }
}

pub(super) async fn notify_unrecognized(client: &Client, event: &Event) -> anyhow::Result<()> {
    send(client, Audience::Safety, &unrecognized(event)).await
}

pub(super) async fn notify_reminder(
    client: &Client,
    hike: &tracker::Model,
    audience: Audience,
    minutes: i64,
) -> anyhow::Result<()> {
    let text = reminder(hike, audience, minutes);
    match audience {
        Audience::Owner => send_owner(client, &text, Phase::Active, false).await,
        Audience::Safety => send(client, Audience::Safety, &text).await,
    }
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
        vec!["Back"],
    ])
}

fn setting_prompt_keyboard() -> ReplyMarkup {
    reply_keyboard(vec![vec!["Back"]])
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

fn location(value: Option<&str>) -> String {
    value.map(|s| format!("\n{s}")).unwrap_or_default()
}

fn started(event: &Event) -> String {
    format!(
        "InReach hike started at {}.{}",
        format_time(event.event_at),
        location(event.location.as_deref())
    )
}

fn recovery(event: &Event) -> String {
    format!(
        "InReach contact resumed at {}.{}",
        format_time(event.event_at),
        location(event.location.as_deref())
    )
}

fn finished(event: &Event) -> String {
    format!(
        "InReach hike FINISHED at {}.{}\n\n{}",
        format_time(event.event_at),
        location(event.location.as_deref()),
        event.body
    )
}

fn unrecognized(event: &Event) -> String {
    format!(
        "SAFETY ALERT: unrecognized InReach message\nEvent time: {}\n\n{}",
        format_time(event.event_at),
        event.body
    )
}

fn reminder(hike: &tracker::Model, audience: Audience, minutes: i64) -> String {
    let prefix = match audience {
        Audience::Owner => "No InReach OK",
        Audience::Safety => "SAFETY ALERT: no InReach OK",
    };
    format!(
        "{prefix} for {minutes} minutes. Last contact: {}.{}\n\n{}",
        format_time(hike.last_ok_at.expect("active hike has last OK")),
        location(hike.location.as_deref()),
        hike.last_body.as_deref().unwrap_or_default()
    )
}

fn setting_label(position: SettingsPosition) -> &'static str {
    match position {
        SettingsPosition::OwnerReminderTimes => "Owner reminder times",
        SettingsPosition::SafetyReminderTimes => "Safety reminder times",
        SettingsPosition::Main | SettingsPosition::Settings => {
            unreachable!("settings position does not select a reminder schedule")
        }
    }
}

fn setting_prompt_text(position: SettingsPosition, value: &ReminderMinutes) -> String {
    format!(
        "{} are currently {}.\nType a new comma-separated list of positive, strictly increasing minutes (for example, 30, 45, 60).",
        setting_label(position),
        value,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn owner_keyboard_serializes_phase_specific_one_time_buttons() {
        let inactive = serde_json::to_value(owner_keyboard(Phase::Finished)).unwrap();
        assert_eq!(
            inactive["keyboard"],
            json!([[{"text": "Start hike"}, {"text": "Settings"}]])
        );
        assert_eq!(inactive["is_persistent"], true);
        assert_eq!(inactive["resize_keyboard"], true);

        let active = serde_json::to_value(owner_keyboard(Phase::Active)).unwrap();
        assert_eq!(
            active["keyboard"],
            json!([[{"text": "OK"}, {"text": "FINISHED"}]])
        );

        let settings = serde_json::to_value(settings_keyboard()).unwrap();
        assert_eq!(
            settings["keyboard"],
            json!([
                [{"text": "Owner reminder times"}],
                [{"text": "Safety reminder times"}],
                [{"text": "Back"}]
            ])
        );

        let prompt = serde_json::to_value(setting_prompt_keyboard()).unwrap();
        assert_eq!(prompt["keyboard"], json!([[{"text": "Back"}]]));
    }
}
