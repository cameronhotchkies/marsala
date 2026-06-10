#[cfg(test)]
use std::sync::{Mutex, OnceLock};
use std::{
    collections::HashSet,
    fs,
    net::SocketAddr,
    sync::{Arc, Once},
    time::Instant,
};

use anyhow::{Context, Result};
use rcgen::{
    CertificateParams, DistinguishedName, DnType, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair,
    KeyUsagePurpose,
};
use serde_json::{Map, Value};
use tokio::{
    io::{
        self, AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt,
        BufReader,
    },
    net::{TcpListener, TcpStream},
    sync::watch,
    time::{timeout, Duration},
};
use tokio_rustls::{
    rustls::{
        pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName},
        ClientConfig, RootCertStore, ServerConfig,
    },
    TlsAcceptor, TlsConnector,
};
use tracing::{debug, info};

use crate::{
    config::{AppConfig, MitmConfig, MitmConnectAction},
    event_log::EventLogHandle,
};

const MAX_HEADER_BYTES: usize = 64 * 1024;
const MAX_MITM_REQUEST_BODY_BYTES: usize = 1024 * 1024;
const HEADER_READ_TIMEOUT: Duration = Duration::from_secs(10);
const MITM_REQUEST_BODY_READ_TIMEOUT: Duration = Duration::from_secs(10);
const MITM_UPSTREAM_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const MITM_UPSTREAM_TLS_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const MITM_RESPONSE_HEADER_READ_TIMEOUT: Duration = Duration::from_secs(10);
#[cfg(not(test))]
const MITM_RESPONSE_BODY_COPY_TIMEOUT: Duration = Duration::from_secs(30);
#[cfg(test)]
const MITM_RESPONSE_BODY_COPY_TIMEOUT: Duration = Duration::from_millis(200);

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
        MitmConnectAction::Mitm => ConnectAction::Mitm,
        MitmConnectAction::Tunnel => ConnectAction::Tunnel,
    };

    if connect_action == ConnectAction::Mitm {
        return handle_mitm_connect(
            reader, peer_addr, config, event_log, started, request, host, port,
        )
        .await;
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

async fn handle_mitm_connect(
    mut reader: BufReader<TcpStream>,
    peer_addr: SocketAddr,
    config: AppConfig,
    event_log: EventLogHandle,
    started: Instant,
    request: ProxyRequest,
    host: String,
    port: u16,
) -> Result<()> {
    let acceptor = match mitm_tls_acceptor(&config.mitm, &host) {
        Ok(acceptor) => acceptor,
        Err(error) => {
            let message = format!("failed to prepare MITM TLS config: {error}");
            let _ = reader
                .get_mut()
                .write_all(b"HTTP/1.1 502 Bad Gateway\r\ncontent-length: 0\r\n\r\n")
                .await;
            emit_mitm_tls_event(
                &event_log,
                MitmTlsEvent {
                    started,
                    target_host: &host,
                    target_port: port,
                    status: "error",
                    alpn: None,
                    error: Some(message.clone()),
                },
            );
            emit_proxy_event(
                &event_log,
                ProxyEvent {
                    started,
                    peer_addr,
                    request: Some(request),
                    status: "error",
                    connect_action: Some(ConnectAction::Mitm),
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
        .context("failed to acknowledge MITM CONNECT")?;

    let stream = reader.into_inner();
    let mut tls_stream = match timeout(HEADER_READ_TIMEOUT, acceptor.accept(stream)).await {
        Ok(Ok(tls_stream)) => tls_stream,
        Ok(Err(error)) => {
            let message = format!("MITM TLS handshake failed: {error}");
            emit_mitm_tls_event(
                &event_log,
                MitmTlsEvent {
                    started,
                    target_host: &host,
                    target_port: port,
                    status: "error",
                    alpn: None,
                    error: Some(message.clone()),
                },
            );
            emit_proxy_event(
                &event_log,
                ProxyEvent {
                    started,
                    peer_addr,
                    request: Some(request),
                    status: "error",
                    connect_action: Some(ConnectAction::Mitm),
                    error: Some(message.clone()),
                },
            );
            return Err(anyhow::anyhow!(message));
        }
        Err(_) => {
            let message = "timed out during MITM TLS handshake".to_string();
            emit_mitm_tls_event(
                &event_log,
                MitmTlsEvent {
                    started,
                    target_host: &host,
                    target_port: port,
                    status: "error",
                    alpn: None,
                    error: Some(message.clone()),
                },
            );
            emit_proxy_event(
                &event_log,
                ProxyEvent {
                    started,
                    peer_addr,
                    request: Some(request),
                    status: "error",
                    connect_action: Some(ConnectAction::Mitm),
                    error: Some(message.clone()),
                },
            );
            return Err(anyhow::anyhow!(message));
        }
    };

    let alpn = tls_stream
        .get_ref()
        .1
        .alpn_protocol()
        .map(|value| String::from_utf8_lossy(value).to_string());
    emit_mitm_tls_event(
        &event_log,
        MitmTlsEvent {
            started,
            target_host: &host,
            target_port: port,
            status: "handshake_ok",
            alpn: alpn.as_deref(),
            error: None,
        },
    );

    let mut tls_reader = BufReader::new(&mut tls_stream);
    let (status, error) = if alpn.as_deref() == Some("h2") {
        let message = "MITM HTTP/2 forwarding is not implemented".to_string();
        emit_mitm_request_event(
            &event_log,
            MitmRequestEvent {
                started,
                target_host: &host,
                target_port: port,
                request: None,
                status: "http2_unsupported",
                alpn: alpn.as_deref(),
                request_body_bytes: None,
                error: Some(message.clone()),
            },
        );
        let _ = tls_reader.get_mut().shutdown().await;
        ("http2_unsupported", Some(message))
    } else {
        match timeout(HEADER_READ_TIMEOUT, read_proxy_request(&mut tls_reader)).await {
            Ok(Ok(http_request)) => {
                match forward_mitm_http1_request(&mut tls_reader, &host, port, &http_request).await
                {
                    Ok(result) => {
                        emit_mitm_request_event(
                            &event_log,
                            MitmRequestEvent {
                                started,
                                target_host: &host,
                                target_port: port,
                                request: Some(&http_request),
                                status: result.request_status,
                                alpn: alpn.as_deref(),
                                request_body_bytes: Some(result.request_body_bytes),
                                error: result.request_error.clone(),
                            },
                        );
                        if let Some(response) = &result.response {
                            emit_mitm_response_event(
                                &event_log,
                                MitmResponseEvent {
                                    started,
                                    target_host: &host,
                                    target_port: port,
                                    request: &http_request,
                                    status: response.status,
                                    upstream_status: response.upstream_status,
                                    response_header_bytes: response.response_header_bytes,
                                    response_body_bytes: response.response_body_bytes,
                                    websocket_tunnel_status: response.websocket_tunnel_status,
                                    error: response.error.clone(),
                                },
                            );
                        }
                        let _ = tls_reader.get_mut().shutdown().await;
                        (result.proxy_status, result.proxy_error)
                    }
                    Err(error) => {
                        let message = error.to_string();
                        let _ = write_mitm_http_error_response(
                            tls_reader.get_mut(),
                            502,
                            "Bad Gateway",
                            "mitm_upstream_error",
                            "Marsala could not complete upstream MITM forwarding",
                        )
                        .await;
                        emit_mitm_request_event(
                            &event_log,
                            MitmRequestEvent {
                                started,
                                target_host: &host,
                                target_port: port,
                                request: Some(&http_request),
                                status: "error",
                                alpn: alpn.as_deref(),
                                request_body_bytes: None,
                                error: Some(message.clone()),
                            },
                        );
                        emit_mitm_response_event(
                            &event_log,
                            MitmResponseEvent {
                                started,
                                target_host: &host,
                                target_port: port,
                                request: &http_request,
                                status: "error",
                                upstream_status: None,
                                response_header_bytes: None,
                                response_body_bytes: None,
                                websocket_tunnel_status: None,
                                error: Some(message.clone()),
                            },
                        );
                        let _ = tls_reader.get_mut().shutdown().await;
                        ("error", Some(message))
                    }
                }
            }
            Ok(Err(error)) => {
                let message = error.to_string();
                emit_mitm_request_event(
                    &event_log,
                    MitmRequestEvent {
                        started,
                        target_host: &host,
                        target_port: port,
                        request: None,
                        status: "error",
                        alpn: alpn.as_deref(),
                        request_body_bytes: None,
                        error: Some(message.clone()),
                    },
                );
                ("error", Some(message))
            }
            Err(_) => {
                let message = "timed out while reading decrypted MITM request headers".to_string();
                emit_mitm_request_event(
                    &event_log,
                    MitmRequestEvent {
                        started,
                        target_host: &host,
                        target_port: port,
                        request: None,
                        status: "error",
                        alpn: alpn.as_deref(),
                        request_body_bytes: None,
                        error: Some(message.clone()),
                    },
                );
                ("error", Some(message))
            }
        }
    };

    emit_proxy_event(
        &event_log,
        ProxyEvent {
            started,
            peer_addr,
            request: Some(request),
            status,
            connect_action: Some(ConnectAction::Mitm),
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

async fn read_proxy_request<R>(reader: &mut R) -> Result<ProxyRequest>
where
    R: AsyncBufRead + Unpin,
{
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
    Mitm,
}

impl ConnectAction {
    fn as_str(self) -> &'static str {
        match self {
            Self::Tunnel => "tunnel",
            Self::Mitm => "mitm",
        }
    }
}

struct MitmTlsEvent<'a> {
    started: Instant,
    target_host: &'a str,
    target_port: u16,
    status: &'a str,
    alpn: Option<&'a str>,
    error: Option<String>,
}

fn emit_mitm_tls_event(event_log: &EventLogHandle, event: MitmTlsEvent<'_>) {
    let mut data = Map::new();
    data.insert(
        "elapsed_ms".into(),
        (event.started.elapsed().as_millis() as u64).into(),
    );
    data.insert("target_host".into(), event.target_host.into());
    data.insert("target_port".into(), event.target_port.into());
    data.insert("status".into(), event.status.into());
    data.insert("body_logging".into(), "disabled".into());
    if let Some(alpn) = event.alpn {
        data.insert("alpn".into(), alpn.into());
    }
    if let Some(error) = event.error {
        data.insert("error".into(), error.into());
    }

    event_log.emit("mitm_tls", Value::Object(data));
}

struct MitmRequestEvent<'a> {
    started: Instant,
    target_host: &'a str,
    target_port: u16,
    request: Option<&'a ProxyRequest>,
    status: &'a str,
    alpn: Option<&'a str>,
    request_body_bytes: Option<u64>,
    error: Option<String>,
}

fn emit_mitm_request_event(event_log: &EventLogHandle, event: MitmRequestEvent<'_>) {
    let mut data = Map::new();
    data.insert(
        "elapsed_ms".into(),
        (event.started.elapsed().as_millis() as u64).into(),
    );
    data.insert("target_host".into(), event.target_host.into());
    data.insert("target_port".into(), event.target_port.into());
    data.insert("status".into(), event.status.into());
    data.insert("body_logging".into(), "disabled".into());
    if let Some(alpn) = event.alpn {
        data.insert("alpn".into(), alpn.into());
    }
    if let Some(error) = event.error {
        data.insert("error".into(), error.into());
    }
    if let Some(request) = event.request {
        data.insert("method".into(), request.method.clone().into());
        data.insert("path".into(), sanitize_target(&request.target).into());
        data.insert("http_version".into(), request.version.clone().into());
        data.insert("header_bytes".into(), (request.header_bytes as u64).into());
        if let Some(request_body_bytes) = event.request_body_bytes {
            data.insert("request_body_bytes".into(), request_body_bytes.into());
        }
        data.insert(
            "auth_shape".into(),
            Value::Object(proxy_auth_shape(&request.headers)),
        );
    }

    event_log.emit("mitm_request", Value::Object(data));
}

struct MitmResponseEvent<'a> {
    started: Instant,
    target_host: &'a str,
    target_port: u16,
    request: &'a ProxyRequest,
    status: &'a str,
    upstream_status: Option<u16>,
    response_header_bytes: Option<u64>,
    response_body_bytes: Option<u64>,
    websocket_tunnel_status: Option<&'a str>,
    error: Option<String>,
}

fn emit_mitm_response_event(event_log: &EventLogHandle, event: MitmResponseEvent<'_>) {
    let mut data = Map::new();
    data.insert(
        "elapsed_ms".into(),
        (event.started.elapsed().as_millis() as u64).into(),
    );
    data.insert("target_host".into(), event.target_host.into());
    data.insert("target_port".into(), event.target_port.into());
    data.insert("method".into(), event.request.method.clone().into());
    data.insert(
        "path".into(),
        redact_path_query(&event.request.target).into(),
    );
    data.insert("status".into(), event.status.into());
    data.insert("body_logging".into(), "disabled".into());
    if let Some(upstream_status) = event.upstream_status {
        data.insert("upstream_status".into(), upstream_status.into());
    }
    if let Some(response_header_bytes) = event.response_header_bytes {
        data.insert("response_header_bytes".into(), response_header_bytes.into());
    }
    if let Some(response_body_bytes) = event.response_body_bytes {
        data.insert("response_body_bytes".into(), response_body_bytes.into());
    }
    if let Some(websocket_tunnel_status) = event.websocket_tunnel_status {
        data.insert(
            "websocket_tunnel_status".into(),
            websocket_tunnel_status.into(),
        );
    }
    if let Some(error) = event.error {
        data.insert("error".into(), error.into());
    }

    event_log.emit("mitm_response", Value::Object(data));
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

async fn write_mitm_http_error_response<W>(
    stream: &mut W,
    status_code: u16,
    reason: &str,
    error_type: &str,
    message: &str,
) -> Result<()>
where
    W: AsyncWrite + Unpin,
{
    let response_body = serde_json::json!({
        "error": {
            "message": message,
            "type": error_type,
            "source": "marsala"
        }
    })
    .to_string();
    let response = format!(
        "HTTP/1.1 {status_code} {reason}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        response_body.as_bytes().len()
    );
    stream
        .write_all(response.as_bytes())
        .await
        .context("failed to write MITM HTTP error response")?;
    stream
        .write_all(response_body.as_bytes())
        .await
        .context("failed to write MITM HTTP error response body")?;
    Ok(())
}

async fn forward_mitm_http1_request<S>(
    downstream: &mut BufReader<S>,
    host: &str,
    port: u16,
    request: &ProxyRequest,
) -> Result<MitmForwardResult>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    if is_http2_request(request, None) {
        write_mitm_http_error_response(
            downstream.get_mut(),
            505,
            "HTTP Version Not Supported",
            "mitm_http2_unsupported",
            "Marsala MITM currently supports HTTP/1.1 forwarding only",
        )
        .await?;
        return Ok(MitmForwardResult::rejected(
            "http2_unsupported",
            "MITM HTTP/2 forwarding is not implemented",
        ));
    }

    if request.version != "HTTP/1.1" {
        write_mitm_http_error_response(
            downstream.get_mut(),
            505,
            "HTTP Version Not Supported",
            "mitm_http_version_unsupported",
            "Marsala MITM currently supports HTTP/1.1 forwarding only",
        )
        .await?;
        return Ok(MitmForwardResult::rejected(
            "http_version_unsupported",
            "only HTTP/1.1 MITM forwarding is implemented",
        ));
    }

    if let Err(message) = validate_mitm_request_authority(request, host, port) {
        write_mitm_http_error_response(
            downstream.get_mut(),
            400,
            "Bad Request",
            "mitm_authority_mismatch",
            "Marsala rejected a decrypted request whose authority did not match the CONNECT target",
        )
        .await?;
        return Ok(MitmForwardResult::rejected("authority_mismatch", message));
    }

    if unsupported_http_upgrade_request(request) {
        write_mitm_http_error_response(
            downstream.get_mut(),
            501,
            "Not Implemented",
            "mitm_upgrade_unsupported",
            "Marsala MITM supports only HTTP/1.1 WebSocket upgrades in this steel thread",
        )
        .await?;
        return Ok(MitmForwardResult::rejected(
            "upgrade_unsupported",
            "only WebSocket HTTP/1.1 upgrade forwarding is implemented",
        ));
    }

    if websocket_upgrade_attempt(request) {
        if let Err(message) = validate_websocket_upgrade_request(request) {
            write_mitm_http_error_response(
                downstream.get_mut(),
                400,
                "Bad Request",
                "mitm_websocket_malformed",
                "Marsala rejected a malformed WebSocket upgrade request",
            )
            .await?;
            return Ok(MitmForwardResult::rejected("websocket_malformed", message));
        }

        return forward_mitm_websocket_upgrade(downstream, host, port, request).await;
    }

    if header_value(&request.headers, "transfer-encoding").is_some() {
        write_mitm_http_error_response(
            downstream.get_mut(),
            501,
            "Not Implemented",
            "mitm_request_body_streaming_unsupported",
            "Marsala MITM currently supports no-body or bounded Content-Length request bodies only",
        )
        .await?;
        return Ok(MitmForwardResult::rejected(
            "request_body_streaming_unsupported",
            "request transfer-encoding is not supported by MITM forwarding",
        ));
    }

    let request_body_len = match request_content_length(request) {
        Ok(request_body_len) => request_body_len,
        Err(error) => {
            write_mitm_http_error_response(
                downstream.get_mut(),
                400,
                "Bad Request",
                "mitm_invalid_content_length",
                "Marsala rejected an ambiguous or invalid Content-Length header",
            )
            .await?;
            return Ok(MitmForwardResult::rejected(
                "invalid_content_length",
                &error.to_string(),
            ));
        }
    };
    if request_body_len > MAX_MITM_REQUEST_BODY_BYTES {
        write_mitm_http_error_response(
            downstream.get_mut(),
            413,
            "Payload Too Large",
            "mitm_request_body_too_large",
            "Marsala MITM request body exceeds the bounded forwarding limit",
        )
        .await?;
        return Ok(MitmForwardResult::rejected(
            "request_body_too_large",
            "request body exceeds MITM forwarding limit",
        ));
    }

    let mut request_body = vec![0u8; request_body_len];
    if request_body_len > 0 {
        match timeout(
            MITM_REQUEST_BODY_READ_TIMEOUT,
            downstream.read_exact(&mut request_body),
        )
        .await
        {
            Ok(Ok(_)) => {}
            Ok(Err(error)) => {
                return Err(error).context("failed to read bounded MITM request body")
            }
            Err(_) => {
                write_mitm_http_error_response(
                    downstream.get_mut(),
                    408,
                    "Request Timeout",
                    "mitm_request_body_timeout",
                    "Marsala timed out while reading the bounded request body",
                )
                .await?;
                return Ok(MitmForwardResult::rejected(
                    "request_body_timeout",
                    "timed out while reading bounded MITM request body",
                ));
            }
        }
    }

    let mut upstream = connect_upstream_tls(host, port).await?;
    write_upstream_request(&mut upstream, host, port, request, &request_body).await?;

    let mut upstream_reader = BufReader::new(upstream);
    let response = timeout(
        MITM_RESPONSE_HEADER_READ_TIMEOUT,
        read_http_response(&mut upstream_reader),
    )
    .await
    .map_err(|_| anyhow::anyhow!("timed out while reading MITM upstream response headers"))??;
    let response_header = serialize_downstream_response(&response);
    downstream
        .get_mut()
        .write_all(response_header.as_bytes())
        .await
        .context("failed to write MITM response headers downstream")?;
    let response_body_bytes = match timeout(
        MITM_RESPONSE_BODY_COPY_TIMEOUT,
        io::copy(&mut upstream_reader, downstream.get_mut()),
    )
    .await
    {
        Ok(Ok(response_body_bytes)) => response_body_bytes,
        Ok(Err(error)) => {
            return Err(error).context("failed to stream MITM response body downstream")
        }
        Err(_) => {
            let message = "timed out while streaming MITM upstream response body";
            return Ok(MitmForwardResult {
                request_status: "forwarded",
                request_body_bytes: request_body_len as u64,
                request_error: None,
                response: Some(MitmResponseResult {
                    status: "response_body_timeout",
                    upstream_status: Some(response.status_code),
                    response_header_bytes: Some(response.header_bytes as u64),
                    response_body_bytes: None,
                    websocket_tunnel_status: None,
                    error: Some(message.to_string()),
                }),
                proxy_status: "response_body_timeout",
                proxy_error: Some(message.to_string()),
            });
        }
    };

    Ok(MitmForwardResult {
        request_status: "forwarded",
        request_body_bytes: request_body_len as u64,
        request_error: None,
        response: Some(MitmResponseResult {
            status: "forwarded",
            upstream_status: Some(response.status_code),
            response_header_bytes: Some(response.header_bytes as u64),
            response_body_bytes: Some(response_body_bytes),
            websocket_tunnel_status: None,
            error: None,
        }),
        proxy_status: "forwarded",
        proxy_error: None,
    })
}

async fn forward_mitm_websocket_upgrade<S>(
    downstream: &mut BufReader<S>,
    host: &str,
    port: u16,
    request: &ProxyRequest,
) -> Result<MitmForwardResult>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut upstream = connect_upstream_tls(host, port).await?;
    write_upstream_websocket_request(&mut upstream, host, port, request).await?;
    flush_buffered_reader_bytes(downstream, &mut upstream)
        .await
        .context("failed to forward buffered WebSocket client bytes upstream")?;

    let mut upstream_reader = BufReader::new(upstream);
    let response = timeout(
        MITM_RESPONSE_HEADER_READ_TIMEOUT,
        read_http_response(&mut upstream_reader),
    )
    .await
    .map_err(|_| anyhow::anyhow!("timed out while reading MITM upstream response headers"))??;

    if response.status_code != 101 {
        let response_header = serialize_downstream_response(&response);
        downstream
            .get_mut()
            .write_all(response_header.as_bytes())
            .await
            .context("failed to write MITM WebSocket non-101 response headers downstream")?;
        let response_body_bytes = match timeout(
            MITM_RESPONSE_BODY_COPY_TIMEOUT,
            io::copy(&mut upstream_reader, downstream.get_mut()),
        )
        .await
        {
            Ok(Ok(response_body_bytes)) => response_body_bytes,
            Ok(Err(error)) => {
                return Err(error)
                    .context("failed to stream MITM WebSocket non-101 response body downstream")
            }
            Err(_) => {
                let message = "timed out while streaming MITM WebSocket non-101 response body";
                return Ok(MitmForwardResult {
                    request_status: "websocket_forwarded",
                    request_body_bytes: 0,
                    request_error: None,
                    response: Some(MitmResponseResult {
                        status: "websocket_non_101_response_body_timeout",
                        upstream_status: Some(response.status_code),
                        response_header_bytes: Some(response.header_bytes as u64),
                        response_body_bytes: None,
                        websocket_tunnel_status: None,
                        error: Some(message.to_string()),
                    }),
                    proxy_status: "websocket_non_101_response_body_timeout",
                    proxy_error: Some(message.to_string()),
                });
            }
        };

        return Ok(MitmForwardResult {
            request_status: "websocket_forwarded",
            request_body_bytes: 0,
            request_error: None,
            response: Some(MitmResponseResult {
                status: "websocket_non_101_response",
                upstream_status: Some(response.status_code),
                response_header_bytes: Some(response.header_bytes as u64),
                response_body_bytes: Some(response_body_bytes),
                websocket_tunnel_status: None,
                error: None,
            }),
            proxy_status: "websocket_non_101_response",
            proxy_error: None,
        });
    }

    let response_header = serialize_downstream_websocket_response(&response);
    downstream
        .get_mut()
        .write_all(response_header.as_bytes())
        .await
        .context("failed to write MITM WebSocket 101 response headers downstream")?;
    flush_buffered_reader_bytes(&mut upstream_reader, downstream.get_mut())
        .await
        .context("failed to forward buffered WebSocket upstream bytes downstream")?;

    let tunnel_result =
        io::copy_bidirectional(downstream.get_mut(), upstream_reader.get_mut()).await;
    let (status, error) = match tunnel_result {
        Ok(_) => ("websocket_tunnel_closed", None),
        Err(error) => ("websocket_tunnel_error", Some(error.to_string())),
    };

    Ok(MitmForwardResult {
        request_status: "websocket_forwarded",
        request_body_bytes: 0,
        request_error: None,
        response: Some(MitmResponseResult {
            status,
            upstream_status: Some(response.status_code),
            response_header_bytes: Some(response.header_bytes as u64),
            response_body_bytes: None,
            websocket_tunnel_status: Some(status),
            error: error.clone(),
        }),
        proxy_status: status,
        proxy_error: error,
    })
}

async fn connect_upstream_tls(
    host: &str,
    port: u16,
) -> Result<tokio_rustls::client::TlsStream<TcpStream>> {
    ensure_rustls_crypto_provider();
    let connect_addr = format!("{host}:{port}");
    let stream = timeout(
        MITM_UPSTREAM_CONNECT_TIMEOUT,
        TcpStream::connect(&connect_addr),
    )
    .await
    .map_err(|_| anyhow::anyhow!("timed out connecting MITM upstream {connect_addr}"))?
    .with_context(|| format!("failed to connect MITM upstream {connect_addr}"))?;
    let server_name = ServerName::try_from(host.to_string())
        .with_context(|| format!("invalid MITM upstream DNS name {host}"))?;
    let client_config = ClientConfig::builder()
        .with_root_certificates(mitm_upstream_root_store())
        .with_no_client_auth();
    let connector = TlsConnector::from(Arc::new(client_config));
    timeout(
        MITM_UPSTREAM_TLS_HANDSHAKE_TIMEOUT,
        connector.connect(server_name, stream),
    )
    .await
    .map_err(|_| {
        anyhow::anyhow!("timed out during MITM upstream TLS handshake with {connect_addr}")
    })?
    .with_context(|| format!("failed MITM upstream TLS handshake with {connect_addr}"))
}

fn mitm_upstream_root_store() -> RootCertStore {
    let mut root_store = RootCertStore::empty();
    root_store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    add_test_upstream_roots(&mut root_store);
    root_store
}

#[cfg(not(test))]
fn add_test_upstream_roots(_root_store: &mut RootCertStore) {}

#[cfg(test)]
fn add_test_upstream_roots(root_store: &mut RootCertStore) {
    for cert in test_upstream_roots()
        .lock()
        .expect("test upstream root lock")
        .iter()
    {
        root_store
            .add(cert.clone())
            .expect("add test upstream root");
    }
}

#[cfg(test)]
fn trust_test_upstream_root(cert: CertificateDer<'static>) {
    test_upstream_roots()
        .lock()
        .expect("test upstream root lock")
        .push(cert);
}

#[cfg(test)]
fn test_upstream_roots() -> &'static Mutex<Vec<CertificateDer<'static>>> {
    static ROOTS: OnceLock<Mutex<Vec<CertificateDer<'static>>>> = OnceLock::new();
    ROOTS.get_or_init(|| Mutex::new(Vec::new()))
}

async fn write_upstream_request<W>(
    upstream: &mut W,
    host: &str,
    port: u16,
    request: &ProxyRequest,
    body: &[u8],
) -> Result<()>
where
    W: AsyncWrite + Unpin,
{
    let target = upstream_request_target(&request.target);
    let mut head = format!("{} {} {}\r\n", request.method, target, request.version);
    for (name, value) in filter_upstream_request_headers(request) {
        head.push_str(&name);
        head.push_str(": ");
        head.push_str(&value);
        head.push_str("\r\n");
    }
    if header_value(&request.headers, "host").is_none() {
        head.push_str("host: ");
        head.push_str(&authority_for_host_header(host, port));
        head.push_str("\r\n");
    }
    head.push_str("connection: close\r\n\r\n");
    upstream
        .write_all(head.as_bytes())
        .await
        .context("failed to write MITM request headers upstream")?;
    if !body.is_empty() {
        upstream
            .write_all(body)
            .await
            .context("failed to write bounded MITM request body upstream")?;
    }
    upstream
        .flush()
        .await
        .context("failed to flush MITM request upstream")?;
    Ok(())
}

async fn write_upstream_websocket_request<W>(
    upstream: &mut W,
    host: &str,
    port: u16,
    request: &ProxyRequest,
) -> Result<()>
where
    W: AsyncWrite + Unpin,
{
    let target = upstream_request_target(&request.target);
    let mut head = format!("{} {} {}\r\n", request.method, target, request.version);
    for (name, value) in filter_upstream_websocket_request_headers(request) {
        head.push_str(&name);
        head.push_str(": ");
        head.push_str(&value);
        head.push_str("\r\n");
    }
    if header_value(&request.headers, "host").is_none() {
        head.push_str("host: ");
        head.push_str(&authority_for_host_header(host, port));
        head.push_str("\r\n");
    }
    let upgrade = header_value(&request.headers, "upgrade").unwrap_or_else(|| "websocket".into());
    head.push_str("upgrade: ");
    head.push_str(&upgrade);
    head.push_str("\r\nconnection: Upgrade\r\n\r\n");
    upstream
        .write_all(head.as_bytes())
        .await
        .context("failed to write MITM WebSocket request headers upstream")?;
    upstream
        .flush()
        .await
        .context("failed to flush MITM WebSocket request upstream")?;
    Ok(())
}

async fn read_http_response<R>(reader: &mut R) -> Result<HttpResponse>
where
    R: AsyncBufRead + Unpin,
{
    let mut total_bytes = 0usize;
    let mut status_line = String::new();
    let read = reader
        .read_line(&mut status_line)
        .await
        .context("failed to read MITM upstream response status line")?;
    total_bytes += read;
    if read == 0 {
        return Err(anyhow::anyhow!(
            "upstream closed before MITM response status line"
        ));
    }
    if total_bytes > MAX_HEADER_BYTES {
        return Err(anyhow::anyhow!("MITM response headers exceeded size limit"));
    }

    let mut parts = status_line.trim_end().splitn(3, ' ');
    let version = parts
        .next()
        .ok_or_else(|| anyhow::anyhow!("missing MITM response version"))?
        .to_string();
    let status_code = parts
        .next()
        .ok_or_else(|| anyhow::anyhow!("missing MITM response status"))?
        .parse::<u16>()
        .context("invalid MITM response status")?;
    let reason = parts.next().unwrap_or("").to_string();

    let mut headers = Vec::new();
    loop {
        let mut line = String::new();
        let read = reader
            .read_line(&mut line)
            .await
            .context("failed to read MITM upstream response header")?;
        total_bytes += read;
        if read == 0 || line == "\r\n" || line == "\n" {
            break;
        }
        if total_bytes > MAX_HEADER_BYTES {
            return Err(anyhow::anyhow!("MITM response headers exceeded size limit"));
        }
        if let Some((name, value)) = line.trim_end().split_once(':') {
            headers.push((name.trim().to_ascii_lowercase(), value.trim().to_string()));
        }
    }

    Ok(HttpResponse {
        version,
        status_code,
        reason,
        headers,
        header_bytes: total_bytes,
    })
}

fn serialize_downstream_response(response: &HttpResponse) -> String {
    let mut head = format!(
        "{} {} {}\r\n",
        response.version, response.status_code, response.reason
    );
    for (name, value) in filter_downstream_response_headers(response) {
        head.push_str(&name);
        head.push_str(": ");
        head.push_str(&value);
        head.push_str("\r\n");
    }
    head.push_str("connection: close\r\n\r\n");
    head
}

fn serialize_downstream_websocket_response(response: &HttpResponse) -> String {
    let mut head = format!(
        "{} {} {}\r\n",
        response.version, response.status_code, response.reason
    );
    for (name, value) in filter_downstream_websocket_response_headers(response) {
        head.push_str(&name);
        head.push_str(": ");
        head.push_str(&value);
        head.push_str("\r\n");
    }
    let upgrade = header_value(&response.headers, "upgrade").unwrap_or_else(|| "websocket".into());
    let connection =
        header_value(&response.headers, "connection").unwrap_or_else(|| "Upgrade".into());
    head.push_str("upgrade: ");
    head.push_str(&upgrade);
    head.push_str("\r\nconnection: ");
    head.push_str(&connection);
    head.push_str("\r\n\r\n");
    head
}

async fn flush_buffered_reader_bytes<R, W>(reader: &mut BufReader<R>, writer: &mut W) -> Result<u64>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let buffered = reader.buffer().to_vec();
    if buffered.is_empty() {
        return Ok(0);
    }

    writer
        .write_all(&buffered)
        .await
        .context("failed to write buffered bytes")?;
    reader.consume(buffered.len());
    Ok(buffered.len() as u64)
}

fn mitm_tls_acceptor(config: &MitmConfig, host: &str) -> Result<TlsAcceptor> {
    let server_config = mitm_server_config(config, host)?;
    Ok(TlsAcceptor::from(Arc::new(server_config)))
}

fn mitm_server_config(config: &MitmConfig, host: &str) -> Result<ServerConfig> {
    ensure_rustls_crypto_provider();

    let ca_cert_pem = fs::read_to_string(&config.ca_cert_path).with_context(|| {
        format!(
            "failed to read CA certificate {}",
            config.ca_cert_path.display()
        )
    })?;
    let ca_key_pem = fs::read_to_string(&config.ca_key_path).with_context(|| {
        format!(
            "failed to read CA private key {}",
            config.ca_key_path.display()
        )
    })?;
    let ca_key = KeyPair::from_pem(&ca_key_pem).context("failed to parse CA private key")?;
    let issuer =
        Issuer::from_ca_cert_pem(&ca_cert_pem, ca_key).context("failed to parse CA certificate")?;

    let leaf_key = KeyPair::generate().context("failed to generate MITM leaf private key")?;
    let mut params = CertificateParams::new(vec![host.to_string()])
        .with_context(|| format!("failed to create MITM leaf params for {host}"))?;
    let mut distinguished_name = DistinguishedName::new();
    distinguished_name.push(DnType::CommonName, host);
    params.distinguished_name = distinguished_name;
    params.is_ca = IsCa::NoCa;
    params.key_usages = vec![
        KeyUsagePurpose::DigitalSignature,
        KeyUsagePurpose::KeyEncipherment,
    ];
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];

    let leaf_cert = params
        .signed_by(&leaf_key, &issuer)
        .with_context(|| format!("failed to sign MITM leaf certificate for {host}"))?;
    let cert_chain = vec![CertificateDer::from(leaf_cert.der().to_vec())];
    let private_key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(leaf_key.serialize_der()));

    let mut server_config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(cert_chain, private_key)
        .context("failed to build MITM TLS server config")?;
    server_config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(server_config)
}

