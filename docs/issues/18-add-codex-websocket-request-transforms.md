---
title: "Add safe Codex WebSocket request transformations"
labels:
  - feature
  - codex
  - websocket
  - transforms
milestone: "Codex request transforms"
assignees: []
status: published
github_issue: "https://github.com/cameronhotchkies/marsala/issues/2"
depends_on:
  - "https://github.com/cameronhotchkies/marsala/issues/1"
roadmap: "Codex request transforms"
---

## User Story

As a Marsala user, I want narrowly configured changes to Codex requests to
survive its WebSocket transport so that I can alter one supported instruction
without damaging the rest of the conversation.

## Evidence

All 539 currently parseable complete captured `response.create` requests have
exactly two textually identical target occurrences in the top-level
`$.instructions` string and zero occurrences in tool definitions or other
request fields. The sanitized fixture from issue #1 must preserve that shape.

## Scope

- Add an opt-in transformation hook for client-to-server `response.create`
  messages on the allowlisted `/backend-api/codex/responses` path.
- Reassemble fragmented text messages before parsing or mutation.
- Decode and re-encode negotiated `permessage-deflate` messages while
  preserving takeover semantics, WebSocket masking, ordering, and framing.
- Parse the complete message as JSON and expose only approved fields to a
  provider-specific transformer.
- Restrict the Goblin rule's mutation surface to the top-level
  `$.instructions` string. Tool definitions and all other fields remain outside
  the transform surface.
- Add shared runtime transform state that can be read by the proxy and changed
  through a narrow authenticated-by-origin UI endpoint without restarting the
  service. A change applies to subsequent complete client messages, including
  messages on already-open WebSocket connections; it does not alter a message
  already being processed.
- Make the server the authority for transform settings. Persist mutable UI
  settings in a dedicated versioned JSON state file rather than browser local
  storage or by rewriting `marsala.toml`; this keeps multiple browsers
  consistent and preserves TOML comments and environment precedence.
- Add a configurable state-file path with a local default. On a successful
  update, write a temporary file in the same directory, flush and sync it,
  atomically replace the previous file, and sync the parent directory where
  supported before publishing the new in-memory state. A failed write leaves
  both the prior durable value and the live value unchanged.
- Load the state file before either the UI or proxy begins serving. A missing
  file uses defaults. An unreadable, malformed, or unsupported-version file
  fails safe to transforms disabled, emits a sanitized startup event and
  warning, and is not silently overwritten until a user successfully changes
  a setting.
- Expose a revisioned `GET`/`PUT` settings contract and a settings-change SSE
  feed. Updates include the caller's last observed revision; stale updates are
  rejected and refreshed so every open browser converges on server state.
- Assign a local `websocket_connection_id` to each intercepted WebSocket and a
  local `request_id` to every complete supported `response.create`. Snapshot
  transform state exactly once after the complete message is identified and
  before mutation begins; that snapshot follows the request even if the UI is
  toggled while its response is streaming.
- Parse server response lifecycle messages. Bind `response.created.response.id`
  to the pending local request on that WebSocket and carry both `request_id`
  and `response_id` through subsequent streamed lifecycle observations. If a
  response cannot be bound unambiguously, log it as unbound instead of guessing.
- Forward the original message unchanged when decoding, parsing, validation,
  or transformation fails.
- Emit bounded audit metadata describing whether a transform was disabled,
  skipped, applied, ambiguous, or failed without logging instruction content.
- Use additive, versioned audit records. Transform and response-lifecycle
  records include `event_schema_version`, `websocket_connection_id`, local
  `request_id`, optional provider `response_id`, the request-time enabled
  snapshot, rule version, outcome and a bounded reason code. They never include
  instructions, request bodies, user content, credentials, or before/after
  prompt hashes.

## Acceptance Criteria

- [ ] With transforms disabled, forwarded WebSocket bytes remain unchanged.
- [ ] Unmatched messages and non-`response.create` messages remain unchanged.
- [ ] Compressed, uncompressed, fragmented, and context-takeover fixtures reach
  the upstream as valid equivalent WebSocket messages.
- [ ] Transform failure never drops, duplicates, reorders, or partially emits a
  client message.
- [ ] Mutated JSON preserves every non-target value and array ordering.
- [ ] The issue #1 regression fixture contains exactly two target occurrences
  in `$.instructions` and zero in tool definitions or other fields, and the
  transport passes that complete logical message to the transformer.
- [ ] Runtime transform state is concurrency-safe, defaults to disabled, and
  can change while the service and an intercepted WebSocket remain running.
- [ ] A successful settings response means the new value is durable; restart
  restores it. Missing state uses defaults, while corrupt or unsupported state
  disables transforms and produces a sanitized diagnostic.
- [ ] Two browser tabs receive revisioned setting changes and converge without
  using browser-local storage; stale writes cannot silently overwrite a newer
  value.
- [ ] The settings endpoint reports the authoritative state, rejects malformed
  or cross-origin changes, and never exposes prompt contents.
- [ ] Every supported request gets one request-time state snapshot and one local
  request ID. Toggling during a stream affects only the next request.
- [ ] Response lifecycle events retain the same request ID, provider response
  ID when available, and transform snapshot; ambiguous correlation is explicit
  and never inferred from timestamps alone.
- [ ] Memory and inflated-message limits are explicit and tested.
- [ ] Audit events contain no raw instructions, credentials, or user content.

## Out Of Scope

- Server-to-client response transformation.
- Arbitrary JSON patch expressions.
- Transforming tunneled or non-allowlisted hosts.
- Storing mutable transform state in browser local storage.
- Rewriting the user's `marsala.toml` from the UI.

## References

- `crates/marsala/src/proxy.rs`
- `docs/validation/mitm-steel-thread.md`
