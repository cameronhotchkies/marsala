# Codex Acquisition Probe

This note validates Marsala as an explicit Codex Responses gateway and metadata-only proxy probe. It validates non-streaming `/v1/responses` forwarding through the configured custom-provider/base-URL path. It does not validate streaming or TLS interception.

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

## Direct OpenAI Provider Base URL

This uses Codex's built-in OpenAI provider and points its base URL at Marsala. `CODEX_API_KEY` only authenticates the local request shape; Marsala forwards upstream with its configured `openai.api_key_env`, defaulting to `OPENAI_API_KEY`.

```bash
CODEX_API_KEY=sk-local-routed-through-marsala codex exec \
  -c 'openai_base_url="http://127.0.0.1:8787/v1"' \
  -c 'model="gpt-5"' \
  'Reply with one short sentence.'
```

Expected Marsala events:

- `responses_request` with `path=/v1/responses`, `target=upstream`, `upstream_url=https://api.openai.com/v1/responses`, `auth_shape.authorization_present=true`, and no body field by default.
- `responses_response` with the upstream status and `source=upstream`.

## Custom Provider

This validates a Codex custom provider using the Responses wire API. The custom provider base URL is explicitly versioned as `/v1`; Marsala appends `/responses` when it forwards to its configured upstream and does not create `/v1/v1/responses`.

```bash
CODEX_API_KEY=sk-local-routed-through-marsala codex exec \
  -c 'model="gpt-5"' \
  -c 'model_provider="marsala_responses"' \
  -c 'model_providers.marsala_responses.name="Marsala Responses"' \
  -c 'model_providers.marsala_responses.base_url="http://127.0.0.1:8787/v1"' \
  -c 'model_providers.marsala_responses.env_key="CODEX_API_KEY"' \
  -c 'model_providers.marsala_responses.wire_api="responses"' \
  'Reply with one short sentence.'
```

Expected Marsala events are the same as the direct `openai_base_url` test.

For this steel thread, send non-streaming requests only. `stream=true` returns a local `400 unsupported_streaming`, logs `target=marsala_local`, and does not call the upstream.

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

- `/v1/responses` streaming passthrough is not implemented; `stream=true` returns local `400 unsupported_streaming`.
- Plain HTTP proxy forwarding is not implemented; it logs metadata and returns local `501`.
- HTTPS `CONNECT` is tunneled without MITM, decryption, request body capture, or response body capture.
- Marsala cannot observe `/v1/responses` paths inside HTTPS tunnels without future opt-in MITM support.
