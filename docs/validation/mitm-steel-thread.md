# MITM Steel Thread Validation

Status: HTTP/1.1 forwarding proof plus opt-in local payload inspection. Marsala can generate a local CA, validate exact-host MITM config, terminate TLS for allowlisted `CONNECT` targets, forward decrypted HTTP/1.1 requests upstream over verified TLS to the same host:port, forward allowlisted HTTP/1.1 WebSocket upgrades, tunnel bytes after upstream `101 Switching Protocols`, and emit sanitized `mitm_tls`, `mitm_request`, and `mitm_response` metadata. When explicitly enabled, it also emits bounded, redacted `mitm_payload` and `mitm_websocket_frame` preview events for local inspection.

Observed facts:

- Inbound-auth passthrough to public `api.openai.com/v1/responses` was transport-success/upstream-rejected with `401 Unauthorized`.
- Normal ChatGPT-backed Codex succeeds through Marsala's proxy when tunneled.
- That normal path currently emits `CONNECT` metadata for `chatgpt.com:443` and `ab.chatgpt.com:443`.
- `github.com:443` may appear during a run and must remain tunneled.
- Treat allowlisted MITM of `chatgpt.com` and `ab.chatgpt.com` as the main path for subscription-backed normal Codex traffic discovery.

## CA Generation

Generate the default local CA paths:

```bash
just mitm-ca-init
```

Expected today:

- Creates `certs/marsala-ca.pem`.
- Creates `certs/marsala-ca-key.pem`.
- Creates `certs/` with restrictive permissions where supported on Unix.
- Refuses to overwrite either file if it already exists.

The default `certs/` directory is gitignored and must not be committed.

## Current Config Validation

Print the full config shape:

```bash
cargo run -p marsala -- config print --all
```

Validate that MITM cannot be enabled without the proxy:

```bash
MARSALA__MITM__ENABLED=true \
MARSALA__MITM__ALLOW_HOSTS=api.openai.com \
  cargo run -p marsala -- config print --all
```

Expected today: config loading fails with `mitm.enabled=true requires proxy.enabled=true`.

Validate the intended allowlisted shape:

```bash
MARSALA__PROXY__ENABLED=true \
MARSALA__MITM__ENABLED=true \
MARSALA__MITM__ALLOW_HOSTS=chatgpt.com,ab.chatgpt.com \
MARSALA__MITM__CA_CERT_PATH=certs/marsala-ca.pem \
MARSALA__MITM__CA_KEY_PATH=certs/marsala-ca-key.pem \
  cargo run -p marsala -- config print --all
```

Expected today: config prints successfully.

Use `api.openai.com` as an optional allowlist host only when validating the API/custom-provider path.

Payload inspection is off by default. The exact local-only toggles are:

```bash
MARSALA__LOGGING__CAPTURE_MITM_PAYLOADS=true
MARSALA__LOGGING__CAPTURE_MITM_WEBSOCKET_FRAMES=true
MARSALA__LOGGING__MITM_PAYLOAD_PREVIEW_BYTES=4096
MARSALA__LOGGING__MITM_WEBSOCKET_FRAME_PREVIEW_BYTES=4096
```

The TOML equivalents are `logging.capture_mitm_payloads`, `logging.capture_mitm_websocket_frames`, `logging.mitm_payload_preview_bytes`, and `logging.mitm_websocket_frame_preview_bytes`. Keep capture disabled for routine validation. When enabled, treat `logs/events.jsonl` as sensitive local inspection output even though obvious bearer tokens, API-key/token/session query parameters, cookie-ish header lines, and obvious JSON secret fields are redacted.

## TLS Termination And HTTP/1.1 Forwarding Proof

Start Marsala with the proxy and MITM config accepted:

```bash
MARSALA__PROXY__ENABLED=true \
MARSALA__MITM__ENABLED=true \
MARSALA__MITM__ALLOW_HOSTS=chatgpt.com,ab.chatgpt.com \
MARSALA__MITM__CA_CERT_PATH=certs/marsala-ca.pem \
MARSALA__MITM__CA_KEY_PATH=certs/marsala-ca-key.pem \
  cargo run -p marsala -- serve
```

For an explicit payload-inspection run, add the capture toggles from the previous section to the same command.

In another terminal, run normal Codex through the proxy:

