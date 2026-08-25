//! Persistent Responses API WebSocket transport for ChatGPT subscriptions.
//!
//! This follows the transport contract used by the open-source Codex CLI:
//! one provider-scoped connection is reused across requests, compatible
//! requests send only their incremental input with `previous_response_id`,
//! and a provider that cannot upgrade is disabled for the rest of the
//! sampler session so the caller can use the ordinary HTTP/SSE transport.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use futures_util::stream::{self, BoxStream};
use futures_util::{SinkExt, StreamExt};
use reqwest::StatusCode;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde_json::{Map, Value};
use tokio::net::TcpStream;
use tokio::sync::{Mutex, OwnedMutexGuard, mpsc};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::{Error as WebSocketError, Message};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};

use xai_grok_sampling_types::{ApiBackend, ApiErrorCode, SamplingError};

use crate::config::{AuthScheme, OriginClientInfo, SamplerConfig};

const RESPONSES_WEBSOCKET_BETA: &str = "responses_websockets=2026-02-06";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const STREAM_CHANNEL_CAPACITY: usize = 1600;

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// Request identity copied into the WebSocket handshake and request metadata.
#[derive(Clone, Debug, Default)]
pub(crate) struct WebSocketRequestMetadata {
    pub(crate) conversation_id: String,
    pub(crate) request_id: String,
    pub(crate) session_id: String,
    pub(crate) turn_id: Option<String>,
}

/// Result of attempting the WebSocket transport.
pub(crate) enum ResponsesWebSocketAttempt {
    Stream {
        raw_events: BoxStream<'static, Result<String, SamplingError>>,
        response_headers: HeaderMap,
        connection_reused: bool,
    },
    /// The endpoint explicitly cannot upgrade (HTTP 426), or WebSockets were
    /// already disabled for this sampler session.
    FallbackToHttp,
}

/// Fields that determine whether a persistent socket can safely survive a
/// sampler configuration update. The model and sampling knobs are deliberately
/// absent: Codex reuses the connection for those changes and simply sends a
/// full request when request properties no longer match.
#[derive(Clone, PartialEq, Eq)]
struct WebSocketConfigurationKey {
    base_url: String,
    responses_backend: bool,
    auth_scheme: AuthScheme,
    api_key: Option<String>,
    extra_headers: indexmap::IndexMap<String, String>,
    origin_client: Option<OriginClientInfo>,
    client_identifier: Option<String>,
    deployment_id: Option<String>,
    user_id: Option<String>,
    client_version: Option<String>,
    doom_loop_check: bool,
    has_bearer_resolver: bool,
    has_header_injector: bool,
}

impl WebSocketConfigurationKey {
    fn from_config(config: &SamplerConfig) -> Self {
        Self {
            base_url: config.base_url.trim_end_matches('/').to_ascii_lowercase(),
            responses_backend: config.api_backend == ApiBackend::Responses,
            auth_scheme: config.auth_scheme,
            api_key: config.api_key.clone(),
            extra_headers: config.extra_headers.clone(),
            origin_client: config.origin_client.clone(),
            client_identifier: config.client_identifier.clone(),
            deployment_id: config.deployment_id.clone(),
            user_id: config.user_id.clone(),
            client_version: config.client_version.clone(),
            doom_loop_check: config.doom_loop_recovery.is_some(),
            // The shell reconstructs equivalent resolver/injector wrappers on
            // every model step. Their allocation address is not provider
            // identity; comparing it would discard Codex's persistent socket
            // before every tool continuation. A token refresh still changes
            // `api_key`, while adding/removing either hook remains material.
            has_bearer_resolver: config.bearer_resolver.is_some(),
            has_header_injector: config.header_injector.is_some(),
        }
    }
}

#[derive(Clone)]
struct CompletedExchange {
    full_request: Value,
    response_id: String,
    output_items: Vec<Value>,
}

#[derive(Default)]
struct WebSocketSession {
    socket: Option<Socket>,
    response_headers: HeaderMap,
    last_exchange: Option<CompletedExchange>,
}

/// Session-scoped WebSocket state shared by all per-request sampling clients.
pub(crate) struct ResponsesWebSocketState {
    configuration: WebSocketConfigurationKey,
    disabled: AtomicBool,
    session: Arc<Mutex<WebSocketSession>>,
}

impl std::fmt::Debug for ResponsesWebSocketState {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ResponsesWebSocketState")
            .field("disabled", &self.disabled.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}

impl ResponsesWebSocketState {
    pub(crate) fn new(config: &SamplerConfig) -> Self {
        Self {
            configuration: WebSocketConfigurationKey::from_config(config),
            disabled: AtomicBool::new(false),
            session: Arc::new(Mutex::new(WebSocketSession::default())),
        }
    }

    pub(crate) fn matches_config(&self, config: &SamplerConfig) -> bool {
        self.configuration == WebSocketConfigurationKey::from_config(config)
    }