fn ensure_rustls_crypto_provider() {
    static RUSTLS_PROVIDER: Once = Once::new();

    RUSTLS_PROVIDER.call_once(|| {
        let _ = tokio_rustls::rustls::crypto::ring::default_provider().install_default();
    });
}

fn is_http2_request(request: &ProxyRequest, alpn: Option<&str>) -> bool {
    alpn == Some("h2")
        || (request.method == "PRI" && request.target == "*" && request.version == "HTTP/2.0")
}

fn is_websocket_upgrade(request: &ProxyRequest) -> bool {
    header_value(&request.headers, "upgrade")
        .map(|value| value.trim().eq_ignore_ascii_case("websocket"))
        .unwrap_or(false)
}

fn websocket_upgrade_attempt(request: &ProxyRequest) -> bool {
    is_websocket_upgrade(request)
        || connection_header_tokens(&request.headers).contains("upgrade")
        || request
            .headers
            .iter()
            .any(|(name, _)| name.starts_with("sec-websocket-"))
}

fn unsupported_http_upgrade_request(request: &ProxyRequest) -> bool {
    header_value(&request.headers, "upgrade")
        .map(|value| !value.trim().eq_ignore_ascii_case("websocket"))
        .unwrap_or(false)
}

fn validate_websocket_upgrade_request(
    request: &ProxyRequest,
) -> std::result::Result<(), &'static str> {
    if !request.method.eq_ignore_ascii_case("GET") {
        return Err("WebSocket upgrade request method must be GET");
    }
    if !is_websocket_upgrade(request) {
        return Err("WebSocket upgrade request is missing Upgrade: websocket");
    }
    if !connection_header_tokens(&request.headers).contains("upgrade") {
        return Err("WebSocket upgrade request is missing Connection: Upgrade");
    }
    if header_value(&request.headers, "sec-websocket-key")
        .map(|value| value.trim().is_empty())
        .unwrap_or(true)
    {
        return Err("WebSocket upgrade request is missing Sec-WebSocket-Key");
    }
    if header_value(&request.headers, "sec-websocket-version")
        .map(|value| value.trim() != "13")
        .unwrap_or(true)
    {
        return Err("WebSocket upgrade request is missing Sec-WebSocket-Version: 13");
    }
    if header_value(&request.headers, "transfer-encoding").is_some() {
        return Err("WebSocket upgrade request transfer-encoding is not supported");
    }
    match request_content_length(request) {
        Ok(0) => Ok(()),
        Ok(_) => Err("WebSocket upgrade request bodies are not supported"),
        Err(_) => Err("WebSocket upgrade request has invalid Content-Length"),
    }
}

