mod common;
use common::*;
use sat_tracker::{Phase, ReminderMinutes, SettingsPosition, entity::inbox};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, EntityTrait, IntoActiveModel, PaginatorTrait, QueryFilter, Set,
};

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
    assert_eq!(h.sends().await.len(), 2);
    assert_eq!(
        h.tick(START + 30 * 60_000).await,
        Some(time(START + 60 * 60_000))
    );
    assert_eq!(h.sends().await[2]["chat_id"], 10);
    assert_eq!(h.tick(START + 60 * 60_000).await, None);
    assert_eq!(h.sends().await[3]["chat_id"], 20);
    h.tick(START + 60 * 60_000).await;
    assert_eq!(h.sends().await.len(), 4);
    h.mail("recovery", "OK", START + 61 * 60_000).await;
    assert_eq!(
        h.tick(START + 61 * 60_000).await,
        Some(time(START + 91 * 60_000))
    );
    let sends = h.sends().await;
    assert_eq!(sends.len(), 7);
    assert_eq!(sends[0]["chat_id"], 10);
    assert_eq!(sends[1]["chat_id"], 10);
    assert_eq!(sends[3]["chat_id"], 20);
    assert_eq!(sends[4]["chat_id"], 10);
    assert_eq!(sends[5]["chat_id"], 20);
    assert_eq!(sends[6]["chat_id"], 10);
    assert!(
        sends[4]["text"]
            .as_str()
            .unwrap()
            .contains("contact resumed")
    );
    h.mail("finish", "FINISHED", START + 62 * 60_000).await;
    assert_eq!(h.tick(START + 62 * 60_000).await, None);
    assert_eq!(h.phase().await, Phase::Finished);
    assert_eq!(h.sends().await.len(), 8);
    assert_eq!(h.sends().await[7]["chat_id"], 10);
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
    assert_eq!(h.sends().await.len(), 4);
    h.mail("finish", "FINISHED", START + 2000).await;
    h.tick(START + 2000).await;
    h.mail("cooldown", "OK", START + 2000 + 300_000).await;
    h.tick(START + 2000 + 300_000).await;
    assert_eq!(h.phase().await, Phase::Finished);
    h.mail("new", "OK", START + 2001 + 300_000).await;
    h.tick(START + 2001 + 300_000).await;
    assert_eq!(h.phase().await, Phase::Active);
    assert_eq!(h.sends().await.len(), 7);
}

#[tokio::test]
async fn alert_mail_alerts_suppresses_reminder_and_recovers() {
    let h = Harness::new().await;
    h.mail("alert-mail", "HELP", START).await;
    h.tick(START).await;
    let sends = h.sends().await;
    assert_eq!(sends.len(), 3);
    assert_eq!(sends[0]["chat_id"], 10);
    assert_eq!(sends[2]["chat_id"], 20);
    h.mail("alert-mail-again", "HELP", START + 1000).await;
    h.tick(START + 60 * 60_000).await;
    let sends = h.sends().await;
    assert_eq!(sends.len(), 4); // only owner reminder after alert-start acknowledgement
    assert_eq!(sends[2]["chat_id"], 20);
    assert_eq!(sends[3]["chat_id"], 10);
    assert_eq!(h.tracker().await.last_ok_at, Some(time(START)));
    h.mail("recovery", "OK", START + 61 * 60_000).await;
    h.tick(START + 61 * 60_000).await;
    let sends = h.sends().await;
    assert_eq!(sends.len(), 7);
    assert_eq!(sends[3]["chat_id"], 10);
    assert_eq!(sends[4]["chat_id"], 10);
    assert_eq!(sends[5]["chat_id"], 20);
    assert_eq!(sends[6]["chat_id"], 10);
    assert!(
        sends[4]["text"]
            .as_str()
            .unwrap()
            .contains("contact resumed")
    );
    assert_eq!(sends[6]["text"], "OK received.");
    assert_eq!(
        h.tracker().await.last_ok_at,
        Some(time(START + 61 * 60_000))
    );
    assert_eq!(h.tracker().await.safety_reminders_sent, 0);
    assert!(!h.tracker().await.owner_alerted);
    assert!(!h.tracker().await.safety_alerted);
}

