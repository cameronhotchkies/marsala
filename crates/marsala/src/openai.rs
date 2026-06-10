use std::time::Duration;
use std::{fmt, future::Future, pin::Pin};

use anyhow::{Context, Result};
use axum::{
    body::Bytes,
    http::{
        header::{ACCEPT, AUTHORIZATION},
        HeaderMap, HeaderName, HeaderValue, StatusCode,
    },
};
use futures_core::Stream;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

const DEFAULT_UPSTREAM_TIMEOUT: Duration = Duration::from_secs(30);
const CHAT_COMPLETIONS_PATH: &str = "/chat/completions";
const RESPONSES_PATH: &str = "/responses";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChatCompletionsRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream: Option<bool>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl ChatCompletionsRequest {
    pub fn stream_requested(&self) -> bool {
        self.stream.unwrap_or(false)
    }

    pub fn to_json_bytes(&self) -> Result<Vec<u8>> {
        serde_json::to_vec(self).context("failed to serialize chat completions request")
    }

    pub fn to_json_value(&self) -> Result<Value> {
        serde_json::to_value(self).context("failed to convert chat completions request to JSON")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ResponsesRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream: Option<bool>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl ResponsesRequest {
    pub fn stream_requested(&self) -> bool {
        self.stream.unwrap_or(false)
    }

    pub fn to_json_bytes(&self) -> Result<Vec<u8>> {
        serde_json::to_vec(self).context("failed to serialize responses request")
    }
}

#[derive(Clone, PartialEq, Eq)]
pub enum UpstreamAuthorization {
    BearerApiKey(String),
    InboundAuthorization(HeaderValue),
}

impl fmt::Debug for UpstreamAuthorization {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BearerApiKey(_) => formatter.write_str("BearerApiKey([redacted])"),
            Self::InboundAuthorization(_) => {
                formatter.write_str("InboundAuthorization([redacted])")
            }
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct UpstreamRequest {
    pub authorization: UpstreamAuthorization,
    pub headers: HeaderMap,
    pub body: Vec<u8>,
}

impl fmt::Debug for UpstreamRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UpstreamRequest")
            .field("authorization", &self.authorization)
            .field("headers", &self.headers)
            .field("body_bytes", &self.body.len())
            .finish()
    }
}

pub type UpstreamChatRequest = UpstreamRequest;
pub type UpstreamResponsesRequest = UpstreamRequest;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpstreamResponse {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: Vec<u8>,
}

pub type UpstreamBodyStream = Pin<Box<dyn Stream<Item = Result<Bytes, UpstreamError>> + Send>>;

pub struct UpstreamStreamingResponse {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: UpstreamBodyStream,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpstreamError {
    Timeout,
    InvalidResponse(String),
    Transport(String),
}

impl fmt::Display for UpstreamError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            UpstreamError::Timeout => formatter.write_str("upstream timeout"),
            UpstreamError::InvalidResponse(message) => {
                write!(formatter, "invalid upstream response: {message}")
            }
            UpstreamError::Transport(message) => formatter.write_str(message),
        }
    }
}

pub type ClientFuture<'a> =
    Pin<Box<dyn Future<Output = Result<UpstreamResponse, UpstreamError>> + Send + 'a>>;
pub type StreamingClientFuture<'a> =
    Pin<Box<dyn Future<Output = Result<UpstreamStreamingResponse, UpstreamError>> + Send + 'a>>;

pub trait OpenAiCompatibleClient: Send + Sync {
    fn send_chat_completions<'a>(&'a self, request: UpstreamChatRequest) -> ClientFuture<'a>;

    fn send_responses<'a>(&'a self, request: UpstreamResponsesRequest) -> ClientFuture<'a>;

    fn send_responses_stream<'a>(
        &'a self,
        request: UpstreamResponsesRequest,
    ) -> StreamingClientFuture<'a>;
}

