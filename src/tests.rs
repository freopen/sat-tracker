use std::{
    collections::VecDeque,
    path::Path,
    sync::Arc,
    time::{Duration, SystemTime},
};

use axum::{
    body::{Body, Bytes},
    http::{Request as HttpRequest, StatusCode},
};
use durable_actions::Action;
use regex::Regex;
use rusqlite::Connection;
use tempfile::TempDir;
use tokio::time::sleep;
use tower::ServiceExt;
use wiremock::{
    Mock, MockServer, Request, ResponseTemplate,
    matchers::{method, path},
};

use crate::{
    App, Config,
    actions::{AlertAction, ProcessMail},
    http::{router, submit},
    mail::{Signal, extract_location, parse},
    state::{
        ActiveHike, AlertParameters, AlertSignal, Audience, FINISHED_COOLDOWN, HikeState,
        OWNER_AFTER, RawMail, TrackerState, push_bounded,
    },
    telegram::{Telegram, split},
};

fn config(api_url: String) -> Config {
    Config {
        ok_regex: Regex::new(r"ALL\s+OK").unwrap(),
        finished_regex: Regex::new(r"FINISH(?:ED)?").unwrap(),
        owner_chat_id: 1,
        safety_chat_id: 2,
        telegram_api_url: api_url,
        telegram_webhook_url: String::new(),
        telegram_bot_token: "test".into(),
    }
}

async fn mount_delete_webhook(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/bottest/deleteWebhook"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "ok": true,
            "result": true
        })))
        .mount(server)
        .await;
}

async fn telegram_server(delay: Duration) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/bottest/sendMessage"))
        .respond_with(ResponseTemplate::new(200).set_delay(delay).set_body_json(
            serde_json::json!({
                "ok": true,
                "result": {
                    "message_id": 1,
                    "date": 0,
                    "chat": {"id": 1, "type": "private"},
                    "text": "accepted"
                }
            }),
        ))
        .mount(&server)
        .await;
    mount_delete_webhook(&server).await;
    server
}

fn raw_mail(id: &str, body: &str) -> Bytes {
    Bytes::from(format!("Message-ID: <{id}>\n\n{body}"))
}

async fn submit_mail(app: &Arc<App>, id: &str, body: &str) -> StatusCode {
    submit(Arc::clone(app), raw_mail(id, body)).await
}

async fn enqueue_at(app: &App, id: &str, body: &str, received_at: SystemTime) {
    app.handle
        .enqueue::<ProcessMail>(&RawMail {
            bytes: raw_mail(id, body).to_vec(),
            received_at,
        })
        .await
        .unwrap();
}

async fn wait_for_requests(server: &MockServer, count: usize) {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if send_requests(server).await.len() >= count {
                return;
            }
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}

async fn send_requests(server: &MockServer) -> Vec<Request> {
    server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|request| request.url.path().ends_with("/sendMessage"))
        .collect()
}

fn telegram_update(update_id: u32, chat_id: i64, date: u64, text: &str) -> serde_json::Value {
    serde_json::json!({
        "update_id": update_id,
        "message": {
            "message_id": update_id,
            "date": date,
            "chat": {"id": chat_id, "type": "private"},
            "text": text,
        }
    })
}

fn read_state(path: &Path) -> TrackerState {
    let connection = Connection::open(path).unwrap();
    let payload: Vec<u8> = connection
        .query_row("SELECT payload FROM state WHERE singleton = 1", [], |row| {
            row.get(0)
        })
        .unwrap();
    serde_json::from_slice(&payload).unwrap()
}

async fn wait_for_state(path: &Path, predicate: impl Fn(&TrackerState) -> bool) -> TrackerState {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let state = read_state(path);
            if predicate(&state) {
                return state;
            }
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap()
}

fn active(last_ok_at: SystemTime) -> ActiveHike {
    ActiveHike {
        started_at: last_ok_at,
        started_location: Some("Lat 1 Lon 2".into()),
        last_event_at: last_ok_at,
        last_ok_at,
        last_body: "ALL OK".into(),
        location: Some("Lat 1 Lon 2".into()),
        owner_started_notified: true,
        owner_alerted: false,
        safety_alerted: false,
        owner_alert_action: None,
        safety_alert_action: None,
    }
}

