use crate::migration::Migrator;
use sea_orm::{ConnectOptions, Database, DatabaseConnection};
use sea_orm_migration::MigratorTrait;
use std::{path::Path, time::Duration};

pub(crate) mod inbox {
    use crate::time::DateTimeUtc;
    use sea_orm::entity::prelude::*;
    use serde::Serialize;

    #[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, EnumIter, DeriveActiveEnum)]
    #[serde(rename_all = "snake_case")]
    #[sea_orm(rs_type = "String", db_type = "Text", rename_all = "snake_case")]
    pub(crate) enum IngressSource {
        Mail,
        Telegram,
    }

    #[sea_orm::model]
    #[derive(Clone, Debug, PartialEq, Eq, Serialize, DeriveEntityModel)]
    #[sea_orm(table_name = "inbox")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i64,
        pub source: IngressSource,
        pub external_id: Option<String>,
        pub received_at: DateTimeUtc,
        pub payload: Option<Vec<u8>>,
        pub processed_at: Option<DateTimeUtc>,
    }

    impl ActiveModelBehavior for ActiveModel {}
}

pub(crate) mod tracker {
    use crate::time::DateTimeUtc;
    use sea_orm::entity::prelude::*;
    use serde::{Deserialize, Serialize};

    #[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, FromJsonQueryResult)]
    pub(crate) struct Location(pub(crate) f64, pub(crate) f64);

    #[sea_orm::model]
    #[derive(Clone, Debug, PartialEq, Serialize, DeriveEntityModel)]
    #[sea_orm(table_name = "tracker")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: i64,
        pub active: bool,
        pub started_at: Option<DateTimeUtc>,
        pub started_location: Option<Location>,
        pub last_event_at: Option<DateTimeUtc>,
        pub last_ok_at: Option<DateTimeUtc>,
        pub last_alert: Option<String>,
        pub location: Option<Location>,
        pub finished_at: DateTimeUtc,
        pub owner_reminders_sent: i64,
        pub safety_reminders_sent: i64,
        pub safety_alerted: bool,
    }

    impl ActiveModelBehavior for ActiveModel {}
}

pub(crate) mod runtime {
    use crate::time::DateTimeUtc;
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Copy, Debug, PartialEq, Eq, EnumIter, DeriveActiveEnum)]
    #[sea_orm(rs_type = "String", db_type = "Text", rename_all = "snake_case")]
    pub(crate) enum SettingsPosition {
        Main,
        Settings,
        OwnerReminderTimes,
        SafetyReminderTimes,
        SafetyAlertTemplate,
        SafetyRecoveryTemplate,
    }

    #[sea_orm::model]
    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "runtime")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: i64,
        pub settings_position: SettingsPosition,
        pub last_tick_at: DateTimeUtc,
        pub next_tick_at: Option<DateTimeUtc>,
        pub last_processed_inbox_id: i64,
    }

    impl ActiveModelBehavior for ActiveModel {}
}

pub(crate) mod settings {
    use sea_orm::entity::prelude::*;
    use serde::{Deserialize, Serialize};

    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, FromJsonQueryResult)]
    pub(crate) struct ReminderMinutes(pub(crate) Vec<i64>);

    impl ReminderMinutes {
        pub(crate) fn parse(input: &str) -> anyhow::Result<Self> {
            let mut minutes = Vec::new();
            for part in input.split(',') {
                let part = part.trim();
                anyhow::ensure!(
                    !part.is_empty(),
                    "reminder times must not contain empty values"
                );
                let minute = part
                    .parse::<i64>()
                    .map_err(|_| anyhow::anyhow!("reminder times must be integers"))?;
                anyhow::ensure!(minute > 0, "reminder times must be positive");
                if let Some(previous) = minutes.last() {
                    anyhow::ensure!(
                        minute > *previous,
                        "reminder times must be strictly increasing"
                    );
                }
                minutes.push(minute);
            }
            anyhow::ensure!(!minutes.is_empty(), "reminder times must not be empty");
            Ok(Self(minutes))
        }

        pub(crate) fn as_slice(&self) -> &[i64] {
            &self.0
        }
    }

    impl std::fmt::Display for ReminderMinutes {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            let value = self
                .0
                .iter()
                .map(i64::to_string)
                .collect::<Vec<_>>()
                .join(", ");
            formatter.write_str(&value)
        }
    }

    #[sea_orm::model]
    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "settings")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: i64,
        pub owner_reminder_minutes: ReminderMinutes,
        pub safety_reminder_minutes: ReminderMinutes,
        pub safety_alert_template: String,
        pub safety_recovery_template: String,
    }

    impl ActiveModelBehavior for ActiveModel {}
}

