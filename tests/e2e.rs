#![cfg(feature = "e2e")]

mod common;

use std::path::PathBuf;

fn scenario(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("scenarios")
        .join(name)
}

#[tokio::test]
async fn startup() {
    common::run_scenario(&scenario("startup.yaml"))
        .await
        .unwrap();
}

#[tokio::test]
async fn http_validation() {
    common::run_scenario(&scenario("http_validation.yaml"))
        .await
        .unwrap();
}

#[tokio::test]
async fn owner_commands() {
    common::run_scenario(&scenario("owner_commands.yaml"))
        .await
        .unwrap();
}

#[tokio::test]
async fn hike_lifecycle() {
    common::run_scenario(&scenario("hike_lifecycle.yaml"))
        .await
        .unwrap();
}

#[tokio::test]
async fn stale_and_cooldown() {
    common::run_scenario(&scenario("stale_and_cooldown.yaml"))
        .await
        .unwrap();
}

#[tokio::test]
async fn mail_parsing() {
    common::run_scenario(&scenario("mail_parsing.yaml"))
        .await
        .unwrap();
}

#[tokio::test]
async fn explicit_alerts() {
    common::run_scenario(&scenario("explicit_alerts.yaml"))
        .await
        .unwrap();
}

#[tokio::test]
async fn reminder_schedules() {
    common::run_scenario(&scenario("reminder_schedules.yaml"))
        .await
        .unwrap();
}

#[tokio::test]
async fn recipient_recovery() {
    common::run_scenario(&scenario("recipient_recovery.yaml"))
        .await
        .unwrap();
}

#[tokio::test]
async fn deduplication() {
    common::run_scenario(&scenario("deduplication.yaml"))
        .await
        .unwrap();
}

#[tokio::test]
async fn deduplication_retention() {
    common::run_scenario(&scenario("deduplication_retention.yaml"))
        .await
        .unwrap();
}

#[tokio::test]
async fn settings() {
    common::run_scenario(&scenario("settings.yaml"))
        .await
        .unwrap();
}

#[tokio::test]
async fn template_editing() {
    common::run_scenario(&scenario("template_editing.yaml"))
        .await
        .unwrap();
}

#[tokio::test]
async fn template_fallback() {
    common::run_scenario(&scenario("template_fallback.yaml"))
        .await
        .unwrap();
}

#[tokio::test]
async fn rich_messages() {
    common::run_scenario(&scenario("rich_messages.yaml"))
        .await
        .unwrap();
}

#[tokio::test]
async fn restart_state() {
    common::run_scenario(&scenario("restart_state.yaml"))
        .await
        .unwrap();
}

#[tokio::test]
async fn crash_pending_input() {
    common::run_scenario(&scenario("crash_pending_input.yaml"))
        .await
        .unwrap();
}

#[tokio::test]
async fn telegram_retries() {
    common::run_scenario(&scenario("telegram_retries.yaml"))
        .await
        .unwrap();
}

#[tokio::test]
async fn failed_tick_replay() {
    common::run_scenario(&scenario("failed_tick_replay.yaml"))
        .await
        .unwrap();
}

#[tokio::test]
async fn failed_template_update() {
    common::run_scenario(&scenario("failed_template_update.yaml"))
        .await
        .unwrap();
}
