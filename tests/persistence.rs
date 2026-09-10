mod common;
use common::*;
use sat_tracker::{Phase, SettingsPosition, entity::inbox};
use sea_orm::{
    ColumnTrait, ConnectionTrait, DbBackend, EntityTrait, PaginatorTrait, QueryFilter, Statement,
};
use sea_orm_migration::{MigratorTrait, SchemaManager};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use wiremock::{Mock, ResponseTemplate, matchers::path};

async fn reopen(h: &Harness) -> sat_tracker::App {
    sat_tracker::App::open(config(h.server.uri()), h.dir.path().join("test.sqlite"))
        .await
        .unwrap()
}

/// Migration assertions need SQLite metadata, which has no handwritten entity.
async fn number(h: &Harness, sql: &str) -> i64 {
    h.db.query_one_raw(Statement::from_string(DbBackend::Sqlite, sql))
        .await
        .unwrap()
        .unwrap()
        .try_get_by_index(0)
        .unwrap()
}

#[tokio::test]
async fn reopen_processes_pending_events_and_overdue_reminders() {
    let h = Harness::new().await;
    h.mail("start", "OK", START).await;
    let reopened = reopen(&h).await;
    reopened.tick(time(START)).await.unwrap();
    let reopened = reopen(&h).await;
    assert_eq!(
        reopened.tick(time(START + 70 * 60_000)).await.unwrap(),
        None
    );
    assert_eq!(h.sends().await.len(), 4);
    assert_eq!(
        h.runtime().await.last_tick_at,
        Some(time(START + 70 * 60_000))
    );
    reopened.tick(time(START)).await.unwrap();
    assert_eq!(
        h.runtime().await.last_tick_at,
        Some(time(START + 70 * 60_000))
    );
    assert_eq!(h.sends().await.len(), 4);
}

#[tokio::test]
async fn settings_prompt_position_survives_restart() {
    let h = Harness::new().await;
    h.app
        .accept_telegram(update(1, 10, START, "Settings"), time(START))
        .await
        .unwrap();
    h.app
        .accept_telegram(
            update(2, 10, START + 1000, "Owner reminder times"),
            time(START + 1000),
        )
        .await
        .unwrap();
    h.tick(START + 1000).await;
    assert_eq!(
        h.runtime().await.settings_position,
        SettingsPosition::OwnerReminderTimes
    );

    let reopened = reopen(&h).await;
    reopened
        .accept_telegram(update(3, 10, START + 2000, "30, 45"), time(START + 2000))
        .await
        .unwrap();
    reopened.tick(time(START + 2000)).await.unwrap();

    assert_eq!(h.settings().await.owner_reminder_minutes.0, vec![30, 45]);
    assert_eq!(
        h.runtime().await.settings_position,
        SettingsPosition::Settings
    );
}

