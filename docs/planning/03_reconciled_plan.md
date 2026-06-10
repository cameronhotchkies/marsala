# Reconciled Project Plan

## Product Thesis

Marsala is a local-first Rust LLM proxy for developers who want to see, inspect, and eventually shape what their tools send to LLM providers.

Initial promise:

- Run locally or in Docker.
- Preserve the currently shipped explicit OpenAI-shaped gateway as a compatibility path.
- Acquire real Codex traffic through Marsala and prove the routing mechanism before adding mutation features.
- Log request and response lifecycle locally with safe defaults.
- Preserve observed streaming behavior once the traffic path is understood.
- Add controlled response cleanup and tool-call capture once Codex traffic acquisition is reliable.

Later promise:

- Expand from Codex to other CLI and SDK clients.
- MITM only allowlisted LLM hosts, never arbitrary traffic.
- Add Anthropic and local providers after the Codex and OpenAI-shaped paths have proven their abstractions.

## Key Decisions

- The explicit OpenAI-shaped gateway (`/v1/chat/completions` and `/v1/responses`) stays as a compatibility scaffold and fixture source, not the product-defining baseline.
- The mainline product path is allowlisted MITM for normal Codex traffic routed through Marsala with proxy environment variables.
- A normal ChatGPT-backed Codex run has been observed through Marsala's proxy as HTTPS `CONNECT` traffic to `chatgpt.com` and `ab.chatgpt.com`; those are the current primary MITM targets.
- `api.openai.com` remains relevant for the API-key gateway and API-billed Codex/custom-provider experiments, but it is not the observed normal subscription-backed Codex host.
- Codex custom-provider `requires_openai_auth=true` plus inbound auth passthrough was tested against `https://api.openai.com/v1/responses` and reached upstream, but upstream returned `401 Unauthorized`. Treat that path as a failed shortcut unless MITM discovers a different subscription-backed upstream.
- Marsala does not claim decrypted Codex interception yet. Current normal Codex proxy success is tunneled metadata only until TLS termination lands.
- Proxy and allowlisted MITM work move ahead of rewrite, tool-capture semantics, and broad provider expansion.
- Streaming is observed and preserved before any streaming mutation.
- Logging is local and useful, but auth headers, proxy credentials, and known secrets are redacted.
- Tool-call capture starts with complete non-streaming calls, then moves to streamed reconstruction.
- CLI interception is no longer deferred until late-phase feature work; it is the next baseline question to retire.

## Must-Verify Unknowns

- [x] Codex custom base URL support: Codex can be pointed at Marsala as a custom Responses provider.
- [x] Proxy env baseline: normal Codex traffic can be routed through Marsala with `HTTPS_PROXY`.
- [x] Initial normal Codex hosts: observed `chatgpt.com:443` and `ab.chatgpt.com:443` through `CONNECT`; unrelated `github.com:443` also appears and must not be MITM'd by default.
- [x] Inbound auth shortcut result: ChatGPT/Codex auth forwarded to `api.openai.com/v1/responses` returns `401 Unauthorized`.
- [ ] Decrypted normal Codex endpoint paths: still unknown until allowlisted MITM TLS termination succeeds.
- [ ] API surface on normal Codex path: whether decrypted traffic uses Responses, ChatGPT backend APIs, WebSocket, or another endpoint family.
- [ ] Streaming shape: framing, chunk ordering, completion markers, and cancellation behavior Marsala must preserve.
- [ ] Auth and session forwarding: which inbound auth headers, bearer tokens, cookies, session headers, or device identifiers must pass through unchanged, and what must always be redacted.
- [ ] TLS, MITM, and cert trust requirements: whether `CODEX_CA_CERTIFICATE` is sufficient, and whether Codex requires HTTP/2, WebSocket, ALPN behavior, or has certificate pinning.

## Recommended Defaults

Local default:

```toml
[server]
host = "127.0.0.1"
port = 8787

[openai]
base_url = "https://api.openai.com/v1"
api_key_env = "OPENAI_API_KEY"

[logging]
enabled = true
log_bodies = false
redact_secrets = true
capture_stream_chunks = false

[rewrite]
mode = "off"
streaming = "passthrough"

[tool_capture]
enabled = false

[proxy]
enabled = false
host = "127.0.0.1"
port = 8788

[mitm]
enabled = false
default_action = "tunnel"
allow_hosts = []
ca_cert_path = "certs/marsala-ca.pem"
ca_key_path = "certs/marsala-ca-key.pem"
```

Docker default should be slightly safer:

```toml
[logging]
enabled = true
log_bodies = false
redact_secrets = true
capture_stream_chunks = false
```

## Phase 0: Project Spine

Goal: a boring Rust service that starts, configures, logs, and shuts down cleanly.

Scope:

- Axum/Hyper server.
- Config file/env support.
- Tracing.
- SQLite migrations or JSONL event writer.
- Health endpoint.
- Basic CLI: `serve`, `config print`, `logs tail`.

Acceptance gates:

- Runs locally and in Docker.
- `GET /healthz` works.
- Clean shutdown under Ctrl-C.
- No proxy behavior yet.

## Compatibility Scaffold: Explicit OpenAI Chat Completions Gateway

Status: current committed behavior retained as a sidecar and fixture source.

Goal: local OpenAI-compatible endpoint that forwards non-streaming chat completions.

Scope:

- `/v1/chat/completions`.
- `stream=false`.
- Forward request to OpenAI.
- Preserve status codes and error bodies.
- Redacted request/response metadata logging.
- Optional body logging behind explicit config.

Acceptance gates:

- OpenAI SDK can point `base_url` at Marsala and complete a non-streaming request.
- Authorization is not stored raw.
- Upstream errors are preserved.
- Fixture tests cover success, 4xx, 5xx, malformed response, and timeout.

Non-goals:

- This scaffold is not evidence that Codex traffic is routed through Marsala.
- This scaffold does not establish Responses API compatibility.
- This scaffold does not prove streaming, interception, or tool-capture behavior.

## Phase 1: Codex Traffic Acquisition Baseline

Status: partially complete. Normal Codex can be routed through Marsala's proxy and completes successfully when tunneled. Decrypted payload inspection is not complete.

Goal: route normal ChatGPT-backed Codex traffic through Marsala and identify the exact allowlisted hosts needed for payload inspection.

Scope:

- Verify the actual Codex routing mechanism in practice:
  - whether Codex honors `HTTP_PROXY`, `HTTPS_PROXY`, both, or neither
  - whether Codex uses direct HTTPS, `CONNECT` tunneling, or another transport shape
  - whether explicit base URL or host overrides participate in the traffic path
- Enumerate the concrete upstream hosts and endpoint paths Codex uses in the baseline workflow.
  - Current observed hosts: `chatgpt.com` and `ab.chatgpt.com`.
  - `github.com` may appear during a run and must remain tunneled unless explicitly in scope.
- Determine the streaming response shape Marsala must preserve:
  - SSE framing
  - chunk boundaries and ordering expectations
  - cancellation and end-of-stream behavior
- Characterize auth and session material on observed Codex traffic:
  - what inbound auth headers/tokens are forwarded upstream
  - whether Marsala ever substitutes env-sourced upstream credentials in this path
  - what must always be redacted from logs
- Establish safe logging defaults for the baseline:
  - metadata on by default
  - request/response body capture off unless explicitly enabled
  - stream chunk capture off by default until shape and volume are understood
- Keep the explicit `/v1/chat/completions` gateway available as a controlled comparison path and compatibility scaffold.

Acceptance gates:

- A documented reproduction shows how Codex is routed through Marsala on a developer machine, with exact proxy env behavior called out.
- The routing result is explicit for both `HTTP_PROXY` and `HTTPS_PROXY`, including whether one is ignored.
- The concrete Codex hosts and endpoint paths observed in the baseline workflow are recorded.
- The plan records what Marsala will need to distinguish later: explicit gateway, tunneled proxy traffic, or payload-inspected traffic.
- The streaming shape is characterized well enough to name the framing Marsala must preserve and the cases still unsupported.
- The observed auth/session shape is written down from sanitized captures without storing raw credentials.
- Safe logging defaults are verified: no raw bearer tokens, no proxy credentials, and no stream/body capture unless explicitly enabled.
- The existing explicit chat completions gateway still works as a compatibility scaffold and does not define success for this milestone.

## Phase 2: CLI Routing And Forward Proxy Baseline

Goal: route real Codex traffic through Marsala with explicit proxy behavior and observable tunnel/intercept outcomes.

Status: tunnel baseline complete for normal Codex. `CONNECT` metadata is logged with target host/port and action. Allowlisted MITM currently selects candidates but returns `mitm_unimplemented` until TLS termination is implemented.

Scope:

- Proxy listener, for example `localhost:8788`.
- Plain HTTP proxy support.
- HTTPS `CONNECT` tunnel support.
- Metadata logging: target host, port, tunnel duration, byte counts if practical, and whether traffic was tunneled or inspected.
- Support the validated Codex routing path discovered in Phase 1, including env-var precedence and exclusions.
- Keep non-allowlisted traffic tunneled or rejected by policy; no arbitrary inspection.
- No claim of decrypted payload interception unless the trust path is verified.

Acceptance gates:

- The validated Codex proxy path can be reproduced on a developer machine.
- The observed behavior for `HTTP_PROXY`, `HTTPS_PROXY`, `ALL_PROXY`, and `NO_PROXY` is documented and reflected in tests or fixtures where possible.
- HTTPS traffic can tunnel untouched when interception is not available or not trusted.
- Marsala can report whether a Codex connection was tunneled, explicitly gatewayed, or payload-inspected.
- The auth forwarding policy for routed/proxied Codex traffic is implemented without storing raw credentials.
- Non-LLM traffic is not inspected by default.

## Phase 3: Allowlisted MITM For Normal Codex

Goal: decrypt and forward only allowlisted normal Codex traffic, starting with `chatgpt.com` and `ab.chatgpt.com`, while leaving unrelated HTTPS traffic tunneled.

Scope:

- Implement and validate the trust path:
  - local CA generation
  - CA export and trust setup with `CODEX_CA_CERTIFICATE`
  - per-host certificate generation for exact allowlisted hosts
  - explicit failure behavior when trust is missing
- Terminate downstream TLS for allowlisted hosts and log handshake metadata without secrets.
- Parse enough decrypted HTTP to record method, redacted path, auth shape, and status.
- Add upstream forwarding only after TLS termination and request visibility are proven.
- Record TLS transport details that affect feasibility: `CONNECT`, SNI, ALPN, HTTP/2, and certificate pinning behavior.
- Keep interception allowlisted and opt-in.

Acceptance gates:

- Normal Codex run produces `mitm_tls` and then `mitm_request` metadata for `chatgpt.com` or `ab.chatgpt.com`.
- Logs contain no raw bearer token, cookie, request body, response body, or stream chunk content by default.
- Non-allowlisted HTTPS targets, including `github.com`, still tunnel and do not produce decrypted metadata.
- Any requirement for local CA trust is recorded with exact setup steps and failure modes.
- No doc claims inspectable Codex interception unless this phase has been proven.

## Phase 4: Logging And Redaction Hardening

Goal: make logging useful without becoming a liability on the validated baseline path.

Scope:

- Request table/event.
- Response table/event.
- Error table/event.
- Optional blob/body records.
- Body hashes when bodies are not stored.
- Retention/prune command.
- Redaction tests.

Acceptance gates:

- Default logs contain no raw bearer tokens.
- Body logging can be disabled and verified.
- Redaction covers nested JSON fields, headers, and proxy credentials.
- Concurrent requests do not corrupt logs or stall proxying badly.

## Phase 5: Streaming Pass-Through

Goal: support the observed streaming shape faithfully after the traffic path is proven.

Scope:

- SSE pass-through.
- Preserve event framing and ordering.
- Observe chunks for metadata where safe.
- Log timing, status, stream completion, and interruption.
- No rewriting.

Acceptance gates:

- The baseline client streaming path works through Marsala without changing framing semantics.
- Client cancellation cleans up upstream work.
- Mid-stream upstream error is logged.
- Fixtures cover fragmented chunks and final usage events.

## Phase 6: Non-Streaming Response Rewriting

Goal: controlled mutation for complete JSON responses only, after routing and streaming behavior are understood.

Scope:

- Pluggable transform interface.
- Modes: `off`, `light`, `aggressive`.
- Deterministic prefix/filler cleanup.
- Validate transformed response shape.
- Before/after audit metadata.
- No streaming rewrite.

Acceptance gates:

- Invalid transform output fails closed or passes through unchanged with an audit event.
- Tool calls are not rewritten unless explicitly allowed.
- Usage metadata is preserved, removed, or marked suspect according to a documented rule.
- OpenAI SDK still accepts transformed responses.

## Phase 7: Tool-Call Capture, Non-Streaming First

Goal: complete tool calls are captured reliably before streamed reconstruction.

Scope:

- Rule config.
- Match by function/tool name.
- Match by complete arguments JSON.
- Capture records in JSONL or SQLite.
- Tests using complete OpenAI tool-call fixtures.

Acceptance gates:

- Non-streaming OpenAI tool calls are captured.
- Matched rules are recorded.
- Capture is observational only and cannot break response delivery.

Example:

```toml
[[tool_capture.rules]]
name = "shell"
tool_name_regex = ".*exec.*|.*shell.*"

[[tool_capture.rules]]
name = "filesystem-write"
arguments_regex = "apply_patch|write_file|overwrite"
```

## Phase 8: Streamed Tool-Call Reconstruction

Goal: streamed tool calls are assembled from deltas.

Scope:

- Fixture-driven streamed tool-call parser.
- Accumulator keyed by tool-call index/id.
- Argument string assembly.
- JSON parse on completion.
- Clear failure logs for malformed or incomplete arguments.
- Rule matching after reconstruction.

Acceptance gates:

- Recorded OpenAI streamed tool-call fixtures reconstruct correctly.
- Partial deltas are logged separately from completed tool calls.
- Malformed reconstruction does not break client streaming.

## Phase 9: Provider Expansion

Goal: introduce broader provider shape based on evidence.

Scope:

- Anthropic spike.
- Local OpenAI-compatible provider config.
- Extract provider interface only after comparing OpenAI and Anthropic needs.
- Provider routing by config.

Acceptance gates:

- The abstraction is informed by a real second-provider implementation.
- OpenAI behavior remains stable.
- Local OpenAI-compatible backends can be configured with minimal changes.

## Defensible Build Order

1. Keep the explicit OpenAI-shaped gateway as a compatibility scaffold.
2. Verify Codex routing, hosts, auth handling, and streaming shape.
3. Add the proxy path and tunnel behavior required to reproduce the Codex baseline.
4. Resolve interception viability and trust requirements before claiming inspectable Codex traffic.
5. Make logging and redaction trustworthy for the Codex baseline.
6. Add streaming passthrough where the observed traffic path needs it.
7. Add buffered-only mutation.
8. Add complete tool-call capture.
9. Add streamed tool-call reconstruction.
10. Generalize providers.

This keeps the ambition intact while avoiding two symmetrical mistakes: treating the compatibility gateway as the product, or assuming Codex interception will fall out automatically once an SDK proxy exists.
