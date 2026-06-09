# Codex Acquisition Probe

This note validates Marsala as an explicit Codex Responses gateway and metadata-only proxy probe. It validates `/v1/responses` forwarding through the configured custom-provider/base-URL path, including HTTP streaming passthrough when Codex sends `stream=true`. It does not validate WebSocket forwarding or TLS interception.

## Start Marsala

```bash
OPENAI_API_KEY=sk-... MARSALA__PROXY__ENABLED=true cargo run -p marsala -- serve
```

Defaults used by these commands:

- Gateway: `http://127.0.0.1:8787`
- Proxy probe: `http://127.0.0.1:8788`
- Events: `logs/events.jsonl`
- Body logging: disabled
- Stream capture: disabled

Watch logs in another terminal:

```bash
cargo run -p marsala -- logs tail --lines 40 --follow
```

## Direct OpenAI Provider Base URL Caveat

Codex's built-in OpenAI provider can attempt a WebSocket request to `/v1/responses` when pointed at Marsala with `openai_base_url`. Marsala does not currently implement `/v1/responses` WebSocket upgrade/proxying, so this path can fail with `HTTP 405` before Codex falls back. Prefer the custom-provider command below, which explicitly disables provider WebSockets.

```bash
CODEX_API_KEY=sk-local-routed-through-marsala codex exec \
  -c 'openai_base_url="http://127.0.0.1:8787/v1"' \
  -c 'model="gpt-5"' \
  'Reply with one short sentence.'
```

If this path falls back to HTTP streaming, expected Marsala events are:

- `responses_request` with `path=/v1/responses`, `target=upstream`, `upstream_url=https://api.openai.com/v1/responses`, `auth_shape.authorization_present=true`, and no body field by default.
- `responses_response` with the upstream status, `source=upstream`, and for streamed responses `stream_status=completed`.

## Custom Provider

This validates a Codex custom provider using the Responses wire API. The custom provider base URL is explicitly versioned as `/v1`; Marsala appends `/responses` when it forwards to its configured upstream and does not create `/v1/v1/responses`. `supports_websockets=false` forces Codex onto the HTTP streaming Responses path that Marsala supports today.

```bash
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

Expected Marsala events:

- `responses_request` with `path=/v1/responses`, `target=upstream`, `stream=true`, `upstream_url=https://api.openai.com/v1/responses`, `auth_shape.authorization_present=true`, and no body field by default.
- `responses_response` with the upstream status, `source=upstream`, `stream=true`, `stream_status=completed`, `body_bytes`, `body_chunks`, and no body/chunk content by default.

## Proxy Environment Matrix

Clear both upper- and lower-case proxy variables in each test so the result is attributable to the variable under test.

Direct gateway, no proxy:

```bash
env -u HTTP_PROXY -u HTTPS_PROXY -u ALL_PROXY -u http_proxy -u https_proxy -u all_proxy \
  NO_PROXY= no_proxy= CODEX_API_KEY=sk-local-routed-through-marsala \
  codex exec \
    -c 'openai_base_url="http://127.0.0.1:8787/v1"' \
    -c 'model="gpt-5"' \
    'Reply with one short sentence.'
```

Expected: `responses_request`; no `proxy_request`.

Plain HTTP proxy metadata:

```bash
env HTTP_PROXY=http://127.0.0.1:8788 http_proxy=http://127.0.0.1:8788 \
  HTTPS_PROXY= https_proxy= ALL_PROXY= all_proxy= NO_PROXY= no_proxy= \
  CODEX_API_KEY=sk-local-probe \
  codex exec \
    -c 'openai_base_url="http://marsala.invalid/v1"' \
    -c 'model="gpt-5"' \
    'Reply with one short sentence.'
```

Expected: `proxy_request` with a plain HTTP method, visible `target_host`, visible redacted `target_path`, `auth_shape`, and `status=not_implemented`.

HTTPS CONNECT metadata:

```bash
env HTTPS_PROXY=http://127.0.0.1:8788 https_proxy=http://127.0.0.1:8788 \
  HTTP_PROXY= http_proxy= ALL_PROXY= all_proxy= NO_PROXY= no_proxy= \
  CODEX_API_KEY="${OPENAI_API_KEY:-sk-local-probe}" \
  codex exec \
    -c 'openai_base_url="https://api.openai.com/v1"' \
    -c 'model="gpt-5"' \
    'Reply with one short sentence.'
```

Expected: `proxy_request` with `method=CONNECT`, `target_host=api.openai.com`, `target_port=443`, and `status=closed` or `status=error`. Marsala does not decrypt the tunneled request.

Both HTTP and HTTPS proxy variables:

```bash
env HTTP_PROXY=http://127.0.0.1:8788 http_proxy=http://127.0.0.1:8788 \
  HTTPS_PROXY=http://127.0.0.1:8788 https_proxy=http://127.0.0.1:8788 \
  ALL_PROXY= all_proxy= NO_PROXY= no_proxy= CODEX_API_KEY="${OPENAI_API_KEY:-sk-local-probe}" \
  codex exec \
    -c 'openai_base_url="https://api.openai.com/v1"' \
    -c 'model="gpt-5"' \
    'Reply with one short sentence.'
```

Expected for an HTTPS base URL: a `CONNECT` `proxy_request`.

NO_PROXY bypass check:

```bash
env HTTP_PROXY=http://127.0.0.1:8788 http_proxy=http://127.0.0.1:8788 \
  HTTPS_PROXY= https_proxy= ALL_PROXY= all_proxy= NO_PROXY=127.0.0.1 no_proxy=127.0.0.1 \
  CODEX_API_KEY=sk-local-probe \
  codex exec \
    -c 'openai_base_url="http://127.0.0.1:8787/v1"' \
    -c 'model="gpt-5"' \
    'Reply with one short sentence.'
```

Expected: direct `responses_request`; no `proxy_request`.

## Current Gaps

- `/v1/responses` WebSocket upgrade/proxying is not implemented; use a custom provider with `model_providers.marsala_responses.supports_websockets=false`.
- `/v1/chat/completions` streaming passthrough is not implemented; `stream=true` returns local `400 unsupported_streaming`.
- Plain HTTP proxy forwarding is not implemented; it logs metadata and returns local `501`.
- HTTPS `CONNECT` is tunneled without MITM, decryption, request body capture, or response body capture.
- Marsala cannot observe `/v1/responses` paths inside HTTPS tunnels without future opt-in MITM support.