    pub(crate) fn enabled_for(&self, base_url: &str, backend: &ApiBackend) -> bool {
        *backend == ApiBackend::Responses
            && !self.disabled.load(Ordering::Relaxed)
            && websocket_url(base_url).is_some()
    }

    /// Permanently select HTTP for this sampler session. Returns `true` only
    /// for the call that actually activated fallback.
    pub(crate) async fn force_http_fallback(&self) -> bool {
        if self.disabled.swap(true, Ordering::Relaxed) {
            return false;
        }
        *self.session.lock().await = WebSocketSession::default();
        true
    }

    pub(crate) async fn stream(
        &self,
        base_url: &str,
        request_body: Value,
        headers: HeaderMap,
        metadata: WebSocketRequestMetadata,
        idle_timeout: Duration,
    ) -> Result<ResponsesWebSocketAttempt, SamplingError> {
        if self.disabled.load(Ordering::Relaxed) {
            return Ok(ResponsesWebSocketAttempt::FallbackToHttp);
        }
        let Some(url) = websocket_url(base_url) else {
            return Ok(ResponsesWebSocketAttempt::FallbackToHttp);
        };
        self.stream_at_url(url.as_str(), request_body, headers, metadata, idle_timeout)
            .await
    }

    async fn stream_at_url(
        &self,
        url: &str,
        request_body: Value,
        mut headers: HeaderMap,
        metadata: WebSocketRequestMetadata,
        idle_timeout: Duration,
    ) -> Result<ResponsesWebSocketAttempt, SamplingError> {
        if self.disabled.load(Ordering::Relaxed) {
            return Ok(ResponsesWebSocketAttempt::FallbackToHttp);
        }

        apply_handshake_headers(&mut headers, &metadata);
        let mut session = Arc::clone(&self.session).lock_owned().await;
        let connection_reused = session.socket.is_some();

        if session.socket.is_none() {
            match connect(url, headers).await {
                Ok((socket, response_headers)) => {
                    session.socket = Some(socket);
                    session.response_headers = response_headers;
                }
                Err(ConnectFailure::UpgradeRequired) => {
                    self.disabled.store(true, Ordering::Relaxed);
                    *session = WebSocketSession::default();
                    tracing::warn!(
                        target: crate::sampling_log::TARGET,
                        transport = "responses_websocket",
                        "Responses WebSocket upgrade unavailable; using HTTP/SSE for this session"
                    );
                    return Ok(ResponsesWebSocketAttempt::FallbackToHttp);
                }
                Err(ConnectFailure::Error(error)) => {
                    *session = WebSocketSession::default();
                    return Err(error);
                }
            }
        }

        let response_headers = session.response_headers.clone();
        let (wire_request, unchained_reason) = prepare_websocket_request(
            &request_body,
            session.last_exchange.as_ref(),
            &metadata,
            timestamp_millis(),
        );
        let request_text =
            serde_json::to_string(&wire_request).map_err(SamplingError::Serialization)?;

        let send_result = {
            let socket = session
                .socket
                .as_mut()
                .expect("socket was connected before sending request");
            tokio::time::timeout(
                idle_timeout,
                socket.send(Message::Text(request_text.into())),
            )
            .await
        };
        match send_result {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                *session = WebSocketSession::default();
                return Err(SamplingError::EventStreamError(format!(
                    "failed to send Responses WebSocket request: {error}"
                )));
            }
            Err(_) => {
                *session = WebSocketSession::default();
                return Err(SamplingError::EventStreamError(
                    "timed out sending Responses WebSocket request".to_owned(),
                ));
            }
        }

        tracing::info!(
            target: crate::sampling_log::TARGET,
            event = "responses_transport",
            transport = "websocket",
            connection_reused,
            chained = wire_request.get("previous_response_id").is_some(),
            unchained_reason = unchained_reason.unwrap_or(""),
            "streaming Responses API request over WebSocket"
        );

        let (event_tx, event_rx) = mpsc::channel(STREAM_CHANNEL_CAPACITY);
        tokio::spawn(read_response(session, event_tx, request_body, idle_timeout));
        let raw_events = stream::unfold(event_rx, |mut receiver| async move {
            receiver.recv().await.map(|event| (event, receiver))
        })
        .boxed();

        Ok(ResponsesWebSocketAttempt::Stream {
            raw_events,
            response_headers,
            connection_reused,
        })
    }
}

/// Return the Responses WebSocket URL only for the built-in ChatGPT Codex
/// endpoint. URL parsing plus exact host/path matching prevents lookalike
/// hosts and custom OpenAI-compatible providers from being opted in.
pub(crate) fn websocket_url(base_url: &str) -> Option<reqwest::Url> {
    let mut url = reqwest::Url::parse(base_url.trim()).ok()?;
    if !url
        .host_str()
        .is_some_and(|host| host.eq_ignore_ascii_case("chatgpt.com"))
        || url.path().trim_end_matches('/') != "/backend-api/codex"
    {
        return None;
    }
    match url.scheme() {
        "https" => url.set_scheme("wss").ok()?,
        "http" => url.set_scheme("ws").ok()?,
        _ => return None,
    }
    url.set_path("/backend-api/codex/responses");
    Some(url)
}

