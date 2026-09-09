use durable_actions::{Action, HandlerError, async_trait};

use crate::{
    actions::OkAction,
    state::{AlertParameters, AlertSignal, Audience, HikeState, TrackerState, format_time},
    telegram::Telegram,
};
use tracing::{info, warn};

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
            AlertSignal::Unrecognized { event } => {
                let expected_last_ok_at = if let HikeState::Active(hike) = &state.hike {
                    hike.last_ok_at
                } else {
                    OkAction {
                        telegram: self.telegram.clone(),
                    }
                    .start_hike(state, event.clone())
                    .await?;
                    event.event_at
                };
                AlertParameters {
                    expected_last_ok_at,
                    audience: Audience::Safety,
                    payload: format!(
                        "SAFETY ALERT: unrecognized InReach message\nEvent time: {}\n\n{}",
                        format_time(event.event_at),
                        event.body
                    ),
                }
            }
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
            warn!(
                audience = ?alert.audience,
                "alert action ignored because no hike is active"
            );
            return Ok(());
        };
        let already_alerted = match alert.audience {
            Audience::Owner => hike.owner_alerted,
            Audience::Safety => hike.safety_alerted,
        };
        if hike.last_ok_at != alert.expected_last_ok_at {
            warn!(
                audience = ?alert.audience,
                expected_last_ok_at = %format_time(alert.expected_last_ok_at),
                last_ok_at = %format_time(hike.last_ok_at),
                "alert action ignored stale alert"
            );
            return Ok(());
        }
        if already_alerted {
            warn!(
                audience = ?alert.audience,
                "alert action ignored because alert was already delivered"
            );
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
        info!(
            audience = ?alert.audience,
            "alert action resulted in alert delivery"
        );
        Ok(())
    }
}
