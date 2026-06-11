# Marsala

Marsala is a local-first interception workbench for Codex and other developer LLM tools. Its core job is simple: show what your AI tooling is actually sending and receiving, keep that evidence local, and create a controlled place to inspect and eventually shape those flows.

The product baseline is normal ChatGPT-backed Codex traffic, not the OpenAI-compatible gateway. Marsala runs as a local HTTP proxy, observes Codex `CONNECT` traffic, and can TLS-terminate exact allowlisted Codex hosts so the live UI and JSONL event log show sanitized request, response, and WebSocket metadata. When local payload capture is explicitly enabled, Marsala also records bounded, redacted previews of HTTP bodies and WebSocket text frames.

The compatibility gateway still exists because it is useful for fixtures and custom-provider experiments: Marsala exposes `POST /v1/chat/completions` and `POST /v1/responses`, including streaming passthrough for `POST /v1/responses` with `stream=true`. That path is not the main product proof. The mainline is allowlisted MITM for normal Codex.

Current steel thread:

- Run Marsala locally with the interception UI at `http://127.0.0.1:8787/ui`.
- Route Codex through Marsala's proxy on `127.0.0.1:8788`.
- MITM only exact allowlisted Codex hosts such as `chatgpt.com` and `ab.chatgpt.com`.
- Tunnel unrelated hosts, including observed `github.com` traffic.
- Capture payload previews only when explicitly enabled.
- Keep generated CA material and captured traffic local.

Still intentionally limited: Marsala does not implement stream chunk capture, HTTP/2 MITM forwarding, WebSocket forwarding outside the allowlisted HTTP/1.1 upgrade steel thread, unbounded request-body streaming, or unbounded MITM response streaming.

## Prerequisites

- Rust 1.96 or newer
- `cargo` from the same toolchain

This repo declares `rust-version = "1.96"` in `Cargo.toml` and pins `1.96.0` in `rust-toolchain.toml` for `rustup` users. If you manage toolchains manually, use `rustup toolchain install 1.96.0` and `rustup override set 1.96.0` or `rustup default 1.96.0`.

## Workspace layout

- `crates/marsala`: service crate
- `docs/planning`: product and implementation planning

## Local usage

```bash
cargo run -p marsala -- serve
cargo run -p marsala -- config print
cargo run -p marsala -- config print --all
cargo run -p marsala -- logs tail --lines 20
```

When Marsala is running, the live interception dashboard is served at `http://127.0.0.1:8787/ui`. It shows recent and live JSONL events from the configured event log with timeline filters, payload previews, and a detail pane.

First-run smoke path:

Terminal 1:

```bash
cargo run -p marsala -- serve
```

Terminal 2:

```bash
cargo run -p marsala -- config print
curl http://127.0.0.1:8787/healthz
curl http://127.0.0.1:8787/v1/chat/completions \
  -H 'content-type: application/json' \
  -d '{"model":"gpt-4.1-mini","messages":[{"role":"user","content":"Say hi"}]}'
curl http://127.0.0.1:8787/v1/responses \
  -H 'content-type: application/json' \
  -d '{"model":"gpt-5","input":"Say hi"}'
cargo run -p marsala -- logs tail --lines 20
```

Local defaults:

- bind: `127.0.0.1:8787`
- health check: `http://127.0.0.1:8787/healthz`
- chat completions: `http://127.0.0.1:8787/v1/chat/completions`
- responses: `http://127.0.0.1:8787/v1/responses`
- JSONL event log: `logs/events.jsonl`
- body logging: disabled unless `logging.log_bodies=true`
- MITM payload capture: disabled unless `logging.capture_mitm_payloads=true`
- MITM WebSocket frame capture: disabled unless `logging.capture_mitm_websocket_frames=true`

Config loading order:

1. built-in defaults
2. `marsala.toml` in the current working directory, if present
3. `--config /path/to/file.toml` or `MARSALA_CONFIG`
4. environment overrides with the `MARSALA__...` prefix