```bash
env HTTPS_PROXY=http://127.0.0.1:8788 https_proxy=http://127.0.0.1:8788 \
  HTTP_PROXY= http_proxy= ALL_PROXY= all_proxy= NO_PROXY= no_proxy= \
  SSL_CERT_FILE="$PWD/certs/marsala-ca.pem" \
  codex exec \
    --skip-git-repo-check \
    -c 'approval_policy="never"' \
    'Reply with one short sentence.'
```

Expected today:

- `proxy_request` entries with `method=CONNECT`, `target_host=chatgpt.com` or `target_host=ab.chatgpt.com`, and `target_port=443`.
- For allowlisted ChatGPT hosts, `connect_action=mitm`.
- `mitm_tls` with the same exact host and `status=handshake_ok`, or `status=error` if the client does not trust the generated CA.
- `mitm_request` with sanitized `method`, redacted `path`, `auth_shape`, and bounded request byte counts for decrypted HTTP/1.1 requests; WebSocket upgrades use `status=websocket_forwarded`.
- `mitm_response` with upstream status and response byte counts when forwarding reaches the upstream server and the response body finishes before the steel-thread timeout; WebSocket `101` responses include `upstream_status=101` and `websocket_tunnel_status`.
- With `logging.capture_mitm_payloads=true`, `mitm_payload` entries for HTTP request and response bodies include `direction`, redacted `path`, `body_bytes`, `preview_bytes`, `truncated`, `utf8`, and a redacted UTF-8 `preview` when the preview is valid UTF-8.
- With `logging.capture_mitm_websocket_frames=true`, `mitm_websocket_frame` entries for WebSocket text frames include direction and redacted previews; binary frames are counted with `skipped_payload=true` and no payload preview.
- The client receives the upstream HTTP/1.1 response when the request has no body or a `Content-Length` body up to 1 MiB and the response body copy does not exceed the current operation timeout.
- HTTP/1.1 WebSocket upgrades to the same CONNECT host:port are forwarded over verified upstream TLS; after upstream `101 Switching Protocols`, Marsala tunnels bytes bidirectionally. Frame inspection/logging occurs only when `logging.capture_mitm_websocket_frames=true`.
- HTTP/2, malformed upgrades, non-WebSocket upgrades, and request `Transfer-Encoding` streaming are logged as explicit unsupported or malformed MITM statuses.
- Stalled upstream response bodies are logged with `response_body_timeout`; this proof does not provide unbounded response streaming.
- Non-allowlisted HTTPS targets, including observed `github.com` traffic, still tunnel and do not produce `mitm_tls` or `mitm_request`.

## CA Fallback

If a client does not honor `CODEX_CA_CERTIFICATE`, test the generic CA variable separately:

```bash
env HTTPS_PROXY=http://127.0.0.1:8788 https_proxy=http://127.0.0.1:8788 \
  HTTP_PROXY= http_proxy= ALL_PROXY= all_proxy= NO_PROXY= no_proxy= \
  CODEX_CA_CERTIFICATE="$PWD/certs/marsala-ca.pem" \
  codex exec \
    --skip-git-repo-check \
    -c 'approval_policy="never"' \
    'Reply with one short sentence.'
```

## Remaining Full-MITM Gap

The current proof terminates downstream TLS and forwards HTTP/1.1 requests/responses and allowlisted HTTP/1.1 WebSocket upgrade tunnels. HTTP body and WebSocket text-frame previews are available only through explicit local-only toggles, are byte-capped, and are emitted as separate JSONL events. Response body forwarding is bounded by the current operation timeout.

Still missing:

- HTTP/2 forwarding
- WebSocket forwarding outside the exact allowlisted HTTP/1.1 upgrade steel thread
- request body streaming for chunked or otherwise unbounded bodies
- validation that a normal ChatGPT-backed Codex run stays on HTTP/1.1 and succeeds through the terminated TLS path

Safety invariant by default: `logs/events.jsonl` must contain no raw bearer token, cookie, request body, response body, stream chunk content, or WebSocket frame bytes. With payload capture enabled, `logs/events.jsonl` may contain redacted, bounded HTTP body and WebSocket text previews and should be handled as sensitive local data.

Do not claim full Codex MITM until a normal Codex run succeeds through the terminated TLS path.
