use crate::{app::Ctx, menu, template::TemplateId};
use anyhow::Context;
use chrono::Duration as ChronoDuration;
use frankenstein::{
    AsyncTelegramApi, ParseMode,
    methods::{SendMessageParams, SendRichMessageParams},
    rich_message::InputRichMessage,
};

pub(crate) async fn send_due_reminders(ctx: &mut Ctx) -> anyhow::Result<()> {
    ctx.runtime.next_tick_at.set_ne(None);
    if !*ctx.tracker.active.as_ref() {
        return Ok(());
    }
    send_due_owner_reminders(ctx).await?;
    send_due_safety_reminders(ctx).await?;
    Ok(())
}

pub(crate) async fn recover_safety(ctx: &mut Ctx) -> anyhow::Result<()> {
    if !*ctx.tracker.safety_alerted.as_ref() {
        return Ok(());
    }

    notify_safety(ctx, TemplateId::SafetyRecovery, "SAFETY CONTACT RESUMED").await?;
    ctx.tracker.safety_reminders_sent.set_ne(0);
    ctx.tracker.safety_alerted.set_ne(false);
    Ok(())
}

pub(crate) async fn send_explicit_alert(ctx: &mut Ctx) -> anyhow::Result<()> {
    notify_safety(ctx, TemplateId::SafetyAlert, "SAFETY ALERT: alert mail").await?;
    let count = i64::try_from(
        ctx.settings
            .safety_reminder_minutes
            .as_ref()
            .as_slice()
            .len(),
    )?;
    ctx.tracker.safety_reminders_sent.set_ne(count);
    ctx.tracker.safety_alerted.set_ne(true);
    Ok(())
}

async fn send_due_owner_reminders(ctx: &mut Ctx) -> anyhow::Result<()> {
    let last_ok = (*ctx.tracker.last_ok_at.as_ref()).context("active hike missing last OK")?;
    let thresholds = ctx.settings.owner_reminder_minutes.as_ref().clone();
    let mut sent = usize::try_from(*ctx.tracker.owner_reminders_sent.as_ref())?;
    for &minutes in thresholds.as_slice().iter().skip(sent) {
        let deadline = last_ok + ChronoDuration::minutes(minutes);
        if deadline > ctx.now {
            let next =
                (*ctx.runtime.next_tick_at.as_ref()).map_or(deadline, |next| next.min(deadline));
            ctx.runtime.next_tick_at.set_ne(Some(next));
            break;
        }
        notify_owner(ctx, &format!("No InReach OK for {minutes} minutes.")).await?;
        sent += 1;
        ctx.tracker
            .owner_reminders_sent
            .set_ne(i64::try_from(sent)?);
    }
    Ok(())
}

async fn send_due_safety_reminders(ctx: &mut Ctx) -> anyhow::Result<()> {
    let last_ok = (*ctx.tracker.last_ok_at.as_ref()).context("active hike missing last OK")?;
    let thresholds = ctx.settings.safety_reminder_minutes.as_ref().clone();
    let mut sent = usize::try_from(*ctx.tracker.safety_reminders_sent.as_ref())?;
    for &minutes in thresholds.as_slice().iter().skip(sent) {
        let deadline = last_ok + ChronoDuration::minutes(minutes);
        if deadline > ctx.now {
            let next =
                (*ctx.runtime.next_tick_at.as_ref()).map_or(deadline, |next| next.min(deadline));
            ctx.runtime.next_tick_at.set_ne(Some(next));
            break;
        }
        notify_safety(ctx, TemplateId::SafetyAlert, "SAFETY ALERT: overdue").await?;
        sent += 1;
        ctx.tracker
            .safety_reminders_sent
            .set_ne(i64::try_from(sent)?);
        ctx.tracker.safety_alerted.set_ne(true);
    }
    Ok(())
}

async fn notify_owner(ctx: &Ctx, text: &str) -> anyhow::Result<()> {
    let params = SendRichMessageParams::builder()
        .chat_id(ctx.config.owner_chat_id)
        .rich_message(
            InputRichMessage::builder()
                .markdown(text.to_owned())
                .build(),
        )
        .reply_markup(menu::keyboard(ctx)?)
        .build();
    ctx.bot.send_rich_message(&params).await?;
    Ok(())
}

