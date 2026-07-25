use durable_actions::{Action, HandlerError, async_trait};

use crate::{
    state::{Audience, EventKey, HikeState, TrackerState, format_time, location_suffix},
    telegram::Telegram,
};

pub(crate) struct NotifyStarted {
    pub(crate) telegram: Telegram,
}

#[async_trait]
impl Action for NotifyStarted {
    const NAME: &'static str = "notify-started";
    type State = TrackerState;
    type Parameters = EventKey;

    async fn run(&self, state: &mut TrackerState, key: EventKey) -> Result<(), HandlerError> {
        let HikeState::Active(hike) = &mut state.hike else {
            return Ok(());
        };
        if hike.started_at != key.event_at || hike.owner_started_notified {
            return Ok(());
        }
        let text = format!(
            "InReach hike started at {}.{}",
            format_time(hike.started_at),
            location_suffix(hike.started_location.as_deref())
        );
        self.telegram.send(Audience::Owner, &text).await?;
        hike.owner_started_notified = true;
        Ok(())
    }
}