#[tokio::test]
async fn unauthorized_commands_are_ignored() {
    let h = Harness::new().await;
    for (id, text) in [
        (1, "/start"),
        (2, "Start hike"),
        (3, "OK"),
        (4, "FINISHED"),
        (5, "/ok"),
        (6, "/finished"),
        (7, "/version"),
    ] {
        h.app
            .accept_telegram(update(id, 99, START, text), time(START))
            .await
            .unwrap();
    }
    h.tick(START).await;
    assert_eq!(h.phase().await, Phase::Idle);
    assert!(h.sends().await.is_empty());
    assert_eq!(h.inbox_count().await, 7);
    assert_eq!(h.pending_inbox_count().await, 0);
}

#[tokio::test]
async fn owner_reply_keyboard_tracks_state_and_silences_ok_acknowledgements() {
    let h = Harness::new().await;
    h.app
        .accept_telegram(update(1, 10, START, "/start"), time(START))
        .await
        .unwrap();
    h.tick(START).await;
    let sends = h.sends().await;
    assert_eq!(sends.len(), 1);
    assert_eq!(sends[0]["text"], "Choose an action.");
    assert_eq!(
        sends[0]["reply_markup"]["keyboard"],
        serde_json::json!([[{"text": "Start hike"}, {"text": "Settings"}]])
    );
    assert!(sends[0]["reply_markup"].get("one_time_keyboard").is_none());
    assert_eq!(sends[0]["reply_markup"]["resize_keyboard"], true);

    h.app
        .accept_telegram(update(2, 10, START, "Start hike"), time(START))
        .await
        .unwrap();
    h.tick(START).await;
    let sends = h.sends().await;
    assert_eq!(sends.len(), 3);
    assert!(sends[1]["text"].as_str().unwrap().contains("hike started"));
    assert_eq!(sends[2]["text"], "OK received.");
    assert_eq!(sends[2]["disable_notification"], true);
    assert_eq!(
        sends[2]["reply_markup"]["keyboard"],
        serde_json::json!([[{"text": "OK"}, {"text": "FINISHED"}]])
    );

    h.app
        .accept_telegram(update(3, 10, START + 1000, "OK"), time(START + 1000))
        .await
        .unwrap();
    h.tick(START + 1000).await;
    let sends = h.sends().await;
    assert_eq!(sends.len(), 4);
    assert_eq!(sends[3]["text"], "OK received.");
    assert_eq!(sends[3]["disable_notification"], true);

    h.app
        .accept_telegram(update(4, 10, START + 2000, "FINISHED"), time(START + 2000))
        .await
        .unwrap();
    h.tick(START + 2000).await;
    let sends = h.sends().await;
    assert_eq!(h.phase().await, Phase::Finished);
    assert_eq!(sends.len(), 5);
    assert_eq!(sends[4]["chat_id"], 10);
    assert_eq!(
        sends[4]["reply_markup"]["keyboard"],
        serde_json::json!([[{"text": "Start hike"}, {"text": "Settings"}]])
    );
    assert!(sends[4].get("reply_markup").is_some());
}

