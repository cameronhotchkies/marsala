# Elevator Pitch Hit List

## Honest Pitch

Marsala is a local-first Rust observability proxy for OpenAI traffic, with safe local audit logging, streaming-aware capture, deterministic response cleanup, and a staged path toward opt-in CLI HTTPS interception for allowlisted LLM providers.

## Hit List

- Local Rust proxy for seeing what LLM clients actually send and receive.
- OpenAI-compatible ingress first: point SDKs at `http://localhost:8787/v1`.
- Faithful streaming passthrough before any stream mutation.
- Local logs with redaction, request IDs, timings, errors, and optional body capture.
- Response cleanup for non-streaming responses, starting with deterministic filler removal.
- Tool-call capture by rule, starting with complete calls and later streamed reconstruction.
- CLI proxy mode enters later through `HTTP_PROXY` and `HTTPS_PROXY`.
- MITM is opt-in, noisy, allowlisted, and disabled by default.
- Anthropic and local model providers are roadmap items after OpenAI behavior is stable.

## Do Not Overpromise

- Do not call it a universal LLM proxy.
- Do not claim it works with every CLI.
- Do not claim drop-in support for all OpenAI-compatible APIs.
- Do not call MITM safe.
- Do not claim a provider-agnostic core before a second provider exists.
- Do not imply streaming rewriting is easy.
- Do not say `HTTPS_PROXY` makes everything work.
- Do not claim it captures all tool calls.
- Do not sell Docker as the host CLI MITM solution.

## One-Liner

Marsala is a local Rust proxy that makes OpenAI traffic observable first, configurable second, and interceptable later without pretending MITM and streaming mutation are simple plumbing.
