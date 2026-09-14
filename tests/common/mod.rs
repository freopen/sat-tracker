use anyhow::{Context, Result, bail, ensure};
use axum::{
    Router,
    body::{Body, Bytes},
    extract::State,
    http::{HeaderMap, HeaderName, Method, StatusCode, Uri, header::CONTENT_TYPE},
    response::Response,
};
use reqwest::Client;
use serde_yaml::{Mapping, Value as YamlValue};
use std::{
    collections::VecDeque,
    fs,
    path::Path,
    process::Stdio,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    net::TcpListener,
    process::{Child, Command},
    sync::oneshot,
    task::JoinHandle,
    time::{sleep, timeout},
};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const STARTUP_TIMEOUT: Duration = Duration::from_secs(10);
const TIME_SETTLE_DELAY: Duration = Duration::from_millis(10);
const DEFAULT_SCENARIO_TIME: i64 = 1_700_000_000_000;
const E2E_START_TIME_ENV: &str = "SAT_TRACKER_E2E_START_TIME";

#[derive(Clone)]
struct BotState {
    expected: Arc<Mutex<VecDeque<ExpectedOut>>>,
    errors: Arc<Mutex<Vec<String>>>,
}

struct BotServer {
    url: String,
    state: BotState,
    stop: Option<oneshot::Sender<()>>,
    task: JoinHandle<()>,
}

#[derive(Clone)]
struct ExpectedOut {
    method: Method,
    path: String,
    headers: Vec<(HeaderName, String)>,
    body: Option<BodyExpectation>,
    response: ResponseSpec,
}

#[derive(Clone)]
enum BodyExpectation {
    Text(String),
    Json(serde_json::Value),
}

#[derive(Clone)]
struct ResponseSpec {
    status: u16,
    headers: Vec<(HeaderName, String)>,
    body: Option<Vec<u8>>,
}

struct ChildProcess {
    child: Child,
    base_url: String,
}

