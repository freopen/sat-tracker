use durable_actions::{Action, HandlerError, async_trait};
use tracing::{error, warn};

use crate::{
    actions::{AlertAction, FinishedAction, OkAction},
    config::Config,
    mail::{Signal, parse},
    state::{AlertSignal, Event, HikeState, RawMail, TrackerState, push_bounded},
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
        let message_id = parsed.event.message_id.clone();
        if message_id
            .as_ref()
            .is_some_and(|id| state.processed_ids.contains(id))
        {
            warn!(message_id = ?message_id, "mail event was already recorded");
            return Ok(());
        }
        if let Some(id) = message_id.clone() {
            push_bounded(&mut state.processed_ids, id);
        }

        let result: Result<(), HandlerError> = match parsed.signal {
            Signal::Ok => OkAction::enqueue(&parsed.event)
                .map(|_| ())
                .map_err(|error| Box::new(error) as HandlerError),
            Signal::Finished => FinishedAction::enqueue(&parsed.event)
                .map(|_| ())
                .map_err(|error| Box::new(error) as HandlerError),
            Signal::Alert => {
                Self::alert(state, parsed.event).map_err(|error| Box::new(error) as HandlerError)
            }
        };
        if let Err(error) = &result {
            error!(message_id = ?message_id, %error, "failed to handle mail event");
        }
        result
    }
}

impl ProcessMail {
    fn alert(state: &mut TrackerState, event: Event) -> Result<(), durable_actions::Error> {
        let expected_last_ok_at = match &state.hike {
            HikeState::Active(hike) => Some(hike.last_ok_at),
            HikeState::Idle | HikeState::Finished(_) => None,
        };
        AlertAction::enqueue(&AlertSignal::Unrecognized {
            event,
            expected_last_ok_at,
        })?;
        Ok(())
    }
}
