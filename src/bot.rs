use crate::config::Config;
use frankenstein::Error as FrankensteinError;
use std::{future::Future, time::Duration};

#[derive(Debug)]
pub(crate) struct TelegramError(FrankensteinError);

impl std::fmt::Display for TelegramError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.0 {
            FrankensteinError::Api(response) => {
                write!(formatter, "Telegram API error {}", response.error_code)
            }
            _ => formatter.write_str("Telegram request failed"),
        }
    }
}

impl std::error::Error for TelegramError {}

impl From<FrankensteinError> for TelegramError {
    fn from(error: FrankensteinError) -> Self {
        Self(error)
    }
}

async fn retry<T, F, Fut>(mut attempt: F) -> Result<T, TelegramError>
where
    T: Send,
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, TelegramError>>,
{
    for retry in 0..=2u8 {
        match attempt().await {
            Ok(value) => return Ok(value),
            Err(error) => {
                if retry == 2 || !retryable(&error.0) {
                    return Err(error);
                }
                let base = match retry {
                    0 => retry_duration(1),
                    1 => retry_duration(2),
                    _ => unreachable!(),
                };
                let delay = retry_after(&error.0)
                    .map_or(base, |seconds| base.max(Duration::from_secs(seconds)));
                crate::time::sleep(delay).await;
            }
        }
    }
    unreachable!()
}

fn retry_duration(seconds: u64) -> Duration {
    Duration::from_secs(seconds)
}

fn retryable(error: &FrankensteinError) -> bool {
    match error {
        FrankensteinError::Api(response) => {
            response.error_code == 429 || response.error_code >= 500
        }
        FrankensteinError::HttpReqwest(error) => {
            error.is_timeout()
                || error.is_connect()
                || error
                    .status()
                    .is_some_and(|status| status.is_server_error() || status.as_u16() == 429)
        }
        _ => false,
    }
}

fn retry_after(error: &FrankensteinError) -> Option<u64> {
    match error {
        FrankensteinError::Api(response) => response
            .parameters
            .and_then(|parameters| parameters.retry_after)
            .map(u64::from),
        _ => None,
    }
}

pub(crate) fn is_message_rejection(error: &anyhow::Error) -> bool {
    let Some(error) = error.downcast_ref::<TelegramError>() else {
        return false;
    };
    let FrankensteinError::Api(response) = &error.0 else {
        return false;
    };
    if response.error_code != 400 {
        return false;
    }
    let description = response.description.to_ascii_lowercase();
    description.contains("can't parse entities")
        || description.contains("can't parse rich message")
        || description.contains("message is too long")
}

pub(crate) fn safe_error(error: &anyhow::Error) -> String {
    error
        .downcast_ref::<TelegramError>()
        .map_or_else(|| error.to_string(), ToString::to_string)
}

#[cfg(not(test))]
mod imp {
    use super::*;
    use frankenstein::AsyncTelegramApi;
    use serde::{Serialize, de::DeserializeOwned};
    use std::path::PathBuf;

    #[derive(Debug, Clone)]
    pub(crate) struct Bot {
        inner: frankenstein::client_reqwest::Bot,
    }

    #[async_trait::async_trait]
    impl AsyncTelegramApi for Bot {
        type Error = TelegramError;

        async fn request<Params, Output>(
            &self,
            method: &str,
            params: Option<Params>,
        ) -> Result<Output, Self::Error>
        where
            Params: Serialize + std::fmt::Debug + Send,
            Output: DeserializeOwned,
        {
            let params = params.map(|params| encode(&params)).transpose()?;
            let value = retry(|| {
                let params = params.clone();
                async move {
                    self.inner
                        .request(method, params)
                        .await
                        .map_err(TelegramError::from)
                }
            })
            .await?;
            decode(value)
        }

        async fn request_with_form_data<Params, Output>(
            &self,
            method: &str,
            params: Params,
            files: Vec<(&str, PathBuf)>,
        ) -> Result<Output, Self::Error>
        where
            Params: Serialize + std::fmt::Debug + Send,
            Output: DeserializeOwned,
        {
            let params = encode(&params)?;
            let value = retry(|| {
                let params = params.clone();
                let files = files.clone();
                async move {
                    self.inner
                        .request_with_form_data(method, params, files)
                        .await
                        .map_err(TelegramError::from)
                }
            })
            .await?;
            decode(value)
        }
    }

    fn encode<Params>(params: &Params) -> Result<serde_json::Value, TelegramError>
    where
        Params: Serialize + std::fmt::Debug,
    {
        serde_json::to_value(params).map_err(|source| {
            FrankensteinError::JsonEncode {
                source,
                input: format!("{params:?}"),
            }
            .into()
        })
    }

    fn decode<Output>(value: serde_json::Value) -> Result<Output, TelegramError>
    where
        Output: DeserializeOwned,
    {
        serde_json::from_value(value.clone()).map_err(|source| {
            FrankensteinError::JsonDecode {
                source,
                input: value.to_string(),
            }
            .into()
        })
    }

