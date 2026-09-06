use durable_actions::{Action, HandlerError, async_trait};

use crate::{
    state::{AlertParameters, AlertSignal, Audience, HikeState, TrackerState, format_time},
    telegram::Telegram,
};

pub(crate) struct AlertAction {
    pub(crate) telegram: Telegram,
}

#[async_trait]
impl Action for AlertAction {
    const NAME: &'static str = "alert";
    type State = TrackerState;
    type Parameters = AlertSignal;

    async fn run(&self, state: &mut TrackerState, signal: AlertSignal) -> Result<(), HandlerError> {
        let alert = match signal {
            AlertSignal::Unrecognized {
                event,
                expected_last_ok_at: Some(expected_last_ok_at),
            } => AlertParameters {
                expected_last_ok_at,
                audience: Audience::Safety,
                payload: format!(
                    "SAFETY ALERT: unrecognized InReach message\nEvent time: {}\n\n{}",
                    format_time(event.event_at),
                    event.body
                ),
            },
            AlertSignal::Unrecognized {
                expected_last_ok_at: None,
                ..
            } => return Ok(()),
            AlertSignal::Overdue(alert) => alert,
        };
        self.deliver(state, alert).await
    }
}

impl AlertAction {
    async fn deliver(
        &self,
        state: &mut TrackerState,
        alert: AlertParameters,
    ) -> Result<(), HandlerError> {
        let HikeState::Active(hike) = &mut state.hike else {
            return Ok(());
        };
        let already_alerted = match alert.audience {
            Audience::Owner => hike.owner_alerted,
            Audience::Safety => hike.safety_alerted,
        };
        if hike.last_ok_at != alert.expected_last_ok_at || already_alerted {
            return Ok(());
        }

        self.telegram.send(alert.audience, &alert.payload).await?;
        match alert.audience {
            Audience::Owner => {
                if let Some(id) = hike.owner_alert_action.take() {
                    Self::cancel(id)?;
                }
                hike.owner_alerted = true;
            }
            Audience::Safety => {
                if let Some(id) = hike.safety_alert_action.take() {
                    Self::cancel(id)?;
                }
                hike.safety_alerted = true;
            }
        }
        Ok(())
    }
}
