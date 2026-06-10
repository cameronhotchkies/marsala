use std::{net::SocketAddr, time::Instant};

use anyhow::{Context, Result};
use serde_json::{Map, Value};
use tokio::{
    io::{self, AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::{TcpListener, TcpStream},
    sync::watch,
    time::{timeout, Duration},
};
use tracing::{debug, info};

use crate::{
    config::{AppConfig, MitmConnectAction},
    event_log::EventLogHandle,
};

const MAX_HEADER_BYTES: usize = 64 * 1024;
const HEADER_READ_TIMEOUT: Duration = Duration::from_secs(10);

pub async fn serve(
    config: AppConfig,
    event_log: EventLogHandle,
    shutdown: watch::Receiver<bool>,
) -> Result<()> {
    let bind_addr = format!("{}:{}", config.proxy.host, config.proxy.port);
    let listener = TcpListener::bind(&bind_addr)
        .await
        .with_context(|| format!("failed to bind proxy listener {bind_addr}"))?;
    let local_addr = listener
        .local_addr()
        .context("missing proxy listener local address")?;

    event_log.emit(
        "proxy_listener_started",
        serde_json::json!({
            "bind_addr": bind_addr,
            "local_addr": local_addr.to_string(),
        }),
    );
    info!(address = %local_addr, "marsala proxy probe listening");

    serve_listener(listener, config, event_log, shutdown).await
}

pub(crate) async fn serve_listener(
    listener: TcpListener,
    config: AppConfig,
    event_log: EventLogHandle,
    mut shutdown: watch::Receiver<bool>,
) -> Result<()> {
    loop {
        if *shutdown.borrow() {
            return Ok(());
        }

        tokio::select! {
            result = listener.accept() => {
                let (stream, peer_addr) = result.context("failed to accept proxy connection")?;
                let connection_log = event_log.clone();
                let connection_config = config.clone();
                tokio::spawn(async move {
                    if let Err(error) = handle_connection(stream, peer_addr, connection_config, connection_log).await {
                        debug!(%error, %peer_addr, "proxy probe connection ended with error");
                    }
                });
            }
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    return Ok(());
                }
            }
        }
    }
}

async fn handle_connection(
    stream: TcpStream,
    peer_addr: SocketAddr,
    config: AppConfig,
    event_log: EventLogHandle,
) -> Result<()> {
    let started = Instant::now();
    let mut reader = BufReader::new(stream);
    let request = match timeout(HEADER_READ_TIMEOUT, read_proxy_request(&mut reader)).await {
        Ok(Ok(request)) => request,
        Ok(Err(error)) => {
            emit_proxy_event(
                &event_log,
                ProxyEvent {
                    started,
                    peer_addr,
                    request: None,
                    status: "error",
                    connect_action: None,
                    error: Some(error.to_string()),
                },
            );
            return Err(error);
        }
        Err(_) => {
            let message = "timed out while reading proxy request headers".to_string();
            emit_proxy_event(
                &event_log,
                ProxyEvent {
                    started,
                    peer_addr,
                    request: None,
                    status: "error",
                    connect_action: None,
                    error: Some(message.clone()),
                },
            );
            return Err(anyhow::anyhow!(message));
        }
    };

    if request.method.eq_ignore_ascii_case("CONNECT") {
        handle_connect(reader, peer_addr, config, event_log, started, request).await
    } else {
        handle_plain_http(reader, peer_addr, event_log, started, request).await
    }
}