    pub(crate) fn make_bot(config: &Config) -> anyhow::Result<Bot> {
        let client = frankenstein::reqwest::Client::builder()
            .retry(frankenstein::reqwest::retry::never())
            .timeout(Duration::from_secs(40))
            .connect_timeout(Duration::from_secs(10))
            .build()?;
        let inner = frankenstein::client_reqwest::Bot::builder()
            .api_url(format!(
                "{}/bot{}",
                config.telegram_api_url.trim_end_matches('/'),
                config.telegram_bot_token
            ))
            .client(client)
            .build();
        Ok(Bot { inner })
    }
}

#[cfg(test)]
mod imp {
    use super::{Config, TelegramError};
    use frankenstein::{
        AsyncTelegramApi,
        methods::{
            DeleteWebhookParams, GetUpdatesParams, SendMessageParams, SendRichMessageParams,
            SetWebhookParams,
        },
        response::MethodResponse,
        types::Message,
        updates::Update,
    };
    use mockall::mock;

    #[async_trait::async_trait]
    trait MockTelegramApi {
        async fn send_message(
            &self,
            params: &SendMessageParams,
        ) -> Result<MethodResponse<Message>, TelegramError>;

        async fn send_rich_message(
            &self,
            params: &SendRichMessageParams,
        ) -> Result<MethodResponse<Message>, TelegramError>;

        async fn get_updates(
            &self,
            params: &GetUpdatesParams,
        ) -> Result<MethodResponse<Vec<Update>>, TelegramError>;

        async fn set_webhook(
            &self,
            params: &SetWebhookParams,
        ) -> Result<MethodResponse<bool>, TelegramError>;

        async fn delete_webhook(
            &self,
            params: &DeleteWebhookParams,
        ) -> Result<MethodResponse<bool>, TelegramError>;
    }

    mock! {
        pub(crate) Bot {}

        #[async_trait::async_trait]
        impl MockTelegramApi for Bot {
            async fn send_message(
                &self,
                params: &SendMessageParams,
            ) -> Result<MethodResponse<Message>, TelegramError>;

            async fn send_rich_message(
                &self,
                params: &SendRichMessageParams,
            ) -> Result<MethodResponse<Message>, TelegramError>;

            async fn get_updates(
                &self,
                params: &GetUpdatesParams,
            ) -> Result<MethodResponse<Vec<Update>>, TelegramError>;

            async fn set_webhook(
                &self,
                params: &SetWebhookParams,
            ) -> Result<MethodResponse<bool>, TelegramError>;

            async fn delete_webhook(
                &self,
                params: &DeleteWebhookParams,
            ) -> Result<MethodResponse<bool>, TelegramError>;
        }

        impl Clone for Bot {
            fn clone(&self) -> Self;
        }
    }

    pub(crate) use MockBot as Bot;

    impl AsyncTelegramApi for MockBot {
        type Error = TelegramError;

        fn request<'a, 'b, 'async_trait, Params, Output>(
            &'a self,
            _method: &'b str,
            _params: Option<Params>,
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<Output = Result<Output, Self::Error>> + Send + 'async_trait,
            >,
        >
        where
            'a: 'async_trait,
            'b: 'async_trait,
            Params: serde::Serialize + std::fmt::Debug + Send + 'async_trait,
            Output: serde::de::DeserializeOwned + 'async_trait,
            Self: 'async_trait,
        {
            Box::pin(async {
                panic!("generic Telegram request is not available in unit-test mock")
            })
        }

        fn request_with_form_data<'a, 'b, 'c, 'async_trait, Params, Output>(
            &'a self,
            _method: &'b str,
            _params: Params,
            _files: Vec<(&'c str, std::path::PathBuf)>,
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<Output = Result<Output, Self::Error>> + Send + 'async_trait,
            >,
        >
        where
            'a: 'async_trait,
            'b: 'async_trait,
            'c: 'async_trait,
            Params: serde::Serialize + std::fmt::Debug + Send + 'async_trait,
            Output: serde::de::DeserializeOwned + 'async_trait,
            Self: 'async_trait,
        {
            Box::pin(async {
                panic!("generic Telegram multipart request is not available in unit-test mock")
            })
        }

        async fn send_message(
            &self,
            params: &SendMessageParams,
        ) -> Result<MethodResponse<Message>, Self::Error> {
            MockTelegramApi::send_message(self, params).await
        }

        fn send_rich_message<'a, 'b, 'async_trait>(
            &'a self,
            params: &'b SendRichMessageParams,
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<Output = Result<MethodResponse<Message>, Self::Error>>
                    + Send
                    + 'async_trait,
            >,
        >
        where
            'a: 'async_trait,
            'b: 'async_trait,
            Self: 'async_trait,
        {
            Box::pin(async move { MockTelegramApi::send_rich_message(self, params).await })
        }

        async fn get_updates(
            &self,
            params: &GetUpdatesParams,
        ) -> Result<MethodResponse<Vec<Update>>, Self::Error> {
            MockTelegramApi::get_updates(self, params).await
        }

        async fn set_webhook(
            &self,
            params: &SetWebhookParams,
        ) -> Result<MethodResponse<bool>, Self::Error> {
            MockTelegramApi::set_webhook(self, params).await
        }

        async fn delete_webhook(
            &self,
            params: &DeleteWebhookParams,
        ) -> Result<MethodResponse<bool>, Self::Error> {
            MockTelegramApi::delete_webhook(self, params).await
        }
    }

    pub(crate) fn make_bot(_config: &Config) -> anyhow::Result<Bot> {
        Ok(Bot::new())
    }
}

pub(crate) use imp::*;

#[cfg(test)]
mod tests {
    use super::*;
    use frankenstein::response::ErrorResponse;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    fn api_error(code: u64) -> TelegramError {
        FrankensteinError::Api(ErrorResponse {
            ok: false,
            description: "temporary".to_owned(),
            error_code: code,
            parameters: None,
        })
        .into()
    }