#[tokio::test]
async fn settings_reply_failure_rolls_back_telegram_state_with_the_inbox_row() {
    let h = Harness::new().await;
    h.app
        .accept_telegram(update(10, 10, START, "Settings"), time(START))
        .await
        .unwrap();
    h.app
        .accept_telegram(update(11, 10, START, "Owner reminder times"), time(START))
        .await
        .unwrap();
    h.tick(START).await;
    assert_eq!(
        h.runtime().await.settings_position,
        SettingsPosition::OwnerReminderTimes
    );

    h.server.reset().await;
    Mock::given(path("/bottest/sendMessage"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&h.server)
        .await;
    h.app
        .accept_telegram(update(12, 10, START + 1000, "45, 60"), time(START + 1000))
        .await
        .unwrap();
    assert!(h.app.tick(time(START + 1000)).await.is_err());
    assert_eq!(h.settings().await.owner_reminder_minutes.0, vec![30]);
    assert_eq!(
        h.runtime().await.settings_position,
        SettingsPosition::OwnerReminderTimes
    );
    assert_eq!(h.pending_inbox_count().await, 1);

    h.server.reset().await;
    h.success().await;
    h.app.tick(time(START + 2000)).await.unwrap();
    assert_eq!(h.settings().await.owner_reminder_minutes.0, vec![45, 60]);
    assert_eq!(
        h.runtime().await.settings_position,
        SettingsPosition::Settings
    );
}

#[tokio::test]
async fn scheduler_updates_preserve_telegram_runtime_fields() {
    let h = Harness::new().await;
    h.app
        .accept_telegram(update(20, 10, START, "Settings"), time(START))
        .await
        .unwrap();
    h.tick(START).await;
    h.db.execute_unprepared("UPDATE runtime SET telegram_poll_offset = 42")
        .await
        .unwrap();

    h.app.tick(time(START + 1000)).await.unwrap();
    let runtime = h.runtime().await;
    assert_eq!(runtime.telegram_poll_offset, 42);
    assert_eq!(runtime.settings_position, SettingsPosition::Settings);
}

#[tokio::test]
async fn later_send_failure_rolls_back_whole_tick_and_replays() {
    let h = Harness::new().await;
    h.server.reset().await;
    let count = Arc::new(AtomicUsize::new(0));
    let calls = count.clone();
    Mock::given(path("/bottest/sendMessage"))
        .respond_with(move |_: &wiremock::Request| {
            if calls.fetch_add(1, Ordering::SeqCst) == 1 {
                ResponseTemplate::new(500).set_body_json(
                    serde_json::json!({"ok":false,"error_code":500,"description":"failed"}),
                )
            } else {
                success()
            }
        })
        .mount(&h.server)
        .await;
    h.mail("unknown", "HELP", START).await; // start succeeds, safety fails
    assert!(h.app.tick(time(START)).await.is_err());
    assert_eq!(count.load(Ordering::SeqCst), 2); // no hidden retry
    assert_eq!(h.phase().await, Phase::Idle);
    assert_eq!(
        inbox::Entity::find()
            .filter(inbox::Column::ProcessedAt.is_null())
            .filter(inbox::Column::Payload.is_not_null())
            .count(&h.db)
            .await
            .unwrap(),
        1
    );
    assert_eq!(h.runtime().await.last_tick_at, None);
    reopen(&h).await.tick(time(START + 5000)).await.unwrap();
    assert_eq!(count.load(Ordering::SeqCst), 4);
    assert_eq!(h.phase().await, Phase::Active);
    let sends = h.sends().await;
    assert_eq!(sends[0], sends[2]);
    assert!(h.tracker().await.safety_alerted);
}

#[tokio::test]
async fn later_event_failure_rolls_back_earlier_inbox_processing() {
    let h = Harness::new().await;
    h.server.reset().await;
    let count = Arc::new(AtomicUsize::new(0));
    let calls = count.clone();
    Mock::given(path("/bottest/sendMessage"))
        .respond_with(move |_: &wiremock::Request| {
            if calls.fetch_add(1, Ordering::SeqCst) == 1 {
                ResponseTemplate::new(500).set_body_json(
                    serde_json::json!({"ok":false,"error_code":500,"description":"failed"}),
                )
            } else {
                success()
            }
        })
        .mount(&h.server)
        .await;

    h.mail("start", "OK", START).await;
    h.mail("finish", "FINISHED", START + 1000).await;
    assert!(h.app.tick(time(START + 1000)).await.is_err());
    assert_eq!(count.load(Ordering::SeqCst), 2);
    assert_eq!(h.phase().await, Phase::Idle);
    assert_eq!(h.pending_inbox_count().await, 2);
    assert_eq!(h.runtime().await.last_tick_at, None);

    h.app.tick(time(START + 5000)).await.unwrap();
    assert_eq!(count.load(Ordering::SeqCst), 6);
    assert_eq!(h.phase().await, Phase::Finished);
    assert_eq!(h.pending_inbox_count().await, 0);
    let sends = h.sends().await;
    assert_eq!(sends[0], sends[2]);
    assert_eq!(sends[4]["chat_id"], 10);
    assert_eq!(sends[5]["chat_id"], 20);
}

#[tokio::test]
async fn reminder_failure_rolls_back_processed_inbox() {
    let h = Harness::new().await;
    h.mail("start", "OK", START).await;
    h.tick(START).await.unwrap();
    h.app
        .accept_telegram(update(9, 10, START + 1000, "/unknown"), time(START + 1000))
        .await
        .unwrap();

    h.server.reset().await;
    let count = Arc::new(AtomicUsize::new(0));
    let calls = count.clone();
    Mock::given(path("/bottest/sendMessage"))
        .respond_with(move |_: &wiremock::Request| {
            if calls.fetch_add(1, Ordering::SeqCst) == 1 {
                ResponseTemplate::new(500).set_body_json(
                    serde_json::json!({"ok":false,"error_code":500,"description":"failed"}),
                )
            } else {
                success()
            }
        })
        .mount(&h.server)
        .await;

    assert!(h.app.tick(time(START + 60 * 60_000)).await.is_err());
    assert_eq!(count.load(Ordering::SeqCst), 2);
    assert_eq!(h.pending_inbox_count().await, 1);
    assert_eq!(h.runtime().await.last_tick_at, Some(time(START)));
    let tracker = h.tracker().await;
    assert_eq!(tracker.owner_reminders_sent, 0);
    assert_eq!(tracker.safety_reminders_sent, 0);
    assert!(!tracker.owner_alerted);
    assert!(!tracker.safety_alerted);

    h.app.tick(time(START + 60 * 60_000 + 5000)).await.unwrap();
    assert_eq!(count.load(Ordering::SeqCst), 4);
    assert_eq!(h.pending_inbox_count().await, 0);
    let tracker = h.tracker().await;
    assert_eq!(tracker.owner_reminders_sent, 1);
    assert_eq!(tracker.safety_reminders_sent, 1);
}

#[tokio::test]
async fn migrations_preserve_state_and_deduplication_key() {
    let h = Harness::new().await;
    h.mail("one", "OK", START).await;
    h.tick(START).await;
    reopen(&h)
        .await
        .accept_mail(b"Message-ID: <one>\r\n\r\nOK".to_vec(), time(START))
        .await
        .unwrap();
    assert_eq!(h.inbox_count().await, 1);
    let manager = SchemaManager::new(&h.db);
    for table in ["runtime", "tracker", "inbox", "settings"] {
        assert!(manager.has_table(table).await.unwrap());
    }
    assert_eq!(h.phase().await, Phase::Active);
}

#[tokio::test]
async fn legacy_database_without_migration_history_is_reset() {
    let h = Harness::new().await;
    h.mail("old", "OK", START).await;
    h.tick(START).await;
    // Covers both the entity-first schema and tables from durable-actions.
    h.db.execute_unprepared("DROP TABLE seaql_migrations; CREATE TABLE old_actions (id INTEGER PRIMARY KEY); INSERT INTO old_actions VALUES (1); CREATE VIEW old_view AS SELECT * FROM old_actions;").await.unwrap();
    let reopened = reopen(&h).await;
    assert_eq!(h.phase().await, Phase::Idle);
    assert_eq!(h.inbox_count().await, 0);
    assert_eq!(
        number(
            &h,
            "SELECT count(*) FROM sqlite_master WHERE name IN ('old_actions', 'old_view')"
        )
        .await,
        0
    );
    assert_eq!(
        sat_tracker::migration::Migrator::get_applied_migrations(&h.db)
            .await
            .unwrap()
            .len(),
        2
    );
    reopened.tick(time(START)).await.unwrap();
    assert_eq!(h.sends().await.len(), 2);
}

#[tokio::test]
async fn unknown_migration_fails_without_resetting_data() {
    let h = Harness::new().await;
    h.mail("keep", "OK", START).await;
    h.tick(START).await;
    h.db.execute_unprepared(
        "INSERT INTO seaql_migrations (version, applied_at) VALUES ('m20990101_000001_future', 0)",
    )
    .await
    .unwrap();
    assert!(
        sat_tracker::App::open(config(h.server.uri()), h.dir.path().join("test.sqlite"))
            .await
            .is_err()
    );
    assert_eq!(h.phase().await, Phase::Active);
    assert_eq!(h.inbox_count().await, 1);
    assert_eq!(number(&h, "SELECT count(*) FROM seaql_migrations").await, 3);
}

#[tokio::test]
async fn empty_migration_history_applies_initial_migration() {
    let h = Harness::new().await;
    h.db.execute_unprepared(
        "DROP TABLE inbox; DROP TABLE tracker; DROP TABLE runtime; DROP TABLE settings; DELETE FROM seaql_migrations;",
    )
    .await
    .unwrap();
    reopen(&h).await;
    assert_eq!(h.phase().await, Phase::Idle);
    assert_eq!(
        sat_tracker::migration::Migrator::get_applied_migrations(&h.db)
            .await
            .unwrap()
            .len(),
        2
    );
}
