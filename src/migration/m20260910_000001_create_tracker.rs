use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

// Keep this schema independent of runtime entities: later model changes must not
// change what an already-released migration does on a fresh database.
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    fn use_transaction(&self) -> Option<bool> {
        Some(true)
    }

    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(
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
                telegram_poll_offset BIGINT NOT NULL DEFAULT 0 CHECK (telegram_poll_offset >= 0),
                last_tick_at DATETIME,
                next_tick_at DATETIME
            );
            INSERT INTO tracker (id, phase) VALUES (1, 'idle');
            INSERT INTO runtime (id) VALUES (1);",
        )
        .await?;
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
