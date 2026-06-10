# MITM Steel Thread Validation

Status: foundation slice. Marsala can generate a local CA and validate exact-host MITM config, but still tunnels HTTPS `CONNECT`; it does not decrypt HTTPS payloads yet.

## CA Generation

Generate the default local CA paths:

```bash
cargo run -p marsala -- mitm ca init
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
MARSALA__MITM__ALLOW_HOSTS=api.openai.com,chatgpt.com \
MARSALA__MITM__CA_CERT_PATH=certs/marsala-ca.pem \
MARSALA__MITM__CA_KEY_PATH=certs/marsala-ca-key.pem \
  cargo run -p marsala -- config print --all
```

Expected today: config prints successfully. This proves only config acceptance, not interception.

## Current CONNECT Baseline

Start Marsala with the proxy and MITM config accepted:

```bash
OPENAI_API_KEY=sk-... \
MARSALA__PROXY__ENABLED=true \
MARSALA__MITM__ENABLED=true \
MARSALA__MITM__ALLOW_HOSTS=api.openai.com,chatgpt.com \
MARSALA__MITM__CA_CERT_PATH=certs/marsala-ca.pem \
MARSALA__MITM__CA_KEY_PATH=certs/marsala-ca-key.pem \
  cargo run -p marsala -- serve
```

In another terminal, run normal Codex through the proxy:

```bash
env HTTPS_PROXY=http://127.0.0.1:8788 https_proxy=http://127.0.0.1:8788 \
  HTTP_PROXY= http_proxy= ALL_PROXY= all_proxy= NO_PROXY= no_proxy= \
  CODEX_CA_CERTIFICATE="$PWD/certs/marsala-ca.pem" \
  CODEX_API_KEY="${OPENAI_API_KEY:-sk-local-probe}" \
  codex exec \
    -c 'openai_base_url="https://api.openai.com/v1"' \
    -c 'model="gpt-5"' \
    'Reply with one short sentence.'
```

Expected today:

- `proxy_request` with `method=CONNECT`, `target_host=api.openai.com`, `target_port=443`.
- `status=closed` or `status=error`.
- No visible `/v1/responses` path from inside the tunnel.
- No `mitm_request` event.
- `CODEX_CA_CERTIFICATE` points at a generated CA, but this slice does not terminate TLS or decrypt traffic yet.

## Future MITM Proof

After TLS termination is implemented, start Marsala the same way and run normal Codex with the Marsala CA:

```bash
env HTTPS_PROXY=http://127.0.0.1:8788 https_proxy=http://127.0.0.1:8788 \
  HTTP_PROXY= http_proxy= ALL_PROXY= all_proxy= NO_PROXY= no_proxy= \
  CODEX_CA_CERTIFICATE="$PWD/certs/marsala-ca.pem" \
  CODEX_API_KEY="${OPENAI_API_KEY:-sk-local-probe}" \
  codex exec \
    -c 'openai_base_url="https://api.openai.com/v1"' \
    -c 'model="gpt-5"' \
    'Reply with one short sentence.'
```

If a client does not honor `CODEX_CA_CERTIFICATE`, test the generic CA variable separately:

```bash
env HTTPS_PROXY=http://127.0.0.1:8788 https_proxy=http://127.0.0.1:8788 \
  HTTP_PROXY= http_proxy= ALL_PROXY= all_proxy= NO_PROXY= no_proxy= \
  SSL_CERT_FILE="$PWD/certs/marsala-ca.pem" \
  CODEX_API_KEY="${OPENAI_API_KEY:-sk-local-probe}" \
  codex exec \
    -c 'openai_base_url="https://api.openai.com/v1"' \
    -c 'model="gpt-5"' \
    'Reply with one short sentence.'
```

Expected future proof:

- `proxy_request` shows the allowlisted `CONNECT`.
- `mitm_request` shows `host=api.openai.com`, `method`, redacted `path`, and upstream `status`.
- `logs/events.jsonl` contains no raw bearer token, cookie, request body, response body, or stream chunk content.
- Non-allowlisted HTTPS targets still tunnel and do not produce `mitm_request`.

Do not claim payload interception until this future proof succeeds on a normal Codex run.
