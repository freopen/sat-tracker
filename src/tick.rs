use crate::{
    App, db,
    entity::{inbox, runtime, settings, tracker},
    mail,
    state::{
        Audience, DateTimeUtc, Event, FINISHED_COOLDOWN, IngressSource, Phase, RawMail, Signal,
        normalize,
    },
};
use anyhow::Context;
use chrono::Duration as ChronoDuration;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseTransaction, EntityTrait, IntoActiveModel, QueryFilter,
    QueryOrder, Set,
};

impl App {
    /// Process all accepted input and due reminders in one IMMEDIATE transaction.
    /// `now` is a UTC timestamp and is normalized to millisecond precision.
    /// Telegram sends are external effects: a rolled-back tick may send them again.
    pub async fn tick(&self, now: DateTimeUtc) -> anyhow::Result<Option<DateTimeUtc>> {
        let tx = db::immediate(&self.db).await?;
        let next = self.process_tick(&tx, now).await?;
        tx.commit().await?;
        Ok(next)
    }

    async fn process_tick(
        &self,
        tx: &DatabaseTransaction,
        requested_now: DateTimeUtc,
    ) -> anyhow::Result<Option<DateTimeUtc>> {
        let runtime = runtime::Entity::find_by_id(1)
            .one(tx)
            .await?
            .context("missing runtime")?;
        let now = effective_now(requested_now, runtime.last_tick_at);
        let mut hike = tracker::Entity::find_by_id(1)
            .one(tx)
            .await?
            .context("missing tracker")?;

        let saved_settings = self.load_settings(tx).await?;
        self.telegram.sync_templates(&saved_settings)?;
        self.telegram.sync_phase(tx, hike.phase).await?;
        self.process_pending_inbox(tx, &mut hike, now).await?;
        let settings = self.load_settings(tx).await?;
        self.telegram.sync_templates(&settings)?;
        let next = self.reminders(&mut hike, &settings, now).await?;

        hike.into_active_model().reset_all().update(tx).await?;
        runtime::ActiveModel {
            id: Set(1),
            last_tick_at: Set(Some(now)),
            next_tick_at: Set(next),
            ..Default::default()
        }
        .update(tx)
        .await?;
        Ok(next)
    }

    async fn process_pending_inbox(
        &self,
        tx: &DatabaseTransaction,
        hike: &mut tracker::Model,
        now: DateTimeUtc,
    ) -> anyhow::Result<()> {
        let rows = inbox::Entity::find()
            .filter(inbox::Column::ProcessedAt.is_null())
            .order_by_asc(inbox::Column::Id)
            .all(tx)
            .await?;
        for row in rows {
            self.process_inbox_row(tx, row, hike, now).await?;
        }
        Ok(())
    }

    async fn process_inbox_row(
        &self,
        tx: &DatabaseTransaction,
        row: inbox::Model,
        hike: &mut tracker::Model,
        now: DateTimeUtc,
    ) -> anyhow::Result<()> {
        let payload = row
            .payload
            .as_deref()
            .context("pending inbox payload missing")?;
        match row.source {
            IngressSource::Mail => {
                let parsed = mail::parse(
                    RawMail {
                        bytes: payload.to_vec(),
                        received_at: row.received_at,
                    },
                    &self.config,
                );
                if parsed.signal == Signal::Ok
                    && mail_ok_in_finished_cooldown(hike, parsed.event.event_at)?
                {
                    tracing::info!("mail OK ignored during finished-hike cooldown");
                } else {
                    let settings = self.load_settings(tx).await?;
                    self.apply_event(hike, parsed.signal, parsed.event, &settings, now)
                        .await?;
                }
            }
            IngressSource::Telegram => {
                if let Some((signal, event)) = self
                    .telegram
                    .handle_update(tx, payload, now, hike.phase)
                    .await?
                {
                    let settings = self.load_settings(tx).await?;
                    self.apply_event(hike, signal, event, &settings, now)
                        .await?;
                }
            }
        }
        self.telegram.sync_phase(tx, hike.phase).await?;
        let mut row = row.into_active_model();
        row.payload = Set(None);
        row.processed_at = Set(Some(now));
        row.update(tx).await?;
        Ok(())
    }

    async fn load_settings(&self, tx: &DatabaseTransaction) -> anyhow::Result<settings::Model> {
        settings::Entity::find_by_id(1)
            .one(tx)
            .await?
            .context("missing settings")
    }

    async fn apply_event(
        &self,
        hike: &mut tracker::Model,
        signal: Signal,
        event: Event,
        settings: &settings::Model,
        now: DateTimeUtc,
    ) -> anyhow::Result<()> {
        match signal {
            Signal::Ok => self.apply_ok(hike, event, settings, now).await,
            Signal::Finished => self.apply_finished(hike, event).await,
            Signal::Alert => self.apply_alert(hike, event, settings, now).await,
        }
    }

    async fn apply_ok(
        &self,
        hike: &mut tracker::Model,
        event: Event,
        settings: &settings::Model,
        now: DateTimeUtc,
    ) -> anyhow::Result<()> {
        if hike.phase == Phase::Active {
            self.refresh_active_hike(hike, event, settings, now).await?;
        } else {
            self.start_hike(hike, &event).await?;
            tracing::info!("started a new hike");
        }

        self.telegram.notify_ok(hike.phase).await?;
        tracing::info!("owner OK acknowledgement sent");
        Ok(())
    }