fn apply_handshake_headers(headers: &mut HeaderMap, metadata: &WebSocketRequestMetadata) {
    headers.insert(
        HeaderName::from_static("openai-beta"),
        HeaderValue::from_static(RESPONSES_WEBSOCKET_BETA),
    );
    insert_header(headers, "x-client-request-id", &metadata.conversation_id);
    insert_header(headers, "session-id", &metadata.session_id);
    insert_header(headers, "thread-id", &metadata.conversation_id);
}

fn insert_header(headers: &mut HeaderMap, name: &'static str, value: &str) {
    if value.is_empty() {
        return;
    }
    if let Ok(value) = HeaderValue::from_str(value) {
        headers.insert(HeaderName::from_static(name), value);
    }
}

enum ConnectFailure {
    UpgradeRequired,
    Error(SamplingError),
}

async fn connect(url: &str, headers: HeaderMap) -> Result<(Socket, HeaderMap), ConnectFailure> {
    let mut request = url.into_client_request().map_err(|error| {
        ConnectFailure::Error(SamplingError::EventStreamError(format!(
            "failed to build Responses WebSocket request: {error}"
        )))
    })?;
    request.headers_mut().extend(headers);

    let connected = tokio::time::timeout(CONNECT_TIMEOUT, connect_async(request))
        .await
        .map_err(|_| {
            ConnectFailure::Error(SamplingError::EventStreamError(
                "Responses WebSocket connection timed out".to_owned(),
            ))
        })?;

    match connected {
        Ok((socket, response)) => Ok((socket, response.headers().clone())),
        Err(WebSocketError::Http(response))
            if response.status() == StatusCode::UPGRADE_REQUIRED =>
        {
            Err(ConnectFailure::UpgradeRequired)
        }
        Err(WebSocketError::Http(response)) => {
            let status = response.status();
            let message = response
                .body()
                .as_deref()
                .map(String::from_utf8_lossy)
                .map(|body| body.trim().to_owned())
                .filter(|body| !body.is_empty())
                .unwrap_or_else(|| format!("Responses WebSocket handshake failed ({status})"));
            if status == StatusCode::UNAUTHORIZED {
                return Err(ConnectFailure::Error(SamplingError::auth_unknown(format!(
                    "Unauthorized (401) from {url}: {message}"
                ))));
            }
            Err(ConnectFailure::Error(SamplingError::Api {
                status,
                message,
                model_metadata: None,
                retry_after_secs: retry_after(response.headers()),
                should_retry: should_retry(response.headers()),
                error_code: None,
            }))
        }
        Err(error) => Err(ConnectFailure::Error(SamplingError::EventStreamError(
            format!("Responses WebSocket connection failed: {error}"),
        ))),
    }
}

fn prepare_websocket_request(
    full_request: &Value,
    previous: Option<&CompletedExchange>,
    metadata: &WebSocketRequestMetadata,
    request_start_ms: u128,
) -> (Value, Option<&'static str>) {
    let mut wire_request = full_request.clone();
    let (incremental, unchained_reason) = match previous {
        Some(previous) => match incremental_input(previous, full_request) {
            Ok(incremental) => (Some(incremental), None),
            Err(reason) => (None, Some(reason)),
        },
        None => (None, Some("no_previous_response")),
    };

    if let Some((response_id, input)) = incremental {
        wire_request["input"] = Value::Array(input);
        wire_request["previous_response_id"] = Value::String(response_id);
    } else if let Some(object) = wire_request.as_object_mut() {
        object.remove("previous_response_id");
    }

    wire_request["type"] = Value::String("response.create".to_owned());
    let client_metadata = wire_request
        .as_object_mut()
        .expect("Responses request serializes as an object")
        .entry("client_metadata")
        .or_insert_with(|| Value::Object(Map::new()));
    if !client_metadata.is_object() {
        *client_metadata = Value::Object(Map::new());
    }
    let client_metadata = client_metadata
        .as_object_mut()
        .expect("client_metadata was normalized to an object");
    insert_metadata(client_metadata, "session_id", &metadata.session_id);
    insert_metadata(client_metadata, "thread_id", &metadata.conversation_id);
    insert_metadata(client_metadata, "turn_id", &metadata.request_id);
    if let Some(turn_id) = metadata.turn_id.as_deref() {
        insert_metadata(client_metadata, "x-grog-turn-index", turn_id);
    }
    client_metadata.insert(
        "x-codex-ws-stream-request-start-ms".to_owned(),
        Value::String(request_start_ms.to_string()),
    );
    (wire_request, unchained_reason)
}

fn insert_metadata(metadata: &mut Map<String, Value>, name: &str, value: &str) {
    if !value.is_empty() {
        metadata.insert(name.to_owned(), Value::String(value.to_owned()));
    }
}