async fn handle_connect(
    mut reader: BufReader<TcpStream>,
    peer_addr: SocketAddr,
    config: AppConfig,
    event_log: EventLogHandle,
    started: Instant,
    request: ProxyRequest,
) -> Result<()> {
    let Some((host, port)) = parse_target_host_port(&request.target, 443) else {
        let message = "CONNECT target must be host:port".to_string();
        let _ = reader
            .get_mut()
            .write_all(b"HTTP/1.1 400 Bad Request\r\ncontent-length: 0\r\n\r\n")
            .await;
        emit_proxy_event(
            &event_log,
            ProxyEvent {
                started,
                peer_addr,
                request: Some(request),
                status: "error",
                connect_action: None,
                error: Some(message.clone()),
            },
        );
        return Err(anyhow::anyhow!(message));
    };

    let connect_action = match config.mitm_connect_action_for_host(&host) {
        MitmConnectAction::Mitm => ConnectAction::MitmUnimplemented,
        MitmConnectAction::Tunnel => ConnectAction::Tunnel,
    };

    if connect_action == ConnectAction::MitmUnimplemented {
        write_mitm_unimplemented_response(reader.get_mut()).await?;
        emit_proxy_event(
            &event_log,
            ProxyEvent {
                started,
                peer_addr,
                request: Some(request),
                status: "mitm_unimplemented",
                connect_action: Some(connect_action),
                error: None,
            },
        );
        return Ok(());
    }

    let connect_addr = format!("{host}:{port}");
    let mut upstream = match TcpStream::connect(&connect_addr).await {
        Ok(upstream) => upstream,
        Err(error) => {
            let message = format!("failed to connect tunnel target: {error}");
            let _ = reader
                .get_mut()
                .write_all(b"HTTP/1.1 502 Bad Gateway\r\ncontent-length: 0\r\n\r\n")
                .await;
            emit_proxy_event(
                &event_log,
                ProxyEvent {
                    started,
                    peer_addr,
                    request: Some(request),
                    status: "error",
                    connect_action: Some(connect_action),
                    error: Some(message.clone()),
                },
            );
            return Err(anyhow::anyhow!(message));
        }
    };

    reader
        .get_mut()
        .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
        .await
        .context("failed to acknowledge CONNECT tunnel")?;

    let copy_result = io::copy_bidirectional(reader.get_mut(), &mut upstream).await;
    let (status, error) = match copy_result {
        Ok(_) => ("closed", None),
        Err(error) => ("error", Some(error.to_string())),
    };

    emit_proxy_event(
        &event_log,
        ProxyEvent {
            started,
            peer_addr,
            request: Some(request),
            status,
            connect_action: Some(connect_action),
            error,
        },
    );

    Ok(())
}

async fn handle_plain_http(
    mut reader: BufReader<TcpStream>,
    peer_addr: SocketAddr,
    event_log: EventLogHandle,
    started: Instant,
    request: ProxyRequest,
) -> Result<()> {
    let response_body = br#"{"error":{"message":"Marsala observed plain HTTP proxy traffic, but forwarding for non-CONNECT requests is not implemented in this validation probe","type":"not_implemented","source":"marsala"}}"#;
    let response = format!(
        "HTTP/1.1 501 Not Implemented\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        response_body.len()
    );
    reader
        .get_mut()
        .write_all(response.as_bytes())
        .await
        .context("failed to write plain HTTP proxy probe response")?;
    reader
        .get_mut()
        .write_all(response_body)
        .await
        .context("failed to write plain HTTP proxy probe body")?;

    emit_proxy_event(
        &event_log,
        ProxyEvent {
            started,
            peer_addr,
            request: Some(request),
            status: "not_implemented",
            connect_action: None,
            error: None,
        },
    );

    Ok(())
}

async fn read_proxy_request(reader: &mut BufReader<TcpStream>) -> Result<ProxyRequest> {
    let mut total_bytes = 0usize;
    let mut request_line = String::new();
    let read = reader
        .read_line(&mut request_line)
        .await
        .context("failed to read proxy request line")?;
    total_bytes += read;
    if read == 0 {
        return Err(anyhow::anyhow!(
            "connection closed before proxy request line"
        ));
    }
    if total_bytes > MAX_HEADER_BYTES {
        return Err(anyhow::anyhow!("proxy request headers exceeded size limit"));
    }

    let mut parts = request_line.trim_end().split_ascii_whitespace();
    let method = parts
        .next()
        .ok_or_else(|| anyhow::anyhow!("missing proxy request method"))?
        .to_string();
    let target = parts
        .next()
        .ok_or_else(|| anyhow::anyhow!("missing proxy request target"))?
        .to_string();
    let version = parts.next().unwrap_or("").to_string();

    let mut headers = Vec::new();
    loop {
        let mut line = String::new();
        let read = reader
            .read_line(&mut line)
            .await
            .context("failed to read proxy request header")?;
        total_bytes += read;
        if read == 0 || line == "\r\n" || line == "\n" {
            break;
        }
        if total_bytes > MAX_HEADER_BYTES {
            return Err(anyhow::anyhow!("proxy request headers exceeded size limit"));
        }
        if let Some((name, value)) = line.trim_end().split_once(':') {
            headers.push((name.trim().to_ascii_lowercase(), value.trim().to_string()));
        }
    }

    Ok(ProxyRequest {
        method,
        target,
        version,
        headers,
        header_bytes: total_bytes,
    })
}

