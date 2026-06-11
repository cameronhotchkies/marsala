# Marsala

Marsala is a local-first observability and control layer for AI traffic.

It sits between AI clients and model providers so developers can inspect what
their tools send and receive, keep an auditable local record, and eventually
apply provider-independent policies to those flows. Marsala is intended for
SDKs, coding agents, command-line tools, local models, hosted APIs, and future
AI protocols rather than any single vendor.

The long-term model is universal:

- connect clients through an explicit gateway or local forward proxy
- identify traffic by transport, host, route, and protocol
- record request, response, streaming, and tool activity locally
- redact credentials and sensitive fields before persistence
- apply routing, capture, transformation, and policy rules without coupling the
  core to one provider
- add protocol adapters only where structured interpretation is useful

Marsala aims to be provider neutral. Its interception foundation works today,
while broader protocol adapters, normalized events, routing, and policy remain
under development.

## What Works Today

- Local Rust service with JSONL event logging and a browser dashboard.
- HTTP forward-proxy listener with HTTPS `CONNECT` tunneling.
- Opt-in TLS interception for exact allowlisted hosts.
- Sanitized HTTP/1.1 request and response metadata.
- HTTP/1.1 WebSocket upgrade forwarding on the validated interception path.
- Optional bounded, redacted previews of HTTP bodies and WebSocket text frames.
- Structured gateway routes for Chat Completions and Responses APIs.
- Streaming byte passthrough for `POST /v1/responses` with `stream=true`.
- A validated coding-agent integration used to exercise proxying, TLS trust,
  WebSockets, capture, and redaction.

Current limitations:

- Structured gateway adapters currently support one API family.
- Plain HTTP forward-proxy requests are observed but not forwarded.
- MITM forwarding is HTTP/1.1 only.
- Request and response forwarding is bounded rather than fully streaming.
- WebSocket support is limited to the validated HTTP/1.1 upgrade path.
- Provider-neutral event normalization, routing, policy, and tool-call models
  are not implemented yet.

## Architecture

Marsala has two complementary ingress paths.

### Interception Proxy

Clients use Marsala as an HTTP/HTTPS proxy. Non-allowlisted HTTPS connections
are tunneled unchanged. Exact allowlisted hosts can be TLS-terminated using a
local CA so Marsala can observe supported HTTP and WebSocket traffic.

This is the provider-neutral foundation: the proxy operates on network hosts
and protocols rather than requiring a provider SDK or API-compatible base URL.

### Structured Gateway

Clients can send supported API requests directly to Marsala. The current
gateway implements Chat Completions and Responses routes. These provide a
working adapter and test bed while broader provider and protocol adapters are
developed.

## Quick Start

Prerequisites:

- Rust 1.96 or newer
- `cargo` from the same toolchain

Start Marsala:

```bash
cargo run -p marsala -- serve
```

Then inspect the service:

```bash
curl http://127.0.0.1:8787/healthz
cargo run -p marsala -- config print --all
cargo run -p marsala -- logs tail --lines 20
```

Open the dashboard at `http://127.0.0.1:8787/ui`.

Local defaults:

- service and UI: `127.0.0.1:8787`
- forward proxy: `127.0.0.1:8788`, disabled by default
- event log: `logs/events.jsonl`
- body and WebSocket payload capture: disabled by default
- TLS interception: disabled by default

## Configuration

Configuration is loaded in this order:

1. built-in defaults
2. `marsala.toml` in the current directory
3. `--config /path/to/file.toml` or `MARSALA_CONFIG`
4. environment variables using the `MARSALA__...` prefix

See `marsala.example.toml` for the current configuration surface.

Example proxy and interception configuration:

```toml
[proxy]
enabled = true
host = "127.0.0.1"
port = 8788

[mitm]
enabled = true
default_action = "tunnel"
allow_hosts = ["provider.example"]
ca_cert_path = "certs/marsala-ca.pem"
ca_key_path = "certs/marsala-ca-key.pem"
```