fn incremental_input(
    previous: &CompletedExchange,
    current_request: &Value,
) -> Result<(String, Vec<Value>), &'static str> {
    if previous.response_id.is_empty() {
        tracing::debug!("incremental WebSocket request unavailable: missing response id");
        return Err("missing_response_id");
    }
    if request_properties(&previous.full_request) != request_properties(current_request) {
        tracing::debug!("incremental WebSocket request unavailable: request properties changed");
        return Err("request_properties_changed");
    }

    let previous_input = previous
        .full_request
        .get("input")
        .and_then(Value::as_array)
        .ok_or("previous_input_not_array")?;
    let current_input = current_request
        .get("input")
        .and_then(Value::as_array)
        .ok_or("current_input_not_array")?;
    let baseline_len = previous_input
        .len()
        .checked_add(previous.output_items.len())
        .ok_or("baseline_length_overflow")?;
    if current_input.len() < baseline_len {
        tracing::debug!(
            previous_input_len = previous_input.len(),
            previous_output_len = previous.output_items.len(),
            current_input_len = current_input.len(),
            "incremental WebSocket request unavailable: current input is shorter than baseline"
        );
        return Err("current_input_shorter_than_baseline");
    }

    for (index, (baseline, current)) in previous_input
        .iter()
        .chain(previous.output_items.iter())
        .zip(current_input.iter().take(baseline_len))
        .enumerate()
    {
        let matches = if index < previous_input.len() {
            request_item_matches(baseline, current)
        } else {
            response_item_matches_request_item(baseline, current)
        };
        if !matches {
            tracing::debug!(
                index,
                baseline_type = item_type(baseline),
                current_type = item_type(current),
                "incremental WebSocket request unavailable: context prefix changed"
            );
            return Err("context_prefix_changed");
        }
    }

    Ok((
        previous.response_id.clone(),
        current_input[baseline_len..].to_vec(),
    ))
}

/// Match two already-serialized request items while ignoring the private
/// metadata that the ChatGPT backend may echo only once. This is the JSON
/// equivalent of Codex's `response_items_equal_ignoring_internal_metadata`.
fn request_item_matches(previous: &Value, current: &Value) -> bool {
    if previous == current {
        return true;
    }
    normalized_item(previous, &[]) == normalized_item(current, &[])
}

/// Compare a server output item with the request item produced after GROG's
/// typed conversation round trip. Unlike Codex's native `ResponseItem`, our
/// domain model intentionally removes output-only fields before replay:
/// `status` on reasoning, and `id`/`status` on function calls. Assistant
/// output messages also become the compact input-message representation.
fn response_item_matches_request_item(response: &Value, request: &Value) -> bool {
    if response == request {
        return true;
    }

    match item_type(response) {
        "message" => message_item_matches(response, request),
        "reasoning" => {
            normalized_item(response, &["status"]) == normalized_item(request, &["status"])
        }
        "function_call" => {
            normalized_item(response, &["id", "status"])
                == normalized_item(request, &["id", "status"])
        }
        _ => request_item_matches(response, request),
    }
}

fn normalized_item(value: &Value, output_only_fields: &[&str]) -> Value {
    let mut value = value.clone();
    if let Some(object) = value.as_object_mut() {
        // Codex explicitly ignores this private wire field. The ChatGPT
        // backend currently also echoes a public `metadata` copy which is not
        // represented by async-openai, so treat both as transport decoration.
        object.remove("internal_chat_message_metadata_passthrough");
        object.remove("metadata");
        for field in output_only_fields {
            object.remove(*field);
        }
    }
    value
}

fn message_item_matches(response: &Value, request: &Value) -> bool {
    if item_type(request) != "message"
        || response.get("role").and_then(Value::as_str)
            != request.get("role").and_then(Value::as_str)
    {
        return false;
    }

    message_text(response) == message_text(request)
}

fn message_text(item: &Value) -> Option<String> {
    let content = item.get("content")?;
    if let Some(text) = content.as_str() {
        return Some(text.to_owned());
    }

    let parts = content.as_array()?;
    let mut text = Vec::with_capacity(parts.len());
    for part in parts {
        let kind = part.get("type").and_then(Value::as_str)?;
        if !matches!(kind, "output_text" | "input_text") {
            return None;
        }
        text.push(part.get("text")?.as_str()?.to_owned());
    }
    Some(text.join("\n"))
}

fn item_type(item: &Value) -> &str {
    item.get("type")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
}

fn request_properties(request: &Value) -> Option<Value> {
    let mut properties = request.as_object()?.clone();
    for ignored in [
        "input",
        "client_metadata",
        "previous_response_id",
        "type",
        // Codex deliberately ignores stream delivery options when deciding
        // whether the server-side response context is reusable.
        "stream_options",
    ] {
        properties.remove(ignored);
    }
    Some(Value::Object(properties))
}

