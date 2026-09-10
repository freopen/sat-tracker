use crate::migration::Migrator;
use sea_orm::{
    ConnectOptions, ConnectionTrait, Database, DatabaseConnection, DatabaseTransaction,
    SqliteTransactionMode, TransactionOptions, TransactionTrait,
};
use sea_orm_migration::{MigratorTrait, SchemaManager};
use std::{path::Path, time::Duration};

pub(crate) async fn immediate(db: &DatabaseConnection) -> anyhow::Result<DatabaseTransaction> {
    Ok(db
        .begin_with_options(TransactionOptions {
            sqlite_transaction_mode: Some(SqliteTransactionMode::Immediate),
            ..Default::default()
        })
        .await?)
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
    if SchemaManager::new(&db)
        .has_table("seaql_migrations")
        .await?
    {
        Migrator::up(&db, None).await?;
    } else {
        tracing::warn!("database has no SeaORM migration history; resetting legacy schema");
        // SeaORM fresh drops tables, but not SQLite views. Use one connection
        // during setup so its foreign_keys PRAGMA applies to every reset statement.
        let views = db
            .query_all_raw(sea_orm::Statement::from_string(
                sea_orm::DbBackend::Sqlite,
                "SELECT name FROM sqlite_master WHERE type = 'view'",
            ))
            .await?;
        for view in views {
            let name: String = view.try_get("", "name")?;
            db.execute_unprepared(&format!("DROP VIEW \"{}\"", name.replace('"', "\"\"")))
                .await?;
        }
        Migrator::fresh(&db).await?;
    }
    db.close().await?;
    options.max_connections(4);
    Ok(Database::connect(options).await?)
}