#[derive(Clone)]
pub struct ReqwestOpenAiCompatibleClient {
    base_url: String,
    client: reqwest::Client,
}

impl ReqwestOpenAiCompatibleClient {
    pub fn new(base_url: &str) -> Result<Self> {
        Self::with_timeout(base_url, DEFAULT_UPSTREAM_TIMEOUT)
    }

    pub fn with_timeout(base_url: &str, timeout: Duration) -> Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .context("failed to construct upstream HTTP client")?;

        Ok(Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            client,
        })
    }

    pub fn chat_completions_url(&self) -> String {
        format!("{}{}", self.base_url, CHAT_COMPLETIONS_PATH)
    }

    pub fn responses_url(&self) -> String {
        format!("{}{}", self.base_url, RESPONSES_PATH)
    }

    async fn send_json_request(
        &self,
        url: String,
        request: UpstreamRequest,
    ) -> Result<UpstreamResponse, UpstreamError> {
        let default_accept = !request.headers.contains_key(ACCEPT);
        let mut builder = self
            .client
            .post(url)
            .header(AUTHORIZATION, authorization_header(&request.authorization)?);

        for (name, value) in &request.headers {
            if name != AUTHORIZATION {
                builder = builder.header(name, value);
            }
        }

        if default_accept {
            builder = builder.header(ACCEPT, "application/json");
        }

        let response = builder
            .body(request.body)
            .send()
            .await
            .map_err(map_send_error)?;
        let status = StatusCode::from_u16(response.status().as_u16()).map_err(|error| {
            UpstreamError::InvalidResponse(format!("invalid upstream status code: {error}"))
        })?;
        let headers = filter_upstream_response_headers(response.headers());
        let body = response.bytes().await.map_err(map_body_error)?.to_vec();

        Ok(UpstreamResponse {
            status,
            headers,
            body,
        })
    }

    async fn send_streaming_request(
        &self,
        url: String,
        request: UpstreamRequest,
    ) -> Result<UpstreamStreamingResponse, UpstreamError> {
        let default_accept = !request.headers.contains_key(ACCEPT);
        let mut builder = self
            .client
            .post(url)
            .header(AUTHORIZATION, authorization_header(&request.authorization)?);

        for (name, value) in &request.headers {
            if name != AUTHORIZATION {
                builder = builder.header(name, value);
            }
        }

        if default_accept {
            builder = builder.header(ACCEPT, "text/event-stream");
        }

        let response = builder
            .body(request.body)
            .send()
            .await
            .map_err(map_send_error)?;
        let status = StatusCode::from_u16(response.status().as_u16()).map_err(|error| {
            UpstreamError::InvalidResponse(format!("invalid upstream status code: {error}"))
        })?;
        let headers = filter_upstream_response_headers(response.headers());
        let body = response
            .bytes_stream()
            .map(|chunk| chunk.map_err(map_body_error));

        Ok(UpstreamStreamingResponse {
            status,
            headers,
            body: Box::pin(body),
        })
    }
}

impl OpenAiCompatibleClient for ReqwestOpenAiCompatibleClient {
    fn send_chat_completions<'a>(&'a self, request: UpstreamChatRequest) -> ClientFuture<'a> {
        Box::pin(async move {
            self.send_json_request(self.chat_completions_url(), request)
                .await
        })
    }

    fn send_responses<'a>(&'a self, request: UpstreamResponsesRequest) -> ClientFuture<'a> {
        Box::pin(async move { self.send_json_request(self.responses_url(), request).await })
    }

    fn send_responses_stream<'a>(
        &'a self,
        request: UpstreamResponsesRequest,
    ) -> StreamingClientFuture<'a> {
        Box::pin(async move {
            self.send_streaming_request(self.responses_url(), request)
                .await
        })
    }
}

pub(crate) fn filter_client_request_headers(headers: &HeaderMap) -> HeaderMap {
    copy_headers(headers, should_forward_request_header)
}