#[test]
fn parses_classifies_clamps_and_extracts_quoted_printable_mail() {
    let received_at = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
    let parsed = parse(
        RawMail {
            bytes: b"Date: Thu, 01 Jan 2099 00:00:00 +0000\nMessage-ID: <future>\n\nALL OK Lat 47.1 Lon 9.6 https://inreachlink.com/example.".to_vec(),
            received_at,
        },
        &config("http://localhost".into()),
    );
    assert_eq!(parsed.signal, Signal::Ok);
    assert_eq!(parsed.event.event_at, received_at);
    assert_eq!(parsed.event.message_id.as_deref(), Some("future"));
    assert_eq!(
        parsed.event.location.as_deref(),
        Some("Lat 47.1 Lon 9.6\nhttps://inreachlink.com/example")
    );

    let ambiguous = parse(
        RawMail {
            bytes: b"\nALL OK and FINISHED".to_vec(),
            received_at,
        },
        &config("http://localhost".into()),
    );
    assert_eq!(ambiguous.signal, Signal::Alert);

    let synthetic = concat!(
        "From: Garmin InReach <noreply@example.test>\r\n",
        "To: tracker@example.test\r\n",
        "Date: Thu, 01 Jan 2026 12:00:00 +0000\r\n",
        "Message-ID: <synthetic-inreach@example.test>\r\n",
        "MIME-Version: 1.0\r\n",
        "Content-Type: text/plain; charset=\"UTF-8\"\r\n",
        "Content-Transfer-Encoding: quoted-printable\r\n",
        "\r\n",
        "ALL OK\r\n",
        "Lat 12.3456 Lon -65.4321\r\n",
        "https://inreachlink.com/synthetic-token?source=3Dtest\r\n",
    );
    let parsed = parse(
        RawMail {
            bytes: synthetic.as_bytes().to_vec(),
            received_at: SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000),
        },
        &config("http://localhost".into()),
    );
    assert_eq!(parsed.signal, Signal::Ok);
    assert_eq!(
        parsed.event.location.as_deref(),
        Some(
            "Lat 12.3456 Lon -65.4321\n\
             https://inreachlink.com/synthetic-token?source=test"
        )
    );
    assert_eq!(extract_location(&parsed.event.body), parsed.event.location);
}

#[test]
fn telegram_chunks_are_utf8_safe_and_cover_empty_messages() {
    let chunks = split(&"ü".repeat(4001));
    assert_eq!(chunks.len(), 2);
    assert_eq!(
        chunks
            .iter()
            .map(|chunk| chunk.chars().count())
            .sum::<usize>(),
        4001
    );
    assert_eq!(split(""), vec!["(empty message body)"]);
}

#[test]
fn message_id_history_is_bounded() {
    let mut ids = VecDeque::new();
    for index in 0..1025 {
        push_bounded(&mut ids, index.to_string());
    }
    assert_eq!(ids.len(), 1024);
    assert_eq!(ids.front().map(String::as_str), Some("1"));
    assert_eq!(ids.back().map(String::as_str), Some("1024"));
}

