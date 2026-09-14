use sea_orm_migration::prelude::*;

mod m20260910_000001_create_tracker {
    use sea_orm_migration::prelude::*;

    pub struct Migration;

    impl MigrationName for Migration {
        fn name(&self) -> &str {
            "m20260910_000001_create_tracker"
        }
    }

    #[async_trait::async_trait]
    impl MigrationTrait for Migration {
        fn use_transaction(&self) -> Option<bool> {
            Some(true)
        }

        async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
            manager.get_connection().execute_unprepared(
                "CREATE TABLE inbox (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    source TEXT NOT NULL CHECK (source IN ('mail', 'telegram')),
                    external_id TEXT,
                    received_at DATETIME NOT NULL,
                    payload BLOB,
                    processed_at DATETIME,
                    CONSTRAINT inbox_source_external_id UNIQUE (source, external_id),
                    CONSTRAINT inbox_telegram_id CHECK (
                        source != 'telegram' OR (external_id IS NOT NULL AND length(external_id) > 0)
                    ),
                    CONSTRAINT inbox_payload_state CHECK (
                        (processed_at IS NULL AND payload IS NOT NULL AND length(payload) > 0)
                        OR (processed_at IS NOT NULL AND payload IS NULL)
                    )
                );
                CREATE INDEX inbox_pending ON inbox (id) WHERE processed_at IS NULL;
                CREATE TABLE tracker (
                    id INTEGER PRIMARY KEY CHECK (id = 1),
                    phase TEXT NOT NULL CHECK (phase IN ('idle', 'active', 'finished')),
                    started_at DATETIME,
                    started_location TEXT,
                    last_event_at DATETIME,
                    last_ok_at DATETIME,
                    last_body TEXT,
                    location TEXT,
                    finished_at DATETIME,
                    owner_reminders_sent INTEGER NOT NULL DEFAULT 0 CHECK (owner_reminders_sent >= 0),
                    safety_reminders_sent INTEGER NOT NULL DEFAULT 0 CHECK (safety_reminders_sent >= 0),
                    owner_alerted BOOLEAN NOT NULL DEFAULT FALSE CHECK (owner_alerted IN (0, 1)),
                    safety_alerted BOOLEAN NOT NULL DEFAULT FALSE CHECK (safety_alerted IN (0, 1)),
                    CONSTRAINT tracker_phase_state CHECK (
                        (phase = 'idle' AND started_at IS NULL AND last_ok_at IS NULL
                            AND last_event_at IS NULL AND finished_at IS NULL AND last_body IS NULL
                            AND started_location IS NULL AND location IS NULL
                            AND owner_reminders_sent = 0 AND safety_reminders_sent = 0
                            AND owner_alerted = FALSE AND safety_alerted = FALSE)
                        OR (phase IN ('active', 'finished') AND started_at IS NOT NULL
                            AND last_ok_at IS NOT NULL AND last_event_at IS NOT NULL
                            AND last_body IS NOT NULL AND started_at <= last_ok_at
                            AND last_ok_at <= last_event_at
                            AND ((phase = 'active' AND finished_at IS NULL)
                                OR (phase = 'finished' AND finished_at IS NOT NULL
                                    AND finished_at = last_event_at)))
                    ),
                    CONSTRAINT tracker_reminder_alerts CHECK (
                        (owner_reminders_sent = 0 OR owner_alerted = TRUE)
                        AND (safety_reminders_sent = 0 OR safety_alerted = TRUE)
                    )
                );
                CREATE TABLE runtime (
                    id INTEGER PRIMARY KEY CHECK (id = 1),
                    last_tick_at DATETIME,
                    next_tick_at DATETIME
                );
                INSERT INTO tracker (id, phase) VALUES (1, 'idle');
                INSERT INTO runtime (id) VALUES (1);",
            ).await?;
            Ok(())
        }

        async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
            manager
                .get_connection()
                .execute_unprepared("DROP TABLE runtime; DROP TABLE tracker; DROP TABLE inbox;")
                .await?;
            Ok(())
        }
    }
}

mod m20260910_000002_create_settings {
    use sea_orm_migration::prelude::*;

    pub struct Migration;

    impl MigrationName for Migration {
        fn name(&self) -> &str {
            "m20260910_000002_create_settings"
        }
    }

    #[async_trait::async_trait]
    impl MigrationTrait for Migration {
        fn use_transaction(&self) -> Option<bool> {
            Some(true)
        }

