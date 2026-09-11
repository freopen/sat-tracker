use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    fn use_transaction(&self) -> Option<bool> {
        Some(true)
    }

    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"ALTER TABLE runtime RENAME TO runtime_old;
                 CREATE TABLE runtime (
                    id INTEGER PRIMARY KEY CHECK (id = 1),
                    telegram_poll_offset BIGINT NOT NULL DEFAULT 0 CHECK (telegram_poll_offset >= 0),
                    last_tick_at DATETIME,
                    next_tick_at DATETIME,
                    settings_position TEXT NOT NULL DEFAULT 'main' CHECK (settings_position IN ('main', 'settings', 'owner_reminder_times', 'safety_reminder_times', 'safety_alert_template', 'safety_recovery_template'))
                 );
                 INSERT INTO runtime (id, telegram_poll_offset, last_tick_at, next_tick_at, settings_position)
                    SELECT id, telegram_poll_offset, last_tick_at, next_tick_at, settings_position FROM runtime_old;
                 DROP TABLE runtime_old;
                 ALTER TABLE settings ADD COLUMN safety_alert_template TEXT NOT NULL DEFAULT '';
                 ALTER TABLE settings ADD COLUMN safety_recovery_template TEXT NOT NULL DEFAULT '';
                 UPDATE settings SET safety_alert_template = '**SAFETY ALERT: {{ alert.reason }}**
At: {{ alert.at | time("wDT") }}
{% if alert.threshold_minutes is not none %}No OK for {{ alert.elapsed_minutes }} minutes (threshold {{ alert.threshold_minutes }}).{% endif %}
{% if hike.location %}Location: {{ hike.location }}{% endif %}
{% if mail is not none and mail.location is not none %}Mail location: {{ mail.location }}{% endif %}
{% if mail is not none %}Message:
{{ mail.body }}{% endif %}',
                    safety_recovery_template = '**SAFETY CONTACT RESUMED**
At: {{ alert.at | time("wDT") }}';"#,
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "ALTER TABLE settings DROP COLUMN safety_recovery_template;
                 ALTER TABLE settings DROP COLUMN safety_alert_template;
                 ALTER TABLE runtime RENAME TO runtime_new;
                 CREATE TABLE runtime (
                    id INTEGER PRIMARY KEY CHECK (id = 1),
                    telegram_poll_offset BIGINT NOT NULL DEFAULT 0 CHECK (telegram_poll_offset >= 0),
                    last_tick_at DATETIME,
                    next_tick_at DATETIME,
                    settings_position TEXT NOT NULL DEFAULT 'main' CHECK (settings_position IN ('main', 'settings', 'owner_reminder_times', 'safety_reminder_times'))
                 );
                 INSERT INTO runtime (id, telegram_poll_offset, last_tick_at, next_tick_at, settings_position)
                    SELECT id, telegram_poll_offset, last_tick_at, next_tick_at,
                        CASE WHEN settings_position IN ('safety_alert_template', 'safety_recovery_template') THEN 'settings' ELSE settings_position END
                    FROM runtime_new;
                 DROP TABLE runtime_new;",
            )
            .await?;
        Ok(())
    }
}