fn validate_mitm_request_authority(
    request: &ProxyRequest,
    connect_host: &str,
    connect_port: u16,
) -> std::result::Result<(), &'static str> {
    let mut target_has_matching_authority = false;
    if let Some((scheme, rest)) = request.target.split_once("://") {
        if scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https") {
            let default_port = if scheme.eq_ignore_ascii_case("http") {
                80
            } else {
                443
            };
            let (authority, _) = split_authority_path(rest);
            let Some((target_host, target_port)) = parse_authority(authority, default_port) else {
                return Err("absolute-form request authority is invalid");
            };
            if !authority_matches(connect_host, connect_port, &target_host, target_port) {
                return Err("absolute-form request authority does not match CONNECT target");
            }
            target_has_matching_authority = true;
        }
    }

    let mut has_host = false;
    for (_, value) in request.headers.iter().filter(|(name, _)| name == "host") {
        has_host = true;
        let Some((host, port)) = parse_authority(value, 443) else {
            return Err("Host header authority is invalid");
        };
        if !authority_matches(connect_host, connect_port, &host, port) {
            return Err("Host header authority does not match CONNECT target");
        }
    }

    if !has_host && !target_has_matching_authority {
        return Err("HTTP/1.1 origin-form request is missing Host header");
    }

    Ok(())
}

