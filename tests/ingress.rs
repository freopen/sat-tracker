mod common;
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use chrono::Utc;
use common::*;
use sat_tracker::{
    Phase,
    entity::{inbox, runtime},
};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, EntityTrait, IntoActiveModel, PaginatorTrait, QueryFilter, Set,
};
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::sync::Notify;
use tower::ServiceExt;
use wiremock::{Mock, ResponseTemplate, matchers::path};

#[tokio::test]
async fn ingress_waits_for_inline_send_then_commits_and_wakes() {
    let h = Harness::new().await;
    h.server.reset().await;
    let entered = Arc::new(Notify::new());
    let signal = entered.clone();
    Mock::given(path("/bottest/sendRichMessage"))
        .respond_with(move |_: &wiremock::Request| {
            signal.notify_one();
            success().set_delay(Duration::from_millis(300))
        })
        .mount(&h.server)
        .await;
    h.mail("start", "OK", START).await;
    let app = h.app.clone();
    let tick = tokio::spawn(async move { app.tick(time(START)).await });
    tokio::time::timeout(Duration::from_secs(3), entered.notified())
        .await
        .unwrap();
    let ingress = h.app.accept_mail(
        b"Message-ID: <finish>\r\n\r\nFINISHED".to_vec(),
        time(START + 1000),
    );
    tokio::pin!(ingress);
    assert!(
        tokio::time::timeout(Duration::from_millis(50), &mut ingress)
            .await
            .is_err()
    );
    tick.await.unwrap().unwrap();
    ingress.await.unwrap();
    assert_eq!(h.pending_inbox_count().await, 1);
    assert_eq!(h.runtime().await.next_tick_at, Some(time(START + 1000)));
    h.tick(START + 1000).await;
    assert_eq!(h.phase().await, Phase::Finished);
}