#[tokio::test]
async fn owner_can_edit_both_reminder_schedules_while_inactive() {
    let h = Harness::new().await;

    h.app
        .accept_telegram(update(10, 10, START, "/start"), time(START))
        .await
        .unwrap();
    h.tick(START).await;
    assert_eq!(h.runtime().await.settings_position, SettingsPosition::Main);
    assert_eq!(
        h.sends().await[0]["reply_markup"]["keyboard"],
        serde_json::json!([[{"text": "Start hike"}, {"text": "Settings"}]])
    );

    h.app
        .accept_telegram(update(11, 10, START, "Settings"), time(START))
        .await
        .unwrap();
    h.tick(START).await;
    assert_eq!(
        h.runtime().await.settings_position,
        SettingsPosition::Settings
    );
    assert_eq!(h.sends().await[1]["text"], "Choose a setting.");

    h.app
        .accept_telegram(update(12, 10, START, "Owner reminder times"), time(START))
        .await
        .unwrap();
    h.tick(START).await;
    assert_eq!(
        h.runtime().await.settings_position,
        SettingsPosition::OwnerReminderTimes
    );
    assert!(h.sends().await[2]["text"].as_str().unwrap().contains("30"));
    assert_eq!(
        h.sends().await[2]["reply_markup"]["keyboard"],
        serde_json::json!([[{"text": "Back"}]])
    );

    h.app
        .accept_telegram(update(13, 10, START, "30, 45, 60"), time(START))
        .await
        .unwrap();
    h.tick(START).await;
    assert_eq!(
        h.settings().await.owner_reminder_minutes.0,
        vec![30, 45, 60]
    );
    assert_eq!(
        h.runtime().await.settings_position,
        SettingsPosition::Settings
    );
    assert!(
        h.sends().await[3]["text"]
            .as_str()
            .unwrap()
            .contains("30, 45, 60")
    );

    h.app
        .accept_telegram(update(14, 10, START, "Safety reminder times"), time(START))
        .await
        .unwrap();
    h.tick(START).await;
    assert_eq!(
        h.runtime().await.settings_position,
        SettingsPosition::SafetyReminderTimes
    );
    assert!(h.sends().await[4]["text"].as_str().unwrap().contains("60"));

    h.app
        .accept_telegram(update(15, 10, START, "45, 90"), time(START))
        .await
        .unwrap();
    h.tick(START).await;
    assert_eq!(h.settings().await.safety_reminder_minutes.0, vec![45, 90]);
    assert_eq!(
        h.runtime().await.settings_position,
        SettingsPosition::Settings
    );

    h.app
        .accept_telegram(update(16, 10, START, "Back"), time(START))
        .await
        .unwrap();
    h.tick(START).await;
    assert_eq!(h.runtime().await.settings_position, SettingsPosition::Main);
    assert_eq!(
        h.sends().await[6]["reply_markup"]["keyboard"],
        serde_json::json!([[{"text": "Start hike"}, {"text": "Settings"}]])
    );
}

#[tokio::test]
async fn safety_alert_and_recovery_templates_keep_editor_state_and_validate_samples() {
    let h = Harness::new().await;
    for (id, text) in [(60, "Settings"), (61, "Safety alert template")] {
        h.app
            .accept_telegram(update(id, 10, START, text), time(START))
            .await
            .unwrap();
    }
    h.tick(START).await;
    assert_eq!(
        h.runtime().await.settings_position,
        SettingsPosition::SafetyAlertTemplate
    );
    let old_alert = h.settings().await.safety_alert_template;

    h.app
        .accept_telegram(update(62, 10, START, "Rendered examples"), time(START))
        .await
        .unwrap();
    h.app
        .accept_telegram(update(63, 10, START, "Guide"), time(START))
        .await
        .unwrap();
    h.tick(START).await;
    assert_eq!(
        h.runtime().await.settings_position,
        SettingsPosition::SafetyAlertTemplate
    );
    let sends = h.sends().await;
    assert_eq!(
        sends
            .iter()
            .filter(|send| {
                send["chat_id"] == 10
                    && send["text"]
                        .as_str()
                        .unwrap_or_default()
                        .contains("Telegram message template Guide")
            })
            .count(),
        1
    );

    h.app
        .accept_telegram(update(64, 10, START, "{{ missing }}"), time(START))
        .await
        .unwrap();
    h.tick(START).await;
    assert_eq!(h.settings().await.safety_alert_template, old_alert);
    assert_eq!(
        h.runtime().await.settings_position,
        SettingsPosition::SafetyAlertTemplate
    );

    let replacement = "**{{ alert.reason }}**";
    h.app
        .accept_telegram(update(65, 10, START, replacement), time(START))
        .await
        .unwrap();
    h.tick(START).await;
    assert_eq!(h.settings().await.safety_alert_template, replacement);
    assert_eq!(
        h.runtime().await.settings_position,
        SettingsPosition::Settings
    );

    h.app
        .accept_telegram(
            update(66, 10, START, "Safety recovery template"),
            time(START),
        )
        .await
        .unwrap();
    h.tick(START).await;
    assert_eq!(
        h.runtime().await.settings_position,
        SettingsPosition::SafetyRecoveryTemplate
    );
    let recovery = "**recovered {{ alert.at | time(format=\"t\") }}**";
    h.app
        .accept_telegram(update(67, 10, START, recovery), time(START))
        .await
        .unwrap();
    h.tick(START).await;
    assert_eq!(h.settings().await.safety_recovery_template, recovery);
    assert_eq!(
        h.runtime().await.settings_position,
        SettingsPosition::Settings
    );
}