fn authority_matches(connect_host: &str, connect_port: u16, host: &str, port: u16) -> bool {
    connect_host.eq_ignore_ascii_case(host) && connect_port == port
}

fn request_content_length(request: &ProxyRequest) -> Result<usize> {
    let values = request
        .headers
        .iter()
        .filter(|(name, _)| name == "content-length")
        .map(|(_, value)| value)
        .collect::<Vec<_>>();
    let Some(value) = values.first() else {
        return Ok(0);
    };
    let first = value
        .parse::<usize>()
        .context("invalid MITM request content-length")?;
    if values.len() > 1 {
        for value in values.iter().skip(1) {
            let next = value
                .parse::<usize>()
                .context("invalid MITM request content-length")?;
            if next != first {
                return Err(anyhow::anyhow!(
                    "conflicting MITM request content-length headers"
                ));
            }
        }
        return Err(anyhow::anyhow!(
            "duplicate MITM request content-length headers"
        ));
    }
    Ok(first)
}

fn upstream_request_target(target: &str) -> String {
    for scheme in ["https://", "http://"] {
        if let Some(rest) = target.strip_prefix(scheme) {
            let (_, path) = split_authority_path(rest);
            return path;
        }
    }
    target.to_string()
}

fn authority_for_host_header(host: &str, port: u16) -> String {
    if port == 443 {
        host.to_string()
    } else {
        format!("{host}:{port}")
    }
}

fn filter_upstream_request_headers(request: &ProxyRequest) -> Vec<(String, String)> {
    let connection_tokens = connection_header_tokens(&request.headers);
    request
        .headers
        .iter()
        .filter(|(name, _)| !is_hop_by_hop_request_header(name, &connection_tokens))
        .cloned()
        .collect()
}

fn filter_upstream_websocket_request_headers(request: &ProxyRequest) -> Vec<(String, String)> {
    request
        .headers
        .iter()
        .filter(|(name, _)| !is_skipped_upstream_websocket_request_header(name))
        .cloned()
        .collect()
}

fn filter_downstream_response_headers(response: &HttpResponse) -> Vec<(String, String)> {
    let connection_tokens = connection_header_tokens(&response.headers);
    response
        .headers
        .iter()
        .filter(|(name, _)| !is_hop_by_hop_response_header(name, &connection_tokens))
        .cloned()
        .collect()
}

fn filter_downstream_websocket_response_headers(response: &HttpResponse) -> Vec<(String, String)> {
    response
        .headers
        .iter()
        .filter(|(name, _)| !is_skipped_downstream_websocket_response_header(name))
        .cloned()
        .collect()
}

fn connection_header_tokens(headers: &[(String, String)]) -> HashSet<String> {
    headers
        .iter()
        .filter(|(name, _)| name == "connection")
        .flat_map(|(_, value)| value.split(','))
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| !value.is_empty())
        .collect()
}

fn is_hop_by_hop_request_header(name: &str, connection_tokens: &HashSet<String>) -> bool {
    connection_tokens.contains(name)
        || matches!(
            name,
            "connection"
                | "keep-alive"
                | "proxy-authenticate"
                | "proxy-authorization"
                | "proxy-connection"
                | "te"
                | "trailer"
                | "transfer-encoding"
                | "upgrade"
        )
}

