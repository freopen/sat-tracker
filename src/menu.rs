use crate::{
    app::Ctx,
    bot,
    db::{runtime::SettingsPosition, settings::ReminderMinutes},
    hike,
    template::{self, TemplateId},
    time::{DateTimeUtc, normalize},
};
use anyhow::Context;
use frankenstein::{
    AsyncTelegramApi, ParseMode,
    methods::{SendMessageParams, SendRichMessageParams},
    rich_message::InputRichMessage,
    types::{KeyboardButton, ReplyKeyboardMarkup, ReplyMarkup},
    updates::{Update, UpdateContent},
};
use regex::Regex;
use std::sync::LazyLock;

macro_rules! re {
    ($pattern:literal, $text:expr) => {{
        static RE: LazyLock<Regex> = LazyLock::new(|| {
            Regex::new(concat!(r"\A\s*(?:", $pattern, r")\s*\z")).expect("command regex is valid")
        });
        RE.is_match($text)
    }};
}

#[derive(Clone, Debug)]
struct Message {
    at: DateTimeUtc,
    text: Option<String>,
    owner: bool,
}

pub(crate) async fn process(ctx: &mut Ctx) -> anyhow::Result<()> {
    let (update, received_at) = {
        let row = ctx.inbox.as_ref().context("pending inbox row missing")?;
        (
            serde_json::from_slice(row.payload.as_deref().context("Telegram payload missing")?)?,
            row.received_at,
        )
    };
    let message = parse_message(&update, received_at, ctx.config.owner_chat_id);
    if !message.owner {
        return Ok(());
    }
    let Some(text) = message.text.as_deref() else {
        return Ok(());
    };

    let active = *ctx.tracker.active.as_ref();
    let settings_position = *ctx.runtime.settings_position.as_ref();
    match (active, settings_position) {
        (_, _) if re!(r"/start(?:@\S+)?", text) => {
            ctx.runtime.settings_position.set_ne(SettingsPosition::Main);
            send_owner(ctx, "Choose an action.").await?;
        }
        (_, _) if re!(r"/version(?:@\S+)?", text) => {
            ctx.runtime.settings_position.set_ne(SettingsPosition::Main);
            let text = format_version();
            send_owner(ctx, &text).await?;
        }
        (_, _) if re!(r"(?:Start hike|OK)", text) => {
            ctx.runtime.settings_position.set_ne(SettingsPosition::Main);
            let last_event_at = *ctx.tracker.last_event_at.as_ref();
            if !active || last_event_at.is_none_or(|last_event_at| last_event_at < message.at) {
                ctx.tracker.last_event_at = sea_orm::Set(Some(message.at));
                ctx.tracker.location = sea_orm::Set(None);
            }
            hike::receive_ok(ctx).await?;
        }
        (_, _) if re!(r"FINISHED", text) => {
            ctx.runtime.settings_position.set_ne(SettingsPosition::Main);
            let last_event_at = *ctx.tracker.last_event_at.as_ref();
            if active {
                if last_event_at.is_none_or(|last_event_at| last_event_at < message.at) {
                    ctx.tracker.last_event_at = sea_orm::Set(Some(message.at));
                }
                ctx.tracker.location = sea_orm::Set(None);
            }
            hike::finish_hike(ctx).await?;
        }
        (true, _) => return Ok(()),
        (_, SettingsPosition::Main) if re!(r"Settings", text) => {
            ctx.runtime
                .settings_position
                .set_ne(SettingsPosition::Settings);
            send_owner(ctx, "Choose a setting.").await?;
        }
        (_, SettingsPosition::Settings) if re!(r"Owner reminder times", text) => {
            let position = SettingsPosition::OwnerReminderTimes;
            let value = reminder_value(ctx, position)
                .context("settings position does not select a reminder schedule")?;
            let value = value.clone();
            ctx.runtime.settings_position.set_ne(position);
            send_owner(ctx, &format_setting_prompt(position, &value)).await?;
        }
        (_, SettingsPosition::Settings) if re!(r"Safety reminder times", text) => {
            let position = SettingsPosition::SafetyReminderTimes;
            let value = reminder_value(ctx, position)
                .context("settings position does not select a reminder schedule")?;
            let value = value.clone();
            ctx.runtime.settings_position.set_ne(position);
            send_owner(ctx, &format_setting_prompt(position, &value)).await?;
        }
        (_, SettingsPosition::Settings) if re!(r"Safety alert template", text) => {
            ctx.runtime
                .settings_position
                .set_ne(SettingsPosition::SafetyAlertTemplate);
            send_owner(
                ctx,
                &format_template_prompt(
                    "Safety alert template",
                    TemplateId::SafetyAlert.source(ctx),
                ),
            )
            .await?;
        }
        (_, SettingsPosition::Settings) if re!(r"Safety recovery template", text) => {
            ctx.runtime
                .settings_position
                .set_ne(SettingsPosition::SafetyRecoveryTemplate);
            send_owner(
                ctx,
                &format_template_prompt(
                    "Safety recovery template",
                    TemplateId::SafetyRecovery.source(ctx),
                ),
            )
            .await?;
        }
        (_, SettingsPosition::SafetyAlertTemplate) if re!(r"Rendered examples", text) => {
            send_template_examples(ctx, TemplateId::SafetyAlert).await?
        }
        (_, SettingsPosition::SafetyRecoveryTemplate) if re!(r"Rendered examples", text) => {
            send_template_examples(ctx, TemplateId::SafetyRecovery).await?
        }
        (_, SettingsPosition::SafetyAlertTemplate) if re!(r"Guide", text) => {
            let guide = template::render_guide(&ctx.templates)?;
            send_owner(ctx, &guide).await?;
        }
        (_, SettingsPosition::SafetyRecoveryTemplate) if re!(r"Guide", text) => {
            let guide = template::render_guide(&ctx.templates)?;
            send_owner(ctx, &guide).await?;
        }
        (
            _,
            SettingsPosition::OwnerReminderTimes
            | SettingsPosition::SafetyReminderTimes
            | SettingsPosition::SafetyAlertTemplate
            | SettingsPosition::SafetyRecoveryTemplate,
        ) if re!(r"Back", text) => {
            ctx.runtime
                .settings_position
                .set_ne(SettingsPosition::Settings);
            send_owner(ctx, "Choose a setting.").await?;
        }
        (_, SettingsPosition::Settings | SettingsPosition::Main) if re!(r"Back", text) => {
            ctx.runtime.settings_position.set_ne(SettingsPosition::Main);
            send_owner(ctx, "Choose an action.").await?;
        }
        (
            _,
            position @ (SettingsPosition::OwnerReminderTimes
            | SettingsPosition::SafetyReminderTimes),
        ) => match ReminderMinutes::parse(text) {
            Ok(value) => {
                if position == SettingsPosition::OwnerReminderTimes {
                    ctx.settings.owner_reminder_minutes.set_ne(value.clone());
                } else {
                    ctx.settings.safety_reminder_minutes.set_ne(value.clone());
                }
                ctx.runtime
                    .settings_position
                    .set_ne(SettingsPosition::Settings);
                send_owner(
                    ctx,
                    &format!("{} updated to {}.", setting_label(position), value),
                )
                .await?;
            }
            Err(_) => {
                let value = reminder_value(ctx, position)
                    .context("missing reminder value")?
                    .clone();
                send_owner(
                    ctx,
                    &format!(
                        "Invalid reminder times.\n\n{} are currently {}.\nType a new comma-separated list of positive, strictly increasing minutes (for example, 30, 45, 60).",
                        setting_label(position),
                        value
                    ),
                )
                .await?;
            }
        },
        (_, SettingsPosition::SafetyAlertTemplate) => {
            submit_template(ctx, TemplateId::SafetyAlert, template_source(text)).await?
        }
        (_, SettingsPosition::SafetyRecoveryTemplate) => {
            submit_template(ctx, TemplateId::SafetyRecovery, template_source(text)).await?
        }
        _ => {}
    }
    Ok(())
}

