use std::time::{Duration, SystemTime};

use durable_actions::{Action, HandlerError, async_trait};
use frankenstein::updates::{Update, UpdateContent};

use crate::{
    actions::{FinishedAction, OkAction},
    state::{Audience, Event, TrackerState, push_bounded},
    telegram::Telegram,
    version::build_info,
};

pub(crate) struct ProcessTelegram {
    pub(crate) owner_chat_id: i64,
    pub(crate) telegram: Telegram,
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
        let Some(command) = classify_command(text) else {
            return Ok(());
        };

        let message_id = format!("telegram:{update_id}");
        if state.processed_ids.contains(&message_id) {
            return Ok(());
        }
        push_bounded(&mut state.processed_ids, message_id);

        match command {
            Command::Version => {
                let build = build_info();
                let response = build.message();
                self.telegram.send(Audience::Owner, &response).await?;
            }
            Command::Ok => {
                let event = telegram_event(message.date, text);
                OkAction::enqueue(&event)?;
            }
            Command::Finished => {
                let event = telegram_event(message.date, text);
                FinishedAction::enqueue(&event)?;
            }
        }
        Ok(())
    }
}

enum Command {
    Ok,
    Finished,
    Version,
}

fn classify_command(text: &str) -> Option<Command> {
    let command = text.split_whitespace().next()?;
    if is_command(command, "/ok") {
        Some(Command::Ok)
    } else if is_command(command, "/finished") {
        Some(Command::Finished)
    } else if is_command(command, "/version") {
        Some(Command::Version)
    } else {
        None
    }
}

fn telegram_event(timestamp: u64, text: &str) -> Event {
    Event {
        message_id: None,
        event_at: telegram_time(timestamp),
        body: text.to_owned(),
        location: None,
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