struct ProxyEvent<'a> {
    started: Instant,
    peer_addr: SocketAddr,
    request: Option<ProxyRequest>,
    status: &'a str,
    connect_action: Option<ConnectAction>,
    error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConnectAction {
    Tunnel,
    MitmUnimplemented,
}

impl ConnectAction {
    fn as_str(self) -> &'static str {
        match self {
            Self::Tunnel => "tunnel",
            Self::MitmUnimplemented => "mitm_unimplemented",
        }
    }
}

fn emit_proxy_event(event_log: &EventLogHandle, event: ProxyEvent<'_>) {
    let mut data = Map::new();
    data.insert("peer_addr".into(), event.peer_addr.to_string().into());
    data.insert(
        "elapsed_ms".into(),
        (event.started.elapsed().as_millis() as u64).into(),
    );
    data.insert("status".into(), event.status.into());
    data.insert("body_logging".into(), "disabled".into());
    if let Some(connect_action) = event.connect_action {
        data.insert("connect_action".into(), connect_action.as_str().into());
    }
    if let Some(error) = event.error {
        data.insert("error".into(), error.into());
    }

    if let Some(request) = event.request {
        data.insert("method".into(), request.method.clone().into());
        data.insert("http_version".into(), request.version.clone().into());
        data.insert("target".into(), sanitize_target(&request.target).into());
        data.insert("header_bytes".into(), (request.header_bytes as u64).into());
        data.insert(
            "auth_shape".into(),
            Value::Object(proxy_auth_shape(&request.headers)),
        );
        add_visible_target_fields(&mut data, &request);
    }

    event_log.emit("proxy_request", Value::Object(data));
}

async fn write_mitm_unimplemented_response(stream: &mut TcpStream) -> Result<()> {
    let response_body = br#"{"error":{"message":"Marsala selected allowlisted MITM for this CONNECT target, but TLS termination is not implemented in this build","type":"mitm_unimplemented","source":"marsala"}}"#;
    let response = format!(
        "HTTP/1.1 501 Not Implemented\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        response_body.len()
    );
    stream
        .write_all(response.as_bytes())
        .await
        .context("failed to write MITM unimplemented response")?;
    stream
        .write_all(response_body)
        .await
        .context("failed to write MITM unimplemented response body")?;
    Ok(())
}

fn add_visible_target_fields(data: &mut Map<String, Value>, request: &ProxyRequest) {
    if request.method.eq_ignore_ascii_case("CONNECT") {
        if let Some((host, port)) = parse_target_host_port(&request.target, 443) {
            data.insert("target_host".into(), host.into());
            data.insert("target_port".into(), port.into());
        }
        return;
    }

    if let Some((host, port, path)) = parse_plain_http_target(request) {
        data.insert("target_host".into(), host.into());
        if let Some(port) = port {
            data.insert("target_port".into(), port.into());
        }
        data.insert("target_path".into(), path.into());
    }
}

fn parse_plain_http_target(request: &ProxyRequest) -> Option<(String, Option<u16>, String)> {
    if let Some(rest) = request.target.strip_prefix("http://") {
        let (authority, path) = split_authority_path(rest);
        let (host, port) = parse_authority(authority, 80)?;
        return Some((host, Some(port), redact_path_query(&path)));
    }

    let host = header_value(&request.headers, "host")?;
    let (host, port) = parse_authority(&host, 80)?;
    Some((host, Some(port), redact_path_query(&request.target)))
}

fn split_authority_path(value: &str) -> (&str, String) {
    if let Some((authority, path)) = value.split_once('/') {
        (authority, format!("/{path}"))
    } else {
        (value, "/".to_string())
    }
}

fn parse_target_host_port(value: &str, default_port: u16) -> Option<(String, u16)> {
    parse_authority(value, default_port)
}

fn parse_authority(value: &str, default_port: u16) -> Option<(String, u16)> {
    let without_userinfo = value.rsplit('@').next().unwrap_or(value);

    if let Some(host) = without_userinfo
        .strip_prefix('[')
        .and_then(|rest| rest.split_once(']').map(|(host, tail)| (host, tail)))
    {
        let port = host
            .1
            .strip_prefix(':')
            .and_then(|port| port.parse::<u16>().ok())
            .unwrap_or(default_port);
        return Some((host.0.to_string(), port));
    }

    match without_userinfo.rsplit_once(':') {
        Some((host, port)) => {
            let port = port.parse::<u16>().ok()?;
            Some((host.to_string(), port))
        }
        None => Some((without_userinfo.to_string(), default_port)),
    }
}