#[tokio::test]
async fn invalid_reminder_input_keeps_confirmed_value_and_prompt() {
    let h = Harness::new().await;
    for (id, text) in [
        (20, "Settings"),
        (21, "Owner reminder times"),
        (22, "45, 30"),
    ] {
        h.app
            .accept_telegram(update(id, 10, START, text), time(START))
            .await
            .unwrap();
    }
    h.tick(START).await;

    assert_eq!(h.settings().await.owner_reminder_minutes.0, vec![30]);
    assert_eq!(
        h.runtime().await.settings_position,
        SettingsPosition::OwnerReminderTimes
    );
    let sends = h.sends().await;
    assert!(sends[2]["text"].as_str().unwrap().starts_with("Invalid"));
    assert_eq!(
        sends[2]["reply_markup"]["keyboard"],
        serde_json::json!([[{"text": "Back"}]])
    );
}

#[tokio::test]
async fn settings_are_immutable_after_hike_start() {
    let h = Harness::new().await;
    for (id, text) in [
        (30, "Settings"),
        (31, "Owner reminder times"),
        (32, "5, 10"),
        (33, "Back"),
        (34, "Start hike"),
    ] {
        h.app
            .accept_telegram(update(id, 10, START, text), time(START))
            .await
            .unwrap();
    }
    h.tick(START).await;

    assert_eq!(h.phase().await, Phase::Active);
    assert_eq!(h.settings().await.owner_reminder_minutes.0, vec![5, 10]);
    assert_eq!(h.runtime().await.settings_position, SettingsPosition::Main);

    h.app
        .accept_telegram(update(35, 10, START, "1, 2"), time(START))
        .await
        .unwrap();
    h.tick(START).await;
    assert_eq!(h.settings().await.owner_reminder_minutes.0, vec![5, 10]);
    assert_eq!(h.runtime().await.settings_position, SettingsPosition::Main);
}

#[tokio::test]
async fn mail_start_resets_telegram_settings_conversation_in_same_tick() {
    let h = Harness::new().await;
    for (id, text) in [
        (40, "Settings"),
        (41, "Owner reminder times"),
        (42, "5, 10"),
    ] {
        h.app
            .accept_telegram(update(id, 10, START, text), time(START))
            .await
            .unwrap();
    }
    h.mail("mail-start", "OK", START).await;
    h.tick(START).await;

    assert_eq!(h.phase().await, Phase::Active);
    assert_eq!(h.settings().await.owner_reminder_minutes.0, vec![5, 10]);
    assert_eq!(h.runtime().await.settings_position, SettingsPosition::Main);
}

#[tokio::test]
async fn persisted_reminder_schedules_drive_deadlines() {
    let h = Harness::new().await;
    let mut settings = h.settings().await.into_active_model();
    settings.owner_reminder_minutes = Set(ReminderMinutes(vec![5, 10]));
    settings.safety_reminder_minutes = Set(ReminderMinutes(vec![15]));
    settings.update(&h.db).await.unwrap();

    h.mail("start", "OK", START).await;
    assert_eq!(h.tick(START).await, Some(time(START + 5 * 60_000)));
    assert_eq!(
        h.tick(START + 5 * 60_000).await,
        Some(time(START + 10 * 60_000))
    );
    assert_eq!(
        h.tick(START + 10 * 60_000).await,
        Some(time(START + 15 * 60_000))
    );
    assert_eq!(h.tick(START + 15 * 60_000).await, None);

    let tracker = h.tracker().await;
    assert_eq!(tracker.owner_reminders_sent, 2);
    assert_eq!(tracker.safety_reminders_sent, 1);
    let sends = h.sends().await;
    assert_eq!(sends.len(), 5);
    assert_eq!(sends[2]["chat_id"], 10);
    assert_eq!(sends[3]["chat_id"], 10);
    assert_eq!(sends[4]["chat_id"], 20);
}