`mitm.allow_hosts` accepts exact hostnames only. Wildcards, URLs, and hostnames
with ports are rejected. Traffic to every other host remains tunneled.

Generate a local CA without overwriting existing files:

```bash
cargo run -p marsala -- mitm ca init
```

CA material under `certs/`, captured events under `logs/`, and local `.env`
files are excluded from Git. Treat all three as sensitive.

## Capture And Privacy

Marsala records metadata by default. HTTP body and WebSocket text previews
require explicit opt-in:

```bash
export MARSALA__LOGGING__CAPTURE_MITM_PAYLOADS=true
export MARSALA__LOGGING__CAPTURE_MITM_WEBSOCKET_FRAMES=true
export MARSALA__LOGGING__MITM_PAYLOAD_PREVIEW_BYTES=65536
export MARSALA__LOGGING__MITM_WEBSOCKET_FRAME_PREVIEW_BYTES=65536
```

Captured previews are bounded and passed through local redaction, but redaction
is not a guarantee that arbitrary sensitive content cannot appear. Keep capture
disabled for routine use and handle `logs/events.jsonl` as sensitive whenever
capture is enabled.

## Current Gateway Adapter

The structured gateway currently targets OpenAI-compatible upstreams:

```bash
export OPENAI_API_KEY=sk-...
cargo run -p marsala -- serve
```

```bash
curl http://127.0.0.1:8787/v1/chat/completions \
  -H 'content-type: application/json' \
  -d '{"model":"gpt-4.1-mini","messages":[{"role":"user","content":"Say hi"}]}'
```

```bash
curl http://127.0.0.1:8787/v1/responses \
  -H 'content-type: application/json' \
  -d '{"model":"gpt-5","input":"Say hi"}'
```

Relevant settings:

- `openai.base_url`: compatible upstream base URL
- `openai.api_key_env`: environment variable containing the upstream API key
- `openai.auth_mode`: configured API key or inbound authorization for the
  supported Responses path

This adapter is useful now, but provider-specific configuration will move
behind broader adapter and routing concepts as Marsala evolves.

## Validated Integration: Codex

Codex is an end-to-end validation target. It has exercised Marsala's HTTPS
proxy routing, exact-host TLS interception, local CA trust, HTTP/1.1 forwarding,
compressed WebSocket previews, redaction, and live UI.

The Codex-specific recipes and observed endpoints are retained as integration
documentation, not as Marsala's general product interface:

- `docs/validation/codex-acquisition-probe.md`
- `docs/validation/mitm-steel-thread.md`
- `docs/planning/06_terminal_codex_interception.md`

Convenience recipes include:

```bash
just ui
just mitm-codex
just codex-env
```

## CLI

- `serve`: start the service, UI, and any enabled proxy listener
- `config print`: print active configuration
- `config print --all`: include disabled and roadmap sections
- `logs tail`: inspect or follow the JSONL event log
- `mitm ca init`: generate the configured local CA
- `codex env install`: install the current Codex integration environment block
- `codex env uninstall`: remove that marked environment block

## Docker

```bash
docker build -t marsala .
docker run --rm -p 8787:8787 -v "$(pwd)/logs:/app/logs" marsala
```

The container binds the service to `0.0.0.0`. Host-level proxy routing and CA
trust still need to be configured outside the container.

## Project Direction

Marsala is working toward:

- provider and protocol adapters for hosted APIs and local model servers
- normalized events across HTTP, streaming, WebSocket, and tool-call transports
- configurable routing across hosted and local models
- provider-independent capture and redaction policies
- deterministic request and response transformations
- replayable fixtures and traffic analysis
- explicit extension points for new clients, providers, and protocols

Provider neutrality means supporting multiple providers without pretending
their APIs are identical. Marsala will preserve provider-specific details where
they matter and normalize only concepts proven common across real integrations.

## Workspace

- `crates/marsala`: service crate
- `docs/planning`: design and implementation notes
- `docs/validation`: integration-specific validation records

Marsala is licensed under the MIT license.
