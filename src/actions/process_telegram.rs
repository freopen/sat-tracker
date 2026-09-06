use std::time::{Duration, SystemTime};

use durable_actions::{Action, HandlerError, async_trait};
use frankenstein::updates::{Update, UpdateContent};

use crate::{
    actions::{FinishedAction, OkAction},
    mail::Signal,
    state::{Event, TrackerState, push_bounded},
};

pub(crate) struct ProcessTelegram {
    pub(crate) owner_chat_id: i64,
}

#[async_trait]
impl Action for ProcessTelegram {
    const NAME: &'static str = "process-telegram";
    type State = TrackerState;
    type Parameters = Update;

    async fn run(&self, state: &mut TrackerState, update: Update) -> Result<(), HandlerError> {
        let update_id = update.update_id;
        let UpdateContent::Message(message) = update.content else {
            return Ok(());
        };
        if message.chat.id != self.owner_chat_id {
            return Ok(());
        }

        let Some(text) = message.text.as_deref() else {
            return Ok(());
        };
        let Some(signal) = classify_command(text) else {
            return Ok(());
        };

        let message_id = format!("telegram:{update_id}");
        if state.processed_ids.contains(&message_id) {
            return Ok(());
        }
        push_bounded(&mut state.processed_ids, message_id);

        let event = Event {
            message_id: None,
            event_at: telegram_time(message.date),
            body: text.to_owned(),
            location: None,
        };
        match signal {
            Signal::Ok => {
                OkAction::enqueue(&event)?;
            }
            Signal::Finished => {
                FinishedAction::enqueue(&event)?;
            }
            Signal::Alert => unreachable!("Telegram commands are classified"),
        }
        Ok(())
    }
}

fn classify_command(text: &str) -> Option<Signal> {
    let command = text.split_whitespace().next()?;
    if is_command(command, "/ok") {
        Some(Signal::Ok)
    } else if is_command(command, "/finished") {
        Some(Signal::Finished)
    } else {
        None
    }
}

fn is_command(value: &str, command: &str) -> bool {
    value == command
        || value
            .strip_prefix(command)
            .is_some_and(|suffix| suffix.starts_with('@') && suffix.len() > 1)
}

fn telegram_time(timestamp: u64) -> SystemTime {
    SystemTime::UNIX_EPOCH
        .checked_add(Duration::from_secs(timestamp))
        .unwrap_or_else(SystemTime::now)
}
