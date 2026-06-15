---
title: "Extract a sanitized Codex instruction request fixture"
labels:
  - research
  - codex
  - websocket
  - testing
milestone: "Codex request transforms"
assignees: []
status: published
github_issue: "https://github.com/cameronhotchkies/marsala/issues/1"
depends_on: []
roadmap: "Codex request transforms"
---

## User Story

As a Marsala user, I want Codex instruction rewriting to be based on a verified
request shape so that an enabled mode changes only the instruction I selected.

## Evidence

Existing captures are sufficient; no new capture is needed. All 539 currently
parseable complete `response.create` captures in `logs/events.jsonl` contain
exactly two textually identical occurrences of the target sentence in the
top-level `$.instructions` string and zero occurrences in tool definitions or
other request fields. The requests were sent to
`/backend-api/codex/responses` over a `permessage-deflate` WebSocket. Truncated
4096-byte previews are excluded from the 539-request population and are not
needed to establish the request shape.

## Scope

- Extract and sanitize an existing complete request into a regression fixture
  for the supported Codex flow; do not collect another live capture.
- Preserve the observed two occurrences of the target sentence in the
  top-level `$.instructions` field while replacing credentials, user content,
  and unrelated private instructions with inert values.
- Preserve zero target occurrences in tool definitions and every other request
  field.
- Record provenance and the observed WebSocket properties: client-to-server
  text traffic on `/backend-api/codex/responses` using `permessage-deflate`.
- Treat uncompressed and fragmented messages as synthetic compatibility cases;
  they have not been established as observed target-bearing traffic.
- Produce a minimal synthetic fixture that preserves the observed structure
  without retaining credentials, user content, or unrelated private prompts.
- Document the 539-request complete capture population and its invariant:
  exactly two identical matches in `$.instructions`, with zero in tools or
  other request fields.

## Acceptance Criteria

- [ ] A sanitized fixture contains the complete structural path to the exact
  target instruction and preserves both observed occurrences in
  `$.instructions`.
- [ ] The fixture contains zero target occurrences in tool definitions or any
  field outside `$.instructions`.
- [ ] The evidence note distinguishes observed facts from assumptions.
- [ ] The fixture records compressed transport as observed evidence and labels
  uncompressed and fragmented fixtures as synthetic compatibility coverage.
- [ ] No authentication material, user prompt content, or unrelated system
  instructions are committed.
- [ ] The fixture can drive byte-preservation and mutation tests offline.

## Out Of Scope

- Rewriting live traffic.
- General prompt editing.
- Collecting another live capture.

## References

- `logs/events.jsonl`
- `docs/validation/mitm-steel-thread.md`
