use std::path::Path;

use durable_actions::{Builder, Handle, Runner};

use crate::{
    actions::{DeliverAlert, NotifyFinished, NotifyRecovery, NotifyStarted, ProcessMail},
    config::Config,
    state::TrackerState,
    telegram::Telegram,
};

pub struct App {
    pub(crate) handle: Handle,
}

impl App {
    pub async fn start(config: Config, path: impl AsRef<Path>) -> anyhow::Result<(Self, Runner)> {
        let telegram = Telegram::new(&config)?;
        let builder = Builder::new(TrackerState::default())
            .register(ProcessMail { config })
            .register(NotifyStarted {
                telegram: telegram.clone(),
            })
            .register(DeliverAlert {
                telegram: telegram.clone(),
            })
            .register(NotifyRecovery {
                telegram: telegram.clone(),
            })
            .register(NotifyFinished { telegram });

        let (handle, runner) = builder.open(path).await?;
        Ok((Self { handle }, runner))
    }

    pub fn shutdown(&self) {
        self.handle.shutdown();
    }
}