async fn submit_template(
    ctx: &mut Ctx,
    template_id: TemplateId,
    source: &str,
) -> anyhow::Result<()> {
    if let Err(error) = template_id.set(ctx, source) {
        send_owner(
            ctx,
            &format_template_invalid(template_id.label(), &error.to_string()),
        )
        .await?;
        return Ok(());
    }
    for sample in template_id.render_examples(ctx)? {
        match send_owner(ctx, &sample).await {
            Ok(()) => {}
            Err(error) if bot::is_message_rejection(&error) => {
                send_owner(
                    ctx,
                    &format_template_invalid(
                        template_id.label(),
                        "template sample was rejected by Telegram",
                    ),
                )
                .await?;
                return Ok(());
            }
            Err(error) => return Err(error),
        }
    }
    ctx.runtime
        .settings_position
        .set_ne(SettingsPosition::Settings);
    send_owner(ctx, &format!("{} updated.", template_id.label())).await
}

async fn send_template_examples(ctx: &Ctx, template_id: TemplateId) -> anyhow::Result<()> {
    for text in template_id.render_mdv2_examples(ctx)? {
        // Workaround for https://bugs.telegram.org/c/63275: Telegram Android
        // does not render rich-message date_time entities in table cells.
        let params = SendMessageParams::builder()
            .chat_id(ctx.config.owner_chat_id)
            .text(text)
            .parse_mode(ParseMode::MarkdownV2)
            .disable_notification(true)
            .reply_markup(keyboard(ctx)?)
            .build();
        ctx.bot.send_message(&params).await?;
    }
    Ok(())
}

