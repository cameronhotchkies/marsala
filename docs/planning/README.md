# Marsala Planning Artifacts

This directory captures the planning pass for `marsala`, a local-first Rust LLM proxy.

Files:

- `01_pm_plan.md`: initial project-manager plan.
- `02_devils_advocate_critique.md`: engineering critique of the initial direction.
- `03_reconciled_plan.md`: current product and implementation sequencing plan.
- `04_elevator_pitch_hit_list.md`: short positioning and pitch bullets.
- `05_mitm_foundation.md`: current MITM mainline architecture and implementation plan.
- `06_terminal_codex_interception.md`: spec for making normal terminal-launched `codex` route through Marsala without a launcher or shim.
- `07_ca_trust_hardening.md`: spec for making Codex CA trust failures visible and reliable.

The reconciled plan and MITM foundation plan are the source of truth for implementation sequencing.

Current planning baseline:

- The explicit OpenAI-shaped gateway is retained as a compatibility scaffold and fixture source.
- The mainline milestone is allowlisted MITM for normal ChatGPT-backed Codex traffic.
- Normal Codex has been observed through Marsala's proxy as tunneled `CONNECT` traffic to `chatgpt.com` and `ab.chatgpt.com`.
- The custom-provider inbound-auth shortcut to `api.openai.com/v1/responses` was rejected upstream with `401 Unauthorized`.
- Marsala has intercepted a normal Codex run through allowlisted HTTP/1.1 MITM, including `/backend-api/codex/responses` WebSocket traffic and decoded request previews.
- The remaining transport work is hardening CA trust, classifying failures, and expanding beyond the bounded HTTP/1.1/WebSocket steel thread where evidence requires it.
- The next productization work is terminal-wide Codex interception through shell environment setup and CA trust hardening/doctor commands.
