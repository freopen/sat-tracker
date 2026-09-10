use crate::{
    App, db,
    entity::{inbox, runtime, tracker},
    mail::{self, Signal},
    messages,
    state::{
        Audience, DateTimeUtc, Event, FINISHED_COOLDOWN, IngressSource, OWNER_MINUTES, Phase,
        RawMail, SAFETY_MINUTES, normalize,
    },
    telegram::{Command, command},
    version::build_info,
};
use anyhow::Context;
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use frankenstein::updates::{Update, UpdateContent};
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
        let mut runtime = runtime::Entity::find_by_id(1)
            .one(tx)
            .await?
            .context("missing runtime")?;
        let now = effective_now(requested_now, runtime.last_tick_at);
        let mut hike = tracker::Entity::find_by_id(1)
            .one(tx)
            .await?
            .context("missing tracker")?;

        self.process_pending_inbox(tx, &mut hike, now).await?;
        let next = self.reminders(&mut hike, now).await?;
        hike.into_active_model().reset_all().update(tx).await?;
        runtime.last_tick_at = Some(now);
        runtime.next_tick_at = next;
        runtime.into_active_model().reset_all().update(tx).await?;
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
                self.apply_event(hike, parsed.signal, parsed.event).await?;
            }
            IngressSource::Telegram => {
                self.process_telegram(payload, now, hike).await?;
            }
        }
        let mut row = row.into_active_model();
        row.payload = Set(None);
        row.processed_at = Set(Some(now));
        row.update(tx).await?;
        Ok(())
    }

    async fn process_telegram(
        &self,
        payload: &[u8],
        now: DateTimeUtc,
        hike: &mut tracker::Model,
    ) -> anyhow::Result<()> {
        let update: Update = serde_json::from_slice(payload)?;
        let UpdateContent::Message(message) = update.content else {
            tracing::info!("ignored Telegram update without a message");
            return Ok(());
        };
        if message.chat.id != self.config.owner_chat_id {
            tracing::info!("ignored Telegram message from a non-owner chat");
            return Ok(());
        }
        let Some(text) = message.text.as_deref() else {
            tracing::info!("ignored Telegram message without text");
            return Ok(());
        };
        let Some(command) = command(text) else {
            tracing::info!("ignored unknown Telegram command");
            return Ok(());
        };
        if command == Command::Version {
            self.telegram
                .send(Audience::Owner, &build_info().message())
                .await?;
            tracing::info!("Telegram version command handled");
            return Ok(());
        }
        let event = Event {
            event_at: telegram_event_time(message.date, now),
            body: text.to_owned(),
            location: None,
        };
        self.apply_event(
            hike,
            if command == Command::Ok {
                Signal::Ok
            } else {
                Signal::Finished
            },
            event,
        )
        .await
    }

    async fn apply_event(
        &self,
        hike: &mut tracker::Model,
        signal: Signal,
        event: Event,
    ) -> anyhow::Result<()> {
        match signal {
            Signal::Ok => self.apply_ok(hike, event).await,
            Signal::Finished => self.apply_finished(hike, event).await,
            Signal::Alert => self.apply_alert(hike, event).await,
        }
    }

    async fn apply_ok(&self, hike: &mut tracker::Model, event: Event) -> anyhow::Result<()> {
        let at = event.event_at;
        if hike.phase == Phase::Active {
            return self.refresh_active_hike(hike, event).await;
        }
        if hike.phase == Phase::Finished
            && at
                <= hike
                    .finished_at
                    .context("finished hike missing timestamp")?
                    .checked_add_signed(FINISHED_COOLDOWN)
                    .context("finished-hike cooldown overflow")?
        {
            tracing::info!("OK ignored during finished-hike cooldown");
            return Ok(());
        }
        self.start_hike(hike, &event).await?;
        tracing::info!("started a new hike");
        Ok(())
    }

    async fn refresh_active_hike(
        &self,
        hike: &mut tracker::Model,
        event: Event,
    ) -> anyhow::Result<()> {
        let at = event.event_at;
        if at <= hike.last_ok_at.context("active hike missing last OK")? {
            tracing::info!("OK ignored stale event");
            return Ok(());
        }
        let recovery = messages::recovery(&event)?;
        let was_alerted = hike.owner_alerted || hike.safety_alerted;
        if hike.owner_alerted {
            self.telegram.send(Audience::Owner, &recovery).await?;
            tracing::info!("owner recovery message sent");
        }
        if hike.safety_alerted {
            self.telegram.send(Audience::Safety, &recovery).await?;
            tracing::info!("safety recovery message sent");
        }
        if was_alerted {
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
        let text = messages::finished(&event)?;
        self.telegram.send(Audience::Owner, &text).await?;
        tracing::info!("owner finished message sent");
        self.telegram.send(Audience::Safety, &text).await?;
        tracing::info!("safety finished message sent");
        hike.phase = Phase::Finished;
        hike.finished_at = Some(at);
        hike.last_event_at = Some(at);
        hike.last_body = Some(event.body);
        hike.location = event.location;
        tracing::info!("finished resulted in completing the hike");
        Ok(())
    }

    async fn apply_alert(&self, hike: &mut tracker::Model, event: Event) -> anyhow::Result<()> {
        if hike.phase != Phase::Active {
            self.start_hike(hike, &event).await?;
            tracing::info!("started a new hike");
        }
        if hike.safety_alerted {
            tracing::info!("unrecognized alert ignored because safety already alerted");
            return Ok(());
        }
        self.telegram
            .send(Audience::Safety, &messages::unrecognized(&event)?)
            .await?;
        tracing::info!("unrecognized mail safety alert sent");
        hike.safety_alerted = true;
        // Preserve suppression of this interval's scheduled safety reminder.
        hike.safety_reminders_sent = SAFETY_MINUTES.len() as i64;
        tracing::info!("unrecognized mail resulted in safety alert delivery");
        Ok(())
    }
    async fn start_hike(&self, hike: &mut tracker::Model, event: &Event) -> anyhow::Result<()> {
        self.telegram
            .send(Audience::Owner, &messages::started(event)?)
            .await?;
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
        now: DateTimeUtc,
    ) -> anyhow::Result<Option<DateTimeUtc>> {
        if hike.phase != Phase::Active {
            return Ok(None);
        }
        let last_ok = hike.last_ok_at.context("active hike missing last OK")?;
        let mut next: Option<DateTimeUtc> = None;
        for (audience, thresholds) in [
            (Audience::Owner, OWNER_MINUTES),
            (Audience::Safety, SAFETY_MINUTES),
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
                    .send(audience, &messages::reminder(hike, audience, minutes))
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

fn effective_now(requested_now: DateTimeUtc, last_tick_at: Option<DateTimeUtc>) -> DateTimeUtc {
    let requested_now = normalize(requested_now);
    last_tick_at.map_or(requested_now, |last| requested_now.max(normalize(last)))
}

fn telegram_event_time(message_date: u64, fallback: DateTimeUtc) -> DateTimeUtc {
    i64::try_from(message_date)
        .ok()
        .and_then(|seconds| DateTime::<Utc>::from_timestamp(seconds, 0))
        .map(normalize)
        .unwrap_or(fallback)
}
