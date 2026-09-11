mod common;
use common::*;
use sat_tracker::{
    ReminderMinutes,
    entity::{inbox, settings},
};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DbBackend, EntityTrait, IntoActiveModel,
    PaginatorTrait, QueryFilter, Set, Statement,
};

#[tokio::test]
async fn inbox_constraints_enforce_source_identity_and_payload_lifecycle() {
    let h = Harness::new().await;
    for sql in [
        "INSERT INTO inbox (source, received_at, payload) VALUES ('unknown', '1970-01-01T00:00:00+00:00', x'01')",
        "INSERT INTO inbox (source, received_at, payload) VALUES ('telegram', '1970-01-01T00:00:00+00:00', x'01')",
        "INSERT INTO inbox (source, external_id, received_at, payload) VALUES ('telegram', '', '1970-01-01T00:00:00+00:00', x'01')",
        "INSERT INTO inbox (source, received_at) VALUES ('mail', '1970-01-01T00:00:00+00:00')",
        "INSERT INTO inbox (source, received_at, payload) VALUES ('mail', '1970-01-01T00:00:00+00:00', x'')",
        "INSERT INTO inbox (source, received_at, payload, processed_at) VALUES ('mail', '1970-01-01T00:00:00+00:00', x'01', '1970-01-01T00:00:01+00:00')",
    ] {
        assert!(
            h.db.execute_unprepared(sql).await.is_err(),
            "accepted: {sql}"
        );
    }
    h.db.execute_unprepared("INSERT INTO inbox (source, external_id, received_at, payload) VALUES ('mail', '42', '2023-11-14T22:13:20+00:00', x'01'), ('telegram', '42', '2023-11-14T22:13:20+00:00', x'01'), ('mail', NULL, '1970-01-01T00:00:00+00:00', x'01'), ('mail', NULL, '1970-01-01T00:00:00+00:00', x'01')").await.unwrap();
    assert!(h.db.execute_unprepared("INSERT INTO inbox (source, external_id, received_at, payload) VALUES ('mail', '42', '1970-01-01T00:00:00+00:00', x'01')").await.is_err());
    assert!(
        h.db.execute_unprepared("UPDATE inbox SET processed_at = '1970-01-01T00:00:01+00:00'")
            .await
            .is_err()
    );
    h.db.execute_unprepared(
        "UPDATE inbox SET processed_at = '1970-01-01T00:00:01+00:00', payload = NULL",
    )
    .await
    .unwrap();
    assert_eq!(
        inbox::Entity::find()
            .filter(inbox::Column::Payload.is_null())
            .count(&h.db)
            .await
            .unwrap(),
        4
    );
}

#[tokio::test]
async fn tracker_and_runtime_constraints_reject_invalid_state() {
    let h = Harness::new().await;
    for sql in [
        "INSERT INTO tracker (id, phase) VALUES (2, 'idle')",
        "INSERT INTO runtime (id) VALUES (2)",
        "UPDATE runtime SET telegram_poll_offset = -1",
        "UPDATE runtime SET settings_position = 'invalid'",
        "UPDATE tracker SET phase = 'unknown'",
        "UPDATE tracker SET phase = 'active'",
        "UPDATE tracker SET phase = 'finished'",
        "UPDATE tracker SET last_ok_at = 1",
    ] {
        assert!(
            h.db.execute_unprepared(sql).await.is_err(),
            "accepted: {sql}"
        );
    }
    h.mail("start", "OK", START).await;
    h.tick(START).await;
    for sql in [
        "UPDATE tracker SET owner_reminders_sent = -1",
        "UPDATE tracker SET safety_reminders_sent = -1",
        "UPDATE tracker SET owner_alerted = 2",
        "UPDATE tracker SET safety_alerted = -1",
        "UPDATE tracker SET owner_reminders_sent = 1",
        "UPDATE tracker SET safety_reminders_sent = 1",
        "UPDATE tracker SET last_ok_at = NULL",
        "UPDATE tracker SET last_body = NULL",
        "UPDATE tracker SET started_at = '2023-11-14T22:13:21+00:00'",
        "UPDATE tracker SET last_event_at = '2023-11-14T22:13:19+00:00'",
        "UPDATE tracker SET finished_at = last_event_at",
        "UPDATE tracker SET phase = 'finished'",
        "UPDATE tracker SET phase = 'finished', finished_at = '2023-11-14T22:13:19+00:00'",
    ] {
        assert!(
            h.db.execute_unprepared(sql).await.is_err(),
            "accepted: {sql}"
        );
    }
    // Ingress may move a deadline behind the last committed tick; this is valid.
    h.db.execute_unprepared("UPDATE runtime SET next_tick_at = '1970-01-01T00:00:00+00:00'")
        .await
        .unwrap();
}

#[tokio::test]
async fn settings_round_trip_as_json_and_keep_only_confirmed_values() {
    let h = Harness::new().await;
    let initial = h.settings().await;
    assert_eq!(initial.owner_reminder_minutes.0, vec![30]);
    assert_eq!(initial.safety_reminder_minutes.0, vec![60]);
    assert!(initial.safety_alert_template.contains("SAFETY ALERT"));
    assert!(
        initial
            .safety_recovery_template
            .contains("SAFETY CONTACT RESUMED")
    );

    let mut model = initial.into_active_model();
    model.owner_reminder_minutes = Set(ReminderMinutes(vec![30, 45, 60]));
    model.update(&h.db).await.unwrap();

    let loaded = settings::Entity::find_by_id(1)
        .one(&h.db)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(loaded.owner_reminder_minutes.0, vec![30, 45, 60]);
    let row =
        h.db.query_one_raw(Statement::from_string(
            DbBackend::Sqlite,
            "SELECT owner_reminder_minutes FROM settings WHERE id = 1",
        ))
        .await
        .unwrap()
        .unwrap();
    let raw: String = row.try_get_by_index(0).unwrap();
    assert_eq!(
        serde_json::from_str::<Vec<i64>>(&raw).unwrap(),
        vec![30, 45, 60]
    );
}

#[tokio::test]
async fn pending_inbox_query_uses_its_partial_index() {
    let h = Harness::new().await;
    let rows =
        h.db.query_all_raw(Statement::from_string(
            DbBackend::Sqlite,
            "EXPLAIN QUERY PLAN SELECT * FROM inbox WHERE processed_at IS NULL ORDER BY id ASC",
        ))
        .await
        .unwrap();
    assert!(rows.iter().any(|row| {
        row.try_get::<String>("", "detail")
            .unwrap()
            .contains("inbox_pending")
    }));
}
