# Codex CA Trust Hardening Spec

## Goal

Make Marsala's local CA trust path reliable and diagnosable for normal Codex MITM runs.

The current certificate generation path appears structurally correct: Marsala creates a local CA and signs per-host leaf certificates with host SANs. The remaining failure mode observed in validation is trust propagation into the exact Codex process or TLS stack that opens the connection.

Validation status: complete for the dual-CA-variable normal Codex run. On 2026-06-10 PDT, `just mitm-codex` completed successfully with both `CODEX_CA_CERTIFICATE` and `SSL_CERT_FILE`; `chatgpt.com` and `ab.chatgpt.com` both emitted `mitm_tls status=handshake_ok`, and the `ab.chatgpt.com` metrics request was forwarded successfully. Structured failure classification and doctor commands remain open below.

## Product Requirements

- Treat CA trust as a first-class validation surface, not a footnote in manual commands.
- Always set both Codex-specific and generic CA environment variables in Marsala-provided Codex interception flows:
  - `CODEX_CA_CERTIFICATE`
  - `SSL_CERT_FILE`
- Expose a doctor command that explains why MITM did not decrypt when `CONNECT` traffic is present.
- Preserve strict local safety:
  - generated CA private key remains in gitignored `certs/`
  - CA generation refuses to overwrite existing files
  - no command prints private key material
  - no command installs a system-wide trusted root automatically

## Failure Modes To Detect

- CA certificate file is missing.
- CA private key file is missing.
- CA certificate path in environment differs from Marsala's configured `mitm.ca_cert_path`.
- `CODEX_CA_CERTIFICATE` is unset in the terminal running Codex.
- `SSL_CERT_FILE` is unset in the terminal running Codex.
- Marsala sees `proxy_request` / `CONNECT` events but no later `mitm_tls` for allowlisted Codex hosts.
- Marsala sees `mitm_tls status=error` with certificate trust indicators such as `UnknownCA`.
- Marsala sees decrypted TLS but rejects the next protocol because the client requires unsupported HTTP/2 or malformed upgrade behavior.

## Proposed Commands

- `just ca-doctor`
  - Validate the generated CA files.
  - Print the configured CA cert/key paths.
  - Print whether `CODEX_CA_CERTIFICATE` and `SSL_CERT_FILE` point to the expected certificate.
  - Scan recent `logs/events.jsonl` for MITM CA/trust failure events.
  - Scan recent logs for allowlisted `CONNECT` events that never reached `mitm_tls handshake_ok`.
- `just codex-env-doctor`
  - Include the CA checks from `ca-doctor`.
  - Include proxy env checks from the terminal-wide interception spec.
- `just ui`
  - Continue to run capture-enabled Marsala.
  - [x] Print the expected `CODEX_CA_CERTIFICATE` and `SSL_CERT_FILE` values beside the UI URL.

## Logging And UI Requirements

When a downstream TLS handshake fails, Marsala should emit enough sanitized metadata to diagnose trust failures:

- event type: `mitm_tls`
- host
- port
- status: `error`
- error category when classifiable:
  - `unknown_ca`
  - `certificate_verify_failed`
  - `client_closed`
  - `unsupported_protocol`
  - `other`
- no raw certificate, key, token, cookie, or request payload

The UI should make this visible without forcing the user to grep JSONL:

- surface CA/trust failures in the timeline
- show host and error category in the detail pane
- provide a compact warning when recent allowlisted `CONNECT` traffic failed before decrypted request capture

## Acceptance Criteria

- `just mitm-codex` and any replacement validation command set both `CODEX_CA_CERTIFICATE` and `SSL_CERT_FILE`.
- `just ca-doctor` reports success when:
  - `certs/marsala-ca.pem` exists
  - `certs/marsala-ca-key.pem` exists
  - the public cert parses
  - the private key parses
  - current env points both CA variables at the public cert
- `just ca-doctor` reports actionable failures for missing files or missing env vars.
- [x] A normal Codex run with the correct env shows `mitm_tls status=handshake_ok` for allowlisted ChatGPT hosts.
- A normal Codex run with the CA env intentionally omitted shows a clear trust failure in logs and in the doctor output.
- The UI distinguishes CA trust failure from unsupported HTTP/2 or upstream forwarding failure.
- Tests cover CA doctor parsing/classification without requiring network access.

## Non-Goals

- Do not install the CA into the OS trust store automatically.
- Do not require `sudo`.
- Do not make the MITM allowlist broad or wildcard-based.
- Do not treat API-key `api.openai.com` gateway behavior as proof that subscription-backed Codex trust is working.

## Open Questions

- Whether every Codex network path honors `CODEX_CA_CERTIFICATE`.
- Whether some Codex paths only honor `SSL_CERT_FILE`.
- Whether GUI-launched Codex or app-server flows need separate environment propagation.
- Whether `ab.chatgpt.com` failures are pure trust propagation issues or a distinct TLS client path.