#[tokio::test]
async fn http_acknowledges_only_committed_input_and_reports_lock_timeout() {
    let h = Harness::new().await;
    let router = sat_tracker::router(h.app.clone());
    let request = |body: &'static str| Request::post("/mail").body(Body::from(body)).unwrap();
    assert_eq!(
        router.clone().oneshot(request("")).await.unwrap().status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        router
            .clone()
            .oneshot(request("Message-ID: <one>\r\n\r\nOK"))
            .await
            .unwrap()
            .status(),
        StatusCode::NO_CONTENT
    );
    assert_eq!(h.inbox_count().await, 1);
    // A separate SQLite writer models the lock held throughout a tick's API calls.
    let tx = sea_orm::TransactionTrait::begin_with_options(
        &h.db,
        sea_orm::TransactionOptions {
            sqlite_transaction_mode: Some(sea_orm::SqliteTransactionMode::Immediate),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let response = tokio::time::timeout(
        Duration::from_secs(8),
        router
            .clone()
            .oneshot(request("Message-ID: <two>\r\n\r\nOK")),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    tx.rollback().await.unwrap();
    assert_eq!(h.inbox_count().await, 1);
    assert_eq!(
        router
            .clone()
            .oneshot(request("Message-ID: <two>\r\n\r\nOK"))
            .await
            .unwrap()
            .status(),
        StatusCode::NO_CONTENT
    );
    let response = router
        .clone()
        .oneshot(Request::get("/healthz").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn http_rejects_malformed_telegram_json() {
    let h = Harness::new().await;
    let router = sat_tracker::router(h.app.clone());
    let response = router
        .oneshot(
            Request::post("/tg")
                .header("content-type", "application/json")
                .body(Body::from("{"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(h.inbox_count().await, 0);
}

#[tokio::test]
async fn webhook_accepts_json() {
    let h = Harness::new().await;
    let router = sat_tracker::router(h.app.clone());
    let request = Request::post("/tg")
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_vec(&update(7, 10, START, "/version")).unwrap(),
        ))
        .unwrap();
    assert_eq!(
        router.oneshot(request).await.unwrap().status(),
        StatusCode::NO_CONTENT
    );
    h.tick(START).await;
    assert_eq!(h.inbox_count().await, 1);
    assert_eq!(h.sends().await.len(), 1);
}

#[tokio::test]
async fn telegram_polling_resumes_persisted_offset_and_deduplicates() {
    let h = Harness::new().await;
    let mut runtime = runtime::Entity::find_by_id(1)
        .one(&h.db)
        .await
        .unwrap()
        .unwrap()
        .into_active_model();
    runtime.telegram_poll_offset = Set(42);
    runtime.update(&h.db).await.unwrap();
    Mock::given(path("/bottest/deleteWebhook"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!({"ok":true,"result":true})),
        )
        .mount(&h.server)
        .await;
    let polls = Arc::new(AtomicUsize::new(0));
    let count = polls.clone();
    Mock::given(path("/bottest/getUpdates"))
        .respond_with(move |request: &wiremock::Request| {
            let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
            let first = count.fetch_add(1, Ordering::SeqCst) == 0;
            assert_eq!(body["offset"], if first { 42 } else { 43 });
            let result = if first {
                let duplicate = update(42, 10, START, "/version");
                vec![duplicate.clone(), duplicate]
            } else {
                vec![]
            };
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"ok":true,"result":result}))
                .set_delay(Duration::from_millis(30))
        })
        .mount(&h.server)
        .await;
    let mut cfg = config(h.server.uri());
    cfg.telegram_webhook_url.clear();
    let app = Arc::new(
        sat_tracker::App::open(cfg, h.dir.path().join("test.sqlite"))
            .await
            .unwrap(),
    );
    let runner = tokio::spawn(app.clone().run());
    tokio::time::timeout(Duration::from_secs(4), async {
        loop {
            if polls.load(Ordering::SeqCst) >= 2
                && inbox::Entity::find()
                    .filter(inbox::Column::ExternalId.eq("42"))
                    .filter(inbox::Column::ProcessedAt.is_not_null())
                    .count(&h.db)
                    .await
                    .unwrap()
                    == 1
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    app.shutdown();
    runner.await.unwrap().unwrap();
    assert_eq!(h.runtime().await.telegram_poll_offset, 43);
    assert_eq!(h.inbox_count().await, 1);
}

#[tokio::test]
async fn concurrent_ticks_serialize_and_send_start_once() {
    let h = Harness::new().await;
    h.mail("start", "OK", START).await;
    let (a, b) = tokio::join!(h.app.tick(time(START)), h.app.tick(time(START)));
    a.unwrap();
    b.unwrap();
    assert_eq!(h.sends().await.len(), 2);
}

#[tokio::test]
async fn scheduler_reacts_to_ingress_and_registers_webhook() {
    let h = Harness::new().await;
    Mock::given(path("/bottest/setWebhook"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!({"ok":true,"result":true})),
        )
        .expect(1)
        .mount(&h.server)
        .await;
    let runner = tokio::spawn(h.app.clone().run());
    tokio::time::timeout(Duration::from_secs(3), async {
        while h.runtime().await.last_tick_at.is_none() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    h.app
        .accept_mail(b"Message-ID: <wake>\r\n\r\nOK".to_vec(), Utc::now())
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        while h.phase().await != Phase::Active {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    h.app.shutdown();
    runner.await.unwrap().unwrap();
    assert_eq!(h.sends().await.len(), 2);
}

#[tokio::test]
async fn retry_after_delays_the_whole_tick_even_when_ingress_wakes_scheduler() {
    let h = Harness::new().await;
    h.server.reset().await;
    Mock::given(path("/bottest/setWebhook"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!({"ok":true,"result":true})),
        )
        .mount(&h.server)
        .await;
    let attempted = Arc::new(Notify::new());
    let notify = attempted.clone();
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = calls.clone();
    let first_at = Arc::new(std::sync::Mutex::new(None::<std::time::Instant>));
    let recorded = first_at.clone();
    Mock::given(path("/bottest/sendRichMessage")).respond_with(move |_: &wiremock::Request| {
        if counter.fetch_add(1, Ordering::SeqCst) == 0 {
            *recorded.lock().unwrap() = Some(std::time::Instant::now());
            notify.notify_one();
            ResponseTemplate::new(429).set_body_json(serde_json::json!({"ok":false,"error_code":429,"description":"retry later","parameters":{"retry_after":6}}))
        } else {
            assert!(recorded.lock().unwrap().unwrap().elapsed() >= Duration::from_secs(6));
            success()
        }
    }).mount(&h.server).await;
    h.app
        .accept_mail(b"Message-ID: <retry>\r\n\r\nHELP".to_vec(), Utc::now())
        .await
        .unwrap();
    let runner = tokio::spawn(h.app.clone().run());
    tokio::time::timeout(Duration::from_secs(3), attempted.notified())
        .await
        .unwrap();
    h.app
        .accept_telegram(update(99, 10, START, "/version"), Utc::now())
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    tokio::time::timeout(Duration::from_secs(8), async {
        while h.pending_inbox_count().await != 0 {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    h.app.shutdown();
    runner.await.unwrap().unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 5);
}
