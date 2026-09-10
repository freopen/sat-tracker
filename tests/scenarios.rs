mod common;
use common::*;
use sat_tracker::{Phase, entity::inbox};
use sea_orm::{ColumnTrait, EntityTrait, PaginatorTrait, QueryFilter};

#[tokio::test]
async fn lifecycle_deadlines_and_recovery() {
    let h = Harness::new().await;
    h.mail("idle-finish", "FINISHED", START).await;
    assert_eq!(h.tick(START).await, None);
    assert!(h.sends().await.is_empty());
    h.mail("start", "OK", START).await;
    assert_eq!(h.tick(START).await, Some(time(START + 30 * 60_000)));
    assert_eq!(h.phase().await, Phase::Active);
    h.tick(START + 30 * 60_000 - 1).await;
    assert_eq!(h.sends().await.len(), 1);
    assert_eq!(
        h.tick(START + 30 * 60_000).await,
        Some(time(START + 60 * 60_000))
    );
    assert_eq!(h.sends().await[1]["chat_id"], 10);
    assert_eq!(h.tick(START + 60 * 60_000).await, None);
    assert_eq!(h.sends().await[2]["chat_id"], 20);
    h.tick(START + 60 * 60_000).await;
    assert_eq!(h.sends().await.len(), 3);
    h.mail("recovery", "OK", START + 61 * 60_000).await;
    assert_eq!(
        h.tick(START + 61 * 60_000).await,
        Some(time(START + 91 * 60_000))
    );
    let sends = h.sends().await;
    assert_eq!(sends.len(), 5);
    assert_eq!(sends[0]["chat_id"], 10);
    assert_eq!(sends[1]["chat_id"], 10);
    assert_eq!(sends[2]["chat_id"], 20);
    assert_eq!(sends[3]["chat_id"], 10);
    assert_eq!(sends[4]["chat_id"], 20);
    assert!(
        sends[3]["text"]
            .as_str()
            .unwrap()
            .contains("contact resumed")
    );
    h.mail("finish", "FINISHED", START + 62 * 60_000).await;
    assert_eq!(h.tick(START + 62 * 60_000).await, None);
    assert_eq!(h.phase().await, Phase::Finished);
    assert_eq!(h.sends().await.len(), 7);
    assert_eq!(h.sends().await[5]["chat_id"], 10);
    assert_eq!(h.sends().await[6]["chat_id"], 20);
    assert_eq!(
        inbox::Entity::find()
            .filter(inbox::Column::Payload.is_not_null())
            .count(&h.db)
            .await
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn stale_events_refresh_and_exact_cooldown() {
    let h = Harness::new().await;
    h.mail("start", "OK", START).await;
    h.tick(START).await;
    h.mail("refresh", "OK", START + 1000).await;
    assert_eq!(
        h.tick(START + 1000).await,
        Some(time(START + 1000 + 30 * 60_000))
    );
    h.mail("stale-ok", "OK", START).await;
    h.mail("stale-finish", "FINISHED", START).await;
    h.tick(START + 2000).await;
    assert_eq!(h.sends().await.len(), 1);
    h.mail("finish", "FINISHED", START + 2000).await;
    h.tick(START + 2000).await;
    h.mail("cooldown", "OK", START + 2000 + 300_000).await;
    h.tick(START + 2000 + 300_000).await;
    assert_eq!(h.phase().await, Phase::Finished);
    h.mail("new", "OK", START + 2001 + 300_000).await;
    h.tick(START + 2001 + 300_000).await;
    assert_eq!(h.phase().await, Phase::Active);
    assert_eq!(h.sends().await.len(), 4);
}

#[tokio::test]
async fn unrecognized_mail_alerts_suppresses_reminder_and_recovers() {
    let h = Harness::new().await;
    h.mail("unknown", "HELP", START).await;
    h.tick(START).await;
    let sends = h.sends().await;
    assert_eq!(sends.len(), 2);
    assert_eq!(sends[0]["chat_id"], 10);
    assert_eq!(sends[1]["chat_id"], 20);
    h.mail("unknown-again", "HELP", START + 1000).await;
    h.tick(START + 60 * 60_000).await;
    let sends = h.sends().await;
    assert_eq!(sends.len(), 3); // only owner reminder
    assert_eq!(sends[2]["chat_id"], 10);
    h.mail("recovery", "OK", START + 61 * 60_000).await;
    h.tick(START + 61 * 60_000).await;
    let sends = h.sends().await;
    assert_eq!(sends.len(), 5);
    assert_eq!(sends[3]["chat_id"], 10);
    assert_eq!(sends[4]["chat_id"], 20);
    assert!(
        sends[3]["text"]
            .as_str()
            .unwrap()
            .contains("contact resumed")
    );
    assert!(
        sends[4]["text"]
            .as_str()
            .unwrap()
            .contains("contact resumed")
    );
    assert_eq!(h.tracker().await.safety_reminders_sent, 0);
    assert!(!h.tracker().await.owner_alerted);
    assert!(!h.tracker().await.safety_alerted);
}

#[tokio::test]
async fn unauthorized_commands_are_ignored() {
    let h = Harness::new().await;
    for (id, chat, text) in [(1, 99, "/ok"), (2, 99, "/finished"), (3, 99, "/version")] {
        h.app
            .accept_telegram(update(id, chat, START, text), time(START))
            .await
            .unwrap();
    }
    h.tick(START).await;
    assert_eq!(h.phase().await, Phase::Idle);
    assert!(h.sends().await.is_empty());
    assert_eq!(h.inbox_count().await, 3);
    assert_eq!(h.pending_inbox_count().await, 0);
}

#[tokio::test]
async fn owner_commands_and_version_are_handled() {
    let h = Harness::new().await;
    for (id, text) in [(1, "/unknown"), (2, "/version"), (3, "/ok@tracker")] {
        h.app
            .accept_telegram(update(id, 10, START, text), time(START))
            .await
            .unwrap();
    }
    h.tick(START).await;
    assert_eq!(h.phase().await, Phase::Active);
    let sends = h.sends().await;
    assert_eq!(sends.len(), 2);
    assert_eq!(sends[0]["chat_id"], 10);
    assert!(sends[0]["text"].as_str().unwrap().contains("Git commit:"));
    assert_eq!(sends[1]["chat_id"], 10);
    assert!(sends[1]["text"].as_str().unwrap().contains("hike started"));
    h.app
        .accept_telegram(update(4, 10, START + 1000, "/finished"), time(START + 1000))
        .await
        .unwrap();
    h.tick(START + 1000).await;
    assert_eq!(h.phase().await, Phase::Finished);
    assert_eq!(h.sends().await.len(), 4);
    assert_eq!(h.sends().await[2]["chat_id"], 10);
    assert_eq!(h.sends().await[3]["chat_id"], 20);
}

#[tokio::test]
async fn duplicate_telegram_updates_are_processed_once() {
    let h = Harness::new().await;
    let first = update(7, 10, START, "/version");
    h.app.accept_telegram(first, time(START)).await.unwrap();
    h.tick(START).await;
    h.app
        .accept_telegram(update(7, 10, START + 1000, "/ok"), time(START + 1000))
        .await
        .unwrap();
    h.tick(START + 1000).await;
    assert_eq!(h.inbox_count().await, 1);
    assert_eq!(h.pending_inbox_count().await, 0);
    assert_eq!(h.phase().await, Phase::Idle);
    assert_eq!(h.sends().await.len(), 1);
}

#[tokio::test]
async fn duplicate_mail_message_ids_ignore_later_payloads() {
    let h = Harness::new().await;
    h.mail("same", "OK", START).await;
    h.tick(START).await;
    h.mail("same", "FINISHED", START + 1000).await;
    h.tick(START + 1000).await;
    assert_eq!(h.inbox_count().await, 1);
    assert_eq!(h.phase().await, Phase::Active);
    assert_eq!(h.sends().await.len(), 1);
    assert_eq!(h.tracker().await.last_body.as_deref(), Some("OK"));
}

#[tokio::test]
async fn pending_ok_is_applied_before_overdue_reminders() {
    let h = Harness::new().await;
    h.mail("start", "OK", START).await;
    h.tick(START).await;
    h.mail("refresh", "OK", START + 70 * 60_000).await;
    assert_eq!(
        h.tick(START + 70 * 60_000).await,
        Some(time(START + 100 * 60_000))
    );
    assert_eq!(h.sends().await.len(), 1);
}

#[tokio::test]
async fn mail_without_id_is_retained_as_separate_events() {
    let h = Harness::new().await;
    h.app
        .accept_mail(b"\r\nHELP".to_vec(), time(START))
        .await
        .unwrap();
    h.app
        .accept_mail(b"\r\nOK".to_vec(), time(START + 1000))
        .await
        .unwrap();
    h.tick(START + 1000).await;
    assert_eq!(
        inbox::Entity::find()
            .filter(inbox::Column::ExternalId.is_null())
            .count(&h.db)
            .await
            .unwrap(),
        2
    );
}

#[tokio::test]
async fn messages_within_limit_are_sent_in_one_request() {
    let h = Harness::new().await;
    let text = "x".repeat(4000);
    h.app
        .accept_mail(format!("\r\n{text}").into_bytes(), time(START))
        .await
        .unwrap();
    h.tick(START).await;
    let sends = h.sends().await;
    assert_eq!(sends.len(), 2); // start and one safety alert
    assert!(sends[1]["text"].as_str().unwrap().ends_with(&text));
}

#[tokio::test]
async fn owner_reminder_recovers_only_the_owner() {
    let h = Harness::new().await;
    h.mail("start", "OK", START).await;
    h.tick(START).await;
    h.tick(START + 30 * 60_000).await;
    h.mail("recovery", "OK", START + 30 * 60_000 + 1000).await;
    h.tick(START + 30 * 60_000 + 1000).await;
    let sends = h.sends().await;
    assert_eq!(sends.len(), 3);
    assert_eq!(sends[0]["chat_id"], 10);
    assert_eq!(sends[1]["chat_id"], 10);
    assert_eq!(sends[2]["chat_id"], 10);
    assert!(
        sends[2]["text"]
            .as_str()
            .unwrap()
            .contains("contact resumed")
    );
    assert_eq!(h.tracker().await.owner_reminders_sent, 0);
    assert_eq!(h.tracker().await.safety_reminders_sent, 0);
    assert!(!h.tracker().await.owner_alerted);
    assert!(!h.tracker().await.safety_alerted);
}

#[tokio::test]
async fn pending_finished_event_precedes_overdue_reminders() {
    let h = Harness::new().await;
    h.mail("start", "OK", START).await;
    h.tick(START).await;
    h.mail("finish", "FINISHED", START + 60 * 60_000).await;
    h.tick(START + 60 * 60_000).await;
    assert_eq!(h.phase().await, Phase::Finished);
    assert_eq!(h.sends().await.len(), 3);
    assert_eq!(h.sends().await[1]["chat_id"], 10);
    assert_eq!(h.sends().await[2]["chat_id"], 20);
}