async fn read_response(
    mut session: OwnedMutexGuard<WebSocketSession>,
    event_tx: mpsc::Sender<Result<String, SamplingError>>,
    full_request: Value,
    idle_timeout: Duration,
) {
    let mut response_id = String::new();
    let mut output_items = Vec::new();

    loop {
        let next_message = {
            let socket = session
                .socket
                .as_mut()
                .expect("socket remains present while reading a response");
            tokio::select! {
                biased;
                _ = event_tx.closed() => {
                    *session = WebSocketSession::default();
                    return;
                }
                result = tokio::time::timeout(idle_timeout, socket.next()) => result,
            }
        };

        let message = match next_message {
            Ok(Some(Ok(message))) => message,
            Ok(Some(Err(error))) => {
                fail_stream(
                    &mut session,
                    &event_tx,
                    SamplingError::EventStreamError(format!(
                        "Responses WebSocket read failed: {error}"
                    )),
                )
                .await;
                return;
            }
            Ok(None) => {
                fail_stream(
                    &mut session,
                    &event_tx,
                    SamplingError::EventStreamError(
                        "Responses WebSocket closed before response.completed".to_owned(),
                    ),
                )
                .await;
                return;
            }
            Err(_) => {
                fail_stream(
                    &mut session,
                    &event_tx,
                    SamplingError::EventStreamError(
                        "idle timeout waiting for Responses WebSocket".to_owned(),
                    ),
                )
                .await;
                return;
            }
        };

        match message {
            Message::Text(text) => {
                let text = text.to_string();
                let Ok(mut value) = serde_json::from_str::<Value>(&text) else {
                    tracing::debug!(
                        target: crate::sampling_log::TARGET,
                        transport = "responses_websocket",
                        "ignoring non-JSON Responses WebSocket frame"
                    );
                    continue;
                };
                let kind = value
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();

                if kind == "error" {
                    let error = map_error_event(&value);
                    fail_stream(&mut session, &event_tx, error).await;
                    return;
                }
                // Codex transports metadata, rate limits, and timing on the
                // same socket. They are not Responses stream events and must
                // not be handed to async-openai's typed decoder.
                if kind.starts_with("codex.") || kind == "responsesapi.websocket_timing" {
                    continue;
                }

                match kind.as_str() {
                    "response.created" => {
                        if let Some(id) = value.pointer("/response/id").and_then(Value::as_str) {
                            response_id = id.to_owned();
                        }
                    }
                    "response.output_item.done" => {
                        if let Some(item) = value.get("item") {
                            output_items.push(item.clone());
                        }
                    }
                    "response.completed" | "response.incomplete" | "response.failed" => {
                        if let Some(id) = value.pointer("/response/id").and_then(Value::as_str) {
                            response_id = id.to_owned();
                        }
                        let terminal_output = value
                            .pointer("/response/output")
                            .and_then(Value::as_array)
                            .cloned()
                            .unwrap_or_default();
                        if output_items.is_empty() {
                            output_items = terminal_output;
                        } else if terminal_output.is_empty() {
                            inject_collected_output(&mut value, &output_items);
                        }
                    }
                    _ => {}
                }

                let forwarded_text = if matches!(
                    kind.as_str(),
                    "response.completed" | "response.incomplete" | "response.failed"
                ) {
                    value.to_string()
                } else {
                    text
                };
                if event_tx.send(Ok(forwarded_text)).await.is_err() {
                    *session = WebSocketSession::default();
                    return;
                }

                match kind.as_str() {
                    "response.completed" => {
                        session.last_exchange =
                            (!response_id.is_empty()).then_some(CompletedExchange {
                                full_request,
                                response_id,
                                output_items,
                            });
                        return;
                    }
                    "response.incomplete" => {
                        session.last_exchange = None;
                        return;
                    }
                    "response.failed" => {
                        *session = WebSocketSession::default();
                        return;
                    }
                    _ => {}
                }
            }
            Message::Ping(payload) => {
                let pong = session
                    .socket
                    .as_mut()
                    .expect("socket remains present while replying to ping")
                    .send(Message::Pong(payload));
                let pong_failed =
                    !matches!(tokio::time::timeout(idle_timeout, pong).await, Ok(Ok(())));
                if pong_failed {
                    fail_stream(
                        &mut session,
                        &event_tx,
                        SamplingError::EventStreamError(
                            "failed to reply to Responses WebSocket ping".to_owned(),
                        ),
                    )
                    .await;
                    return;
                }
            }
            Message::Pong(_) | Message::Frame(_) => {}
            Message::Binary(_) => {
                fail_stream(
                    &mut session,
                    &event_tx,
                    SamplingError::EventStreamError(
                        "unexpected binary Responses WebSocket frame".to_owned(),
                    ),
                )
                .await;
                return;
            }
            Message::Close(_) => {
                fail_stream(
                    &mut session,
                    &event_tx,
                    SamplingError::EventStreamError(
                        "Responses WebSocket closed before response.completed".to_owned(),
                    ),
                )
                .await;
                return;
            }
        }
    }
}