#[tokio::test]
async fn lifecycle_is_durable_deduplicated_and_ignores_finish_while_idle() {
    let server = telegram_server(Duration::ZERO).await;
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("tracker.sqlite");
    let (app, runner) = App::start(config(server.uri()), &path).await.unwrap();
    let app = Arc::new(app);

    assert_eq!(
        submit_mail(&app, "idle-finish", "FINISHED").await,
        StatusCode::NO_CONTENT
    );
    wait_for_state(&path, |state| {
        state.processed_ids.contains(&"idle-finish".to_owned())
    })
    .await;
    assert!(matches!(read_state(&path).hike, HikeState::Idle));
    assert!(send_requests(&server).await.is_empty());

    assert_eq!(
        submit_mail(&app, "ok-1", "ALL OK").await,
        StatusCode::NO_CONTENT
    );
    wait_for_requests(&server, 1).await;
    assert_eq!(
        submit_mail(&app, "ok-1", "ALL OK").await,
        StatusCode::NO_CONTENT
    );
    sleep(Duration::from_millis(50)).await;
    assert_eq!(send_requests(&server).await.len(), 1);

    assert_eq!(
        submit_mail(&app, "finish-1", "FINISHED").await,
        StatusCode::NO_CONTENT
    );
    wait_for_requests(&server, 3).await;

    app.shutdown();
    runner.await.unwrap();
    let state = read_state(&path);
    let HikeState::Finished(finished) = state.hike else {
        panic!("expected a finished hike");
    };
    assert!(finished.owner_notified);
    assert!(finished.safety_notified);
    assert_eq!(
        state.processed_ids,
        VecDeque::from([
            "idle-finish".to_owned(),
            "ok-1".to_owned(),
            "finish-1".to_owned()
        ])
    );
}

#[tokio::test]
async fn telegram_listener_enqueues_owner_commands() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/bottest/sendMessage"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "ok": true,
            "result": {
                "message_id": 1,
                "date": 0,
                "chat": {"id": 1, "type": "private"},
                "text": "accepted"
            }
        })))
        .mount(&server)
        .await;
    mount_delete_webhook(&server).await;

    let owner_ok = telegram_update(42, 1, 1_700_000_000, "/ok");
    let foreign_ok = telegram_update(40, 2, 1_700_000_000, "/ok");
    let owner_finished = telegram_update(43, 1, 1_700_000_060, "/finished");
    Mock::given(method("POST"))
        .and(path("/bottest/getUpdates"))
        .respond_with(move |request: &Request| {
            let offset = serde_json::from_slice::<serde_json::Value>(&request.body)
                .ok()
                .and_then(|body| body.get("offset").and_then(serde_json::Value::as_i64));
            let updates = match offset {
                Some(0) => vec![foreign_ok.clone(), owner_ok.clone()],
                Some(43) => vec![owner_finished.clone()],
                _ => Vec::new(),
            };
            let delay = if updates.is_empty() {
                Duration::from_millis(25)
            } else {
                Duration::ZERO
            };
            ResponseTemplate::new(200)
                .set_delay(delay)
                .set_body_json(serde_json::json!({"ok": true, "result": updates}))
        })
        .mount(&server)
        .await;

    let directory = TempDir::new().unwrap();
    let path = directory.path().join("tracker.sqlite");
    let (app, runner) = App::start(config(server.uri()), &path).await.unwrap();
    let app = Arc::new(app);

    wait_for_state(&path, |state| {
        matches!(&state.hike, HikeState::Finished(finished) if finished.body == "/finished")
    })
    .await;
    wait_for_requests(&server, 3).await;

    let updates = server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|request| request.url.path().ends_with("/getUpdates"))
        .collect::<Vec<_>>();
    assert!(updates.iter().any(|request| {
        request
            .body_json::<serde_json::Value>()
            .unwrap()
            .get("offset")
            == Some(&serde_json::json!(0))
    }));
    assert!(updates.iter().any(|request| {
        request
            .body_json::<serde_json::Value>()
            .unwrap()
            .get("offset")
            == Some(&serde_json::json!(43))
    }));

    app.shutdown();
    runner.await.unwrap();
}