#[tokio::test]
async fn alert_mail_suppresses_all_configured_safety_reminders() {
    let h = Harness::new().await;
    let mut settings = h.settings().await.into_active_model();
    settings.owner_reminder_minutes = Set(ReminderMinutes(vec![30]));
    settings.safety_reminder_minutes = Set(ReminderMinutes(vec![5, 10, 15]));
    settings.update(&h.db).await.unwrap();

    h.mail("alert-mail", "HELP", START).await;
    h.tick(START).await;
    assert_eq!(h.tracker().await.safety_reminders_sent, 3);
    h.tick(START + 15 * 60_000).await;
    assert_eq!(h.sends().await.len(), 3);
    assert_eq!(h.tracker().await.safety_reminders_sent, 3);
}

#[tokio::test]
async fn safety_recovery_uses_its_template_after_a_safety_alert() {
    let h = Harness::new().await;
    let mut settings = h.settings().await.into_active_model();
    settings.safety_recovery_template = Set("{{ alert.reason }} {{ mail.body }}".to_owned());
    settings.update(&h.db).await.unwrap();

    h.mail("start", "OK", START).await;
    h.tick(START).await;
    h.tick(START + 60 * 60_000).await;
    h.mail("recovery", "OK", START + 61 * 60_000).await;
    h.tick(START + 61 * 60_000).await;

    let sends = h.sends().await;
    let safety_recovery = sends
        .iter()
        .find(|send| {
            send["chat_id"] == 20
                && send["text"]
                    .as_str()
                    .unwrap_or_default()
                    .contains("recovery OK")
        })
        .expect("safety recovery message");
    assert!(
        safety_recovery["rich_message"]["markdown"]
            .as_str()
            .unwrap()
            .contains("recovery OK")
    );
}

#[tokio::test]
async fn rejected_alert_rich_markdown_uses_bounded_safety_fallback_and_notifies_owner() {
    let h = Harness::new().await;
    let mut settings = h.settings().await.into_active_model();
    settings.safety_alert_template = Set("bad *".to_owned());
    settings.update(&h.db).await.unwrap();

    h.server.reset().await;
    let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = calls.clone();
    wiremock::Mock::given(wiremock::matchers::path("/bottest/sendRichMessage"))
        .respond_with(move |_: &wiremock::Request| {
            if counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 2 {
                wiremock::ResponseTemplate::new(400).set_body_json(serde_json::json!({
                    "ok": false,
                    "error_code": 400,
                    "description": "Bad Request: can't parse entities"
                }))
            } else {
                success()
            }
        })
        .mount(&h.server)
        .await;

    h.mail("bad-markdown", "HELP", START).await;
    h.tick(START).await.unwrap();
    let sends = h.sends().await;
    assert_eq!(sends.len(), 5);
    assert_eq!(sends[2]["chat_id"], 20);
    assert_eq!(sends[3]["chat_id"], 20);
    assert!(sends[3]["text"].as_str().unwrap().contains("alert mail"));
    assert_eq!(sends[4]["chat_id"], 10);
    assert!(sends[4]["text"].as_str().unwrap().contains("built"));
    assert!(h.tracker().await.safety_alerted);
}