fn inject_collected_output(terminal_event: &mut Value, output_items: &[Value]) {
    if output_items.is_empty() {
        return;
    }
    let Some(response) = terminal_event
        .get_mut("response")
        .and_then(Value::as_object_mut)
    else {
        return;
    };
    response.insert("output".to_owned(), Value::Array(output_items.to_vec()));
}

async fn fail_stream(
    session: &mut WebSocketSession,
    event_tx: &mpsc::Sender<Result<String, SamplingError>>,
    error: SamplingError,
) {
    *session = WebSocketSession::default();
    let _ = event_tx.send(Err(error)).await;
}

fn map_error_event(value: &Value) -> SamplingError {
    let status = value
        .get("status")
        .or_else(|| value.get("status_code"))
        .and_then(Value::as_u64)
        .and_then(|status| u16::try_from(status).ok())
        .and_then(|status| StatusCode::from_u16(status).ok());
    let code = value
        .pointer("/error/code")
        .or_else(|| value.get("code"))
        .and_then(Value::as_str)
        .unwrap_or("websocket_error")
        .to_owned();
    let message = value
        .pointer("/error/message")
        .or_else(|| value.get("message"))
        .and_then(Value::as_str)
        .unwrap_or_else(|| match code.as_str() {
            "previous_response_not_found" => {
                "Previous response was not found. Retrying the full request."
            }
            "websocket_connection_limit_reached" => {
                "Responses WebSocket connection limit reached. Creating a new connection."
            }
            _ => "Responses WebSocket returned an error",
        })
        .to_owned();

    if matches!(
        code.as_str(),
        "previous_response_not_found" | "websocket_connection_limit_reached"
    ) {
        return SamplingError::StreamError {
            code: Some(ApiErrorCode::parse(&code)),
            error_type: code,
            message,
        };
    }
    if status == Some(StatusCode::UNAUTHORIZED) {
        return SamplingError::auth_unknown(format!("Unauthorized (401): {message}"));
    }
    if let Some(status) = status.filter(|status| !status.is_success()) {
        let headers = value.get("headers").and_then(json_headers);
        return SamplingError::Api {
            status,
            message,
            model_metadata: None,
            retry_after_secs: headers.as_ref().and_then(retry_after),
            should_retry: headers.as_ref().and_then(should_retry),
            error_code: Some(ApiErrorCode::parse(&code)),
        };
    }
    SamplingError::StreamError {
        code: Some(ApiErrorCode::parse(&code)),
        error_type: code,
        message,
    }
}

fn json_headers(value: &Value) -> Option<HeaderMap> {
    let object = value.as_object()?;
    let mut headers = HeaderMap::new();
    for (name, value) in object {
        let Ok(name) = HeaderName::from_bytes(name.as_bytes()) else {
            continue;
        };
        let rendered = match value {
            Value::String(value) => value.clone(),
            Value::Number(value) => value.to_string(),
            Value::Bool(value) => value.to_string(),
            _ => continue,
        };
        if let Ok(value) = HeaderValue::from_str(&rendered) {
            headers.insert(name, value);
        }
    }
    Some(headers)
}

fn retry_after(headers: &HeaderMap) -> Option<u64> {
    headers
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .map(|seconds| seconds.min(120))
}

fn should_retry(headers: &HeaderMap) -> Option<bool> {
    headers
        .get("x-should-retry")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| {
            if value.eq_ignore_ascii_case("true") {
                Some(true)
            } else if value.eq_ignore_ascii_case("false") {
                Some(false)
            } else {
                None
            }
        })
}