`MARSALA_CONFIG` only selects which TOML file to read. Field overrides come from `MARSALA__...`.

Example environment overrides:

```bash
export MARSALA__SERVER__PORT=9797
export MARSALA__LOGGING__LOG_BODIES=true
export MARSALA__LOGGING__CAPTURE_MITM_PAYLOADS=true
export MARSALA__LOGGING__CAPTURE_MITM_WEBSOCKET_FRAMES=true
export MARSALA__LOGGING__MITM_PAYLOAD_PREVIEW_BYTES=65536
export MARSALA__LOGGING__MITM_WEBSOCKET_FRAME_PREVIEW_BYTES=65536
export OPENAI_API_KEY=sk-...
```

`marsala.example.toml` shows the current compatibility-gateway and proxy-probe config surface. Use `cargo run -p marsala -- config print --all` to inspect the full effective config, including roadmap placeholders that are not active yet.

Current explicit gateway settings:

- `openai.base_url`: upstream base, default `https://api.openai.com/v1`
- `openai.api_key_env`: env var name that holds the upstream API key, default `OPENAI_API_KEY`
- `openai.auth_mode`: upstream auth mode, default `configured_api_key`
- `configured_api_key`: Marsala sends upstream auth from `openai.api_key_env` and strips inbound `Authorization`
- `inbound_authorization`: `POST /v1/responses` forwards the inbound `Authorization` header upstream; missing inbound auth returns a local `authorization_error`
- `/v1/chat/completions` and `/v1/responses` forward non-streaming JSON requests to the configured upstream
- `/v1/responses` with `stream=true` streams upstream bytes through without parsing or mutating chunks
- `/v1/chat/completions` with `stream=true` is still rejected locally
- `/v1/chat/completions` still uses `openai.api_key_env`; inbound auth passthrough is intentionally limited to `/v1/responses` for this spike

Codex custom-provider steel thread with configured API-key auth. Use a custom provider and disable provider WebSockets so Codex uses the HTTP Responses stream path that Marsala currently supports:

```bash
OPENAI_API_KEY=sk-... cargo run -p marsala -- serve

CODEX_API_KEY=sk-local-routed-through-marsala codex exec \
  -c 'model="gpt-5"' \
  -c 'model_provider="marsala_responses"' \
  -c 'model_providers.marsala_responses.name="Marsala Responses"' \
  -c 'model_providers.marsala_responses.base_url="http://127.0.0.1:8787/v1"' \
  -c 'model_providers.marsala_responses.env_key="CODEX_API_KEY"' \
  -c 'model_providers.marsala_responses.wire_api="responses"' \
  -c 'model_providers.marsala_responses.supports_websockets=false' \
  'Reply with one short sentence.'
```

Codex sends the local request with the configured custom-provider env key. Marsala forwards upstream using `openai.api_key_env` from its own environment, defaulting to `OPENAI_API_KEY`; inbound `Authorization` is logged only as redacted shape metadata and is not forwarded.

Codex-auth passthrough spike. This mode is empirical: it validates whether Codex will send OpenAI/Codex auth to a custom provider and whether OpenAI accepts that token on the upstream Responses API. The observed `api.openai.com` result is transport-success/upstream-rejected: Marsala forwards the inbound auth header to `POST /v1/responses`, and upstream returns `401 Unauthorized`. Treat that as the expected Path A result unless future MITM inspection discovers a different upstream host or path for subscription-backed normal Codex traffic.

```bash
MARSALA__OPENAI__AUTH_MODE=inbound_authorization cargo run -p marsala -- serve

env -u CODEX_API_KEY -u OPENAI_API_KEY \
  codex exec \
    -c 'model="gpt-5"' \
    -c 'model_provider="marsala_responses"' \
    -c 'model_providers.marsala_responses.name="Marsala Responses"' \
    -c 'model_providers.marsala_responses.base_url="http://127.0.0.1:8787/v1"' \
    -c 'model_providers.marsala_responses.requires_openai_auth=true' \
    -c 'model_providers.marsala_responses.wire_api="responses"' \
    -c 'model_providers.marsala_responses.supports_websockets=false' \
    'Reply with one short sentence.'
```