        async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
            manager.get_connection().execute_unprepared(
                "ALTER TABLE runtime ADD COLUMN settings_position TEXT NOT NULL DEFAULT 'main' CHECK (settings_position IN ('main', 'settings', 'owner_reminder_times', 'safety_reminder_times'));
                CREATE TABLE settings (
                    id INTEGER PRIMARY KEY CHECK (id = 1),
                    owner_reminder_minutes JSON NOT NULL,
                    safety_reminder_minutes JSON NOT NULL
                );
                INSERT INTO settings (id, owner_reminder_minutes, safety_reminder_minutes)
                    VALUES (1, '[30]', '[60]');",
            ).await?;
            Ok(())
        }

        async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
            manager
                .get_connection()
                .execute_unprepared(
                    "DROP TABLE settings; ALTER TABLE runtime DROP COLUMN settings_position;",
                )
                .await?;
            Ok(())
        }
    }
}

mod m20260910_000003_add_message_templates {
    use sea_orm_migration::prelude::*;

    pub struct Migration;

    impl MigrationName for Migration {
        fn name(&self) -> &str {
            "m20260910_000003_add_message_templates"
        }
    }

    #[async_trait::async_trait]
    impl MigrationTrait for Migration {
        fn use_transaction(&self) -> Option<bool> {
            Some(true)
        }

        async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
            manager.get_connection().execute_unprepared(
                r#"ALTER TABLE runtime RENAME TO runtime_old;
                CREATE TABLE runtime (
                    id INTEGER PRIMARY KEY CHECK (id = 1),
                    last_tick_at DATETIME,
                    next_tick_at DATETIME,
                    settings_position TEXT NOT NULL DEFAULT 'main' CHECK (settings_position IN ('main', 'settings', 'owner_reminder_times', 'safety_reminder_times', 'safety_alert_template', 'safety_recovery_template'))
                );
                INSERT INTO runtime (id, last_tick_at, next_tick_at, settings_position)
                    SELECT id, last_tick_at, next_tick_at, settings_position FROM runtime_old;
                DROP TABLE runtime_old;
                ALTER TABLE settings ADD COLUMN safety_alert_template TEXT NOT NULL DEFAULT '';
                ALTER TABLE settings ADD COLUMN safety_recovery_template TEXT NOT NULL DEFAULT '';
                UPDATE settings SET safety_alert_template = '{% if last_alert is not none %}**SAFETY ALERT:** {{ last_alert }}{% else %}**SAFETY ALERT:** No OK has been received since {{ last_ok_at }}.{% endif %}

*Here are details to help locate the hiker.*

- **Hike started:** {{ started_at | date }} at {{ started_at | time }}
- **Starting location:** {% if started_location is not none %}{{ started_location }}{% else %}not recorded{% endif %}
- **Most recent event:** {{ last_event_at }}
- **Last OK:** {{ last_ok_at }}
- **Last known location:** {% if location is not none %}{{ location }}{% else %}not recorded{% endif %}

*Contact the hiker and use the locations above to decide what to do next.*',
                    safety_recovery_template = '{% if active %}**SAFETY CONTACT RESUMED:** The hike continues normally.{% else %}**SAFETY CONTACT RESUMED:** The hike was finished without issues.{% endif %}';"#,
            ).await?;
            Ok(())
        }

        async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
            manager.get_connection().execute_unprepared(
                "ALTER TABLE settings DROP COLUMN safety_recovery_template;
                 ALTER TABLE settings DROP COLUMN safety_alert_template;
                 ALTER TABLE runtime RENAME TO runtime_new;
                 CREATE TABLE runtime (
                    id INTEGER PRIMARY KEY CHECK (id = 1),
                    last_tick_at DATETIME,
                    next_tick_at DATETIME,
                    settings_position TEXT NOT NULL DEFAULT 'main' CHECK (settings_position IN ('main', 'settings', 'owner_reminder_times', 'safety_reminder_times'))
                 );
                 INSERT INTO runtime (id, last_tick_at, next_tick_at, settings_position)
                    SELECT id, last_tick_at, next_tick_at,
                        CASE WHEN settings_position IN ('safety_alert_template', 'safety_recovery_template') THEN 'settings' ELSE settings_position END
                    FROM runtime_new;
                 DROP TABLE runtime_new;",
            ).await?;
            Ok(())
        }
    }
}