fn sanitize_target(target: &str) -> String {
    let Some((scheme, rest)) = target.split_once("://") else {
        return sanitize_authority(target);
    };
    let (authority, path) = split_authority_path(rest);
    format!(
        "{scheme}://{}{}",
        sanitize_authority(authority),
        redact_path_query(&path)
    )
}

fn sanitize_authority(authority: &str) -> String {
    match authority.rsplit_once('@') {
        Some((_, host)) => format!("[redacted]@{host}"),
        None => authority.to_string(),
    }
}

fn redact_path_query(path: &str) -> String {
    let Some((base, query)) = path.split_once('?') else {
        return path.to_string();
    };

    let redacted_query = query
        .split('&')
        .map(|part| {
            let Some((key, _value)) = part.split_once('=') else {
                return part.to_string();
            };
            if should_redact_query_key(key) {
                format!("{key}=[redacted]")
            } else {
                part.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("&");

    format!("{base}?{redacted_query}")
}

fn should_redact_query_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    let normalized = key.replace('_', "-");
    matches!(normalized.as_str(), "api-key" | "apikey" | "x-api-key")
        || normalized.contains("token")
        || normalized.contains("session")
        || normalized.contains("bearer")
}

fn proxy_auth_shape(headers: &[(String, String)]) -> Map<String, Value> {
    let mut data = Map::new();
    add_proxy_header_presence(&mut data, headers, "authorization", "authorization", true);
    add_proxy_header_presence(
        &mut data,
        headers,
        "proxy-authorization",
        "proxy_authorization",
        true,
    );
    add_proxy_header_presence(&mut data, headers, "cookie", "cookie", false);
    add_proxy_header_presence(&mut data, headers, "x-api-key", "x_api_key", false);
    data
}

fn add_proxy_header_presence(
    data: &mut Map<String, Value>,
    headers: &[(String, String)],
    header_name: &str,
    field: &str,
    include_scheme: bool,
) {
    let Some(value) = header_value(headers, header_name) else {
        data.insert(format!("{field}_present"), false.into());
        return;
    };

    data.insert(format!("{field}_present"), true.into());
    if include_scheme {
        data.insert(
            format!("{field}_scheme"),
            value
                .split_ascii_whitespace()
                .next()
                .filter(|scheme| !scheme.is_empty())
                .map(|scheme| scheme.to_ascii_lowercase())
                .unwrap_or_else(|| "opaque".to_string())
                .into(),
        );
    }
}

fn header_value(headers: &[(String, String)], name: &str) -> Option<String> {
    headers
        .iter()
        .find(|(header_name, _)| header_name == name)
        .map(|(_, value)| value.clone())
}

#[derive(Debug)]
struct ProxyRequest {
    method: String,
    target: String,
    version: String,
    headers: Vec<(String, String)>,
    header_bytes: usize,
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        sync::watch,
    };

    use super::*;
    use crate::event_log::{read_tail_lines, EventLogWriter};

    #[tokio::test]
    async fn plain_http_proxy_probe_logs_metadata_and_redacts_credentials() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let log_path = tempdir.path().join("events.jsonl");
        let mut writer = EventLogWriter::spawn(&log_path, true)
            .await
            .expect("event writer");
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
        let addr = listener.local_addr().expect("local addr");
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let task = tokio::spawn(serve_listener(
            listener,
            AppConfig::default(),
            writer.handle(),
            shutdown_rx,
        ));

        let mut client = TcpStream::connect(addr).await.expect("connect proxy");
        client
            .write_all(
                b"GET http://user:pass@example.com:8080/v1/responses?api_key=query-secret HTTP/1.1\r\nhost: example.com:8080\r\nauthorization: Bearer direct-secret\r\nproxy-authorization: Basic proxy-secret\r\ncookie: session=session-secret\r\nx-api-key: key-secret\r\n\r\n",
            )
            .await
            .expect("write request");
        let mut response = Vec::new();
        client
            .read_to_end(&mut response)
            .await
            .expect("read response");
        assert!(String::from_utf8_lossy(&response).contains("501 Not Implemented"));

        shutdown_tx.send(true).expect("shutdown");
        task.await.expect("task").expect("serve listener");
        writer.shutdown().await.expect("writer shutdown");

        let log_text = fs::read_to_string(&log_path).expect("read log");
        assert!(!log_text.contains("direct-secret"));
        assert!(!log_text.contains("proxy-secret"));
        assert!(!log_text.contains("session-secret"));
        assert!(!log_text.contains("key-secret"));
        assert!(!log_text.contains("query-secret"));
        assert!(!log_text.contains("user:pass"));
        assert!(log_text.contains("[redacted]@example.com"));

        let records: Vec<Value> = read_tail_lines(&log_path, 20)
            .expect("read lines")
            .into_iter()
            .map(|line| serde_json::from_str(&line).expect("json"))
            .collect();
        let event = records
            .iter()
            .find(|record| record["event_type"] == "proxy_request")
            .expect("proxy request event");

        assert_eq!(event["data"]["method"], "GET");
        assert_eq!(event["data"]["target_host"], "example.com");
        assert_eq!(event["data"]["target_port"], 8080);
        assert_eq!(
            event["data"]["target_path"],
            "/v1/responses?api_key=[redacted]"
        );
        assert_eq!(event["data"]["status"], "not_implemented");
        assert_eq!(event["data"]["body_logging"], "disabled");
        assert_eq!(event["data"]["auth_shape"]["authorization_present"], true);
        assert_eq!(
            event["data"]["auth_shape"]["proxy_authorization_present"],
            true
        );
        assert_eq!(event["data"]["auth_shape"]["cookie_present"], true);
        assert_eq!(event["data"]["auth_shape"]["x_api_key_present"], true);
    }

    #[tokio::test]
    async fn allowlisted_connect_returns_mitm_unimplemented_metadata() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let log_path = tempdir.path().join("events.jsonl");
        let mut writer = EventLogWriter::spawn(&log_path, true)
            .await
            .expect("event writer");
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
        let addr = listener.local_addr().expect("local addr");
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let mut config = AppConfig::default();
        config.proxy.enabled = true;
        config.mitm.enabled = true;
        config.mitm.allow_hosts = vec!["api.openai.com".to_string()];
        let task = tokio::spawn(serve_listener(
            listener,
            config,
            writer.handle(),
            shutdown_rx,
        ));

        let mut client = TcpStream::connect(addr).await.expect("connect proxy");
        client
            .write_all(
                b"CONNECT api.openai.com:443 HTTP/1.1\r\nhost: api.openai.com:443\r\nproxy-authorization: Basic proxy-secret\r\n\r\n",
            )
            .await
            .expect("write request");
        let mut response = Vec::new();
        client
            .read_to_end(&mut response)
            .await
            .expect("read response");
        let response = String::from_utf8_lossy(&response);
        assert!(response.contains("501 Not Implemented"));
        assert!(response.contains("mitm_unimplemented"));

        shutdown_tx.send(true).expect("shutdown");
        task.await.expect("task").expect("serve listener");
        writer.shutdown().await.expect("writer shutdown");

        let log_text = fs::read_to_string(&log_path).expect("read log");
        assert!(!log_text.contains("proxy-secret"));
        let records: Vec<Value> = read_tail_lines(&log_path, 20)
            .expect("read lines")
            .into_iter()
            .map(|line| serde_json::from_str(&line).expect("json"))
            .collect();
        let event = records
            .iter()
            .find(|record| record["event_type"] == "proxy_request")
            .expect("proxy request event");

        assert_eq!(event["data"]["method"], "CONNECT");
        assert_eq!(event["data"]["target_host"], "api.openai.com");
        assert_eq!(event["data"]["target_port"], 443);
        assert_eq!(event["data"]["status"], "mitm_unimplemented");
        assert_eq!(event["data"]["connect_action"], "mitm_unimplemented");
        assert_eq!(
            event["data"]["auth_shape"]["proxy_authorization_present"],
            true
        );
    }

    #[test]
    fn connect_target_metadata_parses_host_and_port_without_credentials() {
        let request = ProxyRequest {
            method: "CONNECT".to_string(),
            target: "user:pass@api.openai.com:443".to_string(),
            version: "HTTP/1.1".to_string(),
            headers: vec![(
                "proxy-authorization".to_string(),
                "Basic proxy-secret".to_string(),
            )],
            header_bytes: 120,
        };
        let mut data = Map::new();
        add_visible_target_fields(&mut data, &request);

        assert_eq!(data["target_host"], "api.openai.com");
        assert_eq!(data["target_port"], 443);
        assert_eq!(
            sanitize_target(&request.target),
            "[redacted]@api.openai.com:443"
        );
    }
}
