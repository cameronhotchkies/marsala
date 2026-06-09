use std::{
    env,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::Instant,
};

use anyhow::Result;
use axum::{
    body::{Body, Bytes},
    extract::State,
    http::{
        header::{AUTHORIZATION, CONTENT_TYPE, COOKIE},
        HeaderMap, HeaderName, Request, StatusCode,
    },
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::Serialize;
use serde_json::{Map, Value};
use tracing::info;

use crate::{
    config::AppConfig,
    event_log::EventLogHandle,
    openai::{
        filter_client_request_headers, filter_upstream_response_headers, ChatCompletionsClient,
        ChatCompletionsRequest, ReqwestChatCompletionsClient, UpstreamChatRequest, UpstreamError,
    },
};

static REQUEST_ID_COUNTER: AtomicU64 = AtomicU64::new(1);

#[derive(Clone)]
struct AppState {
    config: Arc<AppConfig>,
    event_log: EventLogHandle,
    openai_client: Arc<dyn ChatCompletionsClient>,
}

pub fn build_router(config: AppConfig, event_log: EventLogHandle) -> Result<Router> {
    let openai_client = Arc::new(ReqwestChatCompletionsClient::new(&config.openai.base_url)?);
    Ok(build_router_with_client(config, event_log, openai_client))
}

fn build_router_with_client(
    config: AppConfig,
    event_log: EventLogHandle,
    openai_client: Arc<dyn ChatCompletionsClient>,
) -> Router {
    let state = AppState {
        config: Arc::new(config),
        event_log,
        openai_client,
    };

    Router::new()
        .route("/healthz", get(healthz))
        .route("/v1/chat/completions", post(chat_completions))
        .route("/v1/responses", post(responses_observation))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            request_logging_middleware,
        ))
        .with_state(state)
}

async fn healthz() -> impl IntoResponse {
    (
        StatusCode::OK,
        Json(HealthResponse {
            status: "ok",
            service: "marsala",
        }),
    )
}

async fn responses_observation(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let request_id = next_request_id();
    let started = Instant::now();
    let content_type = header_value(&headers, CONTENT_TYPE);

    emit_responses_request_log(&state, &request_id, &headers, &content_type, body.len());

    let body = serde_json::json!({
        "error": {
            "message": "Marsala observed /v1/responses traffic, but forwarding for this endpoint is not implemented in this validation probe",
            "type": "not_implemented",
            "source": "marsala",
        }
    });
    let body_bytes = serde_json::to_vec(&body).expect("serialize local error body");
    emit_responses_response_log(
        &state,
        &request_id,
        StatusCode::NOT_IMPLEMENTED,
        started.elapsed().as_millis() as u64,
        Some("application/json".to_string()),
        body_bytes.len(),
    );

    build_response(
        StatusCode::NOT_IMPLEMENTED,
        &json_content_type_header(),
        body_bytes,
    )
}

async fn chat_completions(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let request_id = next_request_id();
    let started = Instant::now();
    let content_type = header_value(&headers, CONTENT_TYPE);
    let upstream_headers = filter_client_request_headers(&headers);

    let parsed_request = match serde_json::from_slice::<ChatCompletionsRequest>(&body) {
        Ok(request) => request,
        Err(error) => {
            emit_chat_request_log(
                &state,
                &request_id,
                &content_type,
                None,
                body.len(),
                "marsala_local",
            );
            return local_error_response(
                &state,
                &request_id,
                StatusCode::BAD_REQUEST,
                "invalid_request_error",
                &format!("request body must be valid JSON: {error}"),
                started.elapsed().as_millis() as u64,
                content_type,
                None,
                "marsala_local",
            );
        }
    };

    let request_body = match parsed_request.to_json_bytes() {
        Ok(body) => body,
        Err(error) => {
            emit_chat_request_log(
                &state,
                &request_id,
                &content_type,
                Some(&parsed_request),
                body.len(),
                "marsala_local",
            );
            return local_error_response(
                &state,
                &request_id,
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal_error",
                &format!("failed to serialize request body: {error:#}"),
                started.elapsed().as_millis() as u64,
                content_type,
                None,
                "marsala_local",
            );
        }
    };

    if parsed_request.stream_requested() {
        emit_chat_request_log(
            &state,
            &request_id,
            &content_type,
            Some(&parsed_request),
            request_body.len(),
            "marsala_local",
        );
        return local_error_response(
            &state,
            &request_id,
            StatusCode::BAD_REQUEST,
            "unsupported_streaming",
            "Marsala Phase 1 does not support stream=true; retry with stream omitted or false",
            started.elapsed().as_millis() as u64,
            content_type,
            Some("application/json".to_string()),
            "marsala_local",
        );
    }

    let api_key = match load_api_key(&state.config.openai.api_key_env) {
        Ok(api_key) => api_key,
        Err(message) => {
            emit_chat_request_log(
                &state,
                &request_id,
                &content_type,
                Some(&parsed_request),
                request_body.len(),
                "marsala_local",
            );
            return local_error_response(
                &state,
                &request_id,
                StatusCode::INTERNAL_SERVER_ERROR,
                "configuration_error",
                &message,
                started.elapsed().as_millis() as u64,
                content_type,
                Some("application/json".to_string()),
                "marsala_local",
            );
        }
    };

    emit_chat_request_log(
        &state,
        &request_id,
        &content_type,
        Some(&parsed_request),
        request_body.len(),
        "upstream",
    );

    let upstream_response = state
        .openai_client
        .send_chat_completions(UpstreamChatRequest {
            api_key,
            headers: upstream_headers,
            body: request_body,
        })
        .await;

    match upstream_response {
        Ok(upstream) => {
            let elapsed_ms = started.elapsed().as_millis() as u64;
            let response_headers = filter_upstream_response_headers(&upstream.headers);
            emit_chat_response_log(
                &state,
                &request_id,
                upstream.status,
                elapsed_ms,
                header_value(&response_headers, CONTENT_TYPE),
                &upstream.body,
                "upstream",
            );
            build_response(upstream.status, &response_headers, upstream.body)
        }
        Err(UpstreamError::Timeout) => local_error_response(
            &state,
            &request_id,
            StatusCode::GATEWAY_TIMEOUT,
            "upstream_timeout",
            "Marsala timed out while waiting for the configured OpenAI-compatible upstream",
            started.elapsed().as_millis() as u64,
            content_type,
            Some("application/json".to_string()),
            "marsala_local",
        ),
        Err(UpstreamError::InvalidResponse(message)) => local_error_response(
            &state,
            &request_id,
            StatusCode::BAD_GATEWAY,
            "invalid_upstream_response",
            &format!("upstream response was malformed or incomplete: {message}"),
            started.elapsed().as_millis() as u64,
            content_type,
            Some("application/json".to_string()),
            "marsala_local",
        ),
        Err(UpstreamError::Transport(message)) => local_error_response(
            &state,
            &request_id,
            StatusCode::BAD_GATEWAY,
            "upstream_connection_error",
            &format!("failed to reach configured OpenAI-compatible upstream: {message}"),
            started.elapsed().as_millis() as u64,
            content_type,
            Some("application/json".to_string()),
            "marsala_local",
        ),
    }
}