#[tokio::test]
async fn telegram_version_reports_build_information() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/bottest/setWebhook"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "ok": true,
            "result": true
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/bottest/sendMessage"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "ok": true,
            "result": {
                "message_id": 1,
                "date": 0,
                "chat": {"id": 1, "type": "private"},
                "text": "accepted"
            }
        })))
        .mount(&server)
        .await;

    let mut configuration = config(server.uri());
    configuration.telegram_webhook_url = "https://tracker.example/tg".to_owned();
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("tracker.sqlite");
    let (app, runner) = App::start(configuration, &path).await.unwrap();
    let app = Arc::new(app);

    let response = router(Arc::clone(&app))
        .oneshot(
            HttpRequest::builder()
                .method("POST")
                .uri("/tg")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&telegram_update(42, 1, 1_700_000_000, "/version")).unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);

    wait_for_requests(&server, 1).await;
    let request = send_requests(&server).await.pop().unwrap();
    let text = request.body_json::<serde_json::Value>().unwrap()["text"]
        .as_str()
        .unwrap()
        .to_owned();
    let build = crate::build_info();
    assert!(text.contains(&format!("Version: {}", build.version)));
    assert!(text.contains(&format!("Build time: {}", build.build_time)));
    assert!(text.contains(&format!("Git commit: {}", build.git_commit)));
    assert!(text.contains(&format!("Git dirty: {}", build.git_dirty)));

    app.shutdown();
    runner.await.unwrap();
}

#[tokio::test]
async fn telegram_webhook_registers_and_forwards_updates() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/bottest/setWebhook"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "ok": true,
            "result": true
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/bottest/sendMessage"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "ok": true,
            "result": {
                "message_id": 1,
                "date": 0,
                "chat": {"id": 1, "type": "private"},
                "text": "accepted"
            }
        })))
        .mount(&server)
        .await;

    let mut configuration = config(server.uri());
    configuration.telegram_webhook_url = "https://tracker.example/tg".to_owned();
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("tracker.sqlite");
    let (app, runner) = App::start(configuration, &path).await.unwrap();
    let app = Arc::new(app);

    let update = telegram_update(42, 1, 1_700_000_000, "/ok");
    let response = router(Arc::clone(&app))
        .oneshot(
            HttpRequest::builder()
                .method("POST")
                .uri("/tg")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&update).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);

    wait_for_state(&path, |state| matches!(state.hike, HikeState::Active(_))).await;
    wait_for_requests(&server, 1).await;
    sleep(Duration::from_millis(25)).await;

    let requests = server.received_requests().await.unwrap();
    let set_webhook = requests
        .iter()
        .find(|request| request.url.path().ends_with("/setWebhook"))
        .expect("webhook registration request");
    assert_eq!(
        set_webhook.body_json::<serde_json::Value>().unwrap()["url"],
        "https://tracker.example/tg"
    );
    assert!(
        !requests
            .iter()
            .any(|request| request.url.path().ends_with("/getUpdates"))
    );

    app.shutdown();
    runner.await.unwrap();
}

#[tokio::test]
async fn webhook_acknowledges_durable_enqueue_while_telegram_is_blocked() {
    let server = telegram_server(Duration::from_millis(500)).await;
    let directory = TempDir::new().unwrap();
    let (app, runner) = App::start(
        config(server.uri()),
        directory.path().join("tracker.sqlite"),
    )
    .await
    .unwrap();
    let app = Arc::new(app);

    assert_eq!(
        submit_mail(&app, "ok-1", "ALL OK").await,
        StatusCode::NO_CONTENT
    );
    sleep(Duration::from_millis(50)).await;
    let status = tokio::time::timeout(
        Duration::from_millis(100),
        submit_mail(&app, "queued-while-sending", "unknown"),
    )
    .await
    .expect("enqueue waited for a blocked Telegram action");
    assert_eq!(status, StatusCode::NO_CONTENT);

    wait_for_requests(&server, 2).await;
    app.shutdown();
    runner.await.unwrap();
}