fn parse_message(update: &Update, received_at: DateTimeUtc, owner_chat_id: i64) -> Message {
    let UpdateContent::Message(message) = &update.content else {
        return Message {
            at: received_at,
            text: None,
            owner: false,
        };
    };
    let at = u64::try_into(message.date)
        .ok()
        .and_then(|seconds: i64| chrono::DateTime::from_timestamp(seconds, 0))
        .map(normalize)
        .unwrap_or(received_at);
    Message {
        at,
        text: message.text.clone(),
        owner: message.chat.id == owner_chat_id,
    }
}

fn reminder_value(ctx: &Ctx, position: SettingsPosition) -> Option<&ReminderMinutes> {
    match position {
        SettingsPosition::OwnerReminderTimes => Some(ctx.settings.owner_reminder_minutes.as_ref()),
        SettingsPosition::SafetyReminderTimes => {
            Some(ctx.settings.safety_reminder_minutes.as_ref())
        }
        _ => None,
    }
}
fn setting_label(position: SettingsPosition) -> &'static str {
    match position {
        SettingsPosition::OwnerReminderTimes => "Owner reminder times",
        SettingsPosition::SafetyReminderTimes => "Safety reminder times",
        _ => unreachable!(),
    }
}
fn format_setting_prompt(position: SettingsPosition, value: &ReminderMinutes) -> String {
    format!(
        "{} are currently {}.\nType a new comma-separated list of positive, strictly increasing minutes (for example, 30, 45, 60).",
        setting_label(position),
        value
    )
}
fn format_template_prompt(label: &str, source: &str) -> String {
    format!(
        "{label}:\n\n```jinja2\n{source}\n```\n\nSend a new template, or choose an action below."
    )
}

fn template_source(text: &str) -> &str {
    let trimmed = text.trim();
    let Some(body) = trimmed
        .strip_prefix("```")
        .and_then(|body| body.strip_suffix("```"))
    else {
        return text;
    };
    let body = if body
        .chars()
        .next()
        .is_some_and(|character| !character.is_whitespace())
    {
        body.char_indices()
            .find_map(|(index, character)| character.is_whitespace().then_some(&body[index..]))
            .unwrap_or(body)
    } else {
        body
    };
    body.trim()
}

fn format_template_invalid(label: &str, error: &str) -> String {
    format!(
        "{label} was not saved. {}\n\nSend another template, or choose an action below.",
        concise_error(error)
    )
}
fn concise_error(error: &str) -> String {
    error
        .split(':')
        .next()
        .unwrap_or("template validation failed")
        .to_owned()
}
fn format_version() -> String {
    format!(
        "Version: {}\nBuild time: {}\nGit commit: {}\nGit dirty: {}",
        env!("CARGO_PKG_VERSION"),
        option_env!("VERGEN_BUILD_TIMESTAMP").unwrap_or("unknown"),
        option_env!("VERGEN_GIT_SHA").unwrap_or("unknown"),
        option_env!("VERGEN_GIT_DIRTY").unwrap_or("unknown")
    )
}

