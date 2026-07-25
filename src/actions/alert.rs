use durable_actions::{Action, HandlerError, async_trait};

use crate::{
    state::{AlertParameters, Audience, HikeState, TrackerState},
    telegram::Telegram,
};

pub(crate) struct DeliverAlert {
    pub(crate) telegram: Telegram,
}

#[async_trait]
impl Action for DeliverAlert {
    const NAME: &'static str = "deliver-alert";
    type State = TrackerState;
    type Parameters = AlertParameters;

    async fn run(
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