Expected local validation: Marsala logs `responses_request` with `auth_mode=inbound_authorization`, `auth_shape.authorization_present=true`, redacted upstream authorization metadata, and no raw token/body/chunk content by default. For the public `api.openai.com/v1/responses` path, expect `responses_response` with `status=401`; do not interpret passthrough transport as API authorization success.

Proxy probe settings:

- `proxy.enabled=true` starts a metadata-only proxy listener on `proxy.host:proxy.port`
- Plain HTTP proxy requests are logged and return local `501 not_implemented`
- HTTPS `CONNECT` requests are logged; non-allowlisted targets are tunneled without MITM or decryption
- Body logging and stream capture are disabled by default
- Allowlisted MITM HTTP body preview capture is disabled by default; enable it with `logging.capture_mitm_payloads=true` or `MARSALA__LOGGING__CAPTURE_MITM_PAYLOADS=true`
- Allowlisted MITM WebSocket text-frame preview capture is disabled by default; enable it with `logging.capture_mitm_websocket_frames=true` or `MARSALA__LOGGING__CAPTURE_MITM_WEBSOCKET_FRAMES=true`
- Config preview caps default to `logging.mitm_payload_preview_bytes=4096` and `logging.mitm_websocket_frame_preview_bytes=4096`; `just ui` and `just mitm-serve-capture` raise both to `65536` so real Codex request envelopes are inspectable. Captured text runs through lightweight local redaction before it is written to JSONL
- WebSocket upgrade/proxy support on the explicit `/v1/responses` gateway is not implemented; WebSocket forwarding exists only for exact allowlisted MITM HTTP/1.1 `Upgrade: websocket` requests

MITM foundation settings:

- `mitm.enabled=false` keeps TLS interception off by default
- `mitm.default_action="tunnel"` is the only supported default action in this foundation
- `mitm.allow_hosts` must contain exact hostnames only, such as `chatgpt.com`, `ab.chatgpt.com`, or API-path `api.openai.com`; URLs, ports, and wildcards are rejected
- `mitm.ca_cert_path` and `mitm.ca_key_path` define where `marsala mitm ca init` writes the local CA certificate and private key
- `mitm.enabled=true` terminates TLS only for exact allowlisted `CONNECT` hosts, generates a per-host leaf certificate from the configured CA, logs sanitized `mitm_tls` metadata, forwards decrypted HTTP/1.1 requests upstream over verified TLS, copies upstream responses back downstream within the current operation timeout, supports HTTP/1.1 WebSocket upgrade forwarding to the same CONNECT host:port, and emits sanitized `mitm_request` and `mitm_response` metadata
- Current MITM forwarding supports no-body requests, bounded `Content-Length` request bodies up to 1 MiB, bounded response body copying, and allowlisted HTTP/1.1 WebSocket `101 Switching Protocols` tunnels. Optional capture emits separate `mitm_payload` and `mitm_websocket_frame` events with direction, path, byte counts, truncation metadata, and redacted UTF-8 previews; binary WebSocket payloads are counted/skipped. Request `Transfer-Encoding` streaming, HTTP/2, malformed upgrades, and non-WebSocket upgrades are explicitly rejected/logged as unsupported
- MITM is now the main path for discovering the real subscription-backed normal Codex upstream shape, because inbound-auth passthrough to public `api.openai.com` has been observed to return `401 Unauthorized`
- Normal subscription-backed Codex has been observed connecting to `chatgpt.com:443` and `ab.chatgpt.com:443`; do not add unrelated hosts such as `github.com` to the MITM allowlist

Generate the local Marsala CA without overwriting existing files:

```bash
cargo run -p marsala -- mitm ca init
```

