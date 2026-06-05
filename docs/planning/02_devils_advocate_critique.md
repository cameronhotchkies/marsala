# Devil's Advocate Critique

> Supersession note (2026-06-04): This critique was written against an overbroad initial pitch. Its warnings about overclaiming, logging risk, streaming mutation, and MITM complexity still stand. Its implied sequencing does not: the clarified thesis makes Codex traffic acquisition/interception viability the next baseline question, while full allowlisted MITM remains optional, unproven, and explicitly not claimed today.

## Blunt Critique

The proposed project is trying to be five hard systems at once:

1. LLM gateway.
2. OpenAI-compatible API shim.
3. Streaming transformer.
4. SQLite audit logger.
5. HTTPS forward proxy with possible allowlisted MITM interception.

Each has real edge cases. Combining them too early is the fastest path to a toy that looks impressive in demos and then corrupts requests, leaks secrets, breaks streaming, or becomes impossible to test.

The highest-risk assumption is that "OpenAI-compatible" is a stable, simple contract. It is not. Chat completions, Responses API, tools, JSON schema, image/audio fields, streaming deltas, usage blocks, refusal fields, provider-specific quirks, and partial compatibility all create pressure to build a fake universal schema too soon.

MITM interception is also a scope trap. It drags in certificate generation, trust-store installation, `CONNECT` handling, SNI/ALPN, HTTP/2 downgrade behavior, corporate proxy conflicts, host allowlisting correctness, certificate pinning failures, and scary logging defaults. It should not be assumed as the first working mechanism, even though interception viability itself is the first credible milestone to validate.

SQLite logging is a quiet footgun. "Aggressive logging" plus LLM requests means API keys, bearer tokens, prompts, customer data, tool args, files, and streamed partials. Redaction and retention policies need to exist before a beautiful event log.

## Hidden Complexity

- Streaming is not just forwarding chunks. It must preserve SSE framing, backpressure, cancellation, partial JSON boundaries, final usage events, provider errors mid-stream, and client disconnect cleanup.
- Response rewriting during streaming can break tool-call assembly, JSON mode, structured output, token accounting, and latency.
- Tool-call capture is provider-specific. OpenAI tool deltas arrive fragmented and need an accumulator with tests.
- Forward proxy mode will break clients using HTTP/2, custom TLS roots, certificate pinning, nonstandard `CONNECT` behavior, or SDK-level proxy bypasses.
- Rust async proxy code can become a swamp when server, client, streaming bodies, TLS, and dynamic certs are combined.
- Docker can run the service, but it does not solve host CLI trust-store interception.
- Provider-neutral abstractions should not be designed before at least one second provider spike.
- Logging streamed bodies to SQLite can cause write amplification, lock contention, huge DB files, and latency spikes unless batched carefully.

## Concrete Corrections

- Do not start with MITM.
- Do not start with multi-provider abstraction.
- Do not start with response rewriting.
- Do not start with both streaming and mutation.
- Keep the explicit local OpenAI-compatible reverse proxy as a compatibility scaffold.
- Support non-streaming `/v1/chat/completions` first.
- Verify Codex routing and interception behavior before promising streaming, rewriting, or tool capture on that path.
- Add structured request/response logging with redaction.
- Add streaming pass-through with exact byte/SSE behavior preserved.
- Add stream observation only, no mutation.
- Add tool-call reconstruction tests.
- Add response rewriting only for non-streaming.
- Add forward proxy behavior early enough to prove the Codex baseline path.
- Add MITM as a separate opt-in experimental mode only if the verified Codex path requires payload interception.

## Non-Negotiable Constraints

- Full MITM is a possible mechanism, not a promised foundation; Codex interception viability is the foundation question.
- OpenAI first means OpenAI-shaped first.
- Streaming pass-through comes before streaming mutation.
- Logging must be safe by default: no raw auth headers, visible DB path, testable redaction.
- Tool-call capture is observation first.
- Every phase needs replayable fixtures.
- Docker is not the MITM story.
- Transforms fail closed or pass through unchanged with an audit event.
- MITM mode must be opt-in, allowlisted by host, noisy at startup, and easy to disable.

## Grudgingly Approvable Positioning

Marsala is a local-first Rust observability proxy for Codex and OpenAI-shaped traffic, with safe local audit logging, a compatibility chat-completions gateway, and a deliberately staged path toward validated opt-in CLI interception for allowlisted LLM providers.
