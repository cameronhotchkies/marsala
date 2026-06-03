use std::time::Instant;

use axum::{
    extract::State,
    http::{Request, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use serde::Serialize;
use tracing::info;

use crate::event_log::EventLogHandle;

#[derive(Clone)]
struct AppState {
    event_log: EventLogHandle,
}

pub fn build_router(event_log: EventLogHandle) -> Router {
    let state = AppState { event_log };

    Router::new()
        .route("/healthz", get(healthz))
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

async fn request_logging_middleware(
    State(state): State<AppState>,
    request: Request<axum::body::Body>,
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

#[derive(Debug, Serialize)]
struct HealthResponse {
    status: &'static str,
    service: &'static str,
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use tower::ServiceExt;

    use super::*;

    #[tokio::test]
    async fn healthz_returns_ok_json() {
        let app = build_router(EventLogHandle::disabled());
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
}