async fn notify_safety(ctx: &Ctx, template_id: TemplateId, fallback: &str) -> anyhow::Result<()> {
    if let Err(error) = (|| async {
        let text = template_id.render_mdv2(ctx)?;
        // Workaround for https://bugs.telegram.org/c/63275: Telegram Android
        // does not render rich-message date_time entities in table cells.
        let params = SendMessageParams::builder()
            .chat_id(ctx.config.safety_chat_id)
            .text(text)
            .parse_mode(ParseMode::MarkdownV2)
            .build();
        ctx.bot.send_message(&params).await?;
        Ok::<(), anyhow::Error>(())
    })()
    .await
    {
        tracing::error!(
            reason = "safety_notification_failed",
            template = ?template_id,
            kind = %error,
            "using static safety fallback"
        );
        let params = SendMessageParams::builder()
            .chat_id(ctx.config.safety_chat_id)
            .text(fallback.to_owned())
            .build();
        ctx.bot.send_message(&params).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{app::make_test_ctx, bot::TelegramError};
    use frankenstein::{
        ParseMode,
        response::MethodResponse,
        types::{ChatId, Message},
    };
    use mockall::Sequence;

    fn success() -> MethodResponse<Message> {
        serde_json::from_value(serde_json::json!({
            "ok": true,
            "result": {
                "message_id": 1,
                "date": 1700000000,
                "chat": {"id": 20, "type": "private"},
                "text": "sent"
            }
        }))
        .unwrap()
    }

    fn api_error(code: i32) -> TelegramError {
        frankenstein::Error::Api(frankenstein::response::ErrorResponse {
            ok: false,
            description: "test failure".to_owned(),
            error_code: code as u64,
            parameters: None,
        })
        .into()
    }

    fn rejection() -> TelegramError {
        frankenstein::Error::Api(frankenstein::response::ErrorResponse {
            ok: false,
            description: "Bad Request: can't parse entities".to_owned(),
            error_code: 400,
            parameters: None,
        })
        .into()
    }

    fn active_context() -> Ctx {
        let mut ctx = make_test_ctx();
        let at = ctx.now;
        ctx.tracker.active.set_ne(true);
        ctx.tracker.started_at.set_ne(Some(at));
        ctx.tracker.last_event_at.set_ne(Some(at));
        ctx.tracker.last_ok_at.set_ne(Some(at));
        ctx
    }

    #[tokio::test]
    async fn each_current_alert_mail_is_delivered_with_its_own_body() {
        let mut ctx = active_context();
        let at = ctx.now;
        ctx.tracker.last_ok_at = sea_orm::ActiveValue::unchanged(Some(at));

        let mut sequence = Sequence::new();
        for body in ["first alert", "second alert"] {
            ctx.tracker.last_alert = sea_orm::Set(Some(body.to_owned()));
            ctx.bot
                .expect_send_message()
                .withf(move |params| {
                    params.chat_id == ChatId::Integer(20)
                        && params.parse_mode == Some(ParseMode::MarkdownV2)
                        && params.text.contains(body)
                })
                .times(1)
                .in_sequence(&mut sequence)
                .returning(|_| Ok(success()));
            send_explicit_alert(&mut ctx).await.unwrap();
        }

        assert_eq!(*ctx.tracker.last_ok_at.as_ref(), Some(at));
    }

    #[tokio::test]
    async fn identical_alert_bodies_are_still_delivered_as_separate_inputs() {
        let mut ctx = active_context();
        let at = ctx.now;
        ctx.tracker.last_ok_at = sea_orm::ActiveValue::unchanged(Some(at));
        ctx.bot
            .expect_send_message()
            .withf(|params| {
                params.chat_id == ChatId::Integer(20)
                    && params.parse_mode == Some(ParseMode::MarkdownV2)
            })
            .times(2)
            .returning(|_| Ok(success()));

        for _ in 0..2 {
            ctx.tracker.last_alert = sea_orm::Set(Some("same alert".to_owned()));
            send_explicit_alert(&mut ctx).await.unwrap();
        }

        assert_eq!(*ctx.tracker.last_ok_at.as_ref(), Some(at));
    }

    #[tokio::test]
    async fn plain_alerts_are_bounded_by_the_send_message_limit() {
        let mut ctx = active_context();
        let body = "x".repeat(5_000);
        ctx.tracker.last_alert = sea_orm::Set(Some(body.clone()));
        ctx.bot
            .expect_send_message()
            .withf(|params| {
                params.chat_id == ChatId::Integer(20)
                    && params.parse_mode == Some(ParseMode::MarkdownV2)
                    && params.text.chars().count() == 4_096
            })
            .returning(|_| Ok(success()));

        send_explicit_alert(&mut ctx).await.unwrap();
    }

    #[tokio::test]
    async fn receive_ok_recovers_safety_when_it_was_alerted() {
        let mut ctx = active_context();
        let new = ctx.now + ChronoDuration::minutes(1);
        ctx.tracker.safety_alerted.set_ne(true);
        ctx.tracker.last_alert.set_ne(None);
        ctx.tracker.last_event_at.set_ne(Some(new));
        ctx.tracker.last_ok_at.set_ne(Some(new));
        let mut sequence = Sequence::new();
        ctx.bot
            .expect_send_rich_message()
            .withf(|params| params.chat_id == ChatId::Integer(10))
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_| Ok(success()));
        ctx.bot
            .expect_send_message()
            .withf(|params| {
                params.chat_id == ChatId::Integer(20)
                    && params.parse_mode == Some(ParseMode::MarkdownV2)
            })
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_| Ok(success()));

        crate::hike::receive_ok(&mut ctx).await.unwrap();

        assert!(!*ctx.tracker.safety_alerted.as_ref());
    }

    #[tokio::test]
    async fn finish_hike_recovers_safety_when_it_was_alerted() {
        let mut ctx = active_context();
        ctx.tracker.safety_alerted.set_ne(true);
        let mut sequence = Sequence::new();
        ctx.bot
            .expect_send_rich_message()
            .withf(|params| params.chat_id == ChatId::Integer(10))
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_| Ok(success()));
        ctx.bot
            .expect_send_message()
            .withf(|params| {
                params.chat_id == ChatId::Integer(20)
                    && params.parse_mode == Some(ParseMode::MarkdownV2)
            })
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_| Ok(success()));

        crate::hike::finish_hike(&mut ctx).await.unwrap();

        assert!(!*ctx.tracker.safety_alerted.as_ref());
    }

    #[tokio::test]
    async fn reminder_delivery_updates_counters_in_audience_order() {
        let mut ctx = active_context();
        ctx.now += ChronoDuration::minutes(60);
        let mut sequence = Sequence::new();
        ctx.bot
            .expect_send_rich_message()
            .withf(|params| params.chat_id == ChatId::Integer(10))
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_| Ok(success()));
        ctx.bot
            .expect_send_message()
            .withf(|params| {
                params.chat_id == ChatId::Integer(20)
                    && params.parse_mode == Some(ParseMode::MarkdownV2)
            })
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_| Ok(success()));

        send_due_reminders(&mut ctx).await.unwrap();

        assert_eq!(*ctx.tracker.owner_reminders_sent.as_ref(), 1);
        assert_eq!(*ctx.tracker.safety_reminders_sent.as_ref(), 1);
    }

    #[tokio::test]
    async fn safety_fallback_failure_propagates_after_earlier_send() {
        let mut ctx = active_context();
        ctx.now += ChronoDuration::minutes(60);
        let mut sequence = Sequence::new();
        ctx.bot
            .expect_send_rich_message()
            .withf(|params| params.chat_id == ChatId::Integer(10))
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_| Ok(success()));
        ctx.bot
            .expect_send_message()
            .withf(|params| {
                params.chat_id == ChatId::Integer(20)
                    && params.parse_mode == Some(ParseMode::MarkdownV2)
            })
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_| Err(api_error(400)));
        ctx.bot
            .expect_send_message()
            .withf(|params| params.chat_id == ChatId::Integer(20))
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_| Err(api_error(400)));

        let error = send_due_reminders(&mut ctx)
            .await
            .expect_err("safety send must fail");

        assert_eq!(*ctx.tracker.owner_reminders_sent.as_ref(), 1);
        assert!(error.to_string().contains("Telegram API error 400"));
    }

    #[tokio::test]
    async fn rejected_safety_message_uses_the_static_fallback_and_logs_error() {
        let mut ctx = active_context();
        ctx.templates = crate::template::new_environment(
            "bad *",
            crate::template::DEFAULT_SAFETY_RECOVERY_TEMPLATE,
        )
        .unwrap();
        ctx.tracker.last_alert = sea_orm::Set(Some("alert".to_owned()));
        let mut sequence = Sequence::new();
        ctx.bot
            .expect_send_message()
            .withf(|params| {
                params.chat_id == ChatId::Integer(20)
                    && params.text == "bad \\*"
                    && params.parse_mode == Some(ParseMode::MarkdownV2)
            })
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_| Err(rejection()));
        ctx.bot
            .expect_send_message()
            .withf(|params| {
                params.chat_id == ChatId::Integer(20)
                    && params.text == "SAFETY ALERT: alert mail"
                    && params.disable_notification.is_none()
            })
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_| Ok(success()));

        send_explicit_alert(&mut ctx).await.unwrap();
    }

    #[tokio::test]
    async fn failed_safety_template_uses_the_static_fallback() {
        let mut ctx = active_context();
        ctx.templates
            .add_template_owned(
                TemplateId::SafetyAlert.name().to_owned(),
                "{{ missing }}".to_owned(),
            )
            .unwrap();
        ctx.bot
            .expect_send_message()
            .withf(|params| {
                params.chat_id == ChatId::Integer(20)
                    && params.text == "SAFETY ALERT: alert mail"
                    && params.disable_notification.is_none()
            })
            .times(1)
            .returning(|_| Ok(success()));

        send_explicit_alert(&mut ctx).await.unwrap();
    }
}
