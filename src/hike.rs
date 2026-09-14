use crate::{app::Ctx, db::runtime::SettingsPosition, menu, notify};
use anyhow::Context;
use frankenstein::{
    AsyncTelegramApi, methods::SendRichMessageParams, rich_message::InputRichMessage,
};

pub(crate) async fn receive_ok(ctx: &mut Ctx) -> anyhow::Result<()> {
    let at = (*ctx.tracker.last_event_at.as_ref()).context("missing last event")?;
    if *ctx.tracker.active.as_ref() {
        let last_ok = (*ctx.tracker.last_ok_at.as_ref()).context("active hike missing last OK")?;
        ctx.tracker.last_ok_at.set_ne(Some(last_ok.max(at)));
        ctx.tracker.last_alert.set_ne(None);
        ctx.tracker.owner_reminders_sent.set_ne(0);
        ctx.tracker.safety_reminders_sent.set_ne(0);
    } else {
        ctx.tracker.last_alert.set_ne(None);
        start_hike(ctx).await?;
        tracing::info!("started a new hike");
    }

    send_owner(ctx, "OK received.").await?;
    notify::recover_safety(ctx).await?;
    tracing::info!("owner OK acknowledgement sent");
    Ok(())
}

pub(crate) async fn finish_hike(ctx: &mut Ctx) -> anyhow::Result<()> {
    if !*ctx.tracker.active.as_ref() {
        tracing::info!(
            reason = "inactive",
            "finished ignored because no hike is active"
        );
        return Ok(());
    }
    let at = (*ctx.tracker.last_event_at.as_ref()).context("active hike missing last event")?;

    ctx.tracker.active.set_ne(false);
    send_owner(ctx, "InReach hike finished.").await?;
    ctx.tracker.finished_at.set_ne(at);
    notify::recover_safety(ctx).await?;
    tracing::info!("finished resulted in completing the hike");
    Ok(())
}

pub(crate) async fn start_hike(ctx: &mut Ctx) -> anyhow::Result<()> {
    let at = (*ctx.tracker.last_event_at.as_ref()).context("new hike missing last event")?;
    let location = *ctx.tracker.location.as_ref();
    ctx.tracker.active.set_ne(true);
    ctx.runtime.settings_position.set_ne(SettingsPosition::Main);
    ctx.tracker.started_at.set_ne(Some(at));
    ctx.tracker.started_location.set_ne(location);
    ctx.tracker.last_event_at.set_ne(Some(at));
    ctx.tracker.last_ok_at.set_ne(Some(at));
    ctx.tracker.location.set_ne(location);
    ctx.tracker.owner_reminders_sent.set_ne(0);
    ctx.tracker.safety_reminders_sent.set_ne(0);
    ctx.tracker.safety_alerted.set_ne(false);
    send_owner(ctx, "InReach hike started.").await?;
    Ok(())
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
        .reply_markup(menu::keyboard(ctx)?)
        .build();
    ctx.bot.send_rich_message(&params).await?;
    Ok(())
}
