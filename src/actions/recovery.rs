use durable_actions::{Action, HandlerError, async_trait};

use crate::{
    state::{Audience, AudienceEventKey, HikeState, TrackerState, format_time, location_suffix},
    telegram::Telegram,
};

pub(crate) struct NotifyRecovery {
    pub(crate) telegram: Telegram,
}

#[async_trait]
impl Action for NotifyRecovery {
    const NAME: &'static str = "notify-recovery";
    type State = TrackerState;
    type Parameters = AudienceEventKey;

    async fn run(
        &self,
        state: &mut TrackerState,
        key: AudienceEventKey,
    ) -> Result<(), HandlerError> {
        let HikeState::Active(hike) = &mut state.hike else {
            return Ok(());
        };
        let was_alerted = match key.audience {
            Audience::Owner => hike.owner_alerted,
            Audience::Safety => hike.safety_alerted,
        };
        if hike.last_ok_at != key.event_at || !was_alerted {
            return Ok(());
        }
        let text = format!(
            "InReach contact resumed at {}.{}",
            format_time(hike.last_ok_at),
            location_suffix(hike.location.as_deref())
        );
        self.telegram.send(key.audience, &text).await?;
        match key.audience {
            Audience::Owner => hike.owner_alerted = false,
            Audience::Safety => hike.safety_alerted = false,
        }
        Ok(())
    }
}