fn is_skipped_upstream_websocket_request_header(name: &str) -> bool {
    matches!(
        name,
        "connection"
            | "upgrade"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "proxy-connection"
            | "te"
            | "trailer"
            | "transfer-encoding"
    )
}

fn is_hop_by_hop_response_header(name: &str, connection_tokens: &HashSet<String>) -> bool {
    connection_tokens.contains(name)
        || matches!(
            name,
            "connection"
                | "keep-alive"
                | "proxy-authenticate"
                | "proxy-authorization"
                | "proxy-connection"
                | "te"
                | "trailer"
                | "upgrade"
        )
}

fn is_skipped_downstream_websocket_response_header(name: &str) -> bool {
    matches!(
        name,
        "connection"
            | "upgrade"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "proxy-connection"
            | "te"
            | "trailer"
            | "transfer-encoding"
    )
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
        if target.starts_with('/') {
            return redact_path_query(target);
        }
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

#[derive(Debug)]
struct HttpResponse {
    version: String,
    status_code: u16,
    reason: String,
    headers: Vec<(String, String)>,
    header_bytes: usize,
}

#[derive(Debug)]
struct MitmForwardResult {
    request_status: &'static str,
    request_body_bytes: u64,
    request_error: Option<String>,
    response: Option<MitmResponseResult>,
    proxy_status: &'static str,
    proxy_error: Option<String>,
}

impl MitmForwardResult {
    fn rejected(status: &'static str, message: &str) -> Self {
        Self {
            request_status: status,
            request_body_bytes: 0,
            request_error: Some(message.to_string()),
            response: None,
            proxy_status: status,
            proxy_error: Some(message.to_string()),
        }
    }
}

#[derive(Debug)]
struct MitmResponseResult {
    status: &'static str,
    upstream_status: Option<u16>,
    response_header_bytes: Option<u64>,
    response_body_bytes: Option<u64>,
    websocket_tunnel_status: Option<&'static str>,
    error: Option<String>,
}

#[cfg(test)]
mod tests {
    use std::{fs, io::BufReader as StdBufReader, path::PathBuf};

    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        sync::{oneshot, watch},
        task::JoinHandle,
    };
    use tokio_rustls::{
        rustls::{pki_types::ServerName, ClientConfig, RootCertStore},
        TlsConnector,
    };

    use super::*;
    use crate::certs::init_ca;
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
    async fn allowlisted_connect_forwards_http1_request_to_tls_upstream_and_logs_safely() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let log_path = tempdir.path().join("events.jsonl");
        let mut writer = EventLogWriter::spawn(&log_path, true)
            .await
            .expect("event writer");
        let (upstream_addr, upstream_request_rx, upstream_task) = spawn_tls_upstream(
            b"HTTP/1.1 200 OK\r\ncontent-type: text/plain\r\ncontent-length: 5\r\nconnection: close\r\n\r\nhello".to_vec(),
        )
        .await;
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
        let addr = listener.local_addr().expect("local addr");
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let mut config = AppConfig::default();
        config.proxy.enabled = true;
        config.mitm.enabled = true;
        config.mitm.allow_hosts = vec!["localhost".to_string()];
        config.mitm.ca_cert_path = tempdir.path().join("certs/marsala-ca.pem");
        config.mitm.ca_key_path = tempdir.path().join("certs/marsala-ca-key.pem");
        init_ca(&config.mitm).expect("init ca");
        let ca_cert_path = config.mitm.ca_cert_path.clone();
        let task = tokio::spawn(serve_listener(
            listener,
            config,
            writer.handle(),
            shutdown_rx,
        ));

        let mut tls = connect_mitm_client(addr, upstream_addr, &ca_cert_path).await;
        tls.write_all(
            format!(
                "POST /backend-api/codex?api_key=query-secret HTTP/1.1\r\nhost: localhost:{}\r\nauthorization: Bearer direct-secret\r\nproxy-authorization: Basic inner-proxy-secret\r\ncookie: session=session-secret\r\nconnection: keep-alive, x-hop\r\nx-hop: remove-me\r\ncontent-length: 4\r\n\r\nping",
                upstream_addr.port()
            )
            .as_bytes(),
        )
        .await
        .expect("write http request");
        let mut response = Vec::new();
        tls.read_to_end(&mut response).await.expect("read response");
        let response = String::from_utf8_lossy(&response);
        assert!(response.contains("200 OK"));
        assert!(response.ends_with("hello"));

        let upstream_request = upstream_request_rx.await.expect("upstream request");
        upstream_task.await.expect("upstream task");
        assert_eq!(upstream_request.request.method, "POST");
        assert_eq!(
            upstream_request.request.target,
            "/backend-api/codex?api_key=query-secret"
        );
        assert_eq!(upstream_request.body, b"ping");
        assert_eq!(
            header_value(&upstream_request.request.headers, "authorization"),
            Some("Bearer direct-secret".to_string())
        );
        assert!(header_value(&upstream_request.request.headers, "proxy-authorization").is_none());
        assert!(header_value(&upstream_request.request.headers, "x-hop").is_none());
        assert_eq!(
            header_value(&upstream_request.request.headers, "connection"),
            Some("close".to_string())
        );

        shutdown_tx.send(true).expect("shutdown");
        task.await.expect("task").expect("serve listener");
        writer.shutdown().await.expect("writer shutdown");

        let log_text = fs::read_to_string(&log_path).expect("read log");
        assert!(!log_text.contains("proxy-secret"));
        assert!(!log_text.contains("inner-proxy-secret"));
        assert!(!log_text.contains("direct-secret"));
        assert!(!log_text.contains("session-secret"));
        assert!(!log_text.contains("query-secret"));
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
        assert_eq!(event["data"]["target_host"], "localhost");
        assert_eq!(event["data"]["target_port"], upstream_addr.port());
        assert_eq!(event["data"]["status"], "forwarded");
        assert_eq!(event["data"]["connect_action"], "mitm");
        assert_eq!(
            event["data"]["auth_shape"]["proxy_authorization_present"],
            false
        );

        let tls_event = records
            .iter()
            .find(|record| record["event_type"] == "mitm_tls")
            .expect("mitm tls event");
        assert_eq!(tls_event["data"]["target_host"], "localhost");
        assert_eq!(tls_event["data"]["target_port"], upstream_addr.port());
        assert_eq!(tls_event["data"]["status"], "handshake_ok");

        let request_event = records
            .iter()
            .find(|record| record["event_type"] == "mitm_request")
            .expect("mitm request event");
        assert_eq!(request_event["data"]["target_host"], "localhost");
        assert_eq!(request_event["data"]["target_port"], upstream_addr.port());
        assert_eq!(request_event["data"]["method"], "POST");
        assert_eq!(
            request_event["data"]["path"],
            "/backend-api/codex?api_key=[redacted]"
        );
        assert_eq!(request_event["data"]["status"], "forwarded");
        assert_eq!(request_event["data"]["request_body_bytes"], 4);
        assert_eq!(
            request_event["data"]["auth_shape"]["authorization_present"],
            true
        );
        assert_eq!(request_event["data"]["auth_shape"]["cookie_present"], true);

        let response_event = records
            .iter()
            .find(|record| record["event_type"] == "mitm_response")
            .expect("mitm response event");
        assert_eq!(response_event["data"]["target_host"], "localhost");
        assert_eq!(response_event["data"]["target_port"], upstream_addr.port());
        assert_eq!(response_event["data"]["method"], "POST");
        assert_eq!(
            response_event["data"]["path"],
            "/backend-api/codex?api_key=[redacted]"
        );
        assert_eq!(response_event["data"]["status"], "forwarded");
        assert_eq!(response_event["data"]["upstream_status"], 200);
        assert_eq!(response_event["data"]["response_body_bytes"], 5);
    }

    #[tokio::test]
    async fn allowlisted_connect_passes_through_upstream_5xx_response() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let log_path = tempdir.path().join("events.jsonl");
        let mut writer = EventLogWriter::spawn(&log_path, true)
            .await
            .expect("event writer");
        let (upstream_addr, upstream_request_rx, upstream_task) = spawn_tls_upstream(
            b"HTTP/1.1 503 Service Unavailable\r\ncontent-length: 4\r\nconnection: close\r\n\r\nnope".to_vec(),
        )
        .await;
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
        let addr = listener.local_addr().expect("local addr");
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let mut config = AppConfig::default();
        config.proxy.enabled = true;
        config.mitm.enabled = true;
        config.mitm.allow_hosts = vec!["localhost".to_string()];
        config.mitm.ca_cert_path = tempdir.path().join("certs/marsala-ca.pem");
        config.mitm.ca_key_path = tempdir.path().join("certs/marsala-ca-key.pem");
        init_ca(&config.mitm).expect("init ca");
        let ca_cert_path = config.mitm.ca_cert_path.clone();
        let task = tokio::spawn(serve_listener(
            listener,
            config,
            writer.handle(),
            shutdown_rx,
        ));

        let mut tls = connect_mitm_client(addr, upstream_addr, &ca_cert_path).await;
        tls.write_all(
            format!(
                "GET /failure HTTP/1.1\r\nhost: localhost:{}\r\n\r\n",
                upstream_addr.port()
            )
            .as_bytes(),
        )
        .await
        .expect("write http request");
        let mut response = Vec::new();
        tls.read_to_end(&mut response).await.expect("read response");
        let response = String::from_utf8_lossy(&response);
        assert!(response.contains("503 Service Unavailable"));
        assert!(response.ends_with("nope"));

        let _ = upstream_request_rx.await.expect("upstream request");
        upstream_task.await.expect("upstream task");
        shutdown_tx.send(true).expect("shutdown");
        task.await.expect("task").expect("serve listener");
        writer.shutdown().await.expect("writer shutdown");

        let records: Vec<Value> = read_tail_lines(&log_path, 20)
            .expect("read lines")
            .into_iter()
            .map(|line| serde_json::from_str(&line).expect("json"))
            .collect();
        let response_event = records
            .iter()
            .find(|record| record["event_type"] == "mitm_response")
            .expect("mitm response event");
        assert_eq!(response_event["data"]["status"], "forwarded");
        assert_eq!(response_event["data"]["upstream_status"], 503);
        assert_eq!(response_event["data"]["response_body_bytes"], 4);
    }

    #[tokio::test]
    async fn non_allowlisted_connect_tunnel_remains_uninspected() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let log_path = tempdir.path().join("events.jsonl");
        let mut writer = EventLogWriter::spawn(&log_path, true)
            .await
            .expect("event writer");
        let upstream = TcpListener::bind("127.0.0.1:0").await.expect("upstream");
        let upstream_addr = upstream.local_addr().expect("upstream addr");
        let upstream_task = tokio::spawn(async move {
            let (mut stream, _) = upstream.accept().await.expect("accept upstream");
            let mut request = vec![0u8; 11];
            stream.read_exact(&mut request).await.expect("read tunnel");
            assert_eq!(request, b"raw-through");
            stream
                .write_all(b"echo:raw-through")
                .await
                .expect("write echo");
        });
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
        let addr = listener.local_addr().expect("local addr");
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let mut config = AppConfig::default();
        config.proxy.enabled = true;
        config.mitm.enabled = true;
        config.mitm.allow_hosts = vec!["chatgpt.com".to_string()];
        config.mitm.ca_cert_path = tempdir.path().join("certs/marsala-ca.pem");
        config.mitm.ca_key_path = tempdir.path().join("certs/marsala-ca-key.pem");
        init_ca(&config.mitm).expect("init ca");
        let task = tokio::spawn(serve_listener(
            listener,
            config,
            writer.handle(),
            shutdown_rx,
        ));

        let mut client = TcpStream::connect(addr).await.expect("connect proxy");
        client
            .write_all(
                format!(
                    "CONNECT localhost:{} HTTP/1.1\r\nhost: localhost:{}\r\n\r\n",
                    upstream_addr.port(),
                    upstream_addr.port()
                )
                .as_bytes(),
            )
            .await
            .expect("write connect");
        read_connect_response(&mut client).await;
        client.write_all(b"raw-through").await.expect("write raw");
        let mut response = vec![0u8; 16];
        client.read_exact(&mut response).await.expect("read echo");
        assert_eq!(response, b"echo:raw-through");
        let _ = client.shutdown().await;

        upstream_task.await.expect("upstream task");
        shutdown_tx.send(true).expect("shutdown");
        task.await.expect("task").expect("serve listener");
        writer.shutdown().await.expect("writer shutdown");

        let records: Vec<Value> = read_tail_lines(&log_path, 20)
            .expect("read lines")
            .into_iter()
            .map(|line| serde_json::from_str(&line).expect("json"))
            .collect();
        let event = records
            .iter()
            .find(|record| record["event_type"] == "proxy_request")
            .expect("proxy request event");
        assert_eq!(event["data"]["connect_action"], "tunnel");
        assert!(records
            .iter()
            .all(|record| record["event_type"] != "mitm_tls"
                && record["event_type"] != "mitm_request"
                && record["event_type"] != "mitm_response"));
    }

    #[tokio::test]
    async fn allowlisted_connect_logs_http2_preface_as_unsupported() {
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
        config.mitm.allow_hosts = vec!["localhost".to_string()];
        config.mitm.ca_cert_path = tempdir.path().join("certs/marsala-ca.pem");
        config.mitm.ca_key_path = tempdir.path().join("certs/marsala-ca-key.pem");
        init_ca(&config.mitm).expect("init ca");
        let ca_cert_path = config.mitm.ca_cert_path.clone();
        let task = tokio::spawn(serve_listener(
            listener,
            config,
            writer.handle(),
            shutdown_rx,
        ));

        let mut tls = connect_mitm_client_to_target(addr, "localhost:443", &ca_cert_path).await;
        tls.write_all(b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n")
            .await
            .expect("write h2 preface");
        let mut response = Vec::new();
        tls.read_to_end(&mut response).await.expect("read response");
        let response = String::from_utf8_lossy(&response);
        assert!(response.contains("505 HTTP Version Not Supported"));
        assert!(response.contains("mitm_http2_unsupported"));

        shutdown_tx.send(true).expect("shutdown");
        task.await.expect("task").expect("serve listener");
        writer.shutdown().await.expect("writer shutdown");

        let records: Vec<Value> = read_tail_lines(&log_path, 20)
            .expect("read lines")
            .into_iter()
            .map(|line| serde_json::from_str(&line).expect("json"))
            .collect();
        let request_event = records
            .iter()
            .find(|record| record["event_type"] == "mitm_request")
            .expect("mitm request event");
        assert_eq!(request_event["data"]["status"], "http2_unsupported");
        assert_eq!(request_event["data"]["method"], "PRI");
        assert_eq!(request_event["data"]["http_version"], "HTTP/2.0");
    }

    #[tokio::test]
    async fn allowlisted_connect_rejects_absolute_form_authority_mismatch() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let fixture = spawn_allowlisted_mitm_proxy(&tempdir).await;

        let mut tls =
            connect_mitm_client_to_target(fixture.addr, "localhost:443", &fixture.ca_cert_path)
                .await;
        tls.write_all(
            b"GET https://example.com/backend-api/codex?api_key=query-secret HTTP/1.1\r\nhost: localhost\r\n\r\n",
        )
        .await
        .expect("write http request");
        let mut response = Vec::new();
        tls.read_to_end(&mut response).await.expect("read response");
        let response = String::from_utf8_lossy(&response);
        assert!(response.contains("400 Bad Request"));
        assert!(response.contains("mitm_authority_mismatch"));

        let log_path = fixture.log_path.clone();
        fixture.shutdown().await;
        let log_text = fs::read_to_string(&log_path).expect("read log");
        assert!(!log_text.contains("query-secret"));
        let records = read_json_records(&log_path);
        let request_event = records
            .iter()
            .find(|record| record["event_type"] == "mitm_request")
            .expect("mitm request event");
        assert_eq!(request_event["data"]["status"], "authority_mismatch");
        assert_eq!(
            request_event["data"]["error"],
            "absolute-form request authority does not match CONNECT target"
        );
        assert!(records
            .iter()
            .all(|record| record["event_type"] != "mitm_response"));
    }

    #[tokio::test]
    async fn allowlisted_connect_rejects_host_authority_mismatch() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let fixture = spawn_allowlisted_mitm_proxy(&tempdir).await;

        let mut tls =
            connect_mitm_client_to_target(fixture.addr, "localhost:443", &fixture.ca_cert_path)
                .await;
        tls.write_all(b"GET /backend-api/codex HTTP/1.1\r\nhost: example.com\r\n\r\n")
            .await
            .expect("write http request");
        let mut response = Vec::new();
        tls.read_to_end(&mut response).await.expect("read response");
        let response = String::from_utf8_lossy(&response);
        assert!(response.contains("400 Bad Request"));
        assert!(response.contains("mitm_authority_mismatch"));

        let log_path = fixture.log_path.clone();
        fixture.shutdown().await;
        let records = read_json_records(&log_path);
        let request_event = records
            .iter()
            .find(|record| record["event_type"] == "mitm_request")
            .expect("mitm request event");
        assert_eq!(request_event["data"]["status"], "authority_mismatch");
        assert_eq!(
            request_event["data"]["error"],
            "Host header authority does not match CONNECT target"
        );
        assert!(records
            .iter()
            .all(|record| record["event_type"] != "mitm_response"));
    }

    #[tokio::test]
    async fn allowlisted_connect_rejects_origin_form_missing_host() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let fixture = spawn_allowlisted_mitm_proxy(&tempdir).await;

        let mut tls =
            connect_mitm_client_to_target(fixture.addr, "localhost:443", &fixture.ca_cert_path)
                .await;
        tls.write_all(b"GET /backend-api/codex HTTP/1.1\r\n\r\n")
            .await
            .expect("write http request");
        let mut response = Vec::new();
        tls.read_to_end(&mut response).await.expect("read response");
        let response = String::from_utf8_lossy(&response);
        assert!(response.contains("400 Bad Request"));
        assert!(response.contains("mitm_authority_mismatch"));

        let log_path = fixture.log_path.clone();
        fixture.shutdown().await;
        let records = read_json_records(&log_path);
        let request_event = records
            .iter()
            .find(|record| record["event_type"] == "mitm_request")
            .expect("mitm request event");
        assert_eq!(request_event["data"]["status"], "authority_mismatch");
        assert_eq!(
            request_event["data"]["error"],
            "HTTP/1.1 origin-form request is missing Host header"
        );
        assert!(records
            .iter()
            .all(|record| record["event_type"] != "mitm_response"));
    }

    #[tokio::test]
    async fn allowlisted_connect_rejects_duplicate_content_length() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let fixture = spawn_allowlisted_mitm_proxy(&tempdir).await;

        let mut tls =
            connect_mitm_client_to_target(fixture.addr, "localhost:443", &fixture.ca_cert_path)
                .await;
        tls.write_all(
            b"POST /body HTTP/1.1\r\nhost: localhost\r\ncontent-length: 4\r\ncontent-length: 4\r\n\r\nping",
        )
        .await
        .expect("write http request");
        let mut response = Vec::new();
        tls.read_to_end(&mut response).await.expect("read response");
        let response = String::from_utf8_lossy(&response);
        assert!(response.contains("400 Bad Request"));
        assert!(response.contains("mitm_invalid_content_length"));

        let log_path = fixture.log_path.clone();
        fixture.shutdown().await;
        let records = read_json_records(&log_path);
        let request_event = records
            .iter()
            .find(|record| record["event_type"] == "mitm_request")
            .expect("mitm request event");
        assert_eq!(request_event["data"]["status"], "invalid_content_length");
        assert_eq!(
            request_event["data"]["error"],
            "duplicate MITM request content-length headers"
        );
    }

    #[tokio::test]
    async fn allowlisted_connect_rejects_conflicting_content_length() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let fixture = spawn_allowlisted_mitm_proxy(&tempdir).await;

        let mut tls =
            connect_mitm_client_to_target(fixture.addr, "localhost:443", &fixture.ca_cert_path)
                .await;
        tls.write_all(
            b"POST /body HTTP/1.1\r\nhost: localhost\r\ncontent-length: 4\r\ncontent-length: 5\r\n\r\nping",
        )
        .await
        .expect("write http request");
        let mut response = Vec::new();
        tls.read_to_end(&mut response).await.expect("read response");
        let response = String::from_utf8_lossy(&response);
        assert!(response.contains("400 Bad Request"));
        assert!(response.contains("mitm_invalid_content_length"));

        let log_path = fixture.log_path.clone();
        fixture.shutdown().await;
        let records = read_json_records(&log_path);
        let request_event = records
            .iter()
            .find(|record| record["event_type"] == "mitm_request")
            .expect("mitm request event");
        assert_eq!(request_event["data"]["status"], "invalid_content_length");
        assert_eq!(
            request_event["data"]["error"],
            "conflicting MITM request content-length headers"
        );
    }

    #[tokio::test]
    async fn allowlisted_connect_forwards_websocket_101_tunnel_and_logs_safely() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let fixture = spawn_allowlisted_mitm_proxy(&tempdir).await;
        let client_frame = b"client-frame-secret".to_vec();
        let upstream_frame = b"upstream-frame-secret".to_vec();
        let (upstream_addr, upstream_request_rx, upstream_task) =
            spawn_tls_websocket_upstream_101(client_frame.clone(), upstream_frame.clone()).await;

        let mut tls = connect_mitm_client(fixture.addr, upstream_addr, &fixture.ca_cert_path).await;
        tls.write_all(
            format!(
                "GET /backend-api/codex/responses?api_key=query-secret HTTP/1.1\r\nhost: localhost:{}\r\nauthorization: Bearer websocket-secret\r\ncookie: session=websocket-session-secret\r\nproxy-authorization: Basic proxy-secret\r\nconnection: keep-alive, Upgrade\r\nupgrade: websocket\r\nsec-websocket-key: test-key\r\nsec-websocket-version: 13\r\nsec-websocket-protocol: codex\r\n\r\n",
                upstream_addr.port()
            )
            .as_bytes(),
        )
        .await
        .expect("write websocket request");
        let response_head = read_http_response_head(&mut tls).await;
        assert!(response_head.contains("101 Switching Protocols"));
        assert!(response_head
            .to_ascii_lowercase()
            .contains("upgrade: websocket"));
        assert!(response_head
            .to_ascii_lowercase()
            .contains("connection: upgrade"));

        tls.write_all(&client_frame)
            .await
            .expect("write websocket frame bytes");
        let mut received = vec![0u8; upstream_frame.len()];
        tls.read_exact(&mut received)
            .await
            .expect("read websocket frame bytes");
        assert_eq!(received, upstream_frame);
        let _ = tls.shutdown().await;

        let upstream_request = upstream_request_rx.await.expect("upstream request");
        upstream_task.await.expect("upstream task");
        assert_eq!(upstream_request.request.method, "GET");
        assert_eq!(
            upstream_request.request.target,
            "/backend-api/codex/responses?api_key=query-secret"
        );
        assert_eq!(
            header_value(&upstream_request.request.headers, "authorization"),
            Some("Bearer websocket-secret".to_string())
        );
        assert_eq!(
            header_value(&upstream_request.request.headers, "cookie"),
            Some("session=websocket-session-secret".to_string())
        );
        assert_eq!(
            header_value(&upstream_request.request.headers, "connection"),
            Some("Upgrade".to_string())
        );
        assert_eq!(
            header_value(&upstream_request.request.headers, "upgrade"),
            Some("websocket".to_string())
        );
        assert_eq!(
            header_value(&upstream_request.request.headers, "sec-websocket-key"),
            Some("test-key".to_string())
        );
        assert!(header_value(&upstream_request.request.headers, "proxy-authorization").is_none());

        let log_path = fixture.log_path.clone();
        fixture.shutdown().await;
        let log_text = fs::read_to_string(&log_path).expect("read log");
        assert!(!log_text.contains("websocket-secret"));
        assert!(!log_text.contains("websocket-session-secret"));
        assert!(!log_text.contains("proxy-secret"));
        assert!(!log_text.contains("query-secret"));
        assert!(!log_text.contains("client-frame-secret"));
        assert!(!log_text.contains("upstream-frame-secret"));
        let records = read_json_records(&log_path);
        let request_event = records
            .iter()
            .find(|record| record["event_type"] == "mitm_request")
            .expect("mitm request event");
        assert_eq!(request_event["data"]["status"], "websocket_forwarded");
        assert_eq!(
            request_event["data"]["path"],
            "/backend-api/codex/responses?api_key=[redacted]"
        );
        assert_eq!(
            request_event["data"]["auth_shape"]["authorization_present"],
            true
        );
        assert_eq!(request_event["data"]["auth_shape"]["cookie_present"], true);

        let response_event = records
            .iter()
            .find(|record| record["event_type"] == "mitm_response")
            .expect("mitm response event");
        assert_eq!(response_event["data"]["upstream_status"], 101);
        assert_eq!(response_event["data"]["status"], "websocket_tunnel_closed");
        assert_eq!(
            response_event["data"]["websocket_tunnel_status"],
            "websocket_tunnel_closed"
        );

        let proxy_event = records
            .iter()
            .find(|record| record["event_type"] == "proxy_request")
            .expect("proxy request event");
        assert_eq!(proxy_event["data"]["status"], "websocket_tunnel_closed");
    }

    #[tokio::test]
    async fn allowlisted_connect_passes_through_websocket_non_101_response() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let fixture = spawn_allowlisted_mitm_proxy(&tempdir).await;
        let (upstream_addr, upstream_request_rx, upstream_task) = spawn_tls_upstream(
            b"HTTP/1.1 403 Forbidden\r\ncontent-length: 9\r\nconnection: close\r\n\r\nforbidden"
                .to_vec(),
        )
        .await;

        let mut tls = connect_mitm_client(fixture.addr, upstream_addr, &fixture.ca_cert_path).await;
        tls.write_all(
            format!(
                "GET /socket HTTP/1.1\r\nhost: localhost:{}\r\nconnection: Upgrade\r\nupgrade: websocket\r\nsec-websocket-key: test-key\r\nsec-websocket-version: 13\r\n\r\n",
                upstream_addr.port()
            )
            .as_bytes(),
        )
        .await
        .expect("write websocket request");
        let mut response = Vec::new();
        tls.read_to_end(&mut response).await.expect("read response");
        let response = String::from_utf8_lossy(&response);
        assert!(response.contains("403 Forbidden"));
        assert!(response.ends_with("forbidden"));

        let _ = upstream_request_rx.await.expect("upstream request");
        upstream_task.await.expect("upstream task");
        let log_path = fixture.log_path.clone();
        fixture.shutdown().await;
        let records = read_json_records(&log_path);
        let response_event = records
            .iter()
            .find(|record| record["event_type"] == "mitm_response")
            .expect("mitm response event");
        assert_eq!(
            response_event["data"]["status"],
            "websocket_non_101_response"
        );
        assert_eq!(response_event["data"]["upstream_status"], 403);
        assert_eq!(response_event["data"]["response_body_bytes"], 9);
    }

    #[tokio::test]
    async fn allowlisted_connect_rejects_websocket_authority_mismatch() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let fixture = spawn_allowlisted_mitm_proxy(&tempdir).await;

        let mut tls =
            connect_mitm_client_to_target(fixture.addr, "localhost:443", &fixture.ca_cert_path)
                .await;
        tls.write_all(
            b"GET /socket HTTP/1.1\r\nhost: example.com\r\nconnection: Upgrade\r\nupgrade: websocket\r\nsec-websocket-key: test-key\r\nsec-websocket-version: 13\r\n\r\n",
        )
        .await
        .expect("write websocket request");
        let mut response = Vec::new();
        tls.read_to_end(&mut response).await.expect("read response");
        let response = String::from_utf8_lossy(&response);
        assert!(response.contains("400 Bad Request"));
        assert!(response.contains("mitm_authority_mismatch"));

        let log_path = fixture.log_path.clone();
        fixture.shutdown().await;
        let records = read_json_records(&log_path);
        let request_event = records
            .iter()
            .find(|record| record["event_type"] == "mitm_request")
            .expect("mitm request event");
        assert_eq!(request_event["data"]["status"], "authority_mismatch");
        assert!(records
            .iter()
            .all(|record| record["event_type"] != "mitm_response"));
    }

    #[tokio::test]
    async fn allowlisted_connect_rejects_malformed_websocket_upgrade() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let fixture = spawn_allowlisted_mitm_proxy(&tempdir).await;

        let mut tls =
            connect_mitm_client_to_target(fixture.addr, "localhost:443", &fixture.ca_cert_path)
                .await;
        tls.write_all(
            b"GET /socket HTTP/1.1\r\nhost: localhost\r\nconnection: keep-alive, Upgrade\r\nupgrade: websocket\r\n\r\n",
        )
        .await
        .expect("write http request");
        let mut response = Vec::new();
        tls.read_to_end(&mut response).await.expect("read response");
        let response = String::from_utf8_lossy(&response);
        assert!(response.contains("400 Bad Request"));
        assert!(response.contains("mitm_websocket_malformed"));

        let log_path = fixture.log_path.clone();
        fixture.shutdown().await;
        let records = read_json_records(&log_path);
        let request_event = records
            .iter()
            .find(|record| record["event_type"] == "mitm_request")
            .expect("mitm request event");
        assert_eq!(request_event["data"]["status"], "websocket_malformed");
        assert_eq!(
            request_event["data"]["error"],
            "WebSocket upgrade request is missing Sec-WebSocket-Key"
        );
    }

    #[tokio::test]
    async fn allowlisted_connect_times_out_streaming_upstream_response_body() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let fixture = spawn_allowlisted_mitm_proxy(&tempdir).await;
        let (upstream_addr, upstream_task) = spawn_tls_upstream_hanging_response(
            b"HTTP/1.1 200 OK\r\ncontent-type: text/plain\r\ncontent-length: 5\r\nconnection: close\r\n\r\n".to_vec(),
        )
        .await;

        let mut tls = connect_mitm_client(fixture.addr, upstream_addr, &fixture.ca_cert_path).await;
        tls.write_all(
            format!(
                "GET /slow HTTP/1.1\r\nhost: localhost:{}\r\n\r\n",
                upstream_addr.port()
            )
            .as_bytes(),
        )
        .await
        .expect("write http request");
        let mut response = Vec::new();
        tls.read_to_end(&mut response).await.expect("read response");
        let response = String::from_utf8_lossy(&response);
        assert!(response.contains("200 OK"));

        upstream_task.abort();
        let _ = upstream_task.await;
        let log_path = fixture.log_path.clone();
        fixture.shutdown().await;
        let records = read_json_records(&log_path);
        let response_event = records
            .iter()
            .find(|record| record["event_type"] == "mitm_response")
            .expect("mitm response event");
        assert_eq!(response_event["data"]["status"], "response_body_timeout");
        assert_eq!(response_event["data"]["upstream_status"], 200);
        assert_eq!(
            response_event["data"]["error"],
            "timed out while streaming MITM upstream response body"
        );
    }

    struct RunningMitmProxy {
        addr: SocketAddr,
        ca_cert_path: PathBuf,
        log_path: PathBuf,
        shutdown_tx: watch::Sender<bool>,
        task: JoinHandle<Result<()>>,
        writer: EventLogWriter,
    }

    impl RunningMitmProxy {
        async fn shutdown(mut self) {
            self.shutdown_tx.send(true).expect("shutdown");
            self.task.await.expect("task").expect("serve listener");
            self.writer.shutdown().await.expect("writer shutdown");
        }
    }

    async fn spawn_allowlisted_mitm_proxy(tempdir: &tempfile::TempDir) -> RunningMitmProxy {
        let log_path = tempdir.path().join("events.jsonl");
        let writer = EventLogWriter::spawn(&log_path, true)
            .await
            .expect("event writer");
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
        let addr = listener.local_addr().expect("local addr");
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let mut config = AppConfig::default();
        config.proxy.enabled = true;
        config.mitm.enabled = true;
        config.mitm.allow_hosts = vec!["localhost".to_string()];
        config.mitm.ca_cert_path = tempdir.path().join("certs/marsala-ca.pem");
        config.mitm.ca_key_path = tempdir.path().join("certs/marsala-ca-key.pem");
        init_ca(&config.mitm).expect("init ca");
        let ca_cert_path = config.mitm.ca_cert_path.clone();
        let task = tokio::spawn(serve_listener(
            listener,
            config,
            writer.handle(),
            shutdown_rx,
        ));

        RunningMitmProxy {
            addr,
            ca_cert_path,
            log_path,
            shutdown_tx,
            task,
            writer,
        }
    }

    fn read_json_records(log_path: &std::path::Path) -> Vec<Value> {
        read_tail_lines(log_path, 50)
            .expect("read lines")
            .into_iter()
            .map(|line| serde_json::from_str(&line).expect("json"))
            .collect()
    }

    struct CapturedUpstreamRequest {
        request: ProxyRequest,
        body: Vec<u8>,
    }

    async fn spawn_tls_upstream(
        response: Vec<u8>,
    ) -> (
        SocketAddr,
        oneshot::Receiver<CapturedUpstreamRequest>,
        tokio::task::JoinHandle<()>,
    ) {
        let (server_config, root_cert) = test_upstream_server_config();
        trust_test_upstream_root(root_cert);
        let acceptor = TlsAcceptor::from(Arc::new(server_config));
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("upstream listener");
        let addr = listener.local_addr().expect("upstream local addr");
        let (request_tx, request_rx) = oneshot::channel();
        let task = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("upstream accept");
            let mut tls = acceptor.accept(stream).await.expect("upstream tls accept");
            let mut reader = BufReader::new(&mut tls);
            let request = read_proxy_request(&mut reader)
                .await
                .expect("read upstream request");
            let body_len = request_content_length(&request).expect("request content-length");
            let mut body = vec![0u8; body_len];
            if body_len > 0 {
                reader
                    .read_exact(&mut body)
                    .await
                    .expect("read upstream request body");
            }
            let _ = request_tx.send(CapturedUpstreamRequest { request, body });
            reader
                .get_mut()
                .write_all(&response)
                .await
                .expect("write upstream response");
            let _ = reader.get_mut().shutdown().await;
        });
        (addr, request_rx, task)
    }

    async fn spawn_tls_websocket_upstream_101(
        expected_client_bytes: Vec<u8>,
        upstream_bytes: Vec<u8>,
    ) -> (
        SocketAddr,
        oneshot::Receiver<CapturedUpstreamRequest>,
        tokio::task::JoinHandle<()>,
    ) {
        let (server_config, root_cert) = test_upstream_server_config();
        trust_test_upstream_root(root_cert);
        let acceptor = TlsAcceptor::from(Arc::new(server_config));
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("upstream listener");
        let addr = listener.local_addr().expect("upstream local addr");
        let (request_tx, request_rx) = oneshot::channel();
        let task = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("upstream accept");
            let mut tls = acceptor.accept(stream).await.expect("upstream tls accept");
            let mut reader = BufReader::new(&mut tls);
            let request = read_proxy_request(&mut reader)
                .await
                .expect("read upstream request");
            let _ = request_tx.send(CapturedUpstreamRequest {
                request,
                body: Vec::new(),
            });
            reader
                .get_mut()
                .write_all(
                    b"HTTP/1.1 101 Switching Protocols\r\nupgrade: websocket\r\nconnection: Upgrade\r\nsec-websocket-accept: accept-token\r\n\r\n",
                )
                .await
                .expect("write websocket response");
            let mut received = vec![0u8; expected_client_bytes.len()];
            reader
                .read_exact(&mut received)
                .await
                .expect("read websocket client bytes");
            assert_eq!(received, expected_client_bytes);
            reader
                .get_mut()
                .write_all(&upstream_bytes)
                .await
                .expect("write websocket upstream bytes");
            let _ = reader.get_mut().shutdown().await;
        });
        (addr, request_rx, task)
    }

    async fn spawn_tls_upstream_hanging_response(
        response_head: Vec<u8>,
    ) -> (SocketAddr, JoinHandle<()>) {
        let (server_config, root_cert) = test_upstream_server_config();
        trust_test_upstream_root(root_cert);
        let acceptor = TlsAcceptor::from(Arc::new(server_config));
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("upstream listener");
        let addr = listener.local_addr().expect("upstream local addr");
        let task = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("upstream accept");
            let mut tls = acceptor.accept(stream).await.expect("upstream tls accept");
            let mut reader = BufReader::new(&mut tls);
            let request = read_proxy_request(&mut reader)
                .await
                .expect("read upstream request");
            let body_len = request_content_length(&request).expect("request content-length");
            let mut body = vec![0u8; body_len];
            if body_len > 0 {
                reader
                    .read_exact(&mut body)
                    .await
                    .expect("read upstream request body");
            }
            reader
                .get_mut()
                .write_all(&response_head)
                .await
                .expect("write upstream response head");
            std::future::pending::<()>().await;
        });
        (addr, task)
    }

    fn test_upstream_server_config() -> (ServerConfig, CertificateDer<'static>) {
        ensure_rustls_crypto_provider();

        let ca_key = KeyPair::generate().expect("generate upstream ca key");
        let mut ca_params = CertificateParams::default();
        let mut ca_name = DistinguishedName::new();
        ca_name.push(DnType::CommonName, "Marsala Test Upstream CA");
        ca_params.distinguished_name = ca_name;
        ca_params.is_ca = IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        ca_params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
        let ca_cert = ca_params.self_signed(&ca_key).expect("upstream ca cert");
        let root_cert = CertificateDer::from(ca_cert.der().to_vec());
        let issuer = Issuer::from_ca_cert_pem(&ca_cert.pem(), ca_key).expect("upstream issuer");

        let leaf_key = KeyPair::generate().expect("generate upstream leaf key");
        let mut leaf_params =
            CertificateParams::new(vec!["localhost".to_string()]).expect("leaf params");
        let mut leaf_name = DistinguishedName::new();
        leaf_name.push(DnType::CommonName, "localhost");
        leaf_params.distinguished_name = leaf_name;
        leaf_params.is_ca = IsCa::NoCa;
        leaf_params.key_usages = vec![
            KeyUsagePurpose::DigitalSignature,
            KeyUsagePurpose::KeyEncipherment,
        ];
        leaf_params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        let leaf_cert = leaf_params
            .signed_by(&leaf_key, &issuer)
            .expect("upstream leaf cert");
        let cert_chain = vec![CertificateDer::from(leaf_cert.der().to_vec())];
        let private_key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(leaf_key.serialize_der()));
        let mut server_config = ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(cert_chain, private_key)
            .expect("upstream server config");
        server_config.alpn_protocols = vec![b"http/1.1".to_vec()];
        (server_config, root_cert)
    }

    async fn connect_mitm_client(
        proxy_addr: SocketAddr,
        upstream_addr: SocketAddr,
        ca_cert_path: &std::path::Path,
    ) -> tokio_rustls::client::TlsStream<TcpStream> {
        connect_mitm_client_to_target(
            proxy_addr,
            &format!("localhost:{}", upstream_addr.port()),
            ca_cert_path,
        )
        .await
    }

    async fn connect_mitm_client_to_target(
        proxy_addr: SocketAddr,
        target: &str,
        ca_cert_path: &std::path::Path,
    ) -> tokio_rustls::client::TlsStream<TcpStream> {
        let mut client = TcpStream::connect(proxy_addr).await.expect("connect proxy");
        client
            .write_all(format!("CONNECT {target} HTTP/1.1\r\nhost: {target}\r\n\r\n").as_bytes())
            .await
            .expect("write connect");
        read_connect_response(&mut client).await;

        let mut root_store = RootCertStore::empty();
        let ca_cert_pem = fs::read(ca_cert_path).expect("read ca cert");
        let ca_certs = rustls_pemfile::certs(&mut StdBufReader::new(ca_cert_pem.as_slice()))
            .collect::<Result<Vec<_>, _>>()
            .expect("parse ca certs");
        assert_eq!(ca_certs.len(), 1);
        root_store.add(ca_certs[0].clone()).expect("add root ca");
        let client_config = ClientConfig::builder()
            .with_root_certificates(root_store)
            .with_no_client_auth();
        let connector = TlsConnector::from(Arc::new(client_config));
        let server_name = ServerName::try_from("localhost").expect("server name");
        connector
            .connect(server_name, client)
            .await
            .expect("tls handshake")
    }

    async fn read_connect_response(client: &mut TcpStream) {
        let mut connect_response = Vec::new();
        let mut byte = [0u8; 1];
        while !connect_response.ends_with(b"\r\n\r\n") {
            client
                .read_exact(&mut byte)
                .await
                .expect("read connect response");
            connect_response.push(byte[0]);
        }
        let connect_response = String::from_utf8_lossy(&connect_response);
        assert!(connect_response.contains("200 Connection Established"));
    }

    async fn read_http_response_head<R>(stream: &mut R) -> String
    where
        R: AsyncRead + Unpin,
    {
        let mut response = Vec::new();
        let mut byte = [0u8; 1];
        while !response.ends_with(b"\r\n\r\n") {
            stream
                .read_exact(&mut byte)
                .await
                .expect("read response head");
            response.push(byte[0]);
        }
        String::from_utf8_lossy(&response).to_string()
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

    #[test]
    fn connection_upgrade_without_upgrade_websocket_is_not_websocket() {
        let request = ProxyRequest {
            method: "GET".to_string(),
            target: "/socket".to_string(),
            version: "HTTP/1.1".to_string(),
            headers: vec![("connection".to_string(), "upgrade".to_string())],
            header_bytes: 64,
        };

        assert!(!is_websocket_upgrade(&request));
    }
}
