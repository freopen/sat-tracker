use sea_orm_migration::prelude::*;

mod m20260910_000001_create_tracker;
mod m20260910_000002_create_settings;
mod m20260910_000003_add_message_templates;

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(m20260910_000001_create_tracker::Migration),
            Box::new(m20260910_000002_create_settings::Migration),
            Box::new(m20260910_000003_add_message_templates::Migration),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{ConnectionTrait, EntityTrait};

    #[tokio::test]
    async fn initial_migration_can_be_rolled_back_and_reapplied() {
        let dir = tempfile::TempDir::new().unwrap();
        let db = crate::db::open(&dir.path().join("migration.sqlite"))
            .await
            .unwrap();
        Migrator::down(&db, Some(3)).await.unwrap();
        let manager = SchemaManager::new(&db);
        for table in ["inbox", "tracker", "runtime", "settings"] {
            assert!(!manager.has_table(table).await.unwrap());
        }
        assert!(
            Migrator::get_applied_migrations(&db)
                .await
                .unwrap()
                .is_empty()
        );
        Migrator::up(&db, None).await.unwrap();
        for table in ["inbox", "tracker", "runtime", "settings"] {
            assert!(manager.has_table(table).await.unwrap());
        }
        assert_eq!(
            Migrator::get_applied_migrations(&db).await.unwrap().len(),
            3
        );
    }

    #[tokio::test]
    async fn message_template_migration_preserves_existing_runtime_and_settings() {
        let dir = tempfile::TempDir::new().unwrap();
        let db = crate::db::open(&dir.path().join("migration.sqlite"))
            .await
            .unwrap();
        Migrator::up(&db, Some(2)).await.unwrap();
        db.execute_unprepared(
            "UPDATE settings SET owner_reminder_minutes = '[5, 10]', safety_reminder_minutes = '[15]';
             UPDATE runtime SET telegram_poll_offset = 42, settings_position = 'safety_reminder_times';",
        )
        .await
        .unwrap();

        Migrator::up(&db, None).await.unwrap();
        let settings = crate::entity::settings::Entity::find_by_id(1)
            .one(&db)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(settings.owner_reminder_minutes.0, vec![5, 10]);
        assert_eq!(settings.safety_reminder_minutes.0, vec![15]);
        assert!(settings.safety_alert_template.contains("SAFETY ALERT"));
        assert!(
            settings
                .safety_recovery_template
                .contains("SAFETY CONTACT RESUMED")
        );
        let runtime = crate::entity::runtime::Entity::find_by_id(1)
            .one(&db)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(runtime.telegram_poll_offset, 42);
        assert_eq!(
            runtime.settings_position,
            crate::state::SettingsPosition::SafetyReminderTimes
        );

        Migrator::down(&db, Some(1)).await.unwrap();
        assert_eq!(
            Migrator::get_applied_migrations(&db).await.unwrap().len(),
            2
        );
    }
}
