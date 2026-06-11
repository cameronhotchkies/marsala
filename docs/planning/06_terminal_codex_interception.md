# Terminal-Wide Codex Interception Spec

## Goal

When a developer opens a normal terminal and runs `codex`, Marsala can intercept Codex traffic without requiring a specialty `just mitm-codex` command, wrapper binary, or renamed launcher.

This feature makes Marsala usable as the default local observation layer for Codex while keeping interception explicit, reversible, and scoped to terminal sessions where the user has opted in.

## Product Requirements

- Provide a shell-environment hook that users can install into their terminal startup file.
- The hook must apply to normal terminal-launched Codex commands:
  - `codex`
  - `codex exec ...`
  - `codex resume ...`
  - other subcommands that use the same Codex process environment
- The hook must not replace, rename, wrap, or shadow the `codex` executable.
- The hook must not edit Codex auth files or `~/.codex/config.toml`.
- The hook must be clearly marked, idempotent, and removable.
- Marsala must provide a doctor command that explains whether a new terminal is ready to intercept Codex.

## Environment Contract

The installed shell block should export the interception variables Codex needs:

```bash
export HTTPS_PROXY=http://127.0.0.1:8788
export https_proxy=http://127.0.0.1:8788
export HTTP_PROXY=
export http_proxy=
export ALL_PROXY=
export all_proxy=
export NO_PROXY=
export no_proxy=
export CODEX_CA_CERTIFICATE=/home/cameron/code/marsala/certs/marsala-ca.pem
export SSL_CERT_FILE=/home/cameron/code/marsala/certs/marsala-ca.pem
```

`HTTPS_PROXY` routes Codex HTTPS and WebSocket traffic through Marsala. `CODEX_CA_CERTIFICATE` is the Codex-specific trust hook. `SSL_CERT_FILE` is the fallback for any TLS client path that does not consult the Codex-specific variable.

## Proposed Commands

- [x] `just codex-env`
  - Print the exact shell exports without modifying files.
- `just install-codex-env`
  - Append or update a marked Marsala block in the user's shell startup file.
  - Default target for this environment: `~/.bashrc`.
  - Refuse to proceed if the CA file is missing.
- `just uninstall-codex-env`
  - Remove only the marked Marsala block.
  - Leave unrelated shell content untouched.
- `just codex-env-doctor`
  - Report whether:
    - `HTTPS_PROXY` and `https_proxy` point at Marsala
    - `HTTP_PROXY`, `ALL_PROXY`, and `NO_PROXY` are cleared as expected
    - `CODEX_CA_CERTIFICATE` exists
    - `SSL_CERT_FILE` exists
    - Marsala is listening on `127.0.0.1:8788`
    - recent logs show Codex `CONNECT` traffic
    - recent logs show decrypted MITM events or CA trust errors

## User Flow

Initial setup:

```bash
just mitm-ca-init
just install-codex-env
```

Then open a new terminal and run:

```bash
just ui 262144
```

In another terminal:

```bash
codex exec --skip-git-repo-check -c 'approval_policy="never"' 'Reply with one short sentence.'
```

Expected result: the normal `codex` command routes through Marsala and the UI shows allowlisted Codex traffic.

Validation status: complete for applying `just codex-env` to a shell and running plain `codex exec`. On 2026-06-10 PDT, the command completed successfully and produced `proxy_request`, `mitm_tls`, `mitm_request`, and `mitm_response` events for the allowlisted Codex hosts.

## Acceptance Criteria

- [x] `just codex-env` prints the intended exports and does not mutate the machine.
- `just install-codex-env` is idempotent and does not duplicate the Marsala shell block.
- `just uninstall-codex-env` removes the Marsala shell block and does not remove user-authored shell content.
- [x] A new terminal session running plain `codex exec ...` emits `proxy_request` events through Marsala when Marsala is running.
- [x] With MITM enabled and trusted, the same run emits `mitm_tls`, `mitm_request`, and `mitm_response` events. Bounded payload/WebSocket preview events remain covered by the capture-enabled validation workflow.
- If Marsala is not running, the doctor command identifies that condition before the user debugs Codex itself.
- If the CA file is missing or unreadable, the doctor command reports that directly.
- No raw auth tokens or session cookies are printed by any doctor command.

## Non-Goals

- No `codex` shim.
- No wrapper binary.
- No changes to Codex auth state.
- No global system proxy configuration.
- No desktop-launcher support in this feature. GUI-launched Codex may require a separate environment propagation path.
- No MITM of arbitrary hosts. The Marsala allowlist remains exact-host and narrow.

## Risks

- Shell startup files vary. The first supported target should be Bash because the current environment uses Bash.
- Existing user proxy settings could conflict with Marsala. The doctor command must make these conflicts visible.
- Desktop apps and already-running Codex processes will not inherit new shell exports.
- Broad shell exports affect every command launched from that terminal, so non-allowlisted traffic must continue to tunnel untouched.
