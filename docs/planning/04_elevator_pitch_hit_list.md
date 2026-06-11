# Elevator Pitch Hit List

## Honest Pitch

Marsala is a local-first Rust observability proxy for Codex and OpenAI-shaped traffic, with safe local audit logging, a compatibility chat-completions gateway, and a staged path toward reliable opt-in CLI interception for allowlisted LLM providers.

## Hit List

- Local Rust proxy for seeing what LLM clients actually send and receive.
- Codex interception is proven for the current allowlisted HTTP/1.1 WebSocket steel thread; reliability and setup are the next baseline milestone.
- Marsala has demonstrated inspectable normal Codex interception on its allowlisted HTTP/1.1 WebSocket steel thread.
- Existing OpenAI-compatible ingress remains available: point SDKs at `http://localhost:8787/v1`.
- Faithful streaming passthrough follows the observed Codex traffic shape before any stream mutation.
- Local logs with redaction, request IDs, timings, errors, and optional body capture.
- Response cleanup for non-streaming responses, starting with deterministic filler removal.
- Tool-call capture by rule, starting with complete calls and later streamed reconstruction.
- Normal Codex routing through `HTTPS_PROXY` is verified; durable terminal setup and trust diagnostics remain productization work.
- MITM is opt-in, noisy, allowlisted, and disabled by default.
- Anthropic and local model providers are roadmap items after Codex and OpenAI behavior are stable.

## Do Not Overpromise

- Do not call it a universal LLM proxy.
- Do not claim it works with every CLI.
- Do not imply the current gateway proves Codex interception.
- Do not claim drop-in support for all OpenAI-compatible APIs.
- Do not call MITM safe.
- Do not claim a provider-agnostic core before a second provider exists.
- Do not imply streaming rewriting is easy.
- Do not say `HTTPS_PROXY` makes everything work.
- Do not claim it captures all tool calls.
- Do not sell Docker as the host CLI MITM solution.

## One-Liner

Marsala is a local Rust proxy that keeps an explicit OpenAI gateway available while making normal Codex traffic inspectable through narrow, allowlisted MITM.