pub(crate) fn filter_upstream_response_headers(headers: &HeaderMap) -> HeaderMap {
    copy_headers(headers, should_forward_response_header)
}

fn copy_headers(headers: &HeaderMap, predicate: impl Fn(&HeaderName) -> bool) -> HeaderMap {
    let mut filtered = HeaderMap::new();

    for (name, value) in headers {
        if predicate(name) {
            filtered.append(name.clone(), value.clone());
        }
    }

    filtered
}

fn should_forward_request_header(name: &HeaderName) -> bool {
    let name = name.as_str();

    name.eq_ignore_ascii_case("accept")
        || name.eq_ignore_ascii_case("content-type")
        || name.eq_ignore_ascii_case("idempotency-key")
        || name.eq_ignore_ascii_case("openai-beta")
        || name.eq_ignore_ascii_case("openai-organization")
        || name.eq_ignore_ascii_case("openai-project")
        || name.eq_ignore_ascii_case("user-agent")
}

fn should_forward_response_header(name: &HeaderName) -> bool {
    let name = name.as_str();

    name.eq_ignore_ascii_case("content-type")
        || name.eq_ignore_ascii_case("retry-after")
        || name.eq_ignore_ascii_case("x-request-id")
        || name.to_ascii_lowercase().starts_with("openai-")
        || name.to_ascii_lowercase().starts_with("x-ratelimit-")
}

fn authorization_header(
    authorization: &UpstreamAuthorization,
) -> Result<HeaderValue, UpstreamError> {
    match authorization {
        UpstreamAuthorization::BearerApiKey(api_key) => {
            let mut value = HeaderValue::from_str(&format!("Bearer {api_key}")).map_err(|error| {
                UpstreamError::InvalidResponse(format!(
                    "configured upstream API key could not be used as an authorization header: {error}"
                ))
            })?;
            value.set_sensitive(true);
            Ok(value)
        }
        UpstreamAuthorization::InboundAuthorization(value) => {
            let mut value = value.clone();
            value.set_sensitive(true);
            Ok(value)
        }
    }
}

fn map_send_error(error: reqwest::Error) -> UpstreamError {
    if error.is_timeout() {
        UpstreamError::Timeout
    } else {
        UpstreamError::Transport(format!("failed to contact upstream: {error}"))
    }
}

