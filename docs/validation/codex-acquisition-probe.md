# Codex Acquisition Probe

This note validates Marsala as an explicit Codex Responses gateway and metadata-only proxy probe. It validates `/v1/responses` forwarding through the configured custom-provider/base-URL path, including HTTP streaming passthrough when Codex sends `stream=true`. It also records the inbound-auth passthrough result for public `api.openai.com`: transport succeeds, but upstream returns `401 Unauthorized`. WebSocket forwarding is supported only on the exact allowlisted MITM HTTP/1.1 `Upgrade: websocket` steel thread described in `mitm-steel-thread.md`; HTTP/2 MITM forwarding remains unsupported.

## Start Marsala

Configured API-key mode:

```bash
OPENAI_API_KEY=sk-... MARSALA__PROXY__ENABLED=true cargo run -p marsala -- serve
```

Codex-auth passthrough spike mode for `/v1/responses`:

```bash
MARSALA__OPENAI__AUTH_MODE=inbound_authorization MARSALA__PROXY__ENABLED=true cargo run -p marsala -- serve
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

## Custom Provider With Codex Auth Passthrough

This is an empirical spike, not a claimed success path. In `openai.auth_mode=inbound_authorization`, Marsala forwards the inbound `Authorization` header upstream for `POST /v1/responses` instead of loading `openai.api_key_env`.

Observed Path A result for public `api.openai.com`: Marsala forwards the inbound ChatGPT/Codex auth header successfully, then upstream returns `401 Unauthorized`. Treat that as transport-success/upstream-rejected unless future MITM inspection discovers a different upstream host or path for subscription-backed normal Codex traffic.

Start Marsala without requiring `CODEX_API_KEY` or `OPENAI_API_KEY` in Marsala's environment:

```bash
MARSALA__OPENAI__AUTH_MODE=inbound_authorization cargo run -p marsala -- serve
```

Then run Codex with a custom provider that asks Codex to attach OpenAI auth:

```bash
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

Expected Marsala events if Codex sends auth:

- `responses_request` with `path=/v1/responses`, `target=upstream`, `auth_mode=inbound_authorization`, `auth_shape.authorization_present=true`, `upstream_headers.authorization=[redacted]`, `upstream_headers.authorization_source=inbound_authorization`, and no `api_key_env`.
- `responses_response` with `status=401` for the observed public `api.openai.com/v1/responses` path. A future `2xx` would mean the token was accepted for a discovered path; `401` or `403` means the passthrough transport worked but upstream rejected the token.

Expected local failure if Codex does not send auth:

- HTTP `401` from Marsala with error type `authorization_error`.
- `responses_request` with `target=marsala_local`, `auth_mode=inbound_authorization`, and `auth_shape.authorization_present=false`.

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

- `/v1/responses` WebSocket upgrade/proxying is not implemented on the explicit gateway/custom-provider path; use a custom provider with `model_providers.marsala_responses.supports_websockets=false`.
- `/v1/chat/completions` streaming passthrough is not implemented; `stream=true` returns local `400 unsupported_streaming`.
- Plain HTTP proxy forwarding is not implemented; it logs metadata and returns local `501`.
- Non-allowlisted HTTPS `CONNECT` is tunneled without MITM, decryption, request body capture, or response body capture.
- Exact allowlisted HTTPS `CONNECT` can be MITM-forwarded for HTTP/1.1 with sanitized metadata, including HTTP/1.1 WebSocket `101 Switching Protocols` tunnels to the same CONNECT host:port; HTTP/2 and request transfer-encoding streaming remain unsupported.
- Inbound-auth passthrough to public `api.openai.com` is not the subscription-backed normal Codex success path as currently observed; allowlisted MITM is the main path for discovering the real upstream shape.

See [mitm-steel-thread.md](mitm-steel-thread.md) for allowlisted MITM validation commands using normal Codex with `HTTPS_PROXY` and `CODEX_CA_CERTIFICATE`.