    #[test]
    fn telegram_failure_text_is_safe() {
        let error: anyhow::Error = TelegramError(FrankensteinError::Api(ErrorResponse {
            ok: false,
            description: "private body".to_owned(),
            error_code: 429,
            parameters: None,
        }))
        .into();
        assert_eq!(safe_error(&error), "Telegram API error 429");
    }

    #[cfg(not(feature = "e2e"))]
    #[tokio::test(start_paused = true)]
    async fn retries_transient_failures_with_a_bounded_attempt_count() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let counter = attempts.clone();
        let task = tokio::spawn(async move {
            retry(|| {
                let attempt = counter.fetch_add(1, Ordering::SeqCst);
                async move {
                    if attempt < 2 {
                        Err(api_error(500))
                    } else {
                        Ok(serde_json::json!(7))
                    }
                }
            })
            .await
        });

        tokio::task::yield_now().await;
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
        tokio::time::advance(Duration::from_secs(1)).await;
        tokio::task::yield_now().await;
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
        tokio::time::advance(Duration::from_secs(2)).await;
        assert_eq!(task.await.unwrap().unwrap(), serde_json::json!(7));
        assert_eq!(attempts.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn does_not_retry_permanent_client_errors() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let counter = attempts.clone();
        let error = retry(|| {
            counter.fetch_add(1, Ordering::SeqCst);
            async { Err::<serde_json::Value, _>(api_error(400)) }
        })
        .await
        .expect_err("permanent error must propagate");

        assert_eq!(attempts.load(Ordering::SeqCst), 1);
        assert_eq!(error.to_string(), "Telegram API error 400");
    }

    fn response<T>(result: T) -> frankenstein::response::MethodResponse<T> {
        frankenstein::response::MethodResponse {
            ok: true,
            description: None,
            result,
        }
    }

    fn message() -> frankenstein::types::Message {
        serde_json::from_value(serde_json::json!({
            "message_id": 1,
            "date": 1,
            "chat": {"id": 10, "type": "private"},
            "text": "sent"
        }))
        .unwrap()
    }

    #[tokio::test]
    async fn mock_bot_covers_the_telegram_methods_used_by_the_app() {
        use frankenstein::{
            AsyncTelegramApi,
            methods::{
                DeleteWebhookParams, GetUpdatesParams, SendRichMessageParams, SetWebhookParams,
            },
            rich_message::InputRichMessage,
            types::ChatId,
        };

        let mut bot = Bot::new();
        bot.expect_set_webhook()
            .withf(|params| params.url == "https://example.test/tg")
            .returning(|_| Ok(response(true)));
        bot.expect_delete_webhook()
            .returning(|_| Ok(response(true)));
        bot.expect_get_updates()
            .returning(|_| Ok(response(Vec::new())));
        bot.expect_send_rich_message()
            .withf(|params| {
                params.chat_id == ChatId::Integer(10)
                    && params
                        .rich_message
                        .markdown
                        .as_deref()
                        .is_some_and(|text| text == "hello")
            })
            .returning(|_| Ok(response(message())));

        AsyncTelegramApi::set_webhook(
            &bot,
            &SetWebhookParams::builder()
                .url("https://example.test/tg")
                .build(),
        )
        .await
        .unwrap();
        AsyncTelegramApi::delete_webhook(&bot, &DeleteWebhookParams::builder().build())
            .await
            .unwrap();
        AsyncTelegramApi::get_updates(&bot, &GetUpdatesParams::builder().build())
            .await
            .unwrap();
        AsyncTelegramApi::send_rich_message(
            &bot,
            &SendRichMessageParams::builder()
                .chat_id(10)
                .rich_message(InputRichMessage::builder().markdown("hello").build())
                .build(),
        )
        .await
        .unwrap();
    }
}
