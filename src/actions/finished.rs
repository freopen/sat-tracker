use durable_actions::{Action, HandlerError, async_trait};

use crate::{
    actions::AlertAction,
    state::{Audience, Event, FinishedHike, HikeState, TrackerState, format_time, location_suffix},
    telegram::Telegram,
};

pub(crate) struct FinishedAction {
    pub(crate) telegram: Telegram,
}

#[async_trait]
impl Action for FinishedAction {
    const NAME: &'static str = "finished";
    type State = TrackerState;
    type Parameters = Event;

    async fn run(&self, state: &mut TrackerState, event: Event) -> Result<(), HandlerError> {
        let (finished, text) = {
            let HikeState::Active(hike) = &mut state.hike else {
                return Ok(());
            };
            if event.event_at < hike.last_event_at {
                return Ok(());
            }

            cancel_deadlines(hike)?;
            let text = format!(
                "InReach hike FINISHED at {}.{}\n\n{}",
                format_time(event.event_at),
                location_suffix(event.location.as_deref()),
                event.body
            );
            let finished = FinishedHike {
                event_at: event.event_at,
                body: event.body,
                location: event.location,
                owner_notified: false,
                safety_notified: false,
            };
            (finished, text)
        };

        self.telegram.send(Audience::Owner, &text).await?;
        self.telegram.send(Audience::Safety, &text).await?;
        state.hike = HikeState::Finished(FinishedHike {
            owner_notified: true,
            safety_notified: true,
            ..finished
        });
        Ok(())
    }
}

fn cancel_deadlines(hike: &mut crate::state::ActiveHike) -> Result<(), durable_actions::Error> {
    if let Some(id) = hike.owner_alert_action.take() {
        AlertAction::cancel(id)?;
    }
    if let Some(id) = hike.safety_alert_action.take() {
        AlertAction::cancel(id)?;
    }
    Ok(())
}
