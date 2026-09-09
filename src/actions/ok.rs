use durable_actions::{Action, HandlerError, async_trait};

use crate::{
    actions::AlertAction,
    state::{
        ActiveHike, AlertParameters, AlertSignal, Audience, Event, FINISHED_COOLDOWN, HikeState,
        OWNER_AFTER, SAFETY_AFTER, TrackerState, format_time, location_suffix,
    },
    telegram::Telegram,
};
use tracing::{info, warn};

pub(crate) struct OkAction {
    pub(crate) telegram: Telegram,
}

#[async_trait]
impl Action for OkAction {
    const NAME: &'static str = "ok";
    type State = TrackerState;
    type Parameters = Event;

    async fn run(&self, state: &mut TrackerState, event: Event) -> Result<(), HandlerError> {
        if matches!(&state.hike, HikeState::Active(_)) {
            return self.refresh_active(state, event).await;
        }
        if matches!(
            &state.hike,
            HikeState::Finished(finished)
                if event.event_at <= finished.event_at + FINISHED_COOLDOWN
        ) {
            warn!(
                event_at = %format_time(event.event_at),
                "OK action ignored during finished-hike cooldown"
            );
            return Ok(());
        }
        self.start_hike(state, event).await
    }
}

impl OkAction {
    async fn refresh_active(
        &self,
        state: &mut TrackerState,
        event: Event,
    ) -> Result<(), HandlerError> {
        let (owner_recovery, safety_recovery, event_at, location) = {
            let HikeState::Active(hike) = &mut state.hike else {
                return Ok(());
            };
            if event.event_at <= hike.last_ok_at {
                warn!(
                    event_at = %format_time(event.event_at),
                    last_ok_at = %format_time(hike.last_ok_at),
                    "OK action ignored stale event"
                );
                return Ok(());
            }

            cancel_deadlines(hike)?;
            let owner_recovery = hike.owner_alerted;
            let safety_recovery = hike.safety_alerted;
            hike.last_ok_at = event.event_at;
            hike.last_event_at = hike.last_event_at.max(event.event_at);
            hike.last_body = event.body;
            hike.location = event.location;
            hike.owner_alerted = false;
            hike.safety_alerted = false;
            (
                owner_recovery,
                safety_recovery,
                hike.last_ok_at,
                hike.location.clone(),
            )
        };

        let text = format!(
            "InReach contact resumed at {}.{}",
            format_time(event_at),
            location_suffix(location.as_deref())
        );
        if owner_recovery {
            self.telegram.send(Audience::Owner, &text).await?;
        }
        if safety_recovery {
            self.telegram.send(Audience::Safety, &text).await?;
        }

        let HikeState::Active(hike) = &mut state.hike else {
            return Ok(());
        };
        schedule_deadlines(hike)?;
        if owner_recovery || safety_recovery {
            info!(
                event_at = %format_time(event_at),
                owner_recovery,
                safety_recovery,
                "OK action resulted in contact recovery"
            );
        } else {
            info!(
                event_at = %format_time(event_at),
                "OK action refreshed active hike"
            );
        }
        Ok(())
    }

    pub(super) async fn start_hike(
        &self,
        state: &mut TrackerState,
        event: Event,
    ) -> Result<(), HandlerError> {
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
        let text = format!(
            "InReach hike started at {}.{}",
            format_time(hike.started_at),
            location_suffix(hike.started_location.as_deref())
        );
        self.telegram.send(Audience::Owner, &text).await?;
        hike.owner_started_notified = true;
        schedule_deadlines(&mut hike)?;
        state.hike = HikeState::Active(hike);
        info!(
            event_at = %format_time(event.event_at),
            "started a new hike"
        );
        Ok(())
    }
}

fn schedule_deadlines(hike: &mut ActiveHike) -> Result<(), durable_actions::Error> {
    hike.owner_alert_action = Some(AlertAction::enqueue_at(
        &AlertSignal::Overdue(AlertParameters {
            expected_last_ok_at: hike.last_ok_at,
            audience: Audience::Owner,
            payload: overdue_payload(hike, Audience::Owner),
        }),
        hike.last_ok_at + OWNER_AFTER,
    )?);
    hike.safety_alert_action = Some(AlertAction::enqueue_at(
        &AlertSignal::Overdue(AlertParameters {
            expected_last_ok_at: hike.last_ok_at,
            audience: Audience::Safety,
            payload: overdue_payload(hike, Audience::Safety),
        }),
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
        AlertAction::cancel(id)?;
    }
    if let Some(id) = hike.safety_alert_action.take() {
        AlertAction::cancel(id)?;
    }
    Ok(())
}
