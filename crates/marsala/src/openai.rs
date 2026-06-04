use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use anyhow::{Context, Result};
use axum::http::{
    header::{ACCEPT, AUTHORIZATION},
    HeaderMap, HeaderName, StatusCode,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

const DEFAULT_UPSTREAM_TIMEOUT: Duration = Duration::from_secs(30);
const CHAT_COMPLETIONS_PATH: &str = "/chat/completions";

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpstreamChatRequest {
    pub api_key: String,
    pub headers: HeaderMap,
    pub body: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpstreamResponse {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpstreamError {
    Timeout,
    InvalidResponse(String),
    Transport(String),
}

pub type ClientFuture<'a> =
    Pin<Box<dyn Future<Output = Result<UpstreamResponse, UpstreamError>> + Send + 'a>>;

pub trait ChatCompletionsClient: Send + Sync {
    fn send_chat_completions<'a>(&'a self, request: UpstreamChatRequest) -> ClientFuture<'a>;
}

#[derive(Clone)]
pub struct ReqwestChatCompletionsClient {
    base_url: String,
    client: reqwest::Client,
}

impl ReqwestChatCompletionsClient {
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
}

impl ChatCompletionsClient for ReqwestChatCompletionsClient {
    fn send_chat_completions<'a>(&'a self, request: UpstreamChatRequest) -> ClientFuture<'a> {
        Box::pin(async move {
            let default_accept = !request.headers.contains_key(ACCEPT);
            let mut builder = self
                .client
                .post(self.chat_completions_url())
                .header(AUTHORIZATION, format!("Bearer {}", request.api_key));

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
        let client =
            ReqwestChatCompletionsClient::new("https://api.openai.com/v1").expect("reqwest client");

        assert_eq!(
            client.chat_completions_url(),
            "https://api.openai.com/v1/chat/completions"
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
        let client = ReqwestChatCompletionsClient::new(&base_url).expect("reqwest client");

        let response = client
            .send_chat_completions(UpstreamChatRequest {
                api_key: "sk-test-secret".to_string(),
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
            ReqwestChatCompletionsClient::with_timeout(&base_url, Duration::from_millis(50))
                .expect("reqwest client");

        let error = client
            .send_chat_completions(UpstreamChatRequest {
                api_key: "sk-test-secret".to_string(),
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

        let client = ReqwestChatCompletionsClient::new(&format!("http://{address}/v1"))
            .expect("reqwest client");

        let error = client
            .send_chat_completions(UpstreamChatRequest {
                api_key: "sk-test-secret".to_string(),
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
