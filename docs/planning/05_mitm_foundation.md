# MITM Foundation Plan

Status: planned foundation. Marsala does not decrypt HTTPS traffic yet.

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

- `enabled`: opt-in switch for TLS interception. Today it only validates config; it does not decrypt.
- `default_action`: currently only `tunnel` is accepted.
- `allow_hosts`: exact hostnames eligible for future MITM, for example `api.openai.com` and `chatgpt.com`. URLs, ports, wildcards, and whitespace are rejected.
- `ca_cert_path`: local root CA certificate path to export and trust in clients.
- `ca_key_path`: local root CA private key path. The default `certs/` directory is gitignored.

Trust guidance for the next implementation slice:

- Export the CA certificate from `mitm.ca_cert_path`.
- For Codex, test with `CODEX_CA_CERTIFICATE=$PWD/certs/marsala-ca.pem`.
- For generic rustls/reqwest/OpenSSL-based clients, test `SSL_CERT_FILE=$PWD/certs/marsala-ca.pem` only when that client honors it.
- Do not require system trust-store installation for the first validation; document it later as optional platform-specific setup.

## First Steel Thread

Target hosts:

- `api.openai.com:443`
- `chatgpt.com:443`

Flow:

1. Accept a proxy `CONNECT host:443` request.
2. If `mitm.enabled=false`, or `host` is not in `mitm.allow_hosts`, keep the existing untouched tunnel behavior.
3. If `mitm.enabled=true` and `host` is allowlisted, return `HTTP/1.1 200 Connection Established`.
4. Wrap the client side in a rustls server session using a leaf certificate generated for the exact host from the local CA.
5. Open an upstream TLS connection to the same host and port with normal server verification.
6. Negotiate ALPN deliberately:
   - First pass: advertise/support `http/1.1` only.
   - If Codex requires `h2`, record that as the next blocker and add HTTP/2 support before claiming interception.
7. Parse the decrypted request with hyper.
8. Forward method, URI, headers, and streaming body upstream without substituting or logging auth values.
9. Return upstream status, headers, and streaming body to the client.
10. Log sanitized metadata only:
    - event type, peer address, host, port
    - `interception=mitm`
    - method, redacted path/query, status
    - auth-shape booleans/schemes
    - byte counts if available
    - no request body, response body, stream chunks, bearer tokens, cookies, or proxy credentials

Success criterion for the first steel thread:

- A normal Codex run with `HTTPS_PROXY=http://127.0.0.1:8788` and `CODEX_CA_CERTIFICATE=$PWD/certs/marsala-ca.pem` produces `mitm_request` metadata for an allowlisted host, including visible method/path/status, with no body or secret material in `logs/events.jsonl`.

## Blockers And Risks

- ALPN/HTTP2: Codex may require `h2`; an HTTP/1.1-only MITM may fail after the TLS handshake.
- WebSocket over TLS: ChatGPT or Codex subscription paths may use WebSocket upgrades that need explicit proxying.
- Certificate pinning: clients may reject a locally trusted CA regardless of `CODEX_CA_CERTIFICATE`.
- Trust-store setup: `CODEX_CA_CERTIFICATE` may not cover every TLS stack used by Codex or ChatGPT traffic.
- Request body streaming: `/v1/responses` streams and large bodies must be forwarded without buffering everything into memory.
- Upstream auth preservation: inbound bearer/session/cookie headers must pass upstream unchanged but never be logged raw.
- Header semantics: hop-by-hop headers and proxy credentials must be removed or handled correctly.
- Non-allowlisted traffic: accidental wildcard or URL matching would be a security bug; exact host matching is required.

## Next Code Slice

Smallest implementation slice after this foundation:

1. Add dependencies: `rcgen`, direct `rustls`, `tokio-rustls`, `hyper`, `hyper-util`, and `http-body-util` as needed.
2. Add `marsala mitm ca init` or equivalent subcommand to create the local CA only if files do not already exist.
3. Add `marsala mitm ca print` or equivalent export helper that prints the CA certificate path and trust env examples.
4. Add unit tests for CA file creation permissions, no overwrite by default, and PEM parseability.
5. Keep proxy behavior unchanged until CA generation/export is proven.

Second implementation slice:

1. Add `CONNECT` decision logic: tunnel unless enabled and exact host allowlisted.
2. Add downstream TLS termination for one allowlisted host with generated leaf certs.
3. Add upstream TLS connection and HTTP/1.1 forwarding.
4. Emit `mitm_request` metadata with sanitized method/path/status.
5. Validate with Codex before adding body capture, rewrite, or HTTP/2 claims.