fn timestamp_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::StreamExt;
    use reqwest::header::AUTHORIZATION;
    use serde_json::json;
    use tokio::net::TcpListener;
    use tokio::sync::oneshot;
    use tokio_tungstenite::accept_hdr_async;
    use tokio_tungstenite::tungstenite::handshake::server::{Request, Response};

    fn config(base_url: &str) -> SamplerConfig {
        SamplerConfig {
            base_url: base_url.to_owned(),
            api_backend: ApiBackend::Responses,
            ..SamplerConfig::default()
        }
    }

    fn metadata() -> WebSocketRequestMetadata {
        WebSocketRequestMetadata {
            conversation_id: "thread-test".to_owned(),
            request_id: "turn-test".to_owned(),
            session_id: "session-test".to_owned(),
            turn_id: Some("7".to_owned()),
        }
    }

    #[test]
    fn websocket_url_is_gated_to_exact_chatgpt_codex_endpoint() {
        assert_eq!(
            websocket_url("https://chatgpt.com/backend-api/codex/")
                .expect("canonical endpoint should support WebSockets")
                .as_str(),
            "wss://chatgpt.com/backend-api/codex/responses"
        );
        assert!(websocket_url("https://chatgpt.com.evil.test/backend-api/codex").is_none());
        assert!(websocket_url("https://chatgpt.com/backend-api/codex/custom").is_none());
        assert!(websocket_url("https://openrouter.ai/api/v1").is_none());
    }

    #[test]
    fn equivalent_reconstructed_hooks_preserve_websocket_state() {
        #[derive(Debug)]
        struct NoopInjector;
        impl crate::config::HeaderInjector for NoopInjector {
            fn inject(&self, _headers: &mut HeaderMap) {}
        }

        let mut initial = config("https://chatgpt.com/backend-api/codex");
        initial.header_injector = Some(Arc::new(NoopInjector));
        let state = ResponsesWebSocketState::new(&initial);

        let mut reconstructed = initial.clone();
        reconstructed.header_injector = Some(Arc::new(NoopInjector));
        assert!(state.matches_config(&reconstructed));

        reconstructed.header_injector = None;
        assert!(!state.matches_config(&reconstructed));
    }

    #[test]
    fn compatible_request_uses_previous_response_and_only_incremental_input() {
        let assistant = json!({
            "type": "message",
            "id": "msg-1",
            "role": "assistant",
            "status": "completed",
            "content": [{"type": "output_text", "text": "hello", "annotations": []}]
        });
        let previous = CompletedExchange {
            full_request: json!({
                "model": "gpt-test",
                "instructions": "be concise",
                "input": [{"role": "user", "content": "first"}],
                "stream": true
            }),
            response_id: "resp-1".to_owned(),
            output_items: vec![assistant.clone()],
        };
        let current = json!({
            "model": "gpt-test",
            "instructions": "be concise",
            "input": [
                {"role": "user", "content": "first"},
                {"type": "message", "role": "assistant", "content": "hello"},
                {"role": "user", "content": "second"}
            ],
            "stream": true
        });

        let (wire, unchained_reason) =
            prepare_websocket_request(&current, Some(&previous), &metadata(), 123);
        assert!(unchained_reason.is_none());
        assert_eq!(wire["type"], "response.create");
        assert_eq!(wire["previous_response_id"], "resp-1");
        assert_eq!(
            wire["input"],
            json!([{"role": "user", "content": "second"}])
        );
        assert_eq!(wire["client_metadata"]["session_id"], "session-test");
        assert_eq!(
            wire["client_metadata"]["x-codex-ws-stream-request-start-ms"],
            "123"
        );
    }

    #[test]
    fn tool_continuation_matches_output_items_after_typed_round_trip() {
        let previous = CompletedExchange {
            full_request: json!({
                "model": "gpt-test",
                "input": [{"type": "message", "role": "user", "content": "run pwd"}],
                "stream": true
            }),
            response_id: "resp-tool".to_owned(),
            output_items: vec![
                json!({
                    "type": "reasoning",
                    "id": "rs_1",
                    "status": "completed",
                    "summary": [],
                    "internal_chat_message_metadata_passthrough": {"turn_id": "turn-1"}
                }),
                json!({
                    "type": "function_call",
                    "id": "fc_1",
                    "status": "completed",
                    "call_id": "call_1",
                    "name": "run_terminal_command",
                    "arguments": "{\"command\":\"pwd\"}",
                    "metadata": {"turn_id": "turn-1"}
                }),
            ],
        };
        let current = json!({
            "model": "gpt-test",
            "input": [
                {"type": "message", "role": "user", "content": "run pwd"},
                {"type": "reasoning", "id": "rs_1", "summary": []},
                {
                    "type": "function_call",
                    "call_id": "call_1",
                    "name": "run_terminal_command",
                    "arguments": "{\"command\":\"pwd\"}"
                },
                {"type": "function_call_output", "call_id": "call_1", "output": "repo"}
            ],
            "stream": true
        });

        let (wire, unchained_reason) =
            prepare_websocket_request(&current, Some(&previous), &metadata(), 123);
        assert!(unchained_reason.is_none());
        assert_eq!(wire["previous_response_id"], "resp-tool");
        assert_eq!(
            wire["input"],
            json!([{"type": "function_call_output", "call_id": "call_1", "output": "repo"}])
        );
    }

    #[test]
    fn changed_request_properties_send_full_request_without_previous_response() {
        let previous = CompletedExchange {
            full_request: json!({
                "model": "gpt-test",
                "instructions": "old",
                "input": [{"role": "user", "content": "first"}],
                "stream": true
            }),
            response_id: "resp-1".to_owned(),
            output_items: vec![],
        };
        let current = json!({
            "model": "gpt-test",
            "instructions": "new",
            "input": [{"role": "user", "content": "second"}],
            "stream": true
        });

        let (wire, unchained_reason) =
            prepare_websocket_request(&current, Some(&previous), &metadata(), 123);
        assert_eq!(unchained_reason, Some("request_properties_changed"));
        assert!(wire.get("previous_response_id").is_none());
        assert_eq!(wire["input"], current["input"]);
    }

    #[tokio::test]
    async fn reuses_connection_and_sends_codex_beta_and_incremental_request() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (headers_tx, headers_rx) = oneshot::channel::<HeaderMap>();
        let (requests_tx, mut requests_rx) = mpsc::unbounded_channel::<Value>();

        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut headers_tx = Some(headers_tx);
            let mut socket =
                accept_hdr_async(stream, move |request: &Request, response: Response| {
                    headers_tx
                        .take()
                        .expect("handshake callback runs once")
                        .send(request.headers().clone())
                        .unwrap();
                    Ok(response)
                })
                .await
                .unwrap();

            for index in 1..=2 {
                let Message::Text(request) = socket.next().await.unwrap().unwrap() else {
                    panic!("expected text request");
                };
                requests_tx
                    .send(serde_json::from_str(&request).unwrap())
                    .unwrap();
                let response_id = format!("resp-{index}");
                let output = json!({
                    "type": "message",
                    "id": format!("msg-{index}"),
                    "role": "assistant",
                    "status": "completed",
                    "content": [{
                        "type": "output_text",
                        "text": format!("answer-{index}"),
                        "annotations": []
                    }]
                });
                socket
                    .send(Message::Text(
                        json!({
                            "type": "response.output_item.done",
                            "output_index": 0,
                            "sequence_number": 1,
                            "item": output
                        })
                        .to_string()
                        .into(),
                    ))
                    .await
                    .unwrap();
                socket
                    .send(Message::Text(
                        json!({
                            "type": "response.completed",
                            "response": {"id": response_id, "output": []}
                        })
                        .to_string()
                        .into(),
                    ))
                    .await
                    .unwrap();
            }
        });

        let state = ResponsesWebSocketState::new(&config("https://chatgpt.com/backend-api/codex"));
        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, HeaderValue::from_static("Bearer test-token"));
        let first = json!({
            "model": "gpt-test",
            "instructions": "test",
            "input": [{"role": "user", "content": "first"}],
            "stream": true
        });
        let ResponsesWebSocketAttempt::Stream {
            mut raw_events,
            connection_reused,
            ..
        } = state
            .stream_at_url(
                &format!("ws://{address}/responses"),
                first.clone(),
                headers.clone(),
                metadata(),
                Duration::from_secs(5),
            )
            .await
            .unwrap()
        else {
            panic!("expected WebSocket stream");
        };
        assert!(!connection_reused);
        let mut completed = None;
        while let Some(event) = raw_events.next().await {
            let event: Value = serde_json::from_str(&event.unwrap()).unwrap();
            if event["type"] == "response.completed" {
                completed = Some(event);
            }
        }
        assert_eq!(completed.unwrap()["response"]["output"][0]["id"], "msg-1");

        let second = json!({
            "model": "gpt-test",
            "instructions": "test",
            "input": [
                {"role": "user", "content": "first"},
                {"type": "message", "role": "assistant", "content": "answer-1"},
                {"role": "user", "content": "second"}
            ],
            "stream": true
        });
        let ResponsesWebSocketAttempt::Stream {
            mut raw_events,
            connection_reused,
            ..
        } = state
            .stream_at_url(
                &format!("ws://{address}/responses"),
                second,
                headers,
                metadata(),
                Duration::from_secs(5),
            )
            .await
            .unwrap()
        else {
            panic!("expected WebSocket stream");
        };
        assert!(connection_reused);
        while let Some(event) = raw_events.next().await {
            event.unwrap();
        }

        let handshake_headers = headers_rx.await.unwrap();
        assert_eq!(
            handshake_headers.get("openai-beta").unwrap(),
            RESPONSES_WEBSOCKET_BETA
        );
        assert_eq!(
            handshake_headers.get(AUTHORIZATION).unwrap(),
            "Bearer test-token"
        );
        assert_eq!(handshake_headers.get("session-id").unwrap(), "session-test");

        let first_wire = requests_rx.recv().await.unwrap();
        let second_wire = requests_rx.recv().await.unwrap();
        assert_eq!(first_wire["type"], "response.create");
        assert!(first_wire.get("previous_response_id").is_none());
        assert_eq!(second_wire["previous_response_id"], "resp-1");
        assert_eq!(
            second_wire["input"],
            json!([{"role": "user", "content": "second"}])
        );
        server.await.unwrap();
    }

    #[tokio::test]
    async fn upgrade_required_disables_websocket_for_the_session() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                axum::Router::new().fallback(|| async { StatusCode::UPGRADE_REQUIRED }),
            )
            .await
            .unwrap();
        });
        let state = ResponsesWebSocketState::new(&config("https://chatgpt.com/backend-api/codex"));
        let attempt = state
            .stream_at_url(
                &format!("ws://{address}/responses"),
                json!({"model":"gpt-test","input":[],"stream":true}),
                HeaderMap::new(),
                metadata(),
                Duration::from_secs(5),
            )
            .await
            .unwrap();
        assert!(matches!(attempt, ResponsesWebSocketAttempt::FallbackToHttp));
        assert!(!state.enabled_for(
            "https://chatgpt.com/backend-api/codex",
            &ApiBackend::Responses
        ));
        server.abort();
    }
}
