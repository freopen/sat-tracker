#![allow(dead_code)]
use chrono::{DateTime, Utc};
use frankenstein::updates::Update;
use sat_tracker::{
    App, Config, Phase,
    entity::{inbox, runtime, tracker},
};
use sea_orm::{
    ColumnTrait, ConnectOptions, Database, DatabaseConnection, EntityTrait, PaginatorTrait,
    QueryFilter,
};
use std::sync::Arc;
use tempfile::TempDir;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{method, path},
};

pub const START: i64 = 1_700_000_000_000;
pub fn time(ms: i64) -> DateTime<Utc> {
    DateTime::<Utc>::from_timestamp_millis(ms).unwrap()
}
pub fn config(url: String) -> Config {
    Config {
        ok_regex: regex::Regex::new("^OK$").unwrap(),
        finished_regex: regex::Regex::new("^FINISHED$").unwrap(),
        owner_chat_id: 10,
        safety_chat_id: 20,
        telegram_api_url: url,
        telegram_webhook_url: "https://example.test/tg".to_owned(),
        telegram_bot_token: "test".to_owned(),
    }
}
pub struct Harness {
    pub dir: TempDir,
    pub server: MockServer,
    pub app: Arc<App>,
    pub db: DatabaseConnection,
}
impl Harness {
    pub async fn new() -> Self {
        let dir = TempDir::new().unwrap();
        let server = MockServer::start().await;
        let app = Arc::new(
            App::open(config(server.uri()), dir.path().join("test.sqlite"))
                .await
                .unwrap(),
        );
        let mut options = ConnectOptions::new(format!(
            "sqlite://{}",
            dir.path().join("test.sqlite").display()
        ));
        options.sqlx_logging(false);
        let db = Database::connect(options).await.unwrap();
        let this = Self {
            dir,
            server,
            app,
            db,
        };
        this.success().await;
        this
    }
    pub async fn success(&self) {
        Mock::given(method("POST"))
            .and(path("/bottest/sendMessage"))
            .respond_with(success())
            .mount(&self.server)
            .await;
    }
    pub async fn mail(&self, id: &str, body: &str, at: i64) {
        self.app
            .accept_mail(
                format!("Message-ID: <{id}>\r\nContent-Type: text/plain\r\n\r\n{body}")
                    .into_bytes(),
                time(at),
            )
            .await
            .unwrap();
    }
    pub async fn tick(&self, at: i64) -> Option<DateTime<Utc>> {
        self.app.tick(time(at)).await.unwrap()
    }
    pub async fn phase(&self) -> Phase {
        self.tracker().await.phase
    }
    pub async fn tracker(&self) -> tracker::Model {
        tracker::Entity::find_by_id(1)
            .one(&self.db)
            .await
            .unwrap()
            .unwrap()
    }
    pub async fn runtime(&self) -> runtime::Model {
        runtime::Entity::find_by_id(1)
            .one(&self.db)
            .await
            .unwrap()
            .unwrap()
    }
    pub async fn inbox_count(&self) -> u64 {
        inbox::Entity::find().count(&self.db).await.unwrap()
    }
    pub async fn pending_inbox_count(&self) -> u64 {
        inbox::Entity::find()
            .filter(inbox::Column::ProcessedAt.is_null())
            .count(&self.db)
            .await
            .unwrap()
    }
    pub async fn sends(&self) -> Vec<serde_json::Value> {
        self.server
            .received_requests()
            .await
            .unwrap()
            .into_iter()
            .filter(|r| r.url.path().ends_with("sendMessage"))
            .map(|r| serde_json::from_slice(&r.body).unwrap())
            .collect()
    }
}
pub fn success() -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(serde_json::json!({"ok":true,"result":{"message_id":1,"date":1,"chat":{"id":10,"type":"private"},"text":"sent"}}))
}
pub fn update(id: u32, chat: i64, at: i64, text: &str) -> Update {
    serde_json::from_value(serde_json::json!({"update_id":id,"message":{"message_id":id,"date":at / 1000,"chat":{"id":chat,"type":"private"},"text":text}})).unwrap()
}
