use sea_orm::Database;
use sea_orm_migration::MigratorTrait;

/// Apply pending migrations without starting the bot or HTTP server.
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let url = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "sqlite://sat-tracker.sqlite?mode=rwc".to_owned());
    let db = Database::connect(url).await?;
    sat_tracker::migration::Migrator::up(&db, None).await?;
    db.close().await?;
    Ok(())
}
