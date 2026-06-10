# Marsala

Marsala currently ships a local-first Rust service skeleton with non-streaming OpenAI-shaped chat completions and Responses forwarding, plus streaming passthrough for `POST /v1/responses` with `stream=true`. It starts cleanly, loads config from TOML and env, exposes `GET /healthz`, `POST /v1/chat/completions`, and `POST /v1/responses`, writes JSONL events, and shuts down on `Ctrl-C`.

That explicit gateway remains supported as a compatibility path for Codex custom providers. The proxy probe verifies routing, proxy env behavior, visible upstream hosts/endpoints, auth-shape redaction, and safe logging defaults. It does not implement body capture, stream capture, MITM, or TLS decryption.

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
- HTTPS `CONNECT` requests are logged and tunneled without MITM or decryption
- Body logging and stream capture are disabled by default
- WebSocket upgrade/proxy support for `/v1/responses` is not implemented; use the custom-provider `supports_websockets=false` setting above for Codex validation

MITM foundation settings:

- `mitm.enabled=false` keeps TLS interception off by default
- `mitm.default_action="tunnel"` is the only supported default action in this foundation
- `mitm.allow_hosts` must contain exact hostnames only, such as `api.openai.com` or `chatgpt.com`; URLs, ports, and wildcards are rejected
- `mitm.ca_cert_path` and `mitm.ca_key_path` define where `marsala mitm ca init` writes the local CA certificate and private key
- `mitm.enabled=true` currently validates config and enables the exact-host allowlist decision helper; this slice still tunnels `CONNECT` traffic and does not decrypt HTTPS payloads yet
- MITM is now the main path for discovering the real subscription-backed normal Codex upstream shape, because inbound-auth passthrough to public `api.openai.com` has been observed to return `401 Unauthorized`

Generate the local Marsala CA without overwriting existing files:

```bash
cargo run -p marsala -- mitm ca init
```

Run Marsala with the proxy and an exact MITM allowlist:

```bash
MARSALA__PROXY__ENABLED=true \
MARSALA__MITM__ENABLED=true \
MARSALA__MITM__ALLOW_HOSTS=api.openai.com,chatgpt.com \
MARSALA__MITM__CA_CERT_PATH=certs/marsala-ca.pem \
MARSALA__MITM__CA_KEY_PATH=certs/marsala-ca-key.pem \
  cargo run -p marsala -- serve
```

Run normal Codex through Marsala's HTTPS proxy and point Codex at the generated CA:

```bash
env HTTPS_PROXY=http://127.0.0.1:8788 https_proxy=http://127.0.0.1:8788 \
  HTTP_PROXY= http_proxy= ALL_PROXY= all_proxy= NO_PROXY= no_proxy= \
  CODEX_CA_CERTIFICATE="$PWD/certs/marsala-ca.pem" \
  codex exec \
    -c 'openai_base_url="https://api.openai.com/v1"' \
    -c 'model="gpt-5"' \
    'Reply with one short sentence.'
```

Expected for this slice: Marsala logs the `CONNECT` metadata and tunnels the HTTPS connection. The generated CA and `CODEX_CA_CERTIFICATE` prepare the trust path, but no decrypted request path, body, stream chunk, or `mitm_request` event is produced yet.

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
