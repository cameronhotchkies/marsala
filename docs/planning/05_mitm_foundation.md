# MITM Foundation Plan

Status: active mainline. Marsala can generate a local CA, select exact allowlisted `CONNECT` targets, terminate downstream TLS, forward decrypted HTTP/1.1 requests upstream over verified TLS, copy upstream responses downstream within the current steel-thread timeout policy, and log sanitized MITM metadata.

## Decision

Marsala should implement TLS interception only for explicit allowlisted hosts reached through the existing HTTP proxy `CONNECT` path. The default action remains untouched tunneling. Non-allowlisted `CONNECT` targets must never be decrypted.

Recommended Rust stack:

- `rustls` and `tokio-rustls` for downstream TLS termination and upstream TLS client connections.
- `rcgen` for local CA generation and per-host leaf certificate issuance.
- `hyper`, `hyper-util`, and `http-body-util` for HTTP server/client plumbing after TLS termination.
- Existing `tokio` for TCP, timeouts, and bidirectional copy fallback.
- Existing `serde_json` event logging for sanitized method/path/status records.

Why this fits the repo:

- `reqwest` already pulls in the rustls/hyper family through the current lockfile, so the project is already aligned with a rustls-native stack.
- The existing proxy listener already parses `CONNECT`, logs sanitized metadata, and tunnels non-inspected traffic.
- `hyper` gives the right shape for HTTP/1.1 and HTTP/2, but the first implementation can explicitly support HTTP/1.1 and report HTTP/2/ALPN as a blocker if Codex requires it.

## Config Shape

The committed config shape is:

```toml
[proxy]
enabled = true
host = "127.0.0.1"
port = 8788

[mitm]
enabled = false
default_action = "tunnel"
allow_hosts = []
ca_cert_path = "certs/marsala-ca.pem"
ca_key_path = "certs/marsala-ca-key.pem"
```

Semantics:

- `enabled`: opt-in switch for TLS interception of exact allowlisted `CONNECT` hosts.
- `default_action`: currently only `tunnel` is accepted.
- `allow_hosts`: exact hostnames eligible for MITM, for example `api.openai.com` and `chatgpt.com`. URLs, ports, wildcards, and whitespace are rejected.
- `ca_cert_path`: local root CA certificate path to export and trust in clients.
- `ca_key_path`: local root CA private key path. The default `certs/` directory is gitignored.

Trust guidance for the next implementation slice:

- Export the CA certificate from `mitm.ca_cert_path`.
- For Codex, set both `CODEX_CA_CERTIFICATE=$PWD/certs/marsala-ca.pem` and `SSL_CERT_FILE=$PWD/certs/marsala-ca.pem` so Codex-specific and generic TLS paths trust the same local CA.
- Do not require system trust-store installation for the first validation; document it later as optional platform-specific setup.

## First Steel Thread

Target hosts:

- `chatgpt.com:443`
- `ab.chatgpt.com:443`

Secondary/API-key target:

- `api.openai.com:443`

Normal ChatGPT-backed Codex has been observed tunneling to `chatgpt.com` and `ab.chatgpt.com`. `api.openai.com` is the API-billed/custom-provider path and is not the current mainline for subscription-backed Codex.

Flow:

1. Accept a proxy `CONNECT host:443` request.
2. If `mitm.enabled=false`, or `host` is not in `mitm.allow_hosts`, keep the existing untouched tunnel behavior.
3. If `mitm.enabled=true` and `host` is allowlisted, return `HTTP/1.1 200 Connection Established`.
4. Wrap the client side in a rustls server session using a leaf certificate generated for the exact host from the local CA.
5. Open an upstream TLS connection to the same host and port with normal server verification.
6. Negotiate ALPN deliberately:
   - First pass: advertise/support `http/1.1` only.
   - If Codex requires `h2`, record that as the next blocker and add HTTP/2 support before claiming interception.
7. Parse the decrypted HTTP/1.1 request headers.
8. Forward method, URI, sanitized hop-by-hop-filtered headers, and no-body or bounded `Content-Length` bodies upstream without substituting or logging auth values.
9. Return upstream status, safe headers, and response body bytes to the client within the current operation timeout.
10. Log sanitized metadata only:
    - event type, peer address, host, port
    - `interception=mitm`
    - method, redacted path/query, status
    - auth-shape booleans/schemes
    - byte counts if available
    - no request body, response body, stream chunks, bearer tokens, cookies, or proxy credentials

Success criterion for the first steel thread:

- A normal Codex run with `HTTPS_PROXY=http://127.0.0.1:8788`, `CODEX_CA_CERTIFICATE=$PWD/certs/marsala-ca.pem`, and `SSL_CERT_FILE=$PWD/certs/marsala-ca.pem` produces `mitm_request` and `mitm_response` metadata for `chatgpt.com` or `ab.chatgpt.com`, including visible method/path/upstream status, with no body or secret material in `logs/events.jsonl` by default.

## Blockers And Risks

- ALPN/HTTP2: Codex may require `h2`; an HTTP/1.1-only MITM may fail after the TLS handshake.
- WebSocket over TLS: ChatGPT or Codex subscription paths may use WebSocket upgrades that need explicit proxying.
- Certificate pinning: clients may reject a locally trusted CA regardless of `CODEX_CA_CERTIFICATE`.
- Trust-store setup: `CODEX_CA_CERTIFICATE` may not cover every TLS stack used by Codex or ChatGPT traffic.
- Request body streaming: `/v1/responses` streams and large bodies must be forwarded without buffering everything into memory.
- Upstream auth preservation: inbound bearer/session/cookie headers must pass upstream unchanged but never be logged raw.
- Header semantics: hop-by-hop headers and proxy credentials must be removed or handled correctly.
- Non-allowlisted traffic: accidental wildcard or URL matching would be a security bug; exact host matching is required.

## Implemented Slices

Completed implementation slices:

1. CA generation with `marsala mitm ca init`.
2. `.gitignore` protection for generated certs and keys.
3. Config validation for exact host allowlists.
4. Proxy runtime `CONNECT` decision logging.
5. Downstream TLS termination for exact allowlisted hosts.
6. Sanitized `mitm_tls` and first decrypted HTTP/1.1 `mitm_request` metadata.
7. Non-allowlisted targets tunnel unchanged.
8. Upstream TLS connection to the exact allowlisted target host with normal certificate verification.
9. HTTP/1.1 request forwarding with auth/session headers preserved upstream and proxy/hop-by-hop headers stripped.
10. Upstream response forwarding without body/chunk logging, bounded by the current response-copy timeout.
11. Sanitized `mitm_response` metadata with upstream status, timing, and byte counts.
12. Explicit unsupported statuses for HTTP/2, WebSocket upgrades, and request transfer-encoding streaming.

Next implementation slice:

1. Validate with normal Codex and record whether the subscription-backed path stays on HTTP/1.1.
2. Add HTTP/2 forwarding if Codex negotiates or sends `h2`.
3. Add WebSocket forwarding if the ChatGPT path requires upgrades.
4. Replace bounded request body buffering with true request-body streaming for chunked or large uploads.
5. Keep body capture, rewrite, and broad provider expansion out until forwarding is proven.