mod m20260911_000004_processing_cursor_and_tracker_input {
    use sea_orm::{ConnectionTrait, DbBackend, Statement};
    use sea_orm_migration::prelude::*;

    pub struct Migration;

    impl MigrationName for Migration {
        fn name(&self) -> &str {
            "m20260911_000004_processing_cursor_and_tracker_input"
        }
    }

    #[async_trait::async_trait]
    impl MigrationTrait for Migration {
        fn use_transaction(&self) -> Option<bool> {
            Some(true)
        }

        async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
            let db = manager.get_connection();
            let row = db.query_one_raw(Statement::from_string(
                DbBackend::Sqlite,
                "SELECT COALESCE(MAX(id), 0) AS cursor FROM inbox WHERE processed_at IS NOT NULL",
            )).await?.expect("aggregate query always returns one row");
            let cursor: i64 = row.try_get("", "cursor")?;
            let below = db
                .query_one_raw(Statement::from_string(
                    DbBackend::Sqlite,
                    format!(
                        "SELECT id FROM inbox WHERE processed_at IS NULL AND id <= {cursor} LIMIT 1"
                    ),
                ))
                .await?;
            if below.is_some() {
                return Err(DbErr::Custom(
                    "inbox has an unprocessed row below the historical cursor".into(),
                ));
            }
            let missing = db
                .query_one_raw(Statement::from_string(
                    DbBackend::Sqlite,
                    format!("SELECT id FROM inbox WHERE id > {cursor} AND payload IS NULL LIMIT 1"),
                ))
                .await?;
            if missing.is_some() {
                return Err(DbErr::Custom(
                    "inbox row above the historical cursor has no payload".into(),
                ));
            }

            db.execute_unprepared(
                "ALTER TABLE inbox RENAME TO inbox_old;
                 CREATE TABLE inbox (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    source TEXT NOT NULL CHECK (source IN ('mail', 'telegram')),
                    external_id TEXT,
                    received_at DATETIME NOT NULL,
                    payload BLOB,
                    processed_at DATETIME,
                    CONSTRAINT inbox_source_external_id UNIQUE (source, external_id),
                    CONSTRAINT inbox_telegram_id CHECK (
                        source != 'telegram' OR (external_id IS NOT NULL AND length(external_id) > 0)
                    )
                 );
                 INSERT INTO inbox (id, source, external_id, received_at, payload, processed_at)
                    SELECT id, source, external_id, received_at, payload, processed_at FROM inbox_old;
                 DROP TABLE inbox_old;
                 CREATE INDEX inbox_pending ON inbox (id) WHERE processed_at IS NULL;
                 ALTER TABLE tracker RENAME TO tracker_old;
                 CREATE TABLE tracker (
                    id INTEGER PRIMARY KEY CHECK (id = 1),
                    active BOOLEAN NOT NULL DEFAULT FALSE CHECK (active IN (0, 1)),
                    started_at DATETIME, started_location JSON, last_event_at DATETIME, last_ok_at DATETIME,
                    last_alert TEXT, location JSON,
                    finished_at DATETIME NOT NULL DEFAULT '1970-01-01 00:00:00',
                    owner_reminders_sent INTEGER NOT NULL DEFAULT 0 CHECK (owner_reminders_sent >= 0),
                    safety_reminders_sent INTEGER NOT NULL DEFAULT 0 CHECK (safety_reminders_sent >= 0),
                    safety_alerted BOOLEAN NOT NULL DEFAULT FALSE CHECK (safety_alerted IN (0, 1)),
                    CONSTRAINT tracker_active_state CHECK (
                        (active = FALSE AND started_at IS NULL AND last_ok_at IS NULL
                            AND last_event_at IS NULL AND finished_at = '1970-01-01 00:00:00'
                            AND last_alert IS NULL
                            AND started_location IS NULL AND location IS NULL
                            AND owner_reminders_sent = 0 AND safety_reminders_sent = 0
                            AND safety_alerted = FALSE)
                        OR (active = TRUE AND started_at IS NOT NULL
                            AND last_ok_at IS NOT NULL AND last_event_at IS NOT NULL
                            AND started_at <= last_ok_at AND last_ok_at <= last_event_at
                        )
                        OR (active = FALSE AND started_at IS NOT NULL
                            AND last_ok_at IS NOT NULL AND last_event_at IS NOT NULL
                            AND started_at <= last_ok_at AND last_ok_at <= finished_at
                            AND finished_at <= last_event_at)
                    ),
                    CONSTRAINT tracker_reminder_alerts CHECK (
                        safety_reminders_sent = 0 OR safety_alerted = TRUE
                    )
                 );
                 INSERT INTO tracker (
                    id, active, started_at, started_location, last_event_at, last_ok_at, last_alert, location,
                    finished_at, owner_reminders_sent, safety_reminders_sent, safety_alerted
                 )
                    SELECT id, phase = 'active', started_at, NULL, last_event_at, last_ok_at, NULL, NULL,
                        CASE WHEN phase = 'finished' THEN COALESCE(finished_at, '1970-01-01 00:00:00')
                            ELSE '1970-01-01 00:00:00' END,
                        owner_reminders_sent, safety_reminders_sent,
                        safety_alerted
                    FROM tracker_old;
                 DROP TABLE tracker_old;
                 ALTER TABLE runtime RENAME TO runtime_old;
                 CREATE TABLE runtime (
                    id INTEGER PRIMARY KEY CHECK (id = 1),
                    last_tick_at DATETIME NOT NULL DEFAULT '1970-01-01 00:00:00', next_tick_at DATETIME,
                    settings_position TEXT NOT NULL DEFAULT 'main' CHECK (settings_position IN ('main', 'settings', 'owner_reminder_times', 'safety_reminder_times', 'safety_alert_template', 'safety_recovery_template')),
                    last_processed_inbox_id BIGINT NOT NULL DEFAULT 0 CHECK (last_processed_inbox_id >= 0)
                 );
                 INSERT INTO runtime (id, last_tick_at, next_tick_at, settings_position, last_processed_inbox_id)
                    SELECT id, COALESCE(last_tick_at, '1970-01-01 00:00:00'), next_tick_at, settings_position, 0 FROM runtime_old;
                 UPDATE runtime SET last_processed_inbox_id = (SELECT COALESCE(MAX(id), 0) FROM inbox WHERE processed_at IS NOT NULL);
                 DROP TABLE runtime_old;",
            ).await?;
            Ok(())
        }

