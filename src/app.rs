use std::path::Path;

use durable_actions::{Builder, Handle, Runner};

use crate::{
    actions::{AlertAction, FinishedAction, OkAction, ProcessMail, ProcessTelegram},
    config::Config,
    state::TrackerState,
    telegram::Telegram,
};

pub struct App {
    pub(crate) handle: Handle,
    telegram: Telegram,
}

impl App {
    pub async fn start(config: Config, path: impl AsRef<Path>) -> anyhow::Result<(Self, Runner)> {
        let owner_chat_id = config.owner_chat_id;
        let telegram = Telegram::new(&config).await?;
        let builder = Builder::new(TrackerState::default())
            .register(ProcessMail { config })
            .register(ProcessTelegram { owner_chat_id })
            .register(OkAction {
                telegram: telegram.clone(),
            })
            .register(FinishedAction {
                telegram: telegram.clone(),
            })
            .register(AlertAction {
                telegram: telegram.clone(),
            });

        let (handle, runner) = builder.open(path).await?;
        telegram.start_listener(handle.clone());
        Ok((Self { handle, telegram }, runner))
    }

    pub fn shutdown(&self) {
        self.telegram.abort_listener();
        self.handle.shutdown();
    }
}

impl Drop for App {
    fn drop(&mut self) {
        self.telegram.abort_listener();
    }
}
