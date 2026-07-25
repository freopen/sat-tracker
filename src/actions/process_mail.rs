use durable_actions::{Action, HandlerError, async_trait};

use crate::{
    actions::{DeliverAlert, NotifyFinished, NotifyRecovery, NotifyStarted},
    config::Config,
    mail::{Classification, parse},
    state::{
        ActiveHike, AlertParameters, Audience, AudienceEventKey, Event, EventKey,
        FINISHED_COOLDOWN, FinishedHike, HikeState, OWNER_AFTER, RawMail, SAFETY_AFTER,
        TrackerState, format_time, location_suffix, push_bounded,
    },
};

pub(crate) struct ProcessMail {
    pub(crate) config: Config,
}

#[async_trait]
impl Action for ProcessMail {
    const NAME: &'static str = "process-mail";
    type State = TrackerState;
    type Parameters = RawMail;

    async fn run(&self, state: &mut TrackerState, raw: RawMail) -> Result<(), HandlerError> {
        let parsed = parse(raw, &self.config);
        if parsed
            .event
            .message_id
            .as_ref()
            .is_some_and(|id| state.processed_ids.contains(id))
        {
            return Ok(());
        }
        if let Some(id) = parsed.event.message_id.clone() {
            push_bounded(&mut state.processed_ids, id);
        }

        match parsed.classification {
            Classification::Ok => self.record_ok(state, parsed.event)?,
            Classification::Finished => self.record_finished(state, parsed.event)?,
            Classification::Unrecognized => {
                self.alert_unrecognized(state, parsed.event)?;
            }
        }
        Ok(())
    }
}

impl ProcessMail {
    fn alert_unrecognized(
        &self,
        state: &mut TrackerState,
        event: Event,
    ) -> Result<(), durable_actions::Error> {
        let HikeState::Active(hike) = &state.hike else {
            return Ok(());
        };
        DeliverAlert::enqueue(&AlertParameters {
            expected_last_ok_at: hike.last_ok_at,
            audience: Audience::Safety,
            payload: format!(
                "SAFETY ALERT: unrecognized InReach message\nEvent time: {}\n\n{}",
                format_time(event.event_at),
                event.body
            ),
        })?;
        Ok(())
    }

    fn record_ok(&self, state: &mut TrackerState, event: Event) -> Result<(), HandlerError> {
        match &mut state.hike {
            HikeState::Active(hike) => {
                if event.event_at <= hike.last_ok_at {
                    return Ok(());
                }
                Self::cancel_deadlines(hike)?;
                let owner_recovery = hike.owner_alerted;
                let safety_recovery = hike.safety_alerted;
                hike.last_ok_at = event.event_at;
                hike.last_event_at = hike.last_event_at.max(event.event_at);
                hike.last_body = event.body;
                hike.location = event.location;
                let key = EventKey {
                    event_at: hike.last_ok_at,
                };
                if owner_recovery {
                    NotifyRecovery::enqueue(&AudienceEventKey {
                        event_at: key.event_at,
                        audience: Audience::Owner,
                    })?;
                }
                if safety_recovery {
                    NotifyRecovery::enqueue(&AudienceEventKey {
                        event_at: key.event_at,
                        audience: Audience::Safety,
                    })?;
                }
                Self::schedule_deadlines(hike)?;
            }
            HikeState::Finished(finished)
                if event.event_at <= finished.event_at + FINISHED_COOLDOWN => {}
            HikeState::Idle | HikeState::Finished(_) => {
                let key = EventKey {
                    event_at: event.event_at,
                };
                let mut hike = ActiveHike {
                    started_at: event.event_at,
                    started_location: event.location.clone(),
                    last_event_at: event.event_at,
                    last_ok_at: event.event_at,
                    last_body: event.body,
                    location: event.location,
                    owner_started_notified: false,
                    owner_alerted: false,
                    safety_alerted: false,
                    owner_alert_action: None,
                    safety_alert_action: None,
                };
                NotifyStarted::enqueue(&key)?;
                Self::schedule_deadlines(&mut hike)?;
                state.hike = HikeState::Active(hike);
            }
        }
        Ok(())
    }

    fn record_finished(&self, state: &mut TrackerState, event: Event) -> Result<(), HandlerError> {
        let HikeState::Active(hike) = &mut state.hike else {
            return Ok(());
        };
        if event.event_at < hike.last_event_at {
            return Ok(());
        }
        Self::cancel_deadlines(hike)?;
        let key = EventKey {
            event_at: event.event_at,
        };
        state.hike = HikeState::Finished(FinishedHike {
            event_at: event.event_at,
            body: event.body,
            location: event.location,
            owner_notified: false,
            safety_notified: false,
        });
        NotifyFinished::enqueue(&AudienceEventKey {
            event_at: key.event_at,
            audience: Audience::Owner,
        })?;
        NotifyFinished::enqueue(&AudienceEventKey {
            event_at: key.event_at,
            audience: Audience::Safety,
        })?;
        Ok(())
    }

    fn schedule_deadlines(hike: &mut ActiveHike) -> Result<(), durable_actions::Error> {
        hike.owner_alert_action = Some(DeliverAlert::enqueue_at(
            &AlertParameters {
                expected_last_ok_at: hike.last_ok_at,
                audience: Audience::Owner,
                payload: Self::overdue_payload(hike, Audience::Owner),
            },
            hike.last_ok_at + OWNER_AFTER,
        )?);
        hike.safety_alert_action = Some(DeliverAlert::enqueue_at(
            &AlertParameters {
                expected_last_ok_at: hike.last_ok_at,
                audience: Audience::Safety,
                payload: Self::overdue_payload(hike, Audience::Safety),
            },
            hike.last_ok_at + SAFETY_AFTER,
        )?);
        Ok(())
    }

    fn overdue_payload(hike: &ActiveHike, audience: Audience) -> String {
        let (prefix, after) = match audience {
            Audience::Owner => ("No InReach OK", OWNER_AFTER),
            Audience::Safety => ("SAFETY ALERT: no InReach OK", SAFETY_AFTER),
        };
        format!(
            "{prefix} for {} minutes. Last contact: {}.{}\n\n{}",
            after.as_secs() / 60,
            format_time(hike.last_ok_at),
            location_suffix(hike.location.as_deref()),
            hike.last_body
        )
    }

    fn cancel_deadlines(hike: &mut ActiveHike) -> Result<(), durable_actions::Error> {
        if let Some(id) = hike.owner_alert_action.take() {
            DeliverAlert::cancel(id)?;
        }
        if let Some(id) = hike.safety_alert_action.take() {
            DeliverAlert::cancel(id)?;
        }
        Ok(())
    }
}