async fn request_logging_middleware(
    State(state): State<AppState>,
    request: Request<Body>,
    next: Next,
) -> Response {
    let method = request.method().clone();
    let path = request.uri().path().to_string();
    let started = Instant::now();

    let response = next.run(request).await;
    let status = response.status();
    let elapsed_ms = started.elapsed().as_millis() as u64;

    info!(%method, %path, %status, elapsed_ms, "request completed");
    state.event_log.emit(
        "http_request",
        serde_json::json!({
            "method": method.to_string(),
            "path": path,
            "status": status.as_u16(),
            "elapsed_ms": elapsed_ms,
        }),
    );

    response
}

fn emit_responses_request_log(
    state: &AppState,
    request_id: &str,
    headers: &HeaderMap,
    content_type: &Option<String>,
    body_bytes: usize,
) {
    let mut data = Map::new();
    data.insert("request_id".into(), request_id.into());
    data.insert("method".into(), "POST".into());
    data.insert("path".into(), "/v1/responses".into());
    data.insert("target".into(), "marsala_local".into());
    data.insert("auth_shape".into(), Value::Object(auth_shape(headers)));
    if let Some(content_type) = content_type {
        data.insert("content_type".into(), content_type.clone().into());
    }
    data.insert("body_bytes".into(), (body_bytes as u64).into());
    if state.config.logging.log_bodies {
        data.insert("body_logging".into(), "omitted_for_probe".into());
    } else {
        data.insert("body_logging".into(), "disabled".into());
    }

    state
        .event_log
        .emit("responses_request", Value::Object(data));
}

fn emit_responses_response_log(
    state: &AppState,
    request_id: &str,
    status: StatusCode,
    elapsed_ms: u64,
    content_type: Option<String>,
    body_bytes: usize,
) {
    let mut data = Map::new();
    data.insert("request_id".into(), request_id.into());
    data.insert("path".into(), "/v1/responses".into());
    data.insert("status".into(), status.as_u16().into());
    data.insert("elapsed_ms".into(), elapsed_ms.into());
    data.insert("source".into(), "marsala_local".into());
    data.insert("body_bytes".into(), (body_bytes as u64).into());
    data.insert("body_logging".into(), "disabled".into());
    if let Some(content_type) = content_type {
        data.insert("content_type".into(), content_type.into());
    }

    state
        .event_log
        .emit("responses_response", Value::Object(data));
}

fn emit_chat_request_log(
    state: &AppState,
    request_id: &str,
    content_type: &Option<String>,
    request: Option<&ChatCompletionsRequest>,
    body_bytes: usize,
    target: &str,
) {
    let mut data = Map::new();
    data.insert("request_id".into(), request_id.into());
    data.insert("method".into(), "POST".into());
    data.insert("path".into(), "/v1/chat/completions".into());
    data.insert("target".into(), target.into());
    if target == "upstream" {
        data.insert(
            "upstream_url".into(),
            Value::String(chat_completions_url(&state.config.openai.base_url)),
        );
        data.insert(
            "api_key_env".into(),
            Value::String(state.config.openai.api_key_env.clone()),
        );
        data.insert(
            "upstream_headers".into(),
            serde_json::json!({
                "authorization": "[redacted]",
                "content-type": content_type,
            }),
        );
    }
    if let Some(content_type) = content_type {
        data.insert("content_type".into(), content_type.clone().into());
    }
    if let Some(request) = request {
        data.insert("stream".into(), request.stream_requested().into());
        add_request_body_log_fields(
            &mut data,
            request,
            body_bytes,
            state.config.logging.log_bodies,
        );
    } else {
        data.insert("body_bytes".into(), (body_bytes as u64).into());
        data.insert("body_logging".into(), "omitted_due_to_parse_error".into());
    }

    state
        .event_log
        .emit("chat_completions_request", Value::Object(data));
}

