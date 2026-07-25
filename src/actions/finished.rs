use durable_actions::{Action, HandlerError, async_trait};

use crate::{
    state::{Audience, AudienceEventKey, HikeState, TrackerState, format_time, location_suffix},
    telegram::Telegram,
};

pub(crate) struct NotifyFinished {
    pub(crate) telegram: Telegram,
}

#[async_trait]
impl Action for NotifyFinished {
    const NAME: &'static str = "notify-finished";
    type State = TrackerState;
    type Parameters = AudienceEventKey;

    async fn run(
        &self,
        state: &mut TrackerState,
        key: AudienceEventKey,
    ) -> Result<(), HandlerError> {
        let HikeState::Finished(finished) = &mut state.hike else {
            return Ok(());
        };
        let already_notified = match key.audience {
            Audience::Owner => finished.owner_notified,
            Audience::Safety => finished.safety_notified,
        };
        if finished.event_at != key.event_at || already_notified {
            return Ok(());
        }
        let text = format!(
            "InReach hike FINISHED at {}.{}\n\n{}",
            format_time(finished.event_at),
            location_suffix(finished.location.as_deref()),
            finished.body
        );
        self.telegram.send(key.audience, &text).await?;
        match key.audience {
            Audience::Owner => finished.owner_notified = true,
            Audience::Safety => finished.safety_notified = true,
        }
        Ok(())
    }
}
