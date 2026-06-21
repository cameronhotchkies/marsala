# Goblin Mode

Goblin Mode removes the two verified Goblin-related prohibitions from supported
Codex `response.create` instructions. It does not change user messages, tool
definitions, model responses, or other instructions.

## Enable Goblin Mode

1. Start Marsala and open the interception UI.
2. Select the `👺 Goblin mode` checkbox in the toolbar.
3. Wait for the status bar to display:

   `Dance baby, dance! Goblins are back on the menu!`

The setting is stored in Marsala's server-side runtime state. It is shared by
all open UI tabs and remains enabled after Marsala restarts. It does not use
browser local storage or modify `marsala.toml`.

## Verify It

Goblin Mode applies to subsequent supported Codex requests. The event timeline
shows `Goblin on` when the mode was enabled for a request and `Goblin applied`
when the verified rule changed that request. Select an event to see the
request-time setting, outcome, bounded reason, rule version, local request ID,
and provider response ID when available.

The setting is snapshotted when each request begins. Changing the checkbox
while a response is streaming does not rewrite that response's recorded state.

## Logging

Transform and response lifecycle events record whether Goblin Mode was enabled,
whether it applied, and whether processing was skipped or failed. These audit
fields do not contain prompt or instruction text.

If persisted runtime settings cannot be read safely at startup, Marsala starts
with Goblin Mode disabled and emits a sanitized
`runtime_settings_load_problem` event. The event contains a bounded issue code
and the fail-safe action, not file contents or prompt data.

## Disable Goblin Mode

Clear the `👺 Goblin mode` checkbox. The status bar displays
`Goblin mode disabled.` and subsequent messages pass through without the Goblin
transformation. The disabled setting also persists across restarts.

## Fail-Open Behavior

Marsala forwards the original request unchanged when the request shape,
instruction fingerprint, match count, compression, or serialization cannot be
handled safely. The timeline records a bounded `skipped` or `failed` outcome.
Goblin Mode never partially rewrites a request.