/// Run one transcript against the real binary, using a local Telegram API.
pub async fn run_scenario(path: &Path) -> Result<()> {
    let source =
        fs::read_to_string(path).with_context(|| format!("reading scenario {}", path.display()))?;
    let entries: Vec<YamlValue> = serde_yaml::from_str(&source)
        .with_context(|| format!("decoding scenario {}", path.display()))?;
    ensure!(!entries.is_empty(), "scenario {} is empty", path.display());

    let bot = BotServer::start().await?;
    let directory = tempfile::tempdir().context("creating scenario directory")?;
    let port = reserve_port().await?;
    write_config(directory.path(), port, &bot.url)?;
    let client = Client::builder()
        .no_proxy()
        .build()
        .context("building scenario HTTP client")?;
    let mut index = 0;
    let mut virtual_time = DEFAULT_SCENARIO_TIME;
    let mut pending_start_time = None;
    queue_following_outputs(&entries, &mut index, &bot.state, path)?;
    let mut child = Some(start_child(directory.path(), port, &client, virtual_time).await?);
    settle_async(&bot.state).await?;

    while index < entries.len() {
        let entry = &entries[index];
        if let Some(value) = mapping_value(entry, "in")? {
            let request = InputRequest::from_yaml(value)?;
            let request_path = request.path.clone();
            index += 1;
            let request_task = tokio::spawn(send_input(
                client.clone(),
                child
                    .as_ref()
                    .context("scenario child is not running")?
                    .base_url
                    .clone(),
                request,
            ));
            queue_following_outputs(&entries, &mut index, &bot.state, path)?;
            let response = timeout(REQUEST_TIMEOUT, request_task)
                .await
                .context("input request timed out")???;
            response
                .validate()
                .with_context(|| format!("validating input {request_path}"))?;
            settle_async(&bot.state).await?;
            continue;
        }
        if let Some(value) = mapping_value(entry, "wait")? {
            let text = value.as_str().context("wait must be a duration string")?;
            let duration = humantime::parse_duration(text)
                .with_context(|| format!("parsing wait duration {text:?}"))?;
            let millis = i64::try_from(duration.as_millis())
                .context("wait duration is too large for a Unix millisecond timestamp")?;
            ensure!(millis > 0, "wait duration must advance time");
            let timestamp = virtual_time
                .checked_add(millis)
                .context("wait duration overflows a Unix millisecond timestamp")?;
            ensure!(
                timestamp > virtual_time,
                "clock must advance from {virtual_time} to a later timestamp, got {timestamp}"
            );
            index += 1;
            if child.is_some() {
                settle_async(&bot.state).await?;
                let base_url = child
                    .as_ref()
                    .context("scenario child is not running")?
                    .base_url
                    .clone();
                queue_following_outputs(&entries, &mut index, &bot.state, path)?;
                forward_time(client.clone(), base_url, timestamp, &bot.state).await?;
            } else {
                pending_start_time = Some(timestamp);
            }
            virtual_time = timestamp;
            continue;
        }
        if mapping_value(entry, "out")?.is_some() {
            bail!(
                "out entry at index {index} was not immediately after an input or lifecycle entry"
            );
        }
        if mapping_value(entry, "restart")?.is_some() {
            ensure_bool(entry, "restart")?;
            index += 1;
            settle_async(&bot.state).await?;
            queue_following_outputs(&entries, &mut index, &bot.state, path)?;
            if let Some(running) = child.take() {
                stop_gracefully(running).await?;
            }
            let startup_time = pending_start_time.take().unwrap_or(virtual_time);
            child = Some(start_child(directory.path(), port, &client, startup_time).await?);
            settle_async(&bot.state).await?;
            continue;
        }
        if mapping_value(entry, "crash")?.is_some() {
            ensure_bool(entry, "crash")?;
            index += 1;
            settle_async(&bot.state).await?;
            let Some(mut running) = child.take() else {
                bail!("crash entry at index {index} has no running child");
            };
            running
                .child
                .kill()
                .await
                .context("crashing scenario child")?;
            let status = running.child.wait().await.context("waiting for crash")?;
            ensure!(!status.success(), "crashed child exited successfully");
            continue;
        }
        if let Some(value) = mapping_value(entry, "exit")? {
            let expected = value.as_str().context("exit must be a string")?;
            ensure!(
                expected == "failure" || expected == "success",
                "exit must be failure or success"
            );
            index += 1;
            let Some(mut running) = child.take() else {
                bail!("exit entry at index {index} has no running child");
            };
            queue_following_outputs(&entries, &mut index, &bot.state, path)?;
            let status = timeout(REQUEST_TIMEOUT, running.child.wait())
                .await
                .context("waiting for scenario child exit")??;
            ensure!(
                status.success() == (expected == "success"),
                "child exit status {:?} did not match exit: {expected}",
                status.code()
            );
            continue;
        }
        bail!("unknown scenario entry at index {index}");
    }

    wait_for_queue(&bot.state).await?;
    if let Some(mut running) = child.take() {
        if running
            .child
            .try_wait()
            .context("checking scenario child after scenario")?
            .is_none()
        {
            running
                .child
                .kill()
                .await
                .context("stopping scenario child after scenario")?;
        }
        let _ = running
            .child
            .wait()
            .await
            .context("waiting for scenario child after scenario")?;
    }
    ensure_no_transcript_errors(&bot.state)?;
    let remaining = bot
        .state
        .expected
        .lock()
        .expect("scenario queue mutex is not poisoned")
        .len();
    ensure!(
        remaining == 0,
        "scenario left {remaining} expected Telegram calls"
    );
    bot.stop().await;
    Ok(())
}

impl BotServer {
    async fn start() -> Result<Self> {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await?;
        let address = listener.local_addr()?;
        let state = BotState {
            expected: Arc::new(Mutex::new(VecDeque::new())),
            errors: Arc::new(Mutex::new(Vec::new())),
        };
        let (stop, receiver) = oneshot::channel();
        let app = Router::new()
            .fallback(bot_request)
            .with_state(state.clone());
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, app)
                .with_graceful_shutdown(async {
                    let _ = receiver.await;
                })
                .await;
        });
        Ok(Self {
            url: format!("http://{address}"),
            state,
            stop: Some(stop),
            task,
        })
    }

    async fn stop(mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        let _ = self.task.await;
    }
}