#[tokio::test]
async fn ok_after_unrecognized_alert_sends_safety_recovery() {
    let server = telegram_server(Duration::ZERO).await;
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("tracker.sqlite");
    let (app, runner) = App::start(config(server.uri()), &path).await.unwrap();
    let app = Arc::new(app);

    assert_eq!(
        submit_mail(&app, "start", "ALL OK").await,
        StatusCode::NO_CONTENT
    );
    wait_for_requests(&server, 1).await;
    assert_eq!(
        submit_mail(&app, "unexpected", "raw unrecognized payload").await,
        StatusCode::NO_CONTENT
    );
    wait_for_requests(&server, 2).await;
    let state = wait_for_state(
        &path,
        |state| matches!(&state.hike, HikeState::Active(hike) if hike.safety_alerted),
    )
    .await;
    let HikeState::Active(hike) = state.hike else {
        panic!("expected active hike");
    };
    assert!(hike.safety_alert_action.is_none());

    sleep(Duration::from_millis(2)).await;
    assert_eq!(
        submit_mail(&app, "recovered", "ALL OK").await,
        StatusCode::NO_CONTENT
    );
    wait_for_requests(&server, 3).await;
    wait_for_state(
        &path,
        |state| matches!(&state.hike, HikeState::Active(hike) if !hike.safety_alerted),
    )
    .await;

    let requests = send_requests(&server).await;
    let alert_body = String::from_utf8_lossy(&requests[1].body);
    assert!(alert_body.contains("SAFETY ALERT"));
    assert!(alert_body.contains("raw unrecognized payload"));
    let recovery_body = String::from_utf8_lossy(&requests[2].body);
    assert!(recovery_body.contains("contact resumed"));

    app.shutdown();
    runner.await.unwrap();
}

#[tokio::test]
async fn finished_cooldown_boundary_is_exact() {
    let server = telegram_server(Duration::ZERO).await;
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("tracker.sqlite");
    let (app, runner) = App::start(config(server.uri()), &path).await.unwrap();
    let first = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
    enqueue_at(&app, "ok", "ALL OK", first).await;
    wait_for_state(&path, |state| matches!(state.hike, HikeState::Active(_))).await;

    let finished_at = first + Duration::from_secs(100);
    enqueue_at(&app, "finished", "FINISHED", finished_at).await;
    wait_for_state(&path, |state| matches!(state.hike, HikeState::Finished(_))).await;

    let boundary = finished_at + FINISHED_COOLDOWN;
    enqueue_at(&app, "boundary", "ALL OK", boundary).await;
    sleep(Duration::from_millis(50)).await;
    assert!(matches!(read_state(&path).hike, HikeState::Finished(_)));

    enqueue_at(
        &app,
        "new-hike",
        "ALL OK",
        boundary + Duration::from_secs(1),
    )
    .await;
    wait_for_state(&path, |state| matches!(state.hike, HikeState::Active(_))).await;

    app.shutdown();
    runner.await.unwrap();
}

#[tokio::test]
async fn newer_ok_cancels_and_replaces_deadlines_while_stale_ok_is_a_noop() {
    let server = telegram_server(Duration::ZERO).await;
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("tracker.sqlite");
    let (app, runner) = App::start(config(server.uri()), &path).await.unwrap();
    let first = SystemTime::now();
    enqueue_at(&app, "first", "ALL OK", first).await;
    let state = wait_for_state(&path, |state| {
        matches!(
            &state.hike,
            HikeState::Active(hike) if hike.owner_started_notified
        )
    })
    .await;
    let HikeState::Active(first_hike) = state.hike else {
        panic!("expected active hike");
    };
    let first_owner_deadline = first_hike.owner_alert_action.unwrap();
    let first_safety_deadline = first_hike.safety_alert_action.unwrap();

    let newer = first + Duration::from_secs(1);
    enqueue_at(&app, "newer", "ALL OK", newer).await;
    let state = wait_for_state(
        &path,
        |state| matches!(&state.hike, HikeState::Active(hike) if hike.last_ok_at == newer),
    )
    .await;
    let HikeState::Active(newer_hike) = state.hike else {
        panic!("expected active hike");
    };
    assert_ne!(newer_hike.owner_alert_action, Some(first_owner_deadline));
    assert_ne!(newer_hike.safety_alert_action, Some(first_safety_deadline));

    enqueue_at(&app, "stale", "ALL OK", first).await;
    sleep(Duration::from_millis(50)).await;
    let HikeState::Active(after_stale) = read_state(&path).hike else {
        panic!("expected active hike");
    };
    assert_eq!(after_stale.last_ok_at, newer);
    assert_eq!(
        after_stale.owner_alert_action,
        newer_hike.owner_alert_action
    );
    assert_eq!(
        after_stale.safety_alert_action,
        newer_hike.safety_alert_action
    );

    app.shutdown();
    runner.await.unwrap();
}

