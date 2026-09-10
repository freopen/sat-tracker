use sea_orm_migration::prelude::*;

mod m20260910_000001_create_tracker;
mod m20260910_000002_create_settings;

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(m20260910_000001_create_tracker::Migration),
            Box::new(m20260910_000002_create_settings::Migration),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn initial_migration_can_be_rolled_back_and_reapplied() {
        let dir = tempfile::TempDir::new().unwrap();
        let db = crate::db::open(&dir.path().join("migration.sqlite"))
            .await
            .unwrap();
        Migrator::down(&db, Some(2)).await.unwrap();
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
            2
        );
    }
}