async fn bot_request(
    State(state): State<BotState>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let expected = state
        .expected
        .lock()
        .expect("scenario queue mutex is not poisoned")
        .pop_front();
    let Some(expected) = expected else {
        record_error(
            &state,
            format!("unexpected Telegram call {} {}", method, uri),
        );
        return response(
            StatusCode::OK,
            Some(br#"{"ok":true,"result":{"message_id":1,"date":1700000000,"chat":{"id":20,"type":"private"},"text":"sent"}}"#.to_vec()),
            &[],
        );
    };
    let mismatches = expected.mismatches(&method, &uri, &headers, &body);
    if !mismatches.is_empty() {
        record_error(
            &state,
            format!(
                "Telegram call {} {} mismatched: {mismatches:?}",
                method, uri
            ),
        );
        return response(
            StatusCode::OK,
            Some(br#"{"ok":true,"result":{"message_id":1,"date":1700000000,"chat":{"id":20,"type":"private"},"text":"sent"}}"#.to_vec()),
            &[],
        );
    }
    expected.response.into_response()
}

impl ExpectedOut {
    fn mismatches(
        &self,
        method: &Method,
        uri: &Uri,
        headers: &HeaderMap,
        body: &[u8],
    ) -> Vec<String> {
        let mut mismatches = Vec::new();
        if method != self.method {
            mismatches.push(format!("method expected {}, got {method}", self.method));
        }
        let actual_path = uri
            .path_and_query()
            .map_or(uri.path(), |value| value.as_str());
        if actual_path != self.path {
            mismatches.push(format!("path expected {}, got {actual_path}", self.path));
        }
        for (name, expected) in &self.headers {
            let actual = headers
                .get(name)
                .and_then(|value| value.to_str().ok())
                .unwrap_or("<missing>");
            if actual != expected {
                mismatches.push(format!(
                    "header {name} expected {expected:?}, got {actual:?}"
                ));
            }
        }
        if let Some(expected) = &self.body {
            match expected {
                BodyExpectation::Text(expected) => {
                    let actual = String::from_utf8_lossy(body);
                    if actual != expected.as_str() {
                        mismatches.push(format!("text body expected {expected:?}, got {actual:?}"));
                    }
                }
                BodyExpectation::Json(expected) => {
                    let actual = serde_json::from_slice::<serde_json::Value>(body)
                        .map_err(|error| error.to_string());
                    if actual.as_ref().ok() != Some(expected) {
                        mismatches.push(format!(
                            "JSON body expected {expected}, got {actual:?}; raw={}",
                            String::from_utf8_lossy(body)
                        ));
                    }
                }
            }
        } else if !body.is_empty() {
            mismatches.push("expected an empty body".to_owned());
        }
        mismatches
    }
}

impl ResponseSpec {
    fn into_response(self) -> Response {
        let status = StatusCode::from_u16(self.status).expect("scenario response status is valid");
        response(status, self.body, &self.headers)
    }
}

fn response(
    status: StatusCode,
    body: Option<Vec<u8>>,
    headers: &[(HeaderName, String)],
) -> Response {
    let mut builder = Response::builder().status(status);
    for (name, value) in headers {
        builder = builder.header(name, value);
    }
    builder
        .body(Body::from(body.unwrap_or_default()))
        .expect("scenario response is valid")
}

fn record_error(state: &BotState, error: String) {
    state
        .errors
        .lock()
        .expect("scenario error mutex is not poisoned")
        .push(error);
}

fn queue_following_outputs(
    entries: &[YamlValue],
    index: &mut usize,
    state: &BotState,
    scenario: &Path,
) -> Result<()> {
    while *index < entries.len() {
        let Some(value) = mapping_value(&entries[*index], "out")? else {
            break;
        };
        let expected = ExpectedOut::from_yaml(value)
            .with_context(|| format!("decoding out entry {} in {}", *index, scenario.display()))?;
        state
            .expected
            .lock()
            .expect("scenario queue mutex is not poisoned")
            .push_back(expected);
        *index += 1;
    }
    Ok(())
}

impl ExpectedOut {
    fn from_yaml(value: &YamlValue) -> Result<Self> {
        let map = value.as_mapping().context("out must be a mapping")?;
        let path = required_string(map, "path")?;
        let body = optional_body(map, "body")?;
        let method = if body.is_some() {
            Method::POST
        } else {
            Method::GET
        };
        let headers = parse_headers(map.get(YamlValue::String("headers".to_owned())))?;
        let response = map
            .get(YamlValue::String("response".to_owned()))
            .context("every out entry must contain response")?;
        let response = parse_response(response)?;
        Ok(Self {
            method,
            path,
            headers,
            body,
            response,
        })
    }
}

struct InputRequest {
    path: String,
    method: Method,
    headers: Vec<(HeaderName, String)>,
    body: Option<Vec<u8>>,
    response: InputResponse,
}

struct InputResponse {
    status: Option<u16>,
    headers: Vec<(HeaderName, String)>,
    body: Option<BodyExpectation>,
}

struct ActualResponse {
    status: StatusCode,
    headers: HeaderMap,
    body: Bytes,
    expectation: InputResponse,
}

impl InputRequest {
    fn from_yaml(value: &YamlValue) -> Result<Self> {
        let map = value.as_mapping().context("in must be a mapping")?;
        let path = required_string(map, "path")?;
        let raw_body = map.get(YamlValue::String("body".to_owned()));
        let body = raw_body
            .map(|value| request_body(value).map(body_bytes))
            .transpose()?;
        let mut headers = parse_headers(map.get(YamlValue::String("headers".to_owned())))?;
        if let Some(raw_body) = raw_body
            && matches!(raw_body, YamlValue::Mapping(_) | YamlValue::Sequence(_))
            && !headers.iter().any(|(name, _)| name == CONTENT_TYPE)
        {
            headers.push((CONTENT_TYPE, "application/json".to_owned()));
        }
        let response = map
            .get(YamlValue::String("response".to_owned()))
            .map(parse_input_response)
            .transpose()?
            .unwrap_or(InputResponse {
                status: None,
                headers: Vec::new(),
                body: None,
            });
        Ok(Self {
            path,
            method: if body.is_some() {
                Method::POST
            } else {
                Method::GET
            },
            headers,
            body,
            response,
        })
    }
}

fn body_bytes(body: BodyExpectation) -> Vec<u8> {
    match body {
        BodyExpectation::Text(value) => value.into_bytes(),
        BodyExpectation::Json(value) => {
            serde_json::to_vec(&value).expect("scenario JSON serializes")
        }
    }
}

async fn send_input(
    client: Client,
    base_url: String,
    request: InputRequest,
) -> Result<ActualResponse> {
    let mut builder = client.request(request.method, format!("{base_url}{}", request.path));
    for (name, value) in &request.headers {
        builder = builder.header(name, value);
    }
    let response = builder
        .body(request.body.unwrap_or_default())
        .send()
        .await
        .context("sending scenario input")?;
    let actual = ActualResponse {
        status: response.status(),
        headers: response.headers().clone(),
        body: response
            .bytes()
            .await
            .context("reading scenario response")?,
        expectation: request.response,
    };
    Ok(actual)
}

async fn forward_time(
    client: Client,
    base_url: String,
    timestamp: i64,
    state: &BotState,
) -> Result<()> {
    let mark = transcript_mark(state);
    sleep(TIME_SETTLE_DELAY).await;
    ensure_transcript_unchanged(state, mark, "before intermediate time")?;

    let intermediate = timestamp
        .checked_sub(1)
        .context("time timestamp is too small")?;
    send_time(&client, &base_url, intermediate).await?;
    sleep(TIME_SETTLE_DELAY).await;
    ensure_transcript_unchanged(state, mark, "at intermediate time")?;

    send_time(&client, &base_url, timestamp).await?;
    settle_async(state).await
}

async fn send_time(client: &Client, base_url: &str, timestamp: i64) -> Result<()> {
    let response = client
        .get(format!("{base_url}/time?ts={timestamp}"))
        .send()
        .await
        .context("forwarding scenario time")?;
    ensure!(
        response.status() == StatusCode::NO_CONTENT,
        "forwarding time {timestamp} returned {}",
        response.status()
    );
    Ok(())
}

#[derive(Clone, Copy)]
struct TranscriptMark {
    expected: usize,
    errors: usize,
}

fn transcript_mark(state: &BotState) -> TranscriptMark {
    TranscriptMark {
        expected: state
            .expected
            .lock()
            .expect("scenario queue mutex is not poisoned")
            .len(),
        errors: state
            .errors
            .lock()
            .expect("scenario error mutex is not poisoned")
            .len(),
    }
}

fn ensure_transcript_unchanged(state: &BotState, mark: TranscriptMark, phase: &str) -> Result<()> {
    let current = transcript_mark(state);
    ensure!(
        current.expected == mark.expected,
        "Telegram transcript changed {phase}: expected queue had {} entries, now has {}",
        mark.expected,
        current.expected
    );
    ensure!(
        current.errors == mark.errors,
        "Telegram transcript recorded an error {phase}"
    );
    Ok(())
}

impl ActualResponse {
    fn validate(self) -> Result<()> {
        if let Some(status) = self.expectation.status {
            ensure!(
                self.status.as_u16() == status,
                "input status expected {status}, got {}",
                self.status
            );
        } else {
            ensure!(
                self.status.is_success(),
                "input status was {}; body: {}",
                self.status,
                String::from_utf8_lossy(&self.body)
            );
        }
        for (name, expected) in &self.expectation.headers {
            let actual = self
                .headers
                .get(name)
                .and_then(|value| value.to_str().ok())
                .unwrap_or("<missing>");
            ensure!(
                actual == expected,
                "input header {name} expected {expected:?}, got {actual:?}"
            );
        }
        if let Some(expected) = &self.expectation.body {
            match expected {
                BodyExpectation::Text(expected) => ensure!(
                    String::from_utf8_lossy(&self.body) == expected.as_str(),
                    "input response body differed"
                ),
                BodyExpectation::Json(expected) => {
                    let actual: serde_json::Value = serde_json::from_slice(&self.body)
                        .context("decoding input response as JSON")?;
                    ensure!(actual == *expected, "input JSON response differed");
                }
            }
        }
        Ok(())
    }
}

fn parse_input_response(value: &YamlValue) -> Result<InputResponse> {
    let map = value
        .as_mapping()
        .context("input response must be a mapping")?;
    let status = map
        .get(YamlValue::String("status".to_owned()))
        .map(|value| {
            value
                .as_u64()
                .context("response status must be an integer")
                .and_then(|status| u16::try_from(status).context("response status is too large"))
        })
        .transpose()?;
    Ok(InputResponse {
        status,
        headers: parse_headers(map.get(YamlValue::String("headers".to_owned())))?,
        body: optional_body(map, "body")?,
    })
}

fn parse_response(value: &YamlValue) -> Result<ResponseSpec> {
    let map = value
        .as_mapping()
        .context("out response must be a mapping")?;
    let status = map
        .get(YamlValue::String("status".to_owned()))
        .map(|value| {
            value
                .as_u64()
                .context("response status must be an integer")
                .and_then(|status| u16::try_from(status).context("response status is too large"))
        })
        .transpose()?
        .unwrap_or(200);
    let body = map
        .get(YamlValue::String("body".to_owned()))
        .map(response_body)
        .transpose()?;
    Ok(ResponseSpec {
        status,
        headers: parse_headers(map.get(YamlValue::String("headers".to_owned())))?,
        body,
    })
}

fn optional_body(map: &Mapping, key: &str) -> Result<Option<BodyExpectation>> {
    map.get(YamlValue::String(key.to_owned()))
        .map(request_body)
        .transpose()
}

fn request_body(value: &YamlValue) -> Result<BodyExpectation> {
    match value {
        YamlValue::String(value) => Ok(BodyExpectation::Text(resolve_placeholders(value))),
        YamlValue::Mapping(_) | YamlValue::Sequence(_) => Ok(BodyExpectation::Json(
            resolve_placeholders_json(yaml_json(value)?),
        )),
        YamlValue::Null => bail!("request body must not be null"),
        _ => bail!("request body must be a string, mapping, or sequence"),
    }
}

fn response_body(value: &YamlValue) -> Result<Vec<u8>> {
    match value {
        YamlValue::String(value) => Ok(resolve_placeholders(value).into_bytes()),
        _ => Ok(serde_json::to_vec(&resolve_placeholders_json(yaml_json(
            value,
        )?))?),
    }
}

fn parse_headers(value: Option<&YamlValue>) -> Result<Vec<(HeaderName, String)>> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let map = value.as_mapping().context("headers must be a mapping")?;
    map.iter()
        .map(|(name, value)| {
            let name = name.as_str().context("header name must be a string")?;
            let name = HeaderName::try_from(name).context("invalid header name")?;
            let value = value.as_str().context("header value must be a string")?;
            Ok((name, resolve_placeholders(value)))
        })
        .collect()
}

fn yaml_json(value: &YamlValue) -> Result<serde_json::Value> {
    serde_json::from_value(serde_json::to_value(value)?).context("scenario JSON value is invalid")
}

fn resolve_placeholders_json(mut value: serde_json::Value) -> serde_json::Value {
    match &mut value {
        serde_json::Value::String(value) => *value = resolve_placeholders(value),
        serde_json::Value::Array(values) => {
            for value in values {
                *value = resolve_placeholders_json(value.take());
            }
        }
        serde_json::Value::Object(values) => {
            for value in values.values_mut() {
                *value = resolve_placeholders_json(value.take());
            }
        }
        serde_json::Value::Null | serde_json::Value::Bool(_) | serde_json::Value::Number(_) => {}
    }
    value
}

fn resolve_placeholders(value: &str) -> String {
    value
        .replace("${VERSION}", env!("CARGO_PKG_VERSION"))
        .replace(
            "${BUILD_TIME}",
            option_env!("VERGEN_BUILD_TIMESTAMP").unwrap_or("unknown"),
        )
        .replace(
            "${GIT_COMMIT}",
            option_env!("VERGEN_GIT_SHA").unwrap_or("unknown"),
        )
        .replace(
            "${GIT_DIRTY}",
            option_env!("VERGEN_GIT_DIRTY").unwrap_or("unknown"),
        )
}

fn mapping_value<'a>(entry: &'a YamlValue, key: &str) -> Result<Option<&'a YamlValue>> {
    let map = entry
        .as_mapping()
        .context("scenario entries must be mappings")?;
    Ok(map.get(YamlValue::String(key.to_owned())))
}