#[tokio::test]
async fn alert_revalidates_and_records_delivery_only_after_success() {
    let failing = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/bottest/sendMessage"))
        .respond_with(ResponseTemplate::new(400))
        .mount(&failing)
        .await;
    mount_delete_webhook(&failing).await;
    let last_ok_at = SystemTime::now() - OWNER_AFTER - Duration::from_secs(1);
    let mut state = TrackerState {
        hike: HikeState::Active(active(last_ok_at)),
        ..TrackerState::default()
    };
    let action = AlertAction {
        telegram: Telegram::new(&config(failing.uri())).await.unwrap(),
    };
    let alert = AlertParameters {
        expected_last_ok_at: last_ok_at,
        audience: Audience::Owner,
        payload: "owner timeout payload".to_owned(),
    };
    assert!(
        action
            .run(&mut state, AlertSignal::Overdue(alert.clone()))
            .await
            .is_err()
    );
    let HikeState::Active(hike) = &state.hike else {
        panic!("expected active hike");
    };
    assert!(!hike.owner_alerted);

    let working = telegram_server(Duration::ZERO).await;
    let action = AlertAction {
        telegram: Telegram::new(&config(working.uri())).await.unwrap(),
    };
    action
        .run(&mut state, AlertSignal::Overdue(alert.clone()))
        .await
        .unwrap();
    action
        .run(&mut state, AlertSignal::Overdue(alert))
        .await
        .unwrap();
    let HikeState::Active(hike) = &state.hike else {
        panic!("expected active hike");
    };
    assert!(hike.owner_alerted);
    assert_eq!(send_requests(&working).await.len(), 1);
    let requests = send_requests(&working).await;
    assert!(String::from_utf8_lossy(&requests[0].body).contains("owner timeout payload"));
}

#[tokio::test]
async fn alert_payload_is_delivered_once() {
    let server = telegram_server(Duration::ZERO).await;
    let telegram = Telegram::new(&config(server.uri())).await.unwrap();
    let recovered_at = SystemTime::now();
    let hike = active(recovered_at);
    let mut state = TrackerState {
        hike: HikeState::Active(hike),
        processed_ids: VecDeque::from(["unknown".to_owned()]),
    };

    let alert = AlertAction {
        telegram: telegram.clone(),
    };
    let parameters = AlertParameters {
        expected_last_ok_at: recovered_at,
        audience: Audience::Safety,
        payload: "SAFETY ALERT: unrecognized\n\nambiguous".to_owned(),
    };
    alert
        .run(&mut state, AlertSignal::Overdue(parameters.clone()))
        .await
        .unwrap();
    alert
        .run(&mut state, AlertSignal::Overdue(parameters.clone()))
        .await
        .unwrap();
    let HikeState::Active(hike) = &state.hike else {
        panic!("expected active hike");
    };
    assert!(hike.safety_alerted);
    assert_eq!(send_requests(&server).await.len(), 1);
}

#[tokio::test]
async fn failed_notification_is_recovered_after_reopen() {
    let failing = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/bottest/sendMessage"))
        .respond_with(ResponseTemplate::new(400))
        .mount(&failing)
        .await;
    mount_delete_webhook(&failing).await;
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("tracker.sqlite");
    let (app, runner) = App::start(config(failing.uri()), &path).await.unwrap();
    assert_eq!(
        submit(Arc::new(app), raw_mail("recoverable", "ALL OK")).await,
        StatusCode::NO_CONTENT
    );
    assert!(runner.await.is_err());
    assert!(matches!(read_state(&path).hike, HikeState::Idle));

    let working = telegram_server(Duration::ZERO).await;
    let (app, runner) = App::start(config(working.uri()), &path).await.unwrap();
    wait_for_requests(&working, 1).await;
    app.shutdown();
    runner.await.unwrap();
    let HikeState::Active(hike) = read_state(&path).hike else {
        panic!("expected active hike");
    };
    assert!(hike.owner_started_notified);
}