fn emit_chat_response_log(
    state: &AppState,
    request_id: &str,
    status: StatusCode,
    elapsed_ms: u64,
    content_type: Option<String>,
    body: &[u8],
    source: &str,
) {
    let mut data = Map::new();
    data.insert("request_id".into(), request_id.into());
    data.insert("path".into(), "/v1/chat/completions".into());
    data.insert("status".into(), status.as_u16().into());
    data.insert("elapsed_ms".into(), elapsed_ms.into());
    data.insert("source".into(), source.into());
    if let Some(content_type) = content_type {
        data.insert("content_type".into(), content_type.clone().into());
        add_response_body_log_fields(
            &mut data,
            body,
            Some(&content_type),
            state.config.logging.log_bodies,
        );
    } else {
        add_response_body_log_fields(&mut data, body, None, state.config.logging.log_bodies);
    }

    state
        .event_log
        .emit("chat_completions_response", Value::Object(data));
}

fn add_request_body_log_fields(
    data: &mut Map<String, Value>,
    request: &ChatCompletionsRequest,
    body_bytes: usize,
    log_bodies: bool,
) {
    data.insert("body_bytes".into(), (body_bytes as u64).into());
    if !log_bodies {
        data.insert("body_logging".into(), "disabled".into());
        return;
    }

    let mut body = request
        .to_json_value()
        .unwrap_or_else(|_| serde_json::json!({"error": "failed_to_render_request_body"}));
    redact_json_value(&mut body);
    data.insert("body".into(), body);
}

fn add_response_body_log_fields(
    data: &mut Map<String, Value>,
    body: &[u8],
    content_type: Option<&str>,
    log_bodies: bool,
) {
    data.insert("body_bytes".into(), (body.len() as u64).into());
    if !log_bodies {
        data.insert("body_logging".into(), "disabled".into());
        return;
    }

    if content_type.is_some_and(is_json_content_type) {
        if let Ok(mut json) = serde_json::from_slice::<Value>(body) {
            redact_json_value(&mut json);
            data.insert("body".into(), json);
            return;
        }
    }

    if let Ok(text) = std::str::from_utf8(body) {
        data.insert("body".into(), Value::String(text.to_string()));
    } else {
        data.insert("body_logging".into(), "omitted_non_utf8_body".into());
    }
}

fn load_api_key(api_key_env: &str) -> std::result::Result<String, String> {
    match env::var(api_key_env) {
        Ok(value) if !value.trim().is_empty() => Ok(value),
        _ => Err(format!(
            "Marsala is missing a configured upstream API key: environment variable {api_key_env} is unset or empty"
        )),
    }
}

fn local_error_response(
    state: &AppState,
    request_id: &str,
    status: StatusCode,
    error_type: &str,
    message: &str,
    elapsed_ms: u64,
    request_content_type: Option<String>,
    response_content_type: Option<String>,
    source: &str,
) -> Response {
    let body = serde_json::json!({
        "error": {
            "message": message,
            "type": error_type,
            "source": "marsala",
        }
    });
    let body_bytes = serde_json::to_vec(&body).expect("serialize local error body");

    emit_chat_response_log(
        state,
        request_id,
        status,
        elapsed_ms,
        response_content_type.or(request_content_type),
        &body_bytes,
        source,
    );

    build_response(status, &json_content_type_header(), body_bytes)
}

fn build_response(status: StatusCode, headers: &HeaderMap, body: Vec<u8>) -> Response {
    let mut response = Response::builder().status(status);

    for (name, value) in headers {
        response = response.header(name, value);
    }

    response
        .body(Body::from(body))
        .expect("response construction must succeed")
}

fn json_content_type_header() -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        CONTENT_TYPE,
        axum::http::HeaderValue::from_static("application/json"),
    );
    headers
}

fn redact_json_value(value: &mut Value) {
    match value {
        Value::Object(map) => {
            for (key, value) in map.iter_mut() {
                if should_redact_key(key) {
                    *value = Value::String("[redacted]".to_string());
                } else {
                    redact_json_value(value);
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                redact_json_value(item);
            }
        }
        _ => {}
    }
}

fn should_redact_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    let normalized = key.replace('_', "-");
    matches!(
        normalized.as_str(),
        "authorization"
            | "proxy-authorization"
            | "cookie"
            | "set-cookie"
            | "api_key"
            | "api-key"
            | "apikey"
            | "x-api-key"
    ) || normalized.contains("token")
        || normalized.contains("session")
        || normalized.contains("bearer")
}