fn required_string(map: &Mapping, key: &str) -> Result<String> {
    map.get(YamlValue::String(key.to_owned()))
        .and_then(YamlValue::as_str)
        .map(str::to_owned)
        .with_context(|| format!("{key} must be a string"))
}

fn ensure_bool(entry: &YamlValue, key: &str) -> Result<()> {
    ensure!(
        mapping_value(entry, key)?.and_then(YamlValue::as_bool) == Some(true),
        "{key} must be true"
    );
    Ok(())
}

async fn reserve_port() -> Result<u16> {
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await?;
    Ok(listener.local_addr()?.port())
}

fn write_config(directory: &Path, port: u16, bot_url: &str) -> Result<()> {
    let config = format!(
        "ok_regex: '^OK$'\nfinished_regex: '^FINISHED$'\nowner_chat_id: 10\nsafety_chat_id: 20\ntelegram_api_url: '{bot_url}'\ntelegram_webhook_url: 'https://example.test/tg'\ntelegram_bot_token: 'test'\nlisten_address: '127.0.0.1:{port}'\n"
    );
    fs::write(directory.join("config.yaml"), config).context("writing scenario config")?;
    Ok(())
}

async fn start_child(
    directory: &Path,
    port: u16,
    client: &Client,
    startup_time: i64,
) -> Result<ChildProcess> {
    let mut child = Command::new(env!("CARGO_BIN_EXE_sat-tracker"))
        .current_dir(directory)
        .env("RUST_LOG", "sat_tracker=debug")
        .env(E2E_START_TIME_ENV, startup_time.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("starting scenario child")?;
    let base_url = format!("http://127.0.0.1:{port}");
    let deadline = tokio::time::Instant::now() + STARTUP_TIMEOUT;
    loop {
        if let Some(status) = child.try_wait().context("checking scenario child")? {
            bail!("scenario child exited during startup with {status}");
        }
        match client.get(format!("{base_url}/healthz")).send().await {
            Ok(response) if response.status() == StatusCode::NO_CONTENT => {
                return Ok(ChildProcess { child, base_url });
            }
            Ok(_) | Err(_) => {}
        }
        if tokio::time::Instant::now() >= deadline {
            let _ = child.kill().await;
            bail!("scenario child did not become ready");
        }
        sleep(Duration::from_millis(25)).await;
    }
}

async fn stop_gracefully(mut running: ChildProcess) -> Result<()> {
    let pid = running
        .child
        .id()
        .context("scenario child has no process ID")?;
    Command::new("kill")
        .args(["-TERM", &pid.to_string()])
        .status()
        .await
        .context("requesting scenario child shutdown")?
        .success()
        .then_some(())
        .context("scenario child did not accept SIGTERM")?;
    let status = timeout(REQUEST_TIMEOUT, running.child.wait())
        .await
        .context("waiting for graceful child shutdown")??;
    ensure!(
        status.success(),
        "scenario child exited unsuccessfully: {status}"
    );
    Ok(())
}

async fn wait_for_queue(state: &BotState) -> Result<()> {
    let deadline = tokio::time::Instant::now() + REQUEST_TIMEOUT;
    loop {
        if state
            .expected
            .lock()
            .expect("scenario queue mutex is not poisoned")
            .is_empty()
        {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            return Ok(());
        }
        sleep(Duration::from_millis(10)).await;
    }
}

async fn settle_async(state: &BotState) -> Result<()> {
    sleep(TIME_SETTLE_DELAY).await;
    wait_for_queue(state).await?;
    sleep(TIME_SETTLE_DELAY).await;
    ensure_no_transcript_errors(state)
}

fn ensure_no_transcript_errors(state: &BotState) -> Result<()> {
    let errors = state
        .errors
        .lock()
        .expect("scenario error mutex is not poisoned")
        .clone();
    ensure!(
        errors.is_empty(),
        "Telegram transcript mismatches: {errors:?}"
    );
    Ok(())
}