    async fn refresh_active_hike(
        &self,
        hike: &mut tracker::Model,
        event: Event,
        settings: &settings::Model,
        now: DateTimeUtc,
    ) -> anyhow::Result<()> {
        let at = event.event_at;
        if at <= hike.last_ok_at.context("active hike missing last OK")? {
            tracing::info!("OK ignored stale event");
            return Ok(());
        }
        if hike.owner_alerted {
            self.telegram.notify_recovery(&event).await?;
            tracing::info!("owner recovery message sent");
        }
        if hike.safety_alerted {
            self.telegram
                .notify_safety_recovery(hike, settings, &event, now)
                .await?;
            tracing::info!("safety recovery message sent");
        }
        if hike.owner_alerted || hike.safety_alerted {
            tracing::info!("OK resulted in contact recovery");
        } else {
            tracing::info!("OK refreshed active hike");
        }
        hike.last_ok_at = Some(at);
        hike.last_event_at = Some(hike.last_event_at.unwrap_or(at).max(at));
        hike.last_body = Some(event.body);
        hike.location = event.location;
        hike.owner_alerted = false;
        hike.safety_alerted = false;
        hike.owner_reminders_sent = 0;
        hike.safety_reminders_sent = 0;
        Ok(())
    }

    async fn apply_finished(&self, hike: &mut tracker::Model, event: Event) -> anyhow::Result<()> {
        let at = event.event_at;
        if hike.phase != Phase::Active {
            tracing::info!("finished ignored because no hike is active");
            return Ok(());
        }
        if at
            < hike
                .last_event_at
                .context("active hike missing last event")?
        {
            tracing::info!("finished ignored stale event");
            return Ok(());
        }
        self.telegram.notify_finished(&event).await?;
        tracing::info!("owner finished message sent");
        hike.phase = Phase::Finished;
        hike.finished_at = Some(at);
        hike.last_event_at = Some(at);
        hike.last_body = Some(event.body);
        hike.location = event.location;
        tracing::info!("finished resulted in completing the hike");
        Ok(())
    }

    async fn apply_alert(
        &self,
        hike: &mut tracker::Model,
        event: Event,
        settings: &settings::Model,
        now: DateTimeUtc,
    ) -> anyhow::Result<()> {
        if hike.phase != Phase::Active {
            self.start_hike(hike, &event).await?;
            self.telegram.notify_ok(hike.phase).await?;
            tracing::info!("started a new hike");
        }
        if hike.safety_alerted {
            tracing::info!("alert mail ignored because safety already alerted");
            return Ok(());
        }
        self.telegram
            .notify_alert(hike, settings, &event, now)
            .await?;
        tracing::info!("alert mail safety alert sent");
        hike.safety_alerted = true;
        // Preserve suppression of this interval's scheduled safety reminder.
        hike.safety_reminders_sent =
            i64::try_from(settings.safety_reminder_minutes.as_slice().len())?;
        tracing::info!("alert mail resulted in safety alert delivery");
        Ok(())
    }

    async fn start_hike(&self, hike: &mut tracker::Model, event: &Event) -> anyhow::Result<()> {
        self.telegram.notify_started(event).await?;
        tracing::info!("owner hike-start message sent");
        let at = event.event_at;
        *hike = tracker::Model {
            id: 1,
            phase: Phase::Active,
            started_at: Some(at),
            started_location: event.location.clone(),
            last_event_at: Some(at),
            last_ok_at: Some(at),
            last_body: Some(event.body.clone()),
            location: event.location.clone(),
            finished_at: None,
            owner_reminders_sent: 0,
            safety_reminders_sent: 0,
            owner_alerted: false,
            safety_alerted: false,
        };
        Ok(())
    }

    async fn reminders(
        &self,
        hike: &mut tracker::Model,
        settings: &settings::Model,
        now: DateTimeUtc,
    ) -> anyhow::Result<Option<DateTimeUtc>> {
        if hike.phase != Phase::Active {
            return Ok(None);
        }
        let last_ok = hike.last_ok_at.context("active hike missing last OK")?;
        let mut next: Option<DateTimeUtc> = None;
        for (audience, thresholds) in [
            (Audience::Owner, settings.owner_reminder_minutes.as_slice()),
            (
                Audience::Safety,
                settings.safety_reminder_minutes.as_slice(),
            ),
        ] {
            let sent = match audience {
                Audience::Owner => hike.owner_reminders_sent,
                Audience::Safety => hike.safety_reminders_sent,
            };
            for &minutes in thresholds.iter().skip(usize::try_from(sent)?) {
                let deadline = last_ok
                    .checked_add_signed(ChronoDuration::minutes(minutes))
                    .context("reminder deadline overflow")?;
                if deadline > now {
                    next = Some(next.map_or(deadline, |old| old.min(deadline)));
                    break;
                }
                self.telegram
                    .notify_reminder(hike, settings, audience, minutes, now)
                    .await?;
                match audience {
                    Audience::Owner => {
                        hike.owner_reminders_sent += 1;
                        hike.owner_alerted = true;
                        tracing::info!("owner reminder delivered");
                    }
                    Audience::Safety => {
                        hike.safety_reminders_sent += 1;
                        hike.safety_alerted = true;
                        tracing::info!("safety reminder delivered");
                    }
                }
            }
        }
        Ok(next)
    }
}

fn mail_ok_in_finished_cooldown(
    hike: &tracker::Model,
    event_at: DateTimeUtc,
) -> anyhow::Result<bool> {
    if hike.phase != Phase::Finished {
        return Ok(false);
    }
    Ok(event_at
        <= hike
            .finished_at
            .context("finished hike missing timestamp")?
            .checked_add_signed(FINISHED_COOLDOWN)
            .context("finished-hike cooldown overflow")?)
}

fn effective_now(requested_now: DateTimeUtc, last_tick_at: Option<DateTimeUtc>) -> DateTimeUtc {
    let requested_now = normalize(requested_now);
    last_tick_at.map_or(requested_now, |last| requested_now.max(normalize(last)))
}
