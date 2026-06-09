# Codex Acquisition Probe

This note validates Marsala as a metadata-only acquisition probe for Codex traffic. It does not validate `/v1/responses` forwarding, streaming, or TLS interception.

## Start Marsala

```bash
MARSALA__PROXY__ENABLED=true cargo run -p marsala -- serve
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

This uses Codex's built-in OpenAI provider and points its base URL at Marsala. A dummy API key is enough to make Codex send the local request because Marsala returns a local `501`.

```bash
CODEX_API_KEY=sk-local-probe codex exec \
  -c 'openai_base_url="http://127.0.0.1:8787/v1"' \
  -c 'model="gpt-5.5"' \
  'Reply with one short sentence.'
```

Expected Marsala events:

- `responses_request` with `path=/v1/responses`, `auth_shape.authorization_present=true`, and no body field.
- `responses_response` with `status=501`.

## Custom Provider

This validates a Codex custom provider using the Responses wire API.

```bash
CODEX_API_KEY=sk-local-probe codex exec \
  -c 'model="gpt-5.5"' \
  -c 'model_provider="marsala_probe"' \
  -c 'model_providers.marsala_probe.name="Marsala Responses Probe"' \
  -c 'model_providers.marsala_probe.base_url="http://127.0.0.1:8787/v1"' \
  -c 'model_providers.marsala_probe.env_key="CODEX_API_KEY"' \
  -c 'model_providers.marsala_probe.wire_api="responses"' \
  'Reply with one short sentence.'
```

Expected Marsala events are the same as the direct `openai_base_url` test.

## Proxy Environment Matrix

Clear both upper- and lower-case proxy variables in each test so the result is attributable to the variable under test.

Direct gateway, no proxy:

```bash
env -u HTTP_PROXY -u HTTPS_PROXY -u ALL_PROXY -u http_proxy -u https_proxy -u all_proxy \
  NO_PROXY= no_proxy= CODEX_API_KEY=sk-local-probe \
  codex exec \
    -c 'openai_base_url="http://127.0.0.1:8787/v1"' \
    -c 'model="gpt-5.5"' \
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
    -c 'model="gpt-5.5"' \
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
    -c 'model="gpt-5.5"' \
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
    -c 'model="gpt-5.5"' \
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
    -c 'model="gpt-5.5"' \
    'Reply with one short sentence.'
```

Expected: direct `responses_request`; no `proxy_request`.

## Current Gaps

- `/v1/responses` forwarding is not implemented; the endpoint intentionally returns local `501`.
- Plain HTTP proxy forwarding is not implemented; it logs metadata and returns local `501`.
- HTTPS `CONNECT` is tunneled without MITM, decryption, request body capture, or response body capture.
- Marsala cannot observe `/v1/responses` paths inside HTTPS tunnels without future opt-in MITM support.
