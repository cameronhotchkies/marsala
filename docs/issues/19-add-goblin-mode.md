---
title: "Add opt-in goblin mode for Codex instructions"
labels:
  - feature
  - codex
  - transforms
  - ui
milestone: "Codex request transforms"
assignees: []
status: published
github_issue: "https://github.com/cameronhotchkies/marsala/issues/3"
depends_on:
  - "https://github.com/cameronhotchkies/marsala/issues/1"
  - "https://github.com/cameronhotchkies/marsala/issues/2"
roadmap: "Codex request transforms"
---

## User Story

As a user, I want to enable goblin mode so that the specific instruction
forbidding goblin talk is removed from Codex requests while every other
instruction and request field remains intact.

## Evidence

All 539 currently parseable complete captured `response.create` requests have
exactly two textually identical copies of the target sentence in the top-level
`$.instructions` string. None has a target occurrence in tool definitions or
any other request field. Both `$.instructions` occurrences are operative and
must be removed when Goblin mode applies.

## Scope

- Add a `Goblin mode` checkbox to the existing UI toolbar, after the payload
  filter and before the Pause and Clear buttons. Prefix its visible label with
  the Unicode goblin emoji `👺` when the browser can render it; the persistent
  text label is the fallback and must never depend on the emoji for meaning.
- Keep the first-run default off. The control changes shared server state live
  and affects subsequent complete Codex messages without reconnecting or
  restarting Marsala.
- Persist the authoritative value in Marsala's versioned server-side state file
  only after an atomic durable write succeeds. Restore it before serving proxy
  traffic on restart. Do not use browser local storage and do not rewrite
  `marsala.toml`.
- On page load, read the authoritative backend value and revision before
  enabling the checkbox. Subscribe to server setting changes so toggles from
  another browser update the checkbox without a refresh. Reject stale writes,
  refresh to the winning value, and display a safe conflict message.
- On successful activation, write exactly
  `Dance baby, dance! Goblins are back on the menu!` to the existing UI status
  bar. The connection's `live`, `paused`, or `reconnecting` state may resume on
  the next connection-status update; the activation text must not have counts
  or other text appended to it.
- On deactivation, write `Goblin mode disabled.` to the status bar and preserve
  byte-identical pass-through behavior for subsequent messages.
- While a change is pending, disable the checkbox. If the update fails, restore
  the authoritative checkbox state and show `Goblin mode update failed: <safe
  reason>` in the status bar without exposing prompt or request contents.
- Give the checkbox an explicit accessible name of `Goblin mode`; mark the
  emoji decorative, retain a visible keyboard focus indicator, and expose
  status changes through a polite live region.
- Apply it only to the verified `instructions` field in supported
  `response.create` requests, specifically the top-level `$.instructions`
  string.
- Match the exact evidence-backed instruction as a complete logical instruction,
  not as an unrestricted substring across the request.
- Require and remove exactly the two evidence-backed occurrences while
  preserving surrounding text, line-ending style, and every non-target JSON
  value.
- Never inspect or mutate tool definitions or any field outside
  `$.instructions`, where the 539 complete captures contain zero target
  occurrences.
- Skip mutation when the instruction is absent, differs from the supported
  fingerprint, or the match count is anything other than two.
- Emit a sanitized outcome event that records the mode, rule version, match
  count, and result without recording prompt text.
- For every supported `response.create`, snapshot `goblin_mode_enabled` once at
  request processing time and retain that value for the entire response. Record
  `goblin_mode_applied` separately and classify the transform outcome as
  `disabled`, `applied`, `skipped`, or `failed` with a bounded reason code.
- Emit a versioned `codex_request_transform` record with
  `websocket_connection_id`, local `request_id`, enabled snapshot, applied
  boolean, outcome, rule version and match count. When `response.created`
  supplies the provider ID, emit correlated response-started and terminal
  lifecycle records carrying the same snapshot and IDs. Do not infer a binding
  from timing if protocol ordering is ambiguous.
- Show a compact `Goblin on` or `Goblin applied` marker in the timeline for
  correlated response lifecycle records. In the detail view show enabled at
  request time, applied, outcome, bounded reason, rule version, local request ID
  and provider response ID. Never display or persist prompt contents as part of
  this feature.
- Document how users enable, verify, and disable the mode.

## Acceptance Criteria

- [ ] Goblin mode is disabled by default.
- [ ] After a successful activation, restarting Marsala leaves Goblin mode
  enabled; after a successful deactivation, restart leaves it disabled.
- [ ] Missing persisted state uses the off default. Corrupt, unreadable, or
  unsupported state starts disabled and surfaces a sanitized diagnostic rather
  than guessing or silently replacing the file.
- [ ] The toolbar exposes a keyboard-operable `👺 Goblin mode` checkbox with a
  meaningful text fallback and an accessible name independent of the emoji.
- [ ] Activating the checkbox succeeds without a service or WebSocket restart,
  leaves it visibly checked, and writes exactly `Dance baby, dance! Goblins are
  back on the menu!` to the status bar.
- [ ] Enabling it removes both occurrences of the exact verified instruction
  from top-level `$.instructions` in a supported Codex request and leaves all
  other instructions unchanged.
- [ ] Similar wording in user content, tool definitions, metadata, or other JSON
  fields is never changed.
- [ ] Zero, one, three-or-more, changed, malformed, and oversized matches fail
  open by forwarding the original request unchanged.
- [ ] Tests compare the parsed pre/post request and prove the target deletion is
  the only semantic difference.
- [ ] Compressed and fragmented fixture tests prove the transformed request is
  accepted by the upstream fixture server.
- [ ] Logs and UI expose applied/skipped/failed status without prompt contents.
- [ ] Every supported response retains whether Goblin mode was enabled when its
  request began and whether the rule actually applied, even if the mode is
  toggled before the response finishes.
- [ ] Request, response-started and response-terminal records correlate through
  a local request ID and provider response ID when available; unbound responses
  are visibly marked instead of timestamp-correlated.
- [ ] Timeline and detail views expose the Goblin snapshot and outcome without
  requiring raw-event inspection and without exposing instructions.
- [ ] Disabling the mode restores byte-identical pass-through behavior.
- [ ] Opening, refreshing, or using a second UI reflects the current backend
  state and revision; a stale update cannot overwrite a newer toggle.
- [ ] A failed UI update restores the checkbox to the backend state, reports a
  safe error in the status live region, and does not change transform behavior.

## Out Of Scope

- Removing safety policies generally.
- User-defined search-and-replace rules.
- Rewriting model responses or server-provided model metadata.
- Claiming support for unobserved Codex request shapes.
- Per-browser Goblin mode preferences.

## References

- https://github.com/cameronhotchkies/marsala/issues/1
- https://github.com/cameronhotchkies/marsala/issues/2
- `logs/events.jsonl`
