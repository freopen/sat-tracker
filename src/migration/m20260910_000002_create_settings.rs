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
                "ALTER TABLE runtime ADD COLUMN settings_position TEXT NOT NULL DEFAULT 'main' CHECK (settings_position IN ('main', 'settings', 'owner_reminder_times', 'safety_reminder_times'));
                CREATE TABLE settings (
                    id INTEGER PRIMARY KEY CHECK (id = 1),
                    owner_reminder_minutes JSON NOT NULL,
                    safety_reminder_minutes JSON NOT NULL
                );
                INSERT INTO settings (id, owner_reminder_minutes, safety_reminder_minutes)
                    VALUES (1, '[30]', '[60]');",
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "DROP TABLE settings;
                ALTER TABLE runtime DROP COLUMN settings_position;",
            )
            .await?;
        Ok(())
    }
}
