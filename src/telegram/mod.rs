mod common;
mod lifecycle;
mod reply;
mod update;

use crate::{
    App, Config,
    state::{Audience, DateTimeUtc, Event, Phase, Signal},
};
use frankenstein::updates::Update;
use sea_orm::DatabaseTransaction;
use std::sync::Arc;

pub(crate) struct Telegram {
    client: common::Client,
}

impl Telegram {
    pub(crate) fn new(config: &Config) -> anyhow::Result<Self> {
        Ok(Self {
            client: lifecycle::new_client(config)?,
        })
    }

    pub(crate) async fn configure(&self, url: &str) -> anyhow::Result<()> {
        lifecycle::configure(&self.client, url).await
    }

    pub(crate) async fn accept_update(
        &self,
        tx: &DatabaseTransaction,
        update: Update,
        received_at: DateTimeUtc,
        polled: bool,
    ) -> anyhow::Result<()> {
        lifecycle::accept_update(tx, update, received_at, polled).await
    }

    pub(crate) async fn sync_phase(
        &self,
        tx: &DatabaseTransaction,
        phase: Phase,
    ) -> anyhow::Result<()> {
        common::sync_phase(tx, phase).await
    }

    pub(crate) async fn handle_update(
        &self,
        tx: &DatabaseTransaction,
        payload: &[u8],
        now: DateTimeUtc,
        phase: Phase,
    ) -> anyhow::Result<Option<(Signal, Event)>> {
        update::handle_update(&self.client, tx, payload, now, phase).await
    }

    pub(crate) async fn notify_started(&self, event: &Event) -> anyhow::Result<()> {
        reply::notify_started(&self.client, event).await
    }

    pub(crate) async fn notify_ok(&self, phase: Phase) -> anyhow::Result<()> {
        reply::notify_ok(&self.client, phase).await
    }

    pub(crate) async fn notify_recovery(
        &self,
        audience: Audience,
        event: &Event,
    ) -> anyhow::Result<()> {
        reply::notify_recovery(&self.client, audience, event).await
    }

    pub(crate) async fn notify_finished(
        &self,
        audience: Audience,
        event: &Event,
    ) -> anyhow::Result<()> {
        reply::notify_finished(&self.client, audience, event).await
    }

    pub(crate) async fn notify_unrecognized(&self, event: &Event) -> anyhow::Result<()> {
        reply::notify_unrecognized(&self.client, event).await
    }

    pub(crate) async fn notify_reminder(
        &self,
        hike: &crate::entity::tracker::Model,
        audience: Audience,
        minutes: i64,
    ) -> anyhow::Result<()> {
        reply::notify_reminder(&self.client, hike, audience, minutes).await
    }
}

pub(crate) async fn listen(app: Arc<App>) -> anyhow::Result<()> {
    let client = &app.telegram.client;
    let app_for_listener = app.clone();
    lifecycle::listen(client, app_for_listener).await
}

pub(crate) use common::{retry_delay, safe_error};
