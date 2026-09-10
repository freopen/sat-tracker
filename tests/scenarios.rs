mod common;
use common::*;
use sat_tracker::{Phase, entity::inbox};
use sea_orm::{ColumnTrait, EntityTrait, PaginatorTrait, QueryFilter};

#[tokio::test]
async fn lifecycle_deadlines_recovery_and_deduplication() {
    let h = Harness::new().await;
    h.mail("idle-finish", "FINISHED", START).await;
    assert_eq!(h.tick(START).await, None);
    assert!(h.sends().await.is_empty());
    h.mail("start", "OK", START).await;
    assert_eq!(h.tick(START).await, Some(time(START + 30 * 60_000)));
    assert_eq!(h.phase().await, Phase::Active);
    h.mail("start", "OK", START).await;
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
    assert_eq!(h.sends().await.len(), 2);
    h.mail("unknown-again", "HELP", START + 1000).await;
    h.tick(START + 60 * 60_000).await;
    assert_eq!(h.sends().await.len(), 3); // only owner reminder
    h.mail("recovery", "OK", START + 61 * 60_000).await;
    h.tick(START + 61 * 60_000).await;
    assert_eq!(h.sends().await.len(), 5);
    assert_eq!(h.tracker().await.safety_reminders_sent, 0);
}

#[tokio::test]
async fn owner_commands_filtering_and_version() {
    let h = Harness::new().await;
    for (id, chat, text) in [
        (1, 99, "/ok"),
        (2, 10, "/unknown"),
        (3, 10, "/version"),
        (4, 10, "/ok@tracker"),
    ] {
        h.app
            .accept_telegram(update(id, chat, START, text), time(START))
            .await
            .unwrap();
    }
    h.tick(START).await;
    assert_eq!(h.phase().await, Phase::Active);
    let sends = h.sends().await;
    assert_eq!(sends.len(), 2);
    assert!(sends[0]["text"].as_str().unwrap().contains("Git commit:"));
    h.app
        .accept_telegram(update(4, 10, START, "/ok@tracker"), time(START))
        .await
        .unwrap();
    h.app
        .accept_telegram(update(5, 10, START + 1000, "/finished"), time(START + 1000))
        .await
        .unwrap();
    h.tick(START + 1000).await;
    assert_eq!(h.phase().await, Phase::Finished);
    assert_eq!(h.sends().await.len(), 4);
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
async fn mail_without_id_is_retained_separately_and_messages_are_not_chunked() {
    let h = Harness::new().await;
    // The old implementation split at 4,000 characters; this fits a single TG message.
    let text = "x".repeat(4000);
    h.app
        .accept_mail(format!("\r\n{text}").into_bytes(), time(START))
        .await
        .unwrap();
    h.app
        .accept_mail(b"\r\nOK".to_vec(), time(START + 1000))
        .await
        .unwrap();
    h.tick(START + 1000).await;
    let sends = h.sends().await;
    assert_eq!(sends.len(), 3); // start, unknown alert, recovery
    assert!(sends[1]["text"].as_str().unwrap().ends_with(&text));
    assert_eq!(
        inbox::Entity::find()
            .filter(inbox::Column::ExternalId.is_null())
            .count(&h.db)
            .await
            .unwrap(),
        2
    );
}