        async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
            manager.get_connection().execute_unprepared(
                "ALTER TABLE runtime RENAME TO runtime_new;
                 CREATE TABLE runtime (
                    id INTEGER PRIMARY KEY CHECK (id = 1),
                    last_tick_at DATETIME, next_tick_at DATETIME,
                    settings_position TEXT NOT NULL DEFAULT 'main' CHECK (settings_position IN ('main', 'settings', 'owner_reminder_times', 'safety_reminder_times', 'safety_alert_template', 'safety_recovery_template'))
                 );
                 INSERT INTO runtime (id, last_tick_at, next_tick_at, settings_position)
                    SELECT id, last_tick_at, next_tick_at, settings_position FROM runtime_new;
                 DROP TABLE runtime_new;
                 ALTER TABLE tracker RENAME TO tracker_new;
                 CREATE TABLE tracker (
                    id INTEGER PRIMARY KEY CHECK (id = 1), phase TEXT NOT NULL CHECK (phase IN ('idle', 'active', 'finished')),
                    started_at DATETIME, started_location TEXT, last_event_at DATETIME, last_ok_at DATETIME,
                    last_body TEXT, location TEXT, finished_at DATETIME,
                    owner_reminders_sent INTEGER NOT NULL DEFAULT 0 CHECK (owner_reminders_sent >= 0),
                    safety_reminders_sent INTEGER NOT NULL DEFAULT 0 CHECK (safety_reminders_sent >= 0),
                    owner_alerted BOOLEAN NOT NULL DEFAULT FALSE CHECK (owner_alerted IN (0, 1)),
                    safety_alerted BOOLEAN NOT NULL DEFAULT FALSE CHECK (safety_alerted IN (0, 1)),
                    CONSTRAINT tracker_phase_state CHECK (
                        (phase = 'idle' AND started_at IS NULL AND last_ok_at IS NULL AND last_event_at IS NULL AND finished_at IS NULL AND last_body IS NULL AND started_location IS NULL AND location IS NULL AND owner_reminders_sent = 0 AND safety_reminders_sent = 0 AND owner_alerted = FALSE AND safety_alerted = FALSE)
                        OR (phase IN ('active', 'finished') AND started_at IS NOT NULL AND last_ok_at IS NOT NULL AND last_event_at IS NOT NULL AND last_body IS NOT NULL AND started_at <= last_ok_at AND last_ok_at <= last_event_at AND ((phase = 'active' AND finished_at IS NULL) OR (phase = 'finished' AND finished_at IS NOT NULL AND finished_at = last_event_at)))
                    ),
                    CONSTRAINT tracker_reminder_alerts CHECK ((owner_reminders_sent = 0 OR owner_alerted = TRUE) AND (safety_reminders_sent = 0 OR safety_alerted = TRUE))
                 );
                 INSERT INTO tracker (id, phase, started_at, started_location, last_event_at, last_ok_at, last_body, location, finished_at, owner_reminders_sent, safety_reminders_sent, owner_alerted, safety_alerted)
                    SELECT id,
                        CASE WHEN active THEN 'active' WHEN started_at IS NULL THEN 'idle' ELSE 'finished' END,
                        started_at, NULL, last_event_at, last_ok_at,
                        CASE WHEN started_at IS NULL THEN NULL ELSE COALESCE(last_alert, '') END,
                        NULL, CASE WHEN active THEN NULL ELSE last_event_at END,
                        owner_reminders_sent, safety_reminders_sent,
                        owner_reminders_sent > 0, safety_alerted FROM tracker_new;
                 DROP TABLE tracker_new;
                 DROP INDEX IF EXISTS inbox_pending;
                 ALTER TABLE inbox RENAME TO inbox_new;
                 CREATE TABLE inbox (
                    id INTEGER PRIMARY KEY AUTOINCREMENT, source TEXT NOT NULL CHECK (source IN ('mail', 'telegram')),
                    external_id TEXT, received_at DATETIME NOT NULL, payload BLOB, processed_at DATETIME,
                    CONSTRAINT inbox_source_external_id UNIQUE (source, external_id),
                    CONSTRAINT inbox_telegram_id CHECK (source != 'telegram' OR (external_id IS NOT NULL AND length(external_id) > 0)),
                    CONSTRAINT inbox_payload_state CHECK ((processed_at IS NULL AND payload IS NOT NULL AND length(payload) > 0) OR (processed_at IS NOT NULL AND payload IS NULL))
                 );
                 INSERT INTO inbox (id, source, external_id, received_at, payload, processed_at)
                    SELECT id, source, external_id, received_at, CASE WHEN processed_at IS NULL THEN payload ELSE NULL END, processed_at FROM inbox_new;
                 DROP TABLE inbox_new;
                 CREATE INDEX inbox_pending ON inbox (id) WHERE processed_at IS NULL;",
            ).await?;
            Ok(())
        }
    }
}

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(m20260910_000001_create_tracker::Migration),
            Box::new(m20260910_000002_create_settings::Migration),
            Box::new(m20260910_000003_add_message_templates::Migration),
            Box::new(m20260911_000004_processing_cursor_and_tracker_input::Migration),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{ConnectOptions, ConnectionTrait, Database, DbBackend, Statement};
    use sea_orm_migration::MigratorTrait;

    #[tokio::test]
    async fn schema_matches_schema_sql() -> anyhow::Result<()> {
        let mut options = ConnectOptions::new("sqlite::memory:");
        options.max_connections(1);
        let database = Database::connect(options).await?;
        Migrator::up(&database, None).await?;

        let rows = database
            .query_all_raw(Statement::from_string(
                DbBackend::Sqlite,
                "SELECT sql
                 FROM sqlite_schema
                 WHERE sql IS NOT NULL AND name NOT LIKE 'sqlite_%'
                 ORDER BY name"
                    .to_owned(),
            ))
            .await?;
        let actual = rows
            .into_iter()
            .map(|row| row.try_get_by_index::<String>(0))
            .collect::<Result<Vec<_>, _>>()?
            .join(";\n");
        let expected = include_str!("schema.sql");
        assert_eq!(normalize_schema(&actual), normalize_schema(expected));
        Ok(())
    }

    fn normalize_schema(schema: &str) -> String {
        schema
            .split(';')
            .map(|statement| statement.split_whitespace().collect::<Vec<_>>().join(" "))
            .filter(|statement| !statement.is_empty())
            .map(|statement| {
                statement
                    .replace(" ,", ",")
                    .replace("( ", "(")
                    .replace(" )", ")")
            })
            .collect::<Vec<_>>()
            .join(";\n")
    }
}