fn auth_shape(headers: &HeaderMap) -> Map<String, Value> {
    let mut auth = Map::new();
    add_header_presence(&mut auth, "authorization", headers, &AUTHORIZATION, true);
    add_header_presence(
        &mut auth,
        "proxy_authorization",
        headers,
        &HeaderName::from_static("proxy-authorization"),
        true,
    );
    add_header_presence(&mut auth, "cookie", headers, &COOKIE, false);
    add_header_presence(
        &mut auth,
        "x_api_key",
        headers,
        &HeaderName::from_static("x-api-key"),
        false,
    );
    auth
}

fn add_header_presence(
    data: &mut Map<String, Value>,
    field: &str,
    headers: &HeaderMap,
    name: &HeaderName,
    include_scheme: bool,
) {
    let Some(value) = headers.get(name) else {
        data.insert(format!("{field}_present"), false.into());
        return;
    };

    data.insert(format!("{field}_present"), true.into());
    if include_scheme {
        data.insert(
            format!("{field}_scheme"),
            Value::String(auth_scheme(value.to_str().ok())),
        );
    }
}

fn auth_scheme(value: Option<&str>) -> String {
    let Some(value) = value else {
        return "non_utf8".to_string();
    };

    value
        .split_ascii_whitespace()
        .next()
        .filter(|scheme| !scheme.is_empty())
        .map(|scheme| scheme.to_ascii_lowercase())
        .unwrap_or_else(|| "opaque".to_string())
}

fn is_json_content_type(content_type: &str) -> bool {
    let content_type = content_type.to_ascii_lowercase();
    content_type.starts_with("application/json") || content_type.ends_with("+json")
}

fn header_value(headers: &HeaderMap, name: axum::http::header::HeaderName) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(ToOwned::to_owned)
}

fn next_request_id() -> String {
    format!(
        "marsala-{}",
        REQUEST_ID_COUNTER.fetch_add(1, Ordering::Relaxed)
    )
}

fn chat_completions_url(base_url: &str) -> String {
    format!("{}/chat/completions", base_url.trim_end_matches('/'))
}

#[derive(Debug, Serialize)]
struct HealthResponse {
    status: &'static str,
    service: &'static str,
}

#[cfg(test)]
mod tests {
    use std::{collections::VecDeque, env, ffi::OsString, fs, path::Path, sync::Mutex};

    use tower::ServiceExt;

    use super::*;
    use crate::{
        config::AppConfig,
        event_log::{read_tail_lines, EventLogWriter},
        openai::UpstreamResponse,
    };

    struct EnvGuard {
        key: String,
        saved: Option<OsString>,
    }

    impl EnvGuard {
        fn preserve(key: &str) -> Self {
            Self {
                key: key.to_string(),
                saved: env::var_os(key),
            }
        }

        fn set(&self, value: &str) {
            env::set_var(&self.key, value);
        }

        fn remove(&self) {
            env::remove_var(&self.key);
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            if let Some(value) = &self.saved {
                env::set_var(&self.key, value);
            } else {
                env::remove_var(&self.key);
            }
        }
    }

    #[derive(Default)]
    struct StubClient {
        requests: Mutex<Vec<UpstreamChatRequest>>,
        responses: Mutex<VecDeque<std::result::Result<UpstreamResponse, UpstreamError>>>,
    }

    impl StubClient {
        fn with_responses(
            responses: Vec<std::result::Result<UpstreamResponse, UpstreamError>>,
        ) -> Self {
            Self {
                requests: Mutex::new(Vec::new()),
                responses: Mutex::new(responses.into()),
            }
        }

        fn request_count(&self) -> usize {
            self.requests.lock().expect("requests lock").len()
        }

        fn first_request(&self) -> UpstreamChatRequest {
            self.requests
                .lock()
                .expect("requests lock")
                .first()
                .expect("first request")
                .clone()
        }
    }