async fn send_owner(ctx: &Ctx, text: &str) -> anyhow::Result<()> {
    let params = SendRichMessageParams::builder()
        .chat_id(ctx.config.owner_chat_id)
        .rich_message(
            InputRichMessage::builder()
                .markdown(text.to_owned())
                .build(),
        )
        .disable_notification(true)
        .reply_markup(keyboard(ctx)?)
        .build();
    ctx.bot.send_rich_message(&params).await?;
    Ok(())
}

pub(crate) fn keyboard(ctx: &Ctx) -> anyhow::Result<ReplyMarkup> {
    let rows = match (
        *ctx.tracker.active.as_ref(),
        *ctx.runtime.settings_position.as_ref(),
    ) {
        (true, _) => vec![vec!["OK", "FINISHED"]],
        (_, SettingsPosition::Main) => vec![vec!["Start hike", "Settings"]],
        (_, SettingsPosition::Settings) => vec![
            vec!["Owner reminder times"],
            vec!["Safety reminder times"],
            vec!["Safety alert template"],
            vec!["Safety recovery template"],
            vec!["Back"],
        ],
        (_, SettingsPosition::OwnerReminderTimes | SettingsPosition::SafetyReminderTimes) => {
            vec![vec!["Back"]]
        }
        (_, SettingsPosition::SafetyAlertTemplate | SettingsPosition::SafetyRecoveryTemplate) => {
            vec![vec!["Rendered examples", "Guide"], vec!["Back"]]
        }
    };
    Ok(ReplyMarkup::ReplyKeyboardMarkup(
        ReplyKeyboardMarkup::builder()
            .keyboard(
                rows.into_iter()
                    .map(|row| {
                        row.into_iter()
                            .map(|text| KeyboardButton::builder().text(text).build())
                            .collect()
                    })
                    .collect(),
            )
            .is_persistent(true)
            .resize_keyboard(true)
            .build(),
    ))
}

#[cfg(test)]
mod tests {
    use crate::app::make_test_ctx;
    use crate::template::TemplateId;
    use frankenstein::{
        ParseMode,
        response::MethodResponse,
        types::{ChatId, Message},
    };
    use mockall::Sequence;

    use super::{send_template_examples, template_source};

    fn success() -> MethodResponse<Message> {
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

    #[test]
    fn template_source_accepts_plain_text() {
        assert_eq!(template_source("  plain template  "), "  plain template  ");
    }

    #[test]
    fn template_source_unwraps_single_line_code_blocks() {
        assert_eq!(template_source("```plain```"), "plain");
        assert_eq!(
            template_source("```jinja2 plain template```"),
            "plain template"
        );
    }

    #[test]
    fn template_source_unwraps_multiline_code_blocks() {
        assert_eq!(
            template_source("```\nplain template\n```"),
            "plain template"
        );
        assert_eq!(
            template_source(" ```jinja2\nline one\nline two\n``` "),
            "line one\nline two"
        );
    }

    #[test]
    fn template_source_leaves_unfenced_text_unchanged() {
        let text = "plain template";
        assert_eq!(template_source(text), text);
    }

    #[tokio::test]
    async fn template_examples_use_markdown_v2_send_messages() {
        let mut ctx = make_test_ctx();
        let mut sequence = Sequence::new();
        for _ in 0..2 {
            ctx.bot
                .expect_send_message()
                .withf(|params| {
                    params.chat_id == ChatId::Integer(10)
                        && params.parse_mode == Some(ParseMode::MarkdownV2)
                        && params.disable_notification == Some(true)
                        && params.text.contains("![")
                        && params.text.contains("tg://time?")
                        && params
                            .text
                            .contains("https://www.google.com/maps/search/?api=1&query=")
                })
                .times(1)
                .in_sequence(&mut sequence)
                .returning(|_| Ok(success()));
        }

        send_template_examples(&ctx, TemplateId::SafetyAlert)
            .await
            .unwrap();
    }
}
