# MITM Steel Thread Validation

Status: active MITM track. Marsala can generate a local CA, validate exact-host MITM config, and select allowlisted `CONNECT` targets. Allowlisted targets currently return `mitm_unimplemented`; non-allowlisted targets tunnel. HTTPS decryption is not implemented yet.

Observed facts:

- Inbound-auth passthrough to public `api.openai.com/v1/responses` was transport-success/upstream-rejected with `401 Unauthorized`.
- Normal ChatGPT-backed Codex succeeds through Marsala's proxy when tunneled.
- That normal path currently emits `CONNECT` metadata for `chatgpt.com:443` and `ab.chatgpt.com:443`.
- `github.com:443` may appear during a run and must remain tunneled unless deliberately in scope.
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

Expected today: config prints successfully. This proves only config acceptance, not interception.

## Current CONNECT Baseline

Start Marsala with the proxy and MITM config accepted:

```bash
MARSALA__PROXY__ENABLED=true \
MARSALA__MITM__ENABLED=true \
MARSALA__MITM__ALLOW_HOSTS=chatgpt.com,ab.chatgpt.com \
MARSALA__MITM__CA_CERT_PATH=certs/marsala-ca.pem \
MARSALA__MITM__CA_KEY_PATH=certs/marsala-ca-key.pem \
  cargo run -p marsala -- serve
```

In another terminal, run normal Codex through the proxy:

```bash
env HTTPS_PROXY=http://127.0.0.1:8788 https_proxy=http://127.0.0.1:8788 \
  HTTP_PROXY= http_proxy= ALL_PROXY= all_proxy= NO_PROXY= no_proxy= \
  CODEX_CA_CERTIFICATE="$PWD/certs/marsala-ca.pem" \
  codex exec \
    --skip-git-repo-check \
    -c 'approval_policy="never"' \
    'Reply with one short sentence.'
```

Expected today:

- `proxy_request` entries with `method=CONNECT`, `target_host=chatgpt.com` or `target_host=ab.chatgpt.com`, and `target_port=443`.
- If those hosts are allowlisted and TLS termination is still missing, `status=mitm_unimplemented`.
- If MITM is disabled or the hosts are not allowlisted, `connect_action=tunnel` and `status=closed` or `status=error`.
- No visible `/v1/responses` path from inside the tunnel.
- No `mitm_request` event.
- `CODEX_CA_CERTIFICATE` points at a generated CA, but this slice does not terminate TLS or decrypt traffic yet.

## Future MITM Proof

After TLS termination is implemented, start Marsala the same way and run normal Codex with the Marsala CA:

```bash
env HTTPS_PROXY=http://127.0.0.1:8788 https_proxy=http://127.0.0.1:8788 \
  HTTP_PROXY= http_proxy= ALL_PROXY= all_proxy= NO_PROXY= no_proxy= \
  CODEX_CA_CERTIFICATE="$PWD/certs/marsala-ca.pem" \
  codex exec \
    --skip-git-repo-check \
    -c 'approval_policy="never"' \
    'Reply with one short sentence.'
```

If a client does not honor `CODEX_CA_CERTIFICATE`, test the generic CA variable separately:

```bash
env HTTPS_PROXY=http://127.0.0.1:8788 https_proxy=http://127.0.0.1:8788 \
  HTTP_PROXY= http_proxy= ALL_PROXY= all_proxy= NO_PROXY= no_proxy= \
  SSL_CERT_FILE="$PWD/certs/marsala-ca.pem" \
  codex exec \
    --skip-git-repo-check \
    -c 'approval_policy="never"' \
    'Reply with one short sentence.'
```

Expected future proof:

- `proxy_request` shows the allowlisted `CONNECT`.
- `mitm_request` shows `host=chatgpt.com` or `host=ab.chatgpt.com`, `method`, redacted `path`, and upstream `status`.
- `logs/events.jsonl` contains no raw bearer token, cookie, request body, response body, or stream chunk content.
- Non-allowlisted HTTPS targets still tunnel and do not produce `mitm_request`.

Do not claim payload interception until this future proof succeeds on a normal Codex run.
