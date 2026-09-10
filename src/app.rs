use crate::{
    Config, db,
    entity::{inbox, runtime},
    state::{DateTimeUtc, IngressSource, normalize},
    telegram::Telegram,
};
use anyhow::Context;
use frankenstein::updates::Update;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, DatabaseTransaction, EntityTrait,
    QueryFilter, Set,
};
use std::path::Path;
use tokio::sync::{Notify, watch};

pub struct App {
    pub(crate) db: DatabaseConnection,
    pub(crate) config: Config,
    pub(crate) telegram: Telegram,
    pub(crate) wake: Notify,
    pub(crate) shutdown: watch::Sender<bool>,
}
impl App {
    /// Open without starting background tasks, also useful for explicit-time scenarios.
    pub async fn open(config: Config, path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let db = db::open(path.as_ref()).await?;
        let telegram = Telegram::new(&config)?;
        let (shutdown, _) = watch::channel(false);
        Ok(Self {
            db,
            config,
            telegram,
            wake: Notify::new(),
            shutdown,
        })
    }
    pub fn shutdown(&self) {
        self.shutdown.send_replace(true);
    }

    pub async fn accept_mail(
        &self,
        bytes: Vec<u8>,
        received_at: DateTimeUtc,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(!bytes.is_empty(), "empty mail");
        let id = mail_parser::MessageParser::default()
            .parse(&bytes)
            .and_then(|mail| mail.message_id().map(str::to_owned));
        let now = normalize(received_at);
        let tx = db::immediate(&self.db).await?;
        enqueue_inbox(&tx, IngressSource::Mail, id, bytes, now).await?;
        schedule_tick(&tx, now).await?;
        tx.commit().await?;
        self.wake.notify_one();
        Ok(())
    }
    pub async fn accept_telegram(
        &self,
        update: Update,
        received_at: DateTimeUtc,
    ) -> anyhow::Result<()> {
        self.accept_update(update, received_at, false).await
    }
    pub(crate) async fn accept_update(
        &self,
        update: Update,
        received_at: DateTimeUtc,
        polled: bool,
    ) -> anyhow::Result<()> {
        let now = normalize(received_at);
        let tx = db::immediate(&self.db).await?;
        self.telegram
            .accept_update(&tx, update, now, polled)
            .await?;
        tx.commit().await?;
        self.wake.notify_one();
        Ok(())
    }
}

pub(crate) async fn enqueue_inbox(
    tx: &DatabaseTransaction,
    source: IngressSource,
    id: Option<String>,
    payload: Vec<u8>,
    now: DateTimeUtc,
) -> anyhow::Result<()> {
    let duplicate = if let Some(id) = &id {
        inbox::Entity::find()
            .filter(inbox::Column::Source.eq(source))
            .filter(inbox::Column::ExternalId.eq(id))
            .one(tx)
            .await?
            .is_some()
    } else {
        false
    };
    if !duplicate {
        inbox::ActiveModel {
            source: Set(source.to_owned()),
            external_id: Set(id),
            payload: Set(Some(payload)),
            received_at: Set(now),
            processed_at: Set(None),
            ..Default::default()
        }
        .insert(tx)
        .await?;
    } else {
        tracing::warn!(source = source.as_str(), external_id = ?id, "ingress event already recorded");
    }
    Ok(())
}

pub(crate) async fn schedule_tick(
    tx: &DatabaseTransaction,
    now: DateTimeUtc,
) -> anyhow::Result<()> {
    let runtime = runtime::Entity::find_by_id(1)
        .one(tx)
        .await?
        .context("missing runtime")?;
    let next_tick_at = Some(
        runtime
            .next_tick_at
            .map_or(now, |previous| normalize(previous).min(now)),
    );
    runtime::ActiveModel {
        id: Set(1),
        next_tick_at: Set(next_tick_at),
        ..Default::default()
    }
    .update(tx)
    .await?;
    Ok(())
}
