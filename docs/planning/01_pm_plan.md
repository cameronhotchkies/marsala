# Initial PM Plan

> Supersession note (2026-06-04): This document predates the clarified thesis. Treat proxy interception and MITM language here as original intent, not current capability or validated sequencing. The reconciled plan in `03_reconciled_plan.md` is the source of truth: Marsala currently ships a non-streaming `/v1/chat/completions` compatibility scaffold, and proving Codex traffic acquisition/interception viability is the next baseline milestone.

## Elevator Pitch Hit List

- Rust-native LLM proxy that can run locally or in Docker.
- OpenAI-first, OpenAI-compatible ingress from day one.
- Intended to grow from an explicit API gateway into a CLI interception proxy.
- Forward-proxy and allowlisted MITM support were assumed too early here and require separate viability work.
- Captures requests, responses, streaming chunks, tool calls, timings, and errors into aggressive local logs.
- Rewrites responses to remove filler and enforce local response hygiene.
- Detects and records tool calls matching user-defined patterns.
- Extensible provider layer for later Anthropic, local OpenAI-compatible servers, Ollama, vLLM, and others.
- Toy-sized scope, real engineering: usable locally, testable, observable, and safe by default.

## Product Shape

`marsala` is a local Rust service that sits between developer tools and LLM providers.

This draft framed two intended modes:

1. OpenAI-compatible ingress, where clients point `OPENAI_BASE_URL` or equivalent at Marsala.
2. CLI interception proxy, where CLI traffic would flow through Marsala via `HTTP_PROXY` and `HTTPS_PROXY` once that path is proven.

If HTTPS interception is eventually required, Marsala should only MITM explicitly allowlisted LLM hosts. Everything else should tunnel untouched or be rejected based on config.

## Core Goals

- Provide a local OpenAI-compatible endpoint.
- Proxy requests to OpenAI with streaming support.
- Log full request and response lifecycle locally.
- Rewrite assistant responses to remove filler.
- Capture tool calls by pattern.
- Prove CLI traffic can be intercepted through HTTP/HTTPS proxy mode.
- Keep the architecture small, testable, and provider-extensible.

## Initial Phase Sketch

1. Repo foundation: Rust CLI, config, tracing, health endpoint, Dockerfile.
2. OpenAI-compatible ingress: `/v1/chat/completions`, OpenAI forwarding, non-streaming responses, request IDs, request/response logs.
3. Streaming: SSE passthrough, stream chunk logging, normalized stream events.
4. Rewriting: deterministic filler removal, before/after logs, buffered response support.
5. Tool call capture: match by tool name and arguments regex, including streamed tool-call assembly.
6. Forward proxy without MITM: HTTP proxy support, HTTPS `CONNECT` tunneling, proxy metadata logs.
7. Allowlisted MITM viability, only if required by the validated Codex path: local CA, per-host certificates, decrypted OpenAI request parsing.
8. Packaging and developer UX: README, Docker Compose, examples, shell snippets, troubleshooting.
9. Provider expansion: Anthropic spike, local OpenAI-compatible provider config, model routing.

## Major Risks

- MITM complexity: TLS interception, HTTP/2, certificate trust, and client-specific behavior can dominate the project.
- Streaming rewriting: meaningful rewriting during streaming can break latency or produce awkward partial output.
- Provider normalization: OpenAI Responses, Chat Completions, Anthropic Messages, and local APIs differ enough to require careful modeling.
- Secret logging: aggressive logging is useful but dangerous. Redaction defaults and local-only posture matter.
- Tool-call reconstruction: streamed tool calls arrive in deltas and need reliable assembly.
- Compatibility drift: OpenAI-compatible clients vary in subtle ways.
- Rust proxy stack complexity: lower-level `hyper` work can get intricate around upgrades, `CONNECT`, and TLS.

## PM Recommendation

Build the plain OpenAI-compatible gateway first as a compatibility scaffold. Do not treat that as proof of Codex interception, and do not start with MITM.

The project should earn abstractions through this sequence:

1. Proxy OpenAI non-streaming.
2. Add logging.
3. Add streaming.
4. Add rewriting and tool capture.
5. Verify Codex routing and add forward proxy tunneling where the baseline path needs it.
6. Add allowlisted MITM only after the routing path and trust requirements are understood.
