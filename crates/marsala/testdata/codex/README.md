# Codex request fixtures

`response-create.json` is a sanitized, synthetic fixture derived from complete
Codex `response.create` messages observed in local Marsala captures on June 11
and June 12, 2026.

The fixture preserves the request's relevant JSON structure and the observed
duplicate target sentence: exactly twice in the top-level `instructions`
string and nowhere else in the request. Minimal synthetic section markers
represent the two distinct instruction sections where the sentence was
observed. Credentials, user content, private instructions, request and
installation identifiers, cache keys, and client metadata were deliberately
omitted or replaced.

The source captures remain local and must not be committed. Tests should use
this fixture rather than reading `logs/events.jsonl`.
