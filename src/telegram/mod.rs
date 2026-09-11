mod common;
mod lifecycle;
mod reply;
mod template;
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
    templates: template::Renderer,
}

impl Telegram {
    pub(crate) fn new(
        config: &Config,
        settings: &crate::entity::settings::Model,
    ) -> anyhow::Result<Self> {
        Ok(Self {
            client: lifecycle::new_client(config)?,
            templates: template::Renderer::new(
                &settings.safety_alert_template,
                &settings.safety_recovery_template,
            )?,
        })
    }

    pub(crate) fn sync_templates(
        &self,
        settings: &crate::entity::settings::Model,
    ) -> anyhow::Result<()> {
        self.templates.sync_sources(
            &settings.safety_alert_template,
            &settings.safety_recovery_template,
        )
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
        update::handle_update(&self.client, &self.templates, tx, payload, now, phase).await
    }

    pub(crate) async fn notify_started(&self, event: &Event) -> anyhow::Result<()> {
        reply::notify_started(&self.client, &self.templates, event).await
    }

    pub(crate) async fn notify_ok(&self, phase: Phase) -> anyhow::Result<()> {
        reply::notify_ok(&self.client, &self.templates, phase).await
    }

    pub(crate) async fn notify_recovery(&self, event: &Event) -> anyhow::Result<()> {
        reply::notify_recovery(&self.client, &self.templates, event).await
    }

    pub(crate) async fn notify_safety_recovery(
        &self,
        hike: &crate::entity::tracker::Model,
        settings: &crate::entity::settings::Model,
        event: &Event,
        at: DateTimeUtc,
    ) -> anyhow::Result<()> {
        reply::notify_safety_recovery(&self.client, &self.templates, hike, settings, event, at)
            .await
    }

    pub(crate) async fn notify_finished(&self, event: &Event) -> anyhow::Result<()> {
        reply::notify_finished(&self.client, &self.templates, event).await
    }

    pub(crate) async fn notify_alert(
        &self,
        hike: &crate::entity::tracker::Model,
        settings: &crate::entity::settings::Model,
        event: &Event,
        at: DateTimeUtc,
    ) -> anyhow::Result<()> {
        reply::notify_alert(&self.client, &self.templates, hike, settings, event, at).await
    }

    pub(crate) async fn notify_reminder(
        &self,
        hike: &crate::entity::tracker::Model,
        settings: &crate::entity::settings::Model,
        audience: Audience,
        minutes: i64,
        at: DateTimeUtc,
    ) -> anyhow::Result<()> {
        reply::notify_reminder(
            &self.client,
            &self.templates,
            hike,
            settings,
            audience,
            minutes,
            at,
        )
        .await
    }
}

pub(crate) async fn listen(app: Arc<App>) -> anyhow::Result<()> {
    let client = &app.telegram.client;
    let app_for_listener = app.clone();
    lifecycle::listen(client, app_for_listener).await
}

pub(crate) use common::{retry_delay, safe_error};