    impl ChatCompletionsClient for StubClient {
        fn send_chat_completions<'a>(
            &'a self,
            request: UpstreamChatRequest,
        ) -> crate::openai::ClientFuture<'a> {
            Box::pin(async move {
                self.requests.lock().expect("requests lock").push(request);
                self.responses
                    .lock()
                    .expect("responses lock")
                    .pop_front()
                    .expect("stub response")
            })
        }
    }

    #[tokio::test]
    async fn healthz_returns_ok_json() {
        let app = build_router_with_client(
            AppConfig::default(),
            EventLogHandle::disabled(),
            Arc::new(StubClient::default()),
        );
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/healthz")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::OK);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        let json: serde_json::Value = serde_json::from_slice(&body).expect("json");
        assert_eq!(json["status"], "ok");
        assert_eq!(json["service"], "marsala");
    }

    #[tokio::test]
    async fn responses_observation_logs_sanitized_auth_shape_without_body_capture() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let log_path = tempdir.path().join("events.jsonl");
        let mut config = AppConfig::default();
        config.logging.enabled = true;
        config.logging.path = log_path.clone();
        config.logging.log_bodies = false;
        let mut writer = EventLogWriter::spawn(&log_path, true)
            .await
            .expect("event writer");
        let app =
            build_router_with_client(config, writer.handle(), Arc::new(StubClient::default()));

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/responses")
                    .header(CONTENT_TYPE, "application/json")
                    .header("authorization", "Bearer codex-secret")
                    .header("cookie", "sessionid=session-secret")
                    .header("x-api-key", "sk-body-adjacent-secret")
                    .body(Body::from(
                        br#"{"model":"gpt-5","input":"hello","token":"body-secret"}"#.to_vec(),
                    ))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::NOT_IMPLEMENTED);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        let json: Value = serde_json::from_slice(&body).expect("json");
        assert_eq!(json["error"]["type"], "not_implemented");

        writer.shutdown().await.expect("writer shutdown");
        let log_text = fs::read_to_string(&log_path).expect("read log");
        assert!(!log_text.contains("codex-secret"));
        assert!(!log_text.contains("session-secret"));
        assert!(!log_text.contains("sk-body-adjacent-secret"));
        assert!(!log_text.contains("body-secret"));

        let records = read_event_records(&log_path);
        let request = records
            .iter()
            .find(|record| record["event_type"] == "responses_request")
            .expect("responses request event");
        let response = records
            .iter()
            .find(|record| record["event_type"] == "responses_response")
            .expect("responses response event");

        assert_eq!(request["data"]["path"], "/v1/responses");
        assert_eq!(request["data"]["target"], "marsala_local");
        assert_eq!(request["data"]["body_logging"], "disabled");
        assert!(request["data"].get("body").is_none());
        assert_eq!(request["data"]["auth_shape"]["authorization_present"], true);
        assert_eq!(
            request["data"]["auth_shape"]["authorization_scheme"],
            "bearer"
        );
        assert_eq!(request["data"]["auth_shape"]["cookie_present"], true);
        assert_eq!(request["data"]["auth_shape"]["x_api_key_present"], true);
        assert_eq!(response["data"]["status"], 501);
        assert_eq!(
            request["data"]["request_id"],
            response["data"]["request_id"]
        );
    }

    #[tokio::test]
    async fn chat_completions_preserves_upstream_success() {
        let env_guard = EnvGuard::preserve("PHASE1_OPENAI_API_KEY_SUCCESS");
        env_guard.set("sk-upstream-secret");
        let stub = Arc::new(StubClient::with_responses(vec![Ok(UpstreamResponse {
            status: StatusCode::OK,
            headers: test_headers(&[
                ("content-type", "application/json"),
                ("x-request-id", "req_123"),
            ]),
            body: br#"{"id":"chatcmpl-123","object":"chat.completion"}"#.to_vec(),
        })]));
        let app = build_test_app(stub.clone(), true, "PHASE1_OPENAI_API_KEY_SUCCESS");

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header(CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        br#"{"model":"gpt-4.1-mini","messages":[{"role":"user","content":"hi"}]}"#
                            .to_vec(),
                    ))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get(CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some("application/json")
        );
        assert_eq!(
            response
                .headers()
                .get("x-request-id")
                .and_then(|value| value.to_str().ok()),
            Some("req_123")
        );
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        assert_eq!(
            body.as_ref(),
            br#"{"id":"chatcmpl-123","object":"chat.completion"}"#
        );

        let upstream_request = stub.first_request();
        assert_eq!(upstream_request.api_key, "sk-upstream-secret");
        assert_eq!(
            upstream_request
                .headers
                .get(CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some("application/json")
        );
        let forwarded_json: Value =
            serde_json::from_slice(&upstream_request.body).expect("forwarded json");
        assert_eq!(forwarded_json["model"], "gpt-4.1-mini");
        assert!(forwarded_json.get("stream").is_none());
    }

    #[tokio::test]
    async fn chat_completions_preserves_upstream_4xx() {
        let env_guard = EnvGuard::preserve("PHASE1_OPENAI_API_KEY_4XX");
        env_guard.set("sk-upstream-secret");
        let app = build_test_app(
            Arc::new(StubClient::with_responses(vec![Ok(UpstreamResponse {
                status: StatusCode::UNAUTHORIZED,
                headers: test_headers(&[("content-type", "application/json")]),
                body: br#"{"error":{"message":"bad key"}}"#.to_vec(),
            })])),
            true,
            "PHASE1_OPENAI_API_KEY_4XX",
        );

        let response = app
            .oneshot(post_chat_request(
                br#"{"model":"gpt-4.1-mini","messages":[]}"#.to_vec(),
            ))
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        assert_eq!(body.as_ref(), br#"{"error":{"message":"bad key"}}"#);
    }

    #[tokio::test]
    async fn chat_completions_preserves_upstream_5xx() {
        let env_guard = EnvGuard::preserve("PHASE1_OPENAI_API_KEY_5XX");
        env_guard.set("sk-upstream-secret");
        let app = build_test_app(
            Arc::new(StubClient::with_responses(vec![Ok(UpstreamResponse {
                status: StatusCode::BAD_GATEWAY,
                headers: test_headers(&[("content-type", "application/json")]),
                body: br#"{"error":{"message":"upstream overloaded"}}"#.to_vec(),
            })])),
            true,
            "PHASE1_OPENAI_API_KEY_5XX",
        );

        let response = app
            .oneshot(post_chat_request(
                br#"{"model":"gpt-4.1-mini","messages":[]}"#.to_vec(),
            ))
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        assert_eq!(
            body.as_ref(),
            br#"{"error":{"message":"upstream overloaded"}}"#
        );
    }

    #[tokio::test]
    async fn chat_completions_rejects_stream_true_without_upstream_call() {
        let env_guard = EnvGuard::preserve("PHASE1_OPENAI_API_KEY_STREAM_REJECT");
        env_guard.set("sk-upstream-secret");
        let stub = Arc::new(StubClient::default());
        let app = build_test_app(stub.clone(), true, "PHASE1_OPENAI_API_KEY_STREAM_REJECT");

        let response = app
            .oneshot(post_chat_request(
                br#"{"model":"gpt-4.1-mini","messages":[],"stream":true}"#.to_vec(),
            ))
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(stub.request_count(), 0);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        let json: Value = serde_json::from_slice(&body).expect("json");
        assert_eq!(json["error"]["type"], "unsupported_streaming");
    }

    #[tokio::test]
    async fn chat_completions_returns_configuration_error_for_missing_api_key() {
        let env_guard = EnvGuard::preserve("PHASE1_OPENAI_API_KEY_MISSING");
        env_guard.remove();
        let stub = Arc::new(StubClient::default());
        let app = build_test_app(stub.clone(), true, "PHASE1_OPENAI_API_KEY_MISSING");

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header(CONTENT_TYPE, "application/json")
                    .header("authorization", "Bearer inbound-should-not-be-used")
                    .body(Body::from(
                        br#"{"model":"gpt-4.1-mini","messages":[]}"#.to_vec(),
                    ))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(stub.request_count(), 0);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        let json: Value = serde_json::from_slice(&body).expect("json");
        assert_eq!(json["error"]["type"], "configuration_error");
        assert!(json["error"]["message"]
            .as_str()
            .expect("message")
            .contains("PHASE1_OPENAI_API_KEY_MISSING"));
    }

    #[tokio::test]
    async fn chat_completions_forwards_selected_request_headers_only() {
        let env_guard = EnvGuard::preserve("PHASE1_OPENAI_API_KEY_HEADERS");
        env_guard.set("sk-upstream-secret");
        let stub = Arc::new(StubClient::with_responses(vec![Ok(UpstreamResponse {
            status: StatusCode::OK,
            headers: test_headers(&[("content-type", "application/json")]),
            body: br#"{"id":"chatcmpl-123"}"#.to_vec(),
        })]));
        let app = build_test_app(stub.clone(), false, "PHASE1_OPENAI_API_KEY_HEADERS");

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header(CONTENT_TYPE, "application/json")
                    .header("accept", "application/json")
                    .header("authorization", "Bearer inbound-should-not-pass")
                    .header("openai-organization", "org_123")
                    .header("openai-project", "proj_123")
                    .header("openai-beta", "assistants=v2")
                    .header("idempotency-key", "idem_123")
                    .header("user-agent", "marsala-test/1.0")
                    .header("connection", "keep-alive")
                    .header("te", "trailers")
                    .body(Body::from(
                        br#"{"model":"gpt-4.1-mini","messages":[]}"#.to_vec(),
                    ))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::OK);

        let upstream_request = stub.first_request();
        assert_eq!(
            upstream_request
                .headers
                .get("openai-organization")
                .and_then(|value| value.to_str().ok()),
            Some("org_123")
        );
        assert_eq!(
            upstream_request
                .headers
                .get("openai-project")
                .and_then(|value| value.to_str().ok()),
            Some("proj_123")
        );
        assert_eq!(
            upstream_request
                .headers
                .get("openai-beta")
                .and_then(|value| value.to_str().ok()),
            Some("assistants=v2")
        );
        assert_eq!(
            upstream_request
                .headers
                .get("idempotency-key")
                .and_then(|value| value.to_str().ok()),
            Some("idem_123")
        );
        assert_eq!(
            upstream_request
                .headers
                .get("user-agent")
                .and_then(|value| value.to_str().ok()),
            Some("marsala-test/1.0")
        );
        assert!(upstream_request.headers.get("authorization").is_none());
        assert!(upstream_request.headers.get("connection").is_none());
        assert!(upstream_request.headers.get("te").is_none());
    }

    #[tokio::test]
    async fn chat_completions_preserves_safe_upstream_response_headers_only() {
        let env_guard = EnvGuard::preserve("PHASE1_OPENAI_API_KEY_RESPONSE_HEADERS");
        env_guard.set("sk-upstream-secret");
        let app = build_test_app(
            Arc::new(StubClient::with_responses(vec![Ok(UpstreamResponse {
                status: StatusCode::TOO_MANY_REQUESTS,
                headers: test_headers(&[
                    ("content-type", "application/json"),
                    ("x-request-id", "req_429"),
                    ("x-ratelimit-remaining-requests", "0"),
                    ("retry-after", "11"),
                    ("proxy-authenticate", "Basic realm=test"),
                    ("transfer-encoding", "chunked"),
                ]),
                body: br#"{"error":{"message":"slow down"}}"#.to_vec(),
            })])),
            false,
            "PHASE1_OPENAI_API_KEY_RESPONSE_HEADERS",
        );

        let response = app
            .oneshot(post_chat_request(
                br#"{"model":"gpt-4.1-mini","messages":[]}"#.to_vec(),
            ))
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(
            response
                .headers()
                .get(CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some("application/json")
        );
        assert_eq!(
            response
                .headers()
                .get("x-request-id")
                .and_then(|value| value.to_str().ok()),
            Some("req_429")
        );
        assert_eq!(
            response
                .headers()
                .get("x-ratelimit-remaining-requests")
                .and_then(|value| value.to_str().ok()),
            Some("0")
        );
        assert_eq!(
            response
                .headers()
                .get("retry-after")
                .and_then(|value| value.to_str().ok()),
            Some("11")
        );
        assert!(response.headers().get("proxy-authenticate").is_none());
        assert!(response.headers().get("transfer-encoding").is_none());
    }

    #[tokio::test]
    async fn chat_completions_returns_gateway_timeout_for_upstream_timeout() {
        let env_guard = EnvGuard::preserve("PHASE1_OPENAI_API_KEY_TIMEOUT");
        env_guard.set("sk-upstream-secret");
        let app = build_test_app(
            Arc::new(StubClient::with_responses(vec![Err(
                UpstreamError::Timeout,
            )])),
            true,
            "PHASE1_OPENAI_API_KEY_TIMEOUT",
        );

        let response = app
            .oneshot(post_chat_request(
                br#"{"model":"gpt-4.1-mini","messages":[]}"#.to_vec(),
            ))
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::GATEWAY_TIMEOUT);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        let json: Value = serde_json::from_slice(&body).expect("json");
        assert_eq!(json["error"]["type"], "upstream_timeout");
        assert!(json["error"]["message"]
            .as_str()
            .expect("message")
            .contains("timed out"));
        assert_eq!(json["error"]["source"], "marsala");
    }

    #[tokio::test]
    async fn chat_completions_returns_bad_gateway_for_invalid_upstream_response() {
        let env_guard = EnvGuard::preserve("PHASE1_OPENAI_API_KEY_INVALID_UPSTREAM");
        env_guard.set("sk-upstream-secret");
        let app = build_test_app(
            Arc::new(StubClient::with_responses(vec![Err(
                UpstreamError::InvalidResponse("socket closed mid-body".to_string()),
            )])),
            true,
            "PHASE1_OPENAI_API_KEY_INVALID_UPSTREAM",
        );

        let response = app
            .oneshot(post_chat_request(
                br#"{"model":"gpt-4.1-mini","messages":[]}"#.to_vec(),
            ))
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        let json: Value = serde_json::from_slice(&body).expect("json");
        assert_eq!(json["error"]["type"], "invalid_upstream_response");
    }

    #[tokio::test]
    async fn chat_completions_log_redaction_never_writes_raw_api_keys() {
        let env_guard = EnvGuard::preserve("PHASE1_OPENAI_API_KEY_LOG_REDACTION");
        env_guard.set("sk-upstream-secret");
        let tempdir = tempfile::tempdir().expect("tempdir");
        let log_path = tempdir.path().join("events.jsonl");
        let mut config = AppConfig::default();
        config.logging.enabled = true;
        config.logging.path = log_path.clone();
        config.logging.log_bodies = true;
        config.openai.api_key_env = "PHASE1_OPENAI_API_KEY_LOG_REDACTION".to_string();
        let mut writer = EventLogWriter::spawn(&log_path, true)
            .await
            .expect("event writer");
        let stub = Arc::new(StubClient::with_responses(vec![Ok(UpstreamResponse {
            status: StatusCode::OK,
            headers: test_headers(&[("content-type", "application/json")]),
            body: br#"{"id":"chatcmpl-123"}"#.to_vec(),
        })]));
        let app = build_router_with_client(config, writer.handle(), stub);

        let response = app
            .oneshot(post_chat_request(
                br#"{"model":"gpt-4.1-mini","messages":[],"api_key":"body-secret"}"#.to_vec(),
            ))
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::OK);

        writer.shutdown().await.expect("writer shutdown");
        let log_text = fs::read_to_string(&log_path).expect("read log");

        assert!(!log_text.contains("sk-upstream-secret"));
        assert!(!log_text.contains("body-secret"));
        assert!(log_text.contains("[redacted]"));
    }

    #[tokio::test]
    async fn chat_completions_log_body_logging_disabled_omits_bodies() {
        let env_guard = EnvGuard::preserve("PHASE1_OPENAI_API_KEY_LOG_BODIES_OFF");
        env_guard.set("sk-upstream-secret");
        let tempdir = tempfile::tempdir().expect("tempdir");
        let log_path = tempdir.path().join("events.jsonl");
        let mut config = AppConfig::default();
        config.logging.enabled = true;
        config.logging.path = log_path.clone();
        config.logging.log_bodies = false;
        config.openai.api_key_env = "PHASE1_OPENAI_API_KEY_LOG_BODIES_OFF".to_string();
        let mut writer = EventLogWriter::spawn(&log_path, true)
            .await
            .expect("event writer");
        let app = build_router_with_client(
            config,
            writer.handle(),
            Arc::new(StubClient::with_responses(vec![Ok(UpstreamResponse {
                status: StatusCode::OK,
                headers: test_headers(&[("content-type", "application/json")]),
                body: br#"{"id":"chatcmpl-123","secret":"never-log"}"#.to_vec(),
            })])),
        );

        let response = app
            .oneshot(post_chat_request(
                br#"{"model":"gpt-4.1-mini","messages":[],"secret":"never-log"}"#.to_vec(),
            ))
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::OK);

        writer.shutdown().await.expect("writer shutdown");
        let records = read_event_records(&log_path);
        let request = records
            .iter()
            .find(|record| record["event_type"] == "chat_completions_request")
            .expect("request event");
        let response = records
            .iter()
            .find(|record| record["event_type"] == "chat_completions_response")
            .expect("response event");

        assert_eq!(request["data"]["body_logging"], "disabled");
        assert_eq!(response["data"]["body_logging"], "disabled");
        assert!(request["data"].get("body").is_none());
        assert!(response["data"].get("body").is_none());
    }

    #[tokio::test]
    async fn chat_completions_logs_stream_rejection_as_local_only() {
        let env_guard = EnvGuard::preserve("PHASE1_OPENAI_API_KEY_STREAM_LOGGING");
        env_guard.set("sk-upstream-secret");
        let tempdir = tempfile::tempdir().expect("tempdir");
        let log_path = tempdir.path().join("events.jsonl");
        let mut config = AppConfig::default();
        config.logging.enabled = true;
        config.logging.path = log_path.clone();
        config.logging.log_bodies = false;
        config.openai.api_key_env = "PHASE1_OPENAI_API_KEY_STREAM_LOGGING".to_string();
        let mut writer = EventLogWriter::spawn(&log_path, true)
            .await
            .expect("event writer");
        let stub = Arc::new(StubClient::default());
        let app = build_router_with_client(config, writer.handle(), stub.clone());

        let response = app
            .oneshot(post_chat_request(
                br#"{"model":"gpt-4.1-mini","messages":[],"stream":true}"#.to_vec(),
            ))
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(stub.request_count(), 0);

        writer.shutdown().await.expect("writer shutdown");
        let records = read_event_records(&log_path);
        let request = records
            .iter()
            .find(|record| record["event_type"] == "chat_completions_request")
            .expect("request event");
        let response = records
            .iter()
            .find(|record| record["event_type"] == "chat_completions_response")
            .expect("response event");

        assert_eq!(request["data"]["target"], "marsala_local");
        assert_eq!(request["data"]["stream"], true);
        assert!(request["data"].get("upstream_headers").is_none());
        assert!(request["data"].get("upstream_url").is_none());
        assert!(request["data"].get("api_key_env").is_none());
        assert_eq!(response["data"]["source"], "marsala_local");
        assert_eq!(
            request["data"]["request_id"],
            response["data"]["request_id"]
        );
    }

    #[tokio::test]
    async fn chat_completions_logs_missing_api_key_as_local_only() {
        let env_guard = EnvGuard::preserve("PHASE1_OPENAI_API_KEY_MISSING_LOGGING");
        env_guard.remove();
        let tempdir = tempfile::tempdir().expect("tempdir");
        let log_path = tempdir.path().join("events.jsonl");
        let mut config = AppConfig::default();
        config.logging.enabled = true;
        config.logging.path = log_path.clone();
        config.logging.log_bodies = false;
        config.openai.api_key_env = "PHASE1_OPENAI_API_KEY_MISSING_LOGGING".to_string();
        let mut writer = EventLogWriter::spawn(&log_path, true)
            .await
            .expect("event writer");
        let stub = Arc::new(StubClient::default());
        let app = build_router_with_client(config, writer.handle(), stub.clone());

        let response = app
            .oneshot(post_chat_request(
                br#"{"model":"gpt-4.1-mini","messages":[]}"#.to_vec(),
            ))
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(stub.request_count(), 0);

        writer.shutdown().await.expect("writer shutdown");
        let records = read_event_records(&log_path);
        let request = records
            .iter()
            .find(|record| record["event_type"] == "chat_completions_request")
            .expect("request event");
        let response = records
            .iter()
            .find(|record| record["event_type"] == "chat_completions_response")
            .expect("response event");

        assert_eq!(request["data"]["target"], "marsala_local");
        assert!(request["data"].get("upstream_headers").is_none());
        assert!(request["data"].get("upstream_url").is_none());
        assert!(request["data"].get("api_key_env").is_none());
        assert_eq!(response["data"]["source"], "marsala_local");
        assert_eq!(
            request["data"]["request_id"],
            response["data"]["request_id"]
        );
    }

    fn build_test_app(stub: Arc<StubClient>, log_bodies: bool, api_key_env: &str) -> Router {
        let mut config = AppConfig::default();
        config.logging.log_bodies = log_bodies;
        config.openai.api_key_env = api_key_env.to_string();
        build_router_with_client(config, EventLogHandle::disabled(), stub)
    }

    fn post_chat_request(body: Vec<u8>) -> Request<Body> {
        Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header(CONTENT_TYPE, "application/json")
            .body(Body::from(body))
            .expect("request")
    }

    fn read_event_records(path: &Path) -> Vec<Value> {
        read_tail_lines(path, 100)
            .expect("read tail lines")
            .into_iter()
            .map(|line| serde_json::from_str(&line).expect("event json"))
            .collect()
    }

    fn test_headers(values: &[(&str, &str)]) -> HeaderMap {
        let mut headers = HeaderMap::new();

        for (name, value) in values {
            headers.insert(
                axum::http::HeaderName::from_bytes(name.as_bytes()).expect("header name"),
                axum::http::HeaderValue::from_str(value).expect("header value"),
            );
        }

        headers
    }
}
