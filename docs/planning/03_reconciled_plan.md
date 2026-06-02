# Reconciled Project Plan

## Product Thesis

Marsala is a local-first Rust LLM proxy for developers who want to see, inspect, and eventually shape what their tools send to LLM providers.

Initial promise:

- Run locally or in Docker.
- Point OpenAI-compatible clients at it.
- Proxy OpenAI requests faithfully.
- Log request and response lifecycle locally.
- Preserve streaming behavior.
- Add controlled response cleanup and tool-call capture once the traffic path is reliable.

Later promise:

- Intercept CLIs via `HTTP_PROXY` and `HTTPS_PROXY`.
- MITM only allowlisted LLM hosts, never arbitrary traffic.
- Add Anthropic and local providers after the OpenAI-shaped path has proven its abstractions.

## Key Decisions

- No MITM in the first milestone.
- The first internal model is OpenAI-shaped, not provider-neutral.
- Streaming is faithful passthrough before any streaming mutation.
- Logging is local and useful, but auth headers and known secrets are redacted.
- Tool-call capture starts with complete non-streaming calls, then moves to streamed reconstruction.
- CLI interception enters only after the explicit HTTP gateway is useful.

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
log_bodies = true
redact_secrets = true
capture_stream_chunks = true

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

## Phase 1: OpenAI Reverse Proxy, Non-Streaming

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

## Phase 2: Streaming Pass-Through

Goal: support `stream=true` faithfully.

Scope:

- SSE pass-through.
- Preserve event framing and ordering.
- Observe chunks for metadata where safe.
- Log timing, status, stream completion, and interruption.
- No rewriting.

Acceptance gates:

- OpenAI SDK streaming works through Marsala.
- Client cancellation cleans up upstream work.
- Mid-stream upstream error is logged.
- Fixtures cover fragmented chunks and final usage events.

## Phase 3: Logging And Redaction Hardening

Goal: make logging useful without becoming a liability.

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
- Redaction covers nested JSON fields and headers.
- Concurrent requests do not corrupt logs or stall proxying badly.

## Phase 4: Non-Streaming Response Rewriting

Goal: controlled mutation for complete JSON responses only.

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

## Phase 5: Tool-Call Capture, Non-Streaming First

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

## Phase 6: Streamed Tool-Call Reconstruction

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

## Phase 7: Plain CLI Forward Proxy

Goal: proxy env vars can route traffic through Marsala without MITM.

Scope:

- Proxy listener, for example `localhost:8788`.
- Plain HTTP proxy support.
- HTTPS `CONNECT` tunnel support.
- Metadata logging: target host, port, tunnel duration, and byte counts if practical.
- Allowlist policy scaffolding.
- No TLS interception yet.

Acceptance gates:

- `HTTP_PROXY` and `HTTPS_PROXY` can point at Marsala.
- HTTPS traffic tunnels untouched.
- LLM host connections are visible at metadata level.
- Non-LLM traffic is not inspected.

## Phase 8: Allowlisted MITM For LLM Hosts

Goal: Marsala can inspect selected provider HTTPS traffic.

Scope:

- Local CA generation.
- CA print/export commands.
- Trust setup documentation.
- Per-host certificate generation.
- MITM only for configured hosts.
- Default action for non-allowlisted hosts: tunnel.
- Intercepted OpenAI request parsing.
- Shared logging path with normal ingress where practical.

Acceptance gates:

- `api.openai.com` can be intercepted when explicitly allowlisted.
- Non-allowlisted hosts are never MITMed.
- Startup shows intercepted hosts and DB/log path.
- Users can see whether a connection was tunneled or intercepted.
- Failure modes are explicit when clients do not trust the CA.
- Clear uninstall/remove-CA procedure exists.

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

1. Build the OpenAI-shaped gateway.
2. Make logging and redaction trustworthy.
3. Add streaming passthrough.
4. Add buffered-only mutation.
5. Add complete tool-call capture.
6. Add streamed tool-call reconstruction.
7. Add CLI tunnel proxy.
8. Add allowlisted MITM.
9. Generalize providers.

This keeps the ambition intact while avoiding the failure mode of spending the first month building a fragile MITM proxy before the product can proxy one ordinary LLM request.