pub(crate) async fn open(path: &Path) -> anyhow::Result<DatabaseConnection> {
    let mut options = ConnectOptions::new("sqlite://sat-tracker.sqlite?mode=rwc");
    let path = path.to_owned();
    options
        .max_connections(1)
        .sqlx_logging(false)
        .map_sqlx_sqlite_opts(move |opts| {
            opts.filename(&path)
                .create_if_missing(true)
                .foreign_keys(true)
                .pragma("journal_mode", "WAL")
                .pragma("synchronous", "FULL")
                .busy_timeout(Duration::from_secs(5))
        });
    let db = Database::connect(options.clone()).await?;
    Migrator::up(&db, None).await?;
    db.close().await?;
    options.max_connections(4);
    Ok(Database::connect(options).await?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{ActiveModelTrait, ConnectionTrait, DbBackend, EntityTrait, Set, Statement};
    use sea_orm_migration::SchemaManager;
    use settings::ReminderMinutes;

    #[tokio::test]
    async fn fresh_database_is_initialized_with_seaorm_migrations() -> anyhow::Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("database.sqlite");

        let database = open(&path).await?;
        assert!(
            SchemaManager::new(&database)
                .has_table("seaql_migrations")
                .await?
        );
        database.close().await?;
        Ok(())
    }

    #[tokio::test]
    async fn locations_round_trip_as_json_arrays() -> anyhow::Result<()> {
        let directory = tempfile::tempdir()?;
        let database = open(&directory.path().join("database.sqlite")).await?;
        let at = crate::time::DateTimeUtc::from_timestamp(1_700_000_000, 0).unwrap();
        tracker::ActiveModel {
            id: Set(1),
            active: Set(true),
            started_at: Set(Some(at)),
            started_location: Set(Some(tracker::Location(-27.15, -109.4333))),
            last_event_at: Set(Some(at)),
            last_ok_at: Set(Some(at)),
            location: Set(Some(tracker::Location(-10.4217, 105.6791))),
            ..Default::default()
        }
        .update(&database)
        .await?;

        let stored = database
            .query_one_raw(Statement::from_string(
                DbBackend::Sqlite,
                "SELECT started_location, location FROM tracker WHERE id = 1".to_owned(),
            ))
            .await?
            .expect("tracker query always returns one row");
        let started_location: String = stored.try_get_by_index(0)?;
        let location: String = stored.try_get_by_index(1)?;
        assert_eq!(
            serde_json::from_str::<[f64; 2]>(&started_location)?,
            [-27.15, -109.4333]
        );
        assert_eq!(
            serde_json::from_str::<[f64; 2]>(&location)?,
            [-10.4217, 105.6791]
        );

        let tracker = tracker::Entity::find_by_id(1)
            .one(&database)
            .await?
            .expect("tracker row exists");
        assert_eq!(
            tracker.started_location,
            Some(tracker::Location(-27.15, -109.4333))
        );
        assert_eq!(
            tracker.location,
            Some(tracker::Location(-10.4217, 105.6791))
        );
        Ok(())
    }

    #[test]
    fn reminder_minutes_parse_and_format_canonical_values() {
        let minutes = ReminderMinutes::parse(" 30, 45, 60 ").unwrap();
        assert_eq!(minutes.as_slice(), &[30, 45, 60]);
        assert_eq!(minutes.to_string(), "30, 45, 60");
    }

    #[test]
    fn reminder_minutes_reject_invalid_values() {
        for input in [
            "", " ", ",", "30,", "30,,60", "0", "-1", "thirty", "45, 30", "30, 30",
        ] {
            assert!(
                ReminderMinutes::parse(input).is_err(),
                "accepted: {input:?}"
            );
        }
    }
}