fn map_body_error(error: reqwest::Error) -> UpstreamError {
    if error.is_timeout() {
        UpstreamError::Timeout
    } else {
        UpstreamError::InvalidResponse(format!("failed to read upstream response body: {error}"))
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use axum::{
        extract::Request,
        http::{
            header::{CONNECTION, CONTENT_TYPE, RETRY_AFTER},
            HeaderMap, HeaderName, HeaderValue, StatusCode,
        },
        response::IntoResponse,
        routing::post,
        Router,
    };
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
        task::JoinHandle,
    };

    use super::*;

    #[tokio::test]
    async fn reqwest_client_avoids_duplicate_v1_segment() {
        let client = ReqwestOpenAiCompatibleClient::new("https://api.openai.com/v1")
            .expect("reqwest client");

        assert_eq!(
            client.chat_completions_url(),
            "https://api.openai.com/v1/chat/completions"
        );
        assert_eq!(
            client.responses_url(),
            "https://api.openai.com/v1/responses"
        );
    }

    #[tokio::test]
    async fn reqwest_client_posts_to_configured_chat_completions_endpoint() {
        async fn handler(headers: HeaderMap, request: Request) -> impl IntoResponse {
            let auth = headers
                .get(AUTHORIZATION)
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
                .to_string();
            let content_type = headers
                .get(CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
                .to_string();
            let accept = headers
                .get(ACCEPT)
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
                .to_string();
            let openai_project = headers
                .get("openai-project")
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
                .to_string();
            let idempotency_key = headers
                .get("idempotency-key")
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
                .to_string();
            let path = request.uri().path().to_string();
            let body = axum::body::to_bytes(request.into_body(), usize::MAX)
                .await
                .expect("request body");

            (
                StatusCode::OK,
                [
                    ("content-type", HeaderValue::from_static("application/json")),
                    ("x-request-id", HeaderValue::from_static("req_123")),
                    (
                        "x-ratelimit-remaining-requests",
                        HeaderValue::from_static("42"),
                    ),
                    ("retry-after", HeaderValue::from_static("7")),
                    ("connection", HeaderValue::from_static("keep-alive")),
                ],
                serde_json::json!({
                    "path": path,
                    "authorization": auth,
                    "content_type": content_type,
                    "accept": accept,
                    "openai_project": openai_project,
                    "idempotency_key": idempotency_key,
                    "body": serde_json::from_slice::<Value>(&body).expect("json body"),
                })
                .to_string(),
            )
        }

        let (base_url, _server) =
            spawn_axum_server(Router::new().route("/v1/chat/completions", post(handler))).await;
        let client = ReqwestOpenAiCompatibleClient::new(&base_url).expect("reqwest client");

        let response = client
            .send_chat_completions(UpstreamChatRequest {
                authorization: test_api_key("sk-test-secret"),
                headers: request_headers(&[
                    ("content-type", "application/json"),
                    ("openai-project", "proj_123"),
                    ("idempotency-key", "idem_123"),
                ]),
                body: br#"{"model":"gpt-4.1-mini","messages":[{"role":"user","content":"hi"}]}"#
                    .to_vec(),
            })
            .await
            .expect("upstream response");

        assert_eq!(response.status, StatusCode::OK);
        assert_eq!(
            response
                .headers
                .get(CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some("application/json")
        );
        assert_eq!(
            response
                .headers
                .get("x-request-id")
                .and_then(|value| value.to_str().ok()),
            Some("req_123")
        );
        assert_eq!(
            response
                .headers
                .get("x-ratelimit-remaining-requests")
                .and_then(|value| value.to_str().ok()),
            Some("42")
        );
        assert_eq!(
            response
                .headers
                .get(RETRY_AFTER)
                .and_then(|value| value.to_str().ok()),
            Some("7")
        );
        assert!(response.headers.get(CONNECTION).is_none());

        let json: Value = serde_json::from_slice(&response.body).expect("response json");
        assert_eq!(json["path"], "/v1/chat/completions");
        assert_eq!(json["authorization"], "Bearer sk-test-secret");
        assert_eq!(json["content_type"], "application/json");
        assert_eq!(json["accept"], "application/json");
        assert_eq!(json["openai_project"], "proj_123");
        assert_eq!(json["idempotency_key"], "idem_123");
        assert_eq!(json["body"]["model"], "gpt-4.1-mini");
        assert!(json
            .get("body")
            .and_then(|value| value.get("stream"))
            .is_none());
    }

    #[tokio::test]
    async fn reqwest_client_posts_to_configured_responses_endpoint() {
        async fn handler(headers: HeaderMap, request: Request) -> impl IntoResponse {
            let auth = headers
                .get(AUTHORIZATION)
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
                .to_string();
            let path = request.uri().path().to_string();
            let body = axum::body::to_bytes(request.into_body(), usize::MAX)
                .await
                .expect("request body");

            (
                StatusCode::OK,
                [
                    ("content-type", HeaderValue::from_static("application/json")),
                    ("x-request-id", HeaderValue::from_static("resp_req_123")),
                ],
                serde_json::json!({
                    "path": path,
                    "authorization": auth,
                    "body": serde_json::from_slice::<Value>(&body).expect("json body"),
                })
                .to_string(),
            )
        }

        let (base_url, _server) =
            spawn_axum_server(Router::new().route("/v1/responses", post(handler))).await;
        let client = ReqwestOpenAiCompatibleClient::new(&base_url).expect("reqwest client");

        let response = client
            .send_responses(UpstreamResponsesRequest {
                authorization: test_api_key("sk-test-secret"),
                headers: request_headers(&[("content-type", "application/json")]),
                body: br#"{"model":"gpt-5","input":"hi","metadata":{"trace":"abc"}}"#.to_vec(),
            })
            .await
            .expect("upstream response");

        assert_eq!(response.status, StatusCode::OK);
        assert_eq!(
            response
                .headers
                .get("x-request-id")
                .and_then(|value| value.to_str().ok()),
            Some("resp_req_123")
        );

        let json: Value = serde_json::from_slice(&response.body).expect("response json");
        assert_eq!(json["path"], "/v1/responses");
        assert_eq!(json["authorization"], "Bearer sk-test-secret");
        assert_eq!(json["body"]["model"], "gpt-5");
        assert_eq!(json["body"]["metadata"]["trace"], "abc");
    }

    #[tokio::test]
    async fn reqwest_client_can_use_inbound_authorization_for_responses() {
        async fn handler(headers: HeaderMap) -> impl IntoResponse {
            let auth = headers
                .get(AUTHORIZATION)
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
                .to_string();

            (
                StatusCode::OK,
                [("content-type", HeaderValue::from_static("application/json"))],
                serde_json::json!({ "authorization": auth }).to_string(),
            )
        }

        let (base_url, _server) =
            spawn_axum_server(Router::new().route("/v1/responses", post(handler))).await;
        let client = ReqwestOpenAiCompatibleClient::new(&base_url).expect("reqwest client");

        let response = client
            .send_responses(UpstreamResponsesRequest {
                authorization: UpstreamAuthorization::InboundAuthorization(
                    HeaderValue::from_static("Bearer codex-auth-secret"),
                ),
                headers: request_headers(&[
                    ("content-type", "application/json"),
                    ("authorization", "Bearer should-not-win"),
                ]),
                body: br#"{"model":"gpt-5","input":"hi"}"#.to_vec(),
            })
            .await
            .expect("upstream response");

        let json: Value = serde_json::from_slice(&response.body).expect("response json");
        assert_eq!(json["authorization"], "Bearer codex-auth-secret");
    }

    #[tokio::test]
    async fn reqwest_client_streams_configured_responses_endpoint() {
        async fn handler(headers: HeaderMap, request: Request) -> impl IntoResponse {
            let auth = headers
                .get(AUTHORIZATION)
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
                .to_string();
            let accept = headers
                .get(ACCEPT)
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
                .to_string();
            let path = request.uri().path().to_string();
            let body = axum::body::to_bytes(request.into_body(), usize::MAX)
                .await
                .expect("request body");

            (
                StatusCode::OK,
                [
                    ("content-type", HeaderValue::from_static("text/event-stream")),
                    ("x-request-id", HeaderValue::from_static("resp_stream_req_123")),
                ],
                format!(
                    "event: response.output_text.delta\ndata: {{\"path\":\"{path}\",\"authorization\":\"{auth}\",\"accept\":\"{accept}\",\"body\":{}}}\n\n",
                    std::str::from_utf8(&body).expect("utf8 body")
                ),
            )
        }

        let (base_url, _server) =
            spawn_axum_server(Router::new().route("/v1/responses", post(handler))).await;
        let client = ReqwestOpenAiCompatibleClient::new(&base_url).expect("reqwest client");

        let response = client
            .send_responses_stream(UpstreamResponsesRequest {
                authorization: test_api_key("sk-test-secret"),
                headers: request_headers(&[("content-type", "application/json")]),
                body: br#"{"model":"gpt-5","input":"hi","stream":true}"#.to_vec(),
            })
            .await
            .expect("upstream response");

        assert_eq!(response.status, StatusCode::OK);
        assert_eq!(
            response
                .headers
                .get(CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some("text/event-stream")
        );
        assert_eq!(
            response
                .headers
                .get("x-request-id")
                .and_then(|value| value.to_str().ok()),
            Some("resp_stream_req_123")
        );

        let mut body_stream = response.body;
        let mut body = Vec::new();
        while let Some(chunk) = body_stream.next().await {
            body.extend_from_slice(&chunk.expect("stream chunk"));
        }
        let body = String::from_utf8(body).expect("utf8 stream body");
        assert!(body.contains(r#""path":"/v1/responses""#));
        assert!(body.contains(r#""authorization":"Bearer sk-test-secret""#));
        assert!(body.contains(r#""accept":"text/event-stream""#));
        assert!(body.contains(r#""stream":true"#));
    }

    #[tokio::test]
    async fn reqwest_client_maps_upstream_timeout() {
        async fn handler() -> impl IntoResponse {
            tokio::time::sleep(Duration::from_millis(200)).await;
            (
                StatusCode::OK,
                [(CONTENT_TYPE, HeaderValue::from_static("application/json"))],
                "{\"ok\":true}",
            )
        }

        let (base_url, _server) =
            spawn_axum_server(Router::new().route("/v1/chat/completions", post(handler))).await;
        let client =
            ReqwestOpenAiCompatibleClient::with_timeout(&base_url, Duration::from_millis(50))
                .expect("reqwest client");

        let error = client
            .send_chat_completions(UpstreamChatRequest {
                authorization: test_api_key("sk-test-secret"),
                headers: request_headers(&[("content-type", "application/json")]),
                body: br#"{"model":"gpt-4.1-mini","messages":[]}"#.to_vec(),
            })
            .await
            .expect_err("timeout");

        assert_eq!(error, UpstreamError::Timeout);
    }

    #[tokio::test]
    async fn reqwest_client_maps_truncated_response_to_invalid_response() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind listener");
        let address = listener.local_addr().expect("local addr");
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept");
            let mut buffer = [0_u8; 1024];
            let _ = stream.read(&mut buffer).await.expect("read request");
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 32\r\n\r\n{\"id\":\"chatcmpl-truncated\"}",
                )
                .await
                .expect("write response");
        });

        let client = ReqwestOpenAiCompatibleClient::new(&format!("http://{address}/v1"))
            .expect("reqwest client");

        let error = client
            .send_chat_completions(UpstreamChatRequest {
                authorization: test_api_key("sk-test-secret"),
                headers: request_headers(&[("content-type", "application/json")]),
                body: br#"{"model":"gpt-4.1-mini","messages":[]}"#.to_vec(),
            })
            .await
            .expect_err("invalid response");

        assert!(matches!(error, UpstreamError::InvalidResponse(_)));
        server.await.expect("server task");
    }

    #[test]
    fn filter_client_request_headers_keeps_safe_allowlist_only() {
        let headers = request_headers(&[
            ("content-type", "application/json"),
            ("accept", "application/json"),
            ("openai-project", "proj_123"),
            ("authorization", "Bearer inbound"),
            ("connection", "keep-alive"),
        ]);

        let filtered = filter_client_request_headers(&headers);

        assert_eq!(
            filtered
                .get(CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some("application/json")
        );
        assert_eq!(
            filtered
                .get("openai-project")
                .and_then(|value| value.to_str().ok()),
            Some("proj_123")
        );
        assert!(filtered.get(AUTHORIZATION).is_none());
        assert!(filtered.get(CONNECTION).is_none());
    }

    fn request_headers(values: &[(&str, &str)]) -> HeaderMap {
        let mut headers = HeaderMap::new();

        for (name, value) in values {
            headers.insert(
                HeaderName::from_bytes(name.as_bytes()).expect("header name"),
                HeaderValue::from_str(value).expect("header value"),
            );
        }

        headers
    }

    fn test_api_key(api_key: &str) -> UpstreamAuthorization {
        UpstreamAuthorization::BearerApiKey(api_key.to_string())
    }

    async fn spawn_axum_server(app: Router) -> (String, JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind listener");
        let address = listener.local_addr().expect("local addr");
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve test app");
        });

        (format!("http://{address}/v1"), server)
    }
}