Run Marsala with the proxy and an exact MITM allowlist:

```bash
MARSALA__PROXY__ENABLED=true \
MARSALA__MITM__ENABLED=true \
MARSALA__MITM__ALLOW_HOSTS=chatgpt.com,ab.chatgpt.com \
MARSALA__MITM__CA_CERT_PATH=certs/marsala-ca.pem \
MARSALA__MITM__CA_KEY_PATH=certs/marsala-ca-key.pem \
  cargo run -p marsala -- serve
```

For API/custom-provider validation, use `MARSALA__MITM__ALLOW_HOSTS=api.openai.com` instead or add it alongside the exact ChatGPT hosts.

Run normal Codex through Marsala's HTTPS proxy and point Codex at the generated CA:

```bash
just mitm-codex
```

Expected for this slice: Marsala logs the `CONNECT` metadata, emits `mitm_tls` with `status=handshake_ok` for exact allowlisted ChatGPT hosts, forwards decrypted HTTP/1.1 requests and HTTP/1.1 WebSocket upgrades to the same host:port over verified upstream TLS, tunnels WebSocket bytes after upstream `101 Switching Protocols`, and emits sanitized `mitm_request` and `mitm_response` metadata. If payload capture is enabled, Marsala also emits bounded `mitm_payload` and `mitm_websocket_frame` preview events. If Codex uses HTTP/2, a malformed or non-WebSocket upgrade, request-body streaming beyond bounded `Content-Length`, or a response body stalls beyond the steel-thread timeout, Marsala logs an explicit unsupported or timeout status instead of silently forwarding indefinitely.

To inspect local allowlisted MITM payloads for a validation run, use the browser dashboard with the capture-enabled server command:

```bash
just ui
```

`just ui` defaults to 64 KiB HTTP/WebSocket previews. For a deeper run, pass a larger cap:

```bash
just ui 262144
```

Open `http://127.0.0.1:8787/ui`, then run traffic through the proxy:

```bash
just mitm-codex
```

The dashboard provides a two-pane live timeline and detail/payload preview view with category, host, path substring, payload-only, pause/resume, and clear controls. Raw JSONL remains available with `cargo run -p marsala -- logs tail --lines 80 --follow` or `just logs` when debugging the event file itself.

Capture is a local dev-tool feature: keep it off for routine runs, keep `mitm.allow_hosts` exact and narrow, and treat `logs/events.jsonl` and the browser UI contents as sensitive when enabled.

See [docs/validation/codex-acquisition-probe.md](docs/validation/codex-acquisition-probe.md) for exact Codex validation commands.
See [docs/planning/05_mitm_foundation.md](docs/planning/05_mitm_foundation.md) for the planned allowlisted MITM steel thread and Rust stack.

## Docker

Build and run:

```bash
docker build -t marsala .
docker run --rm -p 8787:8787 -v "$(pwd)/logs:/app/logs" marsala
```

The container overrides a runtime setting:

- bind host becomes `0.0.0.0`

## Current CLI surface

- `serve`: start the Axum server
- `config print`: print the merged active gateway/proxy config (`server`, `openai`, `logging`, `proxy`)
- `config print --all`: include roadmap sections such as `openai`, `rewrite`, `tool_capture`, `proxy`, and `mitm`
- `logs tail`: print the last JSONL entries, with optional `--follow`
- `logs tail --follow`: prints one waiting message to stderr when `logs/events.jsonl` does not exist yet, then resumes on file creation or recreation
- `mitm ca init`: generate `mitm.ca_cert_path` and `mitm.ca_key_path` without overwriting existing files

## Interception UI

- `GET /ui`: served browser dashboard for live event inspection
- `GET /ui/events/recent`: recent transformed events from the configured JSONL event log
- `GET /ui/events`: SSE feed of new transformed events from the configured JSONL event log

The UI reads the same `logs/events.jsonl` stream as `logs tail`; it does not change API forwarding, proxy tunneling, or MITM behavior.