#[tokio::test]
async fn every_processed_ok_has_quiet_owner_message_and_telegram_bypasses_mail_cooldown() {
    let h = Harness::new().await;
    h.mail("start", "OK", START).await;
    h.tick(START).await;
    let sends = h.sends().await;
    assert_eq!(sends.len(), 2);
    assert_eq!(sends[1]["text"], "OK received.");
    assert_eq!(sends[1]["disable_notification"], true);

    h.mail("finish", "FINISHED", START + 1000).await;
    h.tick(START + 1000).await;
    let before_cooldown_ok = h.sends().await.len();
    h.mail("mail-cooldown", "OK", START + 301_000).await;
    h.tick(START + 301_000).await;
    assert_eq!(h.phase().await, Phase::Finished);
    assert_eq!(h.sends().await.len(), before_cooldown_ok);

    h.app
        .accept_telegram(
            update(5, 10, START + 2000, "Start hike"),
            time(START + 2000),
        )
        .await
        .unwrap();
    h.tick(START + 2000).await;
    assert_eq!(h.phase().await, Phase::Active);
    let sends = h.sends().await;
    assert_eq!(sends.len(), before_cooldown_ok + 2);
    assert_eq!(sends[sends.len() - 1]["text"], "OK received.");
    assert_eq!(sends[sends.len() - 1]["disable_notification"], true);

    h.mail("refresh", "OK", START + 3000).await;
    h.tick(START + 3000).await;
    let sends = h.sends().await;
    assert_eq!(sends[sends.len() - 1]["text"], "OK received.");
    assert_eq!(sends[sends.len() - 1]["disable_notification"], true);
}

#[tokio::test]
async fn owner_commands_and_version_are_handled() {
    let h = Harness::new().await;
    for (id, text) in [
        (1, "/unknown"),
        (2, "/version"),
        (3, "/start"),
        (4, "Start hike"),
    ] {
        h.app
            .accept_telegram(update(id, 10, START, text), time(START))
            .await
            .unwrap();
    }
    h.tick(START).await;
    assert_eq!(h.phase().await, Phase::Active);
    let sends = h.sends().await;
    assert_eq!(sends.len(), 4);
    assert_eq!(sends[0]["chat_id"], 10);
    assert!(sends[0]["text"].as_str().unwrap().contains("Git commit:"));
    assert_eq!(sends[1]["chat_id"], 10);
    assert_eq!(sends[1]["text"], "Choose an action.");
    assert_eq!(sends[2]["chat_id"], 10);
    assert!(sends[2]["text"].as_str().unwrap().contains("hike started"));
    assert_eq!(sends[3]["text"], "OK received.");
    h.app
        .accept_telegram(update(5, 10, START + 1000, "FINISHED"), time(START + 1000))
        .await
        .unwrap();
    h.tick(START + 1000).await;
    assert_eq!(h.phase().await, Phase::Finished);
    assert_eq!(h.sends().await.len(), 5);
    assert_eq!(h.sends().await[4]["chat_id"], 10);
    assert!(h.sends().await[4]["reply_markup"].is_object());
}

#[tokio::test]
async fn duplicate_telegram_updates_are_processed_once() {
    let h = Harness::new().await;
    let first = update(7, 10, START, "/version");
    h.app.accept_telegram(first, time(START)).await.unwrap();
    h.tick(START).await;
    h.app
        .accept_telegram(update(7, 10, START + 1000, "OK"), time(START + 1000))
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
    assert_eq!(h.sends().await.len(), 2);
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
    assert_eq!(h.sends().await.len(), 3);
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
async fn rich_messages_allow_more_than_the_legacy_text_limit() {
    let h = Harness::new().await;
    let text = "x".repeat(4000);
    h.app
        .accept_mail(format!("\r\n{text}").into_bytes(), time(START))
        .await
        .unwrap();
    h.tick(START).await;
    let sends = h.sends().await;
    assert_eq!(sends.len(), 3); // start, silent OK, and one rich safety alert
    let rendered = sends[2]["rich_message"]["markdown"]
        .as_str()
        .expect("rich Markdown payload");
    assert!(rendered.len() > 4096);
    assert!(rendered.chars().count() <= 32_768);
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
    assert_eq!(sends.len(), 5);
    assert_eq!(sends[0]["chat_id"], 10);
    assert_eq!(sends[1]["chat_id"], 10);
    assert_eq!(sends[2]["chat_id"], 10);
    assert_eq!(sends[3]["chat_id"], 10);
    assert!(
        sends[3]["text"]
            .as_str()
            .unwrap()
            .contains("contact resumed")
    );
    assert_eq!(sends[4]["chat_id"], 10);
    assert_eq!(sends[4]["text"], "OK received.");
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
    assert_eq!(h.sends().await[2]["chat_id"], 10);
}
